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

pub mod cmd3d_names;
pub mod format;

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
#[allow(missing_docs)]
pub mod cmd3d {
    /// First ID of the (obsolete) legacy 3D range.
    pub const LEGACY_BASE: u32 = 1000;
    /// First ID of the current 3D range (`SURFACE_DEFINE`).
    pub const BASE: u32 = 1040;
    /// First 3D context command of the DX interface.
    pub const DX_DEFINE_CONTEXT: u32 = 1143;

    // Legacy 3D contexts (VMware's Direct3D 9 driver): render states,
    // texture stages, the fixed-function pipeline, Direct3D 9 shaders.
    pub const SURFACE_STRETCHBLT: u32 = 1043;
    pub const CONTEXT_DEFINE: u32 = 1045;
    pub const CONTEXT_DESTROY: u32 = 1046;
    pub const SETTRANSFORM: u32 = 1047;
    pub const SETZRANGE: u32 = 1048;
    pub const SETRENDERSTATE: u32 = 1049;
    pub const SETRENDERTARGET: u32 = 1050;
    pub const SETTEXTURESTATE: u32 = 1051;
    pub const SETMATERIAL: u32 = 1052;
    pub const SETLIGHTDATA: u32 = 1053;
    pub const SETLIGHTENABLED: u32 = 1054;
    pub const SETVIEWPORT: u32 = 1055;
    pub const SETCLIPPLANE: u32 = 1056;
    pub const CLEAR: u32 = 1057;
    pub const SHADER_DEFINE: u32 = 1059;
    pub const SHADER_DESTROY: u32 = 1060;
    pub const SET_SHADER: u32 = 1061;
    pub const SET_SHADER_CONST: u32 = 1062;
    pub const DRAW_PRIMITIVES: u32 = 1063;
    pub const SETSCISSORRECT: u32 = 1064;
    pub const GENERATE_MIPMAPS: u32 = 1071;
    pub const DEFINE_GB_CONTEXT: u32 = 1107;
    pub const DESTROY_GB_CONTEXT: u32 = 1108;
    pub const BIND_GB_CONTEXT: u32 = 1109;
    pub const READBACK_GB_CONTEXT: u32 = 1110;
    pub const INVALIDATE_GB_CONTEXT: u32 = 1111;
    pub const DEFINE_GB_SHADER: u32 = 1112;
    pub const DESTROY_GB_SHADER: u32 = 1113;
    pub const BIND_GB_SHADER: u32 = 1114;
    pub const BEGIN_GB_QUERY: u32 = 1116;
    pub const END_GB_QUERY: u32 = 1117;
    pub const WAIT_FOR_GB_QUERY: u32 = 1118;
    pub const SET_GB_SHADERCONSTS_INLINE: u32 = 1130;

    // Guest-backed objects: memory objects (MOBs), object tables, surfaces
    // and screen targets.
    pub const SURFACE_COPY: u32 = 1042;
    pub const SET_OTABLE_BASE: u32 = 1091;
    pub const READBACK_OTABLE: u32 = 1092;
    pub const DEFINE_GB_MOB: u32 = 1093;
    pub const DESTROY_GB_MOB: u32 = 1094;
    pub const DEFINE_GB_SURFACE: u32 = 1097;
    pub const DESTROY_GB_SURFACE: u32 = 1098;
    pub const BIND_GB_SURFACE: u32 = 1099;
    pub const COND_BIND_GB_SURFACE: u32 = 1100;
    pub const UPDATE_GB_IMAGE: u32 = 1101;
    pub const UPDATE_GB_SURFACE: u32 = 1102;
    pub const READBACK_GB_IMAGE: u32 = 1103;
    pub const READBACK_GB_SURFACE: u32 = 1104;
    pub const INVALIDATE_GB_IMAGE: u32 = 1105;
    pub const INVALIDATE_GB_SURFACE: u32 = 1106;
    pub const SET_OTABLE_BASE64: u32 = 1115;
    pub const NOP: u32 = 1119;
    pub const ENABLE_GART: u32 = 1120;
    pub const DISABLE_GART: u32 = 1121;
    pub const MAP_MOB_INTO_GART: u32 = 1122;
    pub const UNMAP_GART_RANGE: u32 = 1123;
    pub const DEFINE_GB_SCREENTARGET: u32 = 1124;
    pub const DESTROY_GB_SCREENTARGET: u32 = 1125;
    pub const BIND_GB_SCREENTARGET: u32 = 1126;
    pub const UPDATE_GB_SCREENTARGET: u32 = 1127;
    pub const READBACK_GB_IMAGE_PARTIAL: u32 = 1128;
    pub const INVALIDATE_GB_IMAGE_PARTIAL: u32 = 1129;
    pub const GB_SCREEN_DMA: u32 = 1131;
    pub const BIND_GB_SURFACE_WITH_PITCH: u32 = 1132;
    pub const GB_MOB_FENCE: u32 = 1133;
    pub const DEFINE_GB_SURFACE_V2: u32 = 1134;
    pub const DEFINE_GB_MOB64: u32 = 1135;
    pub const REDEFINE_GB_MOB64: u32 = 1136;
    pub const NOP_ERROR: u32 = 1137;
    /// Copies (and scales) a surface image to another: how the WDDM driver
    /// presents when 3D is on, whether or not DirectX runs.
    pub const DX_PRESENTBLT: u32 = 1180;
    pub const GROW_OTABLE: u32 = 1236;
    pub const DEFINE_GB_SURFACE_V3: u32 = 1239;
    pub const UPDATE_ZERO_SURFACE: u32 = 1256;
    pub const DEFINE_GB_SURFACE_V4: u32 = 1267;

