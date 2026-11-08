//! Diagnostics: `RSVGA_DUMP_TARGETS=DIR` saves, once the file `DIR/go`
//! exists, every render target drawn to in the next second and a half (a
//! few frames of a program, many of the compositor), as it is then:
//! `DIR/<order>-s<surface>-f<format>.png`, in order of first use. While it
//! records, small draws (full-screen passes) are logged with their
//! targets, textures, pixel shader (its `RSVGA_SHADER_DUMP` file name) and
//! blend state. It shows
//! which pass of a renderer goes wrong (a deferred renderer's geometry
//! buffers, its lighting, its post-processing). Formats without a known
//! conversion are skipped (logged).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use svga_proto::format as f;

use super::Gpu;
use crate::display::{Frame, write_png};
use crate::gb::Gb;

/// What is being recorded.
pub(super) struct Dump {
    dir: PathBuf,
    /// When recording ends.
    after: Instant,
    /// Recording the targets of the current frame.
    recording: bool,
    /// Targets drawn to, in order: surface and view format.
    targets: Vec<(u32, u32)>,
    done: bool,
}

impl Dump {
    /// From `RSVGA_DUMP_TARGETS`, if set.
    pub(super) fn from_env() -> Option<Self> {
        let dir = std::env::var_os("RSVGA_DUMP_TARGETS")?;
        Some(Self {
            dir: PathBuf::from(dir),
            after: Instant::now(),
            recording: false,
            targets: Vec::new(),
            done: false,
        })
    }
}

/// An 8-bit channel from a float.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn unit(x: f32) -> u32 {
    if x.is_nan() {
        0
    } else {
        (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u32
    }
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x3ff);
    match exponent {
        0 => sign * mantissa * 2f32.powi(-24),
        31 => sign * f32::INFINITY,
        e => sign * (1.0 + mantissa / 1024.0) * 2f32.powi(e - 15),
    }
}

/// An unsigned float of `mantissa_bits` and 5 exponent bits (R11G11B10).
fn small_float(bits: u32, mantissa_bits: u32) -> f32 {
    let mantissa = bits & ((1 << mantissa_bits) - 1);
    let exponent = (bits >> mantissa_bits) & 0x1f;
    #[allow(clippy::cast_precision_loss)]
    let m = mantissa as f32 / (1u32 << mantissa_bits) as f32;
    match exponent {
        0 => m * 2f32.powi(-14),
        31 => f32::INFINITY,
        #[allow(clippy::cast_possible_wrap)]
        e => (1.0 + m) * 2f32.powi(e as i32 - 15),
    }
}

/// Magenta: a NaN in a float pixel.
const NAN: u32 = 0x00ff_00ff;

/// One pixel of `format` as 0x00RRGGBB, if the format is known here
/// (floats: magenta if a channel is NaN).
fn pixel(format: u32, p: &[u8]) -> Option<u32> {
    let floats: Option<Vec<f32>> = match format {
        f::R16G16B16A16_FLOAT | f::R16G16_FLOAT | f::R16_FLOAT => Some(
            p.chunks_exact(2)
                .map(|c| half(u16::from_le_bytes([c[0], c[1]])))
                .collect(),
        ),
        f::R32_FLOAT | f::R32G32B32A32_FLOAT => Some(
            p.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        ),
        _ => None,
    };
    if floats.is_some_and(|v| v.iter().any(|x| x.is_nan())) {
        return Some(NAN);
    }
    let rgb = |r: u32, g: u32, b: u32| (r << 16) | (g << 8) | b;
    let word = |i: usize| -> u32 {
        let mut w = [0u8; 4];
        w.copy_from_slice(p.get(i..i + 4).unwrap_or(&[0; 4]));
        u32::from_le_bytes(w)
    };
    let h = |i: usize| {
        half(u16::from_le_bytes([
            p.get(i).copied().unwrap_or(0),
            p.get(i + 1).copied().unwrap_or(0),
        ]))
    };
    let byte = |i: usize| u32::from(p.get(i).copied().unwrap_or(0));
    Some(match format {
        f::B8G8R8A8_UNORM
        | f::B8G8R8X8_UNORM
        | f::B8G8R8A8_UNORM_SRGB
        | f::B8G8R8X8_UNORM_SRGB
        | f::B8G8R8A8_TYPELESS
        | f::B8G8R8X8_TYPELESS
        | f::A8R8G8B8
        | f::X8R8G8B8 => rgb(byte(2), byte(1), byte(0)),
        f::R8G8B8A8_UNORM | f::R8G8B8A8_UNORM_SRGB | f::R8G8B8A8_TYPELESS | f::R8G8B8A8_UINT => {
            rgb(byte(0), byte(1), byte(2))
        }
        f::R10G10B10A2_UNORM | f::R10G10B10A2_TYPELESS | f::R10G10B10A2_UINT => {
            let w = word(0);
            rgb((w >> 2) & 0xff, (w >> 12) & 0xff, (w >> 22) & 0xff)
        }
        f::R16G16B16A16_FLOAT => rgb(unit(h(0)), unit(h(2)), unit(h(4))),
        f::R16G16_FLOAT => rgb(unit(h(0)), unit(h(2)), 0),
        f::R16_FLOAT => {
            let v = unit(h(0));
            rgb(v, v, v)
        }
        f::R11G11B10_FLOAT => {
            let w = word(0);
            rgb(
                unit(small_float(w & 0x7ff, 6)),
                unit(small_float((w >> 11) & 0x7ff, 6)),
                unit(small_float(w >> 22, 5)),
            )
        }
        f::R32_FLOAT | f::R32_TYPELESS => {
            let v = unit(f32::from_bits(word(0)));
            rgb(v, v, v)
        }
        f::R32G32B32A32_FLOAT => rgb(
            unit(f32::from_bits(word(0))),
            unit(f32::from_bits(word(4))),
            unit(f32::from_bits(word(8))),
        ),
        f::R8_UNORM | f::A8_UNORM | f::ALPHA8 => {
            let v = byte(0);
            rgb(v, v, v)
        }
        f::R8G8_UNORM => rgb(byte(0), byte(1), 0),
        _ => return None,
    })
}

