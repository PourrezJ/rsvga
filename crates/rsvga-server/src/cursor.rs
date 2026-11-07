//! The guest's hardware cursor, converted to plain RGBA for the window.
//!
//! - `SVGA_CMD_DEFINE_ALPHA_CURSOR`: 32-bit pixels `0xAARRGGBB` with
//!   premultiplied alpha (the colour is already scaled by alpha).
//! - `SVGA_CMD_DEFINE_CURSOR`: Windows-style AND/XOR masks, each scanline
//!   padded to 32 bits, most significant bit first.

use svga_core::command::Cursor;

/// A cursor image in straight (not premultiplied) RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Hotspot, inside the image.
    pub hot_x: u16,
    /// Hotspot, inside the image.
    pub hot_y: u16,
    /// `width * height * 4` bytes, R G B A.
    pub rgba: Vec<u8>,
}

fn dims(width: u32, height: u32, hot_x: u32, hot_y: u32) -> Option<(u16, u16, u16, u16)> {
    let (w, h) = (u16::try_from(width).ok()?, u16::try_from(height).ok()?);
    if w == 0 || h == 0 {
        return None;
    }
    // Clamp the hotspot inside the image rather than rejecting the cursor.
    let hx = u16::try_from(hot_x).unwrap_or(u16::MAX).min(w - 1);
    let hy = u16::try_from(hot_y).unwrap_or(u16::MAX).min(h - 1);
    Some((w, h, hx, hy))
}

/// Converts an alpha cursor; `None` if the sizes do not add up.
pub fn from_alpha(
    width: u32,
    height: u32,
    hot_x: u32,
    hot_y: u32,
    pixels: &[u32],
) -> Option<CursorImage> {
    let (w, h, hx, hy) = dims(width, height, hot_x, hot_y)?;
    if pixels.len() != usize::from(w) * usize::from(h) {
        return None;
    }
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for &p in pixels {
        let [b, g, r, a] = p.to_le_bytes();
        let straight = |c: u8| {
            if a == 0 {
                0
            } else {
                (u32::from(c) * 255 / u32::from(a)).min(255) as u8
            }
        };
        rgba.extend_from_slice(&[straight(r), straight(g), straight(b), a]);
    }
    Some(CursorImage {
        width: w,
        height: h,
        hot_x: hx,
        hot_y: hy,
        rgba,
    })
}

/// Bit `x` of a 1 bpp scanline stored in 32-bit words (MSB first per byte).
fn bit(line: &[u32], x: usize) -> bool {
    let word = line.get(x / 32).copied().unwrap_or(0).to_le_bytes();
    let byte = word[(x % 32) / 8];
    byte & (0x80 >> (x % 8)) != 0
}