    // DirectX contexts (DX_DEFINE_CONTEXT and DX_PRESENTBLT are above).
    pub const DX_DESTROY_CONTEXT: u32 = 1144;
    pub const DX_BIND_CONTEXT: u32 = 1145;
    pub const DX_READBACK_CONTEXT: u32 = 1146;
    pub const DX_INVALIDATE_CONTEXT: u32 = 1147;
    pub const DX_SET_SINGLE_CONSTANT_BUFFER: u32 = 1148;
    pub const DX_SET_SHADER_RESOURCES: u32 = 1149;
    pub const DX_SET_SHADER: u32 = 1150;
    pub const DX_SET_SAMPLERS: u32 = 1151;
    pub const DX_DRAW: u32 = 1152;
    pub const DX_DRAW_INDEXED: u32 = 1153;
    pub const DX_DRAW_INSTANCED: u32 = 1154;
    pub const DX_DRAW_INDEXED_INSTANCED: u32 = 1155;
    pub const DX_DRAW_AUTO: u32 = 1156;
    pub const DX_SET_INPUT_LAYOUT: u32 = 1157;
    pub const DX_SET_VERTEX_BUFFERS: u32 = 1158;
    pub const DX_SET_INDEX_BUFFER: u32 = 1159;
    pub const DX_SET_TOPOLOGY: u32 = 1160;
    pub const DX_SET_RENDERTARGETS: u32 = 1161;
    pub const DX_SET_BLEND_STATE: u32 = 1162;
    pub const DX_SET_DEPTHSTENCIL_STATE: u32 = 1163;
    pub const DX_SET_RASTERIZER_STATE: u32 = 1164;
    pub const DX_DEFINE_QUERY: u32 = 1165;
    pub const DX_DESTROY_QUERY: u32 = 1166;
    pub const DX_BIND_QUERY: u32 = 1167;
    pub const DX_SET_QUERY_OFFSET: u32 = 1168;
    pub const DX_BEGIN_QUERY: u32 = 1169;
    pub const DX_END_QUERY: u32 = 1170;
    pub const DX_READBACK_QUERY: u32 = 1171;
    pub const DX_SET_PREDICATION: u32 = 1172;
    pub const DX_SET_SOTARGETS: u32 = 1173;
    pub const DX_SET_VIEWPORTS: u32 = 1174;
    pub const DX_SET_SCISSORRECTS: u32 = 1175;
    pub const DX_CLEAR_RENDERTARGET_VIEW: u32 = 1176;
    pub const DX_CLEAR_DEPTHSTENCIL_VIEW: u32 = 1177;
    pub const DX_PRED_COPY_REGION: u32 = 1178;
    pub const DX_PRED_COPY: u32 = 1179;
    pub const DX_GENMIPS: u32 = 1181;
    pub const DX_UPDATE_SUBRESOURCE: u32 = 1182;
    pub const DX_READBACK_SUBRESOURCE: u32 = 1183;
    pub const DX_INVALIDATE_SUBRESOURCE: u32 = 1184;
    pub const DX_DEFINE_SHADERRESOURCE_VIEW: u32 = 1185;
    pub const DX_DESTROY_SHADERRESOURCE_VIEW: u32 = 1186;
    pub const DX_DEFINE_RENDERTARGET_VIEW: u32 = 1187;
    pub const DX_DESTROY_RENDERTARGET_VIEW: u32 = 1188;
    pub const DX_DEFINE_DEPTHSTENCIL_VIEW: u32 = 1189;
    pub const DX_DESTROY_DEPTHSTENCIL_VIEW: u32 = 1190;
    pub const DX_DEFINE_ELEMENTLAYOUT: u32 = 1191;
    pub const DX_DESTROY_ELEMENTLAYOUT: u32 = 1192;
    pub const DX_DEFINE_BLEND_STATE: u32 = 1193;
    pub const DX_DESTROY_BLEND_STATE: u32 = 1194;
    pub const DX_DEFINE_DEPTHSTENCIL_STATE: u32 = 1195;
    pub const DX_DESTROY_DEPTHSTENCIL_STATE: u32 = 1196;
    pub const DX_DEFINE_RASTERIZER_STATE: u32 = 1197;
    pub const DX_DESTROY_RASTERIZER_STATE: u32 = 1198;
    pub const DX_DEFINE_SAMPLER_STATE: u32 = 1199;
    pub const DX_DESTROY_SAMPLER_STATE: u32 = 1200;
    pub const DX_DEFINE_SHADER: u32 = 1201;
    pub const DX_DESTROY_SHADER: u32 = 1202;
    pub const DX_BIND_SHADER: u32 = 1203;
    pub const DX_DEFINE_STREAMOUTPUT: u32 = 1204;
    pub const DX_DESTROY_STREAMOUTPUT: u32 = 1205;
    pub const DX_SET_STREAMOUTPUT: u32 = 1206;
    pub const DX_SET_COTABLE: u32 = 1207;
    pub const DX_READBACK_COTABLE: u32 = 1208;
    pub const DX_BUFFER_COPY: u32 = 1209;
    pub const DX_TRANSFER_FROM_BUFFER: u32 = 1210;
    pub const DX_SURFACE_COPY_AND_READBACK: u32 = 1211;
    pub const DX_MOVE_QUERY: u32 = 1212;
    pub const DX_BIND_ALL_QUERY: u32 = 1213;
    pub const DX_READBACK_ALL_QUERY: u32 = 1214;
    pub const DX_PRED_TRANSFER_FROM_BUFFER: u32 = 1215;
    pub const DX_MOB_FENCE_64: u32 = 1216;
    pub const DX_BIND_ALL_SHADER: u32 = 1217;
    pub const DX_HINT: u32 = 1218;
    pub const DX_BUFFER_UPDATE: u32 = 1219;
    pub const DX_SET_VS_CONSTANT_BUFFER_OFFSET: u32 = 1220;
    pub const DX_SET_PS_CONSTANT_BUFFER_OFFSET: u32 = 1221;
    pub const DX_SET_GS_CONSTANT_BUFFER_OFFSET: u32 = 1222;
    pub const DX_SET_HS_CONSTANT_BUFFER_OFFSET: u32 = 1223;
    pub const DX_SET_DS_CONSTANT_BUFFER_OFFSET: u32 = 1224;
    pub const DX_SET_CS_CONSTANT_BUFFER_OFFSET: u32 = 1225;
    pub const DX_COND_BIND_ALL_SHADER: u32 = 1226;
    pub const DX_GROW_COTABLE: u32 = 1237;
    pub const DX_RESOLVE_COPY: u32 = 1240;
    pub const DX_PRED_RESOLVE_COPY: u32 = 1241;
    pub const DX_PRED_CONVERT_REGION: u32 = 1242;
    pub const DX_PRED_CONVERT: u32 = 1243;
    pub const DX_DEFINE_UA_VIEW: u32 = 1245;
    pub const DX_DESTROY_UA_VIEW: u32 = 1246;
    pub const DX_CLEAR_UA_VIEW_UINT: u32 = 1247;
    pub const DX_CLEAR_UA_VIEW_FLOAT: u32 = 1248;
    pub const DX_COPY_STRUCTURE_COUNT: u32 = 1249;
    pub const DX_SET_UA_VIEWS: u32 = 1250;
    pub const DX_DRAW_INDEXED_INSTANCED_INDIRECT: u32 = 1251;
    pub const DX_DRAW_INSTANCED_INDIRECT: u32 = 1252;
    pub const DX_DISPATCH: u32 = 1253;
    pub const DX_DISPATCH_INDIRECT: u32 = 1254;
    pub const DX_TRANSFER_TO_BUFFER: u32 = 1257;
    pub const DX_SET_STRUCTURE_COUNT: u32 = 1258;
    pub const DX_COPY_COTABLE_INTO_MOB: u32 = 1265;
    pub const DX_SET_CS_UA_VIEWS: u32 = 1268;
    pub const DX_SET_MIN_LOD: u32 = 1269;
    pub const DX_DEFINE_DEPTHSTENCIL_VIEW_V2: u32 = 1272;
    pub const DX_DEFINE_STREAMOUTPUT_WITH_MOB: u32 = 1273;
    pub const DX_SET_SHADER_IFACE: u32 = 1274;
    pub const DX_BIND_STREAMOUTPUT: u32 = 1275;
    pub const DX_BIND_SHADER_IFACE: u32 = 1277;
    pub const DX_PRED_STAGING_COPY: u32 = 1281;
    pub const DX_STAGING_COPY: u32 = 1282;
    pub const DX_PRED_STAGING_COPY_REGION: u32 = 1283;
    pub const DX_SET_VERTEX_BUFFERS_V2: u32 = 1284;
    pub const DX_SET_INDEX_BUFFER_V2: u32 = 1285;
    pub const DX_SET_VERTEX_BUFFERS_OFFSET_AND_SIZE: u32 = 1286;
    pub const DX_SET_INDEX_BUFFER_OFFSET_AND_SIZE: u32 = 1287;
    pub const DX_DEFINE_RASTERIZER_STATE_V2: u32 = 1288;
    pub const DX_PRED_STAGING_CONVERT_REGION: u32 = 1289;
    pub const DX_PRED_STAGING_CONVERT: u32 = 1290;
    pub const DX_STAGING_BUFFER_COPY: u32 = 1291;
    /// A video processor blit. Not in the public headers: decoded from
    /// what VMware's Direct3D 11 driver sends for `VideoProcessorBlt` on a
    /// device with `SVGA_CAP2_VIDEO_BLT` (see `svga_core::gb::VideoBlt`).
    pub const VIDEO_BLT: u32 = 1295;
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

/// The ID meaning "no object" for MOBs, surfaces, screen targets...
pub const INVALID_ID: u32 = 0xffff_ffff;

/// `SVGAMobFormat`: how a MOB (or object table) maps its pages. `PT*_0`
/// point at the data, `PT*_1` at one page of page numbers, `PT*_2` at a
/// page of page-table pages; `RANGE` is physically contiguous.
pub mod mobfmt {
    #![allow(missing_docs)]
    pub const PT_0: u32 = 0;
    pub const PT_1: u32 = 1;
    pub const PT_2: u32 = 2;
    pub const RANGE: u32 = 3;
    pub const PT64_0: u32 = 4;
    pub const PT64_1: u32 = 5;
    pub const PT64_2: u32 = 6;
    pub const EMPTY: u32 = 7;
}

/// `SVGAOTableType`: the object tables the guest hands to the device.
pub mod otable {
    #![allow(missing_docs)]
    pub const MOB: u32 = 0;
    pub const SURFACE: u32 = 1;
    pub const CONTEXT: u32 = 2;
    pub const SHADER: u32 = 3;
    pub const SCREENTARGET: u32 = 4;
    pub const DXCONTEXT: u32 = 5;
    /// Number of table types (including reserved ones).
    pub const MAX: u32 = 8;
}

/// Screen target flags (`SVGA_STFLAG_*`).
pub mod stflag {
    /// The primary screen.
    pub const PRIMARY: u32 = 1 << 0;
}

/// Surface flags used by the device (`SVGA3D_SURFACE_*`, 64-bit).
pub mod surface_flag {
    /// Six faces per layer.
    pub const CUBEMAP: u64 = 1 << 0;
    /// The device keeps a whole mip chain behind the first level
    /// (Direct3D 9 `AUTOGENMIPMAP`), filled by `GENERATE_MIPMAPS`.
    pub const AUTOGENMIPMAPS: u64 = 1 << 10;
    /// A 3D texture.
    pub const VOLUME: u64 = 1 << 15;
    /// Shown by a screen target.
    pub const SCREENTARGET: u64 = 1 << 16;
    /// A 1D texture.
    pub const ONE_D: u64 = 1 << 18;
    /// A texture array.
    pub const ARRAY: u64 = 1 << 19;
    /// Usable as a render target.
    pub const BIND_RENDER_TARGET: u64 = 1 << 24;
    /// Usable as a depth-stencil target.
    pub const BIND_DEPTH_STENCIL: u64 = 1 << 25;
    /// Multisampled.
    pub const MULTISAMPLE: u64 = 1 << 32;
    /// Usable through unordered-access views.
    pub const BIND_UAVIEW: u64 = 1 << 33;
}

/// Flags of `SVGA_3D_CMD_COND_BIND_GB_SURFACE`.
pub mod cond_bind {
    /// Copy the host contents back to the old MOB before rebinding.
    pub const READBACK: u32 = 1 << 0;
    /// Load the host contents from the new MOB after rebinding.
    pub const UPDATE: u32 = 1 << 1;
}

/// MOB cursors (`SVGA_REG_CURSOR_MOBID`): the MOB starts with an
/// `SVGAGBCursorHeader` (type, 6 header words, `sizeInBytes`), then pixels.
pub mod gb_cursor {
    /// AND/XOR masks (`SVGAGBColorCursorHeader`).
    pub const COLOR: u32 = 0;
    /// Premultiplied 0xAARRGGBB pixels (`SVGAGBAlphaCursorHeader`).
    pub const ALPHA: u32 = 1;
    /// Size of `SVGAGBCursorHeader` in bytes.
    pub const HEADER_BYTES: u32 = 32;
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

/// Command buffers (`SVGA_CAP_COMMAND_BUFFERS`): the guest writes the
/// physical address of an `SVGACBHeader` to [`reg::COMMAND_HIGH`] then
/// [`reg::COMMAND_LOW`], the low 6 bits of which select the context.
pub mod cb {
    /// Size of `SVGACBHeader` in bytes; headers are aligned on it.
    pub const HEADER_BYTES: u32 = 64;
    /// Low bits of [`super::reg::COMMAND_LOW`] holding the context.
    pub const CONTEXT_MASK: u32 = 0x3f;
    /// Largest command buffer without `SVGA_CAP2_CB_MAX_SIZE_4MB`, in bytes.
    pub const MAX_SIZE_DEFAULT: u32 = 512 * 1024;
    /// Command buffers a context may hold queued.
    pub const MAX_QUEUED_PER_CONTEXT: usize = 32;
    /// Largest single command inside a command buffer, in bytes.
    pub const MAX_COMMAND_SIZE: u32 = 32 * 1024;