/// FNV-1a of shader code: the name `RSVGA_SHADER_DUMP` gives its file.
pub(super) fn code_hash(code: &[u8]) -> u64 {
    code.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

impl Gpu {
    /// Whether draws are being recorded.
    pub(super) fn dump_recording(&self) -> bool {
        self.dump.as_ref().is_some_and(|d| d.recording && !d.done)
    }

    /// Notes a draw to the render targets `targets` (surface, view
    /// format).
    pub(super) fn dump_draw(&mut self, targets: impl Iterator<Item = (u32, u32)>) {
        let Some(d) = self.dump.as_mut() else { return };
        if !d.recording {
            return;
        }
        for t in targets {
            if !d.targets.contains(&t) {
                d.targets.push(t);
            }
        }
    }

    /// A screen update: starts recording once the time has come, or saves
    /// what the frame drew.
    pub(super) fn dump_frame(&mut self, gb: &Gb) {
        let Some(d) = self.dump.as_mut() else { return };
        if d.done || Instant::now() < d.after {
            return;
        }
        if !d.recording {
            if !d.dir.join("go").exists() {
                return;
            }
            d.recording = true;
            d.after = Instant::now() + Duration::from_millis(1500);
            return;
        }
        d.done = true;
        let (dir, targets) = (d.dir.clone(), std::mem::take(&mut d.targets));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            crate::log!("dump {}: {e}", dir.display());
            return;
        }
        for (n, (sid, format)) in targets.into_iter().enumerate() {
            let Some((layout, _)) = gb.surface_shape(sid) else {
                continue;
            };
            let size = layout.mip_size(0);
            let pitch = usize::try_from(layout.row_pitch(0)).unwrap_or(0);
            let (width, height) = (size.width as usize, size.height as usize);
            let bytes = pitch.checked_div(width).unwrap_or(0);
            let mut data = vec![0u8; usize::try_from(layout.total_bytes()).unwrap_or(0)];
            if let Err(e) = self.renderer.download(sid, &mut data) {
                crate::log!("dump: surface {sid}: {e}");
                continue;
            }
            if pixel(format, &[0; 16]).is_none() {
                crate::log!("dump: surface {sid}: format {format} not converted");
                continue;
            }
            let mut pixels = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let at = y * pitch + x * bytes;
                    let p = data.get(at..at + bytes).unwrap_or(&[]);
                    pixels.push(pixel(format, p).unwrap_or(0));
                }
            }
            let path = dir.join(format!("{n:03}-s{sid}-f{format}.png"));
            let frame = Frame {
                width,
                height,
                pixels,
            };
            match std::fs::File::create(&path).and_then(|mut file| write_png(&mut file, &frame)) {
                Ok(()) => crate::log!("dump: {}", path.display()),
                Err(e) => crate::log!("dump {}: {e}", path.display()),
            }
        }
    }
}
