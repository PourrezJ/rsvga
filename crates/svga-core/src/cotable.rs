//! Context object tables (COTables): the objects of a DX context, in the
//! entry formats of `svga3d_dx.h` (`SVGACOTableDX*Entry`).
//!
//! The guest gives each table a MOB; an object's ID is its entry index, so
//! the MOB size bounds the IDs. The device keeps the objects itself and
//! only uses the entries to save a table into its MOB
//! (`DX_READBACK_COTABLE`) or to load one from it (`DX_SET_COTABLE` with
//! valid contents). [`encode`] and [`decode`] convert between the two; an
//! entry decodes through the very parser of its `DEFINE` command, so both
//! paths validate the same way.

use svga_proto::{cmd3d, dx};

use crate::dx::{
    BlendState, DepthStencilState, DepthStencilView, DxCommand, DxError, InputElement,
    RasterizerState, RtvRange, SamplerState, ShaderStage, SrvRange, StreamOutputDecl, UaRange,
    ViewTarget, parse,
};

/// Bytes of one entry of table `table` (`SVGACOTableType`).
pub const fn entry_size(table: u32) -> Option<usize> {
    Some(match table {
        dx::cotable::RTVIEW | dx::cotable::DSVIEW | dx::cotable::SRVIEW => 32,
        dx::cotable::ELEMENTLAYOUT => 1024,
        dx::cotable::BLENDSTATE => 128,
        dx::cotable::DEPTHSTENCIL => 16,
        dx::cotable::RASTERIZERSTATE => 32,
        dx::cotable::SAMPLER => 64,
        dx::cotable::STREAMOUTPUT => 2048,
        dx::cotable::DXQUERY => 16,
        dx::cotable::DXSHADER => 32,
        dx::cotable::UAVIEW => 64,
        _ => return None,
    })
}

/// A query (`SVGACOTableDXQueryEntry`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Query {
    /// `SVGA3dQueryType`.
    pub kind: u32,
    /// `SVGADX_QDSTATE_*`.
    pub state: u8,
    /// `SVGA3D_DXQUERY_FLAG_*`.
    pub flags: u32,
    /// MOB its result goes to (`SVGA3D_INVALID_ID`: none).
    pub mob: u32,
    /// Byte offset of the result in the MOB.
    pub offset: u32,
}

/// A shader (`SVGACOTableDXShaderEntry`): its code lives in a MOB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shader {
    /// Stage.
    pub stage: ShaderStage,
    /// Bytes of code (with signatures).
    pub size: u32,
    /// Byte offset of the code in the MOB.
    pub offset: u32,
    /// MOB holding the code (`SVGA3D_INVALID_ID`: not bound yet).
    pub mob: u32,
}

/// A stream-output object (without the VMware "with MOB" extension).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamOutput {
    /// Declarations.
    pub decls: Vec<StreamOutputDecl>,
    /// Bytes per vertex of each target.
    pub strides: [u32; dx::MAX_SOTARGETS],
    /// Stream sent to the rasterizer.
    pub rasterized_stream: u32,
}

/// An unordered-access view (`SVGACOTableDXUAViewEntry`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UaView {
    /// Surface, format and dimension.
    pub target: ViewTarget,
    /// What it covers.
    pub range: UaRange,
    /// Hidden counter of append and counter buffer views.
    pub structure_count: u32,
}

