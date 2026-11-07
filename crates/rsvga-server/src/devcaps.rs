//! 3D device capabilities (`SVGA3D_DEVCAP_*`).
//!
//! Drivers read them through `SVGA_REG_DEV_CAP` (with guest-backed
//! objects) or from a record in the FIFO 3D caps area. Without 3D the table
//! is empty, so `SVGA3D_DEVCAP_3D` reads 0 everywhere.
//!
//! The 3D table describes a DirectX 10.1 device (shader model 4.1), the
//! level DWM and most DirectX 10/11 programs need. Format support follows
//! what Direct3D 11 hardware reports (`CheckFormatSupport`), which is also
//! what a VMware host reports. The VMware drivers check it both ways:
//!
//! - their user-mode DirectX driver has, for each feature level (10_0 to
//!   11_1) and each DDI version, the minimum value of every devcap it
//!   relies on, and supports no feature level at all (`D3D11CreateDevice`
//!   fails with `DXGI_ERROR_UNSUPPORTED`, DWM falls back to WARP and stops)
//!   if one is missing. For these formats, `SHADER_SAMPLE` means "readable
//!   by shaders": integer and stencil formats have it too;
//! - their kernel driver (debug build) asserts that impossible combinations
//!   are absent, for instance a typeless or depth-only color target.
//!
//! Direct3D 9 formats with a DXGI equivalent get its support; those without
//! one (bump maps, 16-bit depth with 1-bit stencil...) are left out until
//! the Direct3D 9 path needs them. The values here are what rsvga intends
//! to render; the Vulkan backend will drop whatever the host GPU lacks.
//!
//! The legacy devcaps (indices below 100) describe a Direct3D 9 device
//! (shader model 3, fixed-function limits) for VMware's Direct3D 9 driver,
//! which speaks the legacy SVGA3D protocol. Its format list comes from the
//! `SURFACEFMT_*` entries (`SVGA3DFORMAT_OP_*`, Direct3D 9's
//! `D3DFORMAT_OP_*`): Windows' Direct3D 9 runtime offers no hardware
//! device (`D3DERR_NOTAVAILABLE`) unless the display format is marked
//! "3D accelerated". Formats without a Vulkan twin of the same layout stay
//! 0. That runtime also disables the driver over some combinations
//! (`d3d9.dll`, `FetchDirectDrawData`): "3D accelerated" on an alpha format
//! or without "display mode", `MEMBEROFGROUP_ARGB` outside its formats
//! (A4R4G4B4 is not one), R5G6B5 and X1R5G5B5 both as display modes.

use svga_proto::{devcap, dxfmt, fifo, fifo_caps_record, format as f, format_op as op};

use crate::log;

/// Float or normalized color formats: sampled, rendered to, blended,
/// multisampled, in every texture shape. VMware's own value for
/// `B4G4R4A4_UNORM` (which has no devcap).
const COLOR: u32 = dxfmt::SUPPORTED
    | dxfmt::SHADER_SAMPLE
    | dxfmt::COLOR_RENDERTARGET
    | dxfmt::BLENDABLE
    | dxfmt::MIPS
    | dxfmt::ARRAY
    | dxfmt::VOLUME
    | dxfmt::MULTISAMPLE;
/// Integer color formats: read and rendered to, never filtered nor
/// blended.
const INTEGER: u32 = dxfmt::SUPPORTED
    | dxfmt::SHADER_SAMPLE
    | dxfmt::COLOR_RENDERTARGET
    | dxfmt::MIPS
    | dxfmt::ARRAY
    | dxfmt::VOLUME
    | dxfmt::MULTISAMPLE;
/// Typeless formats: storage only, used through typed views. VMware's
/// own value for `R11G11B10_TYPELESS` (which has no devcap).
const TYPELESS: u32 =
    dxfmt::SUPPORTED | dxfmt::MIPS | dxfmt::ARRAY | dxfmt::VOLUME | dxfmt::MULTISAMPLE;
/// Typeless formats that also hold depth: no 3D textures.
const DEPTH_TYPELESS: u32 = dxfmt::SUPPORTED | dxfmt::MIPS | dxfmt::ARRAY | dxfmt::MULTISAMPLE;
/// Depth/stencil formats.
const DEPTH: u32 = DEPTH_TYPELESS | dxfmt::DEPTH_RENDERTARGET;
/// The depth part of a depth/stencil format, read by shaders.
const DEPTH_READ: u32 = DEPTH_TYPELESS | dxfmt::SHADER_SAMPLE;
/// The stencil part, read with `ld` only.
const STENCIL_READ: u32 = DEPTH_TYPELESS | dxfmt::SHADER_SAMPLE;
/// Block-compressed formats, and others that can only be sampled.
const SAMPLED: u32 =
    dxfmt::SUPPORTED | dxfmt::SHADER_SAMPLE | dxfmt::MIPS | dxfmt::ARRAY | dxfmt::VOLUME;
