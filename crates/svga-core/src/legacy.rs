//! Legacy 3D context commands (`svga3d_cmd.h`, the interface before DX
//! contexts, which VMware calls VGPU9): what VMware's Direct3D 9 driver
//! sends.
//!
//! A legacy context carries the Direct3D 9 device state: render states,
//! texture stage states, transforms, lights and material of the
//! fixed-function pipeline, render targets, Direct3D 9 shaders (bytecode
//! kept in guest-backed shader objects) and their constants. Every command
//! names its context. [`parse`] turns one command into a [`LegacyCommand`]:
//! fields typed, arrays bounded by the protocol's limits. State values stay
//! raw (`SVGA3dRenderStateName` and friends): the device model interprets
//! them.

use svga_proto::cmd3d;

use crate::command::Rect;
use crate::gb::ImageId;
use crate::layout::Box3;

/// Vertex arrays of one draw (`SVGA3D_MAX_VERTEX_ARRAYS`).
pub const MAX_VERTEX_ARRAYS: usize = 32;
/// Primitive ranges of one draw (`SVGA3D_MAX_DRAW_PRIMITIVE_RANGES`).
pub const MAX_PRIMITIVE_RANGES: usize = 32;

/// A Direct3D 9 shader stage (`SVGA3dShaderType`, `VS` or `PS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Vertex shader.
    Vertex,
    /// Pixel shader.
    Pixel,
}

impl Stage {
    /// The stage of an `SVGA3dShaderType` value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Vertex),
            2 => Some(Self::Pixel),
            _ => None,
        }
    }
}

/// Kind of shader constant (`SVGA3dShaderConstType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstKind {
    /// `c#`: four floats.
    Float,
    /// `i#`: four integers.
    Int,
    /// `b#`: one boolean.
    Bool,
}

/// A vertex or index array in a buffer surface (`SVGA3dArray`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Array {
    /// Buffer surface.
    pub sid: u32,
    /// Byte offset of the first element.
    pub offset: u32,
    /// Bytes from one element to the next.
    pub stride: u32,
}

/// One vertex input of a draw (`SVGA3dVertexDecl`: identity, array, then
/// range hint).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexDecl {
    /// `SVGA3dDeclType` (`D3DDECLTYPE`).
    pub decl_type: u32,
    /// `SVGA3dDeclMethod`.
    pub method: u32,
    /// `SVGA3dDeclUsage` (`D3DDECLUSAGE`).
    pub usage: u32,
    /// Usage index.
    pub usage_index: u32,
    /// First and last vertex the draw reads (a hint).
    pub range_hint: (u32, u32),
    /// Where the vertices are.
    pub array: Array,
}

/// One primitive batch of a draw (`SVGA3dPrimitiveRange`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimitiveRange {
    /// `SVGA3dPrimitiveType`.
    pub primitive: u32,
    /// Number of primitives.
    pub count: u32,
    /// Indices; `stride` 0 and `sid` invalid when not indexed.
    pub indices: Array,
    /// Bytes per index: 0 (not indexed), 2 or 4.
    pub index_width: u32,
    /// Added to every vertex index.
    pub index_bias: i32,
}

/// One texture stage state (`SVGA3dTextureState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureState {
    /// Stage (sampler) index.
    pub stage: u32,
    /// `SVGA3dTextureStateName`.
    pub name: u32,
    /// Value: an integer, a float's bits or a surface ID.
    pub value: u32,
}

/// A fixed-function material (`SVGA3dMaterial`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// Diffuse color.
    pub diffuse: [f32; 4],
    /// Ambient color.
    pub ambient: [f32; 4],
    /// Specular color.
    pub specular: [f32; 4],
    /// Emissive color.
    pub emissive: [f32; 4],
    /// Specular power.
    pub shininess: f32,
}

