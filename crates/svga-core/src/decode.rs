//! Decoding of FIFO commands from untrusted guest words.
//!
//! Decoding happens in three steps, so that a guest racing the device cannot
//! make it read inconsistent data (TOCTOU):
//!
//! 1. compute the command length by peeking at its header in shared memory;
//! 2. copy exactly that many words into device-owned memory;
//! 3. recompute the length from the copy, reject the command if it differs,
//!    then build the [`Command`] from the copy only.
//!
//! Nothing is consumed here: the caller advances the ring once it has the
//! decoded command. All size arithmetic is done in `u64` with explicit bounds,
//! so no guest value can overflow it.

use core::fmt;

use svga_proto::{cmd, cmd3d, remap_gmr2};

use crate::command::{
    Command, Cursor, GuestImage, GuestPtr, PageSource, Point, Rect, ScreenObject, SignedRect,
};

/// A window of 32-bit words the decoder can peek at.
pub trait WordSource {
    /// Number of words currently readable.
    fn available(&self) -> usize;
    /// Largest number of words that could ever be readable at once. A command
    /// longer than this can never complete and is rejected.
    fn capacity(&self) -> usize;
    /// Word at `offset`, or `None` if `offset >= available()`.
    fn peek(&self, offset: usize) -> Option<u32>;
}

impl WordSource for [u32] {
    fn available(&self) -> usize {
        self.len()
    }

    fn capacity(&self) -> usize {
        self.len()
    }

    fn peek(&self, offset: usize) -> Option<u32> {
        self.get(offset).copied()
    }
}

/// Device-chosen bounds on variable-length commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest cursor width or height, in pixels.
    pub max_cursor_dimension: u32,
    /// Largest `SVGA_CMD_ESCAPE` payload, in bytes.
    pub max_escape_bytes: u32,
    /// Largest 3D command payload, in bytes.
    pub max_3d_payload_bytes: u32,
    /// Largest `structSize` accepted for a screen object, in bytes.
    pub max_screen_object_bytes: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_cursor_dimension: 256,
            max_escape_bytes: cmd::MAX_DATASIZE,
            max_3d_payload_bytes: cmd::MAX_DATASIZE,
            max_screen_object_bytes: 256,
        }
    }
}

/// Size of the first version of `SVGAScreenObject` (up to `root`), in bytes.
pub const SCREEN_OBJECT_V1_BYTES: u32 = 28;
/// Size once `backingStore` was added, in bytes.
pub const SCREEN_OBJECT_V2_BYTES: u32 = 40;
/// Size once `cloneCount` was added, in bytes.
pub const SCREEN_OBJECT_V3_BYTES: u32 = 44;