/// An object of a context table.
#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    /// `SVGA_COTABLE_RTVIEW`.
    RenderTargetView(ViewTarget, RtvRange),
    /// `SVGA_COTABLE_DSVIEW`.
    DepthStencilView(DepthStencilView),
    /// `SVGA_COTABLE_SRVIEW`.
    ShaderResourceView(ViewTarget, SrvRange),
    /// `SVGA_COTABLE_ELEMENTLAYOUT`.
    ElementLayout(Vec<InputElement>),
    /// `SVGA_COTABLE_BLENDSTATE`.
    BlendState(BlendState),
    /// `SVGA_COTABLE_DEPTHSTENCIL`.
    DepthStencilState(DepthStencilState),
    /// `SVGA_COTABLE_RASTERIZERSTATE`.
    RasterizerState(RasterizerState),
    /// `SVGA_COTABLE_SAMPLER`.
    Sampler(SamplerState),
    /// `SVGA_COTABLE_STREAMOUTPUT`.
    StreamOutput(StreamOutput),
    /// `SVGA_COTABLE_DXQUERY`.
    Query(Query),
    /// `SVGA_COTABLE_DXSHADER`.
    Shader(Shader),
    /// `SVGA_COTABLE_UAVIEW`.
    UaView(UaView),
}

impl Object {
    /// The table this object lives in.
    pub const fn table(&self) -> u32 {
        match self {
            Self::RenderTargetView(..) => dx::cotable::RTVIEW,
            Self::DepthStencilView(_) => dx::cotable::DSVIEW,
            Self::ShaderResourceView(..) => dx::cotable::SRVIEW,
            Self::ElementLayout(_) => dx::cotable::ELEMENTLAYOUT,
            Self::BlendState(_) => dx::cotable::BLENDSTATE,
            Self::DepthStencilState(_) => dx::cotable::DEPTHSTENCIL,
            Self::RasterizerState(_) => dx::cotable::RASTERIZERSTATE,
            Self::Sampler(_) => dx::cotable::SAMPLER,
            Self::StreamOutput(_) => dx::cotable::STREAMOUTPUT,
            Self::Query(_) => dx::cotable::DXQUERY,
            Self::Shader(_) => dx::cotable::DXSHADER,
            Self::UaView(_) => dx::cotable::UAVIEW,
        }
    }

    /// The object a `DEFINE` command creates, with its ID; `None` for other
    /// commands.
    pub fn defined_by(command: &DxCommand) -> Option<(u32, Self)> {
        use DxCommand as C;
        Some(match command {
            C::DefineRenderTargetView { id, target, range } => {
                (*id, Self::RenderTargetView(*target, *range))
            }
            C::DefineDepthStencilView { id, view } => (*id, Self::DepthStencilView(*view)),
            C::DefineShaderResourceView { id, target, range } => {
                (*id, Self::ShaderResourceView(*target, *range))
            }
            C::DefineElementLayout { id, elements } => (*id, Self::ElementLayout(elements.clone())),
            C::DefineBlendState { id, state } => (*id, Self::BlendState(*state)),
            C::DefineDepthStencilState { id, state } => (*id, Self::DepthStencilState(*state)),
            C::DefineRasterizerState { id, state } => (*id, Self::RasterizerState(*state)),
            C::DefineSamplerState { id, state } => (*id, Self::Sampler(*state)),
            C::DefineStreamOutput {
                id,
                decls,
                strides,
                rasterized_stream,
            } => (
                *id,
                Self::StreamOutput(StreamOutput {
                    decls: decls.clone(),
                    strides: *strides,
                    rasterized_stream: *rasterized_stream,
                }),
            ),
            C::DefineQuery { id, kind, flags } => (
                *id,
                Self::Query(Query {
                    kind: *kind,
                    state: QUERY_IDLE,
                    flags: *flags,
                    mob: svga_proto::INVALID_ID,
                    offset: 0,
                }),
            ),
            C::DefineShader { id, stage, size } => (
                *id,
                Self::Shader(Shader {
                    stage: *stage,
                    size: *size,
                    offset: 0,
                    mob: svga_proto::INVALID_ID,
                }),
            ),
            C::DefineUaView { id, target, range } => (
                *id,
                Self::UaView(UaView {
                    target: *target,
                    range: *range,
                    structure_count: 0,
                }),
            ),
            _ => return None,
        })
    }
}

/// `SVGADX_QDSTATE_IDLE`.
pub const QUERY_IDLE: u8 = 0;
/// `SVGADX_QDSTATE_ACTIVE`.
pub const QUERY_ACTIVE: u8 = 1;
/// `SVGADX_QDSTATE_FINISHED`.
pub const QUERY_FINISHED: u8 = 3;

