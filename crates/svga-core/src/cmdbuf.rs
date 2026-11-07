//! Command buffers (`SVGA_CAP_COMMAND_BUFFERS`).
//!
//! The guest builds a buffer of ordinary SVGA commands in its memory,
//! describes it with a 64-byte `SVGACBHeader`, and submits the header's
//! physical address through `SVGA_REG_COMMAND_HIGH` / `COMMAND_LOW`. The
//! device runs the commands, then writes a status (and, on error, the offset
//! of the faulty command) back into the header.
//!
//! This module is pure: it parses a header the device model copied out of
//! guest memory, and decodes a private copy of the buffer contents. Reading
//! and writing guest memory is the device model's job.

use core::fmt;

use svga_proto::cb;

use crate::command::Command;
use crate::decode::{Decode, DecodeError, Limits, decode};

/// Byte offset of `status` in the header (written by the device).
pub const STATUS_OFFSET: u64 = 0;
/// Byte offset of `errorOffset` in the header (written by the device).
pub const ERROR_OFFSET_OFFSET: u64 = 4;

/// Where the commands of a buffer live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// At a guest physical address.
    Physical(u64),
    /// Inside a memory object (`SVGA_CB_FLAG_MOB`).
    Mob {
        /// MOB ID.
        id: u32,
        /// Byte offset inside the MOB.
        offset: u32,
    },
}

/// An `SVGACBHeader`, as copied from guest memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// `status` as the guest left it (normally `SVGA_CB_STATUS_NONE`).
    pub status: u32,
    /// `errorOffset` as the guest left it.
    pub error_offset: u32,
    /// Guest-chosen ID, used by `SVGA_DC_CMD_PREEMPT`.
    pub id: u64,
    /// Bits from [`svga_proto::cb::flag`].
    pub flags: u32,
    /// Size of the buffer in bytes.
    pub length: u32,
    /// Where the buffer is.
    pub location: Location,
    /// Byte offset at which to start (non-zero when resubmitting a buffer
    /// after an error or a preemption).
    pub offset: u32,
    /// DX context the commands apply to (`SVGA_CB_FLAG_DX_CONTEXT`).
    pub dx_context: u32,
    must_be_zero: [u32; 6],
}

/// Why a header was refused (`SVGA_CB_STATUS_CB_HEADER_ERROR`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// A reserved word is not zero.
    Reserved,
    /// The buffer is larger than the device accepts.
    TooLong(u32),
    /// Length or start offset is not a multiple of 4.
    Unaligned,
    /// The start offset lies beyond the end of the buffer.
    OffsetBeyondEnd {
        /// Start offset.
        offset: u32,
        /// Buffer length.
        length: u32,
    },
    /// Unknown flag bits.
    UnknownFlags(u32),
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Reserved => write!(f, "reserved header words are not zero"),
            Self::TooLong(length) => write!(f, "command buffer of {length} bytes is too long"),
            Self::Unaligned => write!(f, "length or offset is not a multiple of 4"),
            Self::OffsetBeyondEnd { offset, length } => {
                write!(f, "start offset {offset} is beyond the length {length}")
            }
            Self::UnknownFlags(flags) => write!(f, "unknown flags {flags:#x}"),
        }
    }
}

impl Header {
    /// Parses the 64 header bytes (little-endian, packed).
    pub fn parse(bytes: &[u8; 64]) -> Self {
        let mut w = [0u32; 16];
        for (slot, chunk) in w.iter_mut().zip(bytes.chunks_exact(4)) {
            *slot = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        let [
            status,
            error_offset,
            id_lo,
            id_hi,
            flags,
            length,
            ptr_lo,
            ptr_hi,
            offset,
            dx_context,
            z0,
            z1,
            z2,
            z3,
            z4,
            z5,
        ] = w;
        let location = if flags & cb::flag::MOB != 0 {
            Location::Mob {
                id: ptr_lo,
                offset: ptr_hi,
            }
        } else {
            Location::Physical(u64::from(ptr_lo) | u64::from(ptr_hi) << 32)
        };
        Self {
            status,
            error_offset,
            id: u64::from(id_lo) | u64::from(id_hi) << 32,
            flags,
            length,
            location,
            offset,
            dx_context,
            must_be_zero: [z0, z1, z2, z3, z4, z5],
        }
    }

    /// Checks the header against the device limits.
    pub fn validate(&self, max_length: u32) -> Result<(), HeaderError> {
        let known = cb::flag::NO_IRQ | cb::flag::DX_CONTEXT | cb::flag::MOB;
        if self.flags & !known != 0 {
            return Err(HeaderError::UnknownFlags(self.flags & !known));
        }
        if self.must_be_zero.iter().any(|&w| w != 0) {
            return Err(HeaderError::Reserved);
        }
        if self.length > max_length {
            return Err(HeaderError::TooLong(self.length));
        }
        if !self.length.is_multiple_of(4) || !self.offset.is_multiple_of(4) {
            return Err(HeaderError::Unaligned);
        }
        if self.offset > self.length {
            return Err(HeaderError::OffsetBeyondEnd {
                offset: self.offset,
                length: self.length,
            });
        }
        Ok(())
    }

    /// Whether the device must raise `SVGA_IRQFLAG_COMMAND_BUFFER` once done.
    pub fn wants_irq(&self) -> bool {
        self.flags & cb::flag::NO_IRQ == 0
    }
}

/// A command of the device context ([`svga_proto::cb::context::DEVICE`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceCommand {
    /// Does nothing.
    Nop,
    /// Starts or stops a context.
    StartStop {
        /// Start (true) or stop.
        enable: bool,
        /// The context.
        context: u32,
    },
    /// Takes back the buffers a context has queued.
    Preempt {
        /// The context.
        context: u32,
        /// Leave buffers whose ID is 0 alone.
        ignore_id_zero: bool,
    },
    /// Starts a context's queue.
    StartQueue {
        /// The context.
        context: u32,
    },
    /// Stops a context's queue.
    AsyncStopQueue {
        /// The context.
        context: u32,
    },
    /// Throws away a context's queued buffers.
    EmptyQueue {
        /// The context.
        context: u32,
    },
}

