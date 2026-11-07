//! Video processor blits (`VIDEO_BLT`).
//!
//! VMware's Direct3D 11 driver turns `VideoProcessorBlt` into one command
//! per stream ([`VideoBlt`]): fill the output's target rectangle with the
//! background color, then show the stream's source rectangle, scaled, in
//! its destination rectangle (clipped to the target), turned by the
//! rotation.
//!
//! RGB images without rotation are blitted on the GPU with linear
//! filtering. YUV sources (NV12, YV12, YUY2, UYVY, which stay in their
//! device copies) and rotations go through the processor: the source
//! rectangle becomes BGRA in a work image of the GPU, turned as asked,
//! which is then blitted like an RGB source. The driver sends no color
//! space: YUV is taken as limited range, BT.601 below 720 lines and BT.709
//! from there, as players expect. Interlaced frames are shown as they are
//! (no deinterlacing).

use std::sync::Mutex;

use svga_core::gb::{VideoBlt, VideoRect};
use svga_core::layout::{Size3, SurfaceLayout};
use svga_proto::format as f;
use svga_render::{At, Target};

use super::{Gpu, gpu_err};
use crate::gb::Gb;
use crate::guest::GuestMemory;

/// The renderer surface of the work image: one of the IDs the guest
/// cannot define ([`crate::gb::RESERVED_SIDS`]).
pub(super) const SCRATCH: u32 = 0xFFFF_FFF0;

/// A rectangle in pixels: left, top, right, bottom (the last two
/// excluded).
type Area = [i64; 4];

/// Logs `message` the first time only (a player repeats its blits).
fn once(message: String) {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let Ok(mut seen) = SEEN.lock() else { return };
    if seen.len() < 32 && !seen.contains(&message) {
        crate::log!("{message}");
        seen.push(message);
    }
}

fn area(r: VideoRect) -> Area {
    r.map(i64::from)
}

fn intersect(a: Area, b: Area) -> Option<Area> {
    let r = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (r[0] < r[2] && r[1] < r[3]).then_some(r)
}

/// What to draw of `source` once `dest` is clipped to `bounds`: both
/// rectangles, the source one scaled like the destination.
fn clip(source: Area, dest: Area, bounds: Area) -> Option<(Area, Area)> {
    let shown = intersect(dest, bounds)?;
    let (dw, dh) = (dest[2] - dest[0], dest[3] - dest[1]);
    let (sw, sh) = (source[2] - source[0], source[3] - source[1]);
    if dw <= 0 || dh <= 0 || sw <= 0 || sh <= 0 {
        return None;
    }
    let map = |d: i64, d0: i64, dlen: i64, s0: i64, slen: i64| {
        s0 + ((d - d0) as f64 * slen as f64 / dlen as f64).round() as i64
    };
    let src = [
        map(shown[0], dest[0], dw, source[0], sw),
        map(shown[1], dest[1], dh, source[1], sh),
        map(shown[2], dest[0], dw, source[0], sw),
        map(shown[3], dest[1], dh, source[1], sh),
    ];
    (src[0] < src[2] && src[1] < src[3]).then_some((src, shown))
}

fn at(layer: u32, mip: u32, a: Area) -> At {
    let c = |v: i64| u32::try_from(v).unwrap_or(0);
    At {
        layer,
        mip,
        offset: [c(a[0]), c(a[1]), 0],
    }
}

fn extent(a: Area) -> [u32; 3] {
    let c = |v: i64| u32::try_from(v).unwrap_or(0);
    [c(a[2] - a[0]), c(a[3] - a[1]), 1]
}

/// Limited-range YUV to RGB; BT.709 if `hd`, else BT.601.
fn yuv_rgb(y: u8, u: u8, v: u8, hd: bool) -> [u8; 3] {
    let c = 298 * (i32::from(y) - 16);
    let (d, e) = (i32::from(u) - 128, i32::from(v) - 128);
    let (r, g, b) = if hd {
        (c + 459 * e, c - 55 * d - 136 * e, c + 541 * d)
    } else {
        (c + 409 * e, c - 100 * d - 208 * e, c + 516 * d)
    };
    let px = |v: i32| ((v + 128) >> 8).clamp(0, 255) as u8;
    [px(r), px(g), px(b)]
}

/// Full-range YCbCr (the background's) to RGB, BT.601.
fn ycbcr_rgb([y, cb, cr, a]: [f32; 4]) -> [f32; 4] {
    let (cb, cr) = (cb - 0.5, cr - 0.5);
    [
        y + 1.402 * cr,
        y - 0.344_136 * cb - 0.714_136 * cr,
        y + 1.772 * cb,
        a,
    ]
    .map(|c| c.clamp(0.0, 1.0))
}

/// Reads the pixels of a source image as BGRA, from its device copy.
struct Pixels<'a> {
    bytes: &'a [u8],
    format: u32,
    base: usize,
    pitch: usize,
    /// Luma rows of planar images (the chroma follows).
    rows: usize,
    hd: bool,
}