/// A fixed-function light (`SVGA3dLightData`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    /// `SVGA3dLightType`.
    pub kind: u32,
    /// Position and direction are in world space (else view space).
    pub in_world_space: bool,
    /// Diffuse color.
    pub diffuse: [f32; 4],
    /// Specular color.
    pub specular: [f32; 4],
    /// Ambient color.
    pub ambient: [f32; 4],
    /// Position.
    pub position: [f32; 4],
    /// Direction.
    pub direction: [f32; 4],
    /// Largest lit distance.
    pub range: f32,
    /// Spotlight falloff.
    pub falloff: f32,
    /// Constant, linear and quadratic attenuation.
    pub attenuation: [f32; 3],
    /// Spotlight inner cone angle.
    pub theta: f32,
    /// Spotlight outer cone angle.
    pub phi: f32,
}

/// A decoded legacy 3D command.
#[derive(Clone, Debug, PartialEq)]
pub enum LegacyCommand {
    /// Creates a context (`DEFINE_GB_CONTEXT`, `CONTEXT_DEFINE`).
    DefineContext {
        /// Context ID.
        cid: u32,
    },
    /// Destroys a context (`DESTROY_GB_CONTEXT`, `CONTEXT_DESTROY`).
    DestroyContext {
        /// Context ID.
        cid: u32,
    },
    /// Gives a context its backing MOB (`INVALID_ID`: none).
    BindContext {
        /// Context ID.
        cid: u32,
        /// MOB ID.
        mob: u32,
        /// Whether the MOB holds state to restore.
        valid_contents: bool,
    },
    /// The context's state must be written to its MOB.
    ReadbackContext {
        /// Context ID.
        cid: u32,
    },
    /// The context's state may be discarded.
    InvalidateContext {
        /// Context ID.
        cid: u32,
    },
    /// Creates a guest-backed shader object.
    DefineShader {
        /// Shader ID.
        shid: u32,
        /// Stage.
        stage: Stage,
        /// Bytecode size in bytes.
        size: u32,
    },
    /// Destroys a guest-backed shader object.
    DestroyShader {
        /// Shader ID.
        shid: u32,
    },
    /// Says where a shader's bytecode is (`INVALID_ID`: unbinds).
    BindShader {
        /// Shader ID.
        shid: u32,
        /// MOB ID.
        mob: u32,
        /// Byte offset in the MOB.
        offset: u32,
    },
    /// Creates a shader with its bytecode (`SHADER_DEFINE`, before
    /// guest-backed objects).
    DefineInlineShader {
        /// Context ID.
        cid: u32,
        /// Shader ID.
        shid: u32,
        /// Stage.
        stage: Stage,
        /// Bytecode.
        code: Vec<u32>,
    },
    /// Destroys a shader made by [`Self::DefineInlineShader`].
    DestroyInlineShader {
        /// Context ID.
        cid: u32,
        /// Shader ID.
        shid: u32,
        /// Stage.
        stage: Stage,
    },
    /// Selects a stage's shader (`INVALID_ID`: the fixed-function stage).
    SetShader {
        /// Context ID.
        cid: u32,
        /// Stage.
        stage: Stage,
        /// Shader ID.
        shid: u32,
    },
    /// Sets shader constants from register `start` on (`SET_SHADER_CONST`,
    /// `SET_GB_SHADERCONSTS_INLINE`): four words per float or integer
    /// register, one per boolean.
    SetShaderConst {
        /// Context ID.
        cid: u32,
        /// Stage.
        stage: Stage,
        /// Register file.
        kind: ConstKind,
        /// First register.
        start: u32,
        /// Values (floats as bits).
        values: Vec<u32>,
    },
    /// Sets a fixed-function matrix.
    SetTransform {
        /// Context ID.
        cid: u32,
        /// `SVGA3dTransformType`.
        kind: u32,
        /// Row-major, as Direct3D 9 gives it.
        matrix: [f32; 16],
    },
    /// Sets the depth range of the viewport.
    SetZRange {
        /// Context ID.
        cid: u32,
        /// Nearest depth.
        min: f32,
        /// Farthest depth.
        max: f32,
    },
    /// Sets render states: `(SVGA3dRenderStateName, value)` pairs.
    SetRenderStates {
        /// Context ID.
        cid: u32,
        /// The states, in order.
        states: Vec<(u32, u32)>,
    },
    /// Binds a render target (`INVALID_ID` surface: unbinds).
    SetRenderTarget {
        /// Context ID.
        cid: u32,
        /// `SVGA3dRenderTargetType`: depth, stencil, colors 0 to 7.
        slot: u32,
        /// The image.
        image: ImageId,
    },
    /// Sets texture stage states.
    SetTextureStates {
        /// Context ID.
        cid: u32,
        /// The states, in order.
        states: Vec<TextureState>,
    },
    /// Sets the fixed-function material.
    SetMaterial {
        /// Context ID.
        cid: u32,
        /// `SVGA3dFace` it applies to.
        face: u32,
        /// The material.
        material: Material,
    },
    /// Sets a fixed-function light.
    SetLightData {
        /// Context ID.
        cid: u32,
        /// Light index.
        index: u32,
        /// The light.
        light: Light,
    },
    /// Turns a fixed-function light on or off.
    SetLightEnabled {
        /// Context ID.
        cid: u32,
        /// Light index.
        index: u32,
        /// On.
        enabled: bool,
    },
    /// Sets the viewport rectangle.
    SetViewport {
        /// Context ID.
        cid: u32,
        /// The rectangle.
        rect: Rect,
    },
    /// Sets a user clip plane.
    SetClipPlane {
        /// Context ID.
        cid: u32,
        /// Plane index.
        index: u32,
        /// Plane equation.
        plane: [f32; 4],
    },
    /// Sets the scissor rectangle.
    SetScissorRect {
        /// Context ID.
        cid: u32,
        /// The rectangle.
        rect: Rect,
    },
    /// Clears the bound targets, in rectangles (none: everything).
    Clear {
        /// Context ID.
        cid: u32,
        /// `SVGA3dClearFlag` bits.
        flags: u32,
        /// Color, `0xAARRGGBB`.
        color: u32,
        /// Depth.
        depth: f32,
        /// Stencil.
        stencil: u32,
        /// Rectangles.
        rects: Vec<Rect>,
    },
    /// Draws primitives from vertex arrays.
    DrawPrimitives {
        /// Context ID.
        cid: u32,
        /// Vertex inputs.
        decls: Vec<VertexDecl>,
        /// Primitive batches.
        ranges: Vec<PrimitiveRange>,
        /// Instancing divisors (`SVGA3dVertexDivisor`), one per input, or
        /// none.
        divisors: Vec<u32>,
    },
    /// Starts a query.
    BeginQuery {
        /// Context ID.
        cid: u32,
        /// `SVGA3dQueryType`.
        kind: u32,
    },
    /// Ends a query; its result (`SVGA3dQueryResult`) goes to a MOB.
    EndQuery {
        /// Context ID.
        cid: u32,
        /// `SVGA3dQueryType`.
        kind: u32,
        /// MOB ID.
        mob: u32,
        /// Byte offset in the MOB.
        offset: u32,
    },
    /// The query's result must be in its MOB once this is done.
    WaitForQuery {
        /// Context ID.
        cid: u32,
        /// `SVGA3dQueryType`.
        kind: u32,
        /// MOB ID.
        mob: u32,
        /// Byte offset in the MOB.
        offset: u32,
    },
    /// Copies a box of an image to a box of another, scaling.
    StretchBlt {
        /// Source image.
        src: ImageId,
        /// Destination image.
        dest: ImageId,
        /// Source box.
        src_box: Box3,
        /// Destination box.
        dest_box: Box3,
        /// `SVGA3dStretchBltMode`: 0 point, 1 linear.
        mode: u32,
    },
    /// Rebuilds a surface's mipmaps from its first level.
    GenerateMipmaps {
        /// Surface ID.
        sid: u32,
        /// `SVGA3dTextureFilter`.
        filter: u32,
    },
}

