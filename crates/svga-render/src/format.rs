//! SVGA surface formats as Vulkan formats.
//!
//! A surface's bytes travel between the guest and the GPU without
//! conversion, so a format maps only to a Vulkan format with the same
//! memory layout (same bytes per texel or block, same channel order in
//! memory). Typeless formats take a typed format of their class (images are
//! created mutable, for views of other formats of the class). Formats
//! without such a twin have no mapping yet: 24-bit depth (AMD has no
//! `D24_UNORM_S8_UINT`, so it is stored as `D32_SFLOAT_S8_UINT`, a
//! different layout), packed 4:2:2 and video formats.

use ash::vk;
use svga_proto::format as f;

/// What kind of Vulkan resource a surface becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A `VkBuffer` (`SVGA3D_BUFFER`).
    Buffer,
    /// A color image.
    Color(vk::Format),
    /// A depth (and stencil) image.
    Depth(vk::Format),
    /// A depth image whose layout differs from the guest's (bytes cannot be
    /// copied as they are).
    DepthConverted(vk::Format),
}

/// The Vulkan resource of an SVGA surface format; `None`: not supported.
/// `d24s8`: the GPU supports `D24_UNORM_S8_UINT`.
pub fn kind(format: u32, d24s8: bool) -> Option<Kind> {
    use vk::Format as V;
    let color = |v| Some(Kind::Color(v));
    match format {
        f::BUFFER => Some(Kind::Buffer),

        // Direct3D 9 names.
        f::X8R8G8B8 | f::A8R8G8B8 => color(V::B8G8R8A8_UNORM),
        f::R5G6B5 => color(V::R5G6B5_UNORM_PACK16),
        f::X1R5G5B5 | f::A1R5G5B5 => color(V::A1R5G5B5_UNORM_PACK16),
        f::A4R4G4B4 => color(V::A4R4G4B4_UNORM_PACK16),
        f::A2R10G10B10 => color(V::A2R10G10B10_UNORM_PACK32),
        f::ALPHA8 | f::LUMINANCE8 => color(V::R8_UNORM),
        f::LUMINANCE16 => color(V::R16_UNORM),
        f::LUMINANCE8_ALPHA8 => color(V::R8G8_UNORM),
        f::ARGB_S10E5 => color(V::R16G16B16A16_SFLOAT),
        f::ARGB_S23E8 => color(V::R32G32B32A32_SFLOAT),
        f::R_S10E5 => color(V::R16_SFLOAT),
        f::R_S23E8 => color(V::R32_SFLOAT),
        f::RG_S10E5 => color(V::R16G16_SFLOAT),
        f::RG_S23E8 => color(V::R32G32_SFLOAT),
        f::G16R16 => color(V::R16G16_UNORM),
        f::A16B16G16R16 => color(V::R16G16B16A16_UNORM),
        f::V8U8 => color(V::R8G8_SNORM),
        f::Q8W8V8U8 => color(V::R8G8B8A8_SNORM),
        f::V16U16 => color(V::R16G16_SNORM),
        f::DXT1 => color(V::BC1_RGBA_UNORM_BLOCK),
        f::DXT2 | f::DXT3 => color(V::BC2_UNORM_BLOCK),
        f::DXT4 | f::DXT5 => color(V::BC3_UNORM_BLOCK),
        f::Z_D16 => Some(Kind::Depth(V::D16_UNORM)),

        // DXGI formats.
        f::R32G32B32A32_TYPELESS | f::R32G32B32A32_UINT => color(V::R32G32B32A32_UINT),
        f::R32G32B32A32_SINT => color(V::R32G32B32A32_SINT),
        f::R32G32B32A32_FLOAT => color(V::R32G32B32A32_SFLOAT),
        f::R32G32B32_TYPELESS | f::R32G32B32_UINT => color(V::R32G32B32_UINT),
        f::R32G32B32_SINT => color(V::R32G32B32_SINT),
        f::R32G32B32_FLOAT => color(V::R32G32B32_SFLOAT),
        f::R16G16B16A16_TYPELESS | f::R16G16B16A16_UINT => color(V::R16G16B16A16_UINT),
        f::R16G16B16A16_SINT => color(V::R16G16B16A16_SINT),
        f::R16G16B16A16_SNORM => color(V::R16G16B16A16_SNORM),
        f::R16G16B16A16_UNORM => color(V::R16G16B16A16_UNORM),
        f::R16G16B16A16_FLOAT => color(V::R16G16B16A16_SFLOAT),
        f::R32G32_TYPELESS | f::R32G32_UINT => color(V::R32G32_UINT),
        f::R32G32_SINT => color(V::R32G32_SINT),
        f::R32G32_FLOAT => color(V::R32G32_SFLOAT),
        f::R10G10B10A2_TYPELESS | f::R10G10B10A2_UNORM => color(V::A2B10G10R10_UNORM_PACK32),
        f::R10G10B10A2_UINT => color(V::A2B10G10R10_UINT_PACK32),
        f::R11G11B10_FLOAT | f::R11G11B10_TYPELESS => color(V::B10G11R11_UFLOAT_PACK32),
        f::R8G8B8A8_TYPELESS | f::R8G8B8A8_UNORM => color(V::R8G8B8A8_UNORM),
        f::R8G8B8A8_UNORM_SRGB => color(V::R8G8B8A8_SRGB),
        f::R8G8B8A8_UINT => color(V::R8G8B8A8_UINT),
        f::R8G8B8A8_SNORM => color(V::R8G8B8A8_SNORM),
        f::R8G8B8A8_SINT => color(V::R8G8B8A8_SINT),
        f::R16G16_TYPELESS | f::R16G16_UINT => color(V::R16G16_UINT),
        f::R16G16_SINT => color(V::R16G16_SINT),
        f::R16G16_FLOAT => color(V::R16G16_SFLOAT),
        f::R16G16_UNORM => color(V::R16G16_UNORM),
        f::R16G16_SNORM => color(V::R16G16_SNORM),
        f::R32_TYPELESS | f::R32_UINT => color(V::R32_UINT),
        f::R32_SINT => color(V::R32_SINT),
        f::R32_FLOAT => color(V::R32_SFLOAT),
        f::D32_FLOAT => Some(Kind::Depth(V::D32_SFLOAT)),
        f::R8G8_TYPELESS | f::R8G8_UINT => color(V::R8G8_UINT),
        f::R8G8_SINT => color(V::R8G8_SINT),
        f::R8G8_UNORM => color(V::R8G8_UNORM),
        f::R8G8_SNORM => color(V::R8G8_SNORM),
        f::R16_TYPELESS | f::R16_UINT => color(V::R16_UINT),
        f::R16_SINT => color(V::R16_SINT),
        f::R16_UNORM => color(V::R16_UNORM),
        f::R16_SNORM => color(V::R16_SNORM),
        f::R16_FLOAT => color(V::R16_SFLOAT),
        f::D16_UNORM => Some(Kind::Depth(V::D16_UNORM)),
        f::R8_TYPELESS | f::R8_UINT => color(V::R8_UINT),
        f::R8_SINT => color(V::R8_SINT),
        f::R8_UNORM | f::A8_UNORM => color(V::R8_UNORM),
        f::R8_SNORM => color(V::R8_SNORM),
        f::R9G9B9E5_SHAREDEXP => color(V::E5B9G9R9_UFLOAT_PACK32),
        f::BC1_TYPELESS | f::BC1_UNORM => color(V::BC1_RGBA_UNORM_BLOCK),
        f::BC1_UNORM_SRGB => color(V::BC1_RGBA_SRGB_BLOCK),
        f::BC2_TYPELESS | f::BC2_UNORM => color(V::BC2_UNORM_BLOCK),
        f::BC2_UNORM_SRGB => color(V::BC2_SRGB_BLOCK),
        f::BC3_TYPELESS | f::BC3_UNORM => color(V::BC3_UNORM_BLOCK),
        f::BC3_UNORM_SRGB => color(V::BC3_SRGB_BLOCK),
        f::BC4_TYPELESS | f::BC4_UNORM | f::ATI1 => color(V::BC4_UNORM_BLOCK),
        f::BC4_SNORM => color(V::BC4_SNORM_BLOCK),
        f::BC5_TYPELESS | f::BC5_UNORM | f::ATI2 => color(V::BC5_UNORM_BLOCK),
        f::BC5_SNORM => color(V::BC5_SNORM_BLOCK),
        f::BC6H_TYPELESS | f::BC6H_UF16 => color(V::BC6H_UFLOAT_BLOCK),
        f::BC6H_SF16 => color(V::BC6H_SFLOAT_BLOCK),
        f::BC7_TYPELESS | f::BC7_UNORM => color(V::BC7_UNORM_BLOCK),
        f::BC7_UNORM_SRGB => color(V::BC7_SRGB_BLOCK),
        f::B8G8R8A8_TYPELESS | f::B8G8R8A8_UNORM | f::B8G8R8X8_TYPELESS | f::B8G8R8X8_UNORM => {
            color(V::B8G8R8A8_UNORM)
        }
        f::B8G8R8A8_UNORM_SRGB | f::B8G8R8X8_UNORM_SRGB => color(V::B8G8R8A8_SRGB),
        f::B5G6R5_UNORM => color(V::R5G6B5_UNORM_PACK16),
        f::B5G5R5A1_UNORM => color(V::A1R5G5B5_UNORM_PACK16),
        f::B4G4R4A4_UNORM => color(V::A4R4G4B4_UNORM_PACK16),

        // 24-bit depth: native where it exists.
        f::Z_D24S8 | f::Z_D24X8 | f::D24_UNORM_S8_UINT | f::R24G8_TYPELESS if d24s8 => {
            Some(Kind::Depth(V::D24_UNORM_S8_UINT))
        }
        f::Z_D24S8 | f::Z_D24X8 | f::D24_UNORM_S8_UINT | f::R24G8_TYPELESS => {
            Some(Kind::DepthConverted(V::D32_SFLOAT_S8_UINT))
        }
        f::D32_FLOAT_S8X24_UINT | f::R32G8X24_TYPELESS => {
            Some(Kind::DepthConverted(V::D32_SFLOAT_S8_UINT))
        }
        _ => None,
    }
}

