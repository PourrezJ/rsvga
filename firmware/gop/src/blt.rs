//! `EFI_GRAPHICS_OUTPUT_PROTOCOL.Blt`, written against a pixel surface so the
//! rectangle arithmetic can be tested on the host.
//!
//! Pixels are `u32` in BGRX byte order, which is both the GOP `BltPixel`
//! layout and the 32 bpp framebuffer format of the device.

/// The framebuffer, addressed by pixel.
pub trait Surface {
    /// Visible width in pixels.
    fn width(&self) -> usize;
    /// Visible height in pixels.
    fn height(&self) -> usize;
    /// Reads pixel (x, y); callers keep it in bounds.
    fn read(&self, x: usize, y: usize) -> u32;
    /// Writes pixel (x, y); callers keep it in bounds.
    fn write(&mut self, x: usize, y: usize, pixel: u32);
}

/// GOP Blt operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Fill a video rectangle with `buffer[0]`.
    VideoFill,
    /// Copy video to the caller's buffer.
    VideoToBuffer,
    /// Copy the caller's buffer to video.
    BufferToVideo,
    /// Copy inside video (overlap allowed).
    VideoToVideo,
}

/// Arguments of one Blt call, as passed by the caller.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    /// Operation.
    pub op: Op,
    /// Source X (video or buffer).
    pub src_x: usize,
    /// Source Y (video or buffer).
    pub src_y: usize,
    /// Destination X (video or buffer).
    pub dst_x: usize,
    /// Destination Y (video or buffer).
    pub dst_y: usize,
    /// Rectangle width.
    pub width: usize,
    /// Rectangle height.
    pub height: usize,
    /// Bytes per buffer row; 0 means `width * 4`.
    pub delta: usize,
}

/// Why a Blt was refused (maps to `EFI_INVALID_PARAMETER`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid;

fn fits(x: usize, y: usize, width: usize, height: usize, max_w: usize, max_h: usize) -> bool {
    x.checked_add(width).is_some_and(|end| end <= max_w)
        && y.checked_add(height).is_some_and(|end| end <= max_h)
}

/// Bottom-right pixel of the rectangle at (x, y), if it does not overflow.
/// Once it exists, every `x + col` and `y + row` inside the rectangle fits.
fn last(x: usize, y: usize, r: &Request) -> Option<(usize, usize)> {
    Some((x.checked_add(r.width - 1)?, y.checked_add(r.height - 1)?))
}