    /// `SVGACBContext`: where a command buffer is queued.
    pub mod context {
        #![allow(missing_docs)]
        pub const CONTEXT_0: u32 = 0;
        pub const CONTEXT_1: u32 = 1;
        /// Number of regular contexts.
        pub const MAX: u32 = 2;
        /// The device context: start, stop and preempt the others.
        pub const DEVICE: u32 = 0x3f;
    }

    /// `SVGACBStatus`, written by the device into the header.
    pub mod status {
        #![allow(missing_docs)]
        /// Not processed yet (the guest writes this before submitting).
        pub const NONE: u32 = 0;
        pub const COMPLETED: u32 = 1;
        pub const QUEUE_FULL: u32 = 2;
        /// A command failed; `errorOffset` points at it.
        pub const COMMAND_ERROR: u32 = 3;
        pub const CB_HEADER_ERROR: u32 = 4;
        pub const PREEMPTED: u32 = 5;
        pub const SUBMISSION_ERROR: u32 = 6;
        pub const PARTIAL_COMPLETE: u32 = 7;
    }

    /// `SVGACBFlags`.
    pub mod flag {
        #![allow(missing_docs)]
        pub const NO_IRQ: u32 = 1 << 0;
        pub const DX_CONTEXT: u32 = 1 << 1;
        /// The commands live in a MOB rather than at a physical address.
        pub const MOB: u32 = 1 << 2;
    }

