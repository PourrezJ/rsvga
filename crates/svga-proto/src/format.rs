//! Surface formats (`SVGA3dSurfaceFormat`) and their block layout.
//!
//! Generated from `g_SVGA3dSurfaceDescs` in the VMware SVGA headers
//! (`svga3d_surfacedefs.h`, MIT license); do not edit by hand.

/// How a format stores its pixels: in blocks of `block_*` pixels
/// (1x1x1 for uncompressed formats), each `bytes_per_block` long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatDesc {
    /// Block width in pixels.
    pub block_width: u32,
    /// Block height in pixels.
    pub block_height: u32,
    /// Block depth in pixels.
    pub block_depth: u32,
    /// Bytes per block.
    pub bytes_per_block: u32,
    /// Bytes per block in a row pitch (differs for planar YUV formats).
    pub pitch_bytes_per_block: u32,
}

#[allow(missing_docs)]
pub const X8R8G8B8: u32 = 1;
#[allow(missing_docs)]
pub const A8R8G8B8: u32 = 2;
#[allow(missing_docs)]
pub const R5G6B5: u32 = 3;
#[allow(missing_docs)]
pub const X1R5G5B5: u32 = 4;
#[allow(missing_docs)]
pub const A1R5G5B5: u32 = 5;
#[allow(missing_docs)]
pub const A4R4G4B4: u32 = 6;
#[allow(missing_docs)]
pub const Z_D32: u32 = 7;
#[allow(missing_docs)]
pub const Z_D16: u32 = 8;
#[allow(missing_docs)]
pub const Z_D24S8: u32 = 9;
#[allow(missing_docs)]
pub const Z_D15S1: u32 = 10;
#[allow(missing_docs)]
pub const LUMINANCE8: u32 = 11;
#[allow(missing_docs)]
pub const LUMINANCE4_ALPHA4: u32 = 12;
#[allow(missing_docs)]
pub const LUMINANCE16: u32 = 13;
#[allow(missing_docs)]
pub const LUMINANCE8_ALPHA8: u32 = 14;
#[allow(missing_docs)]
pub const DXT1: u32 = 15;
#[allow(missing_docs)]
pub const DXT2: u32 = 16;
#[allow(missing_docs)]
pub const DXT3: u32 = 17;
#[allow(missing_docs)]
pub const DXT4: u32 = 18;
#[allow(missing_docs)]
pub const DXT5: u32 = 19;
#[allow(missing_docs)]
pub const BUMPU8V8: u32 = 20;
#[allow(missing_docs)]
pub const BUMPL6V5U5: u32 = 21;
#[allow(missing_docs)]
pub const BUMPX8L8V8U8: u32 = 22;
#[allow(missing_docs)]
pub const FORMAT_DEAD1: u32 = 23;
#[allow(missing_docs)]
pub const ARGB_S10E5: u32 = 24;
#[allow(missing_docs)]
pub const ARGB_S23E8: u32 = 25;
#[allow(missing_docs)]
pub const A2R10G10B10: u32 = 26;
#[allow(missing_docs)]
pub const V8U8: u32 = 27;
#[allow(missing_docs)]
pub const Q8W8V8U8: u32 = 28;
#[allow(missing_docs)]
pub const CXV8U8: u32 = 29;
#[allow(missing_docs)]
pub const X8L8V8U8: u32 = 30;
#[allow(missing_docs)]
pub const A2W10V10U10: u32 = 31;
#[allow(missing_docs)]
pub const ALPHA8: u32 = 32;
#[allow(missing_docs)]
pub const R_S10E5: u32 = 33;
#[allow(missing_docs)]
pub const R_S23E8: u32 = 34;
#[allow(missing_docs)]
pub const RG_S10E5: u32 = 35;
#[allow(missing_docs)]
pub const RG_S23E8: u32 = 36;
#[allow(missing_docs)]
pub const BUFFER: u32 = 37;
#[allow(missing_docs)]
pub const Z_D24X8: u32 = 38;
#[allow(missing_docs)]
pub const V16U16: u32 = 39;
#[allow(missing_docs)]
pub const G16R16: u32 = 40;
#[allow(missing_docs)]
pub const A16B16G16R16: u32 = 41;
#[allow(missing_docs)]
pub const UYVY: u32 = 42;
#[allow(missing_docs)]
pub const YUY2: u32 = 43;
#[allow(missing_docs)]
pub const NV12: u32 = 44;
#[allow(missing_docs)]
pub const FORMAT_DEAD2: u32 = 45;
#[allow(missing_docs)]
pub const R32G32B32A32_TYPELESS: u32 = 46;
#[allow(missing_docs)]
pub const R32G32B32A32_UINT: u32 = 47;
#[allow(missing_docs)]
pub const R32G32B32A32_SINT: u32 = 48;
#[allow(missing_docs)]
pub const R32G32B32_TYPELESS: u32 = 49;
#[allow(missing_docs)]
pub const R32G32B32_FLOAT: u32 = 50;
#[allow(missing_docs)]
pub const R32G32B32_UINT: u32 = 51;
#[allow(missing_docs)]
pub const R32G32B32_SINT: u32 = 52;
#[allow(missing_docs)]
pub const R16G16B16A16_TYPELESS: u32 = 53;
#[allow(missing_docs)]
pub const R16G16B16A16_UINT: u32 = 54;
#[allow(missing_docs)]
pub const R16G16B16A16_SNORM: u32 = 55;
#[allow(missing_docs)]
pub const R16G16B16A16_SINT: u32 = 56;
#[allow(missing_docs)]
pub const R32G32_TYPELESS: u32 = 57;
#[allow(missing_docs)]
pub const R32G32_UINT: u32 = 58;
#[allow(missing_docs)]
pub const R32G32_SINT: u32 = 59;
#[allow(missing_docs)]
pub const R32G8X24_TYPELESS: u32 = 60;
#[allow(missing_docs)]
pub const D32_FLOAT_S8X24_UINT: u32 = 61;
#[allow(missing_docs)]
pub const R32_FLOAT_X8X24: u32 = 62;
#[allow(missing_docs)]
pub const X32_G8X24_UINT: u32 = 63;
#[allow(missing_docs)]
pub const R10G10B10A2_TYPELESS: u32 = 64;
#[allow(missing_docs)]
pub const R10G10B10A2_UINT: u32 = 65;
#[allow(missing_docs)]
pub const R11G11B10_FLOAT: u32 = 66;
#[allow(missing_docs)]
pub const R8G8B8A8_TYPELESS: u32 = 67;
#[allow(missing_docs)]
pub const R8G8B8A8_UNORM: u32 = 68;
#[allow(missing_docs)]
pub const R8G8B8A8_UNORM_SRGB: u32 = 69;
#[allow(missing_docs)]
pub const R8G8B8A8_UINT: u32 = 70;
#[allow(missing_docs)]
pub const R8G8B8A8_SINT: u32 = 71;
#[allow(missing_docs)]
pub const R16G16_TYPELESS: u32 = 72;
#[allow(missing_docs)]
pub const R16G16_UINT: u32 = 73;
#[allow(missing_docs)]
pub const R16G16_SINT: u32 = 74;
#[allow(missing_docs)]
pub const R32_TYPELESS: u32 = 75;
#[allow(missing_docs)]
pub const D32_FLOAT: u32 = 76;
#[allow(missing_docs)]
pub const R32_UINT: u32 = 77;
#[allow(missing_docs)]
pub const R32_SINT: u32 = 78;
#[allow(missing_docs)]
pub const R24G8_TYPELESS: u32 = 79;
#[allow(missing_docs)]
pub const D24_UNORM_S8_UINT: u32 = 80;
#[allow(missing_docs)]
pub const R24_UNORM_X8: u32 = 81;
#[allow(missing_docs)]
pub const X24_G8_UINT: u32 = 82;
#[allow(missing_docs)]
pub const R8G8_TYPELESS: u32 = 83;
#[allow(missing_docs)]
pub const R8G8_UNORM: u32 = 84;
#[allow(missing_docs)]
pub const R8G8_UINT: u32 = 85;
#[allow(missing_docs)]
pub const R8G8_SINT: u32 = 86;
#[allow(missing_docs)]
pub const R16_TYPELESS: u32 = 87;
#[allow(missing_docs)]
pub const R16_UNORM: u32 = 88;
#[allow(missing_docs)]
pub const R16_UINT: u32 = 89;
#[allow(missing_docs)]
pub const R16_SNORM: u32 = 90;
#[allow(missing_docs)]
pub const R16_SINT: u32 = 91;
#[allow(missing_docs)]
pub const R8_TYPELESS: u32 = 92;
#[allow(missing_docs)]
pub const R8_UNORM: u32 = 93;
#[allow(missing_docs)]
pub const R8_UINT: u32 = 94;
#[allow(missing_docs)]
pub const R8_SNORM: u32 = 95;
#[allow(missing_docs)]
pub const R8_SINT: u32 = 96;
#[allow(missing_docs)]
pub const P8: u32 = 97;
#[allow(missing_docs)]
pub const R9G9B9E5_SHAREDEXP: u32 = 98;
#[allow(missing_docs)]
pub const R8G8_B8G8_UNORM: u32 = 99;
#[allow(missing_docs)]
pub const G8R8_G8B8_UNORM: u32 = 100;
#[allow(missing_docs)]
pub const BC1_TYPELESS: u32 = 101;
#[allow(missing_docs)]
pub const BC1_UNORM_SRGB: u32 = 102;
#[allow(missing_docs)]
pub const BC2_TYPELESS: u32 = 103;
#[allow(missing_docs)]
pub const BC2_UNORM_SRGB: u32 = 104;
#[allow(missing_docs)]
pub const BC3_TYPELESS: u32 = 105;
#[allow(missing_docs)]
pub const BC3_UNORM_SRGB: u32 = 106;
#[allow(missing_docs)]
pub const BC4_TYPELESS: u32 = 107;
#[allow(missing_docs)]
pub const ATI1: u32 = 108;
#[allow(missing_docs)]
pub const BC4_SNORM: u32 = 109;
#[allow(missing_docs)]
pub const BC5_TYPELESS: u32 = 110;
#[allow(missing_docs)]
pub const ATI2: u32 = 111;
#[allow(missing_docs)]
pub const BC5_SNORM: u32 = 112;
#[allow(missing_docs)]
pub const R10G10B10_XR_BIAS_A2_UNORM: u32 = 113;
#[allow(missing_docs)]
pub const B8G8R8A8_TYPELESS: u32 = 114;
#[allow(missing_docs)]
pub const B8G8R8A8_UNORM_SRGB: u32 = 115;
#[allow(missing_docs)]
pub const B8G8R8X8_TYPELESS: u32 = 116;
#[allow(missing_docs)]
pub const B8G8R8X8_UNORM_SRGB: u32 = 117;
#[allow(missing_docs)]
pub const Z_DF16: u32 = 118;
#[allow(missing_docs)]
pub const Z_DF24: u32 = 119;
#[allow(missing_docs)]
pub const Z_D24S8_INT: u32 = 120;
#[allow(missing_docs)]
pub const YV12: u32 = 121;
#[allow(missing_docs)]
pub const R32G32B32A32_FLOAT: u32 = 122;
#[allow(missing_docs)]
pub const R16G16B16A16_FLOAT: u32 = 123;
#[allow(missing_docs)]
pub const R16G16B16A16_UNORM: u32 = 124;
#[allow(missing_docs)]
pub const R32G32_FLOAT: u32 = 125;
#[allow(missing_docs)]
pub const R10G10B10A2_UNORM: u32 = 126;
#[allow(missing_docs)]
pub const R8G8B8A8_SNORM: u32 = 127;
#[allow(missing_docs)]
pub const R16G16_FLOAT: u32 = 128;
#[allow(missing_docs)]
pub const R16G16_UNORM: u32 = 129;
#[allow(missing_docs)]
pub const R16G16_SNORM: u32 = 130;
#[allow(missing_docs)]
pub const R32_FLOAT: u32 = 131;
#[allow(missing_docs)]
pub const R8G8_SNORM: u32 = 132;
#[allow(missing_docs)]
pub const R16_FLOAT: u32 = 133;
#[allow(missing_docs)]
pub const D16_UNORM: u32 = 134;
#[allow(missing_docs)]
pub const A8_UNORM: u32 = 135;
#[allow(missing_docs)]
pub const BC1_UNORM: u32 = 136;
#[allow(missing_docs)]
pub const BC2_UNORM: u32 = 137;
#[allow(missing_docs)]
pub const BC3_UNORM: u32 = 138;
#[allow(missing_docs)]
pub const B5G6R5_UNORM: u32 = 139;
#[allow(missing_docs)]
pub const B5G5R5A1_UNORM: u32 = 140;
#[allow(missing_docs)]
pub const B8G8R8A8_UNORM: u32 = 141;
#[allow(missing_docs)]
pub const B8G8R8X8_UNORM: u32 = 142;
#[allow(missing_docs)]
pub const BC4_UNORM: u32 = 143;
#[allow(missing_docs)]
pub const BC5_UNORM: u32 = 144;
#[allow(missing_docs)]
pub const B4G4R4A4_UNORM: u32 = 145;
#[allow(missing_docs)]
pub const BC6H_TYPELESS: u32 = 146;
#[allow(missing_docs)]
pub const BC6H_UF16: u32 = 147;
#[allow(missing_docs)]
pub const BC6H_SF16: u32 = 148;
#[allow(missing_docs)]
pub const BC7_TYPELESS: u32 = 149;
#[allow(missing_docs)]
pub const BC7_UNORM: u32 = 150;
#[allow(missing_docs)]
pub const BC7_UNORM_SRGB: u32 = 151;
#[allow(missing_docs)]
pub const AYUV: u32 = 152;
#[allow(missing_docs)]
pub const R11G11B10_TYPELESS: u32 = 153;

