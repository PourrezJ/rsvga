//! Legacy context (Direct3D 9) clears, draws, blits and mipmaps on the GPU.
//!
//! A draw takes its whole state from the context: Direct3D 9 shaders
//! translated with what the render states add (alpha test, fog, flat
//! shading), vertex inputs matched to the draw's declarations by semantic,
//! render states turned into Direct3D 10 state blocks for the renderer,
//! texture stage states into samplers. Direct3D 9 puts pixel centers on
//! integer coordinates: the viewport moves half a pixel right and down.

use std::sync::Arc;

use svga_core::dx::{
    BlendState, DepthStencilState, RasterizerState, SamplerState, ScissorRect, StencilFace,
    Viewport,
};
use svga_core::gb::ImageId;
use svga_core::legacy::{LegacyCommand, PrimitiveRange, VertexDecl};
use svga_proto::{format as f, surface_flag};
use svga_render::{At, Bound, Call, Draw, Resource, Target, VertexAttribute, VertexBuffer};
use svga_shader::BindingKind;
use svga_shader::d3d9::{self, Fog, Translated, constants};

use super::{Gpu, gpu_err};
use crate::ctx9::{Context9, INVALID_ID, Legacy, rs, ts};
use crate::gb::Gb;
use crate::guest::GuestMemory;

/// A translated Direct3D 9 shader, or why there is none.
pub(super) type Translation9 = Result<Arc<Translated>, String>;

/// Shader code as a cache key: by address, not contents (hashing whole
/// programs at every draw cost more than the rest of the lookup). The key
/// holds the code, so the address cannot be reused while it is cached.
#[derive(Clone, Debug)]
pub(super) struct Code(pub(super) Arc<[u32]>);

impl PartialEq for Code {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Code {}

impl std::hash::Hash for Code {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0).cast::<u32>() as usize).hash(state);
        self.0.len().hash(state);
    }
}

/// Vertex texture samplers start at this texture stage.
const VERTEX_SAMPLER_STAGE: usize = 16;

fn color(argb: u32) -> [f32; 4] {
    let c = |shift: u32| ((argb >> shift) & 0xff) as f32 / 255.0;
    [c(16), c(8), c(0), c(24)]
}

fn target(gb: &Gb, image: ImageId) -> Option<Target> {
    if image.sid == INVALID_ID {
        return None;
    }
    let (format, ..) = gb.surface_info(image.sid)?;
    Some(Target {
        sid: image.sid,
        format,
        mip: image.mipmap,
        first_layer: image.face,
    })
}

/// The format of a vertex element (`SVGA3dDeclType`), and whether it is
/// fetched as (signed) integers.
fn decl_format(decl_type: u32) -> Result<(u32, bool, bool), String> {
    Ok(match decl_type {
        0 => (f::R32_FLOAT, false, false),
        1 => (f::R32G32_FLOAT, false, false),
        2 => (f::R32G32B32_FLOAT, false, false),
        3 => (f::R32G32B32A32_FLOAT, false, false),
        // D3DCOLOR: B, G, R, A bytes.
        4 => (f::B8G8R8A8_UNORM, false, false),
        5 => (f::R8G8B8A8_UINT, true, false),
        6 => (f::R16G16_SINT, true, true),
        7 => (f::R16G16B16A16_SINT, true, true),
        8 => (f::R8G8B8A8_UNORM, false, false),
        9 => (f::R16G16_SNORM, false, false),
        10 => (f::R16G16B16A16_SNORM, false, false),
        11 => (f::R16G16_UNORM, false, false),
        12 => (f::R16G16B16A16_UNORM, false, false),
        13 => (f::R10G10B10A2_UINT, true, false),
        15 => (f::R16G16_FLOAT, false, false),
        16 => (f::R16G16B16A16_FLOAT, false, false),
        t => return Err(format!("vertex element type {t} is not supported yet")),
    })
}

/// The sRGB variant of a texture or render target format (the format
/// itself when there is none).
fn srgb(format: u32) -> u32 {
    match format {
        f::A8R8G8B8 | f::B8G8R8A8_UNORM => f::B8G8R8A8_UNORM_SRGB,
        f::X8R8G8B8 | f::B8G8R8X8_UNORM => f::B8G8R8X8_UNORM_SRGB,
        f::R8G8B8A8_UNORM => f::R8G8B8A8_UNORM_SRGB,
        f::DXT1 | f::BC1_UNORM => f::BC1_UNORM_SRGB,
        f::DXT2 | f::DXT3 | f::BC2_UNORM => f::BC2_UNORM_SRGB,
        f::DXT4 | f::DXT5 | f::BC3_UNORM => f::BC3_UNORM_SRGB,
        other => other,
    }
}

/// Vertices of `count` primitives of `SVGA3dPrimitiveType` `primitive`.
fn vertex_count(primitive: u32, count: u32) -> Option<u32> {
    Some(match primitive {
        1 => count.checked_mul(3)?,
        2 => count,
        3 => count.checked_mul(2)?,
        4 => count.checked_add(1)?,
        5 | 6 => count.checked_add(2)?,
        _ => return None,
    })
}

/// `SVGA3dBlendOp` as `D3D10_BLEND`.
fn blend_factor(op: u32) -> u8 {
    match op {
        1..=11 => op as u8,
        12..=19 => (op + 2) as u8,
        _ => 2,
    }
}

/// A 1-based enumeration (`SVGA3dCmpFunc`, `SVGA3dStencilOp`...) within
/// `1..=max`, else `default`.
fn ranged(value: u32, max: u32, default: u8) -> u8 {
    if (1..=max).contains(&value) {
        value as u8
    } else {
        default
    }
}