    /// `SVGADeviceContextCmdId`: commands of the device context.
    pub mod dc {
        #![allow(missing_docs)]
        pub const NOP: u32 = 0;
        /// `{ enable, context }`.
        pub const START_STOP_CONTEXT: u32 = 1;
        /// `{ context, ignoreIDZero }`.
        pub const PREEMPT: u32 = 2;
        /// `{ context }`.
        pub const START_QUEUE: u32 = 3;
        /// `{ context }`.
        pub const ASYNC_STOP_QUEUE: u32 = 4;
        /// `{ context }`.
        pub const EMPTY_CONTEXT_QUEUE: u32 = 5;
        pub const MAX: u32 = 6;
    }
}

/// 3D device capabilities (`SVGA3D_DEVCAP_*`), read through
/// [`reg::DEV_CAP`] or from the FIFO 3D caps record. Only the indices rsvga
/// sets are named.
pub mod devcap {
    #![allow(missing_docs)]
    pub const THREE_D: u32 = 0;
    pub const MAX_LIGHTS: u32 = 1;
    pub const MAX_TEXTURES: u32 = 2;
    pub const MAX_CLIP_PLANES: u32 = 3;
    pub const VERTEX_SHADER_VERSION: u32 = 4;
    pub const VERTEX_SHADER: u32 = 5;
    pub const FRAGMENT_SHADER_VERSION: u32 = 6;
    pub const FRAGMENT_SHADER: u32 = 7;
    pub const MAX_RENDER_TARGETS: u32 = 8;
    pub const S23E8_TEXTURES: u32 = 9;
    pub const S10E5_TEXTURES: u32 = 10;
    pub const MAX_FIXED_VERTEXBLEND: u32 = 11;
    pub const D16_BUFFER_FORMAT: u32 = 12;
    pub const D24S8_BUFFER_FORMAT: u32 = 13;
    pub const D24X8_BUFFER_FORMAT: u32 = 14;
    pub const QUERY_TYPES: u32 = 15;
    pub const TEXTURE_GRADIENT_SAMPLING: u32 = 16;
    /// A float.
    pub const MAX_POINT_SIZE: u32 = 17;
    pub const MAX_SHADER_TEXTURES: u32 = 18;
    pub const MAX_TEXTURE_WIDTH: u32 = 19;
    pub const MAX_TEXTURE_HEIGHT: u32 = 20;
    pub const MAX_VOLUME_EXTENT: u32 = 21;
    pub const MAX_TEXTURE_REPEAT: u32 = 22;
    pub const MAX_TEXTURE_ASPECT_RATIO: u32 = 23;
    pub const MAX_TEXTURE_ANISOTROPY: u32 = 24;
    pub const MAX_PRIMITIVE_COUNT: u32 = 25;
    pub const MAX_VERTEX_INDEX: u32 = 26;
    pub const MAX_VERTEX_SHADER_INSTRUCTIONS: u32 = 27;
    pub const MAX_FRAGMENT_SHADER_INSTRUCTIONS: u32 = 28;
    pub const MAX_VERTEX_SHADER_TEMPS: u32 = 29;
    pub const MAX_FRAGMENT_SHADER_TEMPS: u32 = 30;
    /// `D3DTEXOPCAPS_*` of fixed-function texture stages.
    pub const TEXTURE_OPS: u32 = 31;
    // `SVGA3DFORMAT_OP_*` of Direct3D 9 formats (see `format_op`).
    pub const SURFACEFMT_X8R8G8B8: u32 = 32;
    pub const SURFACEFMT_A8R8G8B8: u32 = 33;
    pub const SURFACEFMT_A2R10G10B10: u32 = 34;
    pub const SURFACEFMT_X1R5G5B5: u32 = 35;
    pub const SURFACEFMT_A1R5G5B5: u32 = 36;
    pub const SURFACEFMT_A4R4G4B4: u32 = 37;
    pub const SURFACEFMT_R5G6B5: u32 = 38;
    pub const SURFACEFMT_LUMINANCE16: u32 = 39;
    pub const SURFACEFMT_LUMINANCE8_ALPHA8: u32 = 40;
    pub const SURFACEFMT_ALPHA8: u32 = 41;
    pub const SURFACEFMT_LUMINANCE8: u32 = 42;
    pub const SURFACEFMT_Z_D16: u32 = 43;
    pub const SURFACEFMT_Z_D24S8: u32 = 44;
    pub const SURFACEFMT_Z_D24X8: u32 = 45;
    pub const SURFACEFMT_DXT1: u32 = 46;
    pub const SURFACEFMT_DXT2: u32 = 47;
    pub const SURFACEFMT_DXT3: u32 = 48;
    pub const SURFACEFMT_DXT4: u32 = 49;
    pub const SURFACEFMT_DXT5: u32 = 50;
    pub const SURFACEFMT_BUMPX8L8V8U8: u32 = 51;
    pub const SURFACEFMT_A2W10V10U10: u32 = 52;
    pub const SURFACEFMT_BUMPU8V8: u32 = 53;
    pub const SURFACEFMT_Q8W8V8U8: u32 = 54;
    pub const SURFACEFMT_CXV8U8: u32 = 55;
    pub const SURFACEFMT_R_S10E5: u32 = 56;
    pub const SURFACEFMT_R_S23E8: u32 = 57;
    pub const SURFACEFMT_RG_S10E5: u32 = 58;
    pub const SURFACEFMT_RG_S23E8: u32 = 59;
    pub const SURFACEFMT_ARGB_S10E5: u32 = 60;
    pub const SURFACEFMT_ARGB_S23E8: u32 = 61;
    pub const MAX_VERTEX_SHADER_TEXTURES: u32 = 63;
    pub const MAX_SIMULTANEOUS_RENDER_TARGETS: u32 = 64;
    pub const SURFACEFMT_V16U16: u32 = 65;
    pub const SURFACEFMT_G16R16: u32 = 66;
    pub const SURFACEFMT_A16B16G16R16: u32 = 67;
    pub const SURFACEFMT_UYVY: u32 = 68;
    pub const SURFACEFMT_YUY2: u32 = 69;
    pub const AUTOGENMIPMAPS: u32 = 74;
    pub const SURFACEFMT_NV12: u32 = 75;
    pub const MAX_CONTEXT_IDS: u32 = 77;
    pub const MAX_SURFACE_IDS: u32 = 78;
    pub const SURFACEFMT_Z_DF16: u32 = 79;
    pub const SURFACEFMT_Z_DF24: u32 = 80;
    pub const SURFACEFMT_Z_D24S8_INT: u32 = 81;
    pub const SURFACEFMT_ATI1: u32 = 82;
    pub const SURFACEFMT_ATI2: u32 = 83;
    pub const LINE_AA: u32 = 87;
    pub const LINE_STIPPLE: u32 = 88;
    /// A float.
    pub const MAX_LINE_WIDTH: u32 = 89;
    /// A float.
    pub const MAX_AA_LINE_WIDTH: u32 = 90;
    pub const SURFACEFMT_YV12: u32 = 91;
    pub const TS_COLOR_KEY: u32 = 93;
    pub const DXCONTEXT: u32 = 95;
    pub const DX_MAX_VERTEXBUFFERS: u32 = 97;
    pub const DX_MAX_CONSTANT_BUFFERS: u32 = 98;
    pub const DX_PROVOKING_VERTEX: u32 = 99;
    /// `DXFMT_*` of surface formats 1 (`X8R8G8B8`) to 144 (`BC5_UNORM`),
    /// in format order: see [`dxfmt_index`].
    pub const DXFMT_FIRST: u32 = 100;
    pub const SM41: u32 = 244;
    pub const MULTISAMPLE_2X: u32 = 245;
    pub const MULTISAMPLE_4X: u32 = 246;
    pub const MS_FULL_QUALITY: u32 = 247;
    pub const LOGICOPS: u32 = 248;
    pub const LOGIC_BLENDOPS: u32 = 249;
    /// `DXFMT_*` of formats 146 (`BC6H_TYPELESS`) to 151 (`BC7_UNORM_SRGB`).
    pub const DXFMT_BC6H_TYPELESS: u32 = 251;
    pub const SM5: u32 = 258;
    pub const MULTISAMPLE_8X: u32 = 259;
    pub const MAX_FORCED_SAMPLE_COUNT: u32 = 260;
    /// Number of devcap indices.
    pub const MAX: u32 = 262;

