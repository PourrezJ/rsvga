//! The GPU side of surfaces, kept coherent with their device copies.
//!
//! A surface can live in three places: the guest's MOB, the device copy in
//! host memory ([`crate::gb`]) and an image or buffer on the GPU
//! ([`svga_render`]). MOB and device copy are synchronized by the guest's
//! `UPDATE` and `READBACK` commands. Between device copy and GPU, this
//! module remembers which side was written last and copies before the
//! other side is used:
//!
//! - the GPU gets a surface when a GPU operation first needs it, filled
//!   from the device copy (itself loaded from the MOB if needed);
//! - a GPU write makes the GPU copy the newer one; anything that reads or
//!   writes the device copy (readbacks, screen presentation, CPU copies,
//!   uploads from the MOB) downloads it first;
//! - a write to the device copy makes it the newer one; the next GPU use
//!   uploads it.
//!
//! On the GPU now: clearing render target views and copies between
//! surfaces. Draws need the shader translator and stay deferred.

use std::collections::HashMap;

use svga_core::cotable::Object;
use svga_core::dx::{DxCommand, RtvRange};
use svga_core::gb::GbCommand;
use svga_proto::{cond_bind, dx};
use svga_render::{At, Renderer};

use crate::dxctx::Context;
use crate::gb::Gb;
use crate::guest::GuestMemory;

/// Which copy of a surface was written last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Newer {
    /// Both hold the same contents.
    Same,
    /// The device copy.
    Host,
    /// The GPU copy.
    Gpu,
}

/// The renderer and the coherence of its surfaces.
pub struct Gpu {
    renderer: Renderer,
    newer: HashMap<u32, Newer>,
}

/// What a guest-backed command does to device copies.
#[derive(Default)]
struct Effects {
    /// Surfaces whose device copy is read or partly written: the GPU copy
    /// must come back first.
    host_uses: Vec<u32>,
    /// Surfaces whose device copy is written.
    host_writes: Vec<u32>,
    /// Surfaces redefined or destroyed.
    gone: Vec<u32>,
}

fn gb_effects(command: &GbCommand, gb: &Gb) -> Effects {
    let mut e = Effects::default();
    match command {
        GbCommand::DefineSurface(def) => e.gone.push(def.sid),
        GbCommand::DestroySurface { sid } => e.gone.push(*sid),
        GbCommand::CondBindSurface { sid, flags, .. } => {
            if flags & cond_bind::READBACK != 0 {
                e.host_uses.push(*sid);
            }
            if flags & cond_bind::UPDATE != 0 {
                e.host_uses.push(*sid);
                e.host_writes.push(*sid);
            }
        }
        GbCommand::UpdateImage { image, .. } => {
            e.host_uses.push(image.sid);
            e.host_writes.push(image.sid);
        }
        GbCommand::UpdateSurface { sid } | GbCommand::UpdateZeroSurface { sid } => {
            e.host_uses.push(*sid);
            e.host_writes.push(*sid);
        }
        GbCommand::ReadbackImage { image } | GbCommand::ReadbackImagePartial { image, .. } => {
            e.host_uses.push(image.sid);
        }
        GbCommand::ReadbackSurface { sid } => e.host_uses.push(*sid),
        GbCommand::SurfaceCopy { src, dest, .. } => {
            e.host_uses.extend([src.sid, dest.sid]);
            e.host_writes.push(dest.sid);
        }
        GbCommand::PresentBlt { src, dest, .. } => {
            e.host_uses.extend([*src, *dest]);
            e.host_writes.push(*dest);
        }
        GbCommand::BindScreenTarget { image, .. } => e.host_uses.push(image.sid),
        GbCommand::UpdateScreenTarget { stid, .. } => e.host_uses.extend(gb.target_surface(*stid)),
        _ => {}
    }
    e
}

/// What a DX command executed on the device copies does to them.
fn dx_host_effects(command: &DxCommand) -> Effects {
    let mut e = Effects::default();
    match command {
        DxCommand::BufferUpdate { sid, .. } | DxCommand::UpdateSubResource { sid, .. } => {
            e.host_uses.push(*sid);
            e.host_writes.push(*sid);
        }
        DxCommand::ReadbackSubResource { sid, .. } => e.host_uses.push(*sid),
        _ => {}
    }
    e
}

/// Whether the GPU executes `command` (with [`Gpu::execute_dx`]).
pub fn runs_on_gpu(command: &DxCommand) -> bool {
    matches!(
        command,
        DxCommand::ClearRenderTargetView { .. }
            | DxCommand::PredCopy { .. }
            | DxCommand::PredCopyRegion { .. }
            | DxCommand::BufferCopy { .. }
            | DxCommand::TransferFromBuffer { .. }
            | DxCommand::SurfaceCopyAndReadback { .. }
    )
}