/// Bytes per texel or block of a Vulkan format this module uses, to check
/// it against the SVGA layout.
pub fn vk_bytes(format: vk::Format) -> Option<u32> {
    use vk::Format as V;
    Some(match format {
        V::R8_UNORM | V::R8_SNORM | V::R8_UINT | V::R8_SINT => 1,
        V::R8G8_UNORM | V::R8G8_SNORM | V::R8G8_UINT | V::R8G8_SINT => 2,
        V::R16_UNORM | V::R16_SNORM | V::R16_UINT | V::R16_SINT | V::R16_SFLOAT => 2,
        V::R5G6B5_UNORM_PACK16 | V::A1R5G5B5_UNORM_PACK16 | V::A4R4G4B4_UNORM_PACK16 => 2,
        V::D16_UNORM => 2,
        V::R8G8B8A8_UNORM
        | V::R8G8B8A8_SRGB
        | V::R8G8B8A8_SNORM
        | V::R8G8B8A8_UINT
        | V::R8G8B8A8_SINT
        | V::B8G8R8A8_UNORM
        | V::B8G8R8A8_SRGB => 4,
        V::A2R10G10B10_UNORM_PACK32
        | V::A2B10G10R10_UNORM_PACK32
        | V::A2B10G10R10_UINT_PACK32
        | V::B10G11R11_UFLOAT_PACK32
        | V::E5B9G9R9_UFLOAT_PACK32 => 4,
        V::R16G16_UNORM | V::R16G16_SNORM | V::R16G16_UINT | V::R16G16_SINT | V::R16G16_SFLOAT => 4,
        V::R32_UINT | V::R32_SINT | V::R32_SFLOAT | V::D32_SFLOAT | V::D24_UNORM_S8_UINT => 4,
        V::R16G16B16A16_UNORM
        | V::R16G16B16A16_SNORM
        | V::R16G16B16A16_UINT
        | V::R16G16B16A16_SINT
        | V::R16G16B16A16_SFLOAT => 8,
        V::R32G32_UINT | V::R32G32_SINT | V::R32G32_SFLOAT => 8,
        V::R32G32B32_UINT | V::R32G32B32_SINT | V::R32G32B32_SFLOAT => 12,
        V::R32G32B32A32_UINT | V::R32G32B32A32_SINT | V::R32G32B32A32_SFLOAT => 16,
        V::BC1_RGBA_UNORM_BLOCK
        | V::BC1_RGBA_SRGB_BLOCK
        | V::BC4_UNORM_BLOCK
        | V::BC4_SNORM_BLOCK => 8,
        V::BC2_UNORM_BLOCK
        | V::BC2_SRGB_BLOCK
        | V::BC3_UNORM_BLOCK
        | V::BC3_SRGB_BLOCK
        | V::BC5_UNORM_BLOCK
        | V::BC5_SNORM_BLOCK
        | V::BC6H_UFLOAT_BLOCK
        | V::BC6H_SFLOAT_BLOCK
        | V::BC7_UNORM_BLOCK
        | V::BC7_SRGB_BLOCK => 16,
        _ => return None,
    })
}