    /// The `DXFMT_*` devcap of a surface format. Formats 145
    /// (`B4G4R4A4_UNORM`), 152 (`AYUV`) and 153 (`R11G11B10_TYPELESS`)
    /// have none: drivers decide their support themselves.
    pub const fn dxfmt_index(format: u32) -> Option<u32> {
        match format {
            // DXFMT_FIRST - 1 and DXFMT_BC6H_TYPELESS - 146.
            1..=144 => format.checked_add(99),
            146..=151 => format.checked_add(105),
            _ => None,
        }
    }
}

/// DirectX contexts: object kinds, limits and enumerations of the DX
/// commands (`svga3d_dx.h`, `svga3d_types.h`).
pub mod dx {
    #![allow(missing_docs)]

    /// `SVGA3dShaderType`.
    pub mod shader {
        pub const VS: u32 = 1;
        pub const PS: u32 = 2;
        pub const GS: u32 = 3;
        pub const HS: u32 = 4;
        pub const DS: u32 = 5;
        pub const CS: u32 = 6;
    }

    /// `SVGACOTableType`: the per-context object tables.
    pub mod cotable {
        pub const RTVIEW: u32 = 0;
        pub const DSVIEW: u32 = 1;
        pub const SRVIEW: u32 = 2;
        pub const ELEMENTLAYOUT: u32 = 3;
        pub const BLENDSTATE: u32 = 4;
        pub const DEPTHSTENCIL: u32 = 5;
        pub const RASTERIZERSTATE: u32 = 6;
        pub const SAMPLER: u32 = 7;
        pub const STREAMOUTPUT: u32 = 8;
        pub const DXQUERY: u32 = 9;
        pub const DXSHADER: u32 = 10;
        pub const UAVIEW: u32 = 11;
        pub const MAX: u32 = 12;
    }