impl Gpu {
    /// Opens the host GPU.
    pub fn new() -> Result<Self, String> {
        let renderer = Renderer::new().map_err(|e| e.to_string())?;
        Ok(Self {
            renderer,
            newer: HashMap::new(),
        })
    }

    /// The GPU's name.
    pub fn name(&self) -> &str {
        self.renderer.name()
    }

    /// Forgets every surface (device reset).
    pub fn reset(&mut self) {
        for sid in self.newer.keys().copied().collect::<Vec<_>>() {
            self.drop_surface(sid);
        }
    }

    fn drop_surface(&mut self, sid: u32) {
        self.newer.remove(&sid);
        if let Err(e) = self.renderer.destroy(sid) {
            crate::log!("GPU: freeing surface {sid}: {e}");
        }
    }

    /// Brings the device copy of `sid` up to date.
    fn sync_host(&mut self, gb: &mut Gb, guest: &GuestMemory, sid: u32) -> Result<(), String> {
        if self.newer.get(&sid) != Some(&Newer::Gpu) {
            return Ok(());
        }
        let host = gb.host_copy(guest, sid)?;
        self.renderer
            .download(sid, host)
            .map_err(|e| format!("GPU: reading surface {sid}: {e}"))?;
        self.newer.insert(sid, Newer::Same);
        Ok(())
    }

    /// Brings the GPU copy of `sid` up to date, creating it if needed.
    fn sync_gpu(&mut self, gb: &mut Gb, guest: &GuestMemory, sid: u32) -> Result<(), String> {
        let newer = match self.newer.get(&sid) {
            Some(n) => *n,
            None => {
                let (layout, flags) = gb
                    .surface_shape(sid)
                    .ok_or_else(|| format!("surface {sid} is not defined"))?;
                self.renderer
                    .create(sid, &layout, flags)
                    .map_err(|e| format!("GPU: surface {sid}: {e}"))?;
                self.newer.insert(sid, Newer::Host);
                Newer::Host
            }
        };
        if newer == Newer::Host {
            let host = gb.host_copy(guest, sid)?;
            self.renderer
                .upload(sid, host)
                .map_err(|e| format!("GPU: filling surface {sid}: {e}"))?;
            self.newer.insert(sid, Newer::Same);
        }
        Ok(())
    }

    fn gpu_wrote(&mut self, sid: u32) {
        self.newer.insert(sid, Newer::Gpu);
    }

    fn apply_before(
        &mut self,
        e: &Effects,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        for sid in &e.gone {
            self.drop_surface(*sid);
        }
        for sid in &e.host_uses {
            self.sync_host(gb, guest, *sid)?;
        }
        Ok(())
    }

    fn apply_after(&mut self, e: &Effects) {
        for sid in &e.host_writes {
            if let Some(n) = self.newer.get_mut(sid) {
                *n = Newer::Host;
            }
        }
    }

    /// Before a guest-backed command: GPU copies it reads come back, and
    /// surfaces it redefines leave the GPU.
    pub fn before_gb(
        &mut self,
        command: &GbCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        self.apply_before(&gb_effects(command, gb), gb, guest)
    }

    /// After a guest-backed command: device copies it wrote are newer.
    pub fn after_gb(&mut self, command: &GbCommand, gb: &Gb) {
        self.apply_after(&gb_effects(command, gb));
    }

    /// Before a DX command that the context model executes on device
    /// copies (uploads from and readbacks to MOBs).
    pub fn before_dx(
        &mut self,
        command: &DxCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        self.apply_before(&dx_host_effects(command), gb, guest)
    }

    /// After such a command.
    pub fn after_dx(&mut self, command: &DxCommand) {
        self.apply_after(&dx_host_effects(command));
    }

    /// The array layer and mip level of a subresource of `sid`.
    fn image_of(gb: &Gb, sid: u32, sub: u32) -> Result<(u32, u32), String> {
        let image = gb.subresource(sid, sub)?;
        Ok((image.face, image.mipmap))
    }

