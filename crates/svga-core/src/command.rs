//! Decoded FIFO commands.
//!
//! Every value here is an owned copy of guest data: once a [`Command`] exists,
//! the guest can no longer change it behind the device's back. Fields are
//! *structurally* valid (sizes add up, nothing overflows) but not
//! *semantically* checked: whether a rectangle fits the framebuffer or a
//! screen ID exists is decided by the device model that executes the command.

/// An unsigned rectangle, as used by the legacy framebuffer commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// A signed point (`SVGASignedPoint`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

/// A signed rectangle with exclusive right/bottom edges (`SVGASignedRect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge (exclusive).
    pub right: i32,
    /// Bottom edge (exclusive).
    pub bottom: i32,
}

/// A pointer into a guest memory region (`SVGAGuestPtr`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestPtr {
    /// GMR ID, or one of the special IDs in [`svga_proto::gmr`].
    pub gmr_id: u32,
    /// Byte offset inside the GMR.
    pub offset: u32,
}

/// A guest image: a pointer plus a pitch (`SVGAGuestImage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestImage {
    /// Start of the image.
    pub ptr: GuestPtr,
    /// Bytes per scanline; 0 means "computed from the width".
    pub pitch: u32,
}

/// A screen object (`SVGAScreenObject`). Older drivers send shorter
/// versions; missing fields are `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenObject {
    /// `structSize` as sent by the guest, in bytes.
    pub struct_size: u32,
    /// Screen ID.
    pub id: u32,
    /// Bits from [`svga_proto::screen_flag`].
    pub flags: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Position of the screen in the virtual desktop.
    pub root: Point,
    /// Backing store (version 2 and later).
    pub backing_store: Option<GuestImage>,
    /// Number of clones (version 3 and later).
    pub clone_count: Option<u32>,
}

/// Where `REMAP_GMR2` takes its page list from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageSource {
    /// The page list lives in another GMR.
    ViaGmr(GuestPtr),
    /// The page numbers are inline, widened to 64 bits.
    Inline(Vec<u64>),
}