/// A device-context command could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCommandError {
    /// Byte offset of the command in the buffer.
    pub offset: u32,
    /// Its ID.
    pub id: u32,
}

/// Decodes the device-context commands in `words`, each with its byte
/// offset.
pub fn decode_device(words: &[u32]) -> Result<Vec<(u32, DeviceCommand)>, DeviceCommandError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(&id) = words.get(at) {
        let arg = |i: usize| at.checked_add(i).and_then(|j| words.get(j)).copied();
        let error = DeviceCommandError {
            offset: byte_offset(at),
            id,
        };
        let (command, size) = match id {
            cb::dc::NOP => (Some(DeviceCommand::Nop), 1),
            cb::dc::START_STOP_CONTEXT => (
                arg(1)
                    .zip(arg(2))
                    .map(|(enable, context)| DeviceCommand::StartStop {
                        enable: enable != 0,
                        context,
                    }),
                3,
            ),
            cb::dc::PREEMPT => (
                arg(1)
                    .zip(arg(2))
                    .map(|(context, ignore)| DeviceCommand::Preempt {
                        context,
                        ignore_id_zero: ignore != 0,
                    }),
                3,
            ),
            cb::dc::START_QUEUE => (
                arg(1).map(|context| DeviceCommand::StartQueue { context }),
                2,
            ),
            cb::dc::ASYNC_STOP_QUEUE => (
                arg(1).map(|context| DeviceCommand::AsyncStopQueue { context }),
                2,
            ),
            cb::dc::EMPTY_CONTEXT_QUEUE => (
                arg(1).map(|context| DeviceCommand::EmptyQueue { context }),
                2,
            ),
            _ => (None, 1),
        };
        out.push((error.offset, command.ok_or(error)?));
        at = at.saturating_add(size);
    }
    Ok(out)
}

/// Byte offset of word `at` (buffers are far below 4 GiB).
fn byte_offset(at: usize) -> u32 {
    u32::try_from(at.saturating_mul(4)).unwrap_or(u32::MAX)
}

/// A command found in a buffer, with its byte offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Byte offset of the command in the buffer.
    pub offset: u32,
    /// The command.
    pub command: Command,
}

/// Why decoding a buffer stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferError {
    /// The command is malformed.
    Invalid(DecodeError),
    /// The buffer ends in the middle of the command. (A command cut by the
    /// end of the buffer is normally reported as `CommandTooLarge`.)
    Truncated,
}

impl fmt::Display for BufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Truncated => write!(f, "the buffer ends in the middle of a command"),
        }
    }
}

/// Decodes the commands of a buffer, starting `start` bytes in. Stops at
/// the first bad command and returns its byte offset with the reason; the
/// commands before it are valid and must still run.
pub fn decode_buffer(
    words: &[u32],
    start: u32,
    limits: &Limits,
) -> (Vec<Entry>, Option<(u32, BufferError)>) {
    let mut entries = Vec::new();
    let mut at = usize::try_from(start.div_euclid(4)).unwrap_or(usize::MAX);
    while let Some(rest) = words.get(at..).filter(|rest| !rest.is_empty()) {
        let offset = byte_offset(at);
        match decode(rest, limits) {
            Decode::Ready { command, words: n } => {
                entries.push(Entry { offset, command });
                at = at.saturating_add(n);
            }
            Decode::Incomplete => return (entries, Some((offset, BufferError::Truncated))),
            Decode::Invalid(error) => {
                return (entries, Some((offset, BufferError::Invalid(error))));
            }
        }
    }
    (entries, None)
}