impl LegacyCommand {
    /// The context the command works on, if it has one.
    pub fn context(&self) -> Option<u32> {
        use LegacyCommand as C;
        match self {
            C::DefineContext { cid }
            | C::DestroyContext { cid }
            | C::BindContext { cid, .. }
            | C::ReadbackContext { cid }
            | C::InvalidateContext { cid }
            | C::DefineInlineShader { cid, .. }
            | C::DestroyInlineShader { cid, .. }
            | C::SetShader { cid, .. }
            | C::SetShaderConst { cid, .. }
            | C::SetTransform { cid, .. }
            | C::SetZRange { cid, .. }
            | C::SetRenderStates { cid, .. }
            | C::SetRenderTarget { cid, .. }
            | C::SetTextureStates { cid, .. }
            | C::SetMaterial { cid, .. }
            | C::SetLightData { cid, .. }
            | C::SetLightEnabled { cid, .. }
            | C::SetViewport { cid, .. }
            | C::SetClipPlane { cid, .. }
            | C::SetScissorRect { cid, .. }
            | C::Clear { cid, .. }
            | C::DrawPrimitives { cid, .. }
            | C::BeginQuery { cid, .. }
            | C::EndQuery { cid, .. }
            | C::WaitForQuery { cid, .. } => Some(*cid),
            C::DefineShader { .. }
            | C::DestroyShader { .. }
            | C::BindShader { .. }
            | C::StretchBlt { .. }
            | C::GenerateMipmaps { .. } => None,
        }
    }
}