/// Little-endian packed fields, padded to the entry size at the end.
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.u32(v.to_bits())
    }
    fn target(&mut self, t: &ViewTarget) -> &mut Self {
        self.u32(t.sid).u32(t.format).u32(t.dimension)
    }
}

/// The table entry of object `id` (`entry_size(object.table())` bytes).
pub fn encode(id: u32, object: &Object) -> Vec<u8> {
    let mut o = Out(Vec::new());
    match object {
        Object::RenderTargetView(target, range) => {
            o.target(target);
            let (a, b, c) = match *range {
                RtvRange::Buffer { first, count } => (first, count, 0),
                RtvRange::Texture {
                    mip,
                    first_slice,
                    slices,
                } => (mip, first_slice, slices),
                RtvRange::Texture3d {
                    mip,
                    first_w,
                    w_size,
                } => (mip, first_w, w_size),
            };
            o.u32(a).u32(b).u32(c);
        }
        Object::DepthStencilView(v) => {
            o.target(&v.target)
                .u32(v.mip)
                .u32(v.first_slice)
                .u32(v.slices)
                .u8(v.flags);
        }
        Object::ShaderResourceView(target, range) => {
            o.target(target);
            let words = match *range {
                SrvRange::Buffer { first, count } => [first, count, 0, 0],
                SrvRange::BufferEx {
                    first,
                    count,
                    flags,
                } => [first, count, flags, 0],
                SrvRange::Texture {
                    most_detailed_mip,
                    mip_levels,
                    first_slice,
                    slices,
                } => [most_detailed_mip, first_slice, mip_levels, slices],
            };
            for w in words {
                o.u32(w);
            }
        }
        Object::ElementLayout(elements) => {
            o.u32(id)
                .u32(u32::try_from(elements.len()).unwrap_or(u32::MAX));
            for e in elements {
                o.u32(e.slot)
                    .u32(e.offset)
                    .u32(e.format)
                    .u32(u32::from(e.per_instance))
                    .u32(e.step_rate)
                    .u32(e.register);
            }
        }
        Object::BlendState(s) => {
            o.bool(s.alpha_to_coverage).bool(s.independent).u16(0);
            for t in &s.targets {
                o.bool(t.enable)
                    .u8(t.src)
                    .u8(t.dest)
                    .u8(t.op)
                    .u8(t.src_alpha)
                    .u8(t.dest_alpha)
                    .u8(t.op_alpha)
                    .u8(t.write_mask)
                    .bool(t.logic_op_enable)
                    .u8(t.logic_op)
                    .u16(0);
            }
        }
        Object::DepthStencilState(s) => {
            o.bool(s.depth_enable)
                .u8(s.depth_write_mask)
                .u8(s.depth_func)
                .bool(s.stencil_enable)
                .bool(s.front_enable)
                .bool(s.back_enable)
                .u8(s.stencil_read_mask)
                .u8(s.stencil_write_mask);
            for face in [s.front, s.back] {
                o.u8(face.fail_op)
                    .u8(face.depth_fail_op)
                    .u8(face.pass_op)
                    .u8(face.func);
            }
        }
        Object::RasterizerState(s) => {
            o.u8(s.fill_mode)
                .u8(s.cull_mode)
                .bool(s.front_ccw)
                .bool(s.provoking_vertex_last)
                .i32(s.depth_bias)
                .f32(s.depth_bias_clamp)
                .f32(s.slope_scaled_depth_bias)
                .bool(s.depth_clip)
                .bool(s.scissor)
                .u8(s.multisample)
                .bool(s.antialiased_line)
                .f32(s.line_width)
                .bool(s.line_stipple)
                .u8(s.line_stipple_factor)
                .u16(s.line_stipple_pattern)
                .u8(u8::try_from(s.forced_sample_count).unwrap_or(u8::MAX));
        }
        Object::Sampler(s) => {
            o.u32(s.filter)
                .u8(s.address[0])
                .u8(s.address[1])
                .u8(s.address[2])
                .u8(0)
                .f32(s.mip_lod_bias)
                .u8(s.max_anisotropy)
                .u8(s.comparison)
                .u16(0);
            for c in s.border_color {
                o.f32(c);
            }
            o.f32(s.min_lod).f32(s.max_lod);
        }
        Object::StreamOutput(so) => {
            o.u32(u32::try_from(so.decls.len()).unwrap_or(u32::MAX));
            for i in 0..dx::MAX_STREAMOUT_DECLS {
                let d = so.decls.get(i).copied().unwrap_or(StreamOutputDecl {
                    slot: 0,
                    register: 0,
                    mask: 0,
                    stream: 0,
                });
                o.u32(d.slot)
                    .u32(d.register)
                    .u8(d.mask)
                    .u8(0)
                    .u16(0)
                    .u32(d.stream);
            }
            for s in so.strides {
                o.u32(s);
            }
            o.u32(so.rasterized_stream);
        }
        Object::Query(q) => {
            o.u8(u8::try_from(q.kind).unwrap_or(u8::MAX))
                .u16(0)
                .u8(q.state)
                .u32(q.flags)
                .u32(q.mob)
                .u32(q.offset);
        }
        Object::Shader(s) => {
            o.u32(stage_raw(s.stage))
                .u32(s.size)
                .u32(s.offset)
                .u32(s.mob);
        }
        Object::UaView(v) => {
            o.target(&v.target);
            let (a, b, c) = match v.range {
                UaRange::Buffer {
                    first,
                    count,
                    flags,
                } => (first, count, flags),
                UaRange::Texture {
                    mip,
                    first_slice,
                    slices,
                } => (mip, first_slice, slices),
                UaRange::Texture3d {
                    mip,
                    first_w,
                    w_size,
                } => (mip, first_w, w_size),
            };
            o.u32(a).u32(b).u32(c).u32(0).u32(0).u32(v.structure_count);
        }
    }
    let mut bytes = o.0;
    bytes.resize(entry_size(object.table()).unwrap_or(0), 0);
    bytes
}

