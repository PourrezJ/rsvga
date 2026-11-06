//! Register-level constants of the VMware SVGA II virtual device.
//!
//! Values are transcribed from the VMware SVGA Device Developer Kit headers
//! (`svga_reg.h`, `svga3d_cmd.h`), Copyright 1998-2015 VMware, Inc., released
//! under the MIT license (see `VMWARE-NOTICE.txt`).
//!
//! This crate holds plain numbers only: no behaviour, no I/O, no allocation.
//! Register indices are kept as `u32` constants rather than enums because the
//! guest may write any index and the device must handle unknown ones.

#![no_std]

/// PCI identity and BAR layout of the SVGA II device.
pub mod pci {
    /// VMware's PCI vendor ID.
    pub const VENDOR_ID_VMWARE: u16 = 0x15ad;
    /// SVGA II device ID.
    pub const DEVICE_ID_SVGA2: u16 = 0x0405;
    /// BAR 0: I/O ports (index/value pair, BIOS, IRQ status).
    pub const BAR_IO: u8 = 0;
    /// BAR 1: guest framebuffer (VRAM).
    pub const BAR_VRAM: u8 = 1;
    /// BAR 2: FIFO memory.
    pub const BAR_FIFO: u8 = 2;
}

/// Device version IDs negotiated through [`reg::ID`].
pub mod id {
    /// Magic prefix of every SVGA ID.
    pub const MAGIC: u32 = 0x0090_0000;

    /// Builds the ID of a given device version.
    pub const fn make(version: u32) -> u32 {
        (MAGIC << 8) | version
    }

    /// Version 0, the initial version.
    pub const ID_0: u32 = make(0);
    /// Version 1, adds the registers starting at [`super::reg::CAPABILITIES`].
    pub const ID_1: u32 = make(1);
    /// Version 2, the current one.
    pub const ID_2: u32 = make(2);
    /// Invalid value for every kind of SVGA ID (version, screen, surface...).
    pub const INVALID: u32 = 0xffff_ffff;
}

/// I/O port offsets inside BAR 0.
pub mod port {
    /// Selects the register accessed through [`VALUE`].
    pub const INDEX: u16 = 0x0;
    /// Reads or writes the register selected by [`INDEX`].
    pub const VALUE: u16 = 0x1;
    /// BIOS port (unused by modern drivers).
    pub const BIOS: u16 = 0x2;
    /// Pending IRQ flags; writing a flag acknowledges it.
    pub const IRQ_STATUS: u16 = 0x8;
}

/// IRQ flags reported through [`port::IRQ_STATUS`] and masked by [`reg::IRQMASK`].
pub mod irq {
    /// A fence was passed.
    pub const ANY_FENCE: u32 = 0x1;
    /// The device made forward progress in the FIFO.
    pub const FIFO_PROGRESS: u32 = 0x2;
    /// [`super::fifo::FENCE_GOAL`] was reached.
    pub const FENCE_GOAL: u32 = 0x4;
    /// A command buffer completed.
    pub const COMMAND_BUFFER: u32 = 0x8;
    /// An error happened while processing commands.
    pub const ERROR: u32 = 0x10;
}

