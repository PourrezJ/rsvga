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
    /// `SVGA3D_SURFACE_*` flags.
    flags: u64,
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

/// Byte order of a 32-bit color format: `Some(true)` for R, G, B, A in
/// memory, `Some(false)` for B, G, R, A; `None` for other formats.
fn rgba_order(format: u32) -> Option<bool> {
    use svga_proto::format as f;
    match format {
        f::R8G8B8A8_TYPELESS
        | f::R8G8B8A8_UNORM
        | f::R8G8B8A8_UNORM_SRGB
        | f::R8G8B8A8_UINT
        | f::R8G8B8A8_SNORM
        | f::R8G8B8A8_SINT => Some(true),
        f::A8R8G8B8
        | f::X8R8G8B8
        | f::B8G8R8A8_TYPELESS
        | f::B8G8R8A8_UNORM
        | f::B8G8R8A8_UNORM_SRGB
        | f::B8G8R8X8_TYPELESS
        | f::B8G8R8X8_UNORM
        | f::B8G8R8X8_UNORM_SRGB => Some(false),
        _ => None,
    }
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

    /// Size in bytes of MOB `id`, if defined.
    pub(crate) fn mob_size(&self, id: u32) -> Option<u32> {
        self.mobs.get(&id).map(|m| m.size)
    }

    /// Reads `out.len()` bytes at `offset` of MOB `id`.
    pub(crate) fn read_mob(
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

    /// Writes `data` at `offset` of MOB `id`.
    pub(crate) fn write_mob(
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
    /// Layout and `SVGA3D_SURFACE_*` flags of surface `sid`.
    pub(crate) fn surface_shape(&self, sid: u32) -> Option<(SurfaceLayout, u64)> {
        self.surfaces.get(&sid).map(|s| (s.layout.clone(), s.flags))
    }

    /// The device copy of surface `sid`, created (from its MOB) if needed.
    pub(crate) fn host_copy(&mut self, guest: &GuestMemory, sid: u32) -> Result<&mut [u8], String> {
        self.ensure_host(guest, sid)?;
        self.surfaces
            .get_mut(&sid)
            .and_then(|s| s.host.as_deref_mut())
            .ok_or_else(|| format!("surface {sid} has no device copy"))
    }

    /// The surface screen target `stid` shows, if any.
    pub(crate) fn target_surface(&self, stid: u32) -> Option<u32> {
        self.targets.get(&stid)?.image.map(|image| image.sid)
    }

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
                flags: def.flags,
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

    /// The image of Direct3D subresource `sub` (`mip + layer * mips`).
    pub(crate) fn subresource(&self, sid: u32, sub: u32) -> Result<ImageId, String> {
        let layout = &self.surface(sid)?.layout;
        let mips = layout.mip_levels().max(1);
        let image = ImageId {
            sid,
            face: sub / mips,
            mipmap: sub % mips,
        };
        if image.face >= layout.layers() {
            return Err(format!("surface {sid} has no subresource {sub}"));
        }
        Ok(image)
    }

    /// `DX_PRESENTBLT`: copies `src_box` of an image to `dest_box` of
    /// another, scaled to nearest pixels when the sizes differ, then shows
    /// the result if a screen target displays it. Only 2D boxes of
    /// uncompressed formats with the same pixel size; red and blue swap
    /// between RGBA and BGRA orders.
    fn present_blt(
        &mut self,
        guest: &GuestMemory,
        src: ImageId,
        src_box: Box3,
        dest: ImageId,
        dest_box: Box3,
    ) -> Result<(), String> {
        let (sl, dl) = (
            &self.surface(src.sid)?.layout,
            &self.surface(dest.sid)?.layout,
        );
        let (sd, dd) = (sl.desc(), dl.desc());
        let plain = |d: svga_proto::format::FormatDesc| {
            (d.block_width, d.block_height, d.block_depth) == (1, 1, 1)
        };
        if !plain(sd) || !plain(dd) || sd.bytes_per_block != dd.bytes_per_block {
            return Err(format!(
                "present between formats {} and {}",
                sl.format(),
                dl.format()
            ));
        }
        let bpp = u64::from(sd.bytes_per_block);
        let swap = matches!(
            (rgba_order(sl.format()), rgba_order(dl.format())),
            (Some(a), Some(b)) if a != b
        );
        // Each box must be a 2D area inside its image.
        let inside = |layout: &SurfaceLayout, image: ImageId, b: Box3| {
            let size = layout.mip_size(image.mipmap);
            b.z == 0
                && b.d <= 1
                && b.x.checked_add(b.w).is_some_and(|e| e <= size.width)
                && b.y.checked_add(b.h).is_some_and(|e| e <= size.height)
        };
        if !inside(sl, src, src_box) || !inside(dl, dest, dest_box) {
            return Err("present box leaves its image".into());
        }
        if [src_box.w, src_box.h, dest_box.w, dest_box.h].contains(&0) {
            return Ok(());
        }
        let src_base = sl
            .image_offset(src.face, src.mipmap)
            .ok_or("no source image")?;
        let dest_base = dl
            .image_offset(dest.face, dest.mipmap)
            .ok_or("no destination image")?;
        let (src_pitch, dest_pitch) = (sl.row_pitch(src.mipmap), dl.row_pitch(dest.mipmap));

        self.ensure_host(guest, src.sid)?;
        self.ensure_host(guest, dest.sid)?;
        let source = if src.sid == dest.sid {
            self.surfaces[&src.sid].host.clone().unwrap_or_default()
        } else {
            self.surfaces
                .get_mut(&src.sid)
                .and_then(|s| s.host.take())
                .unwrap_or_default()
        };
        let mut outcome: Result<(), String> = Ok(());
        if let Some(target) = self
            .surfaces
            .get_mut(&dest.sid)
            .and_then(|s| s.host.as_mut())
        {
            // Nearest source pixel of a destination pixel (centres mapped).
            let pick = |d: u32, dn: u32, s: u32, sn: u32| {
                let pos = (2 * u64::from(d) + 1) * u64::from(sn) / (2 * u64::from(dn));
                u64::from(s) + pos
            };
            let scaled = (src_box.w, src_box.h) != (dest_box.w, dest_box.h);
            'rows: for y in 0..dest_box.h {
                let sy = pick(y, dest_box.h, src_box.y, src_box.h);
                let s_row = src_base + sy * src_pitch + u64::from(src_box.x) * bpp;
                let d_row = dest_base
                    + u64::from(dest_box.y + y) * dest_pitch
                    + u64::from(dest_box.x) * bpp;
                let row_len = u64::from(dest_box.w) * bpp;
                let Some(d) = range(d_row, row_len, target.len()) else {
                    outcome = Err("present row outside the destination".into());
                    break;
                };
                if !scaled {
                    let Some(s) = range(s_row, row_len, source.len()) else {
                        outcome = Err("present row outside the source".into());
                        break;
                    };
                    target[d.clone()].copy_from_slice(&source[s]);
                } else {
                    for x in 0..dest_box.w {
                        let sx = pick(x, dest_box.w, 0, src_box.w);
                        let Some(s) = range(s_row + sx * bpp, bpp, source.len()) else {
                            outcome = Err("present pixel outside the source".into());
                            break 'rows;
                        };
                        let at = d.start + x as usize * bpp as usize;
                        target[at..at + bpp as usize].copy_from_slice(&source[s]);
                    }
                }
                if swap {
                    for px in target[d].chunks_exact_mut(4) {
                        px.swap(0, 2);
                    }
                }
            }
        }
        if src.sid != dest.sid
            && let Some(s) = self.surfaces.get_mut(&src.sid)
        {
            s.host = Some(source);
        }
        outcome?;
        // Show it at once if a screen target displays the destination.
        let shown: Vec<u32> = self
            .targets
            .iter()
            .filter(|(_, t)| t.image == Some(dest))
            .map(|(id, _)| *id)
            .collect();
        for stid in shown {
            let rect = Rect {
                x: dest_box.x,
                y: dest_box.y,
                width: dest_box.w,
                height: dest_box.h,
            };
            self.present(guest, stid, rect)?;
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
            GbCommand::PresentBlt {
                src,
                src_sub,
                dest,
                dest_sub,
                src_box,
                dest_box,
                mode: _,
            } => {
                // Filtering (SVGADX_PRESENTBLT_LINEAR) and colour space
                // flags will come with the GPU renderer.
                let src = self.subresource(src, src_sub)?;
                let dest = self.subresource(dest, dest_sub)?;
                self.present_blt(guest, src, src_box, dest, dest_box)?;
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

    /// Defines `sid` (format, size) with known pixels in its device copy.
    fn filled(gb: &mut Gb, guest: &GuestMemory, sid: u32, fmt: u32, w: u32, h: u32, px: &[u32]) {
        gb.execute(
            GbCommand::DefineSurface(SurfaceDef {
                sid,
                flags: 0,
                format: fmt,
                mip_levels: 1,
                samples: 0,
                size: Size3 {
                    width: w,
                    height: h,
                    depth: 1,
                },
                array_size: 0,
            }),
            guest,
        )
        .unwrap();
        gb.ensure_host(guest, sid).unwrap();
        let host = gb.surfaces.get_mut(&sid).unwrap().host.as_mut().unwrap();
        for (out, p) in host.chunks_exact_mut(4).zip(px) {
            out.copy_from_slice(&p.to_le_bytes());
        }
    }

    fn area2d(x: u32, y: u32, w: u32, h: u32) -> Box3 {
        Box3 {
            x,
            y,
            z: 0,
            w,
            h,
            d: 1,
        }
    }

    #[test]
    fn present_blt_copies_scales_swaps_and_shows() {
        let (_ram, guest) = ram();
        let mut gb = Gb::new(limits());
        // Source: 2x1 RGBA pixels; destination: a 4x2 BGRX screen.
        filled(
            &mut gb,
            &guest,
            1,
            format::R8G8B8A8_UNORM,
            2,
            1,
            &[0x11_22_33, 0x44_55_66],
        );
        filled(&mut gb, &guest, 2, format::B8G8R8X8_UNORM, 4, 2, &[0; 8]);
        for c in [
            GbCommand::DefineScreenTarget {
                stid: 0,
                width: 4,
                height: 2,
                root: (0, 0),
                flags: stflag::PRIMARY,
                dpi: 0,
            },
            GbCommand::BindScreenTarget {
                stid: 0,
                image: image(2),
            },
        ] {
            gb.execute(c, &guest).unwrap();
        }
        let mut frame = Frame::default();
        gb.capture(&mut frame);
        // Scaled 2x in both directions; red and blue swapped.
        let blt = |src_box, dest_box| GbCommand::PresentBlt {
            src: 1,
            src_sub: 0,
            dest: 2,
            dest_sub: 0,
            src_box,
            dest_box,
            mode: 0,
        };
        gb.execute(blt(area2d(0, 0, 2, 1), area2d(0, 0, 4, 2)), &guest)
            .unwrap();
        assert_eq!(gb.capture(&mut frame), Some(true), "shown at once");
        let (a, b) = (0x33_22_11, 0x66_55_44);
        assert_eq!(frame.pixels, [a, a, b, b, a, a, b, b]);
        // Same size: a plain copy of one pixel.
        gb.execute(blt(area2d(1, 0, 1, 1), area2d(3, 1, 1, 1)), &guest)
            .unwrap();
        gb.capture(&mut frame);
        assert_eq!(frame.pixels[7], b);
        // Outside the images, or a missing subresource: refused.
        assert!(
            gb.execute(blt(area2d(1, 0, 2, 1), area2d(0, 0, 2, 1)), &guest)
                .is_err()
        );
        let mut far = blt(area2d(0, 0, 1, 1), area2d(0, 0, 1, 1));
        if let GbCommand::PresentBlt { src_sub, .. } = &mut far {
            *src_sub = 1;
        }
        assert!(gb.execute(far, &guest).is_err());
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
