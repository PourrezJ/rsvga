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
//! On the GPU: draws, clears, copies between surfaces (plain, scaled or
//! converting), multisample resolves and mipmap generation. A draw gathers
//! the context's bindings ([`Context::bindings`]) into a
//! [`svga_render::Draw`]; its shaders are translated again when the
//! vertex formats or render target views hold integers (the SPIR-V types
//! must match), and those translations are cached. Geometry, hull and
//! domain shaders, stream output, and raw or structured buffer views are
//! not drawn yet.
//!
//! A device copy made only to fill a GPU copy is freed at once, and under
//! memory pressure device copies of surfaces the GPU holds are freed after
//! each command buffer: GPU copies are the ones that count, device copies
//! come back (downloaded) when the guest or the CPU path needs them.
//!
//! Surfaces whose bytes cannot go to the GPU as they are (24-bit depth on
//! AMD, multisampled images) live on the GPU only: their device copies
//! are neither uploaded nor read back.

use std::sync::Arc;
use svga_core::hash::FastMap;

use svga_core::cotable::Object;
use svga_core::dx::{DxCommand, InputElement, RtvRange, SrvRange};
use svga_core::gb::GbCommand;
use svga_proto::{INVALID_ID, cond_bind, dx};
use svga_render::{At, Renderer};
use svga_shader::{BindingKind, Options, Scalar, Shader};

use crate::dxctx::Context;

mod dump;
mod nine;
mod screen;
mod video;
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
    newer: FastMap<u32, Newer>,
    /// Shaders translated with other than the default options.
    shaders: FastMap<(Code<u8>, Options), Translation>,
    /// Direct3D 9 shaders, by code and options.
    shaders9: FastMap<(Code<u32>, svga_shader::d3d9::Options), nine::Translation9>,
    /// The device's own Direct3D 9 programs (pretransformed vertices by
    /// vertex inputs, the default pixel shader), made once.
    programs9: FastMap<Vec<(u32, u32)>, Arc<[u32]>>,
    /// The legacy occlusion query being counted: context, its first
    /// occlusion slot, and the samples of its slots already read.
    query9: Option<(u32, u32, u64)>,
    /// Ended queries waiting for their draws: MOB, offset, first and end
    /// slot, samples already read. Written at the next flush, or at once
    /// when the guest waits for one.
    ended9: Vec<(u32, u32, u32, u32, u64)>,
    /// Parts of the screen being read from the GPU, in order.
    screen_reads: Vec<screen::ScreenRead>,
    /// Size of the work image of video blits, once made.
    video_scratch: Option<(u32, u32)>,
    /// Render target dumps (`RSVGA_DUMP_TARGETS`).
    dump: Option<dump::Dump>,
}

fn gpu_err(e: svga_render::Error) -> String {
    e.to_string()
}

/// A translated shader, or why there is none.
type Translation = Result<Arc<Shader>, String>;

/// Shader code as a cache key: by address, not contents (hashing whole
/// programs at every draw cost more than the rest of the lookup). The key
/// holds the code, so the address cannot be reused while it is cached.
#[derive(Clone, Debug)]
struct Code<T>(Arc<[T]>);

impl<T> PartialEq for Code<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for Code<T> {}

impl<T> std::hash::Hash for Code<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0).cast::<T>() as usize).hash(state);
        self.0.len().hash(state);
    }
}

/// Most translations kept in [`Gpu::shaders`].
const MAX_SHADER_VARIANTS: usize = 1024;

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

fn gb_effects(command: &GbCommand) -> Effects {
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
            | DxCommand::ClearDepthStencilView { .. }
            | DxCommand::Draw { .. }
            | DxCommand::DrawIndexed { .. }
            | DxCommand::DrawInstanced { .. }
            | DxCommand::DrawIndexedInstanced { .. }
            | DxCommand::PredCopy { .. }
            | DxCommand::PredCopyRegion { .. }
            | DxCommand::ResolveCopy { .. }
            | DxCommand::PredConvert { .. }
            | DxCommand::PredConvertRegion { .. }
            | DxCommand::GenMips { .. }
            | DxCommand::BufferCopy { .. }
            | DxCommand::TransferFromBuffer { .. }
            | DxCommand::SurfaceCopyAndReadback { .. }
            | DxCommand::Staging { .. }
            | DxCommand::Dispatch { .. }
            | DxCommand::DispatchIndirect { .. }
            | DxCommand::DrawIndirect { .. }
            | DxCommand::DrawAuto
            | DxCommand::ClearUaViewUint { .. }
            | DxCommand::ClearUaViewFloat { .. }
    )
}

/// The surface a staging copy writes, and its subresource when only one
/// is written.
fn staging_dest(copy: &DxCommand) -> Result<(u32, Option<u32>), String> {
    match *copy {
        DxCommand::PredCopyRegion { dest, dest_sub, .. }
        | DxCommand::PredConvertRegion { dest, dest_sub, .. } => Ok((dest, Some(dest_sub))),
        DxCommand::PredCopy { dest, .. }
        | DxCommand::PredConvert { dest, .. }
        | DxCommand::BufferCopy { dest, .. } => Ok((dest, None)),
        _ => Err(format!("{copy:?} is not a staging copy")),
    }
}

/// The bytes a raw or structured buffer view covers: `first` and `count`
/// count words for raw views (typeless 32-bit formats), else elements of
/// the buffer's structure stride (unknown: from the start, to the end).
fn word_range(gb: &Gb, sid: u32, format: u32, first: u32, count: u32) -> (u64, u64) {
    let stride = match gb.buffer_stride(sid) {
        0 => svga_proto::format::desc(format).map_or(0, |d| d.bytes_per_block),
        s => s,
    };
    if stride == 0 {
        return (0, u64::MAX);
    }
    (
        u64::from(first).saturating_mul(u64::from(stride)),
        u64::from(count).saturating_mul(u64::from(stride)),
    )
}

/// What stream-output declarations capture: stream 0's registers, packed
/// in their buffers in declaration order (registers past 31 are gaps).
fn captures(decls: &[svga_core::dx::StreamOutputDecl]) -> Vec<svga_shader::Capture> {
    let mut offsets = [0u32; 4];
    let mut out = Vec::new();
    for d in decls.iter().filter(|d| d.stream == 0) {
        let Some(offset) = offsets.get_mut(d.slot as usize) else {
            continue;
        };
        if d.register < 32 {
            out.push(svga_shader::Capture {
                buffer: d.slot,
                register: d.register,
                mask: d.mask,
                offset: *offset,
            });
        }
        *offset = offset.saturating_add(d.mask.count_ones().saturating_mul(4));
    }
    out
}

/// The target of depth-stencil view `view` of `context`.
fn depth_target(context: &Context, view: u32) -> Result<svga_render::Target, String> {
    match context.object(dx::cotable::DSVIEW, view) {
        Some(Object::DepthStencilView(d)) => Ok(svga_render::Target {
            sid: d.target.sid,
            format: d.target.format,
            mip: d.mip,
            first_layer: d.first_slice,
        }),
        _ => Err(format!("depth-stencil view {view} is not defined")),
    }
}

impl Gpu {
    /// Opens the host GPU.
    pub fn new() -> Result<Self, String> {
        let renderer = Renderer::new().map_err(|e| e.to_string())?;
        Ok(Self {
            renderer,
            newer: FastMap::default(),
            shaders: FastMap::default(),
            shaders9: FastMap::default(),
            programs9: FastMap::default(),
            query9: None,
            ended9: Vec::new(),
            screen_reads: Vec::new(),
            video_scratch: None,
            dump: dump::Dump::from_env(),
        })
    }

    /// The GPU's name.
    pub fn name(&self) -> &str {
        self.renderer.name()
    }