/// Device registers, accessed through the index/value port pair.
pub mod reg {
    #![allow(missing_docs)]
    pub const ID: u32 = 0;
    pub const ENABLE: u32 = 1;
    pub const WIDTH: u32 = 2;
    pub const HEIGHT: u32 = 3;
    pub const MAX_WIDTH: u32 = 4;
    pub const MAX_HEIGHT: u32 = 5;
    pub const DEPTH: u32 = 6;
    pub const BITS_PER_PIXEL: u32 = 7;
    pub const PSEUDOCOLOR: u32 = 8;
    pub const RED_MASK: u32 = 9;
    pub const GREEN_MASK: u32 = 10;
    pub const BLUE_MASK: u32 = 11;
    pub const BYTES_PER_LINE: u32 = 12;
    pub const FB_START: u32 = 13;
    pub const FB_OFFSET: u32 = 14;
    pub const VRAM_SIZE: u32 = 15;
    pub const FB_SIZE: u32 = 16;
    pub const CAPABILITIES: u32 = 17;
    pub const MEM_START: u32 = 18;
    pub const MEM_SIZE: u32 = 19;
    pub const CONFIG_DONE: u32 = 20;
    pub const SYNC: u32 = 21;
    pub const BUSY: u32 = 22;
    pub const GUEST_ID: u32 = 23;
    pub const DEAD: u32 = 24;
    pub const CURSOR_X: u32 = 25;
    pub const CURSOR_Y: u32 = 26;
    pub const CURSOR_ON: u32 = 27;
    pub const HOST_BITS_PER_PIXEL: u32 = 28;
    pub const SCRATCH_SIZE: u32 = 29;
    pub const MEM_REGS: u32 = 30;
    pub const NUM_DISPLAYS: u32 = 31;
    pub const PITCHLOCK: u32 = 32;
    pub const IRQMASK: u32 = 33;
    pub const NUM_GUEST_DISPLAYS: u32 = 34;
    pub const DISPLAY_ID: u32 = 35;
    pub const DISPLAY_IS_PRIMARY: u32 = 36;
    pub const DISPLAY_POSITION_X: u32 = 37;
    pub const DISPLAY_POSITION_Y: u32 = 38;
    pub const DISPLAY_WIDTH: u32 = 39;
    pub const DISPLAY_HEIGHT: u32 = 40;
    pub const GMR_ID: u32 = 41;
    pub const GMR_DESCRIPTOR: u32 = 42;
    pub const GMR_MAX_IDS: u32 = 43;
    pub const GMR_MAX_DESCRIPTOR_LENGTH: u32 = 44;
    pub const TRACES: u32 = 45;
    pub const GMRS_MAX_PAGES: u32 = 46;
    pub const MEMORY_SIZE: u32 = 47;
    pub const COMMAND_LOW: u32 = 48;
    pub const COMMAND_HIGH: u32 = 49;
    pub const MAX_PRIMARY_MEM: u32 = 50;
    pub const SUGGESTED_GBOBJECT_MEM_SIZE_KB: u32 = 51;
    pub const DEV_CAP: u32 = 52;
    pub const CMD_PREPEND_LOW: u32 = 53;
    pub const CMD_PREPEND_HIGH: u32 = 54;
    pub const SCREENTARGET_MAX_WIDTH: u32 = 55;
    pub const SCREENTARGET_MAX_HEIGHT: u32 = 56;
    pub const MOB_MAX_SIZE: u32 = 57;
    pub const BLANK_SCREEN_TARGETS: u32 = 58;
    pub const CAP2: u32 = 59;
    pub const DEVEL_CAP: u32 = 60;
    pub const GUEST_DRIVER_ID: u32 = 61;
    pub const GUEST_DRIVER_VERSION1: u32 = 62;
    pub const GUEST_DRIVER_VERSION2: u32 = 63;
    pub const GUEST_DRIVER_VERSION3: u32 = 64;
    pub const CURSOR_MOBID: u32 = 65;
    pub const CURSOR_MAX_BYTE_SIZE: u32 = 66;
    pub const CURSOR_MAX_DIMENSION: u32 = 67;
    pub const FIFO_CAPS: u32 = 68;
    pub const FENCE: u32 = 69;
    pub const CURSOR4_ON: u32 = 70;
    pub const CURSOR4_X: u32 = 71;
    pub const CURSOR4_Y: u32 = 72;
    pub const CURSOR4_SCREEN_ID: u32 = 73;
    pub const CURSOR4_SUBMIT: u32 = 74;
    pub const SCREENDMA: u32 = 75;
    pub const GBOBJECT_MEM_SIZE_KB: u32 = 76;
    pub const REGS_START_HIGH32: u32 = 77;
    pub const REGS_START_LOW32: u32 = 78;
    pub const FB_START_HIGH32: u32 = 79;
    pub const FB_START_LOW32: u32 = 80;
    pub const MSHINT: u32 = 81;
    pub const IRQ_STATUS: u32 = 82;
    pub const DIRTY_TRACKING: u32 = 83;
    pub const FENCE_GOAL: u32 = 84;
    /// One past the last regular register.
    pub const TOP: u32 = 85;