/// Typeless block-compressed formats.
const COMPRESSED_TYPELESS: u32 = dxfmt::SUPPORTED | dxfmt::MIPS | dxfmt::ARRAY | dxfmt::VOLUME;
/// Usable as a vertex attribute format.
const VB: u32 = dxfmt::DX_VERTEX_BUFFER;

/// Direct3D 9 texture uses: 2D, volume and cube.
const OP_TEXTURE: u32 = op::TEXTURE | op::VOLUMETEXTURE | op::CUBETEXTURE;
/// Render target uses.
const OP_TARGET: u32 = op::OFFSCREEN_RENDERTARGET | op::SAME_FORMAT_RENDERTARGET;
/// The 8-bit and 16-bit RGB formats: textures, targets, plain surfaces,
/// convertible to each other.
const OP_RGB: u32 = OP_TEXTURE
    | OP_TARGET
    | op::OFFSCREENPLAIN
    | op::CONVERT_TO_ARGB
    | op::MEMBEROFGROUP_ARGB
    | op::AUTOGENMIPMAP;
/// Formats the desktop can be in, with 3D on them.
const OP_DISPLAY: u32 = op::DISPLAYMODE | op::THREE_D_ACCELERATION;
/// Depth buffers.
const OP_DEPTH: u32 = op::ZSTENCIL | op::ZSTENCIL_WITH_ARBITRARY_COLOR_DEPTH;
/// Float formats: textures and targets.
const OP_FLOAT: u32 = OP_TEXTURE | OP_TARGET | op::OFFSCREENPLAIN | op::VERTEXTEXTURE;

/// `SVGA3DFORMAT_OP_*` of each legacy `SURFACEFMT_*` devcap.
const FORMAT_OPS: &[(u32, u32)] = &[
    (
        devcap::SURFACEFMT_X8R8G8B8,
        OP_RGB | OP_DISPLAY | op::SRGBREAD | op::SRGBWRITE | op::VERTEXTEXTURE,
    ),
    (
        devcap::SURFACEFMT_A8R8G8B8,
        OP_RGB
            | op::SAME_FORMAT_UP_TO_ALPHA_RENDERTARGET
            | op::SRGBREAD
            | op::SRGBWRITE
            | op::VERTEXTEXTURE,
    ),
    (devcap::SURFACEFMT_A2R10G10B10, OP_RGB),
    (devcap::SURFACEFMT_X1R5G5B5, OP_RGB),
    (
        devcap::SURFACEFMT_A1R5G5B5,
        OP_RGB | op::SAME_FORMAT_UP_TO_ALPHA_RENDERTARGET,
    ),
    // Not a member of the ARGB group for Direct3D 9: its runtime disables
    // the whole driver if A4R4G4B4 claims it.
    (
        devcap::SURFACEFMT_A4R4G4B4,
        OP_RGB & !op::MEMBEROFGROUP_ARGB,
    ),
    (devcap::SURFACEFMT_R5G6B5, OP_RGB | OP_DISPLAY),
    (
        devcap::SURFACEFMT_LUMINANCE16,
        OP_TEXTURE | op::OFFSCREENPLAIN,
    ),
    (
        devcap::SURFACEFMT_LUMINANCE8_ALPHA8,
        OP_TEXTURE | op::OFFSCREENPLAIN,
    ),
    (devcap::SURFACEFMT_ALPHA8, OP_TEXTURE | op::OFFSCREENPLAIN),
    (
        devcap::SURFACEFMT_LUMINANCE8,
        OP_TEXTURE | op::OFFSCREENPLAIN,
    ),
    // Depth textures, as "hardware shadow maps": D16 and D24S8 compare
    // (NVIDIA's way), DF16, DF24 and INTZ read the depth (ATI's and the
    // later common way). Unigine refuses Direct3D 9 without one of them.
    (devcap::SURFACEFMT_Z_D16, OP_DEPTH | op::TEXTURE),
    (devcap::SURFACEFMT_Z_D24S8, OP_DEPTH | op::TEXTURE),
    (devcap::SURFACEFMT_Z_D24X8, OP_DEPTH | op::TEXTURE),
    (devcap::SURFACEFMT_Z_DF16, OP_DEPTH | op::TEXTURE),
    (devcap::SURFACEFMT_Z_DF24, OP_DEPTH | op::TEXTURE),
    (devcap::SURFACEFMT_Z_D24S8_INT, OP_DEPTH | op::TEXTURE),
    (devcap::SURFACEFMT_DXT1, OP_TEXTURE),
    (devcap::SURFACEFMT_DXT2, OP_TEXTURE),
    (devcap::SURFACEFMT_DXT3, OP_TEXTURE),
    (devcap::SURFACEFMT_DXT4, OP_TEXTURE),
    (devcap::SURFACEFMT_DXT5, OP_TEXTURE),
    (devcap::SURFACEFMT_BUMPU8V8, OP_TEXTURE | op::BUMPMAP),
    (devcap::SURFACEFMT_Q8W8V8U8, OP_TEXTURE | op::BUMPMAP),
    (devcap::SURFACEFMT_V16U16, OP_TEXTURE | op::BUMPMAP),
    (devcap::SURFACEFMT_R_S10E5, OP_FLOAT),
    (devcap::SURFACEFMT_R_S23E8, OP_FLOAT),
    (devcap::SURFACEFMT_RG_S10E5, OP_FLOAT),
    (devcap::SURFACEFMT_RG_S23E8, OP_FLOAT),
    (devcap::SURFACEFMT_ARGB_S10E5, OP_FLOAT),
    (devcap::SURFACEFMT_ARGB_S23E8, OP_FLOAT),
    (
        devcap::SURFACEFMT_G16R16,
        OP_TEXTURE | OP_TARGET | op::OFFSCREENPLAIN | op::AUTOGENMIPMAP,
    ),
    (
        devcap::SURFACEFMT_A16B16G16R16,
        OP_TEXTURE | OP_TARGET | op::OFFSCREENPLAIN | op::AUTOGENMIPMAP,
    ),
    (devcap::SURFACEFMT_ATI1, OP_TEXTURE),
    (devcap::SURFACEFMT_ATI2, OP_TEXTURE),
];

