//! Turning the guest framebuffer into pixels for a window or a PNG file.

use std::io::{self, Write};

use svga_core::Mode;

use crate::device::Svga;

/// A frame in `0x00RRGGBB` pixels, the format minifb expects.
#[derive(Default)]
pub struct Frame {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height` pixels, row by row.
    pub pixels: Vec<u32>,
}

impl Svga {
    /// Copies the visible framebuffer into `frame`. Returns the mode, or
    /// `None` when SVGA mode is off (the frame is left untouched).
    pub fn capture(&self, frame: &mut Frame) -> Option<Mode> {
        let state = self.lock();
        if state.twod.capture(frame) {
            // Screen objects replace the legacy framebuffer when defined.
            return Some(Mode {
                width: frame.width as u32,
                height: frame.height as u32,
                bits_per_pixel: 32,
                pitch: frame.width as u32 * 4,
                offset: 0,
            });
        }
        let mode = state.regs.mode()?;
        drop(state);
        let (width, height) = (mode.width as usize, mode.height as usize);
        frame.width = width;
        frame.height = height;
        frame.pixels.resize(width * height, 0);
        let vram = self.vram();
        let (offset, pitch) = (mode.offset as usize, mode.pitch as usize);
        for (y, row) in frame.pixels.chunks_exact_mut(width).enumerate() {
            let line = offset + y * pitch;
            match mode.bits_per_pixel {
                32 if line % 4 == 0 => {
                    for (x, pixel) in row.iter_mut().enumerate() {
                        *pixel = vram.load_u32(line / 4 + x) & 0x00ff_ffff;
                    }
                }
                32 => {
                    let mut bytes = [0u8; 4];
                    for (x, pixel) in row.iter_mut().enumerate() {
                        vram.read(line + 4 * x, &mut bytes);
                        *pixel = u32::from_le_bytes(bytes) & 0x00ff_ffff;
                    }
                }
                _ => {
                    let mut bytes = [0u8; 2];
                    for (x, pixel) in row.iter_mut().enumerate() {
                        vram.read(line + 2 * x, &mut bytes);
                        *pixel = rgb565_to_888(u16::from_le_bytes(bytes));
                    }
                }
            }
        }
        Some(mode)
    }
}

fn rgb565_to_888(value: u16) -> u32 {
    let value = u32::from(value);
    let (r, g, b) = ((value >> 11) & 0x1f, (value >> 5) & 0x3f, value & 0x1f);
    let (r, g, b) = (
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    );
    (r << 16) | (g << 8) | b
}

/// Writes `frame` as an uncompressed RGB PNG (stored deflate blocks), so no
/// compression library is needed.
pub fn write_png(out: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let (width, height) = (
        u32::try_from(frame.width).map_err(io::Error::other)?,
        u32::try_from(frame.height).map_err(io::Error::other)?,
    );
    let mut raw = Vec::with_capacity(frame.height * (1 + 3 * frame.width));
    for row in frame.pixels.chunks_exact(frame.width.max(1)) {
        raw.push(0); // filter: none
        for pixel in row {
            let [b, g, r, _] = pixel.to_le_bytes();
            raw.extend_from_slice(&[r, g, b]);
        }
    }

    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        zlib.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        let last = u8::from(blocks.peek().is_none());
        let len = block.len() as u16;
        zlib.push(last);
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB

    out.write_all(b"\x89PNG\r\n\x1a\n")?;
    chunk(out, b"IHDR", &ihdr)?;
    chunk(out, b"IDAT", &zlib)?;
    chunk(out, b"IEND", &[])
}

fn chunk(out: &mut impl Write, kind: &[u8; 4], data: &[u8]) -> io::Result<()> {
    out.write_all(
        &u32::try_from(data.len())
            .map_err(io::Error::other)?
            .to_be_bytes(),
    )?;
    out.write_all(kind)?;
    out.write_all(data)?;
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.write_all(&crc.finish().to_be_bytes())
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xffff_ffff)
    }
    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.0 ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (self.0 & 1).wrapping_neg();
                self.0 = (self.0 >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }
    fn finish(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_reference_values() {
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xcbf4_3926);
    }

    #[test]
    fn png_layout() {
        let frame = Frame {
            width: 2,
            height: 1,
            pixels: vec![0x00ff_0000, 0x0000_00ff],
        };
        let mut out = Vec::new();
        write_png(&mut out, &frame).unwrap();
        assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&out[12..16], b"IHDR");
        assert!(
            out.windows(6).any(|w| w == [0, 0xff, 0, 0, 0, 0]),
            "filter byte then red"
        );
        assert_eq!(&out[out.len() - 8..out.len() - 4], b"IEND");
    }

    #[test]
    fn rgb565() {
        assert_eq!(rgb565_to_888(0xf800), 0xff_0000);
        assert_eq!(rgb565_to_888(0x07e0), 0x00_ff00);
        assert_eq!(rgb565_to_888(0x001f), 0x00_00ff);
    }
}