    /// Forgets every surface (device reset).
    pub fn reset(&mut self) {
        self.drop_screen_reads();
        self.drop_video_work();
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
        self.newer.insert(sid, Newer::Same);
        if !self.renderer.copyable(sid) {
            crate::log!("GPU: surface {sid} cannot be read back (format)");
            return Ok(());
        }
        let host = gb.host_copy(guest, sid)?;
        let _span = crate::prof::span(crate::prof::Cat::Download);
        self.renderer
            .download(sid, host)
            .map_err(|e| format!("GPU: reading surface {sid}: {e}"))
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
        if newer == Newer::Host && !self.renderer.copyable(sid) {
            self.newer.insert(sid, Newer::Same);
        } else if newer == Newer::Host {
            // A device copy made only for this upload is not kept: the
            // GPU copy becomes the only one.
            let kept = gb.has_host_copy(sid);
            let _span = crate::prof::span(crate::prof::Cat::Upload);
            let host = gb.host_copy(guest, sid)?;
            self.renderer
                .upload(sid, host)
                .map_err(|e| format!("GPU: filling surface {sid}: {e}"))?;
            if kept {
                self.newer.insert(sid, Newer::Same);
            } else {
                gb.drop_host_copy(sid);
                self.newer.insert(sid, Newer::Gpu);
            }
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

    /// Runs a guest-backed command on the GPU when that saves a round trip
    /// through the device copies; true if it did: `SURFACE_COPY` and
    /// `DX_PRESENTBLT` of a source the GPU holds, what screen targets
    /// show (read from the GPU without waiting, see `screen`), and video
    /// blits (`video`).
    pub fn execute_gb(
        &mut self,
        command: &GbCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<bool, String> {
        match *command {
            GbCommand::SurfaceCopy {
                src,
                dest,
                ref boxes,
            } => return self.surface_copy(src, dest, boxes, gb, guest),
            GbCommand::BindScreenTarget { stid, image } => {
                // Binding shows the new image at once.
                let all = gb.bind_screen(stid, image)?;
                self.show(gb, guest, stid, all)?;
                return Ok(true);
            }
            GbCommand::UpdateScreenTarget { stid, rect } => {
                self.show(gb, guest, stid, rect)?;
                return Ok(true);
            }
            GbCommand::VideoBlt(ref blit) => {
                self.video_blt(blit, gb, guest)?;
                return Ok(true);
            }
            _ => {}
        }
        let GbCommand::PresentBlt {
            src,
            src_sub,
            dest,
            dest_sub,
            src_box,
            dest_box,
            ..
        } = *command
        else {
            return Ok(false);
        };
        if src == dest || self.newer.get(&src) != Some(&Newer::Gpu) {
            return Ok(false);
        }
        let (from, to) = (
            gb.subresource(src, src_sub)?,
            gb.subresource(dest, dest_sub)?,
        );
        if [src_box.w, src_box.h, dest_box.w, dest_box.h].contains(&0) {
            return Ok(false);
        }
        self.sync_gpu(gb, guest, dest)?;
        let scaled = (src_box.w, src_box.h) != (dest_box.w, dest_box.h);
        let done = self.renderer.blit(
            dest,
            At {
                layer: to.face,
                mip: to.mipmap,
                offset: [dest_box.x, dest_box.y, 0],
            },
            [dest_box.w, dest_box.h, 1],
            src,
            At {
                layer: from.face,
                mip: from.mipmap,
                offset: [src_box.x, src_box.y, 0],
            },
            [src_box.w, src_box.h, 1],
            scaled,
        );
        match done {
            Ok(()) => {
                self.gpu_wrote(dest);
                // Shown at once where a screen target displays it.
                let rect = svga_core::command::Rect {
                    x: dest_box.x,
                    y: dest_box.y,
                    width: dest_box.w,
                    height: dest_box.h,
                };
                for stid in gb.screens_showing(to) {
                    self.show(gb, guest, stid, rect)?;
                }
                Ok(true)
            }
            // Formats the GPU cannot blit: the device copies do it.
            Err(_) => Ok(false),
        }
    }

    /// `SURFACE_COPY` on the GPU, when the source is there and both
    /// images are on it (or can be). False: the device copies do it.
    fn surface_copy(
        &mut self,
        src: svga_core::gb::ImageId,
        dest: svga_core::gb::ImageId,
        boxes: &[svga_core::gb::CopyBox],
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<bool, String> {
        if src.sid == dest.sid || self.newer.get(&src.sid) != Some(&Newer::Gpu) {
            return Ok(false);
        }
        if !self.renderer.has(src.sid) {
            return Ok(false);
        }
        self.sync_gpu(gb, guest, dest.sid)?;
        for (n, b) in boxes.iter().enumerate() {
            let copied = self.renderer.copy_image(
                dest.sid,
                At {
                    layer: dest.face,
                    mip: dest.mipmap,
                    offset: [b.dest.x, b.dest.y, b.dest.z],
                },
                src.sid,
                At {
                    layer: src.face,
                    mip: src.mipmap,
                    offset: [b.src.0, b.src.1, b.src.2],
                },
                [b.dest.w, b.dest.h, b.dest.d.max(1)],
            );
            match copied {
                Ok(()) => {}
                // Nothing done yet: the device copies try.
                Err(_) if n == 0 => return Ok(false),
                Err(e) => {
                    // Boxes done on the GPU stay there.
                    self.gpu_wrote(dest.sid);
                    return Err(format!("GPU: surface copy: {e}"));
                }
            }
        }
        self.gpu_wrote(dest.sid);
        Ok(true)
    }

    /// Before a guest-backed command: GPU copies it reads come back, and
    /// surfaces it redefines leave the GPU.
    pub fn before_gb(
        &mut self,
        command: &GbCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let effects = gb_effects(command);
        if effects
            .host_uses
            .iter()
            .any(|sid| self.newer.get(sid) == Some(&Newer::Gpu))
        {
            crate::prof::count(match command {
                GbCommand::SurfaceCopy { .. } => "readback for SurfaceCopy",
                GbCommand::PresentBlt { .. } => "readback for PresentBlt",
                GbCommand::ReadbackSurface { .. } => "readback for ReadbackSurface",
                GbCommand::ReadbackImage { .. } => "readback for ReadbackImage",
                GbCommand::ReadbackImagePartial { .. } => "readback for ReadbackImagePartial",
                GbCommand::UpdateImage { .. } => "readback for UpdateImage",
                GbCommand::UpdateSurface { .. } => "readback for UpdateSurface",
                GbCommand::CondBindSurface { .. } => "readback for CondBindSurface",
                _ => "readback for another GB command",
            });
        }
        self.apply_before(&gb_effects(command), gb, guest)
    }

    /// After a guest-backed command: device copies it wrote are newer.
    pub fn after_gb(&mut self, command: &GbCommand) {
        self.apply_after(&gb_effects(command));
    }

    /// A `DX_BUFFER_UPDATE` of a buffer whose newest copy is the GPU's:
    /// the range goes from its MOB straight to the GPU (true), rather than
    /// into a device copy, then the whole buffer to the GPU at the next
    /// draw. Games update parts of their dynamic buffers every frame. A
    /// device copy equal to the GPU's is dropped: the GPU's becomes the
    /// only one, as after any GPU write.
    pub fn update_on_gpu(
        &mut self,
        command: &DxCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<bool, String> {
        let DxCommand::BufferUpdate { sid, x, width } = *command else {
            return Ok(false);
        };
        if !self.renderer.holds_buffer(sid) {
            return Ok(false);
        }
        match self.newer.get(&sid) {
            Some(Newer::Gpu) => {}
            Some(Newer::Same) => gb.drop_host_copy(sid),
            _ => return Ok(false),
        }
        let _span = crate::prof::span(crate::prof::Cat::Upload);
        let mut error = None;
        self.renderer
            .write_buffer(sid, u64::from(x), u64::from(width), |dest| {
                gb.read_from_mob(guest, sid, u64::from(x), dest)
                    .map_err(|e| error = Some(e))
                    .is_ok()
            })
            .map_err(|e| {
                error
                    .take()
                    .unwrap_or_else(|| format!("GPU: updating buffer {sid}: {e}"))
            })?;
        self.newer.insert(sid, Newer::Gpu);
        Ok(true)
    }

    /// Before a DX command that the context model executes on device
    /// copies (uploads from and readbacks to MOBs).
    pub fn before_dx(
        &mut self,
        command: &DxCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let effects = dx_host_effects(command);
        if effects
            .host_uses
            .iter()
            .any(|sid| self.newer.get(sid) == Some(&Newer::Gpu))
        {
            crate::prof::count("readback for a DX command");
        }
        self.apply_before(&effects, gb, guest)
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
        if self.dump_recording()
            && !matches!(
                command,
                DxCommand::Draw { .. }
                    | DxCommand::DrawIndexed { .. }
                    | DxCommand::DrawInstanced { .. }
                    | DxCommand::DrawIndexedInstanced { .. }
            )
        {
            let mut text = format!("{command:?}");
            text.truncate(200);
            crate::log!("dump command: {text}");
        }
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
            DxCommand::ClearDepthStencilView {
                view,
                flags,
                stencil,
                depth,
            } => {
                let target = depth_target(context, view)?;
                self.sync_gpu(gb, guest, target.sid)?;
                self.renderer
                    .clear_depth_stencil(target, flags, depth, (stencil & 0xff) as u8)
                    .map_err(gpu_err)?;
                self.gpu_wrote(target.sid);
            }
            DxCommand::Draw { .. }
            | DxCommand::DrawIndexed { .. }
            | DxCommand::DrawInstanced { .. }
            | DxCommand::DrawIndexedInstanced { .. }
            | DxCommand::DrawIndirect { .. }
            | DxCommand::DrawAuto => self.draw(command, context, gb, guest)?,
            DxCommand::Dispatch { .. } | DxCommand::DispatchIndirect { .. } => {
                self.dispatch(command, context, gb, guest)?;
            }
            DxCommand::ClearUaViewUint { view, value } => {
                self.clear_ua_view(context, view, Err(value), gb, guest)?;
            }
            DxCommand::ClearUaViewFloat { view, value } => {
                self.clear_ua_view(context, view, Ok(value), gb, guest)?;
            }
            DxCommand::ResolveCopy {
                dest,
                dest_sub,
                src,
                src_sub,
                ..
            } => {
                let (dl, dm) = Self::image_of(gb, dest, dest_sub)?;
                let (sl, sm) = Self::image_of(gb, src, src_sub)?;
                self.sync_gpu(gb, guest, src)?;
                self.sync_gpu(gb, guest, dest)?;
                let at = |layer, mip| At {
                    layer,
                    mip,
                    offset: [0; 3],
                };
                self.renderer
                    .resolve(dest, at(dl, dm), src, at(sl, sm))
                    .map_err(gpu_err)?;
                self.gpu_wrote(dest);
            }
            DxCommand::PredConvertRegion {
                dest,
                dest_sub,
                dest_box,
                src,
                src_sub,
                src_box,
            } => {
                let (dl, dm) = Self::image_of(gb, dest, dest_sub)?;
                let (sl, sm) = Self::image_of(gb, src, src_sub)?;
                self.sync_gpu(gb, guest, src)?;
                self.sync_gpu(gb, guest, dest)?;
                let scaled =
                    (dest_box.w, dest_box.h, dest_box.d) != (src_box.w, src_box.h, src_box.d);
                self.renderer
                    .blit(
                        dest,
                        At {
                            layer: dl,
                            mip: dm,
                            offset: [dest_box.x, dest_box.y, dest_box.z],
                        },
                        [dest_box.w, dest_box.h, dest_box.d],
                        src,
                        At {
                            layer: sl,
                            mip: sm,
                            offset: [src_box.x, src_box.y, src_box.z],
                        },
                        [src_box.w, src_box.h, src_box.d],
                        scaled,
                    )
                    .map_err(gpu_err)?;
                self.gpu_wrote(dest);
            }
            DxCommand::PredConvert { dest, src } => {
                self.sync_gpu(gb, guest, src)?;
                self.sync_gpu(gb, guest, dest)?;
                let shape = |sid| {
                    gb.surface_shape(sid)
                        .map(|(l, _)| l)
                        .ok_or_else(|| format!("surface {sid} is not defined"))
                };
                let (s, d) = (shape(src)?, shape(dest)?);
                // Every subresource both have, each scaled to fit.
                for layer in 0..s.layers().min(d.layers()) {
                    for mip in 0..s.mip_levels().min(d.mip_levels()) {
                        let (a, b) = (s.mip_size(mip), d.mip_size(mip));
                        let at = At {
                            layer,
                            mip,
                            offset: [0; 3],
                        };
                        self.renderer
                            .blit(
                                dest,
                                at,
                                [b.width, b.height, b.depth],
                                src,
                                at,
                                [a.width, a.height, a.depth],
                                (a.width, a.height) != (b.width, b.height),
                            )
                            .map_err(gpu_err)?;
                    }
                }
                self.gpu_wrote(dest);
            }
            DxCommand::GenMips { view } => {
                let Some(Object::ShaderResourceView(t, range)) =
                    context.object(dx::cotable::SRVIEW, view)
                else {
                    return Err(format!("shader resource view {view} is not defined"));
                };
                let SrvRange::Texture {
                    most_detailed_mip,
                    mip_levels,
                    first_slice,
                    slices,
                } = *range
                else {
                    return Err("mipmaps of a buffer view".into());
                };
                let sid = t.sid;
                self.sync_gpu(gb, guest, sid)?;
                self.renderer
                    .generate_mips(sid, most_detailed_mip, mip_levels, first_slice, slices)
                    .map_err(gpu_err)?;
                self.gpu_wrote(sid);
            }
            DxCommand::PredCopy { dest, src } if self.host_only(gb, src) => {
                self.host_copy(gb, guest, dest, src, None)?;
            }
            DxCommand::PredCopyRegion {
                dest,
                dest_sub,
                src,
                src_sub,
                copy,
            } if self.host_only(gb, src) => {
                self.host_copy(gb, guest, dest, src, Some((dest_sub, src_sub, copy)))?;
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
            DxCommand::Staging { ref copy, readback } => {
                let (sid, sub) = staging_dest(copy)?;
                self.execute_dx(copy, context, gb, guest)?;
                if readback {
                    self.sync_host(gb, guest, sid)?;
                    let command = match sub {
                        Some(sub) => GbCommand::ReadbackImage {
                            image: gb.subresource(sid, sub)?,
                        },
                        None => GbCommand::ReadbackSurface { sid },
                    };
                    gb.execute(command, guest)?;
                }
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

    /// Shader `id` of `context`, translated with `options`.
    fn shader(
        &mut self,
        context: &Context,
        id: u32,
        options: &Options,
    ) -> Result<Arc<Shader>, String> {
        if *options == Options::default() {
            return match context.shader_spirv(id) {
                Some(Ok(s)) => Ok(Arc::clone(s)),
                Some(Err(e)) => Err(format!("shader {id}: {e}")),
                None => Err(format!("shader {id} has no code")),
            };
        }
        let code = context
            .shader_code(id)
            .ok_or_else(|| format!("shader {id} has no code"))?;
        let key = (Code(Arc::clone(code)), options.clone());
        if let Some(result) = self.shaders.get(&key) {
            return result.clone();
        }
        let words: Vec<u32> = code
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let result = svga_shader::translate_words(&words, options)
            .map(Arc::new)
            .map_err(|e| format!("shader {id}: {e}"));
        if self.shaders.len() >= MAX_SHADER_VARIANTS {
            self.shaders.clear();
        }
        self.shaders.insert(key, result.clone());
        result
    }

    /// Draws with the pipeline state of `context`.
    fn draw(
        &mut self,
        command: &DxCommand,
        context: &Context,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        use svga_render::{Call, Draw, Target, VertexAttribute, VertexBuffer};
        let b = &context.bindings;
        let call = match *command {
            DxCommand::Draw {
                vertex_count,
                start_vertex,
            } => Call::Draw {
                vertices: vertex_count,
                instances: 1,
                first_vertex: start_vertex,
                first_instance: 0,
            },
            DxCommand::DrawInstanced {
                vertex_count,
                instance_count,
                start_vertex,
                start_instance,
            } => Call::Draw {
                vertices: vertex_count,
                instances: instance_count,
                first_vertex: start_vertex,
                first_instance: start_instance,
            },
            DxCommand::DrawIndexed {
                index_count,
                start_index,
                base_vertex,
            } => Call::Indexed {
                indices: index_count,
                instances: 1,
                first_index: start_index,
                base_vertex,
                first_instance: 0,
            },
            DxCommand::DrawIndexedInstanced {
                index_count,
                instance_count,
                start_index,
                base_vertex,
                start_instance,
            } => Call::Indexed {
                indices: index_count,
                instances: instance_count,
                first_index: start_index,
                base_vertex,
                first_instance: start_instance,
            },
            DxCommand::DrawIndirect {
                indexed,
                sid,
                offset,
            } => {
                self.sync_gpu(gb, guest, sid)?;
                Call::Indirect {
                    sid,
                    offset: u64::from(offset),
                    indexed,
                }
            }
            DxCommand::DrawAuto => {
                // What stream output last wrote to the buffer of slot 0.
                let vb = b.vertex_buffers[0];
                if vb.sid == INVALID_ID {
                    return Ok(());
                }
                self.sync_gpu(gb, guest, vb.sid)?;
                Call::Auto {
                    sid: vb.sid,
                    stride: vb.stride,
                }
            }
            _ => return Err("not a draw".into()),
        };
        let vs_id = b.stages[0].shader;
        if vs_id == INVALID_ID {
            return Err("draw without a vertex shader".into());
        }
        if b.topology == 0 {
            return Ok(());
        }
        // Input layout, and the types of the vertex shader's inputs.
        let elements: &[InputElement] = match b.input_layout {
            INVALID_ID => &[],
            id => match context.object(dx::cotable::ELEMENTLAYOUT, id) {
                Some(Object::ElementLayout(e)) => e,
                _ => return Err(format!("input layout {id} is not defined")),
            },
        };
        let mut vs_options = Options::default();
        for e in elements {
            if let Some(input) = vs_options.inputs.get_mut(e.register as usize) {
                *input = svga_render::format::scalar(e.format);
            }
        }
        // Targets, and the types of the pixel shader's outputs.
        let mut ps_options = Options::default();
        let mut targets = [None; 8];
        for (slot, &view) in b.render_targets.iter().enumerate().take(targets.len()) {
            if view == INVALID_ID {
                continue;
            }
            let Some(Object::RenderTargetView(t, range)) =
                context.object(dx::cotable::RTVIEW, view)
            else {
                return Err(format!("render target view {view} is not defined"));
            };
            let (mip, first_layer) = match *range {
                RtvRange::Texture {
                    mip, first_slice, ..
                } => (mip, first_slice),
                RtvRange::Texture3d { .. } => {
                    return Err("3D render targets are not supported yet".into());
                }
                RtvRange::Buffer { .. } => {
                    return Err("buffer render targets are not supported yet".into());
                }
            };
            targets[slot] = Some(Target {
                sid: t.sid,
                format: t.format,
                mip,
                first_layer,
            });
            ps_options.outputs[slot] = svga_render::format::scalar(t.format);
        }
        let depth = match b.depth_stencil_view {
            INVALID_ID => None,
            view => Some(depth_target(context, view)?),
        };
        // Dual-source blending: the pixel shader's o1 feeds target 0's
        // blend.
        if let Some(Object::BlendState(state)) = context.object(dx::cotable::BLENDSTATE, b.blend.0)
        {
            let t = state.targets[0];
            ps_options.dual_source = t.enable
                && [t.src, t.dest, t.src_alpha, t.dest_alpha]
                    .into_iter()
                    .any(svga_render::dual_source);
        }
        // Stream output: captured by the last stage before the rasterizer.
        let mut gs_options = Options::default();
        let mut ds_options = Options::default();
        let stream_output = match b.stream_output {
            INVALID_ID => None,
            id => {
                let Some(Object::StreamOutput(object)) =
                    context.object(dx::cotable::STREAMOUTPUT, id)
                else {
                    return Err(format!("stream output {id} is not defined"));
                };
                let (strides, rasterized) = (object.strides, object.rasterized_stream);
                let decls = context.stream_output_decls(id, gb, guest)?;
                let options = if b.stages[2].shader != INVALID_ID {
                    &mut gs_options
                } else if b.stages[4].shader != INVALID_ID {
                    &mut ds_options
                } else {
                    &mut vs_options
                };
                options.stream_output = captures(&decls);
                options.so_strides = strides;
                let mut targets = [None; 4];
                for (slot, t) in b.so_targets.iter().enumerate().take(4) {
                    if t.sid == INVALID_ID {
                        continue;
                    }
                    self.sync_gpu(gb, guest, t.sid)?;
                    // An offset of ~0 appends to what was written before.
                    let offset = (t.offset != u32::MAX).then_some(u64::from(t.offset));
                    let size = if t.size == 0 {
                        u64::MAX
                    } else {
                        u64::from(t.size)
                    };
                    targets[slot] = Some((t.sid, offset, size));
                }
                Some(svga_render::StreamOut {
                    targets,
                    rasterize: rasterized == 0,
                })
            }
        };
        let translate = crate::prof::span(crate::prof::Cat::Translate);
        let vs = self.shader(context, vs_id, &vs_options)?;
        let ps = match b.stages[1].shader {
            INVALID_ID => None,
            id => Some(self.shader(context, id, &ps_options)?),
        };
        let gs = match b.stages[2].shader {
            INVALID_ID => None,
            id => Some(self.shader(context, id, &gs_options)?),
        };
        // Tessellation: hull and domain shaders go together.
        let (hs, ds) = match (b.stages[3].shader, b.stages[4].shader) {
            (INVALID_ID, INVALID_ID) => (None, None),
            (INVALID_ID, _) | (_, INVALID_ID) => {
                return Err("a hull shader without a domain shader, or the reverse".into());
            }
            (h, d) => (
                Some(self.shader(context, h, &Options::default())?),
                Some(self.shader(context, d, &ds_options)?),
            ),
        };
        drop(translate);
        // Vertex attributes the shader reads, and their buffer slots.
        let mut attributes: Vec<VertexAttribute> = Vec::new();
        let mut slots: Vec<(u32, bool)> = Vec::new();
        for e in elements {
            if !vs.inputs.contains(&e.register)
                || attributes.iter().any(|a| a.location == e.register)
            {
                continue;
            }
            attributes.push(VertexAttribute {
                location: e.register,
                binding: e.slot,
                format: e.format,
                offset: e.offset,
            });
            if !slots.iter().any(|&(s, _)| s == e.slot) {
                slots.push((e.slot, e.per_instance));
            }
        }
        let mut buffers: Vec<VertexBuffer> = Vec::new();
        for &(slot, per_instance) in &slots {
            let vb = b
                .vertex_buffers
                .get(slot as usize)
                .filter(|v| v.sid != INVALID_ID);
            if let Some(v) = vb {
                self.sync_gpu(gb, guest, v.sid)?;
            }
            buffers.push(VertexBuffer {
                binding: slot,
                sid: vb.map(|v| v.sid),
                offset: vb.map_or(0, |v| u64::from(v.offset)),
                size: vb.map_or(u64::MAX, |v| match v.size {
                    svga_core::dx::WHOLE_SIZE => u64::MAX,
                    size => u64::from(size),
                }),
                stride: vb.map_or(0, |v| v.stride),
                per_instance,
            });
        }
        // Inputs without an attribute read zeros, from a slot of their own.
        let missing: Vec<u32> = vs
            .inputs
            .iter()
            .copied()
            .filter(|&l| !attributes.iter().any(|a| a.location == l))
            .collect();
        if !missing.is_empty() {
            let free = (0..dx::MAX_VERTEXBUFFERS as u32)
                .find(|s| !slots.iter().any(|&(used, _)| used == *s))
                .ok_or("no free vertex buffer slot")?;
            for location in missing {
                let format = match vs_options.inputs.get(location as usize) {
                    Some(Scalar::Uint) => svga_proto::format::R32G32B32A32_UINT,
                    Some(Scalar::Int) => svga_proto::format::R32G32B32A32_SINT,
                    _ => svga_proto::format::R32G32B32A32_FLOAT,
                };
                attributes.push(VertexAttribute {
                    location,
                    binding: free,
                    format,
                    offset: 0,
                });
            }
            buffers.push(VertexBuffer {
                binding: free,
                sid: None,
                offset: 0,
                size: u64::MAX,
                stride: 0,
                per_instance: false,
            });
        }
        let index = match call {
            Call::Indexed { .. } | Call::Indirect { indexed: true, .. } => {
                let (sid, format, offset, _) = b.index_buffer;
                if sid == INVALID_ID {
                    return Ok(());
                }
                self.sync_gpu(gb, guest, sid)?;
                Some((sid, format, u64::from(offset)))
            }
            Call::Draw { .. } | Call::Indirect { indexed: false, .. } | Call::Auto { .. } => None,
        };
        // Descriptors the shaders use; UA views they may write.
        let mut written = Vec::new();
        let mut resources = self.stage_bindings(context, &vs, 0, gb, guest, &mut written)?;
        for (shader, stage) in [(&ps, 1), (&gs, 2), (&hs, 3), (&ds, 4)] {
            if let Some(shader) = shader {
                resources.extend(self.stage_bindings(
                    context,
                    shader,
                    stage,
                    gb,
                    guest,
                    &mut written,
                )?);
            }
        }
        for sid in targets.iter().flatten().chain(depth.iter()).map(|t| t.sid) {
            self.sync_gpu(gb, guest, sid)?;
        }
        let rasterizer = match context.object(dx::cotable::RASTERIZERSTATE, b.rasterizer) {
            Some(Object::RasterizerState(r)) => *r,
            _ => svga_render::default_rasterizer(),
        };
        let depth_stencil = match context.object(dx::cotable::DEPTHSTENCIL, b.depth_stencil.0) {
            Some(Object::DepthStencilState(d)) => *d,
            _ => svga_render::default_depth_stencil(),
        };
        let blend = match context.object(dx::cotable::BLENDSTATE, b.blend.0) {
            Some(Object::BlendState(s)) => *s,
            _ => svga_render::default_blend(),
        };
        let draw = Draw {
            vs: &vs,
            ps: ps.as_deref(),
            gs: gs.as_deref(),
            tessellation: hs.as_deref().zip(ds.as_deref()),
            stream_output,
            attributes,
            buffers,
            index,
            topology: b.topology,
            rasterizer,
            depth_stencil: (depth_stencil, b.depth_stencil.1),
            blend: (blend, b.blend.1, b.blend.2),
            targets,
            depth,
            viewports: b.viewports.clone(),
            scissors: b.scissors.clone(),
            resources,
            data: Vec::new(),
            occlusion: None,
            call,
        };
        {
            let _record = crate::prof::span(crate::prof::Cat::Record);
            self.renderer.draw(&draw).map_err(|e| e.to_string())?;
        }
        if self.dump_recording() && matches!(call, Call::Draw { vertices, .. } if vertices <= 6) {
            let ps_hash = context
                .shader_code(b.stages[1].shader)
                .map_or(0, |c| dump::code_hash(c));
            let textures: Vec<String> = b.stages[1]
                .resources
                .iter()
                .enumerate()
                .filter_map(
                    |(slot, &id)| match context.object(dx::cotable::SRVIEW, id) {
                        Some(Object::ShaderResourceView(t, range)) => {
                            Some(format!("t{slot}=s{}/f{}/{range:?}", t.sid, t.format))
                        }
                        _ => None,
                    },
                )
                .collect();
            let constants: Vec<String> = b.stages[1]
                .constant_buffers
                .iter()
                .enumerate()
                .filter(|(_, cb)| cb.sid != INVALID_ID)
                .map(|(slot, cb)| format!("cb{slot}=s{}+{}:{}", cb.sid, cb.offset, cb.size))
                .collect();
            crate::log!(
                "dump draw: targets {:?} depth {:?} ps {ps_hash:016x} {} {} blend {:?}",
                targets
                    .iter()
                    .flatten()
                    .map(|t| (t.sid, t.format))
                    .collect::<Vec<_>>(),
                depth.map(|t| t.sid),
                textures.join(" "),
                constants.join(" "),
                blend.targets[0],
            );
        }
        self.dump_draw(targets.iter().flatten().map(|t| (t.sid, t.format)));
        for sid in targets.iter().flatten().chain(depth.iter()).map(|t| t.sid) {
            self.gpu_wrote(sid);
        }
        for sid in written {
            self.gpu_wrote(sid);
        }
        for (sid, ..) in stream_output.iter().flat_map(|s| s.targets).flatten() {
            self.gpu_wrote(sid);
        }
        Ok(())
    }

    /// The descriptors of `shader`, bound at `stage` (`SVGA3dShaderType`
    /// minus one) of `context`, their surfaces brought to the GPU; the
    /// surfaces of its UA views go to `written`.
    fn stage_bindings(
        &mut self,
        context: &Context,
        shader: &Shader,
        stage: usize,
        gb: &mut Gb,
        guest: &GuestMemory,
        written: &mut Vec<u32>,
    ) -> Result<Vec<svga_render::Bound>, String> {
        use svga_core::dx::UaRange;
        use svga_render::{Bound, Resource};
        let b = &context.bindings;
        let sb = &b.stages[stage];
        let mut resources = Vec::new();
        for binding in &shader.bindings {
            let resource = match binding.kind {
                BindingKind::ConstantBuffer { slot, .. } => {
                    match sb.constant_buffers.get(slot as usize) {
                        Some(cb) if cb.sid != INVALID_ID => {
                            self.sync_gpu(gb, guest, cb.sid)?;
                            Resource::Buffer {
                                sid: cb.sid,
                                offset: u64::from(cb.offset),
                                size: if cb.size == 0 {
                                    u64::MAX
                                } else {
                                    u64::from(cb.size)
                                },
                            }
                        }
                        _ => Resource::None,
                    }
                }
                BindingKind::Sampler { slot } => match sb
                    .samplers
                    .get(slot as usize)
                    .and_then(|&id| context.object(dx::cotable::SAMPLER, id))
                {
                    Some(Object::Sampler(state)) => Resource::Sampler(*state),
                    _ => Resource::None,
                },
                BindingKind::Resource { slot, .. } | BindingKind::BufferResource { slot } => {
                    match sb
                        .resources
                        .get(slot as usize)
                        .and_then(|&id| context.object(dx::cotable::SRVIEW, id))
                    {
                        Some(Object::ShaderResourceView(
                            t,
                            SrvRange::Texture {
                                most_detailed_mip,
                                mip_levels,
                                first_slice,
                                slices,
                            },
                        )) => {
                            self.sync_gpu(gb, guest, t.sid)?;
                            Resource::Texture {
                                sid: t.sid,
                                format: t.format,
                                first_mip: *most_detailed_mip,
                                mips: *mip_levels,
                                first_layer: *first_slice,
                                layers: *slices,
                            }
                        }
                        Some(Object::ShaderResourceView(t, SrvRange::Buffer { first, count }))
                            if matches!(binding.kind, BindingKind::Resource { .. }) =>
                        {
                            self.sync_gpu(gb, guest, t.sid)?;
                            let element = svga_proto::format::desc(t.format)
                                .map_or(0, |d| u64::from(d.bytes_per_block));
                            Resource::TexelBuffer {
                                sid: t.sid,
                                format: t.format,
                                offset: u64::from(*first).saturating_mul(element),
                                size: u64::from(*count).saturating_mul(element),
                            }
                        }
                        Some(Object::ShaderResourceView(
                            t,
                            SrvRange::Buffer { first, count }
                            | SrvRange::BufferEx { first, count, .. },
                        )) => {
                            self.sync_gpu(gb, guest, t.sid)?;
                            let (offset, size) = word_range(gb, t.sid, t.format, *first, *count);
                            Resource::StorageBuffer {
                                sid: t.sid,
                                offset,
                                size,
                            }
                        }
                        Some(_) => {
                            return Err(format!("shader resource t{slot}: wrong kind of view"));
                        }
                        None => Resource::None,
                    }
                }
                BindingKind::UaView { slot, dimension } => {
                    let views = if stage == 5 {
                        &b.cs_ua_views
                    } else {
                        &b.ua_views
                    };
                    match views
                        .get(slot as usize)
                        .and_then(|&id| context.object(dx::cotable::UAVIEW, id))
                    {
                        Some(Object::UaView(v)) => {
                            let sid = v.target.sid;
                            self.sync_gpu(gb, guest, sid)?;
                            written.push(sid);
                            match (v.range, dimension) {
                                (UaRange::Buffer { first, count, .. }, 0) => {
                                    let (offset, size) =
                                        word_range(gb, sid, v.target.format, first, count);
                                    Resource::StorageBuffer { sid, offset, size }
                                }
                                (UaRange::Buffer { first, count, .. }, _) => {
                                    let element = svga_proto::format::desc(v.target.format)
                                        .map_or(0, |d| u64::from(d.bytes_per_block));
                                    Resource::TexelBuffer {
                                        sid,
                                        format: v.target.format,
                                        offset: u64::from(first).saturating_mul(element),
                                        size: u64::from(count).saturating_mul(element),
                                    }
                                }
                                (
                                    UaRange::Texture {
                                        mip,
                                        first_slice,
                                        slices,
                                    },
                                    _,
                                ) => Resource::StorageImage {
                                    sid,
                                    format: v.target.format,
                                    mip,
                                    first_layer: first_slice,
                                    layers: slices,
                                },
                                (UaRange::Texture3d { mip, .. }, _) => Resource::StorageImage {
                                    sid,
                                    format: v.target.format,
                                    mip,
                                    first_layer: 0,
                                    layers: 1,
                                },
                            }
                        }
                        _ => Resource::None,
                    }
                }
            };
            resources.push(Bound {
                binding: binding.binding,
                resource,
            });
        }
        Ok(resources)
    }

    /// Runs the bound compute shader (`Dispatch`, `DispatchIndirect`).
    fn dispatch(
        &mut self,
        command: &DxCommand,
        context: &Context,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        use svga_render::{Dispatch, Groups};
        let groups = match *command {
            DxCommand::Dispatch { groups } => {
                if groups.contains(&0) {
                    return Ok(());
                }
                Groups::Count(groups)
            }
            DxCommand::DispatchIndirect { sid, offset } => {
                self.sync_gpu(gb, guest, sid)?;
                Groups::Indirect {
                    sid,
                    offset: u64::from(offset),
                }
            }
            _ => return Err("not a dispatch".into()),
        };
        let id = context.bindings.stages[5].shader;
        if id == INVALID_ID {
            return Err("dispatch without a compute shader".into());
        }
        let cs = self.shader(context, id, &Options::default())?;
        let mut written = Vec::new();
        let resources = self.stage_bindings(context, &cs, 5, gb, guest, &mut written)?;
        let dispatch = Dispatch {
            cs: &cs,
            resources,
            data: Vec::new(),
            groups,
        };
        self.renderer.dispatch(&dispatch).map_err(gpu_err)?;
        for sid in written {
            self.gpu_wrote(sid);
        }
        Ok(())
    }

    /// `ClearUnorderedAccessViewFloat` (`Ok`) and `...Uint` (`Err`).
    fn clear_ua_view(
        &mut self,
        context: &Context,
        view: u32,
        value: Result<[f32; 4], [u32; 4]>,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        use svga_core::dx::UaRange;
        let Some(Object::UaView(v)) = context.object(dx::cotable::UAVIEW, view) else {
            return Err(format!("UA view {view} is not defined"));
        };
        let sid = v.target.sid;
        self.sync_gpu(gb, guest, sid)?;
        match v.range {
            UaRange::Buffer { first, count, .. } => {
                // Whole words: raw and structured views, and 32-bit typed
                // ones (others are left as they are).
                let element =
                    svga_proto::format::desc(v.target.format).map_or(0, |d| d.bytes_per_block);
                if element != 0 && element != 4 {
                    crate::log!(
                        "GPU: clearing a typed buffer view of format {}: not yet",
                        v.target.format
                    );
                    return Ok(());
                }
                let word = match value {
                    Ok(f) => f[0].to_bits(),
                    Err(u) => u[0],
                };
                let (offset, size) = word_range(gb, sid, v.target.format, first, count);
                self.renderer
                    .fill_buffer(sid, offset, size, word)
                    .map_err(gpu_err)?;
            }
            UaRange::Texture {
                mip,
                first_slice,
                slices,
            } => self.clear_ua_image(sid, mip, first_slice, slices, value)?,
            UaRange::Texture3d { mip, .. } => self.clear_ua_image(sid, mip, 0, 1, value)?,
        }
        self.gpu_wrote(sid);
        Ok(())
    }

    fn clear_ua_image(
        &mut self,
        sid: u32,
        mip: u32,
        first: u32,
        count: u32,
        value: Result<[f32; 4], [u32; 4]>,
    ) -> Result<(), String> {
        match value {
            Ok(color) => self.renderer.clear_color(sid, mip, first, count, color),
            Err(bits) => self.renderer.clear_uint(sid, mip, first, count, bits),
        }
        .map_err(gpu_err)
    }

    /// Whether surface `sid` stays in its device copy: its format has no
    /// GPU image (planar YUV).
    fn host_only(&self, gb: &Gb, sid: u32) -> bool {
        gb.surface_shape(sid)
            .is_some_and(|(l, _)| !self.renderer.format_supported(l.format()))
    }

    /// A DX copy between surfaces that stay in their device copies: of
    /// subresources `(dest_sub, src_sub, box)`, or of every one.
    fn host_copy(
        &mut self,
        gb: &mut Gb,
        guest: &GuestMemory,
        dest: u32,
        src: u32,
        region: Option<(u32, u32, svga_core::gb::CopyBox)>,
    ) -> Result<(), String> {
        let mut copies = Vec::new();
        match region {
            Some((dest_sub, src_sub, copy)) => {
                let image = |sid, sub| gb.subresource(sid, sub);
                copies.push((image(src, src_sub)?, image(dest, dest_sub)?, copy));
            }
            None => {
                let (layout, _) = gb
                    .surface_shape(src)
                    .ok_or_else(|| format!("surface {src} is not defined"))?;
                for face in 0..layout.layers() {
                    for mipmap in 0..layout.mip_levels() {
                        let size = layout.mip_size(mipmap);
                        let image = |sid| svga_core::gb::ImageId { sid, face, mipmap };
                        let all = svga_core::gb::CopyBox {
                            dest: svga_core::layout::Box3 {
                                x: 0,
                                y: 0,
                                z: 0,
                                w: size.width,
                                h: size.height,
                                d: size.depth,
                            },
                            src: (0, 0, 0),
                        };
                        copies.push((image(src), image(dest), all));
                    }
                }
            }
        }
        for (src, dest, copy) in copies {
            let command = GbCommand::SurfaceCopy {
                src,
                dest,
                boxes: vec![copy],
            };
            self.before_gb(&command, gb, guest)?;
            gb.execute(command.clone(), guest)?;
            self.after_gb(&command);
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

    /// Submits recorded GPU work (end of a command buffer). When device
    /// copies use much of their budget, those of surfaces the GPU holds
    /// are freed: they come back from the GPU when needed.
    pub fn flush(&mut self, gb: &mut Gb, guest: &GuestMemory) {
        if let Err(e) = self.renderer.flush() {
            crate::log!("GPU: {e}");
        }
        let (busy, submissions) = self.renderer.gpu_time();
        crate::prof::add(crate::prof::Cat::Gpu, busy, submissions);
        self.poll_screen(gb);
        if let Err(e) = self.settle_queries(gb, guest) {
            crate::log!("GPU: occlusion queries: {e}");
        }
        if gb.host_pressure() {
            for (sid, newer) in &mut self.newer {
                if *newer != Newer::Host && self.renderer.copyable(*sid) {
                    gb.drop_host_copy(*sid);
                    *newer = Newer::Gpu;
                }
            }
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
            gpu.after_gb(&c);
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
                buffer_stride: 0,
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
        gpu.flush(&mut gb, &guest);
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
        // The Direct3D 11 driver's staging copy: copy, then read back by
        // the device itself.
        let green = DxCommand::ClearRenderTargetView {
            view: 0,
            color: [0.0, 1.0, 0.0, 1.0],
        };
        gpu.execute_dx(&green, context, &mut gb, &guest).unwrap();
        let staging = DxCommand::Staging {
            copy: Box::new(DxCommand::PredCopy { dest: 11, src: 10 }),
            readback: true,
        };
        gpu.execute_dx(&staging, context, &mut gb, &guest).unwrap();
        assert!(ram.read(4096, &mut pixels));
        assert!(
            pixels.chunks(4).all(|p| p == [0, 0xff, 0, 0xff]),
            "green after a staging copy: {pixels:?}"
        );
        // Destroying a surface frees its GPU copy.
        run_gb(&mut gpu, &mut gb, GbCommand::DestroySurface { sid: 10 });
        assert!(!gpu.newer.contains_key(&10));
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod draw_tests {
    use super::*;
    use crate::dxctx::{DxContexts, Limits, Outcome};
    use crate::gb::Limits as GbLimits;
    use crate::shm::SharedMem;
    use std::os::fd::OwnedFd;
    use svga_core::dx::{ShaderStage, VertexBuffer, ViewTarget, Viewport};
    use svga_core::gb::SurfaceDef;
    use svga_core::layout::Size3;
    use svga_proto::{format, mobfmt};

    const RAM_BASE: u64 = 0x10_0000;

    /// The device's pieces, run as `device.rs` runs them.
    struct Rig {
        gpu: Gpu,
        ram: SharedMem,
        guest: GuestMemory,
        gb: Gb,
        dx: DxContexts,
    }

    impl Rig {
        fn new() -> Option<Self> {
            let gpu = match Gpu::new() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("no GPU for this test: {e}");
                    return None;
                }
            };
            let ram = SharedMem::new("ram", 0x10000).unwrap();
            let fd: OwnedFd = ram.fd().try_clone_to_owned().unwrap();
            let mut guest = GuestMemory::default();
            guest.add(RAM_BASE, 0x10000, Some(fd), 0, true).unwrap();
            Some(Self {
                gpu,
                ram,
                guest,
                gb: Gb::new(GbLimits {
                    mob_pages: 16,
                    mob_size: 1 << 16,
                    surface_bytes: 1 << 20,
                    max_width: 64,
                    max_height: 64,
                }),
                dx: DxContexts::new(Limits {
                    contexts: 4,
                    code_bytes: 1 << 16,
                    shader_bytes: 1 << 12,
                }),
            })
        }

        fn gb(&mut self, c: GbCommand) {
            self.gpu.before_gb(&c, &mut self.gb, &self.guest).unwrap();
            self.gb.execute(c.clone(), &self.guest).unwrap();
            self.gpu.after_gb(&c);
        }

        fn dx(&mut self, c: DxCommand) {
            let current = (!matches!(
                c,
                DxCommand::DefineContext { .. }
                    | DxCommand::SetCoTable { .. }
                    | DxCommand::BindShader { .. }
            ))
            .then_some(0);
            if self
                .gpu
                .update_on_gpu(&c, &mut self.gb, &self.guest)
                .unwrap()
            {
                return;
            }
            self.gpu.before_dx(&c, &mut self.gb, &self.guest).unwrap();
            let outcome = self
                .dx
                .execute(c.clone(), current, &mut self.gb, &self.guest)
                .unwrap();
            self.gpu.after_dx(&c);
            if outcome == Outcome::Deferred && runs_on_gpu(&c) {
                let context = self.dx.context(0).unwrap();
                self.gpu
                    .execute_dx(&c, context, &mut self.gb, &self.guest)
                    .unwrap_or_else(|e| panic!("{c:?}: {e}"));
            }
        }

        /// MOB `id` of 4 KiB on RAM page `page`.
        fn mob(&mut self, id: u32, page: u64) {
            self.gb(GbCommand::DefineMob {
                id,
                pt_depth: mobfmt::RANGE,
                base: RAM_BASE / 4096 + page,
                size: 4096,
            });
        }

        fn surface(&mut self, sid: u32, format: u32, width: u32, height: u32, mob: u32) {
            self.gb(GbCommand::DefineSurface(SurfaceDef {
                sid,
                flags: 0,
                format,
                mip_levels: 1,
                samples: 0,
                size: Size3 {
                    width,
                    height,
                    depth: 1,
                },
                array_size: 0,
                buffer_stride: 0,
            }));
            self.gb(GbCommand::BindSurface { sid, mob, pitch: 0 });
        }
    }

    /// A partial update of a buffer the GPU holds goes from the MOB to the
    /// GPU alone: the rest of the GPU copy stays, even where the MOB
    /// changed outside the range.
    #[test]
    fn buffer_updates_go_straight_to_the_gpu() {
        let Some(mut r) = Rig::new() else { return };
        r.ram.write(4096, &[1; 256]);
        r.mob(1, 1);
        r.surface(20, format::BUFFER, 256, 1, 1);
        // Equal copies, as after a first update: the device's goes.
        r.gb.host_copy(&r.guest, 20).unwrap();
        r.gpu.sync_gpu(&mut r.gb, &r.guest, 20).unwrap();
        assert!(r.gb.has_host_copy(20));
        r.dx(DxCommand::DefineContext { cid: 0 });
        r.ram.write(4096, &[3; 64]);
        r.ram.write(4096 + 64, &[2; 64]);
        r.dx(DxCommand::BufferUpdate {
            sid: 20,
            x: 64,
            width: 64,
        });
        assert!(!r.gb.has_host_copy(20), "no device copy on the way");
        r.gb(GbCommand::ReadbackSurface { sid: 20 });
        let mut back = [0u8; 256];
        assert!(r.ram.read(4096, &mut back));
        assert_eq!(back[..64], [1; 64], "outside the range: the GPU's bytes");
        assert_eq!(back[64..128], [2; 64], "the range");
        assert_eq!(back[128..], [1; 128]);
    }

    /// Video blits: an RGB picture scaled on the GPU over a background,
    /// then an NV12 one converted on the way (a 2x2 BT.601 red square).
    #[test]
    fn video_blits_scale_fill_and_convert() {
        use svga_core::gb::{VideoBlt, VideoView};
        let Some(mut r) = Rig::new() else { return };
        // Source: 4x4 BGRA, left half red, right half blue.
        let mut texels = Vec::new();
        for i in 0..16 {
            let px: [u8; 4] = if i % 4 < 2 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            };
            texels.extend_from_slice(&px);
        }
        r.ram.write(4096, &texels);
        r.mob(1, 1);
        r.surface(10, format::B8G8R8A8_UNORM, 4, 4, 1);
        r.mob(2, 2);
        r.surface(11, format::B8G8R8A8_UNORM, 8, 8, 2);
        let view = |sid| VideoView {
            sid,
            dimension: 3,
            mip: 0,
            first_slice: 0,
            slices: 1,
        };
        let mut blit = VideoBlt {
            output: view(11),
            background_ycbcr: false,
            background: [0.0f32, 1.0, 0.0, 1.0].map(f32::to_bits),
            target: [0, 0, 8, 8],
            input: view(10),
            source: [0, 0, 4, 4],
            dest: [0, 0, 8, 6],
            rotation: 0,
            frame_format: 0,
        };
        let pixel = |r: &mut Rig, x: usize, y: usize| {
            r.gb(GbCommand::ReadbackSurface { sid: 11 });
            let mut px = [0u8; 4];
            r.ram.read(2 * 4096 + (y * 8 + x) * 4, &mut px);
            px
        };
        assert!(
            r.gpu
                .execute_gb(&GbCommand::VideoBlt(blit), &mut r.gb, &r.guest)
                .unwrap()
        );
        assert_eq!(pixel(&mut r, 1, 1), [0, 0, 255, 255], "red, scaled");
        assert_eq!(pixel(&mut r, 6, 4), [255, 0, 0, 255], "blue, scaled");
        assert_eq!(pixel(&mut r, 3, 7), [0, 255, 0, 255], "background below");
        // NV12 4x4: Y, then interleaved U and V (2 rows of 4 bytes).
        let mut nv12 = vec![81u8; 16];
        nv12.extend_from_slice(&[90, 240, 90, 240, 90, 240, 90, 240]);
        r.ram.write(3 * 4096, &nv12);
        r.mob(3, 3);
        r.surface(12, format::NV12, 4, 4, 3);
        blit.input = view(12);
        blit.dest = [0, 0, 8, 8];
        blit.rotation = 2;
        assert!(
            r.gpu
                .execute_gb(&GbCommand::VideoBlt(blit), &mut r.gb, &r.guest)
                .unwrap()
        );
        let [b, g, red, a] = pixel(&mut r, 4, 4);
        assert!(red > 250 && g < 5 && b < 5 && a == 255, "{red} {g} {b} {a}");
        // Formats the GPU cannot hold are copied in their device copies.
        r.mob(4, 4);
        r.surface(13, format::NV12, 4, 4, 4);
        r.gpu.host_copy(&mut r.gb, &r.guest, 13, 12, None).unwrap();
        r.gb(GbCommand::ReadbackSurface { sid: 13 });
        let mut back = vec![0u8; 24];
        r.ram.read(4 * 4096, &mut back);
        assert_eq!(back, nv12);
    }

    fn operand(kind: u32, comps_select: u32, dims: u32) -> u32 {
        comps_select | (kind << 12) | (dims << 20)
    }

    fn instr(opcode: u32, rest: &[u32]) -> Vec<u32> {
        let mut v = vec![opcode | (((rest.len() + 1) as u32) << 24)];
        v.extend_from_slice(rest);
        v
    }

    fn program(stage: u32, body: &[Vec<u32>]) -> Vec<u8> {
        let mut words = vec![(stage << 16) | 0x40, 0];
        for b in body {
            words.extend(b);
        }
        words[1] = words.len() as u32;
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn a_triangle_drawn_by_a_context_reaches_the_guest() {
        use svga_shader::opcode as op;
        let Some(mut rig) = Rig::new() else { return };
        // Pages: 1 target, 2-4 tables, 5 shader code, 6 vertices.
        for id in 1..=6 {
            rig.mob(id, u64::from(id));
        }
        rig.surface(10, format::B8G8R8A8_UNORM, 4, 4, 1);
        // Position (float4), color (R8G8B8A8_UNORM): the top-left half,
        // green.
        let mut vertices = Vec::new();
        for (x, y) in [(-1.0f32, 1.0f32), (1.0, 1.0), (-1.0, -1.0)] {
            for f in [x, y, 0.0, 1.0] {
                vertices.extend(f.to_le_bytes());
            }
            vertices.extend([0, 255, 0, 255]);
        }
        assert!(rig.ram.write(6 * 4096, &vertices));
        rig.surface(12, format::BUFFER, vertices.len() as u32, 1, 6);
        // VS: o0 (position) = v0, o1 = v1. PS: o0 = v1.
        let all = 2 | (0xf << 4);
        let swizzle = 2 | (1 << 2) | (0xe4 << 4);
        let o = operand(2, all, 1);
        let v = operand(1, swizzle, 1);
        let vs = program(
            1,
            &[
                instr(op::DCL_INPUT, &[operand(1, all, 1), 0]),
                instr(op::DCL_INPUT, &[operand(1, all, 1), 1]),
                instr(op::DCL_OUTPUT_SIV, &[o, 0, 1]),
                instr(op::DCL_OUTPUT, &[o, 1]),
                instr(op::MOV, &[o, 0, v, 0]),
                instr(op::MOV, &[o, 1, v, 1]),
                instr(op::RET, &[]),
            ],
        );
        let ps = program(
            0,
            &[
                instr(op::DCL_INPUT_PS | (2 << 11), &[operand(1, all, 1), 1]),
                instr(op::DCL_OUTPUT, &[o, 0]),
                instr(op::MOV, &[o, 0, v, 1]),
                instr(op::RET, &[]),
            ],
        );
        assert!(rig.ram.write(5 * 4096, &vs));
        assert!(rig.ram.write(5 * 4096 + 512, &ps));
        rig.dx(DxCommand::DefineContext { cid: 0 });
        for (mob, table) in [
            (2, dx::cotable::RTVIEW),
            (3, dx::cotable::ELEMENTLAYOUT),
            (4, dx::cotable::DXSHADER),
        ] {
            rig.dx(DxCommand::SetCoTable {
                cid: 0,
                mob,
                table,
                valid_size: 0,
                grow: false,
            });
        }
        rig.dx(DxCommand::DefineRenderTargetView {
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
        });
        let element = |offset, format, register| InputElement {
            slot: 0,
            offset,
            format,
            per_instance: false,
            step_rate: 0,
            register,
        };
        rig.dx(DxCommand::DefineElementLayout {
            id: 0,
            elements: vec![
                element(0, format::R32G32B32A32_FLOAT, 0),
                element(16, format::R8G8B8A8_UNORM, 1),
            ],
        });
        for (id, stage, code, offset) in [
            (0, ShaderStage::Vertex, &vs, 0),
            (1, ShaderStage::Pixel, &ps, 512),
        ] {
            rig.dx(DxCommand::DefineShader {
                id,
                stage,
                size: code.len() as u32,
            });
            rig.dx(DxCommand::BindShader {
                cid: 0,
                shader: id,
                mob: 5,
                offset,
            });
            rig.dx(DxCommand::SetShader { stage, shader: id });
        }
        rig.dx(DxCommand::SetInputLayout { layout: 0 });
        rig.dx(DxCommand::SetVertexBuffers {
            start: 0,
            buffers: vec![VertexBuffer {
                sid: 12,
                stride: 20,
                offset: 0,
                size: svga_core::dx::WHOLE_SIZE,
            }],
        });
        rig.dx(DxCommand::SetTopology { topology: 1 });
        rig.dx(DxCommand::SetRenderTargets {
            depth_stencil: INVALID_ID,
            targets: vec![0],
        });
        rig.dx(DxCommand::SetViewports {
            viewports: vec![Viewport {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        });
        rig.dx(DxCommand::Draw {
            vertex_count: 3,
            start_vertex: 0,
        });
        rig.gpu.flush(&mut rig.gb, &rig.guest);
        // The guest reads its target back.
        rig.gb(GbCommand::ReadbackSurface { sid: 10 });
        let mut pixels = [0u8; 64];
        assert!(rig.ram.read(4096, &mut pixels));
        let pixel = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..][..4];
        let name = rig.gpu.name().to_string();
        assert_eq!(pixel(0, 0), [0, 255, 0, 255], "green on {name}");
        assert_eq!(pixel(2, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(3, 3), [0; 4], "black");
    }
}