    /// Number of palette entries (256 colours, 3 components each).
    pub const NUM_PALETTE_REGS: u32 = 3 * 256;
    /// First palette register.
    pub const PALETTE_BASE: u32 = 1024;
    /// First scratch register, right after the palette.
    pub const SCRATCH_BASE: u32 = PALETTE_BASE + NUM_PALETTE_REGS;

    const NAMES: [&str; TOP as usize] = [
        "ID",
        "ENABLE",
        "WIDTH",
        "HEIGHT",
        "MAX_WIDTH",
        "MAX_HEIGHT",
        "DEPTH",
        "BITS_PER_PIXEL",
        "PSEUDOCOLOR",
        "RED_MASK",
        "GREEN_MASK",
        "BLUE_MASK",
        "BYTES_PER_LINE",
        "FB_START",
        "FB_OFFSET",
        "VRAM_SIZE",
        "FB_SIZE",
        "CAPABILITIES",
        "MEM_START",
        "MEM_SIZE",
        "CONFIG_DONE",
        "SYNC",
        "BUSY",
        "GUEST_ID",
        "DEAD",
        "CURSOR_X",
        "CURSOR_Y",
        "CURSOR_ON",
        "HOST_BITS_PER_PIXEL",
        "SCRATCH_SIZE",
        "MEM_REGS",
        "NUM_DISPLAYS",
        "PITCHLOCK",
        "IRQMASK",
        "NUM_GUEST_DISPLAYS",
        "DISPLAY_ID",
        "DISPLAY_IS_PRIMARY",
        "DISPLAY_POSITION_X",
        "DISPLAY_POSITION_Y",
        "DISPLAY_WIDTH",
        "DISPLAY_HEIGHT",
        "GMR_ID",
        "GMR_DESCRIPTOR",
        "GMR_MAX_IDS",
        "GMR_MAX_DESCRIPTOR_LENGTH",
        "TRACES",
        "GMRS_MAX_PAGES",
        "MEMORY_SIZE",
        "COMMAND_LOW",
        "COMMAND_HIGH",
        "MAX_PRIMARY_MEM",
        "SUGGESTED_GBOBJECT_MEM_SIZE_KB",
        "DEV_CAP",
        "CMD_PREPEND_LOW",
        "CMD_PREPEND_HIGH",
        "SCREENTARGET_MAX_WIDTH",
        "SCREENTARGET_MAX_HEIGHT",
        "MOB_MAX_SIZE",
        "BLANK_SCREEN_TARGETS",
        "CAP2",
        "DEVEL_CAP",
        "GUEST_DRIVER_ID",
        "GUEST_DRIVER_VERSION1",
        "GUEST_DRIVER_VERSION2",
        "GUEST_DRIVER_VERSION3",
        "CURSOR_MOBID",
        "CURSOR_MAX_BYTE_SIZE",
        "CURSOR_MAX_DIMENSION",
        "FIFO_CAPS",
        "FENCE",
        "CURSOR4_ON",
        "CURSOR4_X",
        "CURSOR4_Y",
        "CURSOR4_SCREEN_ID",
        "CURSOR4_SUBMIT",
        "SCREENDMA",
        "GBOBJECT_MEM_SIZE_KB",
        "REGS_START_HIGH32",
        "REGS_START_LOW32",
        "FB_START_HIGH32",
        "FB_START_LOW32",
        "MSHINT",
        "IRQ_STATUS",
        "DIRTY_TRACKING",
        "FENCE_GOAL",
    ];

    /// Name of a regular register, for tracing. `None` for palette, scratch
    /// and unknown indices.
    pub fn name(index: u32) -> Option<&'static str> {
        NAMES.get(usize::try_from(index).ok()?).copied()
    }
}