/// Why a legacy command could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LegacyError {
    /// Shorter than its fixed part.
    Short {
        /// Command ID.
        id: u32,
        /// Payload words.
        words: usize,
        /// Words needed.
        needed: usize,
    },
    /// Its array part is not a whole number of elements, or does not match
    /// its counts.
    Ragged {
        /// Command ID.
        id: u32,
        /// Words of the array part.
        words: usize,
    },
    /// More array elements than the protocol allows.
    TooMany {
        /// Command ID.
        id: u32,
        /// Elements.
        count: usize,
        /// Largest count.
        max: usize,
    },
    /// A field outside its enumeration.
    BadValue {
        /// Command ID.
        id: u32,
        /// Field.
        field: &'static str,
        /// Value.
        value: u32,
    },
}

impl core::fmt::Display for LegacyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Short { id, words, needed } => {
                write!(f, "3D command {id} has {words} words, {needed} needed")
            }
            Self::Ragged { id, words } => {
                write!(f, "3D command {id}: {words} array words do not match")
            }
            Self::TooMany { id, count, max } => {
                write!(f, "3D command {id}: {count} elements, at most {max}")
            }
            Self::BadValue { id, field, value } => {
                write!(f, "3D command {id}: {field} = {value} is not valid")
            }
        }
    }
}

/// Fixed words of each command decoded here.
fn fixed_words(id: u32) -> Option<usize> {
    use cmd3d::*;
    Some(match id {
        CONTEXT_DEFINE
        | CONTEXT_DESTROY
        | DEFINE_GB_CONTEXT
        | DESTROY_GB_CONTEXT
        | READBACK_GB_CONTEXT
        | INVALIDATE_GB_CONTEXT
        | DESTROY_GB_SHADER
        | SETRENDERSTATE
        | SETTEXTURESTATE => 1,
        BEGIN_GB_QUERY | GENERATE_MIPMAPS => 2,
        BIND_GB_CONTEXT | DEFINE_GB_SHADER | BIND_GB_SHADER | SET_SHADER | SETZRANGE
        | SETLIGHTENABLED | SHADER_DESTROY | SHADER_DEFINE | DRAW_PRIMITIVES => 3,
        END_GB_QUERY | WAIT_FOR_GB_QUERY | SET_GB_SHADERCONSTS_INLINE => 4,
        SETRENDERTARGET | SETVIEWPORT | SETSCISSORRECT | CLEAR => 5,
        SETCLIPPLANE => 6,
        SET_SHADER_CONST => 8,
        SETTRANSFORM => 18,
        SETMATERIAL => 19,
        SURFACE_STRETCHBLT => 19,
        SETLIGHTDATA => 31,
        _ => return None,
    })
}

