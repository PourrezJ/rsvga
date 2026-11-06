//! 2D objects of the Screen Object path: GMRs, screens and the blits that
//! move pixels between guest memory (the GMRFB) and screens.
//!
//! Screens are device-owned pixel buffers (host memory). The guest never
//! sees them directly: it pushes pixels with `BLIT_GMRFB_TO_SCREEN` and reads
//! them back with `BLIT_SCREEN_TO_GMRFB`.

use svga_core::command::{Command, GuestPtr, PageSource};
use svga_core::gmr::{GmrError, GmrTable};
use svga_core::screen::{Screens, clip};
use svga_proto::{gmr, remap_gmr2};

use crate::display::Frame;
use crate::guest::GuestMemory;
use crate::shm::SharedMem;

/// Screens the device supports.
pub const MAX_SCREENS: u32 = 16;

/// The image blits read from or write to (`SVGA_CMD_DEFINE_GMRFB`).
#[derive(Clone, Copy, Debug)]
struct GmrFb {
    ptr: GuestPtr,
    bytes_per_line: u32,
}

/// Memory the 2D engine can reach.
pub struct Memory<'a> {
    /// Guest RAM.
    pub guest: &'a GuestMemory,
    /// The VRAM BAR (`SVGA_GMR_FRAMEBUFFER`).
    pub vram: &'a SharedMem,
}

/// GMRs, screens and their pixels.
pub struct TwoD {
    gmrs: GmrTable,
    screens: Screens,
    pixels: Vec<Option<Vec<u32>>>,
    gmrfb: Option<GmrFb>,
}

impl TwoD {
    /// Empty state with the given GMR limits and screen size limits.
    pub fn new(gmr_ids: u32, gmr_pages: u32, max_width: u32, max_height: u32) -> Self {
        Self {
            gmrs: GmrTable::new(gmr_ids, u64::from(gmr_pages)),
            screens: Screens::new(MAX_SCREENS, max_width, max_height),
            pixels: vec![None; MAX_SCREENS as usize],
            gmrfb: None,
        }
    }

    /// Forgets everything (device reset).
    pub fn reset(&mut self) {
        self.gmrs.clear();
        self.clear_screens();
        self.gmrfb = None;
    }

    /// Destroys every screen (the legacy mode registers were written).
    pub fn clear_screens(&mut self) {
        self.screens.clear();
        self.pixels.iter_mut().for_each(|p| *p = None);
    }

    /// Reads `out.len()` bytes at `offset` of GMR `id`.
    fn read(&self, mem: &Memory<'_>, id: u32, offset: u64, out: &mut [u8]) -> Result<(), String> {
        if id == gmr::FRAMEBUFFER {
            let ok = usize::try_from(offset).is_ok_and(|at| mem.vram.read(at, out));
            return if ok {
                Ok(())
            } else {
                Err("framebuffer access out of bounds".into())
            };
        }
        let pieces = self
            .gmrs
            .translate(id, offset, out.len() as u64)
            .map_err(|e| e.to_string())?;
        let mut done = 0usize;
        for (gpa, len) in pieces {
            let len = usize::try_from(len).map_err(|e| e.to_string())?;
            if !mem.guest.read(gpa, &mut out[done..done + len]) {
                return Err(format!("guest memory {gpa:#x} is not mapped"));
            }
            done += len;
        }
        Ok(())
    }

    /// Writes `data` at `offset` of GMR `id`.
    fn write(&self, mem: &Memory<'_>, id: u32, offset: u64, data: &[u8]) -> Result<(), String> {
        if id == gmr::FRAMEBUFFER {
            let ok = usize::try_from(offset).is_ok_and(|at| mem.vram.write(at, data));
            return if ok {
                Ok(())
            } else {
                Err("framebuffer access out of bounds".into())
            };
        }
        let pieces = self
            .gmrs
            .translate(id, offset, data.len() as u64)
            .map_err(|e| e.to_string())?;
        let mut done = 0usize;
        for (gpa, len) in pieces {
            let len = usize::try_from(len).map_err(|e| e.to_string())?;
            if !mem.guest.write(gpa, &data[done..done + len]) {
                return Err(format!("guest memory {gpa:#x} is not mapped"));
            }
            done += len;
        }
        Ok(())
    }

