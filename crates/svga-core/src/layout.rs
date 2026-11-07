//! Layout of a guest-backed surface in its backing memory (MOB).
//!
//! Guest drivers store a surface as one layer (array slice or cube face)
//! after the other; inside a layer, every mip level in turn; each image is
//! tightly packed in blocks of the format (1x1 pixel for plain formats,
//! 4x4 for block-compressed ones). The device's host copy uses the very same
//! layout, so a byte offset means the same thing on both sides.
//!
//! All arithmetic is done in `u64` and checked: guest sizes cannot overflow.

use svga_proto::format::{self, FormatDesc};

/// A size or position in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size3 {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Depth in pixels (1 for 2D images).
    pub depth: u32,
}

/// A box inside an image, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Box3 {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Front.
    pub z: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
    /// Depth.
    pub d: u32,
}

/// Why a surface definition cannot be laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// The format is not part of the protocol.
    UnknownFormat(u32),
    /// A dimension, the mip count or the layer count is zero or too large.
    BadDimensions,
    /// The surface would exceed the device limit.
    TooLarge(u64),
    /// Planar (YUV) formats are not supported.
    Planar(u32),
}

/// Largest mip chain: 2^16 pixels need 17 levels.
pub const MAX_MIP_LEVELS: u32 = 17;
/// Largest number of layers (arrays of cube maps included).
pub const MAX_LAYERS: u32 = 6 * 2048;

/// Where each image of a surface lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceLayout {
    format: u32,
    desc: FormatDesc,
    size: Size3,
    layers: u32,
    samples: u32,
    /// Offset of each mip level inside a layer, then the layer size.
    mip_offsets: Vec<u64>,
}

fn blocks(pixels: u32, block: u32) -> u64 {
    u64::from(pixels).div_ceil(u64::from(block.max(1)))
}

impl SurfaceLayout {
    /// Lays out a surface; `max_bytes` bounds its total size.
    pub fn new(
        format: u32,
        size: Size3,
        mip_levels: u32,
        layers: u32,
        samples: u32,
        max_bytes: u64,
    ) -> Result<Self, LayoutError> {
        let desc = format::desc(format).ok_or(LayoutError::UnknownFormat(format))?;
        if desc.bytes_per_block != desc.pitch_bytes_per_block {
            return Err(LayoutError::Planar(format));
        }
        let dims_ok = size.width > 0 && size.height > 0 && size.depth > 0;
        if !dims_ok
            || !(1..=MAX_MIP_LEVELS).contains(&mip_levels)
            || !(1..=MAX_LAYERS).contains(&layers)
        {
            return Err(LayoutError::BadDimensions);
        }
        let mut layout = Self {
            format,
            desc,
            size,
            layers,
            samples: samples.max(1),
            mip_offsets: Vec::with_capacity(mip_levels as usize + 1),
        };
        let mut offset = 0u64;
        for mip in 0..mip_levels {
            layout.mip_offsets.push(offset);
            offset = offset
                .checked_add(layout.image_bytes(mip))
                .ok_or(LayoutError::TooLarge(u64::MAX))?;
        }
        layout.mip_offsets.push(offset);
        let total = layout.total_bytes();
        if total > max_bytes {
            return Err(LayoutError::TooLarge(total));
        }
        Ok(layout)
    }

    /// The surface format.
    pub fn format(&self) -> u32 {
        self.format
    }

    /// Block layout of the format.
    pub fn desc(&self) -> FormatDesc {
        self.desc
    }

    /// Size of the base level.
    pub fn size(&self) -> Size3 {
        self.size
    }

    /// Number of mip levels.
    pub fn mip_levels(&self) -> u32 {
        u32::try_from(self.mip_offsets.len().saturating_sub(1)).unwrap_or(0)
    }

    /// Number of layers (array slices, cube faces).
    pub fn layers(&self) -> u32 {
        self.layers
    }

