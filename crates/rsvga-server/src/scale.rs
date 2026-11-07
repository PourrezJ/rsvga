//! Fitting the guest frame into the window without degrading it.
//!
//! - When the frame fits, it is shown at an *integer* scale (1x, 2x, ...),
//!   centred: every guest pixel becomes an exact square, so text stays sharp.
//!   Fractional upscaling (e.g. 1.07x) duplicates some rows and columns but
//!   not others, which makes text look jagged.
//! - When the window is smaller than the frame, the frame is reduced by
//!   averaging the guest pixels behind each window pixel (box filter) rather
//!   than skipping them, so thin strokes do not vanish.

/// Where and how large the frame is drawn in the window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct View {
    /// Left edge of the drawn frame, in window pixels.
    pub x: u32,
    /// Top edge of the drawn frame, in window pixels.
    pub y: u32,
    /// Drawn width.
    pub width: u32,
    /// Drawn height.
    pub height: u32,
    /// Guest frame width.
    pub frame_w: u32,
    /// Guest frame height.
    pub frame_h: u32,
}

impl View {
    /// Layout of a `frame_w` x `frame_h` frame in a `window_w` x `window_h`
    /// window.
    pub fn fit(window_w: u32, window_h: u32, frame_w: u32, frame_h: u32) -> Self {
        if frame_w == 0 || frame_h == 0 || window_w == 0 || window_h == 0 {
            return Self::default();
        }
        let factor = (window_w / frame_w).min(window_h / frame_h);
        let (width, height) = if factor >= 1 {
            (frame_w * factor, frame_h * factor)
        } else {
            // Shrink, keeping the aspect ratio, never to zero.
            let ratio = (f64::from(window_w) / f64::from(frame_w))
                .min(f64::from(window_h) / f64::from(frame_h));
            let w = ((f64::from(frame_w) * ratio) as u32).clamp(1, window_w);
            let h = ((f64::from(frame_h) * ratio) as u32).clamp(1, window_h);
            (w, h)
        };
        Self {
            x: (window_w - width) / 2,
            y: (window_h - height) / 2,
            width,
            height,
            frame_w,
            frame_h,
        }
    }

    /// Guest pixel under window pixel (`wx`, `wy`), clamped to the frame.
    pub fn frame_position(&self, wx: f64, wy: f64) -> Option<(f64, f64)> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let fx = (wx - f64::from(self.x)) * f64::from(self.frame_w) / f64::from(self.width);
        let fy = (wy - f64::from(self.y)) * f64::from(self.frame_h) / f64::from(self.height);
        Some((
            fx.clamp(0.0, f64::from(self.frame_w - 1)),
            fy.clamp(0.0, f64::from(self.frame_h - 1)),
        ))
    }
}

/// Source range `[start, end)` behind destination index `d` when `src`
/// pixels map onto `dst` pixels (`dst <= src`): never empty.
fn span(d: u32, dst: u32, src: u32) -> (usize, usize) {
    let start = u64::from(d) * u64::from(src) / u64::from(dst);
    let end = (u64::from(d) + 1) * u64::from(src) / u64::from(dst);
    (start as usize, end.max(start + 1) as usize)
}

/// Draws `pixels` (`frame_w` x `frame_h`, 0x00RRGGBB) into `out`
/// (`out_w` pixels per row) according to `view`. Pixels outside the view are
/// left untouched.
pub fn render(view: &View, pixels: &[u32], out: &mut [u32], out_w: u32) {
    let (fw, fh) = (view.frame_w as usize, view.frame_h as usize);
    if view.width == 0 || pixels.len() < fw * fh {
        return;
    }
    let row_at = |y: u32| (view.y + y) as usize * out_w as usize + view.x as usize;

    if view.width >= view.frame_w {
        // Integer scale: replicate each guest pixel into a k x k square.
        let k = (view.width / view.frame_w) as usize;
        let mut line = vec![0u32; view.width as usize];
        for (fy, src) in pixels.chunks_exact(fw).take(fh).enumerate() {
            for (fx, &pixel) in src.iter().enumerate() {
                line[fx * k..(fx + 1) * k].fill(pixel);
            }
            for dy in 0..k {
                let start = row_at((fy * k + dy) as u32);
                if let Some(dst) = out.get_mut(start..start + line.len()) {
                    dst.copy_from_slice(&line);
                }
            }
        }
        return;
    }

    // Box filter: average the guest pixels behind each window pixel.
    let columns: Vec<(usize, usize)> = (0..view.width)
        .map(|d| span(d, view.width, view.frame_w))
        .collect();
    for dy in 0..view.height {
        let (y0, y1) = span(dy, view.height, view.frame_h);
        let start = row_at(dy);
        let Some(dst) = out.get_mut(start..start + view.width as usize) else {
            continue;
        };
        for (pixel, &(x0, x1)) in dst.iter_mut().zip(&columns) {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for row in pixels[y0 * fw..y1 * fw].chunks_exact(fw) {
                for &p in &row[x0..x1] {
                    r += (p >> 16) & 0xff;
                    g += (p >> 8) & 0xff;
                    b += p & 0xff;
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            *pixel = ((r / n) << 16) | ((g / n) << 8) | (b / n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_scale_when_the_frame_fits() {
        // 1920x1080 in a slightly larger window: 1:1, centred.
        let v = View::fit(2114, 1157, 1920, 1080);
        assert_eq!((v.x, v.y, v.width, v.height), (97, 38, 1920, 1080));
        // Twice as large: exact 2x.
        let v = View::fit(2600, 1700, 1280, 800);
        assert_eq!((v.width, v.height), (2560, 1600));
    }

    #[test]
    fn shrinking_keeps_the_aspect_ratio() {
        let v = View::fit(960, 1000, 1920, 1080);
        assert_eq!((v.width, v.height), (960, 540));
        assert_eq!((v.x, v.y), (0, 230));
        assert_eq!(View::fit(0, 10, 10, 10), View::default());
    }

    #[test]
    fn replication_is_exact() {
        let view = View::fit(4, 4, 2, 2);
        let mut out = vec![0u32; 16];
        render(&view, &[1, 2, 3, 4], &mut out, 4);
        assert_eq!(out, [1, 1, 2, 2, 1, 1, 2, 2, 3, 3, 4, 4, 3, 3, 4, 4]);
    }

    #[test]
    fn shrinking_averages_instead_of_dropping() {
        // A one-pixel white line on black, halved: it becomes grey, not lost.
        let mut frame = vec![0u32; 4 * 4];
        frame[4..8].fill(0x00ff_ffff);
        let view = View::fit(2, 2, 4, 4);
        let mut out = vec![0u32; 4];
        render(&view, &frame, &mut out, 2);
        assert_eq!(out, [0x007f_7f7f, 0x007f_7f7f, 0, 0]);
    }

    #[test]
    fn pointer_mapping_follows_the_view() {
        let v = View::fit(2114, 1157, 1920, 1080);
        assert_eq!(v.frame_position(97.0, 38.0), Some((0.0, 0.0)));
        assert_eq!(v.frame_position(0.0, 0.0), Some((0.0, 0.0)), "clamped");
        assert_eq!(v.frame_position(5000.0, 5000.0), Some((1919.0, 1079.0)));
        let half = View::fit(960, 540, 1920, 1080);
        assert_eq!(half.frame_position(480.0, 270.0), Some((960.0, 540.0)));
    }
}