/// One decoded FIFO command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `SVGA_CMD_UPDATE`: a framebuffer area changed.
    Update(Rect),
    /// `SVGA_CMD_UPDATE_VERBOSE`: same, with a debugging reason.
    UpdateVerbose {
        /// Changed area.
        rect: Rect,
        /// Driver-defined reason code.
        reason: u32,
    },
    /// `SVGA_CMD_RECT_COPY`: copy inside the framebuffer.
    RectCopy {
        /// Source left edge.
        src_x: u32,
        /// Source top edge.
        src_y: u32,
        /// Destination area.
        dst: Rect,
    },
    /// `SVGA_CMD_RECT_ROP_COPY`: copy with a raster operation.
    RectRopCopy {
        /// Source left edge.
        src_x: u32,
        /// Source top edge.
        src_y: u32,
        /// Destination area.
        dst: Rect,
        /// Raster operation.
        rop: u32,
    },
    /// `SVGA_CMD_FRONT_ROP_FILL`: fill an area of the front buffer.
    FrontRopFill {
        /// Fill colour, in framebuffer format.
        color: u32,
        /// Filled area.
        rect: Rect,
        /// Raster operation (must be [`svga_proto::ROP_COPY`]).
        rop: u32,
    },
    /// `SVGA_CMD_DEFINE_CURSOR`: monochrome or colour AND/XOR cursor.
    DefineCursor(Cursor),
    /// `SVGA_CMD_DEFINE_ALPHA_CURSOR`: 32-bit ARGB cursor.
    DefineAlphaCursor {
        /// Hotspot position.
        hotspot_x: u32,
        /// Hotspot position.
        hotspot_y: u32,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// `width * height` ARGB pixels.
        pixels: Vec<u32>,
    },
    /// `SVGA_CMD_FENCE`.
    Fence(u32),
    /// `SVGA_CMD_ESCAPE`: namespaced, driver-specific data.
    Escape {
        /// Namespace ID.
        nsid: u32,
        /// Exactly `size` bytes of payload.
        data: Vec<u8>,
    },
    /// `SVGA_CMD_DEFINE_SCREEN`.
    DefineScreen(ScreenObject),
    /// `SVGA_CMD_DESTROY_SCREEN`.
    DestroyScreen {
        /// Screen to destroy.
        id: u32,
    },
    /// `SVGA_CMD_DEFINE_GMRFB`: select the source/destination of blits.
    DefineGmrFb {
        /// Start of the image.
        ptr: GuestPtr,
        /// Bytes per scanline.
        bytes_per_line: u32,
        /// Bits per pixel (low byte of the format word).
        bits_per_pixel: u8,
        /// Colour depth (second byte of the format word).
        color_depth: u8,
        /// Upper 16 bits of the format word, which must be zero.
        format_reserved: u16,
    },
    /// `SVGA_CMD_BLIT_GMRFB_TO_SCREEN`.
    BlitGmrFbToScreen {
        /// Origin inside the GMRFB.
        src_origin: Point,
        /// Destination area on the screen.
        dst_rect: SignedRect,
        /// Destination screen.
        dst_screen: u32,
    },
    /// `SVGA_CMD_BLIT_SCREEN_TO_GMRFB`.
    BlitScreenToGmrFb {
        /// Origin inside the GMRFB.
        dst_origin: Point,
        /// Source area on the screen.
        src_rect: SignedRect,
        /// Source screen.
        src_screen: u32,
    },
    /// `SVGA_CMD_ANNOTATION_FILL`: hint for the next blit.
    AnnotationFill {
        /// BGRX colour.
        color: u32,
    },
    /// `SVGA_CMD_ANNOTATION_COPY`: hint for the next blit.
    AnnotationCopy {
        /// Source origin.
        src_origin: Point,
        /// Source screen.
        src_screen: u32,
    },
    /// `SVGA_CMD_DEFINE_GMR2`: create or resize a guest memory region.
    DefineGmr2 {
        /// GMR ID.
        gmr_id: u32,
        /// Size in pages (0 destroys the GMR).
        num_pages: u32,
    },
    /// `SVGA_CMD_REMAP_GMR2`: set the pages backing part of a GMR.
    RemapGmr2 {
        /// GMR ID.
        gmr_id: u32,
        /// Bits from [`svga_proto::remap_gmr2`].
        flags: u32,
        /// First page to remap.
        offset_pages: u32,
        /// Number of pages to remap.
        num_pages: u32,
        /// The new page numbers.
        pages: PageSource,
    },
    /// `SVGA_CMD_NOP`.
    Nop,
    /// `SVGA_CMD_NOP_ERROR`: the device must raise an error.
    NopError,
    /// A 3D-framed command (`id` in `[LEGACY_BASE, FUTURE_MAX)`), not yet
    /// interpreted. The 3D layer validates the ID and the payload.
    Command3d {
        /// 3D command ID.
        id: u32,
        /// Payload size in bytes, as announced by the guest.
        size: u32,
        /// Payload, padded with zeroes to a whole number of words.
        payload: Vec<u32>,
    },
}

/// An AND/XOR cursor (`SVGA_CMD_DEFINE_CURSOR`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Reserved, should be zero.
    pub id: u32,
    /// Hotspot position.
    pub hotspot_x: u32,
    /// Hotspot position.
    pub hotspot_y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per pixel of the AND mask (1 or the framebuffer depth).
    pub and_mask_depth: u32,
    /// Bits per pixel of the XOR mask (1 or the framebuffer depth).
    pub xor_mask_depth: u32,
    /// AND mask, each scanline padded to 32 bits.
    pub and_mask: Vec<u32>,
    /// XOR mask, each scanline padded to 32 bits.
    pub xor_mask: Vec<u32>,
}