    /// Size of mip level `mip` (at least one pixel in each direction).
    pub fn mip_size(&self, mip: u32) -> Size3 {
        let shrink = |v: u32| v.checked_shr(mip).unwrap_or(0).max(1);
        Size3 {
            width: shrink(self.size.width),
            height: shrink(self.size.height),
            depth: shrink(self.size.depth),
        }
    }

    /// Bytes per row of blocks of mip level `mip`.
    pub fn row_pitch(&self, mip: u32) -> u64 {
        let size = self.mip_size(mip);
        blocks(size.width, self.desc.block_width)
            .saturating_mul(u64::from(self.desc.pitch_bytes_per_block))
    }

    /// Bytes per 2D slice of mip level `mip`.
    pub fn slice_pitch(&self, mip: u32) -> u64 {
        let size = self.mip_size(mip);
        blocks(size.height, self.desc.block_height).saturating_mul(self.row_pitch(mip))
    }

    /// Bytes of one image (one mip level of one layer).
    pub fn image_bytes(&self, mip: u32) -> u64 {
        let size = self.mip_size(mip);
        blocks(size.depth, self.desc.block_depth).saturating_mul(self.slice_pitch(mip))
    }

    /// Bytes of one layer (its whole mip chain).
    pub fn layer_bytes(&self) -> u64 {
        self.mip_offsets.last().copied().unwrap_or(0)
    }

    /// Bytes of the whole surface, samples included.
    pub fn total_bytes(&self) -> u64 {
        self.layer_bytes()
            .saturating_mul(u64::from(self.layers))
            .saturating_mul(u64::from(self.samples))
    }

    /// Offset of image (`layer`, `mip`), or `None` if it does not exist.
    pub fn image_offset(&self, layer: u32, mip: u32) -> Option<u64> {
        if layer >= self.layers {
            return None;
        }
        let in_layer = *self.mip_offsets.get(usize::try_from(mip).ok()?)?;
        if mip >= self.mip_levels() {
            return None;
        }
        self.layer_bytes()
            .checked_mul(u64::from(layer))?
            .checked_add(in_layer)
    }