/// The `DXFMT` value of a surface format.
pub fn dxfmt(format: u32) -> u32 {
    match format {
        // Direct3D 9 names of DXGI formats. The VMware DirectX 10/11 driver
        // uses some of them for DXGI formats (A8R8G8B8 for B8G8R8A8_UNORM,
        // ALPHA8 for A8_UNORM, ...): same layout, same support.
        f::X8R8G8B8 | f::A8R8G8B8 | f::R5G6B5 | f::A1R5G5B5 | f::A4R4G4B4 => COLOR,
        f::ALPHA8 | f::A2R10G10B10 => COLOR,
        f::ARGB_S10E5 | f::ARGB_S23E8 | f::A16B16G16R16 => COLOR,
        f::R_S10E5 | f::R_S23E8 | f::RG_S10E5 | f::RG_S23E8 | f::G16R16 => COLOR,
        f::V8U8 | f::Q8W8V8U8 | f::V16U16 => COLOR,
        f::Z_D16 | f::Z_D24S8 | f::Z_D24X8 => DEPTH,
        f::DXT1 | f::DXT2 | f::DXT3 | f::DXT4 | f::DXT5 => SAMPLED,
        // Luminance: a red channel read through a swizzle, sampled only.
        f::LUMINANCE8 | f::LUMINANCE16 | f::LUMINANCE8_ALPHA8 => SAMPLED,
        // Raw data: buffers of any kind.
        f::BUFFER => dxfmt::SUPPORTED | VB,

        f::R32G32B32A32_FLOAT => COLOR | VB,
        f::R32G32B32A32_UINT | f::R32G32B32A32_SINT => INTEGER | VB,
        f::R32G32B32A32_TYPELESS => TYPELESS,
        // Three-channel 32-bit formats: read and vertex data, never
        // rendered to.
        f::R32G32B32_FLOAT | f::R32G32B32_UINT | f::R32G32B32_SINT => SAMPLED | VB,
        f::R32G32B32_TYPELESS => COMPRESSED_TYPELESS,

        f::R16G16B16A16_FLOAT | f::R16G16B16A16_UNORM | f::R16G16B16A16_SNORM => COLOR | VB,
        f::R16G16B16A16_UINT | f::R16G16B16A16_SINT => INTEGER | VB,
        f::R16G16B16A16_TYPELESS => TYPELESS,

        f::R32G32_FLOAT => COLOR | VB,
        f::R32G32_UINT | f::R32G32_SINT => INTEGER | VB,
        f::R32G32_TYPELESS => TYPELESS,

        f::R32G8X24_TYPELESS => DEPTH_TYPELESS,
        f::D32_FLOAT_S8X24_UINT => DEPTH,
        f::R32_FLOAT_X8X24 => DEPTH_READ,
        f::X32_G8X24_UINT => STENCIL_READ,

        f::R10G10B10A2_UNORM => COLOR | VB,
        f::R10G10B10A2_UINT => INTEGER | VB,
        f::R10G10B10A2_TYPELESS => TYPELESS,
        f::R11G11B10_FLOAT => COLOR | VB,

        f::R8G8B8A8_UNORM | f::R8G8B8A8_SNORM => COLOR | VB,
        f::R8G8B8A8_UNORM_SRGB => COLOR,
        f::R8G8B8A8_UINT | f::R8G8B8A8_SINT => INTEGER | VB,
        f::R8G8B8A8_TYPELESS => TYPELESS,

        f::R16G16_FLOAT | f::R16G16_UNORM | f::R16G16_SNORM => COLOR | VB,
        f::R16G16_UINT | f::R16G16_SINT => INTEGER | VB,
        f::R16G16_TYPELESS => TYPELESS,

        f::R32_FLOAT => COLOR | VB,
        f::R32_UINT | f::R32_SINT => INTEGER | VB,
        f::R32_TYPELESS => TYPELESS,
        f::D32_FLOAT => DEPTH,

        f::R24G8_TYPELESS => DEPTH_TYPELESS,
        f::D24_UNORM_S8_UINT => DEPTH,
        f::R24_UNORM_X8 => DEPTH_READ,
        f::X24_G8_UINT => STENCIL_READ,

        f::R8G8_UNORM | f::R8G8_SNORM => COLOR | VB,
        f::R8G8_UINT | f::R8G8_SINT => INTEGER | VB,
        f::R8G8_TYPELESS => TYPELESS,

        f::R16_FLOAT | f::R16_UNORM | f::R16_SNORM => COLOR | VB,
        f::R16_UINT | f::R16_SINT => INTEGER | VB,
        f::R16_TYPELESS => TYPELESS,
        f::D16_UNORM => DEPTH,

        f::R8_UNORM | f::R8_SNORM => COLOR | VB,
        f::R8_UINT | f::R8_SINT => INTEGER | VB,
        f::R8_TYPELESS => TYPELESS,
        f::A8_UNORM => COLOR,

        // Shared exponent and packed 4:2:2: sampled, never rendered to.
        f::R9G9B9E5_SHAREDEXP => SAMPLED,
        f::R8G8_B8G8_UNORM | f::G8R8_G8B8_UNORM => SAMPLED,
        // Scan-out only (the extended range is not read nor rendered).
        f::R10G10B10_XR_BIAS_A2_UNORM => dxfmt::SUPPORTED,
        // Video: storage and copies (the DirectX 11.1 DDI requires them).
        f::YUY2 | f::NV12 => dxfmt::SUPPORTED,

        f::BC1_UNORM | f::BC1_UNORM_SRGB | f::BC2_UNORM | f::BC2_UNORM_SRGB => SAMPLED,
        f::BC3_UNORM | f::BC3_UNORM_SRGB => SAMPLED,
        f::BC4_UNORM | f::BC4_SNORM | f::ATI1 | f::BC5_UNORM | f::BC5_SNORM | f::ATI2 => SAMPLED,
        f::BC1_TYPELESS | f::BC2_TYPELESS | f::BC3_TYPELESS => COMPRESSED_TYPELESS,
        f::BC4_TYPELESS | f::BC5_TYPELESS => COMPRESSED_TYPELESS,
        // BC6H and BC7 come with Direct3D 11 (shader model 5).
        f::BC6H_UF16 | f::BC6H_SF16 | f::BC7_UNORM | f::BC7_UNORM_SRGB => SAMPLED,
        f::BC6H_TYPELESS | f::BC7_TYPELESS => COMPRESSED_TYPELESS,

        f::B8G8R8A8_UNORM | f::B8G8R8X8_UNORM => COLOR | VB,
        f::B8G8R8A8_UNORM_SRGB | f::B8G8R8X8_UNORM_SRGB => COLOR,
        f::B8G8R8A8_TYPELESS | f::B8G8R8X8_TYPELESS => TYPELESS,
        f::B5G6R5_UNORM | f::B5G5R5A1_UNORM => COLOR,

        _ => 0,
    }
}