    /// Executes a 2D object command. `Ok(false)`: not one of ours.
    pub fn execute(&mut self, command: &Command, mem: &Memory<'_>) -> Result<bool, String> {
        match command {
            Command::DefineGmr2 { gmr_id, num_pages } => {
                self.gmrs
                    .define(*gmr_id, *num_pages)
                    .map_err(|e| e.to_string())?;
            }
            Command::RemapGmr2 {
                gmr_id,
                flags,
                offset_pages,
                num_pages,
                pages,
            } => {
                let single = flags & remap_gmr2::SINGLE_PPN != 0;
                let list = match pages {
                    PageSource::Inline(list) => list.clone(),
                    PageSource::ViaGmr(ptr) => self.page_list(mem, *ptr, *flags, *num_pages)?,
                };
                self.gmrs
                    .remap(*gmr_id, *offset_pages, *num_pages, &list, single)
                    .map_err(|e: GmrError| e.to_string())?;
            }
            Command::DefineScreen(screen) => {
                self.screens.define(*screen).map_err(|e| format!("{e:?}"))?;
                let size = screen.width as usize * screen.height as usize;
                self.pixels[screen.id as usize] = Some(vec![0; size]);
            }
            Command::DestroyScreen { id } => {
                self.screens.destroy(*id);
                if let Some(slot) = self.pixels.get_mut(*id as usize) {
                    *slot = None;
                }
            }
            Command::DefineGmrFb {
                ptr,
                bytes_per_line,
                bits_per_pixel,
                ..
            } => {
                if *bits_per_pixel != 32 {
                    return Err(format!("GMRFB depth {bits_per_pixel} bpp is not supported"));
                }
                self.gmrfb = Some(GmrFb {
                    ptr: *ptr,
                    bytes_per_line: *bytes_per_line,
                });
            }
            Command::BlitGmrFbToScreen {
                src_origin,
                dst_rect,
                dst_screen,
            } => {
                self.blit(mem, *dst_screen, *src_origin, *dst_rect, true)?;
            }
            Command::BlitScreenToGmrFb {
                dst_origin,
                src_rect,
                src_screen,
            } => {
                self.blit(mem, *src_screen, *dst_origin, *src_rect, false)?;
            }
            // Hints describing the next blit; the blit itself carries the pixels.
            Command::AnnotationFill { .. } | Command::AnnotationCopy { .. } => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Reads a `REMAP_GMR2` page list stored in another GMR.
    fn page_list(
        &self,
        mem: &Memory<'_>,
        ptr: GuestPtr,
        flags: u32,
        count: u32,
    ) -> Result<Vec<u64>, String> {
        let entries = if flags & remap_gmr2::SINGLE_PPN != 0 {
            1
        } else {
            count as usize
        };
        let wide = flags & remap_gmr2::PPN64 != 0;
        let size = if wide { 8 } else { 4 };
        let mut raw = vec![0u8; entries.checked_mul(size).ok_or("page list too large")?];
        self.read(mem, ptr.gmr_id, u64::from(ptr.offset), &mut raw)?;
        Ok(raw
            .chunks_exact(size)
            .map(|c| {
                if wide {
                    u64::from_le_bytes(c.try_into().unwrap_or_default())
                } else {
                    u64::from(u32::from_le_bytes(c.try_into().unwrap_or_default()))
                }
            })
            .collect())
    }

    /// Moves a rectangle between the GMRFB and a screen.
    fn blit(
        &mut self,
        mem: &Memory<'_>,
        screen_id: u32,
        origin: svga_core::command::Point,
        rect: svga_core::command::SignedRect,
        to_screen: bool,
    ) -> Result<(), String> {
        let fb = self.gmrfb.ok_or("blit without DEFINE_GMRFB")?;
        let screen = *self
            .screens
            .get(screen_id)
            .ok_or("blit to an undefined screen")?;
        let Some(b) = clip(origin, rect, screen.width, screen.height) else {
            return Ok(());
        };
        let row_bytes = b.width as usize * 4;
        let mut row = vec![0u8; row_bytes];
        for y in 0..b.height {
            let offset = u64::from(fb.ptr.offset)
                + u64::from(b.image_y + y) * u64::from(fb.bytes_per_line)
                + u64::from(b.image_x) * 4;
            let start = (b.screen_y + y) as usize * screen.width as usize + b.screen_x as usize;
            if to_screen {
                self.read(mem, fb.ptr.gmr_id, offset, &mut row)?;
                let pixels = self.pixels[screen_id as usize]
                    .as_mut()
                    .ok_or("screen without pixels")?;
                for (pixel, bytes) in pixels[start..start + b.width as usize]
                    .iter_mut()
                    .zip(row.chunks_exact(4))
                {
                    *pixel = u32::from_le_bytes(bytes.try_into().unwrap_or_default());
                }
            } else {
                let pixels = self.pixels[screen_id as usize]
                    .as_ref()
                    .ok_or("screen without pixels")?;
                for (bytes, pixel) in row
                    .chunks_exact_mut(4)
                    .zip(&pixels[start..start + b.width as usize])
                {
                    bytes.copy_from_slice(&pixel.to_le_bytes());
                }
                self.write(mem, fb.ptr.gmr_id, offset, &row)?;
            }
        }
        Ok(())
    }

    /// Copies the primary screen into `frame`; false if no screen exists.
    pub fn capture(&self, frame: &mut Frame) -> bool {
        let Some(screen) = self.screens.primary() else {
            return false;
        };
        let Some(pixels) = self.pixels[screen.id as usize].as_ref() else {
            return false;
        };
        frame.width = screen.width as usize;
        frame.height = screen.height as usize;
        frame.pixels.clear();
        frame.pixels.extend(pixels.iter().map(|p| p & 0x00ff_ffff));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;
    use svga_core::command::{Point, ScreenObject, SignedRect};

    fn screen(width: u32, height: u32) -> Command {
        Command::DefineScreen(ScreenObject {
            struct_size: 44,
            id: 0,
            flags: svga_proto::screen_flag::IS_PRIMARY,
            width,
            height,
            root: Point { x: 0, y: 0 },
            backing_store: None,
            clone_count: Some(0),
        })
    }

    #[test]
    fn gmrfb_blits_reach_the_screen_and_back() {
        // Guest RAM: two pages at 0x10000, shared like QEMU does.
        let ram = SharedMem::new("ram", 8192).unwrap();
        let fd: OwnedFd = ram.fd().try_clone_to_owned().unwrap();
        let mut guest = GuestMemory::default();
        guest.add(0x10000, 8192, Some(fd), 0, true).unwrap();
        let vram = SharedMem::new("vram", 4096).unwrap();
        let mem = Memory {
            guest: &guest,
            vram: &vram,
        };
        // A 4x2 image, 16 bytes per line, in GMR 1 backed by pages 0x11, 0x10.
        for (i, pixel) in [1u32, 2, 3, 4, 5, 6, 7, 8].iter().enumerate() {
            ram.write(4096 + i * 4, &pixel.to_le_bytes());
        }
        let mut twod = TwoD::new(8, 64, 1920, 1080);
        let run = |t: &mut TwoD, c: Command| t.execute(&c, &mem).unwrap();
        assert!(run(
            &mut twod,
            Command::DefineGmr2 {
                gmr_id: 1,
                num_pages: 2
            }
        ));
        assert!(run(
            &mut twod,
            Command::RemapGmr2 {
                gmr_id: 1,
                flags: 0,
                offset_pages: 0,
                num_pages: 2,
                pages: PageSource::Inline(vec![0x11, 0x10]),
            }
        ));
        run(&mut twod, screen(8, 4));
        run(
            &mut twod,
            Command::DefineGmrFb {
                ptr: GuestPtr {
                    gmr_id: 1,
                    offset: 0,
                },
                bytes_per_line: 16,
                bits_per_pixel: 32,
                color_depth: 24,
                format_reserved: 0,
            },
        );
        run(
            &mut twod,
            Command::BlitGmrFbToScreen {
                src_origin: Point { x: 1, y: 0 },
                dst_rect: SignedRect {
                    left: 2,
                    top: 1,
                    right: 5,
                    bottom: 3,
                },
                dst_screen: 0,
            },
        );
        let mut frame = Frame::default();
        assert!(twod.capture(&mut frame));
        assert_eq!((frame.width, frame.height), (8, 4));
        assert_eq!(&frame.pixels[8 + 2..8 + 5], &[2, 3, 4]);
        assert_eq!(&frame.pixels[16 + 2..16 + 5], &[6, 7, 8]);

        // Read the screen back into the second half of the image.
        run(
            &mut twod,
            Command::BlitScreenToGmrFb {
                dst_origin: Point { x: 0, y: 1 },
                src_rect: SignedRect {
                    left: 2,
                    top: 1,
                    right: 4,
                    bottom: 2,
                },
                src_screen: 0,
            },
        );
        let mut out = [0u8; 8];
        ram.read(4096 + 16, &mut out);
        assert_eq!(out, [2, 0, 0, 0, 3, 0, 0, 0]);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let ram = SharedMem::new("ram", 4096).unwrap();
        let guest = GuestMemory::default();
        let mem = Memory {
            guest: &guest,
            vram: &ram,
        };
        let mut twod = TwoD::new(8, 64, 1920, 1080);
        let blit = Command::BlitGmrFbToScreen {
            src_origin: Point { x: 0, y: 0 },
            dst_rect: SignedRect {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            dst_screen: 0,
        };
        assert!(twod.execute(&blit, &mem).is_err(), "no GMRFB");
        assert!(
            twod.execute(
                &Command::DefineGmr2 {
                    gmr_id: 99,
                    num_pages: 1
                },
                &mem
            )
            .is_err()
        );
        assert_eq!(twod.execute(&Command::Nop, &mem), Ok(false));
        let mut frame = Frame::default();
        assert!(!twod.capture(&mut frame));
    }
}