const fn stage_raw(stage: ShaderStage) -> u32 {
    match stage {
        ShaderStage::Vertex => dx::shader::VS,
        ShaderStage::Pixel => dx::shader::PS,
        ShaderStage::Geometry => dx::shader::GS,
        ShaderStage::Hull => dx::shader::HS,
        ShaderStage::Domain => dx::shader::DS,
        ShaderStage::Compute => dx::shader::CS,
    }
}

fn word(bytes: &[u8], at: usize) -> u32 {
    let mut w = [0u8; 4];
    if let Some(src) = bytes.get(at..at.saturating_add(4)) {
        w.copy_from_slice(src);
    }
    u32::from_le_bytes(w)
}

/// The object stored in an entry of `table` (`bytes` is the entry).
pub fn decode(table: u32, bytes: &[u8]) -> Result<Object, DxError> {
    let size = entry_size(table).ok_or(DxError::BadValue {
        id: cmd3d::DX_SET_COTABLE,
        field: "object table",
        value: table,
    })?;
    let entry = bytes.get(..size).ok_or(DxError::Short {
        id: cmd3d::DX_SET_COTABLE,
        bytes: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        needed: u32::try_from(size).unwrap_or(u32::MAX),
    })?;
    // Most entries are their DEFINE payload without the ID.
    let define = |id: u32, len: usize| -> Result<Object, DxError> {
        let mut payload = vec![0u8; 4];
        payload.extend_from_slice(entry.get(..len).unwrap_or(entry));
        let size = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        let words: Vec<u32> = payload.chunks(4).map(|c| word(c, 0)).collect();
        match parse(id, size, &words) {
            Some(Ok(command)) => Object::defined_by(&command)
                .map(|(_, object)| object)
                .ok_or(DxError::BadValue {
                    id,
                    field: "command",
                    value: id,
                }),
            Some(Err(error)) => Err(error),
            None => Err(DxError::BadValue {
                id,
                field: "command",
                value: id,
            }),
        }
    };
    match table {
        dx::cotable::RTVIEW => define(cmd3d::DX_DEFINE_RENDERTARGET_VIEW, 24),
        dx::cotable::DSVIEW => define(cmd3d::DX_DEFINE_DEPTHSTENCIL_VIEW, 28),
        dx::cotable::SRVIEW => define(cmd3d::DX_DEFINE_SHADERRESOURCE_VIEW, 28),
        dx::cotable::ELEMENTLAYOUT => {
            let count = usize::try_from(word(entry, 4)).unwrap_or(usize::MAX);
            if count > dx::MAX_ELEMENTS {
                return Err(DxError::TooMany {
                    id: cmd3d::DX_DEFINE_ELEMENTLAYOUT,
                    count: word(entry, 4),
                    max: u32::try_from(dx::MAX_ELEMENTS).unwrap_or(u32::MAX),
                });
            }
            let mut payload = vec![0u8; 4];
            payload.extend_from_slice(
                entry
                    .get(8..8usize.saturating_add(count.saturating_mul(24)))
                    .unwrap_or_default(),
            );
            let words: Vec<u32> = payload.chunks(4).map(|c| word(c, 0)).collect();
            let size = u32::try_from(payload.len()).unwrap_or(u32::MAX);
            match parse(cmd3d::DX_DEFINE_ELEMENTLAYOUT, size, &words) {
                Some(Ok(DxCommand::DefineElementLayout { elements, .. })) => {
                    Ok(Object::ElementLayout(elements))
                }
                Some(Err(error)) => Err(error),
                _ => unreachable_command(),
            }
        }
        dx::cotable::BLENDSTATE => define(cmd3d::DX_DEFINE_BLEND_STATE, 100),
        dx::cotable::DEPTHSTENCIL => define(cmd3d::DX_DEFINE_DEPTHSTENCIL_STATE, 16),
        dx::cotable::RASTERIZERSTATE => {
            // The entry's forced sample count is a byte; the _V2 command's
            // is a word: read the byte alone.
            let mut object = define(cmd3d::DX_DEFINE_RASTERIZER_STATE, 28)?;
            if let Object::RasterizerState(s) = &mut object {
                s.forced_sample_count = u32::from(entry.get(28).copied().unwrap_or(0));
            }
            Ok(object)
        }
        dx::cotable::SAMPLER => define(cmd3d::DX_DEFINE_SAMPLER_STATE, 40),
        dx::cotable::STREAMOUTPUT => define(cmd3d::DX_DEFINE_STREAMOUTPUT, 1048),
        dx::cotable::DXQUERY => {
            let kind = u32::from(entry.first().copied().unwrap_or(0));
            if kind >= dx::query::MAX {
                return Err(DxError::BadValue {
                    id: cmd3d::DX_DEFINE_QUERY,
                    field: "query type",
                    value: kind,
                });
            }
            Ok(Object::Query(Query {
                kind,
                state: entry.get(3).copied().unwrap_or(0),
                flags: word(entry, 4),
                mob: word(entry, 8),
                offset: word(entry, 12),
            }))
        }
        dx::cotable::DXSHADER => {
            let raw = word(entry, 0);
            let stage = ShaderStage::from_raw(raw).ok_or(DxError::BadValue {
                id: cmd3d::DX_DEFINE_SHADER,
                field: "shader type",
                value: raw,
            })?;
            Ok(Object::Shader(Shader {
                stage,
                size: word(entry, 4),
                offset: word(entry, 8),
                mob: word(entry, 12),
            }))
        }
        dx::cotable::UAVIEW => {
            let mut object = define(cmd3d::DX_DEFINE_UA_VIEW, 32)?;
            if let Object::UaView(v) = &mut object {
                v.structure_count = word(entry, 32);
            }
            Ok(object)
        }
        _ => Err(DxError::BadValue {
            id: cmd3d::DX_SET_COTABLE,
            field: "object table",
            value: table,
        }),
    }
}