/// Converts an AND/XOR cursor. Supports a 1 bpp AND mask with a 1 bpp or
/// 32 bpp XOR mask (what Windows drivers send); `None` otherwise.
pub fn from_masks(cursor: &Cursor) -> Option<CursorImage> {
    let (w, h, hx, hy) = dims(
        cursor.width,
        cursor.height,
        cursor.hotspot_x,
        cursor.hotspot_y,
    )?;
    if cursor.and_mask_depth != 1 || !matches!(cursor.xor_mask_depth, 1 | 32) {
        return None;
    }
    let (width, height) = (usize::from(w), usize::from(h));
    let and_stride = width.div_ceil(32);
    let xor_stride = (width * cursor.xor_mask_depth as usize).div_ceil(32);
    if cursor.and_mask.len() < and_stride * height || cursor.xor_mask.len() < xor_stride * height {
        return None;
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let and = &cursor.and_mask[y * and_stride..(y + 1) * and_stride];
        let xor = &cursor.xor_mask[y * xor_stride..(y + 1) * xor_stride];
        for x in 0..width {
            let transparent = bit(and, x);
            let color = if cursor.xor_mask_depth == 1 {
                if bit(xor, x) { 0x00ff_ffff } else { 0 }
            } else {
                xor[x] & 0x00ff_ffff
            };
            let pixel = match (transparent, color) {
                (true, 0) => [0, 0, 0, 0],
                // "Invert the screen" has no RGBA equivalent: draw it black,
                // which reads well on the light backgrounds it is used on.
                (true, _) => [0, 0, 0, 0xff],
                (false, c) => {
                    let [b, g, r, _] = c.to_le_bytes();
                    [r, g, b, 0xff]
                }
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    Some(CursorImage {
        width: w,
        height: h,
        hot_x: hx,
        hot_y: hy,
        rgba,
    })
}

impl CursorImage {
    /// A short fingerprint of the pixels, to tell cursors apart in logs.
    pub fn fingerprint(&self) -> u32 {
        // FNV-1a over the RGBA bytes.
        self.rgba.iter().fold(0x811c_9dc5u32, |h, &b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        })
    }

    /// The image over a grey checkerboard, so transparency is visible when
    /// the cursor is saved as an (opaque) PNG for diagnosis.
    pub fn on_checkerboard(&self) -> crate::display::Frame {
        let (w, h) = (usize::from(self.width), usize::from(self.height));
        let mut pixels = Vec::with_capacity(w * h);
        for (i, px) in self.rgba.chunks_exact(4).enumerate() {
            let (x, y) = (i % w, i / w);
            let bg: u32 = if (x / 4 + y / 4) % 2 == 0 { 0x99 } else { 0x66 };
            let a = u32::from(px[3]);
            let mix = |c: u8| (u32::from(c) * a + bg * (255 - a)) / 255;
            pixels.push((mix(px[0]) << 16) | (mix(px[1]) << 8) | mix(px[2]));
        }
        crate::display::Frame {
            width: w,
            height: h,
            pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_cursors_are_unpremultiplied() {
        // From a real Windows cursor: 60 % opaque premultiplied blue.
        let image = from_alpha(2, 1, 0, 0, &[0x9917_5589, 0]).unwrap();
        assert_eq!(&image.rgba[..4], &[38, 141, 228, 153]);
        assert_eq!(&image.rgba[4..], &[0, 0, 0, 0]);
        assert!(from_alpha(2, 2, 0, 0, &[0; 3]).is_none(), "size mismatch");
        assert!(from_alpha(0, 2, 0, 0, &[]).is_none());
        assert_eq!(
            from_alpha(1, 1, 99, 99, &[0]).map(|c| (c.hot_x, c.hot_y)),
            Some((0, 0))
        );
    }

    #[test]
    fn checkerboard_preview_and_fingerprint() {
        let image = from_alpha(1, 1, 0, 0, &[0xffff_ffff]).unwrap();
        assert_eq!(
            image.on_checkerboard().pixels,
            [0x00ff_ffff],
            "opaque white stays white"
        );
        let clear = from_alpha(1, 1, 0, 0, &[0]).unwrap();
        assert_eq!(
            clear.on_checkerboard().pixels,
            [0x0099_9999],
            "transparent shows the board"
        );
        assert_ne!(image.fingerprint(), clear.fingerprint());
    }

    #[test]
    fn monochrome_masks() {
        // 2x1: pixel 0 transparent (AND 1, XOR 0), pixel 1 white (AND 0, XOR 1).
        let cursor = Cursor {
            id: 0,
            hotspot_x: 1,
            hotspot_y: 0,
            width: 2,
            height: 1,
            and_mask_depth: 1,
            xor_mask_depth: 1,
            and_mask: vec![u32::from_le_bytes([0b1000_0000, 0, 0, 0])],
            xor_mask: vec![u32::from_le_bytes([0b0100_0000, 0, 0, 0])],
        };
        let image = from_masks(&cursor).unwrap();
        assert_eq!(image.rgba, [0, 0, 0, 0, 255, 255, 255, 255]);
        assert_eq!(image.hot_x, 1);
        let color = Cursor {
            xor_mask_depth: 32,
            xor_mask: vec![0, 0x0012_3456],
            ..cursor.clone()
        };
        assert_eq!(
            &from_masks(&color).unwrap().rgba[4..],
            &[0x12, 0x34, 0x56, 255]
        );
        let truncated = Cursor {
            and_mask: Vec::new(),
            ..cursor
        };
        assert!(from_masks(&truncated).is_none());
    }
}