/// Bits of [`reg::CAPABILITIES`].
pub mod cap {
    #![allow(missing_docs)]
    pub const NONE: u32 = 0x0000_0000;
    pub const RECT_COPY: u32 = 0x0000_0002;
    pub const CURSOR: u32 = 0x0000_0020;
    pub const CURSOR_BYPASS: u32 = 0x0000_0040;
    pub const CURSOR_BYPASS_2: u32 = 0x0000_0080;
    pub const EIGHT_BIT_EMULATION: u32 = 0x0000_0100;
    pub const ALPHA_CURSOR: u32 = 0x0000_0200;
    pub const THREE_D: u32 = 0x0000_4000;
    pub const EXTENDED_FIFO: u32 = 0x0000_8000;
    pub const MULTIMON: u32 = 0x0001_0000;
    pub const PITCHLOCK: u32 = 0x0002_0000;
    pub const IRQMASK: u32 = 0x0004_0000;
    pub const DISPLAY_TOPOLOGY: u32 = 0x0008_0000;
    pub const GMR: u32 = 0x0010_0000;
    pub const TRACES: u32 = 0x0020_0000;
    pub const GMR2: u32 = 0x0040_0000;
    pub const SCREEN_OBJECT_2: u32 = 0x0080_0000;
    pub const COMMAND_BUFFERS: u32 = 0x0100_0000;
    pub const DEAD1: u32 = 0x0200_0000;
    pub const CMD_BUFFERS_2: u32 = 0x0400_0000;
    pub const GBOBJECTS: u32 = 0x0800_0000;
    pub const DX: u32 = 0x1000_0000;
    pub const CMD_BUFFERS_3: u32 = DX;
    pub const HP_CMD_QUEUE: u32 = 0x2000_0000;
    pub const NO_BB_RESTRICTION: u32 = 0x4000_0000;
    pub const CAP2_REGISTER: u32 = 0x8000_0000;
}

/// Bits of [`reg::CAP2`].
pub mod cap2 {
    #![allow(missing_docs)]
    pub const NONE: u32 = 0x0000_0000;
    pub const GROW_OTABLE: u32 = 0x0000_0001;
    pub const INTRA_SURFACE_COPY: u32 = 0x0000_0002;
    pub const DX2: u32 = 0x0000_0004;
    pub const GB_MEMSIZE_2: u32 = 0x0000_0008;
    pub const SCREENDMA_REG: u32 = 0x0000_0010;
    pub const OTABLE_PTDEPTH_2: u32 = 0x0000_0020;
    pub const NON_MS_TO_MS_STRETCHBLT: u32 = 0x0000_0040;
    pub const CURSOR_MOB: u32 = 0x0000_0080;
    pub const MSHINT: u32 = 0x0000_0100;
    pub const CB_MAX_SIZE_4MB: u32 = 0x0000_0200;
    pub const DX3: u32 = 0x0000_0400;
    pub const FRAME_TYPE: u32 = 0x0000_0800;
    pub const COTABLE_COPY: u32 = 0x0000_1000;
    pub const TRACE_FULL_FB: u32 = 0x0000_2000;
    pub const EXTRA_REGS: u32 = 0x0000_4000;
    pub const LO_STAGING: u32 = 0x0000_8000;
    pub const VIDEO_BLT: u32 = 0x0001_0000;
    pub const RESERVED: u32 = 0x8000_0000;
}