impl Pixels<'_> {
    fn byte(&self, at: usize) -> u8 {
        self.bytes.get(at).copied().unwrap_or(0)
    }

    /// The pixel at (`x`, `y`), as B, G, R, A.
    fn get(&self, x: usize, y: usize) -> [u8; 4] {
        let row = self.base + y * self.pitch;
        let yuv = |y: u8, u: u8, v: u8| {
            let [r, g, b] = yuv_rgb(y, u, v, self.hd);
            [b, g, r, 255]
        };
        match self.format {
            f::NV12 => {
                let chroma =
                    self.base + self.rows * self.pitch + (y / 2) * self.pitch + (x / 2) * 2;
                yuv(self.byte(row + x), self.byte(chroma), self.byte(chroma + 1))
            }
            f::YV12 => {
                let half = self.pitch / 2;
                let v_plane = self.base + self.rows * self.pitch;
                let u_plane = v_plane + half * (self.rows / 2);
                let at = (y / 2) * half + x / 2;
                yuv(
                    self.byte(row + x),
                    self.byte(u_plane + at),
                    self.byte(v_plane + at),
                )
            }
            f::YUY2 => {
                let b = row + (x / 2) * 4;
                yuv(
                    self.byte(b + (x & 1) * 2),
                    self.byte(b + 1),
                    self.byte(b + 3),
                )
            }
            f::UYVY => {
                let b = row + (x / 2) * 4;
                yuv(
                    self.byte(b + 1 + (x & 1) * 2),
                    self.byte(b),
                    self.byte(b + 2),
                )
            }
            f::R8G8B8A8_UNORM | f::R8G8B8A8_UNORM_SRGB => {
                let b = row + x * 4;
                [
                    self.byte(b + 2),
                    self.byte(b + 1),
                    self.byte(b),
                    self.byte(b + 3),
                ]
            }
            // B, G, R, A or X in memory.
            _ => {
                let b = row + x * 4;
                [
                    self.byte(b),
                    self.byte(b + 1),
                    self.byte(b + 2),
                    self.byte(b + 3),
                ]
            }
        }
    }
}

/// Formats the processor path reads.
fn readable(format: u32) -> bool {
    matches!(
        format,
        f::NV12
            | f::YV12
            | f::YUY2
            | f::UYVY
            | f::R8G8B8A8_UNORM
            | f::R8G8B8A8_UNORM_SRGB
            | f::B8G8R8A8_UNORM
            | f::B8G8R8A8_UNORM_SRGB
            | f::B8G8R8X8_UNORM
            | f::B8G8R8X8_UNORM_SRGB
            | f::A8R8G8B8
            | f::X8R8G8B8
    )
}

impl Gpu {
    /// Executes a video processor blit; errors are the guest's.
    pub(super) fn video_blt(
        &mut self,
        v: &VideoBlt,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let (out, input) = (v.output, v.input);
        let (out_layout, _) = gb
            .surface_shape(out.sid)
            .ok_or_else(|| format!("video output {} is not defined", out.sid))?;
        let (in_layout, _) = gb
            .surface_shape(input.sid)
            .ok_or_else(|| format!("video input {} is not defined", input.sid))?;
        if !self.renderer.format_supported(out_layout.format()) {
            once(format!(
                "video blit: output format {} is not supported",
                out_layout.format()
            ));
            return Ok(());
        }
        let size = |l: &SurfaceLayout, mip| {
            let s = l.mip_size(mip);
            [0, 0, i64::from(s.width), i64::from(s.height)]
        };
        let bounds = size(&out_layout, out.mip);
        let Some(target) = intersect(area(v.target), bounds) else {
            return Ok(());
        };
        self.sync_gpu(gb, guest, out.sid)?;
        // The background, then the stream over it.
        let mut color = v.background_color();
        if v.background_ycbcr {
            color = ycbcr_rgb(color);
        }
        let mut targets = [None; 8];
        targets[0] = Some(Target {
            sid: out.sid,
            format: out_layout.format(),
            mip: out.mip,
            first_layer: out.first_slice,
        });
        let [x, y, w, h] = {
            let [l, t, _, _] = target;
            let [ew, eh, _] = extent(target);
            [
                u32::try_from(l).unwrap_or(0),
                u32::try_from(t).unwrap_or(0),
                ew,
                eh,
            ]
        };
        self.renderer
            .clear_rects(&targets, None, Some(color), (None, None), &[(x, y, w, h)])
            .map_err(gpu_err)?;
        self.gpu_wrote(out.sid);
        let Some(source) = intersect(area(v.source), size(&in_layout, input.mip)) else {
            return Ok(());
        };
        let in_format = in_layout.format();
        let on_gpu = v.rotation == 0
            && self.renderer.format_supported(in_format)
            && !in_layout.planar()
            && !matches!(in_format, f::YUY2 | f::UYVY);
        let (src, src_layer, src_mip, source) = if on_gpu {
            self.sync_gpu(gb, guest, input.sid)?;
            (input.sid, input.first_slice, input.mip, source)
        } else {
            let Some(work) = self.video_work(gb, guest, v, &in_layout, source)? else {
                return Ok(());
            };
            (SCRATCH, 0, 0, work)
        };
        let Some((from, to)) = clip(source, area(v.dest), target) else {
            return Ok(());
        };
        self.renderer
            .blit(
                out.sid,
                at(out.first_slice, out.mip, to),
                extent(to),
                src,
                at(src_layer, src_mip, from),
                extent(from),
                true,
            )
            .map_err(gpu_err)?;
        self.gpu_wrote(out.sid);
        Ok(())
    }