/// Decodes a legacy 3D context command. `None`: not one of them.
pub fn parse(id: u32, payload: &[u32]) -> Option<Result<LegacyCommand, LegacyError>> {
    let needed = fixed_words(id)?;
    if payload.len() < needed {
        return Some(Err(LegacyError::Short {
            id,
            words: payload.len(),
            needed,
        }));
    }
    Some(decode(id, payload, needed))
}

fn float(word: u32) -> f32 {
    f32::from_bits(word)
}

fn floats<const N: usize>(words: &[u32]) -> [f32; N] {
    let mut out = [0.0; N];
    for (o, w) in out.iter_mut().zip(words) {
        *o = float(*w);
    }
    out
}

fn rect(w: &[u32]) -> Rect {
    Rect {
        x: w[0],
        y: w[1],
        width: w[2],
        height: w[3],
    }
}

fn image(w: &[u32]) -> ImageId {
    ImageId {
        sid: w[0],
        face: w[1],
        mipmap: w[2],
    }
}

fn area(w: &[u32]) -> Box3 {
    Box3 {
        x: w[0],
        y: w[1],
        z: w[2],
        w: w[3],
        h: w[4],
        d: w[5],
    }
}

fn array(w: &[u32]) -> Array {
    Array {
        sid: w[0],
        offset: w[1],
        stride: w[2],
    }
}

fn stage(id: u32, value: u32) -> Result<Stage, LegacyError> {
    Stage::from_raw(value).ok_or(LegacyError::BadValue {
        id,
        field: "shader type",
        value,
    })
}

fn const_kind(id: u32, value: u32) -> Result<ConstKind, LegacyError> {
    match value {
        0 => Ok(ConstKind::Float),
        1 => Ok(ConstKind::Int),
        2 => Ok(ConstKind::Bool),
        _ => Err(LegacyError::BadValue {
            id,
            field: "constant type",
            value,
        }),
    }
}

/// `tail` split into elements of `size` words.
fn elements(
    id: u32,
    tail: &[u32],
    size: usize,
) -> Result<core::slice::ChunksExact<'_, u32>, LegacyError> {
    if tail.len().checked_rem(size) != Some(0) {
        return Err(LegacyError::Ragged {
            id,
            words: tail.len(),
        });
    }
    Ok(tail.chunks_exact(size))
}