/// FIFO registers: 32-bit word indices at the start of FIFO memory.
///
/// Registers from [`fifo::CAPABILITIES`] onwards exist only when the guest
/// sets [`fifo::MIN`] high enough to leave room for them.
pub mod fifo {
    #![allow(missing_docs)]
    pub const MIN: u32 = 0;
    pub const MAX: u32 = 1;
    pub const NEXT_CMD: u32 = 2;
    pub const STOP: u32 = 3;
    pub const CAPABILITIES: u32 = 4;
    pub const FLAGS: u32 = 5;
    pub const FENCE: u32 = 6;
    pub const HWVERSION_3D: u32 = 7;
    pub const PITCHLOCK: u32 = 8;
    pub const CURSOR_ON: u32 = 9;
    pub const CURSOR_X: u32 = 10;
    pub const CURSOR_Y: u32 = 11;
    pub const CURSOR_COUNT: u32 = 12;
    pub const CURSOR_LAST_UPDATED: u32 = 13;
    pub const RESERVED: u32 = 14;
    pub const CURSOR_SCREEN_ID: u32 = 15;
    pub const DEAD: u32 = 16;
    pub const HWVERSION_3D_REVISED: u32 = 17;
    pub const CAPS_3D: u32 = 32;
    pub const CAPS_3D_LAST: u32 = 32 + 255;
    pub const GUEST_3D_HWVERSION: u32 = 288;
    pub const FENCE_GOAL: u32 = 289;
    pub const BUSY: u32 = 290;
    /// Number of FIFO registers known to this version of the protocol.
    pub const NUM_REGS: u32 = 291;
    /// Registers guaranteed present when the extended FIFO is in use.
    pub const EXTENDED_MANDATORY_REGS: u32 = CAPS_3D_LAST + 1;
    /// The four registers that always exist (MIN, MAX, NEXT_CMD, STOP).
    pub const MANDATORY_REGS: u32 = 4;
}

/// Bits of [`fifo::CAPABILITIES`].
pub mod fifo_cap {
    #![allow(missing_docs)]
    pub const NONE: u32 = 0;
    pub const FENCE: u32 = 1 << 0;
    pub const ACCELFRONT: u32 = 1 << 1;
    pub const PITCHLOCK: u32 = 1 << 2;
    pub const VIDEO: u32 = 1 << 3;
    pub const CURSOR_BYPASS_3: u32 = 1 << 4;
    pub const ESCAPE: u32 = 1 << 5;
    pub const RESERVE: u32 = 1 << 6;
    pub const SCREEN_OBJECT: u32 = 1 << 7;
    pub const GMR2: u32 = 1 << 8;
    pub const HWVERSION_3D_REVISED: u32 = GMR2;
    pub const SCREEN_OBJECT_2: u32 = 1 << 9;
    pub const DEAD: u32 = 1 << 10;
}

/// Bits of [`fifo::FLAGS`].
pub mod fifo_flag {
    #![allow(missing_docs)]
    pub const NONE: u32 = 0;
    pub const ACCELFRONT: u32 = 1 << 0;
    pub const RESERVED: u32 = 1 << 31;
}

/// 2D FIFO command IDs (the first word of each command).
pub mod cmd {
    #![allow(missing_docs)]
    pub const INVALID_CMD: u32 = 0;
    pub const UPDATE: u32 = 1;
    pub const RECT_COPY: u32 = 3;
    pub const RECT_ROP_COPY: u32 = 14;
    pub const DEFINE_CURSOR: u32 = 19;
    pub const DEFINE_ALPHA_CURSOR: u32 = 22;
    pub const UPDATE_VERBOSE: u32 = 25;
    pub const FRONT_ROP_FILL: u32 = 29;
    pub const FENCE: u32 = 30;
    pub const ESCAPE: u32 = 33;
    pub const DEFINE_SCREEN: u32 = 34;
    pub const DESTROY_SCREEN: u32 = 35;
    pub const DEFINE_GMRFB: u32 = 36;
    pub const BLIT_GMRFB_TO_SCREEN: u32 = 37;
    pub const BLIT_SCREEN_TO_GMRFB: u32 = 38;
    pub const ANNOTATION_FILL: u32 = 39;
    pub const ANNOTATION_COPY: u32 = 40;
    pub const DEFINE_GMR2: u32 = 41;
    pub const REMAP_GMR2: u32 = 42;
    pub const DEAD: u32 = 43;
    pub const DEAD_2: u32 = 44;
    pub const NOP: u32 = 45;
    pub const NOP_ERROR: u32 = 46;
    pub const MAX: u32 = 47;