/// How a clear color reaches the image: Direct3D always gives floats, which
/// integer formats take as integers.
pub fn clear_value(format: vk::Format, color: [f32; 4]) -> vk::ClearColorValue {
    use vk::Format as V;
    let as_uint = matches!(
        format,
        V::R8_UINT
            | V::R8G8_UINT
            | V::R8G8B8A8_UINT
            | V::R16_UINT
            | V::R16G16_UINT
            | V::R16G16B16A16_UINT
            | V::R32_UINT
            | V::R32G32_UINT
            | V::R32G32B32_UINT
            | V::R32G32B32A32_UINT
            | V::A2B10G10R10_UINT_PACK32
    );
    let as_sint = matches!(
        format,
        V::R8_SINT
            | V::R8G8_SINT
            | V::R8G8B8A8_SINT
            | V::R16_SINT
            | V::R16G16_SINT
            | V::R16G16B16A16_SINT
            | V::R32_SINT
            | V::R32G32_SINT
            | V::R32G32B32_SINT
            | V::R32G32B32A32_SINT
    );
    // Saturating float-to-integer conversions, as Direct3D does: `as`
    // saturates and truncates toward zero, which is the intent.
    #[allow(clippy::cast_possible_truncation)]
    if as_uint {
        vk::ClearColorValue {
            uint32: color.map(|c| c as u32),
        }
    } else if as_sint {
        vk::ClearColorValue {
            int32: color.map(|c| c as i32),
        }
    } else {
        vk::ClearColorValue { float32: color }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_formats_keep_the_guest_layout() {
        for format in 1..=f::R11G11B10_TYPELESS {
            let (Some(desc), Some(kind)) = (f::desc(format), kind(format, false)) else {
                continue;
            };
            let vk_format = match kind {
                Kind::Buffer | Kind::DepthConverted(_) => continue,
                Kind::Color(v) | Kind::Depth(v) => v,
            };
            assert_eq!(
                vk_bytes(vk_format),
                Some(desc.bytes_per_block),
                "format {format} as {vk_format:?}"
            );
        }
    }

    #[test]
    fn the_desktop_and_depth_formats_map() {
        assert_eq!(
            kind(f::A8R8G8B8, false),
            Some(Kind::Color(vk::Format::B8G8R8A8_UNORM))
        );
        assert_eq!(kind(f::BUFFER, false), Some(Kind::Buffer));
        assert_eq!(
            kind(f::D24_UNORM_S8_UINT, false),
            Some(Kind::DepthConverted(vk::Format::D32_SFLOAT_S8_UINT))
        );
        assert_eq!(
            kind(f::D24_UNORM_S8_UINT, true),
            Some(Kind::Depth(vk::Format::D24_UNORM_S8_UINT))
        );
        assert_eq!(kind(f::NV12, false), None);
    }

    #[test]
    fn integer_clears_take_integers() {
        // SAFETY: reading the union member the function filled.
        let uint = unsafe { clear_value(vk::Format::R32_UINT, [3.7, 0.0, 0.0, 0.0]).uint32 };
        assert_eq!(uint[0], 3);
        // SAFETY: as above.
        let float =
            unsafe { clear_value(vk::Format::B8G8R8A8_UNORM, [1.0, 0.5, 0.0, 1.0]).float32 };
        assert_eq!(float, [1.0, 0.5, 0.0, 1.0]);
    }
}