/// DX contexts the device holds (`SVGA3D_DEVCAP_MAX_CONTEXT_IDS`).
pub const MAX_CONTEXT_IDS: u32 = 256;

/// `(devcap, value)` of everything but the formats.
const CAPS: &[(u32, u32)] = &[
    (devcap::THREE_D, 1),
    // Direct3D 9 fixed function and shader model 3 limits.
    (devcap::MAX_LIGHTS, 8),
    (devcap::MAX_TEXTURES, 8),
    (devcap::MAX_CLIP_PLANES, 6),
    (devcap::S23E8_TEXTURES, 1),
    (devcap::S10E5_TEXTURES, 1),
    (devcap::MAX_FIXED_VERTEXBLEND, 4),
    (devcap::D16_BUFFER_FORMAT, 1),
    (devcap::D24S8_BUFFER_FORMAT, 1),
    (devcap::D24X8_BUFFER_FORMAT, 1),
    // Occlusion queries (bit SVGA3D_QUERYTYPE_OCCLUSION).
    (devcap::QUERY_TYPES, 1),
    (devcap::TEXTURE_GRADIENT_SAMPLING, 1),
    (devcap::MAX_POINT_SIZE, 0x4380_0000), // 256.0
    (devcap::MAX_SHADER_TEXTURES, 16),
    (devcap::MAX_TEXTURE_ASPECT_RATIO, 16_384),
    (devcap::MAX_VERTEX_SHADER_INSTRUCTIONS, 32_768),
    (devcap::MAX_FRAGMENT_SHADER_INSTRUCTIONS, 32_768),
    (devcap::MAX_VERTEX_SHADER_TEMPS, 32),
    (devcap::MAX_FRAGMENT_SHADER_TEMPS, 32),
    // Every D3DTEXOPCAPS_* operation, DISABLE to LERP.
    (devcap::TEXTURE_OPS, 0x03ff_ffff),
    (devcap::MAX_VERTEX_SHADER_TEXTURES, 4),
    (devcap::AUTOGENMIPMAPS, 1),
    (devcap::LINE_AA, 1),
    (devcap::MAX_LINE_WIDTH, 0x3f80_0000),    // 1.0
    (devcap::MAX_AA_LINE_WIDTH, 0x3f80_0000), // 1.0
    // Shader model 4.1, in the legacy enumerations: SVGA3DVSVERSION_40
    // and SVGA3DPSVERSION_40.
    (devcap::VERTEX_SHADER_VERSION, 9),
    (devcap::VERTEX_SHADER, 1),
    (devcap::FRAGMENT_SHADER_VERSION, 15),
    (devcap::FRAGMENT_SHADER, 1),
    (devcap::MAX_RENDER_TARGETS, 8),
    (devcap::MAX_SIMULTANEOUS_RENDER_TARGETS, 8),
    // Direct3D 11 limits (feature level 11 needs 16384).
    (devcap::MAX_TEXTURE_WIDTH, 16_384),
    (devcap::MAX_TEXTURE_HEIGHT, 16_384),
    (devcap::MAX_VOLUME_EXTENT, 2048),
    (devcap::MAX_TEXTURE_REPEAT, 16_384),
    (devcap::MAX_TEXTURE_ANISOTROPY, 16),
    (devcap::MAX_PRIMITIVE_COUNT, 0x00ff_ffff),
    (devcap::MAX_VERTEX_INDEX, 0x00ff_ffff),
    (devcap::MAX_CONTEXT_IDS, MAX_CONTEXT_IDS),
    (devcap::MAX_SURFACE_IDS, 32_768),
    (devcap::DXCONTEXT, 1),
    (devcap::DX_MAX_VERTEXBUFFERS, 32),
    (devcap::DX_MAX_CONSTANT_BUFFERS, 14),
    (devcap::DX_PROVOKING_VERTEX, 1),
    (devcap::SM41, 1),
    (devcap::MULTISAMPLE_2X, 1),
    (devcap::MULTISAMPLE_4X, 1),
    (devcap::MS_FULL_QUALITY, 1),
    (devcap::LOGICOPS, 1),
    (devcap::LOGIC_BLENDOPS, 1),
    (devcap::MULTISAMPLE_8X, 1),
    (devcap::MAX_FORCED_SAMPLE_COUNT, 8),
];