fn decode(id: u32, w: &[u32], fixed: usize) -> Result<LegacyCommand, LegacyError> {
    use LegacyCommand as C;
    use cmd3d::*;
    let tail = &w[fixed..];
    Ok(match id {
        CONTEXT_DEFINE | DEFINE_GB_CONTEXT => C::DefineContext { cid: w[0] },
        CONTEXT_DESTROY | DESTROY_GB_CONTEXT => C::DestroyContext { cid: w[0] },
        BIND_GB_CONTEXT => C::BindContext {
            cid: w[0],
            mob: w[1],
            valid_contents: w[2] != 0,
        },
        READBACK_GB_CONTEXT => C::ReadbackContext { cid: w[0] },
        INVALIDATE_GB_CONTEXT => C::InvalidateContext { cid: w[0] },
        DEFINE_GB_SHADER => C::DefineShader {
            shid: w[0],
            stage: stage(id, w[1])?,
            size: w[2],
        },
        DESTROY_GB_SHADER => C::DestroyShader { shid: w[0] },
        BIND_GB_SHADER => C::BindShader {
            shid: w[0],
            mob: w[1],
            offset: w[2],
        },
        SHADER_DEFINE => C::DefineInlineShader {
            cid: w[0],
            shid: w[1],
            stage: stage(id, w[2])?,
            code: tail.to_vec(),
        },
        SHADER_DESTROY => C::DestroyInlineShader {
            cid: w[0],
            shid: w[1],
            stage: stage(id, w[2])?,
        },
        SET_SHADER => C::SetShader {
            cid: w[0],
            stage: stage(id, w[1])?,
            shid: w[2],
        },
        SET_SHADER_CONST => C::SetShaderConst {
            cid: w[0],
            start: w[1],
            stage: stage(id, w[2])?,
            kind: const_kind(id, w[3])?,
            values: w[4..].to_vec(),
        },
        SET_GB_SHADERCONSTS_INLINE => C::SetShaderConst {
            cid: w[0],
            start: w[1],
            stage: stage(id, w[2])?,
            kind: const_kind(id, w[3])?,
            values: tail.to_vec(),
        },
        SETTRANSFORM => C::SetTransform {
            cid: w[0],
            kind: w[1],
            matrix: floats(&w[2..18]),
        },
        SETZRANGE => C::SetZRange {
            cid: w[0],
            min: float(w[1]),
            max: float(w[2]),
        },
        SETRENDERSTATE => C::SetRenderStates {
            cid: w[0],
            states: elements(id, tail, 2)?.map(|s| (s[0], s[1])).collect(),
        },
        SETRENDERTARGET => C::SetRenderTarget {
            cid: w[0],
            slot: w[1],
            image: image(&w[2..5]),
        },
        SETTEXTURESTATE => C::SetTextureStates {
            cid: w[0],
            states: elements(id, tail, 3)?
                .map(|s| TextureState {
                    stage: s[0],
                    name: s[1],
                    value: s[2],
                })
                .collect(),
        },
        SETMATERIAL => C::SetMaterial {
            cid: w[0],
            face: w[1],
            material: Material {
                diffuse: floats(&w[2..6]),
                ambient: floats(&w[6..10]),
                specular: floats(&w[10..14]),
                emissive: floats(&w[14..18]),
                shininess: float(w[18]),
            },
        },
        SETLIGHTDATA => C::SetLightData {
            cid: w[0],
            index: w[1],
            light: Light {
                kind: w[2],
                in_world_space: w[3] != 0,
                diffuse: floats(&w[4..8]),
                specular: floats(&w[8..12]),
                ambient: floats(&w[12..16]),
                position: floats(&w[16..20]),
                direction: floats(&w[20..24]),
                range: float(w[24]),
                falloff: float(w[25]),
                attenuation: floats(&w[26..29]),
                theta: float(w[29]),
                phi: float(w[30]),
            },
        },
        SETLIGHTENABLED => C::SetLightEnabled {
            cid: w[0],
            index: w[1],
            enabled: w[2] != 0,
        },
        SETVIEWPORT => C::SetViewport {
            cid: w[0],
            rect: rect(&w[1..5]),
        },
        SETCLIPPLANE => C::SetClipPlane {
            cid: w[0],
            index: w[1],
            plane: floats(&w[2..6]),
        },
        SETSCISSORRECT => C::SetScissorRect {
            cid: w[0],
            rect: rect(&w[1..5]),
        },
        CLEAR => C::Clear {
            cid: w[0],
            flags: w[1],
            color: w[2],
            depth: float(w[3]),
            stencil: w[4],
            rects: elements(id, tail, 4)?.map(rect).collect(),
        },
        DRAW_PRIMITIVES => draw_primitives(id, w)?,
        BEGIN_GB_QUERY => C::BeginQuery {
            cid: w[0],
            kind: w[1],
        },
        END_GB_QUERY => C::EndQuery {
            cid: w[0],
            kind: w[1],
            mob: w[2],
            offset: w[3],
        },
        WAIT_FOR_GB_QUERY => C::WaitForQuery {
            cid: w[0],
            kind: w[1],
            mob: w[2],
            offset: w[3],
        },
        SURFACE_STRETCHBLT => C::StretchBlt {
            src: image(&w[0..3]),
            dest: image(&w[3..6]),
            src_box: area(&w[6..12]),
            dest_box: area(&w[12..18]),
            mode: w[18],
        },
        GENERATE_MIPMAPS => C::GenerateMipmaps {
            sid: w[0],
            filter: w[1],
        },
        _ => {
            return Err(LegacyError::BadValue {
                id,
                field: "command",
                value: id,
            });
        }
    })
}

