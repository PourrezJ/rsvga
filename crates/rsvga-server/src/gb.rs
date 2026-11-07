//! Guest-backed objects: memory objects (MOBs), object tables, surfaces and
//! screen targets. This is the display path of drivers that use
//! `SVGA_CAP_GBOBJECTS` (the VMware WDDM driver in its modern mode).
//!
//! - A MOB is guest memory described by a page table; its page list is read
//!   once, when it is defined.
//! - A surface has a guest copy (in the MOB it is bound to) and a device
//!   copy (here: a byte buffer with the very same layout, see
//!   [`SurfaceLayout`]). `UPDATE` commands copy guest to device,
//!   `READBACK` commands device to guest; copies between surfaces use the
//!   device copies.
//! - A screen target shows an image of a surface. `UPDATE_GB_SCREENTARGET`
//!   presents an area: it is copied to the pixels the window shows.
//!
//! Object tables are only recorded: rsvga keeps its object state in its
//! own memory, which the guest never reads back.

use std::collections::HashMap;

use svga_core::command::Rect;
use svga_core::gb::{CopyBox, GbCommand, ImageId, SurfaceDef};
use svga_core::layout::{Box3, SurfaceLayout};
use svga_core::mob::{self, PAGE_BYTES, PAGE_SIZE};
use svga_proto::{INVALID_ID, cond_bind, otable, stflag, surface_flag};

use crate::damage::Damage;
use crate::display::Frame;
use crate::guest::GuestMemory;

/// Device limits on guest-backed objects.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Pages all MOBs together may map.
    pub mob_pages: u64,
    /// Largest MOB, in bytes.
    pub mob_size: u32,
    /// Bytes all device surface copies together may use.
    pub surface_bytes: u64,
    /// Largest screen target.
    pub max_width: u32,
    /// Largest screen target.
    pub max_height: u32,
}

/// Screen targets the device supports.
pub const MAX_SCREEN_TARGETS: usize = 16;

struct Mob {
    pages: Vec<u64>,
    size: u32,
}

struct Surface {
    layout: SurfaceLayout,
    mob: Option<u32>,
    /// Row pitch of the base level in the MOB; 0: tightly packed.
    mob_pitch: u64,
    /// The device copy; `None` until first needed.
    host: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct Target {
    width: u32,
    height: u32,
    flags: u32,
    image: Option<ImageId>,
}

/// The pixels of the shown screen target, as last presented.
struct Screen {
    stid: u32,
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    damage: Damage,
}

/// Guest-backed object state.
pub struct Gb {
    limits: Limits,
    otables: [Option<(u64, u32)>; otable::MAX as usize],
    mobs: HashMap<u32, Mob>,
    mob_pages: u64,
    surfaces: HashMap<u32, Surface>,
    host_bytes: u64,
    targets: HashMap<u32, Target>,
    gart: Option<u32>,
    screen: Option<Screen>,
}

/// Merges `(device offset, MOB offset, length)` pieces that are
/// contiguous on both sides, so full-width boxes become one copy.
fn merge(pieces: Vec<(u64, u64, u64)>) -> Vec<(u64, u64, u64)> {
    let mut out: Vec<(u64, u64, u64)> = Vec::with_capacity(pieces.len());
    for (host, mob, len) in pieces {
        match out.last_mut() {
            Some((h, m, l))
                if h.checked_add(*l) == Some(host) && m.checked_add(*l) == Some(mob) =>
            {
                *l = l.saturating_add(len);
            }
            _ => out.push((host, mob, len)),
        }
    }
    out
}

/// Maps byte ranges of a surface's device copy to its MOB. They match when
/// the MOB is tightly packed; with another base-level pitch (only allowed
/// for a single 2D image, as screens are), each row moves.
fn to_mob(
    layout: &SurfaceLayout,
    mob_pitch: u64,
    ranges: Vec<(u64, u64)>,
) -> Result<Vec<(u64, u64, u64)>, String> {
    let packed = layout.row_pitch(0);
    if mob_pitch == 0 || mob_pitch == packed {
        return Ok(merge(ranges.into_iter().map(|(o, l)| (o, o, l)).collect()));
    }
    if layout.mip_levels() != 1 || layout.layers() != 1 || layout.size().depth != 1 {
        return Err(format!(
            "MOB pitch {mob_pitch} on a surface with mips or layers"
        ));
    }
    if mob_pitch < packed || packed == 0 {
        return Err(format!(
            "MOB pitch {mob_pitch} is below the row size {packed}"
        ));
    }
    let mut out = Vec::new();
    for (mut offset, mut len) in ranges {
        while len > 0 {
            let (row, col) = (offset / packed, offset % packed);
            let take = len.min(packed - col);
            let mob = row
                .checked_mul(mob_pitch)
                .and_then(|o| o.checked_add(col))
                .ok_or("MOB offset overflows")?;
            out.push((offset, mob, take));
            offset = offset.saturating_add(take);
            len -= take;
        }
    }
    Ok(merge(out))
}

fn range(offset: u64, len: u64, total: usize) -> Option<std::ops::Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    (end <= total).then_some(start..end)
}