    /// `SVGA3dResourceType`: the dimension a view sees.
    pub mod resource {
        pub const BUFFER: u32 = 1;
        pub const TEXTURE1D: u32 = 2;
        pub const TEXTURE2D: u32 = 3;
        pub const TEXTURE3D: u32 = 4;
        pub const TEXTURECUBE: u32 = 5;
        pub const BUFFEREX: u32 = 6;
    }

    /// `SVGA3dQueryType`.
    pub mod query {
        pub const OCCLUSION: u32 = 0;
        pub const TIMESTAMP: u32 = 1;
        pub const TIMESTAMPDISJOINT: u32 = 2;
        pub const PIPELINESTATS: u32 = 3;
        pub const OCCLUSIONPREDICATE: u32 = 4;
        pub const STREAMOUTPUTSTATS: u32 = 5;
        pub const STREAMOVERFLOWPREDICATE: u32 = 6;
        pub const OCCLUSION64: u32 = 7;
        pub const SOSTATS_STREAM0: u32 = 8;
        pub const SOSTATS_STREAM1: u32 = 9;
        pub const SOSTATS_STREAM2: u32 = 10;
        pub const SOSTATS_STREAM3: u32 = 11;
        pub const SOP_STREAM0: u32 = 12;
        pub const SOP_STREAM1: u32 = 13;
        pub const SOP_STREAM2: u32 = 14;
        pub const SOP_STREAM3: u32 = 15;
        pub const MAX: u32 = 16;
    }