/// `DRAW_PRIMITIVES`: the header, `numVertexDecls` declarations of 9
/// words, `numRanges` ranges of 7 words, then optionally one divisor per
/// declaration.
fn draw_primitives(id: u32, w: &[u32]) -> Result<LegacyCommand, LegacyError> {
    let decl_count = usize::try_from(w[1]).unwrap_or(usize::MAX);
    let range_count = usize::try_from(w[2]).unwrap_or(usize::MAX);
    if decl_count > MAX_VERTEX_ARRAYS {
        return Err(LegacyError::TooMany {
            id,
            count: decl_count,
            max: MAX_VERTEX_ARRAYS,
        });
    }
    if range_count > MAX_PRIMITIVE_RANGES {
        return Err(LegacyError::TooMany {
            id,
            count: range_count,
            max: MAX_PRIMITIVE_RANGES,
        });
    }
    let tail = &w[3..];
    let decl_words = decl_count.saturating_mul(9);
    let range_words = range_count.saturating_mul(7);
    let rest = tail
        .len()
        .checked_sub(decl_words.saturating_add(range_words))
        .ok_or(LegacyError::Ragged {
            id,
            words: tail.len(),
        })?;
    // Divisors: all of them or none (trailing padding is tolerated).
    let divisors = if rest >= decl_count && decl_count > 0 {
        &tail[decl_words.saturating_add(range_words)..][..decl_count]
    } else {
        &[]
    };
    let decls = tail[..decl_words]
        .chunks_exact(9)
        .map(|d| VertexDecl {
            decl_type: d[0],
            method: d[1],
            usage: d[2],
            usage_index: d[3],
            array: array(&d[4..7]),
            range_hint: (d[7], d[8]),
        })
        .collect();
    let ranges = tail[decl_words..][..range_words]
        .chunks_exact(7)
        .map(|r| PrimitiveRange {
            primitive: r[0],
            count: r[1],
            indices: array(&r[2..5]),
            index_width: r[5],
            index_bias: r[6] as i32,
        })
        .collect();
    Ok(LegacyCommand::DrawPrimitives {
        cid: w[0],
        decls,
        ranges,
        divisors: divisors.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_texture_states_come_in_pairs_and_triples() {
        let states = parse(cmd3d::SETRENDERSTATE, &[3, 1, 1, 36, 4])
            .unwrap()
            .unwrap();
        assert_eq!(
            states,
            LegacyCommand::SetRenderStates {
                cid: 3,
                states: vec![(1, 1), (36, 4)],
            }
        );
        assert!(matches!(
            parse(cmd3d::SETRENDERSTATE, &[3, 1, 1, 36]),
            Some(Err(LegacyError::Ragged { .. }))
        ));
        let textures = parse(cmd3d::SETTEXTURESTATE, &[3, 0, 2, 4, 1, 1, 7])
            .unwrap()
            .unwrap();
        assert_eq!(
            textures,
            LegacyCommand::SetTextureStates {
                cid: 3,
                states: vec![
                    TextureState {
                        stage: 0,
                        name: 2,
                        value: 4
                    },
                    TextureState {
                        stage: 1,
                        name: 1,
                        value: 7
                    },
                ],
            }
        );
    }

    #[test]
    fn a_draw_carries_declarations_ranges_and_optional_divisors() {
        let mut w = vec![5, 2, 1];
        // Position: FLOAT3 from surface 9, stride 16; color at offset 12.
        w.extend([2, 0, 0, 0, 9, 0, 16, 0, 2]);
        w.extend([4, 0, 10, 0, 9, 12, 16, 0, 2]);
        // One triangle list of 1 primitive, indexed 16-bit from surface 8.
        w.extend([1, 1, 8, 0, 2, 2, (-1i32) as u32]);
        let Some(Ok(LegacyCommand::DrawPrimitives {
            cid,
            decls,
            ranges,
            divisors,
        })) = parse(cmd3d::DRAW_PRIMITIVES, &w)
        else {
            panic!("not a draw");
        };
        assert_eq!((cid, decls.len(), ranges.len()), (5, 2, 1));
        assert_eq!(decls[1].usage, 10);
        assert_eq!(decls[1].array.offset, 12);
        assert_eq!(ranges[0].index_width, 2);
        assert_eq!(ranges[0].index_bias, -1);
        assert!(divisors.is_empty());
        w.extend([0, 0]);
        let Some(Ok(LegacyCommand::DrawPrimitives { divisors, .. })) =
            parse(cmd3d::DRAW_PRIMITIVES, &w)
        else {
            panic!("not a draw");
        };
        assert_eq!(divisors, vec![0, 0]);
        // Counts beyond the words, or beyond the protocol limits.
        assert!(matches!(
            parse(cmd3d::DRAW_PRIMITIVES, &[5, 3, 1, 0, 0]),
            Some(Err(LegacyError::Ragged { .. }))
        ));
        assert!(matches!(
            parse(cmd3d::DRAW_PRIMITIVES, &[5, 33, 0]),
            Some(Err(LegacyError::TooMany { .. }))
        ));
    }

    #[test]
    fn shaders_and_constants_name_their_stage() {
        assert_eq!(
            parse(cmd3d::DEFINE_GB_SHADER, &[7, 2, 256]),
            Some(Ok(LegacyCommand::DefineShader {
                shid: 7,
                stage: Stage::Pixel,
                size: 256,
            }))
        );
        assert!(matches!(
            parse(cmd3d::DEFINE_GB_SHADER, &[7, 3, 256]),
            Some(Err(LegacyError::BadValue { .. }))
        ));
        assert_eq!(
            parse(cmd3d::SET_GB_SHADERCONSTS_INLINE, &[1, 4, 1, 0, 1, 2, 3, 4]),
            Some(Ok(LegacyCommand::SetShaderConst {
                cid: 1,
                stage: Stage::Vertex,
                kind: ConstKind::Float,
                start: 4,
                values: vec![1, 2, 3, 4],
            }))
        );
        assert!(matches!(
            parse(cmd3d::SET_SHADER_CONST, &[1, 0, 1, 3, 0, 0, 0, 0]),
            Some(Err(LegacyError::BadValue { .. }))
        ));
    }

    #[test]
    fn clears_lights_and_short_payloads() {
        assert_eq!(
            parse(
                cmd3d::CLEAR,
                &[1, 3, 0xff00_0000, 1.0f32.to_bits(), 0, 0, 0, 8, 8]
            ),
            Some(Ok(LegacyCommand::Clear {
                cid: 1,
                flags: 3,
                color: 0xff00_0000,
                depth: 1.0,
                stencil: 0,
                rects: vec![Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8
                }],
            }))
        );
        let mut light = vec![1, 0, 4, 1];
        light.extend((0..27).map(|i| (i as f32).to_bits()));
        let Some(Ok(LegacyCommand::SetLightData { light, .. })) =
            parse(cmd3d::SETLIGHTDATA, &light)
        else {
            panic!("not a light");
        };
        assert_eq!(light.kind, 4);
        assert!(light.in_world_space);
        assert_eq!(light.direction, [16.0, 17.0, 18.0, 19.0]);
        assert_eq!(light.phi, 26.0);
        assert!(matches!(
            parse(cmd3d::SETLIGHTDATA, &[0; 30]),
            Some(Err(LegacyError::Short { .. }))
        ));
        assert_eq!(parse(cmd3d::DX_DRAW, &[0; 4]), None);
    }
}