    /// Largest payload a single FIFO command may carry, in bytes.
    pub const MAX_DATASIZE: u32 = 256 * 1024;
    /// Largest number of fixed arguments of a 2D command.
    pub const MAX_ARGS: u32 = 64;
}

/// 3D command ID ranges. 3D commands are framed as `{ id, size_in_bytes }`
/// followed by `size_in_bytes` of payload.
pub mod cmd3d {
    /// First ID of the (obsolete) legacy 3D range.
    pub const LEGACY_BASE: u32 = 1000;
    /// First ID of the current 3D range (`SURFACE_DEFINE`).
    pub const BASE: u32 = 1040;
    /// First 3D context command of the DX interface.
    pub const DX_DEFINE_CONTEXT: u32 = 1143;
    /// One past the last 3D command known to this protocol version.
    pub const MAX: u32 = 1303;
    /// One past the last ID reserved for 3D-framed commands.
    pub const FUTURE_MAX: u32 = 3000;
}

/// Flags of `SVGA_CMD_REMAP_GMR2`.
pub mod remap_gmr2 {
    /// Inline list of 32-bit page numbers.
    pub const PPN32: u32 = 0;
    /// The page list lives in another GMR (a single `SVGAGuestPtr` follows).
    pub const VIA_GMR: u32 = 1 << 0;
    /// Page numbers are 64-bit.
    pub const PPN64: u32 = 1 << 1;
    /// The list holds one entry, applied to every page.
    pub const SINGLE_PPN: u32 = 1 << 2;
}

/// Flags of a screen object (`SVGA_CMD_DEFINE_SCREEN`).
pub mod screen_flag {
    #![allow(missing_docs)]
    pub const MUST_BE_SET: u32 = 1 << 0;
    pub const IS_PRIMARY: u32 = 1 << 1;
    pub const FULLSCREEN_HINT: u32 = 1 << 2;
    pub const DEACTIVATE: u32 = 1 << 3;
    pub const BLANKING: u32 = 1 << 4;
}

/// Special guest memory region IDs.
pub mod gmr {
    /// No GMR.
    pub const NULL: u32 = 0xffff_ffff;
    /// The guest framebuffer (VRAM BAR).
    pub const FRAMEBUFFER: u32 = 0xffff_fffe;
}

/// Values of [`reg::CURSOR_ON`] and [`fifo::CURSOR_ON`].
pub mod cursor_on {
    #![allow(missing_docs)]
    pub const HIDE: u32 = 0;
    pub const SHOW: u32 = 1;
    pub const REMOVE_FROM_FB: u32 = 2;
    pub const RESTORE_TO_FB: u32 = 3;
}

/// The only raster operation modern commands accept.
pub const ROP_COPY: u32 = 0x03;

/// Legacy mode limits advertised by the original device.
pub mod limits {
    #![allow(missing_docs)]
    pub const MAX_WIDTH: u32 = 2560;
    pub const MAX_HEIGHT: u32 = 1600;
    pub const MAX_BITS_PER_PIXEL: u32 = 32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_the_reference_values() {
        assert_eq!(id::ID_0, 0x9000_0000);
        assert_eq!(id::ID_2, 0x9000_0002);
    }

    #[test]
    fn register_names_cover_every_regular_register() {
        assert_eq!(reg::name(reg::ID), Some("ID"));
        assert_eq!(reg::name(reg::FENCE_GOAL), Some("FENCE_GOAL"));
        assert_eq!(reg::name(reg::TOP), None);
        assert_eq!(reg::name(u32::MAX), None);
        assert_eq!(reg::SCRATCH_BASE, 1792);
    }

    #[test]
    fn fifo_register_layout() {
        assert_eq!(fifo::EXTENDED_MANDATORY_REGS, 288);
        assert_eq!(fifo::NUM_REGS, fifo::BUSY + 1);
    }
}
