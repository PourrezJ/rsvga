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
//! On the GPU: draws, clears and copies between surfaces. A draw gathers
//! the context's bindings ([`Context::bindings`]) into a
//! [`svga_render::Draw`]; its shaders are translated again when the
//! vertex formats or render target views hold integers (the SPIR-V types
//! must match), and those translations are cached. Geometry, hull and
//! domain shaders, stream output, and raw or structured buffer views are
//! not drawn yet.
//!
//! Surfaces whose bytes cannot go to the GPU as they are (24-bit depth on
//! AMD, multisampled images) live on the GPU only: their device copies
//! are neither uploaded nor read back.

use std::collections::HashMap;
use std::sync::Arc;

use svga_core::cotable::Object;
use svga_core::dx::{DxCommand, InputElement, RtvRange, SrvRange};
use svga_core::gb::GbCommand;
use svga_proto::{INVALID_ID, cond_bind, dx};
use svga_render::{At, Renderer};
use svga_shader::{BindingKind, Options, Scalar, Shader};

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
    /// Shaders translated with other than the default options.
    shaders: HashMap<(Arc<[u8]>, Options), Translation>,
}

/// A translated shader, or why there is none.
type Translation = Result<Arc<Shader>, String>;

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
            | DxCommand::ClearDepthStencilView { .. }
            | DxCommand::Draw { .. }
            | DxCommand::DrawIndexed { .. }
            | DxCommand::DrawInstanced { .. }
            | DxCommand::DrawIndexedInstanced { .. }
            | DxCommand::PredCopy { .. }
            | DxCommand::PredCopyRegion { .. }
            | DxCommand::BufferCopy { .. }
            | DxCommand::TransferFromBuffer { .. }
            | DxCommand::SurfaceCopyAndReadback { .. }
    )
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
            newer: HashMap::new(),
            shaders: HashMap::new(),
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
        self.newer.insert(sid, Newer::Same);
        if !self.renderer.copyable(sid) {
            crate::log!("GPU: surface {sid} cannot be read back (format)");
            return Ok(());
        }
        let host = gb.host_copy(guest, sid)?;
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
            | DxCommand::DrawIndexedInstanced { .. } => self.draw(command, context, gb, guest)?,
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
        let key = (Arc::clone(code), options.clone());
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
        use svga_render::{Bound, Call, Draw, Resource, Target, VertexAttribute, VertexBuffer};
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
            _ => return Err("not a draw".into()),
        };
        // What is not drawn yet.
        for (stage, name) in [(2, "geometry"), (3, "hull"), (4, "domain")] {
            if b.stages[stage].shader != INVALID_ID {
                return Err(format!("{name} shaders are not supported yet"));
            }
        }
        if b.so_targets.iter().any(|t| t.sid != INVALID_ID) {
            return Err("stream output is not supported yet".into());
        }
        let vs_id = b.stages[0].shader;
        if vs_id == INVALID_ID {
            return Err("draw without a vertex shader".into());
        }
        if b.topology == 0 {
            return Ok(());
        }
        // Input layout, and the types of the vertex shader's inputs.
        let elements: Vec<InputElement> = match b.input_layout {
            INVALID_ID => Vec::new(),
            id => match context.object(dx::cotable::ELEMENTLAYOUT, id) {
                Some(Object::ElementLayout(e)) => e.clone(),
                _ => return Err(format!("input layout {id} is not defined")),
            },
        };
        let mut vs_options = Options::default();
        for e in &elements {
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
        let vs = self.shader(context, vs_id, &vs_options)?;
        let ps = match b.stages[1].shader {
            INVALID_ID => None,
            id => Some(self.shader(context, id, &ps_options)?),
        };
        // Vertex attributes the shader reads, and their buffer slots.
        let mut attributes: Vec<VertexAttribute> = Vec::new();
        let mut slots: Vec<(u32, bool)> = Vec::new();
        for e in &elements {
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
                stride: 0,
                per_instance: false,
            });
        }
        let index = match call {
            Call::Indexed { .. } => {
                let (sid, format, offset) = b.index_buffer;
                if sid == INVALID_ID {
                    return Ok(());
                }
                self.sync_gpu(gb, guest, sid)?;
                Some((sid, format, u64::from(offset)))
            }
            Call::Draw { .. } => None,
        };
        // Descriptors the shaders use.
        let mut resources = Vec::new();
        for (shader, stage) in [(Some(&vs), 0usize), (ps.as_ref(), 1)] {
            let Some(shader) = shader else { continue };
            let sb = &b.stages[stage];
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
                    BindingKind::Resource { slot, .. } => match sb
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
                        Some(Object::ShaderResourceView(t, SrvRange::Buffer { first, count })) => {
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
                        Some(_) => {
                            return Err(
                                "raw and structured buffer views are not supported yet".into()
                            );
                        }
                        None => Resource::None,
                    },
                };
                resources.push(Bound {
                    binding: binding.binding,
                    resource,
                });
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
            call,
        };
        self.renderer.draw(&draw).map_err(|e| e.to_string())?;
        for sid in targets.iter().flatten().chain(depth.iter()).map(|t| t.sid) {
            self.gpu_wrote(sid);
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
            self.gpu.after_gb(&c, &self.gb);
        }

        fn dx(&mut self, c: DxCommand) {
            let current = (!matches!(
                c,
                DxCommand::DefineContext { .. }
                    | DxCommand::SetCoTable { .. }
                    | DxCommand::BindShader { .. }
            ))
            .then_some(0);
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
            }));
            self.gb(GbCommand::BindSurface { sid, mob, pitch: 0 });
        }
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
        rig.gpu.flush();
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