/// Why a command was rejected. Once a command is rejected the FIFO cannot be
/// resynchronised: the device stops processing and reports an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The command ID is not part of the protocol.
    UnknownCommand(u32),
    /// The command ID was retired from the protocol.
    ObsoleteCommand(u32),
    /// A cursor exceeds [`Limits::max_cursor_dimension`].
    CursorTooLarge {
        /// Announced width.
        width: u32,
        /// Announced height.
        height: u32,
    },
    /// A cursor mask depth is outside `1..=32`.
    InvalidCursorDepth(u32),
    /// An escape payload exceeds [`Limits::max_escape_bytes`].
    EscapeTooLarge(u32),
    /// A screen object size is unaligned, too small or too large.
    InvalidScreenObjectSize(u32),
    /// A 3D payload exceeds [`Limits::max_3d_payload_bytes`].
    Payload3dTooLarge {
        /// Command ID.
        id: u32,
        /// Announced payload size in bytes.
        size: u32,
    },
    /// The command could never fit in the ring.
    CommandTooLarge {
        /// Command ID.
        id: u32,
        /// Length of the command (or of the header read so far), in words.
        words: u64,
    },
    /// The guest modified the command while the device was copying it.
    ChangedWhileReading {
        /// Command ID.
        id: u32,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnknownCommand(id) => write!(f, "unknown FIFO command {id}"),
            Self::ObsoleteCommand(id) => write!(f, "obsolete FIFO command {id}"),
            Self::CursorTooLarge { width, height } => {
                write!(f, "cursor of {width}x{height} pixels is too large")
            }
            Self::InvalidCursorDepth(depth) => write!(f, "invalid cursor mask depth {depth}"),
            Self::EscapeTooLarge(size) => write!(f, "escape payload of {size} bytes is too large"),
            Self::InvalidScreenObjectSize(size) => write!(f, "invalid screen object size {size}"),
            Self::Payload3dTooLarge { id, size } => {
                write!(f, "3D command {id} payload of {size} bytes is too large")
            }
            Self::CommandTooLarge { id, words } => {
                write!(f, "command {id} of {words} words cannot fit in the FIFO")
            }
            Self::ChangedWhileReading { id } => {
                write!(f, "command {id} was modified by the guest while being read")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Result of [`decode`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decode {
    /// A complete command; the caller must consume `words` words.
    Ready {
        /// The decoded command.
        command: Command,
        /// Length of the command in words, ID included.
        words: usize,
    },
    /// The command is not fully written yet. Nothing must be consumed.
    Incomplete,
    /// The command is malformed. The FIFO must stop.
    Invalid(DecodeError),
}

/// Decodes the command at the start of `src`, without consuming it.
pub fn decode<S: WordSource + ?Sized>(src: &S, limits: &Limits) -> Decode {
    let Some(id) = src.peek(0) else {
        return Decode::Incomplete;
    };

    // Step 1: length from the shared header.
    let words = match length(id, src, limits) {
        Ok(words) => words,
        Err(Halt::Incomplete) => return Decode::Incomplete,
        Err(Halt::Invalid(error)) => return Decode::Invalid(error),
    };
    let too_large = Decode::Invalid(DecodeError::CommandTooLarge {
        id,
        words: words.get(),
    });
    let Ok(words) = usize::try_from(words.get()) else {
        return too_large;
    };
    if words > src.capacity() {
        return too_large;
    }
    if words > src.available() {
        return Decode::Incomplete;
    }

    // Step 2: private copy.
    let copy: Vec<u32> = (0..words).map(|i| src.peek(i).unwrap_or(0)).collect();

    // Step 3: the copy must describe the same command.
    let consistent = copy.first() == Some(&id)
        && matches!(length(id, copy.as_slice(), limits), Ok(n) if n.get() == words as u64);
    if !consistent {
        return Decode::Invalid(DecodeError::ChangedWhileReading { id });
    }

    Decode::Ready {
        command: parse(id, &copy),
        words,
    }
}

/// Length of a command in words, ID included. Never zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Words(u64);

impl Words {
    fn get(self) -> u64 {
        self.0
    }
}

enum Halt {
    Incomplete,
    Invalid(DecodeError),
}

impl From<DecodeError> for Halt {
    fn from(error: DecodeError) -> Self {
        Self::Invalid(error)
    }
}

/// Reads header word `offset` of command `id`, needed to compute its length.
fn header<S: WordSource + ?Sized>(src: &S, id: u32, offset: usize) -> Result<u32, Halt> {
    if offset >= src.capacity() {
        let words = u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1);
        return Err(DecodeError::CommandTooLarge { id, words }.into());
    }
    src.peek(offset).ok_or(Halt::Incomplete)
}

fn words_for_bytes(bytes: u32) -> u64 {
    u64::from(bytes).div_ceil(4)
}

fn check_cursor(width: u32, height: u32, limits: &Limits) -> Result<(), DecodeError> {
    if width > limits.max_cursor_dimension || height > limits.max_cursor_dimension {
        return Err(DecodeError::CursorTooLarge { width, height });
    }
    Ok(())
}

/// Words of one cursor mask: each scanline is padded to 32 bits. Callers have
/// bounded `width` and `height` by [`Limits::max_cursor_dimension`], so the
/// `u64` products cannot overflow.
fn mask_words(width: u32, height: u32, depth: u32) -> Result<u64, DecodeError> {
    if !(1..=32).contains(&depth) {
        return Err(DecodeError::InvalidCursorDepth(depth));
    }
    let bits_per_line = u64::from(width).saturating_mul(u64::from(depth));
    Ok(bits_per_line.div_ceil(32).saturating_mul(u64::from(height)))
}

/// Number of inline page entries of `REMAP_GMR2`, in words.
fn remap_payload_words(flags: u32, num_pages: u32) -> u64 {
    if flags & remap_gmr2::VIA_GMR != 0 {
        return 2;
    }
    let entries = if flags & remap_gmr2::SINGLE_PPN != 0 {
        1
    } else {
        u64::from(num_pages)
    };
    let per_entry = if flags & remap_gmr2::PPN64 != 0 { 2 } else { 1 };
    entries.saturating_mul(per_entry)
}

fn length<S: WordSource + ?Sized>(id: u32, src: &S, limits: &Limits) -> Result<Words, Halt> {
    // `fixed(n)`: the ID word plus `n` argument words.
    let fixed = |args: u64| Ok(Words(args.saturating_add(1)));
    let h = |offset| header(src, id, offset);

    match id {
        cmd::UPDATE => fixed(4),
        cmd::RECT_COPY => fixed(6),
        cmd::RECT_ROP_COPY => fixed(7),
        cmd::UPDATE_VERBOSE => fixed(5),
        cmd::FRONT_ROP_FILL => fixed(6),
        cmd::FENCE | cmd::DESTROY_SCREEN | cmd::ANNOTATION_FILL => fixed(1),
        cmd::DEFINE_GMRFB => fixed(4),
        cmd::BLIT_GMRFB_TO_SCREEN | cmd::BLIT_SCREEN_TO_GMRFB => fixed(7),
        cmd::ANNOTATION_COPY => fixed(3),
        cmd::DEFINE_GMR2 => fixed(2),
        cmd::NOP | cmd::NOP_ERROR => fixed(0),
        cmd::DEFINE_CURSOR => {
            let (width, height) = (h(4)?, h(5)?);
            let (and_depth, xor_depth) = (h(6)?, h(7)?);
            check_cursor(width, height, limits)?;
            let masks = mask_words(width, height, and_depth)?
                .saturating_add(mask_words(width, height, xor_depth)?);
            fixed(masks.saturating_add(7))
        }
        cmd::DEFINE_ALPHA_CURSOR => {
            let (width, height) = (h(4)?, h(5)?);
            check_cursor(width, height, limits)?;
            fixed(
                u64::from(width)
                    .saturating_mul(u64::from(height))
                    .saturating_add(5),
            )
        }
        cmd::ESCAPE => {
            let size = h(2)?;
            if size > limits.max_escape_bytes {
                return Err(DecodeError::EscapeTooLarge(size).into());
            }
            fixed(words_for_bytes(size).saturating_add(2))
        }
        cmd::DEFINE_SCREEN => {
            let size = h(1)?;
            if size % 4 != 0
                || size < SCREEN_OBJECT_V1_BYTES
                || size > limits.max_screen_object_bytes
            {
                return Err(DecodeError::InvalidScreenObjectSize(size).into());
            }
            fixed(u64::from(size / 4))
        }
        cmd::REMAP_GMR2 => {
            let (flags, num_pages) = (h(2)?, h(4)?);
            fixed(remap_payload_words(flags, num_pages).saturating_add(4))
        }
        cmd::DEAD | cmd::DEAD_2 => Err(DecodeError::ObsoleteCommand(id).into()),
        _ if (cmd3d::LEGACY_BASE..cmd3d::FUTURE_MAX).contains(&id) => {
            let size = h(1)?;
            if size > limits.max_3d_payload_bytes {
                return Err(DecodeError::Payload3dTooLarge { id, size }.into());
            }
            fixed(words_for_bytes(size).saturating_add(1))
        }
        _ => Err(DecodeError::UnknownCommand(id).into()),
    }
}

/// Sequential reader over a private, length-checked copy of a command.
struct Reader<'a> {
    words: &'a [u32],
}

impl Reader<'_> {
    fn u32(&mut self) -> u32 {
        match self.words.split_first() {
            Some((&first, rest)) => {
                self.words = rest;
                first
            }
            None => 0,
        }
    }

    fn i32(&mut self) -> i32 {
        self.u32().cast_signed()
    }

    fn take(&mut self, count: u64) -> Vec<u32> {
        let count = usize::try_from(count)
            .unwrap_or(usize::MAX)
            .min(self.words.len());
        let (taken, rest) = self.words.split_at(count);
        self.words = rest;
        taken.to_vec()
    }

    fn rest(&mut self) -> Vec<u32> {
        core::mem::take(&mut self.words).to_vec()
    }

    fn rect(&mut self) -> Rect {
        Rect {
            x: self.u32(),
            y: self.u32(),
            width: self.u32(),
            height: self.u32(),
        }
    }

    fn point(&mut self) -> Point {
        Point {
            x: self.i32(),
            y: self.i32(),
        }
    }

    fn signed_rect(&mut self) -> SignedRect {
        SignedRect {
            left: self.i32(),
            top: self.i32(),
            right: self.i32(),
            bottom: self.i32(),
        }
    }

    fn guest_ptr(&mut self) -> GuestPtr {
        GuestPtr {
            gmr_id: self.u32(),
            offset: self.u32(),
        }
    }
}

