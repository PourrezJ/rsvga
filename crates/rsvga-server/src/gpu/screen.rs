//! What screen targets show, read from the GPU without waiting for it.
//!
//! The display shows pixels that [`Gb`] keeps. When the image a screen
//! target shows was last written by the GPU, showing part of it starts a
//! copy of that part to host memory ([`svga_render::Renderer::read_async`])
//! instead of downloading the whole surface and waiting for the GPU: the
//! pixels reach the screen once the GPU is done, which is checked after
//! each command buffer and before each refresh of the window
//! ([`Gpu::poll_screen`]). Reads land in the order they were made, and a
//! read whose area a later finished one covers is skipped. Showing from
//! the device copy lands the pending reads first, so an older picture
//! never replaces a newer one.

use svga_core::command::Rect;
use svga_render::At;

use super::{Gpu, Newer};
use crate::gb::Gb;
use crate::guest::GuestMemory;

/// A part of a screen target being read from the GPU.
pub(super) struct ScreenRead {
    ticket: u64,
    stid: u32,
    /// `[x0, y0, x1, y1]`.
    area: [u32; 4],
}

/// Whether area `a` contains area `b`.
fn covers(a: [u32; 4], b: [u32; 4]) -> bool {
    a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3]
}

impl Gpu {
    /// Shows `rect` of screen target `stid`: read from the GPU if it holds
    /// the newer image, else from the device copy at once.
    pub(super) fn show(
        &mut self,
        gb: &mut Gb,
        guest: &GuestMemory,
        stid: u32,
        rect: Rect,
    ) -> Result<(), String> {
        let Some((image, area)) = gb.screen_area(stid, rect)? else {
            return Ok(());
        };
        if self.newer.get(&image.sid) == Some(&Newer::Gpu) && self.renderer.copyable(image.sid) {
            let [x0, y0, x1, y1] = area;
            let at = At {
                layer: image.face,
                mip: image.mipmap,
                offset: [x0, y0, 0],
            };
            match self
                .renderer
                .read_async(image.sid, at, [x1 - x0, y1 - y0, 1])
            {
                Ok(ticket) => {
                    crate::prof::count("screen read from the GPU");
                    self.screen_reads.push(ScreenRead { ticket, stid, area });
                    return Ok(());
                }
                Err(e) => crate::log!("GPU: reading screen target {stid}: {e}"),
            }
        }
        crate::prof::count("screen shown from the device copy");
        self.land_screen(gb, true);
        self.sync_host(gb, guest, image.sid)?;
        gb.present(guest, stid, rect)
    }

    /// Shows the screen reads the GPU has finished, without waiting.
    pub fn poll_screen(&mut self, gb: &mut Gb) {
        self.land_screen(gb, false);
    }

    /// Shows the finished screen reads in order; all of them if `wait`.
    pub(super) fn land_screen(&mut self, gb: &mut Gb, wait: bool) {
        if self.screen_reads.is_empty() {
            return;
        }
        if !wait && let Err(e) = self.renderer.poll() {
            crate::log!("GPU: {e}");
        }
        let ready = if wait {
            self.screen_reads.len()
        } else {
            self.screen_reads
                .iter()
                .take_while(|r| self.renderer.read_done(r.ticket))
                .count()
        };
        let reads: Vec<ScreenRead> = self.screen_reads.drain(..ready).collect();
        for (n, r) in reads.iter().enumerate() {
            let covered = reads[n + 1..]
                .iter()
                .any(|later| later.stid == r.stid && covers(later.area, r.area));
            let shown = self.renderer.take_read(r.ticket, wait, |bytes| {
                covered || gb.show_pixels(r.stid, r.area, bytes)
            });
            match shown {
                Ok(Some(_)) => {}
                Ok(None) => crate::log!("GPU: screen read {} lost", r.ticket),
                Err(e) => crate::log!("GPU: screen read: {e}"),
            }
        }
    }

    /// Frees the pending screen reads (device reset).
    pub(super) fn drop_screen_reads(&mut self) {
        for r in std::mem::take(&mut self.screen_reads) {
            if let Err(e) = self.renderer.take_read(r.ticket, true, |_| ()) {
                crate::log!("GPU: screen read: {e}");
            }
        }
    }
}