/// Device limits for commands inside command buffers: no single command may
/// exceed `SVGA_CB_MAX_COMMAND_SIZE`.
pub fn buffer_limits(base: &Limits) -> Limits {
    Limits {
        max_3d_payload_bytes: base.max_3d_payload_bytes.min(cb::MAX_COMMAND_SIZE),
        max_escape_bytes: base.max_escape_bytes.min(cb::MAX_COMMAND_SIZE),
        ..*base
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::command::Rect;
    use svga_proto::cmd;

    fn header_bytes(flags: u32, length: u32, ptr: u64, offset: u32) -> [u8; 64] {
        let mut b = [0u8; 64];
        let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(8, 7); // id
        put(16, flags);
        put(20, length);
        put(24, ptr as u32);
        put(28, (ptr >> 32) as u32);
        put(32, offset);
        put(36, 3); // dxContext
        b
    }

    #[test]
    fn header_fields_and_validation() {
        let h = Header::parse(&header_bytes(cb::flag::NO_IRQ, 64, 0x1_2345_6000, 8));
        assert_eq!(h.id, 7);
        assert_eq!(h.location, Location::Physical(0x1_2345_6000));
        assert_eq!((h.length, h.offset, h.dx_context), (64, 8, 3));
        assert!(!h.wants_irq());
        assert_eq!(h.validate(4096), Ok(()));
        assert_eq!(h.validate(32), Err(HeaderError::TooLong(64)));

        let mob = Header::parse(&header_bytes(cb::flag::MOB, 8, 5 | 16 << 32, 0));
        assert_eq!(mob.location, Location::Mob { id: 5, offset: 16 });

        let mut reserved = header_bytes(0, 8, 0, 0);
        reserved[60] = 1;
        assert_eq!(
            Header::parse(&reserved).validate(4096),
            Err(HeaderError::Reserved)
        );
        assert_eq!(
            Header::parse(&header_bytes(0, 6, 0, 0)).validate(4096),
            Err(HeaderError::Unaligned)
        );
        assert_eq!(
            Header::parse(&header_bytes(0, 8, 0, 12)).validate(4096),
            Err(HeaderError::OffsetBeyondEnd {
                offset: 12,
                length: 8
            })
        );
        assert_eq!(
            Header::parse(&header_bytes(1 << 9, 8, 0, 0)).validate(4096),
            Err(HeaderError::UnknownFlags(1 << 9))
        );
    }

    #[test]
    fn device_commands() {
        let words = [
            cb::dc::START_STOP_CONTEXT,
            1,
            0,
            cb::dc::NOP,
            cb::dc::PREEMPT,
            1,
            1,
            cb::dc::EMPTY_CONTEXT_QUEUE,
            0,
        ];
        assert_eq!(
            decode_device(&words),
            Ok(vec![
                (
                    0,
                    DeviceCommand::StartStop {
                        enable: true,
                        context: 0
                    }
                ),
                (12, DeviceCommand::Nop),
                (
                    16,
                    DeviceCommand::Preempt {
                        context: 1,
                        ignore_id_zero: true
                    }
                ),
                (28, DeviceCommand::EmptyQueue { context: 0 }),
            ])
        );
        assert_eq!(
            decode_device(&[cb::dc::NOP, 99]),
            Err(DeviceCommandError { offset: 4, id: 99 })
        );
        assert_eq!(
            decode_device(&[cb::dc::START_STOP_CONTEXT, 1]),
            Err(DeviceCommandError {
                offset: 0,
                id: cb::dc::START_STOP_CONTEXT
            }),
            "truncated"
        );
    }

    #[test]
    fn buffers_decode_until_the_first_bad_command() {
        let limits = buffer_limits(&Limits::default());
        let words = [cmd::UPDATE, 1, 2, 3, 4, cmd::FENCE, 9, 12345, 0];
        let (entries, error) = decode_buffer(&words, 0, &limits);
        assert_eq!(
            entries,
            vec![
                Entry {
                    offset: 0,
                    command: Command::Update(Rect {
                        x: 1,
                        y: 2,
                        width: 3,
                        height: 4
                    })
                },
                Entry {
                    offset: 20,
                    command: Command::Fence(9)
                },
            ]
        );
        assert_eq!(
            error,
            Some((28, BufferError::Invalid(DecodeError::UnknownCommand(12345))))
        );

        // Restarting past the update, and a buffer cut in a command.
        let (entries, error) = decode_buffer(&words[..7], 20, &limits);
        assert_eq!(entries.len(), 1);
        assert_eq!(error, None);
        let (_, error) = decode_buffer(&words[..3], 0, &limits);
        assert_eq!(
            error,
            Some((
                0,
                BufferError::Invalid(DecodeError::CommandTooLarge {
                    id: cmd::UPDATE,
                    words: 5
                })
            )),
            "cut short"
        );
    }
}