/// Layout of `format`, or `None` for an unknown format.
pub const fn desc(format: u32) -> Option<FormatDesc> {
    let (block_width, block_height, block_depth, bytes_per_block, pitch_bytes_per_block) =
        match format {
            X8R8G8B8 => (1, 1, 1, 4, 4),
            A8R8G8B8 => (1, 1, 1, 4, 4),
            R5G6B5 => (1, 1, 1, 2, 2),
            X1R5G5B5 => (1, 1, 1, 2, 2),
            A1R5G5B5 => (1, 1, 1, 2, 2),
            A4R4G4B4 => (1, 1, 1, 2, 2),
            Z_D32 => (1, 1, 1, 4, 4),
            Z_D16 => (1, 1, 1, 2, 2),
            Z_D24S8 => (1, 1, 1, 4, 4),
            Z_D15S1 => (1, 1, 1, 2, 2),
            LUMINANCE8 => (1, 1, 1, 1, 1),
            LUMINANCE4_ALPHA4 => (1, 1, 1, 1, 1),
            LUMINANCE16 => (1, 1, 1, 2, 2),
            LUMINANCE8_ALPHA8 => (1, 1, 1, 2, 2),
            DXT1 => (4, 4, 1, 8, 8),
            DXT2 => (4, 4, 1, 16, 16),
            DXT3 => (4, 4, 1, 16, 16),
            DXT4 => (4, 4, 1, 16, 16),
            DXT5 => (4, 4, 1, 16, 16),
            BUMPU8V8 => (1, 1, 1, 2, 2),
            BUMPL6V5U5 => (1, 1, 1, 2, 2),
            BUMPX8L8V8U8 => (1, 1, 1, 4, 4),
            FORMAT_DEAD1 => (1, 1, 1, 3, 3),
            ARGB_S10E5 => (1, 1, 1, 8, 8),
            ARGB_S23E8 => (1, 1, 1, 16, 16),
            A2R10G10B10 => (1, 1, 1, 4, 4),
            V8U8 => (1, 1, 1, 2, 2),
            Q8W8V8U8 => (1, 1, 1, 4, 4),
            CXV8U8 => (1, 1, 1, 2, 2),
            X8L8V8U8 => (1, 1, 1, 4, 4),
            A2W10V10U10 => (1, 1, 1, 4, 4),
            ALPHA8 => (1, 1, 1, 1, 1),
            R_S10E5 => (1, 1, 1, 2, 2),
            R_S23E8 => (1, 1, 1, 4, 4),
            RG_S10E5 => (1, 1, 1, 4, 4),
            RG_S23E8 => (1, 1, 1, 8, 8),
            BUFFER => (1, 1, 1, 1, 1),
            Z_D24X8 => (1, 1, 1, 4, 4),
            V16U16 => (1, 1, 1, 4, 4),
            G16R16 => (1, 1, 1, 4, 4),
            A16B16G16R16 => (1, 1, 1, 8, 8),
            UYVY => (2, 1, 1, 4, 4),
            YUY2 => (2, 1, 1, 4, 4),
            NV12 => (2, 2, 1, 6, 2),
            FORMAT_DEAD2 => (1, 1, 1, 4, 4),
            R32G32B32A32_TYPELESS => (1, 1, 1, 16, 16),
            R32G32B32A32_UINT => (1, 1, 1, 16, 16),
            R32G32B32A32_SINT => (1, 1, 1, 16, 16),
            R32G32B32_TYPELESS => (1, 1, 1, 12, 12),
            R32G32B32_FLOAT => (1, 1, 1, 12, 12),
            R32G32B32_UINT => (1, 1, 1, 12, 12),
            R32G32B32_SINT => (1, 1, 1, 12, 12),
            R16G16B16A16_TYPELESS => (1, 1, 1, 8, 8),
            R16G16B16A16_UINT => (1, 1, 1, 8, 8),
            R16G16B16A16_SNORM => (1, 1, 1, 8, 8),
            R16G16B16A16_SINT => (1, 1, 1, 8, 8),
            R32G32_TYPELESS => (1, 1, 1, 8, 8),
            R32G32_UINT => (1, 1, 1, 8, 8),
            R32G32_SINT => (1, 1, 1, 8, 8),
            R32G8X24_TYPELESS => (1, 1, 1, 8, 8),
            D32_FLOAT_S8X24_UINT => (1, 1, 1, 8, 8),
            R32_FLOAT_X8X24 => (1, 1, 1, 8, 8),
            X32_G8X24_UINT => (1, 1, 1, 8, 8),
            R10G10B10A2_TYPELESS => (1, 1, 1, 4, 4),
            R10G10B10A2_UINT => (1, 1, 1, 4, 4),
            R11G11B10_FLOAT => (1, 1, 1, 4, 4),
            R8G8B8A8_TYPELESS => (1, 1, 1, 4, 4),
            R8G8B8A8_UNORM => (1, 1, 1, 4, 4),
            R8G8B8A8_UNORM_SRGB => (1, 1, 1, 4, 4),
            R8G8B8A8_UINT => (1, 1, 1, 4, 4),
            R8G8B8A8_SINT => (1, 1, 1, 4, 4),
            R16G16_TYPELESS => (1, 1, 1, 4, 4),
            R16G16_UINT => (1, 1, 1, 4, 4),
            R16G16_SINT => (1, 1, 1, 4, 4),
            R32_TYPELESS => (1, 1, 1, 4, 4),
            D32_FLOAT => (1, 1, 1, 4, 4),
            R32_UINT => (1, 1, 1, 4, 4),
            R32_SINT => (1, 1, 1, 4, 4),
            R24G8_TYPELESS => (1, 1, 1, 4, 4),
            D24_UNORM_S8_UINT => (1, 1, 1, 4, 4),
            R24_UNORM_X8 => (1, 1, 1, 4, 4),
            X24_G8_UINT => (1, 1, 1, 4, 4),
            R8G8_TYPELESS => (1, 1, 1, 2, 2),
            R8G8_UNORM => (1, 1, 1, 2, 2),
            R8G8_UINT => (1, 1, 1, 2, 2),
            R8G8_SINT => (1, 1, 1, 2, 2),
            R16_TYPELESS => (1, 1, 1, 2, 2),
            R16_UNORM => (1, 1, 1, 2, 2),
            R16_UINT => (1, 1, 1, 2, 2),
            R16_SNORM => (1, 1, 1, 2, 2),
            R16_SINT => (1, 1, 1, 2, 2),
            R8_TYPELESS => (1, 1, 1, 1, 1),
            R8_UNORM => (1, 1, 1, 1, 1),
            R8_UINT => (1, 1, 1, 1, 1),
            R8_SNORM => (1, 1, 1, 1, 1),
            R8_SINT => (1, 1, 1, 1, 1),
            P8 => (1, 1, 1, 1, 1),
            R9G9B9E5_SHAREDEXP => (1, 1, 1, 4, 4),
            R8G8_B8G8_UNORM => (2, 1, 1, 4, 4),
            G8R8_G8B8_UNORM => (2, 1, 1, 4, 4),
            BC1_TYPELESS => (4, 4, 1, 8, 8),
            BC1_UNORM_SRGB => (4, 4, 1, 8, 8),
            BC2_TYPELESS => (4, 4, 1, 16, 16),
            BC2_UNORM_SRGB => (4, 4, 1, 16, 16),
            BC3_TYPELESS => (4, 4, 1, 16, 16),
            BC3_UNORM_SRGB => (4, 4, 1, 16, 16),
            BC4_TYPELESS => (4, 4, 1, 8, 8),
            ATI1 => (4, 4, 1, 8, 8),
            BC4_SNORM => (4, 4, 1, 8, 8),
            BC5_TYPELESS => (4, 4, 1, 16, 16),
            ATI2 => (4, 4, 1, 16, 16),
            BC5_SNORM => (4, 4, 1, 16, 16),
            R10G10B10_XR_BIAS_A2_UNORM => (1, 1, 1, 4, 4),
            B8G8R8A8_TYPELESS => (1, 1, 1, 4, 4),
            B8G8R8A8_UNORM_SRGB => (1, 1, 1, 4, 4),
            B8G8R8X8_TYPELESS => (1, 1, 1, 4, 4),
            B8G8R8X8_UNORM_SRGB => (1, 1, 1, 4, 4),
            Z_DF16 => (1, 1, 1, 2, 2),
            Z_DF24 => (1, 1, 1, 4, 4),
            Z_D24S8_INT => (1, 1, 1, 4, 4),
            YV12 => (2, 2, 1, 6, 2),
            R32G32B32A32_FLOAT => (1, 1, 1, 16, 16),
            R16G16B16A16_FLOAT => (1, 1, 1, 8, 8),
            R16G16B16A16_UNORM => (1, 1, 1, 8, 8),
            R32G32_FLOAT => (1, 1, 1, 8, 8),
            R10G10B10A2_UNORM => (1, 1, 1, 4, 4),
            R8G8B8A8_SNORM => (1, 1, 1, 4, 4),
            R16G16_FLOAT => (1, 1, 1, 4, 4),
            R16G16_UNORM => (1, 1, 1, 4, 4),
            R16G16_SNORM => (1, 1, 1, 4, 4),
            R32_FLOAT => (1, 1, 1, 4, 4),
            R8G8_SNORM => (1, 1, 1, 2, 2),
            R16_FLOAT => (1, 1, 1, 2, 2),
            D16_UNORM => (1, 1, 1, 2, 2),
            A8_UNORM => (1, 1, 1, 1, 1),
            BC1_UNORM => (4, 4, 1, 8, 8),
            BC2_UNORM => (4, 4, 1, 16, 16),
            BC3_UNORM => (4, 4, 1, 16, 16),
            B5G6R5_UNORM => (1, 1, 1, 2, 2),
            B5G5R5A1_UNORM => (1, 1, 1, 2, 2),
            B8G8R8A8_UNORM => (1, 1, 1, 4, 4),
            B8G8R8X8_UNORM => (1, 1, 1, 4, 4),
            BC4_UNORM => (4, 4, 1, 8, 8),
            BC5_UNORM => (4, 4, 1, 16, 16),
            B4G4R4A4_UNORM => (1, 1, 1, 2, 2),
            BC6H_TYPELESS => (4, 4, 1, 16, 16),
            BC6H_UF16 => (4, 4, 1, 16, 16),
            BC6H_SF16 => (4, 4, 1, 16, 16),
            BC7_TYPELESS => (4, 4, 1, 16, 16),
            BC7_UNORM => (4, 4, 1, 16, 16),
            BC7_UNORM_SRGB => (4, 4, 1, 16, 16),
            AYUV => (1, 1, 1, 4, 4),
            R11G11B10_TYPELESS => (1, 1, 1, 4, 4),
            _ => return None,
        };
    Some(FormatDesc {
        block_width,
        block_height,
        block_depth,
        bytes_per_block,
        pitch_bytes_per_block,
    })
}