fn rasterizer(c: &Context9, depth_format: Option<u32>) -> RasterizerState {
    let mut r = svga_render::default_rasterizer();
    r.fill_mode = if c.state(rs::FILLMODE) & 0xffff == 2 {
        2
    } else {
        3
    };
    r.cull_mode = match c.state(rs::CULLMODE) {
        2 => 2,
        3 => 3,
        _ => 1,
    };
    r.front_ccw = c.state(rs::FRONTWINDING) == 2;
    // Direct3D 9 gives the bias in depth units: in units of the format's
    // resolution for the renderer.
    let resolution = match depth_format {
        Some(f::Z_D16 | f::D16_UNORM) => 65535.0,
        _ => 16_777_215.0,
    };
    r.depth_bias = (c.state_f32(rs::DEPTHBIAS) * resolution) as i32;
    r.slope_scaled_depth_bias = c.state_f32(rs::SLOPESCALEDEPTHBIAS);
    r.scissor = c.state(rs::SCISSORTESTENABLE) != 0;
    r.multisample = u8::from(c.state(rs::MULTISAMPLEANTIALIAS) != 0);
    r
}

fn depth_stencil(c: &Context9, has_depth: bool) -> DepthStencilState {
    let face = |func, fail, zfail, pass| StencilFace {
        fail_op: ranged(c.state(fail), 8, 1),
        depth_fail_op: ranged(c.state(zfail), 8, 1),
        pass_op: ranged(c.state(pass), 8, 1),
        func: ranged(c.state(func), 8, 8),
    };
    let front = face(
        rs::STENCILFUNC,
        rs::STENCILFAIL,
        rs::STENCILZFAIL,
        rs::STENCILPASS,
    );
    let back = if c.state(rs::STENCILENABLE2SIDED) != 0 {
        face(
            rs::CCWSTENCILFUNC,
            rs::CCWSTENCILFAIL,
            rs::CCWSTENCILZFAIL,
            rs::CCWSTENCILPASS,
        )
    } else {
        front
    };
    DepthStencilState {
        depth_enable: has_depth && c.state(rs::ZENABLE) != 0,
        depth_write_mask: u8::from(c.state(rs::ZWRITEENABLE) != 0),
        depth_func: ranged(c.state(rs::ZFUNC), 8, 4),
        stencil_enable: has_depth && c.state(rs::STENCILENABLE) != 0,
        front_enable: true,
        back_enable: true,
        stencil_read_mask: c.state(rs::STENCILMASK) as u8,
        stencil_write_mask: c.state(rs::STENCILWRITEMASK) as u8,
        front,
        back,
    }
}

fn blend(c: &Context9) -> BlendState {
    let mut b = svga_render::default_blend();
    let enable = c.state(rs::BLENDENABLE) != 0;
    let src = blend_factor(c.state(rs::SRCBLEND));
    let dest = blend_factor(c.state(rs::DSTBLEND));
    let op = ranged(c.state(rs::BLENDEQUATION), 5, 1);
    let (src_alpha, dest_alpha, op_alpha) = if c.state(rs::SEPARATEALPHABLENDENABLE) != 0 {
        (
            blend_factor(c.state(rs::SRCBLENDALPHA)),
            blend_factor(c.state(rs::DSTBLENDALPHA)),
            ranged(c.state(rs::BLENDEQUATIONALPHA), 5, 1),
        )
    } else {
        (src, dest, op)
    };
    b.independent = true;
    for (n, t) in b.targets.iter_mut().enumerate().take(4) {
        t.enable = enable;
        t.src = src;
        t.dest = dest;
        t.op = op;
        t.src_alpha = src_alpha;
        t.dest_alpha = dest_alpha;
        t.op_alpha = op_alpha;
        let mask = if n == 0 {
            rs::COLORWRITEENABLE
        } else {
            rs::COLORWRITEENABLE1 + n - 1
        };
        t.write_mask = (c.state(mask) & 0xf) as u8;
    }
    b
}

fn sampler(c: &Context9, stage: usize) -> SamplerState {
    let t = |name| c.texture_state(stage, name);
    let linear = |v: u32| v >= 2;
    let mut filter = 0;
    if t(ts::MIPFILTER) >= 2 {
        filter |= 1;
    }
    if linear(t(ts::MAGFILTER)) {
        filter |= 1 << 2;
    }
    if linear(t(ts::MINFILTER)) {
        filter |= 1 << 4;
    }
    if t(ts::MINFILTER) == 3 || t(ts::MAGFILTER) == 3 {
        filter |= 1 << 6;
    }
    let address = |name| match t(name) {
        a @ 1..=5 => a as u8,
        6 => 3,
        _ => 1,
    };
    // Mip levels from TEXTURE_MIPMAP_LEVEL on; none but that one without a
    // mip filter.
    let min_lod = t(ts::TEXTURE_MIPMAP_LEVEL).min(15) as f32;
    SamplerState {
        filter,
        address: [
            address(ts::ADDRESSU),
            address(ts::ADDRESSV),
            address(ts::ADDRESSW),
        ],
        mip_lod_bias: f32::from_bits(t(ts::TEXTURE_LOD_BIAS)),
        max_anisotropy: t(ts::TEXTURE_ANISOTROPIC_LEVEL).clamp(1, 16) as u8,
        comparison: 0,
        border_color: color(t(ts::BORDERCOLOR)),
        min_lod,
        max_lod: if t(ts::MIPFILTER) == 0 {
            min_lod
        } else {
            1000.0
        },
    }
}

/// Whether surface `sid` is a depth texture that Direct3D 9 samples as a
/// shadow map (comparison): D16 and D24 formats. DF16, DF24 and INTZ
/// read the depth instead.
fn is_shadow_map(gb: &Gb, sid: u32) -> bool {
    gb.surface_info(sid).is_some_and(|(format, ..)| {
        matches!(format, f::Z_D16 | f::Z_D24S8 | f::Z_D24X8 | f::Z_D15S1)
    })
}