    /// The source rectangle of a blit the GPU cannot do itself, as BGRA in
    /// the work image, turned by the rotation; its area there, or `None`
    /// for formats the processor cannot read.
    fn video_work(
        &mut self,
        gb: &mut Gb,
        guest: &GuestMemory,
        v: &VideoBlt,
        layout: &SurfaceLayout,
        source: Area,
    ) -> Result<Option<Area>, String> {
        let input = v.input;
        let format = layout.format();
        if !readable(format) {
            once(format!(
                "video blit: input format {format} is not supported"
            ));
            return Ok(None);
        }
        self.sync_host(gb, guest, input.sid)?;
        let base = layout
            .image_offset(input.first_slice, input.mip)
            .ok_or("video input image outside its surface")?;
        let mip_size = layout.mip_size(input.mip);
        let usize_of = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        let pixels = Pixels {
            bytes: gb.host_copy(guest, input.sid)?,
            format,
            base: usize_of(base),
            pitch: usize_of(layout.row_pitch(input.mip)),
            rows: usize_of(u64::from(mip_size.height.div_ceil(2) * 2)),
            hd: mip_size.height >= 720,
        };
        let [sl, st, sr, sb] = source.map(|v| usize::try_from(v).unwrap_or(0));
        let (sw, sh) = (sr - sl, sb - st);
        let turned = v.rotation & 1 == 1;
        let (w, h) = if turned { (sh, sw) } else { (sw, sh) };
        let mut bytes = Vec::with_capacity(w * h * 4);
        for oy in 0..h {
            for ox in 0..w {
                // Output pixel (ox, oy) of an image turned clockwise.
                let (x, y) = match v.rotation & 3 {
                    1 => (oy, sh - 1 - ox),
                    2 => (sw - 1 - ox, sh - 1 - oy),
                    3 => (sw - 1 - oy, ox),
                    _ => (ox, oy),
                };
                bytes.extend_from_slice(&pixels.get(sl + x, st + y));
            }
        }
        let (w32, h32) = (
            u32::try_from(w).map_err(|_| "video work image too large")?,
            u32::try_from(h).map_err(|_| "video work image too large")?,
        );
        if self.video_scratch != Some((w32, h32)) {
            let layout = SurfaceLayout::new(
                f::B8G8R8A8_UNORM,
                Size3 {
                    width: w32,
                    height: h32,
                    depth: 1,
                },
                1,
                1,
                1,
                u64::MAX,
            )
            .map_err(|e| format!("video work image: {e:?}"))?;
            self.renderer.create(SCRATCH, &layout, 0).map_err(gpu_err)?;
            self.video_scratch = Some((w32, h32));
        }
        self.renderer.upload(SCRATCH, &bytes).map_err(gpu_err)?;
        Ok(Some([0, 0, i64::from(w32), i64::from(h32)]))
    }

    /// Frees the work image (device reset).
    pub(super) fn drop_video_work(&mut self) {
        if self.video_scratch.take().is_some()
            && let Err(e) = self.renderer.destroy(SCRATCH)
        {
            crate::log!("GPU: video work image: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_scales_the_source_like_the_destination() {
        // A 32x32 source shown at 64x64 from (8, 8), cut at 40 by the target.
        let (from, to) = clip([0, 0, 32, 32], [8, 8, 72, 72], [0, 0, 40, 64]).unwrap();
        assert_eq!(to, [8, 8, 40, 64]);
        assert_eq!(from, [0, 0, 16, 28]);
        assert_eq!(clip([0, 0, 32, 32], [80, 0, 90, 10], [0, 0, 64, 64]), None);
    }

    #[test]
    fn yuv_becomes_rgb() {
        assert_eq!(yuv_rgb(16, 128, 128, false), [0, 0, 0]);
        assert_eq!(yuv_rgb(235, 128, 128, true), [255, 255, 255]);
        // BT.601 red.
        let [r, g, b] = yuv_rgb(81, 90, 240, false);
        assert!(r > 250 && g < 5 && b < 5, "{r} {g} {b}");
        assert_eq!(ycbcr_rgb([0.5, 0.5, 0.5, 1.0]), [0.5, 0.5, 0.5, 1.0]);
    }
}