    /// Executes a command for which [`runs_on_gpu`] holds, in `context`.
    pub fn execute_dx(
        &mut self,
        command: &DxCommand,
        context: &Context,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let gpu_err = |e: svga_render::Error| e.to_string();
        match *command {
            DxCommand::ClearRenderTargetView { view, color } => {
                let Some(Object::RenderTargetView(target, range)) =
                    context.object(dx::cotable::RTVIEW, view)
                else {
                    return Err(format!("render target view {view} is not defined"));
                };
                let (mip, first, count) = match *range {
                    RtvRange::Texture {
                        mip,
                        first_slice,
                        slices,
                    } => (mip, first_slice, slices),
                    RtvRange::Texture3d { mip, .. } => (mip, 0, 1),
                    RtvRange::Buffer { .. } => return Err("clearing a buffer view".into()),
                };
                let sid = target.sid;
                self.sync_gpu(gb, guest, sid)?;
                // Direct3D clamps the slice count to the surface.
                let layers = gb.surface_shape(sid).map_or(1, |(l, _)| l.layers());
                let count = count.min(layers.saturating_sub(first));
                self.renderer
                    .clear_color(sid, mip, first, count, color)
                    .map_err(gpu_err)?;
                self.gpu_wrote(sid);
            }
            DxCommand::PredCopy { dest, src } => {
                self.sync_gpu(gb, guest, src)?;
                self.sync_gpu(gb, guest, dest)?;
                self.renderer.copy_whole(dest, src).map_err(gpu_err)?;
                self.gpu_wrote(dest);
            }
            DxCommand::PredCopyRegion {
                dest,
                dest_sub,
                src,
                src_sub,
                copy,
            } => self.copy_region(gb, guest, dest, dest_sub, src, src_sub, &copy)?,
            DxCommand::SurfaceCopyAndReadback { src, dest, copy } => {
                self.copy_region(gb, guest, dest, 0, src, 0, &copy)?;
                self.sync_host(gb, guest, dest)?;
                gb.execute(GbCommand::ReadbackSurface { sid: dest }, guest)?;
            }
            DxCommand::BufferCopy {
                dest,
                src,
                dest_x,
                src_x,
                width,
            } => {
                self.sync_gpu(gb, guest, src)?;
                self.sync_gpu(gb, guest, dest)?;
                self.renderer
                    .copy_buffer(
                        dest,
                        u64::from(dest_x),
                        src,
                        u64::from(src_x),
                        u64::from(width),
                    )
                    .map_err(gpu_err)?;
                self.gpu_wrote(dest);
            }
            DxCommand::TransferFromBuffer {
                src,
                src_offset,
                src_pitch,
                src_slice_pitch,
                dest,
                dest_sub,
                dest_box,
                ..
            } => {
                let (layer, mip) = Self::image_of(gb, dest, dest_sub)?;
                self.sync_gpu(gb, guest, src)?;
                self.sync_gpu(gb, guest, dest)?;
                let to = At {
                    layer,
                    mip,
                    offset: [dest_box.x, dest_box.y, dest_box.z],
                };
                self.renderer
                    .copy_buffer_to_image(
                        dest,
                        to,
                        [dest_box.w, dest_box.h, dest_box.d],
                        src,
                        u64::from(src_offset),
                        src_pitch,
                        src_slice_pitch,
                    )
                    .map_err(gpu_err)?;
                self.gpu_wrote(dest);
            }
            _ => return Err("not a GPU command".into()),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_region(
        &mut self,
        gb: &mut Gb,
        guest: &GuestMemory,
        dest: u32,
        dest_sub: u32,
        src: u32,
        src_sub: u32,
        copy: &svga_core::gb::CopyBox,
    ) -> Result<(), String> {
        self.sync_gpu(gb, guest, src)?;
        self.sync_gpu(gb, guest, dest)?;
        let b = copy.dest;
        let buffers = gb
            .surface_shape(dest)
            .is_some_and(|(l, _)| l.format() == svga_proto::format::BUFFER);
        let result = if buffers {
            self.renderer.copy_buffer(
                dest,
                u64::from(b.x),
                src,
                u64::from(copy.src.0),
                u64::from(b.w),
            )
        } else {
            let (dl, dm) = Self::image_of(gb, dest, dest_sub)?;
            let (sl, sm) = Self::image_of(gb, src, src_sub)?;
            self.renderer.copy_image(
                dest,
                At {
                    layer: dl,
                    mip: dm,
                    offset: [b.x, b.y, b.z],
                },
                src,
                At {
                    layer: sl,
                    mip: sm,
                    offset: [copy.src.0, copy.src.1, copy.src.2],
                },
                [b.w, b.h, b.d],
            )
        };
        result.map_err(|e| e.to_string())?;
        self.gpu_wrote(dest);
        Ok(())
    }

    /// Submits recorded GPU work (end of a command buffer).
    pub fn flush(&mut self) {
        if let Err(e) = self.renderer.flush() {
            crate::log!("GPU: {e}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::dxctx::{DxContexts, Limits};
    use crate::gb::Limits as GbLimits;
    use crate::shm::SharedMem;
    use std::os::fd::OwnedFd;
    use svga_core::dx::{RtvRange, ViewTarget};
    use svga_core::gb::SurfaceDef;
    use svga_core::layout::Size3;
    use svga_proto::{format, mobfmt};

    const RAM_BASE: u64 = 0x10_0000;

    #[test]
    fn a_clear_on_the_gpu_reaches_the_guest() {
        let mut gpu = match Gpu::new() {
            Ok(g) => g,
            Err(e) => return eprintln!("no GPU for this test: {e}"),
        };
        let ram = SharedMem::new("ram", 0x10000).unwrap();
        let fd: OwnedFd = ram.fd().try_clone_to_owned().unwrap();
        let mut guest = GuestMemory::default();
        guest.add(RAM_BASE, 0x10000, Some(fd), 0, true).unwrap();
        let mut gb = Gb::new(GbLimits {
            mob_pages: 16,
            mob_size: 1 << 16,
            surface_bytes: 1 << 20,
            max_width: 64,
            max_height: 64,
        });
        let mut dx = DxContexts::new(Limits {
            contexts: 4,
            code_bytes: 1 << 16,
            shader_bytes: 1 << 12,
        });
        // Runs a guest-backed command through the coherence hooks.
        let run_gb = |gpu: &mut Gpu, gb: &mut Gb, c: GbCommand| {
            gpu.before_gb(&c, gb, &guest).unwrap();
            gb.execute(c.clone(), &guest).unwrap();
            gpu.after_gb(&c, gb);
        };
        // MOB 1: the staging surface's guest copy; MOB 2: the RTV table.
        for (id, page) in [(1, 1), (2, 2)] {
            run_gb(
                &mut gpu,
                &mut gb,
                GbCommand::DefineMob {
                    id,
                    pt_depth: mobfmt::RANGE,
                    base: RAM_BASE / 4096 + page,
                    size: 4096,
                },
            );
        }
        let surface = |sid| {
            GbCommand::DefineSurface(SurfaceDef {
                sid,
                flags: 0,
                format: format::B8G8R8A8_UNORM,
                mip_levels: 1,
                samples: 0,
                size: Size3 {
                    width: 4,
                    height: 4,
                    depth: 1,
                },
                array_size: 0,
            })
        };
        run_gb(&mut gpu, &mut gb, surface(10));
        run_gb(&mut gpu, &mut gb, surface(11));
        run_gb(
            &mut gpu,
            &mut gb,
            GbCommand::BindSurface {
                sid: 11,
                mob: 1,
                pitch: 0,
            },
        );
        // A context with a render target view of surface 10.
        let mut dx_run = |c: DxCommand, current| dx.execute(c, current, &mut gb, &guest).unwrap();
        dx_run(DxCommand::DefineContext { cid: 0 }, None);
        dx_run(
            DxCommand::SetCoTable {
                cid: 0,
                mob: 2,
                table: dx::cotable::RTVIEW,
                valid_size: 0,
                grow: false,
            },
            None,
        );
        dx_run(
            DxCommand::DefineRenderTargetView {
                id: 0,
                target: ViewTarget {
                    sid: 10,
                    format: format::B8G8R8A8_UNORM,
                    dimension: dx::resource::TEXTURE2D,
                },
                range: RtvRange::Texture {
                    mip: 0,
                    first_slice: 0,
                    slices: 1,
                },
            },
            Some(0),
        );
        // Clear to red, copy to the staging surface: all on the GPU.
        let context = dx.context(0).unwrap();
        let clear = DxCommand::ClearRenderTargetView {
            view: 0,
            color: [1.0, 0.0, 0.0, 1.0],
        };
        gpu.execute_dx(&clear, context, &mut gb, &guest).unwrap();
        let copy = DxCommand::PredCopy { dest: 11, src: 10 };
        gpu.execute_dx(&copy, context, &mut gb, &guest).unwrap();
        gpu.flush();
        let mut pixel = [0u8; 4];
        assert!(ram.read(4096, &mut pixel));
        assert_eq!(pixel, [0; 4], "not read back yet");
        // The guest reads the staging surface back: the GPU copy comes
        // to the device copy, then to the MOB.
        run_gb(&mut gpu, &mut gb, GbCommand::ReadbackSurface { sid: 11 });
        let mut pixels = [0u8; 64];
        assert!(ram.read(4096, &mut pixels));
        assert!(
            pixels.chunks(4).all(|p| p == [0, 0, 0xff, 0xff]),
            "red in B, G, R, A order on {}: {pixels:?}",
            gpu.name()
        );
        // Destroying a surface frees its GPU copy.
        run_gb(&mut gpu, &mut gb, GbCommand::DestroySurface { sid: 10 });
        assert!(!gpu.newer.contains_key(&10));
    }
}