/// Shadow map samplers of a stage, as [`d3d9::Options::shadow`] bits.
fn shadow_samplers(c: &Context9, gb: &Gb, vertex: bool) -> u16 {
    (0..16u32)
        .filter(|&n| {
            let unit = texture_stage(vertex, n);
            unit < crate::ctx9::STAGES && is_shadow_map(gb, c.texture_state(unit, ts::BIND_TEXTURE))
        })
        .fold(0, |bits, n| bits | (1 << n))
}

/// The texture stage of sampler `num` of a stage.
fn texture_stage(vertex: bool, num: u32) -> usize {
    if vertex {
        VERTEX_SAMPLER_STAGE + num as usize
    } else {
        num as usize
    }
}

fn put(block: &mut [u32], index: u32, value: [f32; 4]) {
    let at = index as usize * 4;
    for (w, v) in block[at..at + 4].iter_mut().zip(value) {
        *w = v.to_bits();
    }
}

impl Gpu {
    fn translate9(&mut self, code: &Arc<[u32]>, options: &d3d9::Options) -> Translation9 {
        let _span = crate::prof::span(crate::prof::Cat::Translate);
        let key = (Code(Arc::clone(code)), options.clone());
        if let Some(t) = self.shaders9.get(&key) {
            return t.clone();
        }
        let result = d3d9::translate_words(code, options)
            .map(Arc::new)
            .map_err(|e| e.to_string());
        if self.shaders9.len() >= super::MAX_SHADER_VARIANTS {
            self.shaders9.clear();
        }
        self.shaders9.insert(key, result.clone());
        result
    }