fn bytes_of(words: &[u32], size: u32) -> Vec<u8> {
    let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.truncate(usize::try_from(size).unwrap_or(usize::MAX));
    bytes
}

/// Clears the bytes of the last word that lie beyond `size`.
fn zero_padding(mut payload: Vec<u32>, size: u32) -> Vec<u32> {
    // Mask of the valid low bytes for 1, 2 and 3 used bytes (little-endian).
    let mask = match size % 4 {
        1 => 0x0000_00ff,
        2 => 0x0000_ffff,
        3 => 0x00ff_ffff,
        _ => return payload,
    };
    if let Some(last) = payload.last_mut() {
        *last &= mask;
    }
    payload
}

/// Builds the command from its private copy. `copy` has been length-checked
/// against `id`, so every variable-length part is fully present.
fn parse(id: u32, copy: &[u32]) -> Command {
    let mut r = Reader {
        words: copy.get(1..).unwrap_or_default(),
    };
    match id {
        cmd::UPDATE => Command::Update(r.rect()),
        cmd::UPDATE_VERBOSE => Command::UpdateVerbose {
            rect: r.rect(),
            reason: r.u32(),
        },
        cmd::RECT_COPY => Command::RectCopy {
            src_x: r.u32(),
            src_y: r.u32(),
            dst: r.rect(),
        },
        cmd::RECT_ROP_COPY => Command::RectRopCopy {
            src_x: r.u32(),
            src_y: r.u32(),
            dst: r.rect(),
            rop: r.u32(),
        },
        cmd::FRONT_ROP_FILL => Command::FrontRopFill {
            color: r.u32(),
            rect: r.rect(),
            rop: r.u32(),
        },
        cmd::DEFINE_CURSOR => {
            let (cursor_id, hotspot_x, hotspot_y) = (r.u32(), r.u32(), r.u32());
            let (width, height) = (r.u32(), r.u32());
            let (and_mask_depth, xor_mask_depth) = (r.u32(), r.u32());
            let and_words = mask_words(width, height, and_mask_depth).unwrap_or(0);
            let xor_words = mask_words(width, height, xor_mask_depth).unwrap_or(0);
            Command::DefineCursor(Cursor {
                id: cursor_id,
                hotspot_x,
                hotspot_y,
                width,
                height,
                and_mask_depth,
                xor_mask_depth,
                and_mask: r.take(and_words),
                xor_mask: r.take(xor_words),
            })
        }
        cmd::DEFINE_ALPHA_CURSOR => {
            let _reserved_id = r.u32();
            let (hotspot_x, hotspot_y, width, height) = (r.u32(), r.u32(), r.u32(), r.u32());
            Command::DefineAlphaCursor {
                hotspot_x,
                hotspot_y,
                width,
                height,
                pixels: r.rest(),
            }
        }
        cmd::FENCE => Command::Fence(r.u32()),
        cmd::ESCAPE => {
            let (nsid, size) = (r.u32(), r.u32());
            Command::Escape {
                nsid,
                data: bytes_of(&r.rest(), size),
            }
        }
        cmd::DEFINE_SCREEN => {
            let struct_size = r.u32();
            let (screen_id, flags, width, height) = (r.u32(), r.u32(), r.u32(), r.u32());
            let root = r.point();
            let backing_store = (struct_size >= SCREEN_OBJECT_V2_BYTES).then(|| GuestImage {
                ptr: r.guest_ptr(),
                pitch: r.u32(),
            });
            let clone_count = (struct_size >= SCREEN_OBJECT_V3_BYTES).then(|| r.u32());
            Command::DefineScreen(ScreenObject {
                struct_size,
                id: screen_id,
                flags,
                width,
                height,
                root,
                backing_store,
                clone_count,
            })
        }
        cmd::DESTROY_SCREEN => Command::DestroyScreen { id: r.u32() },
        cmd::DEFINE_GMRFB => {
            let ptr = r.guest_ptr();
            let bytes_per_line = r.u32();
            let [bits_per_pixel, color_depth, reserved_lo, reserved_hi] = r.u32().to_le_bytes();
            Command::DefineGmrFb {
                ptr,
                bytes_per_line,
                bits_per_pixel,
                color_depth,
                format_reserved: u16::from_le_bytes([reserved_lo, reserved_hi]),
            }
        }
        cmd::BLIT_GMRFB_TO_SCREEN => Command::BlitGmrFbToScreen {
            src_origin: r.point(),
            dst_rect: r.signed_rect(),
            dst_screen: r.u32(),
        },
        cmd::BLIT_SCREEN_TO_GMRFB => Command::BlitScreenToGmrFb {
            dst_origin: r.point(),
            src_rect: r.signed_rect(),
            src_screen: r.u32(),
        },
        cmd::ANNOTATION_FILL => Command::AnnotationFill { color: r.u32() },
        cmd::ANNOTATION_COPY => Command::AnnotationCopy {
            src_origin: r.point(),
            src_screen: r.u32(),
        },
        cmd::DEFINE_GMR2 => Command::DefineGmr2 {
            gmr_id: r.u32(),
            num_pages: r.u32(),
        },
        cmd::REMAP_GMR2 => {
            let (gmr_id, flags, offset_pages, num_pages) = (r.u32(), r.u32(), r.u32(), r.u32());
            let pages = if flags & remap_gmr2::VIA_GMR != 0 {
                PageSource::ViaGmr(r.guest_ptr())
            } else {
                let raw = r.rest();
                let entries = if flags & remap_gmr2::PPN64 != 0 {
                    raw.chunks_exact(2)
                        .map(|pair| u64::from(pair[0]) | (u64::from(pair[1]) << 32))
                        .collect()
                } else {
                    raw.into_iter().map(u64::from).collect()
                };
                PageSource::Inline(entries)
            };
            Command::RemapGmr2 {
                gmr_id,
                flags,
                offset_pages,
                num_pages,
                pages,
            }
        }
        cmd::NOP => Command::Nop,
        cmd::NOP_ERROR => Command::NopError,
        _ => {
            // Only 3D-framed IDs reach this arm: `length` rejected the rest.
            let size = r.u32();
            Command::Command3d {
                id,
                size,
                payload: zero_padding(r.rest(), size),
            }
        }
    }
}