/// The table, indexed by devcap: empty without 3D.
pub fn table(three_d: bool) -> Vec<u32> {
    if !three_d {
        return Vec::new();
    }
    let mut table = vec![0u32; devcap::MAX as usize];
    for &(index, value) in CAPS.iter().chain(FORMAT_OPS) {
        table[index as usize] = value;
    }
    for format in 1..=f::R11G11B10_TYPELESS {
        if let Some(index) = devcap::dxfmt_index(format) {
            table[index as usize] = dxfmt(format);
        }
    }
    if let Ok(probe) = std::env::var("RSVGA_PROBE_DEVCAPS") {
        apply_probe(&mut table, &probe);
    }
    table
}

/// Driver probing: `RSVGA_PROBE_DEVCAPS="258=1,245=0x0"` overrides
/// entries (index in decimal, value in decimal or 0x hex).
fn apply_probe(table: &mut [u32], probe: &str) {
    let number = |s: &str| match s.trim().strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.trim().parse().ok(),
    };
    for entry in probe.split(',').filter(|e| !e.trim().is_empty()) {
        let parsed = entry
            .split_once('=')
            .and_then(|(i, v)| Some((number(i)?, number(v)?)));
        match parsed.and_then(|(i, v)| Some((table.get_mut(i as usize)?, i, v))) {
            Some((slot, index, value)) => {
                *slot = value;
                log!("PROBE devcap {index} = {value:#x}: advertised, not implemented");
            }
            None => log!("PROBE devcap entry {entry:?} ignored"),
        }
    }
}