    /// The byte ranges of `area` in image (`layer`, `mip`): one
    /// `(offset, length)` per row of blocks, in surface offsets. The box is
    /// clipped to the image; `None` if the image does not exist or nothing
    /// is left.
    pub fn box_rows(&self, layer: u32, mip: u32, area: Box3) -> Option<Vec<(u64, u64)>> {
        let base = self.image_offset(layer, mip)?;
        let size = self.mip_size(mip);
        let clip = |start: u32, len: u32, max: u32| {
            let end = start.saturating_add(len).min(max);
            (start < end).then_some((start, end))
        };
        let (x0, x1) = clip(area.x, area.w, size.width)?;
        let (y0, y1) = clip(area.y, area.h, size.height)?;
        let (z0, z1) = clip(area.z, area.d, size.depth)?;
        let d = self.desc;
        let (bx0, bx1) = (
            u64::from(x0.checked_div(d.block_width).unwrap_or(0)),
            blocks(x1, d.block_width),
        );
        let (by0, by1) = (
            u64::from(y0.checked_div(d.block_height).unwrap_or(0)),
            blocks(y1, d.block_height),
        );
        let (bz0, bz1) = (
            u64::from(z0.checked_div(d.block_depth).unwrap_or(0)),
            blocks(z1, d.block_depth),
        );
        let bpb = u64::from(d.bytes_per_block);
        let (row, slice) = (self.row_pitch(mip), self.slice_pitch(mip));
        let length = bx1.checked_sub(bx0)?.checked_mul(bpb)?;
        let mut rows = Vec::new();
        for bz in bz0..bz1 {
            for by in by0..by1 {
                let offset = base
                    .checked_add(bz.checked_mul(slice)?)?
                    .checked_add(by.checked_mul(row)?)?
                    .checked_add(bx0.checked_mul(bpb)?)?;
                rows.push((offset, length));
            }
        }
        Some(rows)
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn size(width: u32, height: u32) -> Size3 {
        Size3 {
            width,
            height,
            depth: 1,
        }
    }

    #[test]
    fn a_desktop_surface() {
        let l = SurfaceLayout::new(format::B8G8R8X8_UNORM, size(1920, 1080), 1, 1, 0, GIB).unwrap();
        assert_eq!(l.row_pitch(0), 7680);
        assert_eq!(l.total_bytes(), 7680 * 1080);
        assert_eq!(l.image_offset(0, 0), Some(0));
        assert_eq!(l.image_offset(1, 0), None);
        let rows = l
            .box_rows(
                0,
                0,
                Box3 {
                    x: 10,
                    y: 2,
                    z: 0,
                    w: 5,
                    h: 2,
                    d: 1,
                },
            )
            .unwrap();
        assert_eq!(rows, [(2 * 7680 + 40, 20), (3 * 7680 + 40, 20)]);
    }

    #[test]
    fn mips_then_layers_like_vmwgfx() {
        // 8x8 RGBA, 4 levels (8, 4, 2, 1), 2 layers.
        let l = SurfaceLayout::new(format::A8R8G8B8, size(8, 8), 4, 2, 1, GIB).unwrap();
        let level = [256, 64, 16, 4];
        assert_eq!(l.layer_bytes(), level.iter().sum::<u64>());
        assert_eq!(l.image_offset(0, 2), Some(256 + 64));
        assert_eq!(l.image_offset(1, 1), Some(340 + 256));
        assert_eq!(l.mip_size(3), size(1, 1));
        assert_eq!(l.total_bytes(), 680);
    }

    #[test]
    fn compressed_blocks() {
        // DXT1: 4x4 blocks of 8 bytes; 10x6 pixels is 3x2 blocks.
        let l = SurfaceLayout::new(format::DXT1, size(10, 6), 1, 1, 1, GIB).unwrap();
        assert_eq!(l.row_pitch(0), 24);
        assert_eq!(l.total_bytes(), 48);
        let rows = l
            .box_rows(
                0,
                0,
                Box3 {
                    x: 4,
                    y: 0,
                    z: 0,
                    w: 6,
                    h: 5,
                    d: 1,
                },
            )
            .unwrap();
        assert_eq!(rows, [(8, 16), (24 + 8, 16)], "blocks 1..3 of both rows");
    }

    #[test]
    fn hostile_definitions_are_refused() {
        let too_big = SurfaceLayout::new(format::A8R8G8B8, size(65535, 65535), 1, 1, 1, GIB);
        assert!(matches!(too_big, Err(LayoutError::TooLarge(_))));
        assert_eq!(
            SurfaceLayout::new(9999, size(1, 1), 1, 1, 1, GIB),
            Err(LayoutError::UnknownFormat(9999))
        );
        assert_eq!(
            SurfaceLayout::new(format::A8R8G8B8, size(0, 1), 1, 1, 1, GIB),
            Err(LayoutError::BadDimensions)
        );
        assert_eq!(
            SurfaceLayout::new(format::A8R8G8B8, size(1, 1), 99, 1, 1, GIB),
            Err(LayoutError::BadDimensions)
        );
        assert!(matches!(
            SurfaceLayout::new(format::NV12, size(4, 4), 1, 1, 1, GIB),
            Err(LayoutError::Planar(_))
        ));
        // Boxes outside the image give nothing.
        let l = SurfaceLayout::new(format::A8R8G8B8, size(4, 4), 1, 1, 1, GIB).unwrap();
        let outside = Box3 {
            x: 9,
            y: 0,
            z: 0,
            w: 1,
            h: 1,
            d: 1,
        };
        assert_eq!(l.box_rows(0, 0, outside), None);
        assert_eq!(l.box_rows(0, 1, Box3::default()), None, "no such mip");
    }
}