    /// Executes a legacy command that runs on the GPU.
    pub fn execute_legacy(
        &mut self,
        command: &LegacyCommand,
        legacy: &Legacy,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        match command {
            LegacyCommand::Clear {
                cid,
                flags,
                color: argb,
                depth,
                stencil,
                rects,
            } => {
                let c = legacy
                    .context(*cid)
                    .ok_or_else(|| format!("context {cid} is not defined"))?;
                let mut targets = [None; 8];
                for (slot, t) in targets.iter_mut().enumerate() {
                    *t = target(gb, c.targets[2 + slot]);
                }
                let depth_target = target(gb, c.targets[0]);
                let rects: Vec<(u32, u32, u32, u32)> = if rects.is_empty() {
                    let v = c.viewport;
                    vec![(v.x, v.y, v.width, v.height)]
                } else {
                    rects
                        .iter()
                        .map(|r| (r.x, r.y, r.width, r.height))
                        .collect()
                };
                for t in targets.iter().flatten().chain(depth_target.iter()) {
                    self.sync_gpu(gb, guest, t.sid)?;
                }
                let color_value = (flags & 9 != 0).then(|| color(*argb));
                let depth_value = (flags & 2 != 0).then_some(*depth);
                let stencil_value = (flags & 4 != 0).then_some(*stencil as u8);
                self.renderer
                    .clear_rects(
                        &targets,
                        if depth_value.is_some() || stencil_value.is_some() {
                            depth_target
                        } else {
                            None
                        },
                        color_value,
                        (depth_value, stencil_value),
                        &rects,
                    )
                    .map_err(gpu_err)?;
                for t in targets.iter().flatten().chain(depth_target.iter()) {
                    self.gpu_wrote(t.sid);
                }
                Ok(())
            }
            LegacyCommand::DrawPrimitives {
                cid,
                decls,
                ranges,
                divisors,
            } => {
                let c = legacy
                    .context(*cid)
                    .ok_or_else(|| format!("context {cid} is not defined"))?;
                self.draw9(*cid, c, legacy, decls, ranges, divisors, gb, guest)
            }
            LegacyCommand::StretchBlt {
                src,
                dest,
                src_box,
                dest_box,
                mode,
            } => {
                self.sync_gpu(gb, guest, src.sid)?;
                self.sync_gpu(gb, guest, dest.sid)?;
                self.renderer
                    .blit(
                        dest.sid,
                        At {
                            layer: dest.face,
                            mip: dest.mipmap,
                            offset: [dest_box.x, dest_box.y, dest_box.z],
                        },
                        [dest_box.w, dest_box.h, dest_box.d.max(1)],
                        src.sid,
                        At {
                            layer: src.face,
                            mip: src.mipmap,
                            offset: [src_box.x, src_box.y, src_box.z],
                        },
                        [src_box.w, src_box.h, src_box.d.max(1)],
                        *mode == 1,
                    )
                    .map_err(gpu_err)?;
                self.gpu_wrote(dest.sid);
                Ok(())
            }
            LegacyCommand::BeginQuery { cid, .. } => {
                // A query begun again drops what it counted.
                self.query9 = Some((*cid, self.renderer.occlusion_used(), 0));
                Ok(())
            }
            LegacyCommand::EndQuery {
                cid, mob, offset, ..
            } => match self.query9.take() {
                Some((query_cid, first, carried)) if query_cid == *cid => {
                    // Counted once its draws ran: pending until then.
                    let end = self.renderer.occlusion_used();
                    self.ended9.push((*mob, *offset, first, end, carried));
                    crate::ctx9::write_query_state(gb, guest, *mob, *offset, 0, 0)
                }
                other => {
                    self.query9 = other;
                    crate::ctx9::write_query(gb, guest, *mob, *offset, 0)
                }
            },
            LegacyCommand::WaitForQuery { mob, offset, .. } => {
                if self.ended9.iter().any(|q| (q.0, q.1) == (*mob, *offset)) {
                    self.settle_queries(gb, guest)?;
                }
                Ok(())
            }
            LegacyCommand::GenerateMipmaps { sid, .. } => {
                let (layout, _) = gb
                    .surface_shape(*sid)
                    .ok_or_else(|| format!("surface {sid} is not defined"))?;
                self.sync_gpu(gb, guest, *sid)?;
                self.renderer
                    .generate_mips(*sid, 0, u32::MAX, 0, layout.layers())
                    .map_err(gpu_err)?;
                self.gpu_wrote(*sid);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Reads the occlusion slots (after the GPU ran their draws): ended
    /// queries get their results in the guest's MOBs, the running one keeps
    /// its partial count.
    pub(super) fn settle_queries(&mut self, gb: &Gb, guest: &GuestMemory) -> Result<(), String> {
        if self.ended9.is_empty() && self.query9.is_none() {
            return Ok(());
        }
        let results = self.renderer.occlusion_read().map_err(gpu_err)?;
        let sum = |first: u32, end: u32| {
            results
                .get(first as usize..(end as usize).min(results.len()))
                .map_or(0u64, |r| r.iter().fold(0u64, |s, x| s.saturating_add(*x)))
        };
        let mut outcome = Ok(());
        for (mob, offset, first, end, carried) in std::mem::take(&mut self.ended9) {
            let total = carried.saturating_add(sum(first, end));
            let result = u32::try_from(total).unwrap_or(u32::MAX);
            if let Err(e) = crate::ctx9::write_query(gb, guest, mob, offset, result) {
                outcome = Err(e);
            }
        }
        if let Some((cid, first, carried)) = self.query9 {
            let all = u32::try_from(results.len()).unwrap_or(u32::MAX);
            self.query9 = Some((cid, 0, carried.saturating_add(sum(first, all))));
        }
        outcome
    }

    fn shader_code(legacy: &Legacy, shid: u32) -> Result<Arc<[u32]>, String> {
        legacy
            .shader(shid)
            .ok_or_else(|| format!("shader {shid} is not defined"))?
            .code
            .clone()
            .ok_or_else(|| format!("shader {shid} has no code"))
    }

    #[allow(clippy::too_many_arguments)]
    fn draw9(
        &mut self,
        cid: u32,
        c: &Context9,
        legacy: &Legacy,
        decls: &[VertexDecl],
        ranges: &[PrimitiveRange],
        divisors: &[u32],
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let mut targets = [None; 8];
        for (slot, t) in targets.iter_mut().enumerate() {
            *t = target(gb, c.targets[2 + slot]);
        }
        let depth = target(gb, c.targets[0]);
        if targets.iter().all(Option::is_none) && depth.is_none() {
            return Ok(());
        }
        // An output gamma of 2.2: sRGB writes (blending stays linear).
        if c.state_f32(rs::OUTPUTGAMMA) > 1.5 {
            for t in targets.iter_mut().flatten() {
                t.format = srgb(t.format);
            }
        }
        if c.state(rs::CULLMODE) == 4 {
            // Both faces culled.
            return Ok(());
        }
        // The vertex shader: the device's own for pretransformed vertices
        // (they skip vertex processing even with a shader bound, as the
        // driver leaves its fixed-function one), else the bound one.
        let pretransformed = decls.iter().any(|d| d.usage == 9);
        let vs_code: Arc<[u32]> = if pretransformed {
            let semantics: Vec<(u32, u32)> =
                decls.iter().map(|d| (d.usage, d.usage_index)).collect();
            Arc::clone(
                self.programs9
                    .entry(semantics.clone())
                    .or_insert_with(|| d3d9::pretransformed_vs(&semantics).into()),
            )
        } else if c.shaders[0] == INVALID_ID {
            return Err("draw without a vertex shader or pretransformed vertices".into());
        } else {
            Self::shader_code(legacy, c.shaders[0])?
        };
        let plain = self.translate9(&vs_code, &d3d9::Options::default())?;
        // Vertex inputs, by semantic; integer elements are converted.
        let mut vs_options = d3d9::Options::default();
        let mut attributes = Vec::new();
        let mut buffers: Vec<VertexBuffer> = Vec::new();
        let mut missing = Vec::new();
        let mut instances = 1;
        for d in divisors {
            if d & (1 << 30) != 0 {
                instances = (d & 0x3fff_ffff).max(1);
            }
        }
        for &(reg, usage, index) in &plain.inputs {
            let Some((n, decl)) = decls
                .iter()
                .enumerate()
                .find(|(_, d)| d.usage == usage && d.usage_index == index)
            else {
                missing.push(reg);
                continue;
            };
            let (format, int, signed) = decl_format(decl.decl_type)?;
            let bit = 1u16 << (reg & 15);
            if int {
                vs_options.int_inputs |= bit;
                if signed {
                    vs_options.signed_inputs |= bit;
                }
            }
            let binding = n as u32;
            attributes.push(VertexAttribute {
                location: reg,
                binding,
                format,
                offset: 0,
            });
            if !buffers.iter().any(|b| b.binding == binding) {
                if decl.array.sid != INVALID_ID {
                    self.sync_gpu(gb, guest, decl.array.sid)?;
                }
                buffers.push(VertexBuffer {
                    binding,
                    sid: (decl.array.sid != INVALID_ID).then_some(decl.array.sid),
                    offset: u64::from(decl.array.offset),
                    size: u64::MAX,
                    stride: decl.array.stride,
                    per_instance: divisors.get(n).is_some_and(|d| d & (1 << 31) != 0),
                });
            }
        }
        if !missing.is_empty() {
            let free = buffers.iter().map(|b| b.binding + 1).max().unwrap_or(0);
            for location in missing {
                attributes.push(VertexAttribute {
                    location,
                    binding: free,
                    format: f::R32G32B32A32_FLOAT,
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
        // Points (all of a draw's ranges are taken as one kind) get their
        // size, and sprites their texture coordinates; user clip planes do
        // not apply to pretransformed vertices.
        let points = ranges.first().is_some_and(|r| r.primitive == 2);
        vs_options.points = points;
        vs_options.shadow = shadow_samplers(c, gb, true);
        if !pretransformed {
            vs_options.clip_planes = (c.state(rs::CLIPPLANEENABLE) & 0x3f) as u8;
        }
        let vs = if vs_options == d3d9::Options::default() {
            plain
        } else {
            self.translate9(&vs_code, &vs_options)?
        };
        // The pixel shader, with alpha test, fog and flat shading.
        let ps_code: Arc<[u32]> = match c.shaders[1] {
            INVALID_ID => Arc::clone(
                self.programs9
                    .entry(Vec::new())
                    .or_insert_with(|| d3d9::default_ps().into()),
            ),
            id => Self::shader_code(legacy, id)?,
        };
        let mut ps_options = d3d9::Options::default();
        if c.state(rs::ALPHATESTENABLE) != 0 {
            ps_options.alpha_test = ranged(c.state(rs::ALPHAFUNC), 8, 8);
        }
        if c.state(rs::FOGENABLE) != 0 {
            // `SVGA3dFogMode`: function (16 bits), type, base.
            let mode = c.state(rs::FOGMODE);
            ps_options.fog = match ((mode >> 16) & 0xff, mode & 0xffff) {
                (2, 1) => Fog::Exp,
                (2, 2) => Fog::Exp2,
                (2, 3) => Fog::Linear,
                (1, _) => Fog::Vertex,
                _ => Fog::None,
            };
        }
        ps_options.flat = c.state(rs::SHADEMODE) == 1;
        ps_options.point_sprite = points && c.state(rs::POINTSPRITEENABLE) != 0;
        ps_options.shadow = shadow_samplers(c, gb, false);
        for (stage, ty) in ps_options.sampler_types.iter_mut().enumerate() {
            let sid = c.texture_state(stage, ts::BIND_TEXTURE);
            if let Some((_, flags, depth, _)) = gb.surface_info(sid) {
                *ty = if flags & surface_flag::CUBEMAP != 0 {
                    3
                } else if depth > 1 {
                    4
                } else {
                    2
                };
            }
        }
        let ps = self.translate9(&ps_code, &ps_options)?;
        // Constants, textures and samplers of both stages.
        let mut data = Vec::new();
        let mut resources = Vec::new();
        let vp = c.viewport;
        for (stage, t) in [(0usize, &vs), (1, &ps)] {
            let mut block = c.constants[stage].clone();
            for (index, bits) in &t.defs {
                let at = *index as usize * 4;
                if let Some(slot) = block.get_mut(at..at + 4) {
                    slot.copy_from_slice(bits);
                }
            }
            put(
                &mut block,
                constants::ALPHA_FOG,
                [
                    c.state_f32(rs::ALPHAREF),
                    c.state_f32(rs::FOGSTART),
                    c.state_f32(rs::FOGEND),
                    c.state_f32(rs::FOGDENSITY),
                ],
            );
            put(
                &mut block,
                constants::FOG_COLOR,
                color(c.state(rs::FOGCOLOR)),
            );
            put(
                &mut block,
                constants::POINT,
                [
                    c.state_f32(rs::POINTSIZE),
                    c.state_f32(rs::POINTSIZEMIN),
                    c.state_f32(rs::POINTSIZEMAX),
                    0.0,
                ],
            );
            for (n, plane) in c.clip_planes.iter().enumerate() {
                put(&mut block, constants::CLIP_PLANES + n as u32, *plane);
            }
            for unit in 0..8usize {
                let t = |name| f32::from_bits(c.texture_state(unit, name));
                let m = ts::BUMPENVMAT00;
                put(
                    &mut block,
                    constants::BUMP_ENV + unit as u32,
                    [t(m), t(m + 1), t(m + 2), t(m + 3)],
                );
                put(
                    &mut block,
                    constants::BUMP_LUMINANCE + unit as u32,
                    [t(ts::BUMPENVLSCALE), t(ts::BUMPENVLOFFSET), 0.0, 0.0],
                );
            }
            if pretransformed && stage == 0 {
                let (w, h) = (vp.width.max(1) as f32, vp.height.max(1) as f32);
                put(
                    &mut block,
                    constants::VIEWPORT,
                    [
                        2.0 / w,
                        -2.0 / h,
                        -1.0 - 2.0 * vp.x as f32 / w,
                        1.0 + 2.0 * vp.y as f32 / h,
                    ],
                );
                let (zmin, zmax) = c.zrange;
                let range = zmax - zmin;
                let (scale, offset) = if range.abs() > f32::EPSILON {
                    (1.0 / range, -zmin / range)
                } else {
                    (0.0, 0.0)
                };
                put(&mut block, constants::DEPTH, [scale, offset, 0.0, 0.0]);
            }
            data.push(
                block
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .collect::<Vec<u8>>(),
            );
            let n = data.len() - 1;
            for b in &t.shader.bindings {
                let resource = match b.kind {
                    BindingKind::ConstantBuffer { .. } => Resource::Data(n),
                    BindingKind::Sampler { slot } => {
                        let unit = texture_stage(stage == 0, slot);
                        let mut state = sampler(c, unit);
                        if is_shadow_map(gb, c.texture_state(unit, ts::BIND_TEXTURE)) {
                            // Lit where z <= the stored depth.
                            state.filter |= 1 << 7;
                            state.comparison = 4;
                        }
                        Resource::Sampler(state)
                    }
                    BindingKind::Resource { slot, .. } => {
                        let unit = texture_stage(stage == 0, slot);
                        let sid = c.texture_state(unit, ts::BIND_TEXTURE);
                        match gb.surface_info(sid) {
                            Some((surface_format, _, _, layers)) => {
                                self.sync_gpu(gb, guest, sid)?;
                                // A sampler gamma of 2.2: sRGB texels.
                                let gamma = f32::from_bits(c.texture_state(unit, ts::GAMMA));
                                let format = if gamma > 1.5 {
                                    srgb(surface_format)
                                } else {
                                    surface_format
                                };
                                Resource::Texture {
                                    sid,
                                    format,
                                    first_mip: 0,
                                    mips: u32::MAX,
                                    first_layer: 0,
                                    layers,
                                }
                            }
                            None => Resource::None,
                        }
                    }
                    // Direct3D 9 shaders have no buffer views.
                    BindingKind::BufferResource { .. } | BindingKind::UaView { .. } => {
                        Resource::None
                    }
                };
                resources.push(Bound {
                    binding: b.binding,
                    resource,
                });
            }
        }
        for t in targets.iter().flatten().chain(depth.iter()) {
            self.sync_gpu(gb, guest, t.sid)?;
        }
        let viewports = vec![Viewport {
            x: vp.x as f32 + 0.5,
            y: vp.y as f32 + 0.5,
            width: vp.width as f32,
            height: vp.height as f32,
            min_depth: c.zrange.0,
            max_depth: c.zrange.1,
        }];
        let s = c.scissor;
        let scissors = vec![ScissorRect {
            left: s.x as i32,
            top: s.y as i32,
            right: s.x.saturating_add(s.width) as i32,
            bottom: s.y.saturating_add(s.height) as i32,
        }];
        let rasterizer = rasterizer(c, depth.map(|d| d.format));
        let depth_stencil = depth_stencil(c, depth.is_some());
        let blend = blend(c);
        // One draw, its call changed for each range.
        let mut draw = Draw {
            vs: &vs.shader,
            ps: Some(&ps.shader),
            gs: None,
            attributes,
            buffers,
            index: None,
            topology: 0,
            rasterizer,
            depth_stencil: (depth_stencil, c.state(rs::STENCILREF) & 0xff),
            blend: (
                blend,
                color(c.state(rs::BLENDCOLOR)),
                c.state(rs::MULTISAMPLEMASK),
            ),
            targets,
            depth,
            viewports,
            scissors,
            resources,
            data,
            occlusion: None,
            call: Call::Draw {
                vertices: 0,
                instances: 0,
                first_vertex: 0,
                first_instance: 0,
            },
        };
        for r in ranges {
            let Some(vertices) = vertex_count(r.primitive, r.count) else {
                return Err(format!("primitive type {}", r.primitive));
            };
            if vertices == 0 {
                continue;
            }
            let (call, index) = if r.index_width != 0 {
                if r.indices.sid == INVALID_ID {
                    continue;
                }
                self.sync_gpu(gb, guest, r.indices.sid)?;
                let format = if r.index_width == 4 {
                    f::R32_UINT
                } else {
                    f::R16_UINT
                };
                (
                    Call::Indexed {
                        indices: vertices,
                        instances,
                        first_index: 0,
                        base_vertex: r.index_bias,
                        first_instance: 0,
                    },
                    Some((r.indices.sid, format, u64::from(r.indices.offset))),
                )
            } else {
                (
                    Call::Draw {
                        vertices,
                        instances,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    None,
                )
            };
            // Counted by the context's occlusion query, if one runs.
            let occlusion = match self.query9 {
                Some((query_cid, ..)) if query_cid == cid => {
                    match self.renderer.occlusion_slot().map_err(gpu_err)? {
                        Some(slot) => Some(slot),
                        None => {
                            // All slots in use: read them, then go on.
                            self.settle_queries(gb, guest)?;
                            self.renderer.occlusion_slot().map_err(gpu_err)?
                        }
                    }
                }
                _ => None,
            };
            draw.index = index;
            draw.topology = r.primitive;
            draw.occlusion = occlusion;
            draw.call = call;
            let _span = crate::prof::span(crate::prof::Cat::Record);
            self.renderer.draw(&draw).map_err(gpu_err)?;
        }
        for t in targets.iter().flatten().chain(depth.iter()) {
            self.gpu_wrote(t.sid);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::gb::Limits as GbLimits;
    use crate::shm::SharedMem;
    use std::os::fd::OwnedFd;
    use svga_core::command::Rect;
    use svga_core::gb::{GbCommand, SurfaceDef};
    use svga_core::layout::Size3;
    use svga_core::legacy::{Array, Stage, TextureState};
    use svga_proto::mobfmt;

    const RAM_BASE: u64 = 0x10_0000;

    struct Rig {
        gpu: Gpu,
        ram: SharedMem,
        guest: GuestMemory,
        gb: Gb,
        legacy: Legacy,
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
                legacy: Legacy::default(),
            })
        }

        fn gb(&mut self, c: GbCommand) {
            self.gpu.before_gb(&c, &mut self.gb, &self.guest).unwrap();
            self.gb.execute(c.clone(), &self.guest).unwrap();
            self.gpu.after_gb(&c);
        }

        fn nine(&mut self, c: LegacyCommand) {
            let outcome = self
                .legacy
                .execute(&c, &mut self.gb, &self.guest, true)
                .unwrap_or_else(|e| panic!("{c:?}: {e}"));
            if outcome == crate::ctx9::Outcome::Gpu {
                self.gpu
                    .execute_legacy(&c, &self.legacy, &mut self.gb, &self.guest)
                    .unwrap_or_else(|e| panic!("{c:?}: {e}"));
            }
        }

        /// MOB `id` of 4 KiB on RAM page `page`, holding `bytes`.
        fn mob(&mut self, id: u32, page: u64, bytes: &[u8]) {
            self.ram.write(page as usize * 4096, bytes);
            self.gb(GbCommand::DefineMob {
                id,
                pt_depth: mobfmt::RANGE,
                base: RAM_BASE / 4096 + page,
                size: 4096,
            });
        }

        fn surface(&mut self, sid: u32, format: u32, size: (u32, u32), mob: u32) {
            self.gb(GbCommand::DefineSurface(SurfaceDef {
                sid,
                flags: 0,
                format,
                mip_levels: 1,
                samples: 0,
                size: Size3 {
                    width: size.0,
                    height: size.1,
                    depth: 1,
                },
                array_size: 0,
                buffer_stride: 0,
            }));
            self.gb(GbCommand::BindSurface { sid, mob, pitch: 0 });
        }
    }

    /// A present from a surface drawn on the GPU stays on the GPU (no
    /// readback): scaled 2x, red and blue swapped by the formats.
    #[test]
    fn a_present_of_a_gpu_surface_runs_on_the_gpu() {
        let Some(mut r) = Rig::new() else { return };
        r.mob(1, 1, &[0; 4096]);
        r.surface(10, f::A8R8G8B8, (4, 4), 1);
        r.mob(2, 2, &[0; 4096]);
        r.surface(11, f::R8G8B8A8_UNORM, (8, 8), 2);
        r.nine(LegacyCommand::DefineContext { cid: 0 });
        r.nine(LegacyCommand::SetRenderTarget {
            cid: 0,
            slot: 2,
            image: ImageId {
                sid: 10,
                face: 0,
                mipmap: 0,
            },
        });
        r.nine(LegacyCommand::Clear {
            cid: 0,
            flags: 1,
            color: 0xff20_4080,
            depth: 1.0,
            stencil: 0,
            rects: vec![Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            }],
        });
        let area = |n| svga_core::layout::Box3 {
            x: 0,
            y: 0,
            z: 0,
            w: n,
            h: n,
            d: 1,
        };
        let present = GbCommand::PresentBlt {
            src: 10,
            src_sub: 0,
            dest: 11,
            dest_sub: 0,
            src_box: area(4),
            dest_box: area(8),
            mode: 0,
        };
        assert!(r.gpu.execute_gb(&present, &mut r.gb, &r.guest).unwrap());
        r.gb(GbCommand::ReadbackSurface { sid: 11 });
        let mut out = [0u8; 4];
        r.ram.read(2 * 4096 + (8 * 7 + 7) * 4, &mut out);
        assert_eq!(out, [0x20, 0x40, 0x80, 0xff], "R, G, B, A bytes");
    }

    /// A present into the surface a screen target shows stays on the GPU
    /// too: the window gets the pixels once the GPU is done, and a later
    /// update from the device copy lands after them.
    #[test]
    fn the_screen_is_read_from_the_gpu_without_waiting() {
        let Some(mut r) = Rig::new() else { return };
        r.mob(1, 1, &[0; 4096]);
        r.surface(10, f::A8R8G8B8, (4, 4), 1);
        r.mob(2, 2, &[0; 4096]);
        r.surface(11, f::A8R8G8B8, (8, 8), 2);
        r.gb(GbCommand::DefineScreenTarget {
            stid: 0,
            width: 8,
            height: 8,
            root: (0, 0),
            flags: svga_proto::stflag::PRIMARY,
            dpi: 0,
        });
        let shown = ImageId {
            sid: 11,
            face: 0,
            mipmap: 0,
        };
        let bind = GbCommand::BindScreenTarget {
            stid: 0,
            image: shown,
        };
        assert!(r.gpu.execute_gb(&bind, &mut r.gb, &r.guest).unwrap());
        r.nine(LegacyCommand::DefineContext { cid: 0 });
        r.nine(LegacyCommand::SetRenderTarget {
            cid: 0,
            slot: 2,
            image: ImageId {
                sid: 10,
                face: 0,
                mipmap: 0,
            },
        });
        r.nine(LegacyCommand::Clear {
            cid: 0,
            flags: 1,
            color: 0xff20_4080,
            depth: 1.0,
            stencil: 0,
            rects: vec![Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            }],
        });
        let area = |n| svga_core::layout::Box3 {
            x: 0,
            y: 0,
            z: 0,
            w: n,
            h: n,
            d: 1,
        };
        let present = GbCommand::PresentBlt {
            src: 10,
            src_sub: 0,
            dest: 11,
            dest_sub: 0,
            src_box: area(4),
            dest_box: area(8),
            mode: 0,
        };
        assert!(r.gpu.execute_gb(&present, &mut r.gb, &r.guest).unwrap());
        let mut frame = crate::display::Frame::default();
        r.gb.capture(&mut frame);
        assert_eq!(frame.pixels[63], 0, "not read yet");
        r.gpu.flush(&mut r.gb, &r.guest);
        for _ in 0..1000 {
            r.gpu.poll_screen(&mut r.gb);
            r.gb.capture(&mut frame);
            if frame.pixels[63] != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(frame.pixels[63], 0x0020_4080, "read from the GPU");
        // Pending GPU reads land before an update from the device copy.
        assert!(r.gpu.execute_gb(&present, &mut r.gb, &r.guest).unwrap());
        r.gb(GbCommand::UpdateSurface { sid: 11 });
        let all = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        };
        let update = GbCommand::UpdateScreenTarget { stid: 0, rect: all };
        assert!(r.gpu.execute_gb(&update, &mut r.gb, &r.guest).unwrap());
        r.gpu.flush(&mut r.gb, &r.guest);
        r.gb.capture(&mut frame);
        assert_eq!(frame.pixels[63], 0, "the MOB's zeros, shown last");
        // Updates of a target that is not shown are skipped.
        let other = GbCommand::UpdateScreenTarget { stid: 5, rect: all };
        assert!(r.gpu.execute_gb(&other, &mut r.gb, &r.guest).unwrap());
    }

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn floats(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// The driver's textured quad of pretransformed vertices: a 2x2
    /// texture over a 16x16 target, sampled by a ps_2_0 `texld`, counted by
    /// an occlusion query.
    #[test]
    fn a_textured_quad_of_pretransformed_vertices_reaches_the_guest() {
        let Some(mut r) = Rig::new() else { return };
        // Texture: red, green / blue, white (A8R8G8B8: B, G, R, A bytes).
        r.mob(
            2,
            2,
            &bytes(&[0xffff_0000, 0xff00_ff00, 0xff00_00ff, 0xffff_ffff]),
        );
        r.surface(11, f::A8R8G8B8, (2, 2), 2);
        r.gb(GbCommand::UpdateSurface { sid: 11 });
        r.mob(1, 1, &[0; 4096]);
        r.surface(10, f::A8R8G8B8, (16, 16), 1);
        // x, y, z, rhw, u, v.
        let quad = floats(&[
            0.0, 0.0, 0.5, 1.0, 0.0, 0.0, //
            16.0, 0.0, 0.5, 1.0, 1.0, 0.0, //
            0.0, 16.0, 0.5, 1.0, 0.0, 1.0, //
            16.0, 16.0, 0.5, 1.0, 1.0, 1.0,
        ]);
        r.mob(3, 3, &quad);
        r.gb(GbCommand::DefineSurface(SurfaceDef {
            sid: 12,
            flags: 0,
            format: f::BUFFER,
            mip_levels: 1,
            samples: 0,
            size: Size3 {
                width: 4096,
                height: 1,
                depth: 1,
            },
            array_size: 0,
            buffer_stride: 0,
        }));
        r.gb(GbCommand::BindSurface {
            sid: 12,
            mob: 3,
            pitch: 0,
        });
        r.gb(GbCommand::UpdateSurface { sid: 12 });
        // ps_2_0: dcl t0; dcl_2d s0; texld r0, t0, s0; mov oC0, r0.
        let ps = [
            0xffff_0200,
            0x0200_001f,
            0x8000_0000,
            0xb00f_0000,
            0x0200_001f,
            0x9000_0000,
            0xa00f_0800,
            0x0300_0042,
            0x800f_0000,
            0xb0e4_0000,
            0xa0e4_0800,
            0x0200_0001,
            0x800f_0800,
            0x80e4_0000,
            0x0000_ffff,
        ];
        r.mob(4, 4, &bytes(&ps));
        let size = (ps.len() * 4) as u32;
        r.nine(LegacyCommand::DefineContext { cid: 0 });
        r.nine(LegacyCommand::SetRenderStates {
            cid: 0,
            states: vec![(rs::CULLMODE as u32, 3)],
        });
        r.nine(LegacyCommand::SetRenderTarget {
            cid: 0,
            slot: 2,
            image: ImageId {
                sid: 10,
                face: 0,
                mipmap: 0,
            },
        });
        let full = Rect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        };
        r.nine(LegacyCommand::SetViewport { cid: 0, rect: full });
        r.nine(LegacyCommand::SetZRange {
            cid: 0,
            min: 0.0,
            max: 1.0,
        });
        r.nine(LegacyCommand::DefineShader {
            shid: 1,
            stage: Stage::Pixel,
            size,
        });
        r.nine(LegacyCommand::BindShader {
            shid: 1,
            mob: 4,
            offset: 0,
        });
        r.nine(LegacyCommand::SetShader {
            cid: 0,
            stage: Stage::Pixel,
            shid: 1,
        });
        r.nine(LegacyCommand::SetTextureStates {
            cid: 0,
            states: vec![TextureState {
                stage: 0,
                name: ts::BIND_TEXTURE as u32,
                value: 11,
            }],
        });
        r.nine(LegacyCommand::Clear {
            cid: 0,
            flags: 1,
            color: 0xff12_3456,
            depth: 1.0,
            stencil: 0,
            rects: vec![full],
        });
        let decl = |decl_type, usage, offset| VertexDecl {
            decl_type,
            method: 0,
            usage,
            usage_index: 0,
            array: Array {
                sid: 12,
                offset,
                stride: 24,
            },
            range_hint: (0, 3),
        };
        r.mob(5, 5, &[0; 16]);
        r.nine(LegacyCommand::BeginQuery { cid: 0, kind: 0 });
        r.nine(LegacyCommand::DrawPrimitives {
            cid: 0,
            decls: vec![decl(3, 9, 0), decl(1, 5, 16)],
            ranges: vec![PrimitiveRange {
                primitive: 5,
                count: 2,
                indices: Array {
                    sid: INVALID_ID,
                    offset: 0,
                    stride: 1,
                },
                index_width: 0,
                index_bias: 0,
            }],
            divisors: vec![1, 1],
        });
        r.nine(LegacyCommand::EndQuery {
            cid: 0,
            kind: 0,
            mob: 5,
            offset: 4,
        });
        let mut query = [0u8; 16];
        r.ram.read(5 * 4096, &mut query);
        // SVGA3dQueryResult: size, state (pending), samples.
        assert_eq!(&query[4..16], &bytes(&[12, 0, 0])[..], "query pending");
        r.nine(LegacyCommand::WaitForQuery {
            cid: 0,
            kind: 0,
            mob: 5,
            offset: 4,
        });
        r.ram.read(5 * 4096, &mut query);
        // Succeeded, with the samples drawn.
        assert_eq!(&query[4..16], &bytes(&[12, 1, 256])[..], "occlusion query");
        r.gb(GbCommand::ReadbackSurface { sid: 10 });
        let mut out = [0u8; 1024];
        r.ram.read(4096, &mut out);
        let at = |x: usize, y: usize| {
            let i = (y * 16 + x) * 4;
            u32::from_le_bytes([out[i], out[i + 1], out[i + 2], out[i + 3]])
        };
        assert_eq!(at(2, 2), 0xffff_0000, "red quadrant");
        assert_eq!(at(13, 2), 0xff00_ff00, "green quadrant");
        assert_eq!(at(2, 13), 0xff00_00ff, "blue quadrant");
        assert_eq!(at(13, 13), 0xffff_ffff, "white quadrant");
    }
}