fn unreachable_command() -> Result<Object, DxError> {
    Err(DxError::BadValue {
        id: cmd3d::DX_DEFINE_ELEMENTLAYOUT,
        field: "command",
        value: cmd3d::DX_DEFINE_ELEMENTLAYOUT,
    })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::dx::{BlendTarget, StencilFace};

    fn objects() -> Vec<Object> {
        let target = ViewTarget {
            sid: 12,
            format: 141,
            dimension: dx::resource::TEXTURE2D,
        };
        let mut blend = BlendState {
            alpha_to_coverage: true,
            independent: false,
            targets: [BlendTarget::default(); 8],
        };
        blend.targets[0] = BlendTarget {
            enable: true,
            src: 5,
            dest: 6,
            op: 1,
            src_alpha: 2,
            dest_alpha: 1,
            op_alpha: 1,
            write_mask: 0xf,
            logic_op_enable: false,
            logic_op: 0,
        };
        let face = StencilFace {
            fail_op: 1,
            depth_fail_op: 2,
            pass_op: 3,
            func: 8,
        };
        vec![
            Object::RenderTargetView(
                target,
                RtvRange::Texture {
                    mip: 1,
                    first_slice: 2,
                    slices: 3,
                },
            ),
            Object::DepthStencilView(DepthStencilView {
                target: ViewTarget {
                    format: 80,
                    ..target
                },
                mip: 0,
                first_slice: 0,
                slices: 1,
                flags: 2,
            }),
            Object::ShaderResourceView(
                ViewTarget {
                    dimension: dx::resource::BUFFEREX,
                    ..target
                },
                SrvRange::BufferEx {
                    first: 4,
                    count: 100,
                    flags: 1,
                },
            ),
            Object::ShaderResourceView(
                target,
                SrvRange::Texture {
                    most_detailed_mip: 1,
                    mip_levels: u32::MAX,
                    first_slice: 0,
                    slices: 6,
                },
            ),
            Object::ElementLayout(vec![
                InputElement {
                    slot: 0,
                    offset: 0,
                    format: 50,
                    per_instance: false,
                    step_rate: 0,
                    register: 0,
                },
                InputElement {
                    slot: 1,
                    offset: 12,
                    format: 68,
                    per_instance: true,
                    step_rate: 1,
                    register: 1,
                },
            ]),
            Object::BlendState(blend),
            Object::DepthStencilState(DepthStencilState {
                depth_enable: true,
                depth_write_mask: 1,
                depth_func: 4,
                stencil_enable: true,
                front_enable: true,
                back_enable: true,
                stencil_read_mask: 0xff,
                stencil_write_mask: 0x0f,
                front: face,
                back: StencilFace { pass_op: 7, ..face },
            }),
            Object::RasterizerState(RasterizerState {
                fill_mode: 3,
                cull_mode: 2,
                front_ccw: true,
                provoking_vertex_last: true,
                depth_bias: -4,
                depth_bias_clamp: 0.5,
                slope_scaled_depth_bias: 1.5,
                depth_clip: true,
                scissor: true,
                multisample: 1,
                antialiased_line: false,
                line_width: 1.0,
                line_stipple: false,
                line_stipple_factor: 0,
                line_stipple_pattern: 0xffff,
                forced_sample_count: 4,
            }),
            Object::Sampler(SamplerState {
                filter: 0x15,
                address: [1, 2, 3],
                mip_lod_bias: -1.0,
                max_anisotropy: 16,
                comparison: 2,
                border_color: [0.0, 0.25, 0.5, 1.0],
                min_lod: f32::MIN,
                max_lod: f32::MAX,
            }),
            Object::StreamOutput(StreamOutput {
                decls: vec![StreamOutputDecl {
                    slot: 0,
                    register: 3,
                    mask: 0xf,
                    stream: 0,
                }],
                strides: [16, 0, 0, 0],
                rasterized_stream: 0,
            }),
            Object::Query(Query {
                kind: dx::query::OCCLUSION64,
                state: QUERY_FINISHED,
                flags: 1,
                mob: 9,
                offset: 64,
            }),
            Object::Shader(Shader {
                stage: ShaderStage::Pixel,
                size: 400,
                offset: 128,
                mob: 3,
            }),
            Object::UaView(UaView {
                target: ViewTarget {
                    dimension: dx::resource::BUFFER,
                    ..target
                },
                range: UaRange::Buffer {
                    first: 2,
                    count: 64,
                    flags: crate::dx::UABUFFER_APPEND,
                },
                structure_count: 17,
            }),
            Object::UaView(UaView {
                target,
                range: UaRange::Texture {
                    mip: 1,
                    first_slice: 0,
                    slices: 4,
                },
                structure_count: 0,
            }),
        ]
    }

    #[test]
    fn entries_round_trip() {
        for object in objects() {
            let bytes = encode(5, &object);
            assert_eq!(Some(bytes.len()), entry_size(object.table()), "{object:?}");
            assert_eq!(decode(object.table(), &bytes), Ok(object.clone()));
        }
    }

    #[test]
    fn entries_follow_the_header_layouts() {
        // Spot checks of offsets: the shader entry, the query state byte,
        // the rasterizer forced sample count, the element count.
        let all = objects();
        let shader = encode(1, &all[11]);
        assert_eq!(word(&shader, 0), dx::shader::PS);
        assert_eq!(
            (word(&shader, 4), word(&shader, 8), word(&shader, 12)),
            (400, 128, 3)
        );
        let query = encode(1, &all[10]);
        assert_eq!(
            (query[0], query[3], word(&query, 8)),
            (7, QUERY_FINISHED, 9)
        );
        assert_eq!(encode(1, &all[7])[28], 4);
        // UA view: the description's three words, then structureCount
        // after the description's two padding words.
        let view = encode(1, &all[12]);
        assert_eq!(
            (word(&view, 12), word(&view, 20), word(&view, 32)),
            (2, crate::dx::UABUFFER_APPEND, 17)
        );
        let layout = encode(6, &all[4]);
        assert_eq!(
            (word(&layout, 0), word(&layout, 4)),
            (6, 2),
            "elid, numDescs"
        );
    }

    #[test]
    fn define_commands_give_their_objects() {
        let command = DxCommand::DefineShader {
            id: 4,
            stage: ShaderStage::Vertex,
            size: 360,
        };
        let (id, object) = Object::defined_by(&command).unwrap();
        assert_eq!(id, 4);
        assert_eq!(
            object,
            Object::Shader(Shader {
                stage: ShaderStage::Vertex,
                size: 360,
                offset: 0,
                mob: svga_proto::INVALID_ID
            })
        );
        assert_eq!(Object::defined_by(&DxCommand::DrawAuto), None);
    }

    #[test]
    fn hostile_entries_are_refused() {
        let mut layout = vec![0u8; 1024];
        layout[4] = 33; // more elements than an entry holds
        assert!(matches!(
            decode(dx::cotable::ELEMENTLAYOUT, &layout),
            Err(DxError::TooMany { count: 33, .. })
        ));
        let mut view = vec![0u8; 32];
        view[8] = 9; // no such resource dimension
        assert!(matches!(
            decode(dx::cotable::SRVIEW, &view),
            Err(DxError::BadValue { value: 9, .. })
        ));
        assert!(
            decode(dx::cotable::DXSHADER, &[0u8; 32]).is_err(),
            "shader type 0"
        );
        assert!(decode(dx::cotable::SAMPLER, &[0u8; 10]).is_err(), "short");
        assert!(
            decode(dx::cotable::UAVIEW, &[0u8; 64]).is_err(),
            "resource dimension 0"
        );
        assert!(decode(99, &[0u8; 64]).is_err());
    }
}