/// Words to store at [`fifo::CAPS_3D`]: one `DEVCAPS` record holding the
/// non-zero entries, then an empty record that ends the list. Entries that
/// do not fit in the area are left out (the guest then reads them as 0).
pub fn fifo_record(table: &[u32]) -> Vec<u32> {
    let area = (fifo::CAPS_3D_LAST - fifo::CAPS_3D + 1) as usize;
    // Two header words, then pairs, then the two-word terminator.
    let room = (area - 4) / 2;
    let pairs: Vec<(u32, u32)> = (0u32..)
        .zip(table.iter().copied())
        .filter(|&(_, value)| value != 0)
        .take(room)
        .collect();
    if pairs.is_empty() {
        return vec![0, 0];
    }
    let length = 2 + 2 * pairs.len();
    let mut words = Vec::with_capacity(length + 2);
    words.push(u32::try_from(length).unwrap_or(u32::MAX));
    words.push(fifo_caps_record::DEVCAPS);
    for (index, value) in pairs {
        words.extend([index, value]);
    }
    words.extend([0, 0]);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_3d_means_an_empty_table_and_record() {
        assert!(table(false).is_empty());
        assert_eq!(fifo_record(&table(false)), [0, 0]);
    }

    #[test]
    fn the_fifo_record_lists_non_zero_entries() {
        let mut t = vec![0u32; 10];
        t[0] = 1;
        t[7] = 9;
        assert_eq!(
            fifo_record(&t),
            [6, fifo_caps_record::DEVCAPS, 0, 1, 7, 9, 0, 0]
        );
        let full = table(true);
        assert_eq!(full[devcap::THREE_D as usize], 1);
        let record = fifo_record(&full);
        assert!(record.len() <= (fifo::CAPS_3D_LAST - fifo::CAPS_3D + 1) as usize);
        // Never more than the area, even with every entry set.
        let crowded = vec![1u32; devcap::MAX as usize];
        assert!(fifo_record(&crowded).len() <= (fifo::CAPS_3D_LAST - fifo::CAPS_3D + 1) as usize);
    }

    #[test]
    fn probes_override_entries() {
        let mut t = vec![0u32; 300];
        apply_probe(&mut t, "258=1, 0x10=0x20,bad,999=1,");
        assert_eq!((t[258], t[16]), (1, 0x20));
    }

    #[test]
    fn direct3d_9_sees_a_shader_model_3_device_with_a_3d_display_format() {
        let t = table(true);
        // VMware's Direct3D 9 driver maps these to D3DCAPS9 (checked in the
        // guest with guest/umdcaps: VS and PS 3.0, 16384 textures).
        assert!(t[devcap::VERTEX_SHADER_VERSION as usize] >= svga_proto::shader_version::VS_30);
        assert!(t[devcap::FRAGMENT_SHADER_VERSION as usize] >= svga_proto::shader_version::PS_30);
        assert_eq!(t[devcap::MAX_TEXTURE_WIDTH as usize], 16_384);
        // Without "display mode, 3D accelerated" on the desktop format,
        // Windows offers no Direct3D 9 hardware device.
        let x8r8g8b8 = t[devcap::SURFACEFMT_X8R8G8B8 as usize];
        assert_eq!(x8r8g8b8 & op::DISPLAYMODE, op::DISPLAYMODE);
        assert_eq!(
            x8r8g8b8 & op::THREE_D_ACCELERATION,
            op::THREE_D_ACCELERATION
        );
        // The rules of Windows' Direct3D 9 runtime, which otherwise
        // disables the driver: 3D acceleration only with display mode and
        // never on an alpha format; the ARGB group only on its formats;
        // not both 16-bit RGB formats as display modes.
        let alpha = [
            devcap::SURFACEFMT_A8R8G8B8,
            devcap::SURFACEFMT_A1R5G5B5,
            devcap::SURFACEFMT_A4R4G4B4,
            devcap::SURFACEFMT_A2R10G10B10,
        ];
        for &(index, ops) in FORMAT_OPS {
            if ops & op::THREE_D_ACCELERATION != 0 {
                assert_ne!(ops & op::DISPLAYMODE, 0, "devcap {index}");
                assert!(!alpha.contains(&index), "devcap {index}");
            }
        }
        let argb_group = [
            devcap::SURFACEFMT_X8R8G8B8,
            devcap::SURFACEFMT_A8R8G8B8,
            devcap::SURFACEFMT_R5G6B5,
            devcap::SURFACEFMT_X1R5G5B5,
            devcap::SURFACEFMT_A1R5G5B5,
            devcap::SURFACEFMT_A2R10G10B10,
            devcap::SURFACEFMT_ARGB_S10E5,
            devcap::SURFACEFMT_ARGB_S23E8,
        ];
        for &(index, ops) in FORMAT_OPS {
            if ops & op::MEMBEROFGROUP_ARGB != 0 {
                assert!(argb_group.contains(&index), "devcap {index}");
            }
        }
        let display = |i: u32| t[i as usize] & op::DISPLAYMODE != 0;
        assert!(!(display(devcap::SURFACEFMT_R5G6B5) && display(devcap::SURFACEFMT_X1R5G5B5)));
        // Depth formats are depth buffers and shadow map textures.
        for depth in [
            devcap::SURFACEFMT_Z_D16,
            devcap::SURFACEFMT_Z_D24S8,
            devcap::SURFACEFMT_Z_DF16,
            devcap::SURFACEFMT_Z_DF24,
            devcap::SURFACEFMT_Z_D24S8_INT,
        ] {
            assert_ne!(t[depth as usize] & op::TEXTURE, 0);
            assert_ne!(t[depth as usize] & op::ZSTENCIL, 0);
        }
        // The float entries hold IEEE bits.
        assert_eq!(f32::from_bits(t[devcap::MAX_POINT_SIZE as usize]), 256.0);
    }

    #[test]
    fn format_devcaps_sit_where_the_headers_put_them() {
        assert_eq!(devcap::dxfmt_index(f::X8R8G8B8), Some(100));
        assert_eq!(devcap::dxfmt_index(f::B8G8R8X8_UNORM_SRGB), Some(216));
        assert_eq!(devcap::dxfmt_index(f::BC5_UNORM), Some(243));
        assert_eq!(devcap::dxfmt_index(f::BC6H_TYPELESS), Some(251));
        assert_eq!(devcap::dxfmt_index(f::BC7_UNORM_SRGB), Some(256));
        for none in [0, f::B4G4R4A4_UNORM, f::AYUV, f::R11G11B10_TYPELESS, 154] {
            assert_eq!(devcap::dxfmt_index(none), None);
        }
        // The VMware driver's own values for the formats without devcap.
        assert_eq!(COLOR, 0x2f7);
        assert_eq!(TYPELESS, 0x2e1);
    }

    /// Minimum devcaps of feature level 10_1, every DDI version (up to
    /// DirectX 11.1), as the VMware user-mode DirectX driver (9.17) checks
    /// them: values for limits, bits that must be set for formats.
    const FEATURE_LEVEL_10_1: &[(u32, u32)] = &[
        (0, 0x1),
        (95, 0x1),
        (19, 0x2000),
        (20, 0x2000),
        (21, 0x800),
        (22, 0x2000),
        (244, 0x1),
        (246, 0x1),
        (136, 0x1),
        (145, 0x2e1),
        (221, 0x3f7),
        (146, 0x3e7),
        (147, 0x3e7),
        (148, 0xe1),
        (149, 0x1e3),
        (150, 0x1e3),
        (151, 0x1e3),
        (152, 0x2e1),
        (222, 0x3f7),
        (223, 0x3f7),
        (153, 0x3e7),
        (154, 0x3f7),
        (155, 0x3e7),
        (156, 0x2e1),
        (224, 0x3f7),
        (157, 0x3e7),
        (158, 0x3e7),
        (159, 0x261),
        (160, 0x269),
        (161, 0x63),
        (162, 0x63),
        (163, 0x2e1),
        (225, 0x3f7),
        (164, 0x3e7),
        (165, 0x3f7),
        (166, 0x2e1),
        (167, 0x3f7),
        (168, 0x2f7),
        (169, 0x3e7),
        (226, 0x3f7),
        (170, 0x3e7),
        (171, 0x2e1),
        (227, 0x3f7),
        (228, 0x3f7),
        (172, 0x3e7),
        (229, 0x3f7),
        (173, 0x3e7),
        (174, 0x2e1),
        (175, 0x269),
        (230, 0x3f7),
        (176, 0x3e7),
        (177, 0x3e7),
        (178, 0x261),
        (179, 0x269),
        (180, 0x63),
        (181, 0x63),
        (182, 0x2e1),
        (183, 0x3f7),
        (184, 0x3e7),
        (231, 0x3f7),
        (185, 0x3e7),
        (186, 0x2e1),
        (232, 0x3f7),
        (233, 0x269),
        (187, 0x3f7),
        (188, 0x3e7),
        (189, 0x3f7),
        (190, 0x3e7),
        (191, 0x2e1),
        (192, 0x3f7),
        (193, 0x3e7),
        (194, 0x3f7),
        (195, 0x3e7),
        (234, 0x2f7),
        (197, 0xe3),
        (198, 0xe3),
        (199, 0xe3),
        (200, 0xe1),
        (235, 0xe3),
        (201, 0xe3),
        (202, 0xe1),
        (236, 0xe3),
        (203, 0xe3),
        (204, 0xe1),
        (237, 0xe3),
        (205, 0xe3),
        (206, 0xe1),
        (242, 0xe3),
        (208, 0xe3),
        (209, 0xe1),
        (243, 0xe3),
        (211, 0xe3),
        (238, 0x2f7),
        (239, 0xe3),
        (240, 0x3f7),
        (241, 0x3f7),
        (212, 0x1),
        (213, 0x2e1),
        (214, 0x2f7),
        (215, 0x2e1),
        (216, 0x2f7),
        (143, 0x1),
        (142, 0x1),
    ];

    #[test]
    fn the_vmware_driver_gets_feature_level_10_1() {
        let t = table(true);
        let missing: Vec<String> = FEATURE_LEVEL_10_1
            .iter()
            .filter(|&&(index, need)| {
                let have = t[index as usize];
                let is_format = devcap::dxfmt_index(1).is_some_and(|first| index >= first)
                    && !(devcap::SM41..=devcap::LOGIC_BLENDOPS).contains(&index)
                    && index != devcap::SM5
                    && index != devcap::MULTISAMPLE_8X;
                if is_format {
                    have & need != need
                } else {
                    have < need
                }
            })
            .map(|(index, need)| format!("{index}: {:#x} < {need:#x}", t[*index as usize]))
            .collect();
        assert!(missing.is_empty(), "missing devcaps: {missing:?}");
    }

    #[test]
    fn formats_follow_direct3d_rules() {
        let render = |format| dxfmt(format) & dxfmt::COLOR_RENDERTARGET != 0;
        // What the VMware debug driver asserts on.
        for never in [
            f::R9G9B9E5_SHAREDEXP,
            f::R32G32B32_TYPELESS,
            f::R32G32B32_FLOAT,
            f::R24G8_TYPELESS,
            f::R24_UNORM_X8,
        ] {
            assert!(!render(never), "format {never}");
        }
        for format in 1..=f::R11G11B10_TYPELESS {
            let caps = dxfmt(format);
            // Anything advertised is at least supported.
            assert!(caps == 0 || caps & dxfmt::SUPPORTED != 0, "format {format}");
            // Color and depth targets exclude each other, and only color
            // targets blend.
            assert!(
                caps & dxfmt::COLOR_RENDERTARGET == 0 || caps & dxfmt::DEPTH_RENDERTARGET == 0,
                "format {format}"
            );
            if caps & dxfmt::BLENDABLE != 0 {
                assert!(render(format), "format {format}");
            }
        }
        // The desktop formats, and the glyph atlas of DWM (ALPHA8).
        for desktop in [
            f::A8R8G8B8,
            f::X8R8G8B8,
            f::B8G8R8A8_UNORM,
            f::B8G8R8X8_UNORM,
            f::ALPHA8,
        ] {
            assert_eq!(dxfmt(desktop) & COLOR, COLOR);
        }
    }
}