/// Runs a Blt. `buffer` is the caller's pixel buffer, or `None` if it passed
/// a null pointer; it must cover every row the operation touches.
pub fn blt(
    surface: &mut impl Surface,
    buffer: Option<&mut [u32]>,
    r: &Request,
) -> Result<(), Invalid> {
    if r.width == 0 || r.height == 0 {
        return Err(Invalid);
    }
    let (w, h) = (surface.width(), surface.height());
    let stride = if r.delta == 0 { r.width } else { r.delta / 4 };
    if !r.delta.is_multiple_of(4) || stride < r.width {
        return Err(Invalid);
    }
    // Index of buffer pixel (x, y), if it lies inside `len` pixels.
    let at = |x: usize, y: usize, len: usize| -> Option<usize> {
        y.checked_mul(stride)?.checked_add(x).filter(|&i| i < len)
    };

    match r.op {
        Op::VideoFill => {
            let color = *buffer.ok_or(Invalid)?.first().ok_or(Invalid)?;
            if !fits(r.dst_x, r.dst_y, r.width, r.height, w, h) {
                return Err(Invalid);
            }
            for y in r.dst_y..r.dst_y + r.height {
                for x in r.dst_x..r.dst_x + r.width {
                    surface.write(x, y, color);
                }
            }
        }
        Op::VideoToBuffer => {
            let buffer = buffer.ok_or(Invalid)?;
            if !fits(r.src_x, r.src_y, r.width, r.height, w, h) {
                return Err(Invalid);
            }
            let (lx, ly) = last(r.dst_x, r.dst_y, r).ok_or(Invalid)?;
            at(lx, ly, buffer.len()).ok_or(Invalid)?;
            for row in 0..r.height {
                for col in 0..r.width {
                    let i = at(r.dst_x + col, r.dst_y + row, buffer.len()).ok_or(Invalid)?;
                    buffer[i] = surface.read(r.src_x + col, r.src_y + row);
                }
            }
        }
        Op::BufferToVideo => {
            let buffer = buffer.ok_or(Invalid)?;
            if !fits(r.dst_x, r.dst_y, r.width, r.height, w, h) {
                return Err(Invalid);
            }
            let (lx, ly) = last(r.src_x, r.src_y, r).ok_or(Invalid)?;
            at(lx, ly, buffer.len()).ok_or(Invalid)?;
            for row in 0..r.height {
                for col in 0..r.width {
                    let i = at(r.src_x + col, r.src_y + row, buffer.len()).ok_or(Invalid)?;
                    surface.write(r.dst_x + col, r.dst_y + row, buffer[i]);
                }
            }
        }
        Op::VideoToVideo => {
            if !fits(r.src_x, r.src_y, r.width, r.height, w, h)
                || !fits(r.dst_x, r.dst_y, r.width, r.height, w, h)
            {
                return Err(Invalid);
            }
            // Walk rows and columns away from the overlap, like memmove.
            let rows: &mut dyn Iterator<Item = usize> = if r.dst_y > r.src_y {
                &mut (0..r.height).rev()
            } else {
                &mut (0..r.height)
            };
            for row in rows {
                let cols: &mut dyn Iterator<Item = usize> = if r.dst_x > r.src_x {
                    &mut (0..r.width).rev()
                } else {
                    &mut (0..r.width)
                };
                for col in cols {
                    let pixel = surface.read(r.src_x + col, r.src_y + row);
                    surface.write(r.dst_x + col, r.dst_y + row, pixel);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Vram {
        w: usize,
        h: usize,
        px: Vec<u32>,
    }

    impl Surface for Vram {
        fn width(&self) -> usize {
            self.w
        }
        fn height(&self) -> usize {
            self.h
        }
        fn read(&self, x: usize, y: usize) -> u32 {
            self.px[y * self.w + x]
        }
        fn write(&mut self, x: usize, y: usize, pixel: u32) {
            self.px[y * self.w + x] = pixel;
        }
    }

    fn vram() -> Vram {
        Vram {
            w: 8,
            h: 4,
            px: (0..32).collect(),
        }
    }

    fn req(
        op: Op,
        src: (usize, usize),
        dst: (usize, usize),
        size: (usize, usize),
        delta: usize,
    ) -> Request {
        Request {
            op,
            src_x: src.0,
            src_y: src.1,
            dst_x: dst.0,
            dst_y: dst.1,
            width: size.0,
            height: size.1,
            delta,
        }
    }

    #[test]
    fn fill() {
        let mut v = vram();
        blt(
            &mut v,
            Some(&mut [7]),
            &req(Op::VideoFill, (0, 0), (6, 2), (2, 2), 0),
        )
        .unwrap();
        assert_eq!((v.read(6, 2), v.read(7, 3), v.read(5, 2)), (7, 7, 21));
        assert_eq!(
            blt(
                &mut v,
                Some(&mut [7]),
                &req(Op::VideoFill, (0, 0), (7, 0), (2, 1), 0)
            ),
            Err(Invalid)
        );
        assert_eq!(
            blt(&mut v, None, &req(Op::VideoFill, (0, 0), (0, 0), (1, 1), 0)),
            Err(Invalid)
        );
    }

    #[test]
    fn buffer_round_trip_with_delta() {
        let mut v = vram();
        let mut buf = [0u32; 12]; // 3 rows of 4 pixels
        blt(
            &mut v,
            Some(&mut buf),
            &req(Op::VideoToBuffer, (1, 1), (1, 1), (2, 2), 16),
        )
        .unwrap();
        assert_eq!(buf, [0, 0, 0, 0, 0, 9, 10, 0, 0, 17, 18, 0]);
        blt(
            &mut v,
            Some(&mut buf),
            &req(Op::BufferToVideo, (1, 1), (0, 0), (2, 2), 16),
        )
        .unwrap();
        assert_eq!((v.read(0, 0), v.read(1, 1)), (9, 18));
        // Buffer too small for the rectangle.
        assert_eq!(
            blt(
                &mut v,
                Some(&mut buf),
                &req(Op::VideoToBuffer, (0, 0), (2, 2), (3, 1), 16)
            ),
            Err(Invalid)
        );
        assert_eq!(
            blt(
                &mut v,
                Some(&mut buf),
                &req(Op::VideoToBuffer, (0, 0), (0, 0), (2, 1), 6)
            ),
            Err(Invalid)
        );
    }

    #[test]
    fn video_to_video_handles_overlap() {
        let mut v = vram();
        blt(
            &mut v,
            None,
            &req(Op::VideoToVideo, (0, 0), (1, 0), (4, 1), 0),
        )
        .unwrap();
        assert_eq!(&v.px[..6], &[0, 0, 1, 2, 3, 5]);
        let mut v = vram();
        blt(
            &mut v,
            None,
            &req(Op::VideoToVideo, (0, 1), (0, 0), (8, 3), 0),
        )
        .unwrap();
        assert_eq!(&v.px[..8], &[8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(
            blt(
                &mut v,
                None,
                &req(Op::VideoToVideo, (0, 0), (0, 3), (1, 2), 0)
            ),
            Err(Invalid)
        );
    }

    #[test]
    fn huge_values_do_not_overflow() {
        let mut v = vram();
        let r = req(Op::VideoFill, (0, 0), (usize::MAX, 0), (2, 1), 0);
        assert_eq!(blt(&mut v, Some(&mut [1]), &r), Err(Invalid));
        let r = req(Op::BufferToVideo, (usize::MAX - 1, 0), (0, 0), (1, 1), 0);
        assert_eq!(blt(&mut v, Some(&mut [1]), &r), Err(Invalid));
        assert_eq!(
            blt(
                &mut v,
                Some(&mut [1]),
                &req(Op::VideoFill, (0, 0), (0, 0), (0, 1), 0)
            ),
            Err(Invalid)
        );
    }
}