    /// `SVGA3D_INPUT_*`: input element classification.
    pub const INPUT_PER_INSTANCE_DATA: u32 = 1;

    /// Bindings per draw (`SVGA3D_DX_MAX_*`).
    pub const MAX_RENDER_TARGETS: usize = 8;
    pub const MAX_VERTEXBUFFERS: usize = 32;
    pub const MAX_SOTARGETS: usize = 4;
    pub const MAX_SRVIEWS: usize = 128;
    pub const MAX_CONSTBUFFERS: usize = 16;
    pub const MAX_SAMPLERS: usize = 16;
    pub const MAX_VIEWPORTS: usize = 16;
    pub const MAX_SCISSORRECTS: usize = 16;
    /// Unordered-access view slots (`SVGA3D_DX11_1_MAX_UAVIEWS`; Direct3D
    /// 11.0 uses the first 8).
    pub const MAX_UAVIEWS: usize = 64;
    /// Elements of an input layout (`SVGACOTableDXElementLayoutEntry`).
    pub const MAX_ELEMENTS: usize = 32;
    /// Declarations of a stream-output object.
    pub const MAX_STREAMOUT_DECLS: usize = 64;
}

/// What a Direct3D 9 format can be used for (`SVGA3dFormatOp`, the bits of
/// `D3DFORMAT_OP_*`): values of the legacy `SURFACEFMT_*` devcaps.
pub mod format_op {
    #![allow(missing_docs)]
    pub const TEXTURE: u32 = 0x0000_0001;
    pub const VOLUMETEXTURE: u32 = 0x0000_0002;
    pub const CUBETEXTURE: u32 = 0x0000_0004;
    pub const OFFSCREEN_RENDERTARGET: u32 = 0x0000_0008;
    pub const SAME_FORMAT_RENDERTARGET: u32 = 0x0000_0010;
    pub const ZSTENCIL: u32 = 0x0000_0040;
    pub const ZSTENCIL_WITH_ARBITRARY_COLOR_DEPTH: u32 = 0x0000_0080;
    pub const SAME_FORMAT_UP_TO_ALPHA_RENDERTARGET: u32 = 0x0000_0100;
    pub const DISPLAYMODE: u32 = 0x0000_0400;
    pub const THREE_D_ACCELERATION: u32 = 0x0000_0800;
    pub const PIXELSIZE: u32 = 0x0000_1000;
    pub const CONVERT_TO_ARGB: u32 = 0x0000_2000;
    pub const OFFSCREENPLAIN: u32 = 0x0000_4000;
    pub const SRGBREAD: u32 = 0x0000_8000;
    pub const BUMPMAP: u32 = 0x0001_0000;
    pub const DMAP: u32 = 0x0002_0000;
    pub const NOFILTER: u32 = 0x0004_0000;
    pub const MEMBEROFGROUP_ARGB: u32 = 0x0008_0000;
    pub const SRGBWRITE: u32 = 0x0010_0000;
    pub const NOALPHABLEND: u32 = 0x0020_0000;
    pub const AUTOGENMIPMAP: u32 = 0x0040_0000;
    pub const VERTEXTEXTURE: u32 = 0x0080_0000;
    pub const NOTEXCOORDWRAPNORMIP: u32 = 0x0100_0000;
}

/// Legacy shader model versions (`SVGA3dShaderVersion`).
pub mod shader_version {
    #![allow(missing_docs)]
    pub const VS_30: u32 = 7;
    pub const VS_40: u32 = 9;
    pub const PS_30: u32 = 13;
    pub const PS_40: u32 = 15;
}

/// What a surface format can do (`SVGA3D_DXFMT_*`), the value of its
/// `DXFMT` devcap.
pub mod dxfmt {
    #![allow(missing_docs)]
    /// Surfaces of this format can be created (and read by `ld`).
    pub const SUPPORTED: u32 = 1 << 0;
    /// Shaders can sample it with filtering.
    pub const SHADER_SAMPLE: u32 = 1 << 1;
    pub const COLOR_RENDERTARGET: u32 = 1 << 2;
    pub const DEPTH_RENDERTARGET: u32 = 1 << 3;
    pub const BLENDABLE: u32 = 1 << 4;
    pub const MIPS: u32 = 1 << 5;
    /// Texture arrays and cube maps.
    pub const ARRAY: u32 = 1 << 6;
    /// 3D textures.
    pub const VOLUME: u32 = 1 << 7;
    pub const DX_VERTEX_BUFFER: u32 = 1 << 8;
    pub const MULTISAMPLE: u32 = 1 << 9;
}

/// Record types in the FIFO 3D caps area ([`fifo::CAPS_3D`]): records of
/// `{ length_in_words, type }` followed by `(index, value)` pairs, ended
/// by a record of length 0.
pub mod fifo_caps_record {
    /// Device capabilities, as `(devcap index, value)` pairs.
    pub const DEVCAPS: u32 = 0x100;
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