impl Gb {
    /// No objects.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            otables: [None; otable::MAX as usize],
            mobs: HashMap::new(),
            mob_pages: 0,
            surfaces: HashMap::new(),
            host_bytes: 0,
            targets: HashMap::new(),
            gart: None,
            screen: None,
        }
    }

    /// Forgets every object (device reset).
    pub fn reset(&mut self) {
        *self = Self::new(self.limits);
    }

    // ---- MOB memory -------------------------------------------------------

    fn mob_access(&self, id: u32, offset: u64, len: usize) -> Result<Vec<(u64, u64)>, String> {
        let m = self
            .mobs
            .get(&id)
            .ok_or_else(|| format!("MOB {id} is not defined"))?;
        let end = offset
            .checked_add(len as u64)
            .ok_or("MOB access overflows")?;
        if end > u64::from(m.size) {
            return Err(format!(
                "MOB {id} access {offset:#x}+{len:#x} is beyond its {} bytes",
                m.size
            ));
        }
        mob::pieces(&m.pages, offset, len as u64).ok_or_else(|| format!("MOB {id} is too small"))
    }

    fn read_mob(
        &self,
        guest: &GuestMemory,
        id: u32,
        offset: u64,
        out: &mut [u8],
    ) -> Result<(), String> {
        let mut done = 0usize;
        for (gpa, len) in self.mob_access(id, offset, out.len())? {
            let len = usize::try_from(len).map_err(|e| e.to_string())?;
            if !guest.read(gpa, &mut out[done..done + len]) {
                return Err(format!("MOB {id}: guest memory {gpa:#x} is not mapped"));
            }
            done += len;
        }
        Ok(())
    }

    fn write_mob(
        &self,
        guest: &GuestMemory,
        id: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<(), String> {
        let mut done = 0usize;
        for (gpa, len) in self.mob_access(id, offset, data.len())? {
            let len = usize::try_from(len).map_err(|e| e.to_string())?;
            if !guest.write(gpa, &data[done..done + len]) {
                return Err(format!("MOB {id}: guest memory {gpa:#x} is not mapped"));
            }
            done += len;
        }
        Ok(())
    }

    // ---- surfaces ---------------------------------------------------------

    /// Makes sure the device copy of `sid` exists: loaded from its MOB, or
    /// zeroes if it has none (or if the MOB does not cover the surface).
    fn ensure_host(&mut self, guest: &GuestMemory, sid: u32) -> Result<(), String> {
        let s = self
            .surfaces
            .get(&sid)
            .ok_or_else(|| format!("surface {sid} is not defined"))?;
        if s.host.is_some() {
            return Ok(());
        }
        let total = s.layout.total_bytes();
        let budget = self.host_bytes.saturating_add(total);
        if budget > self.limits.surface_bytes {
            return Err(format!(
                "no room for surface {sid} ({total} bytes, {} in use)",
                self.host_bytes
            ));
        }
        let host = vec![0u8; usize::try_from(total).map_err(|e| e.to_string())?];
        let bound = s.mob.is_some();
        self.host_bytes = budget;
        if let Some(s) = self.surfaces.get_mut(&sid) {
            s.host = Some(host);
        }
        if bound {
            let _ = self.copy_mob(guest, sid, vec![(0, total)], true);
        }
        Ok(())
    }

    /// Copies byte ranges between a surface's MOB and its existing device
    /// copy, in either direction.
    fn copy_mob(
        &mut self,
        guest: &GuestMemory,
        sid: u32,
        ranges: Vec<(u64, u64)>,
        to_host: bool,
    ) -> Result<(), String> {
        let surface = self.surface(sid)?;
        let mob = surface
            .mob
            .ok_or_else(|| format!("surface {sid} has no MOB"))?;
        let pieces = to_mob(&surface.layout, surface.mob_pitch, ranges)?;
        // Take the buffer out so the MOB can be read while it is borrowed.
        let mut host = self
            .surfaces
            .get_mut(&sid)
            .and_then(|s| s.host.take())
            .ok_or_else(|| format!("surface {sid} has no device copy"))?;
        let total = host.len();
        let mut result = Ok(());
        for (offset, mob_offset, len) in pieces {
            let Some(r) = range(offset, len, total) else {
                result = Err(format!(
                    "surface {sid}: range {offset:#x}+{len:#x} is outside"
                ));
                break;
            };
            result = if to_host {
                self.read_mob(guest, mob, mob_offset, &mut host[r])
            } else {
                self.write_mob(guest, mob, mob_offset, &host[r])
            };
            if result.is_err() {
                break;
            }
        }
        if let Some(s) = self.surfaces.get_mut(&sid) {
            s.host = Some(host);
        }
        result
    }

    fn drop_surface(&mut self, sid: u32) {
        if let Some(s) = self.surfaces.remove(&sid)
            && let Some(host) = s.host
        {
            self.host_bytes = self.host_bytes.saturating_sub(host.len() as u64);
        }
    }

    fn define_surface(&mut self, def: SurfaceDef) -> Result<(), String> {
        if def.sid == INVALID_ID {
            return Err("surface with the invalid ID".into());
        }
        let layers = if def.array_size > 0 {
            def.array_size
        } else if def.flags & surface_flag::CUBEMAP != 0 {
            6
        } else {
            1
        };
        let layout = SurfaceLayout::new(
            def.format,
            def.size,
            def.mip_levels,
            layers,
            def.samples,
            self.limits.surface_bytes,
        )
        .map_err(|e| format!("surface {}: {e:?}", def.sid))?;
        self.drop_surface(def.sid);
        self.surfaces.insert(
            def.sid,
            Surface {
                layout,
                mob: None,
                mob_pitch: 0,
                host: None,
            },
        );
        Ok(())
    }

    fn surface(&self, sid: u32) -> Result<&Surface, String> {
        self.surfaces
            .get(&sid)
            .ok_or_else(|| format!("surface {sid} is not defined"))
    }

    fn bound_mob(&self, sid: u32) -> Result<u32, String> {
        self.surface(sid)?
            .mob
            .ok_or_else(|| format!("surface {sid} has no MOB"))
    }

    /// Copies byte ranges between a surface's MOB and its device copy,
    /// creating the device copy first if needed.
    fn transfer(
        &mut self,
        guest: &GuestMemory,
        sid: u32,
        ranges: Vec<(u64, u64)>,
        to_host: bool,
    ) -> Result<(), String> {
        self.bound_mob(sid)?;
        self.ensure_host(guest, sid)?;
        self.copy_mob(guest, sid, ranges, to_host)
    }

    fn image_rows(&self, image: ImageId, area: Option<Box3>) -> Result<Vec<(u64, u64)>, String> {
        let layout = &self.surface(image.sid)?.layout;
        let area = area.unwrap_or_else(|| {
            let size = layout.mip_size(image.mipmap);
            Box3 {
                x: 0,
                y: 0,
                z: 0,
                w: size.width,
                h: size.height,
                d: size.depth,
            }
        });
        layout
            .box_rows(image.face, image.mipmap, area)
            .ok_or_else(|| format!("surface {}: no image {image:?} or empty box", image.sid))
    }

    fn whole(&self, sid: u32) -> Result<Vec<(u64, u64)>, String> {
        Ok(vec![(0, self.surface(sid)?.layout.total_bytes())])
    }

    fn surface_copy(
        &mut self,
        guest: &GuestMemory,
        src: ImageId,
        dest: ImageId,
        boxes: &[CopyBox],
    ) -> Result<(), String> {
        let (sd, dd) = (
            self.surface(src.sid)?.layout.desc(),
            self.surface(dest.sid)?.layout.desc(),
        );
        if sd.bytes_per_block != dd.bytes_per_block
            || (sd.block_width, sd.block_height) != (dd.block_width, dd.block_height)
        {
            return Err(format!(
                "copy between incompatible formats ({} and {})",
                self.surface(src.sid)?.layout.format(),
                self.surface(dest.sid)?.layout.format()
            ));
        }
        self.ensure_host(guest, src.sid)?;
        self.ensure_host(guest, dest.sid)?;
        for b in boxes {
            let dest_rows = self.image_rows(dest, Some(b.dest))?;
            let from = Box3 {
                x: b.src.0,
                y: b.src.1,
                z: b.src.2,
                ..b.dest
            };
            let src_rows = self.image_rows(src, Some(from))?;
            if src_rows.len() != dest_rows.len()
                || src_rows.iter().zip(&dest_rows).any(|(s, d)| s.1 != d.1)
            {
                return Err("copy box leaves one of the images".into());
            }
            if src.sid == dest.sid {
                let host = self
                    .surfaces
                    .get_mut(&dest.sid)
                    .and_then(|s| s.host.as_mut())
                    .ok_or("surface vanished")?;
                for ((so, len), (doff, _)) in src_rows.into_iter().zip(dest_rows) {
                    let s = range(so, len, host.len()).ok_or("source row outside")?;
                    let d = range(doff, len, host.len()).ok_or("destination row outside")?;
                    host.copy_within(s, d.start);
                }
            } else {
                let source = self
                    .surfaces
                    .get_mut(&src.sid)
                    .and_then(|s| s.host.take())
                    .unwrap_or_default();
                let mut outcome = Ok(());
                if let Some(target) = self
                    .surfaces
                    .get_mut(&dest.sid)
                    .and_then(|s| s.host.as_mut())
                {
                    for ((so, len), (doff, _)) in src_rows.into_iter().zip(dest_rows) {
                        match (range(so, len, source.len()), range(doff, len, target.len())) {
                            (Some(s), Some(d)) => target[d].copy_from_slice(&source[s]),
                            _ => {
                                outcome = Err("copy row outside a surface".to_string());
                                break;
                            }
                        }
                    }
                }
                if let Some(s) = self.surfaces.get_mut(&src.sid) {
                    s.host = Some(source);
                }
                outcome?;
            }
        }
        Ok(())
    }

    // ---- screen targets ---------------------------------------------------

    /// Picks the screen target to show: the primary one, else the lowest ID.
    fn choose_screen(&mut self) {
        let chosen = self
            .targets
            .iter()
            .filter(|(_, t)| t.flags & stflag::PRIMARY != 0)
            .map(|(id, _)| *id)
            .min()
            .or_else(|| self.targets.keys().copied().min());
        match chosen {
            None => self.screen = None,
            Some(stid) => {
                let t = self.targets[&stid];
                let same = self
                    .screen
                    .as_ref()
                    .is_some_and(|s| (s.stid, s.width, s.height) == (stid, t.width, t.height));
                if !same {
                    let mut damage = Damage::default();
                    damage.all();
                    self.screen = Some(Screen {
                        stid,
                        width: t.width,
                        height: t.height,
                        pixels: vec![0; t.width as usize * t.height as usize],
                        damage,
                    });
                }
            }
        }
    }

    /// Copies `rect` of a screen target's image to the shown pixels.
    fn present(&mut self, guest: &GuestMemory, stid: u32, rect: Rect) -> Result<(), String> {
        if self.screen.as_ref().is_none_or(|s| s.stid != stid) {
            return Ok(()); // not the screen rsvga shows
        }
        let target = *self
            .targets
            .get(&stid)
            .ok_or_else(|| format!("screen target {stid} is not defined"))?;
        let Some(image) = target.image else {
            return Ok(());
        };
        self.ensure_host(guest, image.sid)?;
        // Field borrows (not `self.surface()`): the screen is mutated below.
        let surface = self
            .surfaces
            .get(&image.sid)
            .ok_or_else(|| format!("surface {} is not defined", image.sid))?;
        let layout = &surface.layout;
        let desc = layout.desc();
        if desc.bytes_per_block != 4 || desc.block_width != 1 || desc.block_height != 1 {
            return Err(format!(
                "screen target {stid} shows format {}, not a 32-bit colour format",
                layout.format()
            ));
        }
        let size = layout.mip_size(image.mipmap);
        let (w, h) = (target.width.min(size.width), target.height.min(size.height));
        let x0 = rect.x.min(w);
        let y0 = rect.y.min(h);
        let x1 = rect.x.saturating_add(rect.width).min(w);
        let y1 = rect.y.saturating_add(rect.height).min(h);
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        let base = layout
            .image_offset(image.face, image.mipmap)
            .ok_or("no such image")?;
        let pitch = layout.row_pitch(image.mipmap);
        let host = surface.host.as_deref().unwrap_or_default();
        let screen = self.screen.as_mut().ok_or("no screen")?;
        for y in y0..y1 {
            let start = base + u64::from(y) * pitch + u64::from(x0) * 4;
            let Some(src) = range(start, u64::from(x1 - x0) * 4, host.len()) else {
                return Err("screen target image is outside its surface".into());
            };
            let line = y as usize * screen.width as usize;
            for (out, px) in screen.pixels[line + x0 as usize..line + x1 as usize]
                .iter_mut()
                .zip(host[src].chunks_exact(4))
            {
                *out = u32::from_le_bytes([px[0], px[1], px[2], px[3]]) & 0x00ff_ffff;
            }
        }
        screen.damage.add(x0, y0, x1 - x0, y1 - y0);
        Ok(())
    }

    /// Brings `frame` up to date with the shown screen target, copying only
    /// what changed. `None` if no screen target exists, else whether
    /// `frame` changed.
    pub fn capture(&mut self, frame: &mut Frame) -> Option<bool> {
        let screen = self.screen.as_mut()?;
        let (width, height) = (screen.width as usize, screen.height as usize);
        if frame.width != width || frame.height != height {
            frame.width = width;
            frame.height = height;
            frame.pixels.resize(width * height, 0);
            screen.damage.all();
        }
        let Some((x0, y0, x1, y1)) = screen.damage.take(screen.width, screen.height) else {
            return Some(false);
        };
        let (x0, x1) = (x0 as usize, x1 as usize);
        for y in y0 as usize..y1 as usize {
            let line = y * width;
            frame.pixels[line + x0..line + x1]
                .copy_from_slice(&screen.pixels[line + x0..line + x1]);
        }
        Some(true)
    }

    // ---- commands ---------------------------------------------------------

    /// Executes a command. Errors are the guest's: the command is skipped.
    pub fn execute(&mut self, command: GbCommand, guest: &GuestMemory) -> Result<(), String> {
        match command {
            GbCommand::Nop | GbCommand::ReadbackOTable { .. } => {}
            GbCommand::SetOTable {
                table, base, size, ..
            } => {
                let slot = self
                    .otables
                    .get_mut(table as usize)
                    .ok_or_else(|| format!("unknown object table {table}"))?;
                *slot = Some((base, size));
            }
            GbCommand::DefineMob {
                id,
                pt_depth,
                base,
                size,
            } => {
                if id == INVALID_ID {
                    return Err("MOB with the invalid ID".into());
                }
                if size > self.limits.mob_size {
                    return Err(format!("MOB {id} of {size} bytes is too large"));
                }
                let old = self.mobs.remove(&id).map_or(0, |m| m.pages.len() as u64);
                self.mob_pages = self.mob_pages.saturating_sub(old);
                let room = self.limits.mob_pages.saturating_sub(self.mob_pages);
                let mut reader = |ppn: u64, out: &mut [u8; PAGE_BYTES]| {
                    ppn.checked_mul(PAGE_SIZE)
                        .is_some_and(|gpa| guest.read(gpa, out))
                };
                let pages = mob::pages(pt_depth, base, size, room, &mut reader)
                    .map_err(|e| format!("MOB {id}: {e}"))?;
                self.mob_pages = self.mob_pages.saturating_add(pages.len() as u64);
                self.mobs.insert(id, Mob { pages, size });
            }
            GbCommand::DestroyMob { id } => {
                if let Some(m) = self.mobs.remove(&id) {
                    self.mob_pages = self.mob_pages.saturating_sub(m.pages.len() as u64);
                }
                for s in self.surfaces.values_mut() {
                    if s.mob == Some(id) {
                        s.mob = None;
                    }
                }
                if self.gart == Some(id) {
                    self.gart = None;
                }
            }
            GbCommand::DefineSurface(def) => self.define_surface(def)?,
            GbCommand::DestroySurface { sid } => self.drop_surface(sid),
            GbCommand::BindSurface { sid, mob, pitch } => {
                let s = self
                    .surfaces
                    .get_mut(&sid)
                    .ok_or_else(|| format!("surface {sid} is not defined"))?;
                s.mob = (mob != INVALID_ID).then_some(mob);
                s.mob_pitch = u64::from(pitch);
            }
            GbCommand::CondBindSurface {
                sid,
                test_mob,
                mob,
                flags,
            } => {
                let current = self.surface(sid)?.mob.unwrap_or(INVALID_ID);
                if current != test_mob {
                    return Ok(());
                }
                if flags & cond_bind::READBACK != 0 && current != INVALID_ID {
                    let all = self.whole(sid)?;
                    self.transfer(guest, sid, all, false)?;
                }
                if let Some(s) = self.surfaces.get_mut(&sid) {
                    s.mob = (mob != INVALID_ID).then_some(mob);
                    s.mob_pitch = 0;
                }
                if flags & cond_bind::UPDATE != 0 && mob != INVALID_ID {
                    let all = self.whole(sid)?;
                    self.transfer(guest, sid, all, true)?;
                }
            }
            GbCommand::UpdateImage { image, area } => {
                let rows = self.image_rows(image, Some(area))?;
                self.transfer(guest, image.sid, rows, true)?;
            }
            GbCommand::UpdateSurface { sid } => {
                let all = self.whole(sid)?;
                self.transfer(guest, sid, all, true)?;
            }
            GbCommand::ReadbackImage { image } => {
                let rows = self.image_rows(image, None)?;
                self.transfer(guest, image.sid, rows, false)?;
            }
            GbCommand::ReadbackImagePartial {
                image,
                area,
                invert,
            } => {
                // "Everything but the box" is read back as the whole image:
                // the box then gets the same bytes it already has.
                let rows = self.image_rows(image, (!invert).then_some(area))?;
                self.transfer(guest, image.sid, rows, false)?;
            }
            GbCommand::ReadbackSurface { sid } => {
                let all = self.whole(sid)?;
                self.transfer(guest, sid, all, false)?;
            }
            GbCommand::InvalidateImage { .. } | GbCommand::InvalidateSurface { .. } => {}
            GbCommand::UpdateZeroSurface { sid } => {
                self.ensure_host(guest, sid)?;
                if let Some(host) = self.surfaces.get_mut(&sid).and_then(|s| s.host.as_mut()) {
                    host.fill(0);
                }
            }
            GbCommand::SurfaceCopy { src, dest, boxes } => {
                self.surface_copy(guest, src, dest, &boxes)?;
            }
            GbCommand::EnableGart { mob } => self.gart = Some(mob),
            GbCommand::DisableGart => self.gart = None,
            GbCommand::MapMobIntoGart { .. } | GbCommand::UnmapGartRange { .. } => {}
            GbCommand::DefineScreenTarget {
                stid,
                width,
                height,
                flags,
                ..
            } => {
                if !(1..=self.limits.max_width).contains(&width)
                    || !(1..=self.limits.max_height).contains(&height)
                {
                    return Err(format!("screen target {stid} of {width}x{height}"));
                }
                if !self.targets.contains_key(&stid) && self.targets.len() >= MAX_SCREEN_TARGETS {
                    return Err("too many screen targets".into());
                }
                self.targets.insert(
                    stid,
                    Target {
                        width,
                        height,
                        flags,
                        image: None,
                    },
                );
                self.choose_screen();
            }
            GbCommand::DestroyScreenTarget { stid } => {
                self.targets.remove(&stid);
                self.choose_screen();
            }
            GbCommand::BindScreenTarget { stid, image } => {
                let t = self
                    .targets
                    .get_mut(&stid)
                    .ok_or_else(|| format!("screen target {stid} is not defined"))?;
                t.image = (image.sid != INVALID_ID).then_some(image);
                let (width, height) = (t.width, t.height);
                // Binding shows the new image at once.
                let all = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                self.present(guest, stid, all)?;
            }
            GbCommand::UpdateScreenTarget { stid, rect } => self.present(guest, stid, rect)?,
            GbCommand::MobFence { value, mob, offset } => {
                self.write_mob(guest, mob, u64::from(offset), &value.to_le_bytes())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::SharedMem;
    use std::os::fd::OwnedFd;
    use svga_core::layout::Size3;
    use svga_proto::{format, mobfmt};

    const RAM_BASE: u64 = 0x10_0000;

    fn limits() -> Limits {
        Limits {
            mob_pages: 1024,
            mob_size: 1 << 20,
            surface_bytes: 64 << 20,
            max_width: 4096,
            max_height: 4096,
        }
    }

    /// 64 KiB of guest RAM at `RAM_BASE`.
    fn ram() -> (SharedMem, GuestMemory) {
        let ram = SharedMem::new("ram", 0x10000).unwrap();
        let fd: OwnedFd = ram.fd().try_clone_to_owned().unwrap();
        let mut guest = GuestMemory::default();
        guest.add(RAM_BASE, 0x10000, Some(fd), 0, true).unwrap();
        (ram, guest)
    }

    fn surface(sid: u32, width: u32, height: u32) -> GbCommand {
        GbCommand::DefineSurface(SurfaceDef {
            sid,
            flags: 0,
            format: format::B8G8R8X8_UNORM,
            mip_levels: 1,
            samples: 0,
            size: Size3 {
                width,
                height,
                depth: 1,
            },
            array_size: 0,
        })
    }

    fn image(sid: u32) -> ImageId {
        ImageId {
            sid,
            face: 0,
            mipmap: 0,
        }
    }

    #[test]
    fn a_desktop_through_a_mob_a_copy_and_a_screen_target() {
        let (ram, guest) = ram();
        let mut gb = Gb::new(limits());
        let run = |gb: &mut Gb, c: GbCommand| gb.execute(c, &guest).unwrap();
        // MOB 1: two pages through a level-1 table at page 0x100 + 0.
        let table_gpa = 0;
        let pages = [(RAM_BASE >> 12) + 2, (RAM_BASE >> 12) + 1];
        for (i, p) in pages.iter().enumerate() {
            ram.write(table_gpa + i * 8, &p.to_le_bytes());
        }
        run(
            &mut gb,
            GbCommand::DefineMob {
                id: 1,
                pt_depth: mobfmt::PT64_1,
                base: RAM_BASE >> 12,
                size: 8192,
            },
        );
        // A 4x2 image in the MOB: pixel n is 0x00n0n0n0-ish.
        let pixels: Vec<u32> = (0..8).map(|n| 0x0010_1010 * (n + 1)).collect();
        for (i, p) in pixels.iter().enumerate() {
            ram.write(2 * 4096 + i * 4, &p.to_le_bytes()); // MOB page 0 = RAM page 2
        }
        run(&mut gb, surface(10, 4, 2));
        run(
            &mut gb,
            GbCommand::BindSurface {
                sid: 10,
                mob: 1,
                pitch: 0,
            },
        );
        run(
            &mut gb,
            GbCommand::UpdateImage {
                image: image(10),
                area: Box3 {
                    x: 0,
                    y: 0,
                    z: 0,
                    w: 4,
                    h: 2,
                    d: 1,
                },
            },
        );
        // Copy it into a primary surface shown by screen target 0.
        run(&mut gb, surface(20, 4, 2));
        run(&mut gb, GbCommand::UpdateZeroSurface { sid: 20 });
        run(
            &mut gb,
            GbCommand::DefineScreenTarget {
                stid: 0,
                width: 4,
                height: 2,
                root: (0, 0),
                flags: stflag::PRIMARY,
                dpi: 0,
            },
        );
        run(
            &mut gb,
            GbCommand::BindScreenTarget {
                stid: 0,
                image: image(20),
            },
        );
        let mut frame = Frame::default();
        assert_eq!(gb.capture(&mut frame), Some(true));
        assert_eq!(frame.pixels, [0; 8], "bound to a zeroed surface");
        run(
            &mut gb,
            GbCommand::SurfaceCopy {
                src: image(10),
                dest: image(20),
                boxes: vec![CopyBox {
                    dest: Box3 {
                        x: 1,
                        y: 0,
                        z: 0,
                        w: 2,
                        h: 2,
                        d: 1,
                    },
                    src: (0, 0, 0),
                }],
            },
        );
        assert_eq!(gb.capture(&mut frame), Some(false), "not presented yet");
        run(
            &mut gb,
            GbCommand::UpdateScreenTarget {
                stid: 0,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 2,
                },
            },
        );
        assert_eq!(gb.capture(&mut frame), Some(true));
        assert_eq!(
            frame.pixels,
            [0, pixels[0], pixels[1], 0, 0, pixels[4], pixels[5], 0]
        );

        // Fences land in the MOB; readbacks write the device copy back.
        run(
            &mut gb,
            GbCommand::MobFence {
                value: 0xfe,
                mob: 1,
                offset: 4096 + 8,
            },
        );
        let mut word = [0u8; 4];
        ram.read(4096 + 8, &mut word); // MOB page 1 = RAM page 1
        assert_eq!(u32::from_le_bytes(word), 0xfe);
        run(
            &mut gb,
            GbCommand::BindSurface {
                sid: 20,
                mob: 1,
                pitch: 0,
            },
        );
        run(&mut gb, GbCommand::ReadbackSurface { sid: 20 });
        ram.read(2 * 4096 + 4, &mut word);
        assert_eq!(
            u32::from_le_bytes(word),
            pixels[0],
            "copied pixel read back"
        );
    }

    #[test]
    fn mob_pitch_moves_rows() {
        let layout = SurfaceLayout::new(
            format::B8G8R8X8_UNORM,
            Size3 {
                width: 4,
                height: 2,
                depth: 1,
            },
            1,
            1,
            1,
            1 << 20,
        )
        .unwrap();
        // Packed rows are 16 bytes; in the MOB they are 32 bytes apart.
        assert_eq!(
            to_mob(&layout, 32, vec![(0, 32)]),
            Ok(vec![(0, 0, 16), (16, 32, 16)])
        );
        assert_eq!(to_mob(&layout, 32, vec![(20, 8)]), Ok(vec![(20, 36, 8)]));
        assert_eq!(
            to_mob(&layout, 16, vec![(0, 16), (16, 16)]),
            Ok(vec![(0, 0, 32)]),
            "packed: one merged copy"
        );
        assert!(
            to_mob(&layout, 8, vec![(0, 16)]).is_err(),
            "pitch below a row"
        );
    }

    #[test]
    fn hostile_commands_fail_without_harm() {
        let (_ram, guest) = ram();
        let mut gb = Gb::new(limits());
        let fails = |gb: &mut Gb, c: GbCommand| assert!(gb.execute(c, &guest).is_err());
        // A page table outside guest RAM, a MOB beyond the limits.
        fails(
            &mut gb,
            GbCommand::DefineMob {
                id: 1,
                pt_depth: mobfmt::PT64_1,
                base: 0x9999,
                size: 4096,
            },
        );
        fails(
            &mut gb,
            GbCommand::DefineMob {
                id: 1,
                pt_depth: mobfmt::RANGE,
                base: RAM_BASE >> 12,
                size: u32::MAX,
            },
        );
        // Surfaces: unknown, too large, without MOB.
        fails(&mut gb, GbCommand::UpdateSurface { sid: 5 });
        fails(&mut gb, surface(5, 1 << 16, 1 << 16));
        gb.execute(surface(5, 4, 4), &guest).unwrap();
        fails(&mut gb, GbCommand::UpdateSurface { sid: 5 });
        fails(
            &mut gb,
            GbCommand::MobFence {
                value: 1,
                mob: 7,
                offset: 0,
            },
        );
        fails(
            &mut gb,
            GbCommand::DefineScreenTarget {
                stid: 0,
                width: 0,
                height: 10,
                root: (0, 0),
                flags: 0,
                dpi: 0,
            },
        );
        let mut frame = Frame::default();
        assert_eq!(gb.capture(&mut frame), None);
    }
}
