//! Direct3D 9 shaders (shader models 1 to 3 bytecode, as VMware's
//! Direct3D 9 driver sends them in legacy contexts) translated to SPIR-V.
//!
//! The driver emulates the fixed-function pipeline with shaders of its own;
//! what remains for the device is what Direct3D 9 does around shaders: the
//! alpha test and fog (pixel shaders before model 3), flat shading, and
//! pretransformed vertices ([`pretransformed_vs`]).
//!
//! Every register is a `vec4` of floats in a function variable; integer
//! registers (`a0`, `aL`, `i#`) are ints, booleans (`b#`, `p0`) bools.
//! Constants live in one uniform block per stage ([`constants`]), filled
//! by the device from the context's state; `def` values replace those
//! registers ([`Translated::defs`]). Vertex shader inputs are matched to
//! the draw's vertex declarations by semantic; outputs and pixel shader
//! inputs get a location per semantic ([`location`]).
//!
//! Supported: vs_1_1 to vs_3_0, ps_2_0 to ps_3_0, and ps_1_1 to ps_1_4
//! without the texture addressing instructions (`texbem`, `texm3x3`...),
//! subroutines (`call`) and shadow samplers. Anything else is refused with
//! a reason.

use std::collections::HashMap;

use crate::decode::Stage;
use crate::spirv::{Module, Scalar, glsl, op};
use crate::translate::{Binding, BindingKind, Shader, TranslateError, binding};

/// Largest program, in tokens.
pub const MAX_TOKENS: usize = 1 << 16;

/// The uniform block of each stage: `vec4` indices.
pub mod constants {
    /// `c0` to `c255`.
    pub const FLOATS: u32 = 0;
    /// `i0` to `i15` (int bits).
    pub const INTS: u32 = 256;
    /// `b0` to `b15`, four per `vec4` (uint bits, 0 or 1).
    pub const BOOLS: u32 = 272;
    /// Alpha test reference, fog start, fog end, fog density.
    pub const ALPHA_FOG: u32 = 276;
    /// Fog color.
    pub const FOG_COLOR: u32 = 277;
    /// Pretransformed vertices: x scale, y scale, x offset, y offset (from
    /// screen to clip coordinates).
    pub const VIEWPORT: u32 = 278;
    /// Pretransformed vertices: z scale, z offset.
    pub const DEPTH: u32 = 279;
    /// Point size, smallest and largest size.
    pub const POINT: u32 = 280;
    /// User clip planes 0 to 5.
    pub const CLIP_PLANES: u32 = 281;
    /// Bump mapping matrix of texture stages 0 to 7: 00, 01, 10, 11.
    pub const BUMP_ENV: u32 = 287;
    /// Bump mapping luminance scale and offset of stages 0 to 7.
    pub const BUMP_LUMINANCE: u32 = 295;
    /// Size of the block.
    pub const COUNT: u32 = 303;
}

/// How fog is applied (pixel shaders before model 3 and the fixed
/// function).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Fog {
    /// No fog.
    #[default]
    None,
    /// The vertex's fog factor (`oFog`).
    Vertex,
    /// From depth: `exp(-density * z)`.
    Exp,
    /// From depth: `exp(-(density * z)^2)`.
    Exp2,
    /// From depth: `(end - z) / (end - start)`.
    Linear,
}

/// What the device state adds to a program.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Options {
    /// Alpha test (`SVGA3dCmpFunc`; 0 and 8 (always): none).
    pub alpha_test: u8,
    /// Fog.
    pub fog: Fog,
    /// Flat shading: colors are not interpolated.
    pub flat: bool,
    /// Vertex shader inputs fetched as integers (`UBYTE4`, `SHORT2`...):
    /// bit per input register, converted to floats.
    pub int_inputs: u16,
    /// Signed among those.
    pub signed_inputs: u16,
    /// Texture types for pixel shaders without sampler declarations
    /// (`D3DSAMPLER_TEXTURE_TYPE`: 2 2D, 3 cube, 4 volume), per sampler.
    pub sampler_types: [u8; 16],
    /// Vertex shader of a point draw: writes the point size
    /// ([`constants::POINT`]) unless the program does, and clamps it.
    pub points: bool,
    /// Pixel shader of point sprites: texture coordinates span each point.
    pub point_sprite: bool,
    /// Samplers reading a depth texture as a shadow map (bit per sampler,
    /// 2D only): `texld` returns the comparison of the coordinates' z
    /// (after `texldp`'s division) with the depth, filtered.
    pub shadow: u16,
    /// Vertex shader: user clip planes enabled (bit per plane,
    /// [`constants::CLIP_PLANES`], in clip space).
    pub clip_planes: u8,
}

/// A translated program and what the device needs to run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translated {
    /// The SPIR-V and its descriptors.
    pub shader: Shader,
    /// Shader model (major, minor).
    pub version: (u8, u8),
    /// Vertex shader inputs: (register, `D3DDECLUSAGE`, usage index).
    pub inputs: Vec<(u32, u32, u32)>,
    /// Constants defined by the program: (`vec4` index in the block,
    /// value bits).
    pub defs: Vec<(u32, [u32; 4])>,
    /// Samplers read: (sampler index, `D3DSAMPLER_TEXTURE_TYPE`).
    pub samplers: Vec<(u32, u32)>,
    /// Whether the vertex shader writes the point size.
    pub writes_point_size: bool,
}

/// Interface location of a semantic (`D3DDECLUSAGE`, index) between the
/// vertex and pixel shaders.
pub fn location(usage: u32, index: u32) -> Option<u32> {
    Some(match (usage, index) {
        (10, 0..=1) => index,                    // COLOR
        (5, 0..=15) => index.saturating_add(2),  // TEXCOORD
        (11, 0) => 18,                           // FOG
        (3, 0..=1) => index.saturating_add(19),  // NORMAL
        (6, 0) => 21,                            // TANGENT
        (7, 0) => 22,                            // BINORMAL
        (1, 0) => 23,                            // BLENDWEIGHT
        (2, 0) => 24,                            // BLENDINDICES
        (0, 1..=2) => index.saturating_add(24),  // POSITION1, POSITION2
        (12, 0) => 27,                           // DEPTH
        (4, 1..=2) => index.saturating_add(27),  // PSIZE1, PSIZE2
        (13, 0) => 30,                           // SAMPLE
        (10, 2..=3) => index.saturating_add(29), // COLOR2, COLOR3 (31, 32: beyond)
        _ => return None,
    })
    .filter(|l| *l < 31)
}

// ---- tokens -----------------------------------------------------------

// Register types (`D3DSHADER_PARAM_REGISTER_TYPE`).
const R_TEMP: u32 = 0;
const R_INPUT: u32 = 1;
const R_CONST: u32 = 2;
const R_ADDR: u32 = 3; // vertex shaders; pixel shaders: texture
const R_RASTOUT: u32 = 4;
const R_ATTROUT: u32 = 5;
const R_OUTPUT: u32 = 6; // vs_3_0; earlier: texture coordinate output
const R_CONSTINT: u32 = 7;
const R_COLOROUT: u32 = 8;
const R_DEPTHOUT: u32 = 9;
const R_SAMPLER: u32 = 10;
const R_CONSTBOOL: u32 = 14;
const R_LOOP: u32 = 15;
const R_MISC: u32 = 17;
const R_LABEL: u32 = 18;
const R_PREDICATE: u32 = 19;

// Opcodes (`D3DSHADER_INSTRUCTION_OPCODE_TYPE`).
mod o {
    pub const NOP: u16 = 0;
    pub const MOV: u16 = 1;
    pub const ADD: u16 = 2;
    pub const SUB: u16 = 3;
    pub const MAD: u16 = 4;
    pub const MUL: u16 = 5;
    pub const RCP: u16 = 6;
    pub const RSQ: u16 = 7;
    pub const DP3: u16 = 8;
    pub const DP4: u16 = 9;
    pub const MIN: u16 = 10;
    pub const MAX: u16 = 11;
    pub const SLT: u16 = 12;
    pub const SGE: u16 = 13;
    pub const EXP: u16 = 14;
    pub const LOG: u16 = 15;
    pub const LIT: u16 = 16;
    pub const DST: u16 = 17;
    pub const LRP: u16 = 18;
    pub const FRC: u16 = 19;
    pub const M4X4: u16 = 20;
    pub const M4X3: u16 = 21;
    pub const M3X4: u16 = 22;
    pub const M3X3: u16 = 23;
    pub const M3X2: u16 = 24;
    pub const CALL: u16 = 25;
    pub const CALLNZ: u16 = 26;
    pub const LOOP: u16 = 27;
    pub const RET: u16 = 28;
    pub const ENDLOOP: u16 = 29;
    pub const LABEL: u16 = 30;
    pub const DCL: u16 = 31;
    pub const POW: u16 = 32;
    pub const CRS: u16 = 33;
    pub const SGN: u16 = 34;
    pub const ABS: u16 = 35;
    pub const NRM: u16 = 36;
    pub const SINCOS: u16 = 37;
    pub const REP: u16 = 38;
    pub const ENDREP: u16 = 39;
    pub const IF: u16 = 40;
    pub const IFC: u16 = 41;
    pub const ELSE: u16 = 42;
    pub const ENDIF: u16 = 43;
    pub const BREAK: u16 = 44;
    pub const BREAKC: u16 = 45;
    pub const MOVA: u16 = 46;
    pub const DEFB: u16 = 47;
    pub const DEFI: u16 = 48;
    pub const TEXCOORD: u16 = 64;
    pub const TEXKILL: u16 = 65;
    pub const TEX: u16 = 66;
    pub const TEXBEM: u16 = 67;
    pub const TEXBEML: u16 = 68;
    pub const TEXREG2AR: u16 = 69;
    pub const TEXREG2GB: u16 = 70;
    pub const TEXM3X2PAD: u16 = 71;
    pub const TEXM3X2TEX: u16 = 72;
    pub const TEXM3X3PAD: u16 = 73;
    pub const TEXM3X3TEX: u16 = 74;
    pub const TEXM3X3SPEC: u16 = 76;
    pub const TEXM3X3VSPEC: u16 = 77;
    pub const TEXREG2RGB: u16 = 82;
    pub const TEXDP3TEX: u16 = 83;
    pub const TEXM3X2DEPTH: u16 = 84;
    pub const TEXDP3: u16 = 85;
    pub const TEXM3X3: u16 = 86;
    pub const TEXDEPTH: u16 = 87;
    pub const EXPP: u16 = 78;
    pub const LOGP: u16 = 79;
    pub const CND: u16 = 80;
    pub const DEF: u16 = 81;
    pub const CMP: u16 = 88;
    pub const BEM: u16 = 89;
    pub const DP2ADD: u16 = 90;
    pub const DSX: u16 = 91;
    pub const DSY: u16 = 92;
    pub const TEXLDD: u16 = 93;
    pub const SETP: u16 = 94;
    pub const TEXLDL: u16 = 95;
    pub const BREAKP: u16 = 96;
    pub const PHASE: u16 = 0xfffd;
    pub const COMMENT: u16 = 0xfffe;
    pub const END: u16 = 0xffff;
}

/// A register.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reg {
    /// `D3DSPR_*`.
    pub kind: u32,
    /// Number.
    pub num: u32,
}

/// A relative address: register and component.
type Relative = Option<(Reg, u8)>;

/// A source parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Src {
    /// Register.
    pub reg: Reg,
    /// Component read for each of x, y, z, w.
    pub swizzle: [u8; 4],
    /// `D3DSHADER_PARAM_SRCMOD_TYPE`.
    pub modifier: u32,
    /// Relative addressing.
    pub relative: Relative,
}

/// A destination parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dst {
    /// Register.
    pub reg: Reg,
    /// Write mask (bit 0: x).
    pub mask: u8,
    /// `_sat`.
    pub saturate: bool,
    /// `_centroid` (declarations).
    pub centroid: bool,
    /// Result scale of shader model 1 (`_x2`: 1, `_d2`: -1).
    pub shift: i8,
    /// Relative addressing (vs_3_0 outputs).
    pub relative: Relative,
}

/// An instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ins {
    /// Opcode.
    pub op: u16,
    /// Opcode-specific bits 16 to 23 (comparison, `texld` variant).
    pub control: u32,
    /// Destination.
    pub dst: Option<Dst>,
    /// Predicate (`(p0) instruction`).
    pub pred: Option<Src>,
    /// Sources.
    pub srcs: Vec<Src>,
    /// `dcl`: the usage token; `def*`: the values.
    pub extra: Vec<u32>,
}

/// A decoded program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// Vertex or pixel.
    pub stage: Stage,
    /// Shader model major version.
    pub major: u8,
    /// Shader model minor version.
    pub minor: u8,
    /// Instructions, comments dropped.
    pub ins: Vec<Ins>,
}

fn err<T>(what: impl core::fmt::Display) -> Result<T, TranslateError> {
    Err(TranslateError(what.to_string()))
}

fn unsupported<T>(what: impl core::fmt::Display) -> Result<T, TranslateError> {
    Err(TranslateError(format!("unsupported: {what}")))
}

fn reg_of(token: u32) -> Reg {
    Reg {
        kind: ((token >> 28) & 7) | ((token >> 8) & 0x18),
        num: token & 0x7ff,
    }
}

/// Decodes a program's tokens (anything after `end` is ignored).
pub fn decode(words: &[u32]) -> Result<Program, TranslateError> {
    let version = *words.first().ok_or(TranslateError("empty shader".into()))?;
    let stage = match version >> 16 {
        0xfffe => Stage::Vertex,
        0xffff => Stage::Pixel,
        _ => return err(format_args!("bad version token {version:#x}")),
    };
    let (major, minor) = (((version >> 8) & 0xff) as u8, (version & 0xff) as u8);
    let valid = match stage {
        Stage::Vertex => matches!((major, minor), (1, 0 | 1) | (2, 0 | 1) | (3, 0)),
        _ => matches!((major, minor), (1, 0..=4) | (2, 0 | 1) | (3, 0)),
    };
    if !valid {
        return unsupported(format_args!("shader model {major}.{minor}"));
    }
    let words = &words[..words.len().min(MAX_TOKENS)];
    let mut ins = Vec::new();
    let mut at = 1;
    loop {
        let Some(&token) = words.get(at) else {
            return err("no end token");
        };
        at = at.saturating_add(1);
        let opcode = (token & 0xffff) as u16;
        match opcode {
            o::END => break,
            o::COMMENT => {
                at = at.saturating_add(((token >> 16) & 0x7fff) as usize);
                continue;
            }
            o::PHASE => {
                ins.push(Ins {
                    op: o::PHASE,
                    control: 0,
                    dst: None,
                    pred: None,
                    srcs: Vec::new(),
                    extra: Vec::new(),
                });
                continue;
            }
            _ => {}
        }
        // The parameters: a length from model 2 on, else every following
        // token with bit 31 (`def` values excepted).
        let len = if major >= 2 {
            ((token >> 24) & 0xf) as usize
        } else if opcode == o::DEF {
            5
        } else {
            words[at..]
                .iter()
                .take_while(|t| *t & 0x8000_0000 != 0)
                .count()
        };
        let params = words
            .get(at..at.saturating_add(len))
            .ok_or(TranslateError("instruction past the end".into()))?;
        at = at.saturating_add(len);
        ins.push(instruction(opcode, token, params, major)?);
    }
    Ok(Program {
        stage,
        major,
        minor,
        ins,
    })
}

fn has_dst(opcode: u16) -> bool {
    !matches!(
        opcode,
        o::NOP
            | o::CALL
            | o::CALLNZ
            | o::LOOP
            | o::RET
            | o::ENDLOOP
            | o::LABEL
            | o::REP
            | o::ENDREP
            | o::IF
            | o::IFC
            | o::ELSE
            | o::ENDIF
            | o::BREAK
            | o::BREAKC
            | o::BREAKP
    )
}

fn instruction(opcode: u16, token: u32, params: &[u32], major: u8) -> Result<Ins, TranslateError> {
    let mut i = Ins {
        op: opcode,
        control: (token >> 16) & 0xff,
        dst: None,
        pred: None,
        srcs: Vec::new(),
        extra: Vec::new(),
    };
    let mut p = params.iter().copied();
    if opcode == o::DCL {
        i.extra
            .push(p.next().ok_or(TranslateError("short dcl".into()))?);
    }
    if has_dst(opcode) {
        let t = p
            .next()
            .ok_or(TranslateError("missing destination".into()))?;
        let rel = relative(t, major, &mut p);
        let shift = (((t >> 24) & 0xf) as i8) << 4 >> 4;
        i.dst = Some(Dst {
            reg: reg_of(t),
            mask: ((t >> 16) & 0xf) as u8,
            saturate: t & (1 << 20) != 0,
            centroid: t & (4 << 20) != 0,
            shift,
            relative: rel,
        });
    }
    if matches!(opcode, o::DEF | o::DEFI | o::DEFB) {
        i.extra.extend(p);
        return Ok(i);
    }
    if token & (1 << 28) != 0 && major >= 2 {
        let t = p.next().ok_or(TranslateError("missing predicate".into()))?;
        i.pred = Some(src_of(t, None));
    }
    while let Some(t) = p.next() {
        let rel = relative(t, major, &mut p);
        i.srcs.push(src_of(t, rel));
    }
    Ok(i)
}

/// The relative address of a parameter token: the next token from model 2
/// on, a0.x in model 1.
fn relative(t: u32, major: u8, p: &mut impl Iterator<Item = u32>) -> Relative {
    if t & (1 << 13) == 0 {
        return None;
    }
    if major >= 2 {
        let a = p.next()?;
        Some((reg_of(a), ((a >> 16) & 3) as u8))
    } else {
        Some((
            Reg {
                kind: R_ADDR,
                num: 0,
            },
            0,
        ))
    }
}

/// The four values of a `def` or `defi`.
fn values4(ins: &Ins) -> Result<[u32; 4], TranslateError> {
    match ins.extra[..] {
        [a, b, c, d, ..] => Ok([a, b, c, d]),
        _ => err("def with fewer than four values"),
    }
}

fn src_of(t: u32, relative: Relative) -> Src {
    let s = (t >> 16) & 0xff;
    Src {
        reg: reg_of(t),
        swizzle: [
            (s & 3) as u8,
            ((s >> 2) & 3) as u8,
            ((s >> 4) & 3) as u8,
            ((s >> 6) & 3) as u8,
        ],
        modifier: (t >> 24) & 0xf,
        relative,
    }
}

// ---- SPIR-V -------------------------------------------------------------

const STORAGE_INPUT: u32 = 1;
const STORAGE_UNIFORM: u32 = 2;
const STORAGE_OUTPUT: u32 = 3;
const STORAGE_UNIFORM_CONSTANT: u32 = 0;
const STORAGE_FUNCTION: u32 = 7;
const DECORATION_BLOCK: u32 = 2;
const DECORATION_ARRAY_STRIDE: u32 = 6;
const DECORATION_BUILTIN: u32 = 11;
const DECORATION_FLAT: u32 = 14;
const DECORATION_CENTROID: u32 = 16;
const DECORATION_LOCATION: u32 = 30;
const DECORATION_OFFSET: u32 = 35;
const BUILTIN_POSITION: u32 = 0;
const BUILTIN_POINT_SIZE: u32 = 1;
const BUILTIN_FRAG_COORD: u32 = 15;
const BUILTIN_FRONT_FACING: u32 = 17;
const BUILTIN_FRAG_DEPTH: u32 = 22;
const BUILTIN_POINT_COORD: u32 = 16;
const BUILTIN_CLIP_DISTANCE: u32 = 3;
const CAP_CLIP_DISTANCE: u32 = 32;
const CAP_SHADER: u32 = 1;
const MODEL_VERTEX: u32 = 0;
const MODEL_FRAGMENT: u32 = 4;
const MODE_ORIGIN_UPPER_LEFT: u32 = 7;
const MODE_DEPTH_REPLACING: u32 = 12;
const IMAGE_BIAS: u32 = 0x1;
const IMAGE_LOD: u32 = 0x2;
const IMAGE_GRAD: u32 = 0x4;

// Opcodes the shared module does not name.
const OP_LOGICAL_NOT: u16 = 168;
const OP_LOGICAL_AND: u16 = 167;
const OP_F_SUB: u16 = 131;
const OP_ALL: u16 = 155;
const OP_F_ORD_NOT_EQUAL: u16 = 182;
const OP_F_ORD_GREATER_THAN: u16 = 186;
const OP_F_ORD_LESS_THAN_EQUAL: u16 = 188;
const OP_S_LESS_THAN: u16 = 177;
const OP_I_ADD: u16 = 128;

// GLSL.std.450 instructions the shared module does not name.
const GLSL_ROUND: u32 = 1;
const GLSL_F_SIGN: u32 = 6;
const GLSL_POW: u32 = 26;
const GLSL_F_MIN: u32 = 37;
const GLSL_F_MAX: u32 = 40;
const GLSL_F_CLAMP: u32 = 43;
const GLSL_F_MIX: u32 = 46;
const GLSL_CROSS: u32 = 68;
const GLSL_EXP: u32 = 27;

/// Flow control being translated.
enum Flow {
    If {
        merge: u32,
        otherwise: u32,
        has_else: bool,
    },
    Loop {
        header: u32,
        merge: u32,
        next: u32,
        counter: u32,
        /// `aL` of this loop and its step (`loop`), none for `rep`.
        al: Option<(u32, u32)>,
    },
}

struct Tx<'a> {
    m: Module,
    p: &'a Program,
    options: &'a Options,
    f: u32,
    f4: u32,
    i: u32,
    i4: u32,
    b: u32,
    b4: u32,
    u: u32,
    /// Register variables (temps, outputs, a0, p0, ps 1 textures).
    regs: HashMap<(u32, u32), u32>,
    /// Interface inputs: register → (variable, semantic location).
    inputs: HashMap<(u32, u32), u32>,
    /// Outputs written at the end: (local, interface variable, saturate).
    sinks: Vec<(u32, u32, bool)>,
    /// Scalar outputs: (local, variable) for point size and depth.
    scalar_sinks: Vec<(u32, u32)>,
    interface: Vec<u32>,
    constants: u32,
    /// Sampler → (image, image type, sampler, coordinates, shadow).
    samplers: HashMap<u32, (u32, u32, u32, u32, bool)>,
    defs: HashMap<u32, [u32; 4]>,
    bool_defs: HashMap<u32, bool>,
    fog_input: Option<u32>,
    point_coord: Option<u32>,
    /// The program's point size output.
    point_size_var: Option<u32>,
    /// The local of the position output.
    position: Option<u32>,
    /// Dot products of `texm3x2pad` / `texm3x3pad`, for the instruction
    /// that ends the matrix.
    pads: Vec<u32>,
    /// Subroutine calls being inlined.
    calls: u32,
    flow: Vec<Flow>,
    bindings: Vec<Binding>,
    input_list: Vec<(u32, u32, u32)>,
    input_locations: Vec<u32>,
    output_locations: Vec<u32>,
    sampler_list: Vec<(u32, u32)>,
    frag_coord: Option<u32>,
    front_facing: Option<u32>,
    depth_out: bool,
    point_size: bool,
    /// Semantics of `dcl`'d registers: (kind, num) → (usage, index).
    semantics: HashMap<(u32, u32), (u32, u32)>,
    centroid: HashMap<(u32, u32), bool>,
    sampler_types: HashMap<u32, u32>,
}

/// Translates `program`.
pub fn translate(program: &Program, options: &Options) -> Result<Translated, TranslateError> {
    let mut m = Module::new();
    m.capability(CAP_SHADER);
    let f = m.scalar(Scalar::Float);
    let f4 = m.vector(Scalar::Float, 4);
    let i = m.scalar(Scalar::Int);
    let i4 = m.vector(Scalar::Int, 4);
    let b = m.scalar(Scalar::Bool);
    let b4 = m.vector(Scalar::Bool, 4);
    let u = m.scalar(Scalar::Uint);
    let mut tx = Tx {
        m,
        p: program,
        options,
        f,
        f4,
        i,
        i4,
        b,
        b4,
        u,
        regs: HashMap::new(),
        inputs: HashMap::new(),
        sinks: Vec::new(),
        scalar_sinks: Vec::new(),
        interface: Vec::new(),
        constants: 0,
        samplers: HashMap::new(),
        defs: HashMap::new(),
        bool_defs: HashMap::new(),
        fog_input: None,
        point_coord: None,
        point_size_var: None,
        position: None,
        pads: Vec::new(),
        calls: 0,
        flow: Vec::new(),
        bindings: Vec::new(),
        input_list: Vec::new(),
        input_locations: Vec::new(),
        output_locations: Vec::new(),
        sampler_list: Vec::new(),
        frag_coord: None,
        front_facing: None,
        depth_out: false,
        point_size: false,
        semantics: HashMap::new(),
        centroid: HashMap::new(),
        sampler_types: HashMap::new(),
    };
    let function = tx.m.id();
    let entry = tx.m.id();
    tx.m.emit_void(op::LABEL, &[entry]);
    tx.declare_constants();
    tx.scan()?;
    let pixel_1x = program.stage == Stage::Pixel && program.major == 1;
    tx.run(&program.ins)?;
    if !tx.flow.is_empty() {
        return err("unterminated if or loop");
    }
    if pixel_1x {
        // Shader model 1: the color is r0.
        let r0 = tx.reg_var(R_TEMP, 0);
        let oc = tx.reg_var(R_COLOROUT, 0);
        let v = tx.m.emit(op::LOAD, f4, &[r0]);
        tx.m.emit_void(op::STORE, &[oc, v]);
    }
    tx.finish_outputs()?;
    tx.m.emit_void(op::RETURN, &[]);
    let (model, mut modes): (u32, Vec<Vec<u32>>) = match program.stage {
        Stage::Vertex => (MODEL_VERTEX, Vec::new()),
        _ => (MODEL_FRAGMENT, vec![vec![MODE_ORIGIN_UPPER_LEFT]]),
    };
    if tx.depth_out {
        modes.push(vec![MODE_DEPTH_REPLACING]);
    }
    let mode_refs: Vec<&[u32]> = modes.iter().map(Vec::as_slice).collect();
    let interface = tx.interface.clone();
    tx.m.entry_point(model, function, &interface, &mode_refs);
    let spirv = tx.m.finish(function);
    let mut defs: Vec<(u32, [u32; 4])> = tx.defs.iter().map(|(k, v)| (*k, *v)).collect();
    defs.sort_unstable();
    Ok(Translated {
        shader: Shader {
            spirv: Vec::new(),
            hash: 0,
            stage: program.stage,
            bindings: tx.bindings,
            inputs: tx.input_locations,
            outputs: tx.output_locations,
        }
        .with_spirv(spirv),
        version: (program.major, program.minor),
        inputs: tx.input_list,
        defs,
        samplers: tx.sampler_list,
        writes_point_size: tx.point_size,
    })
}

/// Decodes then translates.
pub fn translate_words(words: &[u32], options: &Options) -> Result<Translated, TranslateError> {
    translate(&decode(words)?, options)
}

impl Tx<'_> {
    fn stage(&self) -> Stage {
        self.p.stage
    }

    fn ptr(&mut self, class: u32, ty: u32) -> u32 {
        self.m.pointer(class, ty)
    }

    fn fconst(&mut self, v: f32) -> u32 {
        self.m.constant_f32_bits(v.to_bits())
    }

    fn f4const(&mut self, v: [f32; 4]) -> u32 {
        let bits: Vec<u32> = v.iter().map(|x| x.to_bits()).collect();
        self.m.constant_vector(Scalar::Float, &bits)
    }

    fn splat(&mut self, s: u32) -> u32 {
        self.m.emit(op::COMPOSITE_CONSTRUCT, self.f4, &[s, s, s, s])
    }

    fn extract(&mut self, v: u32, c: u32) -> u32 {
        self.m.emit(op::COMPOSITE_EXTRACT, self.f, &[v, c])
    }

    fn shuffle(&mut self, v: u32, s: [u32; 4]) -> u32 {
        self.m
            .emit(op::VECTOR_SHUFFLE, self.f4, &[v, v, s[0], s[1], s[2], s[3]])
    }

    fn declare_constants(&mut self) {
        let array = self.m.array_unique(self.f4, constants::COUNT);
        self.m.decorate(array, &[DECORATION_ARRAY_STRIDE, 16]);
        let block = self.m.structure(&[array]);
        self.m.decorate(block, &[DECORATION_BLOCK]);
        self.m.member_decorate(block, 0, &[DECORATION_OFFSET, 0]);
        let p = self.ptr(STORAGE_UNIFORM, block);
        let var = self.m.global(p, STORAGE_UNIFORM);
        self.bind(
            var,
            BindingKind::ConstantBuffer {
                slot: 0,
                vec4s: constants::COUNT,
            },
        );
        self.constants = var;
    }

    fn bind(&mut self, var: u32, kind: BindingKind) {
        let b = binding(self.stage(), &kind);
        self.m.decorate(var, &[34, 0]);
        self.m.decorate(var, &[33, b]);
        self.bindings.push(Binding { kind, binding: b });
    }

    /// `vec4` number `index` (a constant or an int) of the uniform block.
    fn uniform(&mut self, index: u32) -> u32 {
        let zero = self.m.constant_u32(0);
        let p = self.ptr(STORAGE_UNIFORM, self.f4);
        let chain = self
            .m
            .emit(op::ACCESS_CHAIN, p, &[self.constants, zero, index]);
        self.m.emit(op::LOAD, self.f4, &[chain])
    }

    fn uniform_at(&mut self, index: u32) -> u32 {
        let c = self.m.constant_u32(index);
        self.uniform(c)
    }

    /// A function variable for a register, zeroed at entry.
    fn reg_var(&mut self, kind: u32, num: u32) -> u32 {
        if let Some(v) = self.regs.get(&(kind, num)) {
            return *v;
        }
        let ty = match kind {
            R_ADDR if self.stage() == Stage::Vertex => self.i4,
            R_LOOP => self.i4,
            R_PREDICATE => self.b4,
            _ => self.f4,
        };
        let p = self.ptr(STORAGE_FUNCTION, ty);
        let var = self.m.local(p);
        self.regs.insert((kind, num), var);
        var
    }

    /// Declarations, defined constants and the registers the program uses,
    /// created and zeroed up front (their first use may be in a branch).
    fn scan(&mut self) -> Result<(), TranslateError> {
        let p = self.p;
        for ins in &p.ins {
            match ins.op {
                o::DCL => self.dcl(ins)?,
                o::DEF => {
                    let d = ins
                        .dst
                        .ok_or(TranslateError("def without register".into()))?;
                    let v = values4(ins)?;
                    self.defs.insert(constants::FLOATS + d.reg.num.min(255), v);
                }
                o::DEFI => {
                    let d = ins
                        .dst
                        .ok_or(TranslateError("defi without register".into()))?;
                    let v = values4(ins)?;
                    self.defs
                        .insert(constants::INTS.saturating_add(d.reg.num.min(15)), v);
                }
                o::DEFB => {
                    let d = ins
                        .dst
                        .ok_or(TranslateError("defb without register".into()))?;
                    self.bool_defs.insert(
                        d.reg.num.min(15),
                        ins.extra.first().is_some_and(|v| *v != 0),
                    );
                }
                _ => {}
            }
        }
        // Registers written anywhere: zeroed variables.
        let mut written: Vec<Reg> = Vec::new();
        for ins in &p.ins {
            if let Some(d) = ins.dst
                && !matches!(ins.op, o::DCL | o::DEF | o::DEFI | o::DEFB | o::TEXKILL)
            {
                written.push(d.reg);
            }
            for s in &ins.srcs {
                if matches!(s.reg.kind, R_TEMP) {
                    written.push(s.reg);
                }
            }
        }
        if p.ins
            .iter()
            .any(|i| matches!(i.op, o::TEXDEPTH | o::TEXM3X2DEPTH))
        {
            written.push(Reg {
                kind: R_DEPTHOUT,
                num: 0,
            });
        }
        if p.stage == Stage::Pixel && p.major == 1 {
            written.push(Reg {
                kind: R_TEMP,
                num: 0,
            });
            written.push(Reg {
                kind: R_COLOROUT,
                num: 0,
            });
        }
        for r in written {
            if self.regs.contains_key(&(r.kind, r.num)) {
                continue;
            }
            self.output(r)?;
        }
        Ok(())
    }

    /// Creates the variable of a written register (and its interface
    /// output), zeroed.
    fn output(&mut self, r: Reg) -> Result<(), TranslateError> {
        let vertex = self.stage() == Stage::Vertex;
        let major = self.p.major;
        // (location or builtin, saturate)
        enum Out {
            Local,
            Location(u32, bool),
            Position,
            PointSize,
            Depth,
        }
        let out = match (r.kind, vertex) {
            (R_TEMP | R_ADDR | R_LOOP | R_PREDICATE, _) => Out::Local,
            (R_RASTOUT, true) => match r.num {
                0 => Out::Position,
                1 => Out::Location(18, false),
                2 => Out::PointSize,
                n => return err(format_args!("rasterizer output {n}")),
            },
            (R_ATTROUT, true) => Out::Location(r.num.min(1), true),
            (R_OUTPUT, true) if major < 3 => Out::Location(r.num.min(7).saturating_add(2), false),
            (R_OUTPUT, true) => match self.semantics.get(&(R_OUTPUT, r.num)) {
                Some((0, 0)) => Out::Position,
                Some((4, 0)) => Out::PointSize,
                Some(&(usage, index)) => match location(usage, index) {
                    Some(l) => Out::Location(l, false),
                    None => return unsupported(format_args!("output semantic {usage}/{index}")),
                },
                None => return err(format_args!("output o{} is not declared", r.num)),
            },
            (R_COLOROUT, false) => Out::Location(r.num.min(3), false),
            (R_DEPTHOUT, false) => Out::Depth,
            (kind, _) => return err(format_args!("register type {kind} is not writable")),
        };
        let var = self.reg_var(r.kind, r.num);
        let zero = match r.kind {
            R_ADDR if vertex => self.m.constant_vector(Scalar::Int, &[0; 4]),
            R_LOOP => self.m.constant_vector(Scalar::Int, &[0; 4]),
            R_PREDICATE => {
                let f = self.m.constant_bool(false);
                self.m.emit(op::COMPOSITE_CONSTRUCT, self.b4, &[f, f, f, f])
            }
            _ => self.f4const([0.0; 4]),
        };
        self.m.emit_void(op::STORE, &[var, zero]);
        match out {
            Out::Local => {}
            Out::Location(l, saturate) => {
                let p = self.ptr(STORAGE_OUTPUT, self.f4);
                let iv = self.m.global(p, STORAGE_OUTPUT);
                self.m.decorate(iv, &[DECORATION_LOCATION, l]);
                self.interface.push(iv);
                self.output_locations.push(l);
                self.sinks.push((var, iv, saturate && major < 3));
            }
            Out::Position => {
                self.position = Some(var);
                let p = self.ptr(STORAGE_OUTPUT, self.f4);
                let iv = self.m.global(p, STORAGE_OUTPUT);
                self.m.decorate(iv, &[DECORATION_BUILTIN, BUILTIN_POSITION]);
                self.interface.push(iv);
                self.sinks.push((var, iv, false));
            }
            Out::PointSize => {
                let p = self.ptr(STORAGE_OUTPUT, self.f);
                let iv = self.m.global(p, STORAGE_OUTPUT);
                self.point_size_var = Some(iv);
                self.m
                    .decorate(iv, &[DECORATION_BUILTIN, BUILTIN_POINT_SIZE]);
                self.interface.push(iv);
                self.scalar_sinks.push((var, iv));
                self.point_size = true;
            }
            Out::Depth => {
                let p = self.ptr(STORAGE_OUTPUT, self.f);
                let iv = self.m.global(p, STORAGE_OUTPUT);
                self.m
                    .decorate(iv, &[DECORATION_BUILTIN, BUILTIN_FRAG_DEPTH]);
                self.interface.push(iv);
                self.scalar_sinks.push((var, iv));
                self.depth_out = true;
            }
        }
        Ok(())
    }

    fn dcl(&mut self, ins: &Ins) -> Result<(), TranslateError> {
        let d = ins
            .dst
            .ok_or(TranslateError("dcl without register".into()))?;
        let usage_token = *ins
            .extra
            .first()
            .ok_or(TranslateError("dcl without usage".into()))?;
        let usage = usage_token & 0x1f;
        let index = (usage_token >> 16) & 0xf;
        match d.reg.kind {
            R_SAMPLER => {
                let ty = (usage_token >> 27) & 0xf;
                self.sampler_types.insert(d.reg.num, ty);
            }
            R_INPUT | R_OUTPUT | R_ADDR => {
                self.semantics
                    .insert((d.reg.kind, d.reg.num), (usage, index));
                self.centroid.insert((d.reg.kind, d.reg.num), d.centroid);
            }
            _ => {}
        }
        Ok(())
    }

    /// The interface variable of an input register.
    fn input(&mut self, r: Reg) -> Result<u32, TranslateError> {
        if let Some(v) = self.inputs.get(&(r.kind, r.num)) {
            return Ok(*v);
        }
        let vertex = self.stage() == Stage::Vertex;
        let major = self.p.major;
        let ty = self.f4;
        let (ty, decorations): (u32, Vec<Vec<u32>>) = if vertex {
            let (usage, index) = self
                .semantics
                .get(&(R_INPUT, r.num))
                .copied()
                .unwrap_or((0, r.num));
            self.input_list.push((r.num, usage, index));
            self.input_locations.push(r.num);
            let bit = 1u16.checked_shl(r.num).unwrap_or(0);
            let ty = if self.options.int_inputs & bit == 0 {
                ty
            } else if self.options.signed_inputs & bit != 0 {
                self.i4
            } else {
                self.m.vector(Scalar::Uint, 4)
            };
            (ty, vec![vec![DECORATION_LOCATION, r.num]])
        } else {
            let (usage, index) = match (r.kind, major) {
                (R_INPUT, 3) => self
                    .semantics
                    .get(&(R_INPUT, r.num))
                    .copied()
                    .ok_or_else(|| TranslateError(format!("input v{} is not declared", r.num)))?,
                (R_INPUT, _) => (10, r.num),
                (_, _) => (5, r.num), // t#: texture coordinates
            };
            let l = location(usage, index)
                .ok_or_else(|| TranslateError(format!("input semantic {usage}/{index}")))?;
            let mut d = vec![vec![DECORATION_LOCATION, l]];
            if usage == 10 && self.options.flat {
                d.push(vec![DECORATION_FLAT]);
            } else if self.centroid.get(&(r.kind, r.num)) == Some(&true) {
                d.push(vec![DECORATION_CENTROID]);
            }
            self.input_locations.push(l);
            (ty, d)
        };
        let p = self.ptr(STORAGE_INPUT, ty);
        let var = self.m.global(p, STORAGE_INPUT);
        for d in &decorations {
            self.m.decorate(var, d);
        }
        self.interface.push(var);
        self.inputs.insert((r.kind, r.num), var);
        Ok(var)
    }

    fn load_input(&mut self, r: Reg) -> Result<u32, TranslateError> {
        if self.options.point_sprite && self.stage() == Stage::Pixel {
            let texcoord = match r.kind {
                R_ADDR => true,
                R_INPUT if self.p.major >= 3 => self
                    .semantics
                    .get(&(R_INPUT, r.num))
                    .is_some_and(|s| s.0 == 5),
                _ => false,
            };
            if texcoord {
                // Point sprites: (0, 0) at the top left of each point.
                let f2 = self.m.vector(Scalar::Float, 2);
                let var = match self.point_coord {
                    Some(v) => v,
                    None => {
                        let v = self.builtin_input(f2, BUILTIN_POINT_COORD);
                        self.point_coord = Some(v);
                        v
                    }
                };
                let pc = self.m.emit(op::LOAD, f2, &[var]);
                let x = self.m.emit(op::COMPOSITE_EXTRACT, self.f, &[pc, 0]);
                let y = self.m.emit(op::COMPOSITE_EXTRACT, self.f, &[pc, 1]);
                let zero = self.fconst(0.0);
                let one = self.fconst(1.0);
                return Ok(self
                    .m
                    .emit(op::COMPOSITE_CONSTRUCT, self.f4, &[x, y, zero, one]));
            }
        }
        let var = self.input(r)?;
        if self.stage() == Stage::Vertex {
            let bit = 1u16.checked_shl(r.num).unwrap_or(0);
            if self.options.int_inputs & bit != 0 {
                let signed = self.options.signed_inputs & bit != 0;
                let ty = if signed {
                    self.i4
                } else {
                    self.m.vector(Scalar::Uint, 4)
                };
                let v = self.m.emit(op::LOAD, ty, &[var]);
                let conv = if signed {
                    op::CONVERT_S_TO_F
                } else {
                    op::CONVERT_U_TO_F
                };
                return Ok(self.m.emit(conv, self.f4, &[v]));
            }
        }
        Ok(self.m.emit(op::LOAD, self.f4, &[var]))
    }

    fn builtin_input(&mut self, ty: u32, builtin: u32) -> u32 {
        let p = self.ptr(STORAGE_INPUT, ty);
        let var = self.m.global(p, STORAGE_INPUT);
        self.m.decorate(var, &[DECORATION_BUILTIN, builtin]);
        self.interface.push(var);
        var
    }

    fn frag_coord(&mut self) -> u32 {
        if let Some(v) = self.frag_coord {
            return v;
        }
        let v = self.builtin_input(self.f4, BUILTIN_FRAG_COORD);
        self.frag_coord = Some(v);
        v
    }

    /// The int index of a relative address.
    fn relative_index(&mut self, rel: (Reg, u8)) -> Result<u32, TranslateError> {
        let (reg, comp) = rel;
        let var = match reg.kind {
            R_ADDR => self.reg_var(R_ADDR, 0),
            R_LOOP => self.current_al()?,
            k => return err(format_args!("relative addressing with register type {k}")),
        };
        let v = self.m.emit(op::LOAD, self.i4, &[var]);
        Ok(self
            .m
            .emit(op::COMPOSITE_EXTRACT, self.i, &[v, u32::from(comp)]))
    }

    fn current_al(&mut self) -> Result<u32, TranslateError> {
        for f in self.flow.iter().rev() {
            if let Flow::Loop {
                al: Some((al, _)), ..
            } = f
            {
                return Ok(*al);
            }
        }
        err("aL outside a loop")
    }

    /// The raw `vec4` of a source register (before swizzle and modifier).
    fn raw(&mut self, s: &Src) -> Result<u32, TranslateError> {
        let r = s.reg;
        Ok(match r.kind {
            R_TEMP => {
                let v = self.reg_var(R_TEMP, r.num);
                self.m.emit(op::LOAD, self.f4, &[v])
            }
            R_INPUT => self.load_input(r)?,
            R_ADDR if self.stage() == Stage::Pixel => {
                if self.p.major == 1 {
                    let v = self.reg_var(R_ADDR, r.num);
                    self.m.emit(op::LOAD, self.f4, &[v])
                } else {
                    self.load_input(r)?
                }
            }
            R_ADDR | R_LOOP => {
                let var = if r.kind == R_LOOP {
                    self.current_al()?
                } else {
                    self.reg_var(R_ADDR, 0)
                };
                let v = self.m.emit(op::LOAD, self.i4, &[var]);
                self.m.emit(op::CONVERT_S_TO_F, self.f4, &[v])
            }
            R_CONST => {
                let max: i32 = if self.stage() == Stage::Vertex {
                    255
                } else {
                    223
                };
                match s.relative {
                    None => {
                        // Direct: the device's own programs read beyond c255.
                        let n = r.num.min(constants::COUNT - 1);
                        match self.defs.get(&n).copied() {
                            Some(bits) => self.m.constant_vector(Scalar::Float, &bits),
                            None => self.uniform_at(constants::FLOATS + n),
                        }
                    }
                    Some(rel) => {
                        let a = self.relative_index(rel)?;
                        let base = self.m.constant_i32(i32::try_from(r.num).unwrap_or(0));
                        let sum = self.m.emit(OP_I_ADD, self.i, &[a, base]);
                        let lo = self.m.constant_i32(0);
                        let hi = self.m.constant_i32(max);
                        let c = self.m.glsl(self.i, glsl::S_MAX, &[sum, lo]);
                        let c = self.m.glsl(self.i, glsl::S_MIN, &[c, hi]);
                        self.uniform(c)
                    }
                }
            }
            R_CONSTINT => {
                let v = self.int_constant(r.num)?;
                self.m.emit(op::CONVERT_S_TO_F, self.f4, &[v])
            }
            R_CONSTBOOL => {
                let b = self.bool_constant(r.num);
                let one = self.fconst(1.0);
                let zero = self.fconst(0.0);
                let x = self.m.emit(op::SELECT, self.f, &[b, one, zero]);
                self.splat(x)
            }
            R_MISC => match r.num {
                0 => {
                    // vPos: Direct3D 9 pixel centers are on integers.
                    let var = self.frag_coord();
                    let v = self.m.emit(op::LOAD, self.f4, &[var]);
                    let half = self.f4const([0.5, 0.5, 0.0, 0.0]);
                    self.m.emit(OP_F_SUB, self.f4, &[v, half])
                }
                1 => {
                    let var = match self.front_facing {
                        Some(v) => v,
                        None => {
                            let v = self.builtin_input(self.b, BUILTIN_FRONT_FACING);
                            self.front_facing = Some(v);
                            v
                        }
                    };
                    let front = self.m.emit(op::LOAD, self.b, &[var]);
                    let one = self.fconst(1.0);
                    let minus = self.fconst(-1.0);
                    let x = self.m.emit(op::SELECT, self.f, &[front, one, minus]);
                    self.splat(x)
                }
                n => return err(format_args!("misc register {n}")),
            },
            R_PREDICATE => {
                let var = self.reg_var(R_PREDICATE, 0);
                let p = self.m.emit(op::LOAD, self.b4, &[var]);
                let one = self.f4const([1.0; 4]);
                let zero = self.f4const([0.0; 4]);
                self.m.emit(op::SELECT, self.f4, &[p, one, zero])
            }
            R_COLOROUT | R_RASTOUT | R_ATTROUT | R_OUTPUT => {
                let v = self.reg_var(r.kind, r.num);
                self.m.emit(op::LOAD, self.f4, &[v])
            }
            k => return err(format_args!("source register type {k}")),
        })
    }

    fn int_constant(&mut self, num: u32) -> Result<u32, TranslateError> {
        let n = num.min(15);
        let v = match self.defs.get(&constants::INTS.saturating_add(n)).copied() {
            Some(bits) => return Ok(self.m.constant_vector(Scalar::Int, &bits)),
            None => self.uniform_at(constants::INTS.saturating_add(n)),
        };
        Ok(self.m.emit(op::BITCAST, self.i4, &[v]))
    }

    fn bool_constant(&mut self, num: u32) -> u32 {
        let n = num.min(15);
        if let Some(&b) = self.bool_defs.get(&n) {
            return self.m.constant_bool(b);
        }
        let index = constants::BOOLS.saturating_add(n / 4);
        let v = self.uniform_at(index);
        let u4 = self.m.vector(Scalar::Uint, 4);
        let bits = self.m.emit(op::BITCAST, u4, &[v]);
        let x = self.m.emit(op::COMPOSITE_EXTRACT, self.u, &[bits, n % 4]);
        let zero = self.m.constant_u32(0);
        self.m.emit(op::I_NOT_EQUAL, self.b, &[x, zero])
    }

    /// A source: swizzled and modified.
    fn src(&mut self, s: &Src) -> Result<u32, TranslateError> {
        let raw = self.raw(s)?;
        let sw = s.swizzle.map(u32::from);
        let v = if sw == [0, 1, 2, 3] {
            raw
        } else {
            self.shuffle(raw, sw)
        };
        self.modify(v, s.modifier)
    }

    fn modify(&mut self, v: u32, modifier: u32) -> Result<u32, TranslateError> {
        let f4 = self.f4;
        Ok(match modifier {
            0 => v,
            1 => self.m.emit(op::F_NEGATE, f4, &[v]),
            2 | 3 => {
                let half = self.f4const([0.5; 4]);
                let b = self.m.emit(OP_F_SUB, f4, &[v, half]);
                if modifier == 3 {
                    self.m.emit(op::F_NEGATE, f4, &[b])
                } else {
                    b
                }
            }
            4 | 5 => {
                let two = self.f4const([2.0; 4]);
                let one = self.f4const([1.0; 4]);
                let d = self.m.emit(op::F_MUL, f4, &[v, two]);
                let s = self.m.emit(OP_F_SUB, f4, &[d, one]);
                if modifier == 5 {
                    self.m.emit(op::F_NEGATE, f4, &[s])
                } else {
                    s
                }
            }
            6 => {
                let one = self.f4const([1.0; 4]);
                self.m.emit(OP_F_SUB, f4, &[one, v])
            }
            7 | 8 => {
                let two = self.f4const([2.0; 4]);
                let d = self.m.emit(op::F_MUL, f4, &[v, two]);
                if modifier == 8 {
                    self.m.emit(op::F_NEGATE, f4, &[d])
                } else {
                    d
                }
            }
            9 | 10 => {
                // _dz, _dw (ps_1_4 texld): divide by z or w.
                let c = if modifier == 9 { 2 } else { 3 };
                let d = self.extract(v, c);
                let dd = self.splat(d);
                self.m.emit(op::F_DIV, f4, &[v, dd])
            }
            11 | 12 => {
                let a = self.m.glsl(f4, glsl::F_ABS, &[v]);
                if modifier == 12 {
                    self.m.emit(op::F_NEGATE, f4, &[a])
                } else {
                    a
                }
            }
            m => return err(format_args!("source modifier {m}")),
        })
    }

    /// A boolean source (b#, p0 component), with `NOT`.
    fn src_bool(&mut self, s: &Src) -> Result<u32, TranslateError> {
        let b = match s.reg.kind {
            R_CONSTBOOL => self.bool_constant(s.reg.num),
            R_PREDICATE => {
                let var = self.reg_var(R_PREDICATE, 0);
                let p = self.m.emit(op::LOAD, self.b4, &[var]);
                self.m
                    .emit(op::COMPOSITE_EXTRACT, self.b, &[p, u32::from(s.swizzle[0])])
            }
            k => return err(format_args!("boolean source of register type {k}")),
        };
        Ok(if s.modifier == 13 {
            self.m.emit(OP_LOGICAL_NOT, self.b, &[b])
        } else {
            b
        })
    }

    /// Writes `value` (all four components computed) to `d`.
    fn store(&mut self, d: &Dst, value: u32, pred: Option<&Src>) -> Result<(), TranslateError> {
        let f4 = self.f4;
        let mut value = value;
        if d.shift != 0 {
            let scale = 2f32.powi(i32::from(d.shift));
            let s = self.f4const([scale; 4]);
            value = self.m.emit(op::F_MUL, f4, &[value, s]);
        }
        if d.saturate {
            let zero = self.f4const([0.0; 4]);
            let one = self.f4const([1.0; 4]);
            value = self.m.glsl(f4, GLSL_F_CLAMP, &[value, zero, one]);
        }
        if d.relative.is_some() {
            return unsupported("relative output addressing");
        }
        let r = d.reg;
        let (var, int) = match r.kind {
            R_ADDR if self.stage() == Stage::Vertex => (self.reg_var(R_ADDR, 0), true),
            R_TEMP | R_RASTOUT | R_ATTROUT | R_OUTPUT | R_COLOROUT | R_DEPTHOUT => {
                (self.reg_var(r.kind, r.num), false)
            }
            R_ADDR => (self.reg_var(R_ADDR, r.num), false),
            k => return err(format_args!("destination register type {k}")),
        };
        let mask = if d.mask == 0 { 0xf } else { d.mask };
        let ty = if int { self.i4 } else { f4 };
        if int {
            // a0: rounded to the nearest integer.
            let rounded = self.m.glsl(f4, GLSL_ROUND, &[value]);
            value = self.m.emit(op::CONVERT_F_TO_S, self.i4, &[rounded]);
        }
        let old = if mask != 0xf || pred.is_some() {
            Some(self.m.emit(op::LOAD, ty, &[var]))
        } else {
            None
        };
        if let Some(old) = old {
            if mask != 0xf {
                let comps: Vec<u32> = (0..4)
                    .map(|c| if mask & (1 << c) != 0 { c | 4 } else { c })
                    .collect();
                value = self.m.emit(
                    op::VECTOR_SHUFFLE,
                    ty,
                    &[old, value, comps[0], comps[1], comps[2], comps[3]],
                );
            }
            if let Some(p) = pred {
                let var_p = self.reg_var(R_PREDICATE, 0);
                let pv = self.m.emit(op::LOAD, self.b4, &[var_p]);
                let sw = p.swizzle.map(u32::from);
                let mut cond = self.m.emit(
                    op::VECTOR_SHUFFLE,
                    self.b4,
                    &[pv, pv, sw[0], sw[1], sw[2], sw[3]],
                );
                if p.modifier == 13 {
                    cond = self.m.emit(OP_LOGICAL_NOT, self.b4, &[cond]);
                }
                value = self.m.emit(op::SELECT, ty, &[cond, value, old]);
            }
        }
        self.m.emit_void(op::STORE, &[var, value]);
        Ok(())
    }

    fn dst(ins: &Ins) -> Result<Dst, TranslateError> {
        ins.dst.ok_or(TranslateError("missing destination".into()))
    }

    fn srcs(&mut self, ins: &Ins, n: usize) -> Result<Vec<u32>, TranslateError> {
        if ins.srcs.len() < n {
            return err(format_args!("opcode {} needs {n} sources", ins.op));
        }
        (0..n).map(|k| self.src(&ins.srcs[k])).collect()
    }

    fn emit_result(&mut self, ins: &Ins, value: u32) -> Result<(), TranslateError> {
        let d = Self::dst(ins)?;
        self.store(&d, value, ins.pred.as_ref())
    }

    fn dot(&mut self, a: u32, b: u32, n: u32) -> u32 {
        if n == 4 {
            return self.m.emit(op::DOT, self.f, &[a, b]);
        }
        let v3 = self.m.vector(Scalar::Float, n);
        let sel: Vec<u32> = (0..n).collect();
        let mut oa = vec![a, a];
        oa.extend(&sel);
        let mut ob = vec![b, b];
        ob.extend(&sel);
        let a3 = self.m.emit(op::VECTOR_SHUFFLE, v3, &oa);
        let b3 = self.m.emit(op::VECTOR_SHUFFLE, v3, &ob);
        self.m.emit(op::DOT, self.f, &[a3, b3])
    }

    /// A comparison of `D3DSHADER_COMPARISON` on floats.
    fn compare(&mut self, control: u32, ty: u32, a: u32, b: u32) -> Result<u32, TranslateError> {
        let opcode = match control & 7 {
            1 => OP_F_ORD_GREATER_THAN,
            2 => op::F_ORD_EQUAL,
            3 => op::F_ORD_GREATER_THAN_EQUAL,
            4 => op::F_ORD_LESS_THAN,
            5 => OP_F_ORD_NOT_EQUAL,
            6 => OP_F_ORD_LESS_THAN_EQUAL,
            c => return err(format_args!("comparison {c}")),
        };
        Ok(self.m.emit(opcode, ty, &[a, b]))
    }

    fn block(&mut self) -> u32 {
        let label = self.m.id();
        self.m.emit_void(op::LABEL, &[label]);
        label
    }

    fn begin_if(&mut self, cond: u32) {
        let then = self.m.id();
        let merge = self.m.id();
        let otherwise = self.m.id();
        self.m.emit_void(op::SELECTION_MERGE, &[merge, 0]);
        self.m
            .emit_void(op::BRANCH_CONDITIONAL, &[cond, then, otherwise]);
        self.m.emit_void(op::LABEL, &[then]);
        self.flow.push(Flow::If {
            merge,
            otherwise,
            has_else: false,
        });
    }

    fn loop_merge(&self) -> Result<u32, TranslateError> {
        for f in self.flow.iter().rev() {
            if let Flow::Loop { merge, .. } = f {
                return Ok(*merge);
            }
        }
        err("break outside a loop")
    }

    /// `if (cond) break;`
    fn break_if(&mut self, cond: u32) -> Result<(), TranslateError> {
        let target = self.loop_merge()?;
        let brk = self.m.id();
        let merge = self.m.id();
        self.m.emit_void(op::SELECTION_MERGE, &[merge, 0]);
        self.m
            .emit_void(op::BRANCH_CONDITIONAL, &[cond, brk, merge]);
        self.m.emit_void(op::LABEL, &[brk]);
        self.m.emit_void(op::BRANCH, &[target]);
        self.m.emit_void(op::LABEL, &[merge]);
        Ok(())
    }

    fn begin_loop(&mut self, count: u32, al: Option<(u32, u32)>) {
        let p = self.ptr(STORAGE_FUNCTION, self.i);
        let counter = self.m.local(p);
        let zero = self.m.constant_i32(0);
        self.m.emit_void(op::STORE, &[counter, zero]);
        let header = self.m.id();
        let merge = self.m.id();
        let next = self.m.id();
        let check = self.m.id();
        let body = self.m.id();
        self.m.emit_void(op::BRANCH, &[header]);
        self.m.emit_void(op::LABEL, &[header]);
        self.m.emit_void(op::LOOP_MERGE, &[merge, next, 0]);
        self.m.emit_void(op::BRANCH, &[check]);
        self.m.emit_void(op::LABEL, &[check]);
        let c = self.m.emit(op::LOAD, self.i, &[counter]);
        let go = self.m.emit(OP_S_LESS_THAN, self.b, &[c, count]);
        self.m.emit_void(op::BRANCH_CONDITIONAL, &[go, body, merge]);
        self.m.emit_void(op::LABEL, &[body]);
        self.flow.push(Flow::Loop {
            header,
            merge,
            next,
            counter,
            al,
        });
    }

    fn end_loop(&mut self) -> Result<(), TranslateError> {
        let Some(Flow::Loop {
            header,
            merge,
            next,
            counter,
            al,
            ..
        }) = self.flow.pop()
        else {
            return err("endloop without loop");
        };
        self.m.emit_void(op::BRANCH, &[next]);
        self.m.emit_void(op::LABEL, &[next]);
        let c = self.m.emit(op::LOAD, self.i, &[counter]);
        let one = self.m.constant_i32(1);
        let c1 = self.m.emit(OP_I_ADD, self.i, &[c, one]);
        self.m.emit_void(op::STORE, &[counter, c1]);
        if let Some((al, step)) = al {
            let v = self.m.emit(op::LOAD, self.i4, &[al]);
            let x = self.m.emit(op::COMPOSITE_EXTRACT, self.i, &[v, 0]);
            let x1 = self.m.emit(OP_I_ADD, self.i, &[x, step]);
            let nv = self
                .m
                .emit(op::COMPOSITE_CONSTRUCT, self.i4, &[x1, x1, x1, x1]);
            self.m.emit_void(op::STORE, &[al, nv]);
        }
        self.m.emit_void(op::BRANCH, &[header]);
        self.m.emit_void(op::LABEL, &[merge]);
        Ok(())
    }

    fn sampler(&mut self, num: u32) -> Result<(u32, u32, u32, u32, bool), TranslateError> {
        if let Some(s) = self.samplers.get(&num) {
            return Ok(*s);
        }
        let ty = match self.sampler_types.get(&num) {
            Some(&t) if t != 0 => t,
            _ => match self.options.sampler_types.get(num as usize) {
                Some(&t) if t != 0 => u32::from(t),
                _ => 2,
            },
        };
        // (SPIR-V Dim, VGPU10 dimension, coordinates)
        let (dim, dimension, coords) = match ty {
            2 => (1, 3, 2),
            3 => (3, 6, 3),
            4 => (2, 5, 3),
            t => return unsupported(format_args!("sampler type {t}")),
        };
        let shadow = ty == 2 && self.options.shadow & 1u16.checked_shl(num).unwrap_or(0) != 0;
        let image_ty = self.m.image(&[self.f, dim, u32::from(shadow), 0, 0, 1, 0]);
        let p = self.ptr(STORAGE_UNIFORM_CONSTANT, image_ty);
        let image = self.m.global(p, STORAGE_UNIFORM_CONSTANT);
        self.bind(
            image,
            BindingKind::Resource {
                slot: num,
                dimension,
            },
        );
        let sty = self.m.sampler();
        let p = self.ptr(STORAGE_UNIFORM_CONSTANT, sty);
        let sampler = self.m.global(p, STORAGE_UNIFORM_CONSTANT);
        self.bind(sampler, BindingKind::Sampler { slot: num });
        self.sampler_list.push((num, ty));
        let entry = (image, image_ty, sampler, coords, shadow);
        self.samplers.insert(num, entry);
        Ok(entry)
    }

    /// Samples sampler `num` at `coord` (a `vec4`): `kind` 0 plain, 1
    /// projected, 2 bias (w), 3 lod (w), 4 gradients.
    fn sample(
        &mut self,
        num: u32,
        coord: u32,
        kind: u32,
        grads: Option<(u32, u32)>,
    ) -> Result<u32, TranslateError> {
        let (image, image_ty, sampler, coords, shadow) = self.sampler(num)?;
        let img = self.m.emit(op::LOAD, image_ty, &[image]);
        let sty = self.m.sampler();
        let smp = self.m.emit(op::LOAD, sty, &[sampler]);
        let sity = self.m.sampled_image(image_ty);
        let si = self.m.emit(op::SAMPLED_IMAGE, sity, &[img, smp]);
        let mut coord = coord;
        if kind == 1 {
            let w = self.extract(coord, 3);
            let ww = self.splat(w);
            coord = self.m.emit(op::F_DIV, self.f4, &[coord, ww]);
        }
        let cty = self.m.vector(Scalar::Float, coords);
        let sel: Vec<u32> = (0..coords).collect();
        let mut operands = vec![coord, coord];
        operands.extend(&sel);
        let c = self.m.emit(op::VECTOR_SHUFFLE, cty, &operands);
        let vertex = self.stage() == Stage::Vertex;
        if shadow {
            // A shadow map: z against the depth, filtered (the sampler
            // compares); the result in every component.
            let dref = self.extract(coord, 2);
            let f = self.f;
            let result = match kind {
                2 if !vertex => {
                    let w = self.extract(coord, 3);
                    self.m.emit(
                        op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD,
                        f,
                        &[si, c, dref, IMAGE_BIAS, w],
                    )
                }
                3 | 2 => {
                    let w = self.extract(coord, 3);
                    self.m.emit(
                        op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD,
                        f,
                        &[si, c, dref, IMAGE_LOD, w],
                    )
                }
                _ if vertex || kind == 4 => {
                    let zero = self.fconst(0.0);
                    self.m.emit(
                        op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD,
                        f,
                        &[si, c, dref, IMAGE_LOD, zero],
                    )
                }
                _ => self
                    .m
                    .emit(op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD, f, &[si, c, dref]),
            };
            return Ok(self.splat(result));
        }
        let result = match kind {
            2 if !vertex => {
                let w = self.extract(coord, 3);
                self.m.emit(
                    op::IMAGE_SAMPLE_IMPLICIT_LOD,
                    self.f4,
                    &[si, c, IMAGE_BIAS, w],
                )
            }
            3 | 2 => {
                let w = self.extract(coord, 3);
                self.m.emit(
                    op::IMAGE_SAMPLE_EXPLICIT_LOD,
                    self.f4,
                    &[si, c, IMAGE_LOD, w],
                )
            }
            4 => {
                let (dx, dy) = grads.ok_or(TranslateError("texldd without gradients".into()))?;
                let mut ox = vec![dx, dx];
                ox.extend(&sel);
                let mut oy = vec![dy, dy];
                oy.extend(&sel);
                let gx = self.m.emit(op::VECTOR_SHUFFLE, cty, &ox);
                let gy = self.m.emit(op::VECTOR_SHUFFLE, cty, &oy);
                self.m.emit(
                    op::IMAGE_SAMPLE_EXPLICIT_LOD,
                    self.f4,
                    &[si, c, IMAGE_GRAD, gx, gy],
                )
            }
            _ if vertex => {
                let zero = self.fconst(0.0);
                self.m.emit(
                    op::IMAGE_SAMPLE_EXPLICIT_LOD,
                    self.f4,
                    &[si, c, IMAGE_LOD, zero],
                )
            }
            _ => self
                .m
                .emit(op::IMAGE_SAMPLE_IMPLICIT_LOD, self.f4, &[si, c]),
        };
        Ok(result)
    }

    /// Discards the pixel when any of `value`'s components in `mask` is
    /// negative.
    fn kill_if_negative(&mut self, value: u32, mask: u8) {
        let zero = self.f4const([0.0; 4]);
        let lt = self.m.emit(op::F_ORD_LESS_THAN, self.b4, &[value, zero]);
        let mut any = None;
        for c in 0..4 {
            if mask & (1 << c) == 0 {
                continue;
            }
            let b = self.m.emit(op::COMPOSITE_EXTRACT, self.b, &[lt, c]);
            any = Some(match any {
                None => b,
                Some(a) => self.m.emit(166, self.b, &[a, b]),
            });
        }
        if let Some(cond) = any {
            self.kill_if(cond);
        }
    }

    fn kill_if(&mut self, cond: u32) {
        let kill = self.m.id();
        let merge = self.m.id();
        self.m.emit_void(op::SELECTION_MERGE, &[merge, 0]);
        self.m
            .emit_void(op::BRANCH_CONDITIONAL, &[cond, kill, merge]);
        self.m.emit_void(op::LABEL, &[kill]);
        self.m.emit_void(op::KILL, &[]);
        self.m.emit_void(op::LABEL, &[merge]);
    }

    fn instruction(&mut self, ins: &Ins) -> Result<(), TranslateError> {
        let f4 = self.f4;
        let f = self.f;
        let major = self.p.major;
        match ins.op {
            o::NOP | o::DCL | o::DEF | o::DEFI | o::DEFB | o::PHASE => {}
            o::MOV | o::MOVA => {
                let s = self.srcs(ins, 1)?;
                self.emit_result(ins, s[0])?;
            }
            o::ADD | o::SUB | o::MUL | o::MIN | o::MAX => {
                let s = self.srcs(ins, 2)?;
                let v = match ins.op {
                    o::ADD => self.m.emit(op::F_ADD, f4, &[s[0], s[1]]),
                    o::SUB => self.m.emit(OP_F_SUB, f4, &[s[0], s[1]]),
                    o::MUL => self.m.emit(op::F_MUL, f4, &[s[0], s[1]]),
                    o::MIN => self.m.glsl(f4, GLSL_F_MIN, &[s[0], s[1]]),
                    _ => self.m.glsl(f4, GLSL_F_MAX, &[s[0], s[1]]),
                };
                self.emit_result(ins, v)?;
            }
            o::MAD => {
                let s = self.srcs(ins, 3)?;
                let m = self.m.emit(op::F_MUL, f4, &[s[0], s[1]]);
                let v = self.m.emit(op::F_ADD, f4, &[m, s[2]]);
                self.emit_result(ins, v)?;
            }
            o::RCP | o::RSQ | o::EXP | o::LOG | o::EXPP | o::LOGP => {
                let s = self.srcs(ins, 1)?;
                let x = self.extract(s[0], 0);
                let one = self.fconst(1.0);
                let r = match ins.op {
                    o::RCP => self.m.emit(op::F_DIV, f, &[one, x]),
                    o::RSQ => {
                        let a = self.m.glsl(f, glsl::F_ABS, &[x]);
                        self.m.glsl(f, glsl::INVERSE_SQRT, &[a])
                    }
                    o::EXP => self.m.glsl(f, glsl::EXP2, &[x]),
                    o::EXPP if major < 2 => {
                        // (2^floor(x), fract(x), 2^x, 1).
                        let fl = self.m.glsl(f, glsl::FLOOR, &[x]);
                        let a = self.m.glsl(f, glsl::EXP2, &[fl]);
                        let b = self.m.glsl(f, glsl::FRACT, &[x]);
                        let c = self.m.glsl(f, glsl::EXP2, &[x]);
                        let v = self.m.emit(op::COMPOSITE_CONSTRUCT, f4, &[a, b, c, one]);
                        return self.emit_result(ins, v);
                    }
                    o::EXPP => self.m.glsl(f, glsl::EXP2, &[x]),
                    _ => {
                        let a = self.m.glsl(f, glsl::F_ABS, &[x]);
                        self.m.glsl(f, glsl::LOG2, &[a])
                    }
                };
                let v = self.splat(r);
                self.emit_result(ins, v)?;
            }
            o::DP3 | o::DP4 => {
                let s = self.srcs(ins, 2)?;
                let n = if ins.op == o::DP3 { 3 } else { 4 };
                let d = self.dot(s[0], s[1], n);
                let v = self.splat(d);
                self.emit_result(ins, v)?;
            }
            o::DP2ADD => {
                let s = self.srcs(ins, 3)?;
                let d = self.dot(s[0], s[1], 2);
                let c = self.extract(s[2], 0);
                let r = self.m.emit(op::F_ADD, f, &[d, c]);
                let v = self.splat(r);
                self.emit_result(ins, v)?;
            }
            o::SLT | o::SGE => {
                let s = self.srcs(ins, 2)?;
                let opcode = if ins.op == o::SLT {
                    op::F_ORD_LESS_THAN
                } else {
                    op::F_ORD_GREATER_THAN_EQUAL
                };
                let c = self.m.emit(opcode, self.b4, &[s[0], s[1]]);
                let one = self.f4const([1.0; 4]);
                let zero = self.f4const([0.0; 4]);
                let v = self.m.emit(op::SELECT, f4, &[c, one, zero]);
                self.emit_result(ins, v)?;
            }
            o::LIT => {
                let s = self.srcs(ins, 1)?;
                let x = self.extract(s[0], 0);
                let y = self.extract(s[0], 1);
                let w = self.extract(s[0], 3);
                let zero = self.fconst(0.0);
                let one = self.fconst(1.0);
                let lo = self.fconst(-128.0);
                let hi = self.fconst(128.0);
                let dy = self.m.glsl(f, GLSL_F_MAX, &[x, zero]);
                let ym = self.m.glsl(f, GLSL_F_MAX, &[y, zero]);
                let p = self.m.glsl(f, GLSL_F_CLAMP, &[w, lo, hi]);
                let pw = self.m.glsl(f, GLSL_POW, &[ym, p]);
                let pos = self.m.emit(OP_F_ORD_GREATER_THAN, self.b, &[x, zero]);
                let z = self.m.emit(op::SELECT, f, &[pos, pw, zero]);
                let v = self.m.emit(op::COMPOSITE_CONSTRUCT, f4, &[one, dy, z, one]);
                self.emit_result(ins, v)?;
            }
            o::DST => {
                let s = self.srcs(ins, 2)?;
                let one = self.fconst(1.0);
                let y0 = self.extract(s[0], 1);
                let y1 = self.extract(s[1], 1);
                let y = self.m.emit(op::F_MUL, f, &[y0, y1]);
                let z = self.extract(s[0], 2);
                let w = self.extract(s[1], 3);
                let v = self.m.emit(op::COMPOSITE_CONSTRUCT, f4, &[one, y, z, w]);
                self.emit_result(ins, v)?;
            }
            o::LRP => {
                let s = self.srcs(ins, 3)?;
                let d = self.m.emit(OP_F_SUB, f4, &[s[1], s[2]]);
                let m = self.m.emit(op::F_MUL, f4, &[s[0], d]);
                let v = self.m.emit(op::F_ADD, f4, &[m, s[2]]);
                self.emit_result(ins, v)?;
            }
            o::FRC => {
                let s = self.srcs(ins, 1)?;
                let v = self.m.glsl(f4, glsl::FRACT, &[s[0]]);
                self.emit_result(ins, v)?;
            }
            o::ABS => {
                let s = self.srcs(ins, 1)?;
                let v = self.m.glsl(f4, glsl::F_ABS, &[s[0]]);
                self.emit_result(ins, v)?;
            }
            o::SGN => {
                let s = self.srcs(ins, 1)?;
                let v = self.m.glsl(f4, GLSL_F_SIGN, &[s[0]]);
                self.emit_result(ins, v)?;
            }
            o::POW => {
                let s = self.srcs(ins, 2)?;
                let x = self.extract(s[0], 0);
                let y = self.extract(s[1], 0);
                let a = self.m.glsl(f, glsl::F_ABS, &[x]);
                let p = self.m.glsl(f, GLSL_POW, &[a, y]);
                let v = self.splat(p);
                self.emit_result(ins, v)?;
            }
            o::CRS => {
                let s = self.srcs(ins, 2)?;
                let v3 = self.m.vector(Scalar::Float, 3);
                let a = self.m.emit(op::VECTOR_SHUFFLE, v3, &[s[0], s[0], 0, 1, 2]);
                let b = self.m.emit(op::VECTOR_SHUFFLE, v3, &[s[1], s[1], 0, 1, 2]);
                let c = self.m.glsl(v3, GLSL_CROSS, &[a, b]);
                let zero = self.fconst(0.0);
                let x = self.m.emit(op::COMPOSITE_EXTRACT, f, &[c, 0]);
                let y = self.m.emit(op::COMPOSITE_EXTRACT, f, &[c, 1]);
                let z = self.m.emit(op::COMPOSITE_EXTRACT, f, &[c, 2]);
                let v = self.m.emit(op::COMPOSITE_CONSTRUCT, f4, &[x, y, z, zero]);
                self.emit_result(ins, v)?;
            }
            o::NRM => {
                let s = self.srcs(ins, 1)?;
                let d = self.dot(s[0], s[0], 3);
                let r = self.m.glsl(f, glsl::INVERSE_SQRT, &[d]);
                let rr = self.splat(r);
                let v = self.m.emit(op::F_MUL, f4, &[s[0], rr]);
                self.emit_result(ins, v)?;
            }
            o::SINCOS => {
                let s = self.srcs(ins, 1)?;
                // A replicate swizzle selects the angle.
                let x = self.extract(s[0], 0);
                let c = self.m.glsl(f, glsl::COS, &[x]);
                let sn = self.m.glsl(f, glsl::SIN, &[x]);
                let zero = self.fconst(0.0);
                let v = self
                    .m
                    .emit(op::COMPOSITE_CONSTRUCT, f4, &[c, sn, zero, zero]);
                self.emit_result(ins, v)?;
            }
            o::M4X4 | o::M4X3 | o::M3X4 | o::M3X3 | o::M3X2 => {
                let (n, rows) = match ins.op {
                    o::M4X4 => (4, 4),
                    o::M4X3 => (4, 3),
                    o::M3X4 => (3, 4),
                    o::M3X3 => (3, 3),
                    _ => (3, 2),
                };
                if ins.srcs.len() < 2 {
                    return err("matrix instruction without a matrix");
                }
                let a = self.src(&ins.srcs[0])?;
                let zero = self.fconst(0.0);
                let mut parts = vec![zero; 4];
                for (row, part) in parts.iter_mut().enumerate().take(rows) {
                    let mut m = ins.srcs[1];
                    m.reg.num = m.reg.num.saturating_add(u32::try_from(row).unwrap_or(0));
                    let b = self.src(&m)?;
                    *part = self.dot(a, b, n);
                }
                let v = self.m.emit(op::COMPOSITE_CONSTRUCT, f4, &parts);
                self.emit_result(ins, v)?;
            }
            o::CMP => {
                let s = self.srcs(ins, 3)?;
                let zero = self.f4const([0.0; 4]);
                let c = self
                    .m
                    .emit(op::F_ORD_GREATER_THAN_EQUAL, self.b4, &[s[0], zero]);
                let v = self.m.emit(op::SELECT, f4, &[c, s[1], s[2]]);
                self.emit_result(ins, v)?;
            }
            o::CND => {
                let s = self.srcs(ins, 3)?;
                let half = self.f4const([0.5; 4]);
                let cond = if self.p.minor < 4 {
                    // ps_1_1 to 1_3: r0.a only.
                    let a = self.extract(s[0], 3);
                    self.splat(a)
                } else {
                    s[0]
                };
                let c = self.m.emit(OP_F_ORD_GREATER_THAN, self.b4, &[cond, half]);
                let v = self.m.emit(op::SELECT, f4, &[c, s[1], s[2]]);
                self.emit_result(ins, v)?;
            }
            o::DSX | o::DSY => {
                let s = self.srcs(ins, 1)?;
                let opcode = if ins.op == o::DSX { op::DPDX } else { op::DPDY };
                let v = self.m.emit(opcode, f4, &[s[0]]);
                self.emit_result(ins, v)?;
            }
            o::BEM => {
                // ps_1_4: src0.xy plus the bump matrix of the destination's
                // stage times src1.xy.
                let s = self.srcs(ins, 2)?;
                let d = Self::dst(ins)?;
                let v = self.bump(d.reg.num, s[0], s[1]);
                self.emit_result(ins, v)?;
            }
            o::SETP => {
                let s = self.srcs(ins, 2)?;
                let c = self.compare(ins.control, self.b4, s[0], s[1])?;
                let d = Self::dst(ins)?;
                let var = self.reg_var(R_PREDICATE, 0);
                let old = self.m.emit(op::LOAD, self.b4, &[var]);
                let mask = if d.mask == 0 { 0xf } else { d.mask };
                let comps: Vec<u32> = (0..4)
                    .map(|k| if mask & (1 << k) != 0 { k | 4 } else { k })
                    .collect();
                let v = self.m.emit(
                    op::VECTOR_SHUFFLE,
                    self.b4,
                    &[old, c, comps[0], comps[1], comps[2], comps[3]],
                );
                self.m.emit_void(op::STORE, &[var, v]);
            }
            o::TEX if self.stage() == Stage::Pixel && major == 1 => self.tex_1x(ins)?,
            o::TEXCOORD => self.texcoord_1x(ins)?,
            o::TEX => {
                // texld, texldp, texldb.
                if ins.srcs.len() < 2 {
                    return err("texld needs a coordinate and a sampler");
                }
                let coord = self.src(&ins.srcs[0])?;
                let num = ins.srcs[1].reg.num;
                let kind = match ins.control & 3 {
                    1 => 1,
                    2 => 2,
                    _ => 0,
                };
                let v = self.sample(num, coord, kind, None)?;
                let sw = ins.srcs[1].swizzle.map(u32::from);
                let v = if sw == [0, 1, 2, 3] {
                    v
                } else {
                    self.shuffle(v, sw)
                };
                self.emit_result(ins, v)?;
            }
            o::TEXLDL => {
                let coord = match ins.srcs.first() {
                    Some(s) => self.src(s)?,
                    None => return err("texldl without coordinates"),
                };
                let num = ins.srcs.get(1).map_or(0, |s| s.reg.num);
                let v = self.sample(num, coord, 3, None)?;
                self.emit_result(ins, v)?;
            }
            o::TEXLDD => {
                if ins.srcs.len() < 4 {
                    return err("texldd needs four sources");
                }
                let coord = self.src(&ins.srcs[0])?;
                let num = ins.srcs[1].reg.num;
                let dx = self.src(&ins.srcs[2])?;
                let dy = self.src(&ins.srcs[3])?;
                let v = self.sample(num, coord, 4, Some((dx, dy)))?;
                self.emit_result(ins, v)?;
            }
            o::TEXKILL => {
                let d = Self::dst(ins)?;
                let src = Src {
                    reg: d.reg,
                    swizzle: [0, 1, 2, 3],
                    modifier: 0,
                    relative: None,
                };
                let v = if major == 1 && d.reg.kind == R_ADDR && self.p.minor < 4 {
                    // ps_1_1 to 1_3: the texture coordinates of t#.
                    self.load_input(d.reg)?
                } else {
                    self.raw(&src)?
                };
                let mask = if major == 1 {
                    0x7
                } else if d.mask == 0 {
                    0xf
                } else {
                    d.mask
                };
                self.kill_if_negative(v, mask);
            }
            o::IF => {
                let s = ins
                    .srcs
                    .first()
                    .ok_or(TranslateError("if without condition".into()))?;
                let c = self.src_bool(s)?;
                self.begin_if(c);
            }
            o::IFC => {
                let s = self.srcs(ins, 2)?;
                let a = self.extract(s[0], 0);
                let b = self.extract(s[1], 0);
                let c = self.compare(ins.control, self.b, a, b)?;
                self.begin_if(c);
            }
            o::ELSE => {
                let Some(Flow::If {
                    merge,
                    otherwise,
                    has_else,
                }) = self.flow.last_mut()
                else {
                    return err("else without if");
                };
                if *has_else {
                    return err("two else");
                }
                *has_else = true;
                let (merge, otherwise) = (*merge, *otherwise);
                self.m.emit_void(op::BRANCH, &[merge]);
                self.m.emit_void(op::LABEL, &[otherwise]);
            }
            o::ENDIF => self.end_if()?,
            o::REP => {
                let s = ins
                    .srcs
                    .first()
                    .ok_or(TranslateError("rep without count".into()))?;
                let iv = self.int_constant(s.reg.num)?;
                let count = self.m.emit(op::COMPOSITE_EXTRACT, self.i, &[iv, 0]);
                self.begin_loop(count, None);
            }
            o::LOOP => {
                let s = ins
                    .srcs
                    .get(1)
                    .ok_or(TranslateError("loop without count".into()))?;
                let iv = self.int_constant(s.reg.num)?;
                let count = self.m.emit(op::COMPOSITE_EXTRACT, self.i, &[iv, 0]);
                let start = self.m.emit(op::COMPOSITE_EXTRACT, self.i, &[iv, 1]);
                let step = self.m.emit(op::COMPOSITE_EXTRACT, self.i, &[iv, 2]);
                // Each loop has its own aL.
                let p = self.ptr(STORAGE_FUNCTION, self.i4);
                let al = self.m.local(p);
                let v = self.m.emit(
                    op::COMPOSITE_CONSTRUCT,
                    self.i4,
                    &[start, start, start, start],
                );
                self.m.emit_void(op::STORE, &[al, v]);
                self.begin_loop(count, Some((al, step)));
            }
            o::ENDLOOP | o::ENDREP => self.end_loop()?,
            o::BREAK => {
                let target = self.loop_merge()?;
                self.m.emit_void(op::BRANCH, &[target]);
                self.block();
            }
            o::BREAKC => {
                let s = self.srcs(ins, 2)?;
                let a = self.extract(s[0], 0);
                let b = self.extract(s[1], 0);
                let c = self.compare(ins.control, self.b, a, b)?;
                self.break_if(c)?;
            }
            o::BREAKP => {
                let s = ins
                    .srcs
                    .first()
                    .ok_or(TranslateError("breakp without predicate".into()))?;
                let c = self.src_bool(s)?;
                self.break_if(c)?;
            }
            o::RET | o::LABEL => {
                // Handled by `run`.
            }
            o::CALL | o::CALLNZ => self.call(ins)?,
            o::TEXBEM..=o::TEXM3X3VSPEC
            | o::TEXREG2RGB
            | o::TEXDP3TEX
            | o::TEXM3X2DEPTH
            | o::TEXDP3
            | o::TEXM3X3
            | o::TEXDEPTH => self.texture_op(ins)?,
            op => return unsupported(format_args!("opcode {op}")),
        }
        Ok(())
    }

    /// Translates instructions up to the end of main or of a subroutine
    /// (`ret`, `label`); subroutine calls are inlined.
    fn run(&mut self, list: &[Ins]) -> Result<(), TranslateError> {
        for ins in list {
            if matches!(ins.op, o::RET | o::LABEL) {
                return Ok(());
            }
            self.instruction(ins)?;
        }
        Ok(())
    }

    /// `call l#` and `callnz l#, b#/p0`: the subroutine's body, inlined.
    fn call(&mut self, ins: &Ins) -> Result<(), TranslateError> {
        let label = ins
            .srcs
            .first()
            .ok_or(TranslateError("call without label".into()))?
            .reg
            .num;
        let program = self.p;
        let start = program
            .ins
            .iter()
            .position(|i| i.op == o::LABEL && i.srcs.first().is_some_and(|s| s.reg.num == label))
            .ok_or_else(|| TranslateError(format!("label l{label} is not defined")))?;
        if self.calls >= 8 {
            return err("subroutine calls nested too deep");
        }
        self.calls = self.calls.saturating_add(1);
        let depth = self.flow.len();
        let conditional = ins.op == o::CALLNZ;
        if conditional {
            let s = ins
                .srcs
                .get(1)
                .ok_or(TranslateError("callnz without condition".into()))?;
            let c = self.src_bool(s)?;
            self.begin_if(c);
        }
        let body = &program.ins[start.saturating_add(1)..];
        self.run(body)?;
        if self.flow.len() != depth.saturating_add(usize::from(conditional)) {
            return unsupported("a subroutine returning inside flow control");
        }
        if conditional {
            self.end_if()?;
        }
        self.calls = self.calls.saturating_sub(1);
        Ok(())
    }

    fn end_if(&mut self) -> Result<(), TranslateError> {
        let Some(Flow::If {
            merge,
            otherwise,
            has_else,
        }) = self.flow.pop()
        else {
            return err("endif without if");
        };
        self.m.emit_void(op::BRANCH, &[merge]);
        if !has_else {
            self.m.emit_void(op::LABEL, &[otherwise]);
            self.m.emit_void(op::BRANCH, &[merge]);
        }
        self.m.emit_void(op::LABEL, &[merge]);
        Ok(())
    }

    /// `xy + M * d.xy`, with `M` the bump matrix of texture stage `stage`.
    fn bump(&mut self, stage: u32, xy: u32, d: u32) -> u32 {
        let m = self.uniform_at(constants::BUMP_ENV.saturating_add(stage.min(7)));
        let f = self.f;
        let (m00, m01, m10, m11) = (
            self.extract(m, 0),
            self.extract(m, 1),
            self.extract(m, 2),
            self.extract(m, 3),
        );
        let du = self.extract(d, 0);
        let dv = self.extract(d, 1);
        let x = self.extract(xy, 0);
        let y = self.extract(xy, 1);
        let a = self.m.emit(op::F_MUL, f, &[m00, du]);
        let b = self.m.emit(op::F_MUL, f, &[m10, dv]);
        let ab = self.m.emit(op::F_ADD, f, &[a, b]);
        let u = self.m.emit(op::F_ADD, f, &[x, ab]);
        let c = self.m.emit(op::F_MUL, f, &[m01, du]);
        let e = self.m.emit(op::F_MUL, f, &[m11, dv]);
        let ce = self.m.emit(op::F_ADD, f, &[c, e]);
        let v = self.m.emit(op::F_ADD, f, &[y, ce]);
        let z = self.extract(xy, 2);
        let w = self.extract(xy, 3);
        self.m.emit(op::COMPOSITE_CONSTRUCT, self.f4, &[u, v, z, w])
    }

    /// The texture addressing instructions of ps_1_1 to 1_3 (and
    /// `texdepth` of 1_4): `dst` is t#, its texture coordinates and stage
    /// are #; `src` is a t# read before.
    fn texture_op(&mut self, ins: &Ins) -> Result<(), TranslateError> {
        let d = Self::dst(ins)?;
        let n = d.reg.num;
        let f = self.f;
        let f4 = self.f4;
        let tc = |tx: &mut Self| {
            tx.load_input(Reg {
                kind: R_ADDR,
                num: n,
            })
        };
        if ins.op == o::TEXDEPTH {
            // depth = r5.r / r5.g (1 when g is 0).
            let v = self.raw(&Src {
                reg: d.reg,
                swizzle: [0, 1, 2, 3],
                modifier: 0,
                relative: None,
            })?;
            let r = self.extract(v, 0);
            let g = self.extract(v, 1);
            let q = self.m.emit(op::F_DIV, f, &[r, g]);
            let zero = self.fconst(0.0);
            let one = self.fconst(1.0);
            let nz = self.m.emit(OP_F_ORD_NOT_EQUAL, self.b, &[g, zero]);
            let z = self.m.emit(op::SELECT, f, &[nz, q, one]);
            return self.write_depth(z);
        }
        let src = ins
            .srcs
            .first()
            .ok_or(TranslateError("texture instruction without source".into()))?;
        let t = self.src(src)?;
        let dot3 = |tx: &mut Self| -> Result<u32, TranslateError> {
            let c = tc(tx)?;
            Ok(tx.dot(c, t, 3))
        };
        let result = match ins.op {
            o::TEXBEM | o::TEXBEML => {
                let c = tc(self)?;
                let coord = self.bump(n, c, t);
                let mut v = self.sample(n, coord, 0, None)?;
                if ins.op == o::TEXBEML {
                    let l = self.uniform_at(constants::BUMP_LUMINANCE.saturating_add(n.min(7)));
                    let scale = self.extract(l, 0);
                    let offset = self.extract(l, 1);
                    let b = self.extract(t, 2);
                    let lum = self.m.emit(op::F_MUL, f, &[b, scale]);
                    let lum = self.m.emit(op::F_ADD, f, &[lum, offset]);
                    let one = self.fconst(1.0);
                    let factor = self
                        .m
                        .emit(op::COMPOSITE_CONSTRUCT, f4, &[lum, lum, lum, one]);
                    v = self.m.emit(op::F_MUL, f4, &[v, factor]);
                }
                v
            }
            o::TEXREG2AR | o::TEXREG2GB | o::TEXREG2RGB => {
                let s = match ins.op {
                    o::TEXREG2AR => [3, 0, 2, 2],
                    o::TEXREG2GB => [1, 2, 2, 2],
                    _ => [0, 1, 2, 2],
                };
                let coord = self.shuffle(t, s);
                self.sample(n, coord, 0, None)?
            }
            o::TEXM3X2PAD | o::TEXM3X3PAD => {
                let dp = dot3(self)?;
                self.pads.push(dp);
                // A pad's register keeps nothing useful.
                return Ok(());
            }
            o::TEXDP3TEX => {
                let u = dot3(self)?;
                let zero = self.fconst(0.0);
                let coord = self
                    .m
                    .emit(op::COMPOSITE_CONSTRUCT, f4, &[u, zero, zero, zero]);
                self.sample(n, coord, 0, None)?
            }
            o::TEXDP3 => {
                let u = dot3(self)?;
                self.splat(u)
            }
            o::TEXM3X2TEX | o::TEXM3X2DEPTH => {
                let last = dot3(self)?;
                let Some(first) = self.pads.pop() else {
                    return err("texm3x2 without texm3x2pad");
                };
                self.pads.clear();
                if ins.op == o::TEXM3X2DEPTH {
                    // z / w, 1 when w is 0.
                    let q = self.m.emit(op::F_DIV, f, &[first, last]);
                    let zero = self.fconst(0.0);
                    let one = self.fconst(1.0);
                    let nz = self.m.emit(OP_F_ORD_NOT_EQUAL, self.b, &[last, zero]);
                    let z = self.m.emit(op::SELECT, f, &[nz, q, one]);
                    return self.write_depth(z);
                }
                let zero = self.fconst(0.0);
                let coord = self
                    .m
                    .emit(op::COMPOSITE_CONSTRUCT, f4, &[first, last, zero, zero]);
                self.sample(n, coord, 0, None)?
            }
            o::TEXM3X3TEX | o::TEXM3X3SPEC | o::TEXM3X3VSPEC | o::TEXM3X3 => {
                let third = dot3(self)?;
                if self.pads.len() < 2 {
                    return err("texm3x3 without two texm3x3pad");
                }
                let second = self.pads.pop().unwrap_or(third);
                let first = self.pads.pop().unwrap_or(third);
                self.pads.clear();
                let one = self.fconst(1.0);
                let normal = self
                    .m
                    .emit(op::COMPOSITE_CONSTRUCT, f4, &[first, second, third, one]);
                match ins.op {
                    o::TEXM3X3 => normal,
                    o::TEXM3X3TEX => self.sample(n, normal, 0, None)?,
                    _ => {
                        // The eye vector: a constant, or the w of the three
                        // texture coordinates.
                        let eye = if ins.op == o::TEXM3X3SPEC {
                            let e = ins
                                .srcs
                                .get(1)
                                .ok_or(TranslateError("texm3x3spec without eye".into()))?;
                            self.src(e)?
                        } else {
                            let mut ws = Vec::new();
                            for k in [n.saturating_sub(2), n.saturating_sub(1), n] {
                                let c = self.load_input(Reg {
                                    kind: R_ADDR,
                                    num: k,
                                })?;
                                ws.push(self.extract(c, 3));
                            }
                            let zero = self.fconst(0.0);
                            self.m
                                .emit(op::COMPOSITE_CONSTRUCT, f4, &[ws[0], ws[1], ws[2], zero])
                        };
                        // R = 2 N (N.E) / (N.N) - E.
                        let ne = self.dot(normal, eye, 3);
                        let nn = self.dot(normal, normal, 3);
                        let two = self.fconst(2.0);
                        let k = self.m.emit(op::F_MUL, f, &[two, ne]);
                        let k = self.m.emit(op::F_DIV, f, &[k, nn]);
                        let kk = self.splat(k);
                        let scaled = self.m.emit(op::F_MUL, f4, &[normal, kk]);
                        let r = self.m.emit(OP_F_SUB, f4, &[scaled, eye]);
                        self.sample(n, r, 0, None)?
                    }
                }
            }
            op => return unsupported(format_args!("texture opcode {op}")),
        };
        self.store(&d, result, None)
    }

    /// Point size and clip distances.
    fn vertex_extras(&mut self) -> Result<(), TranslateError> {
        let f = self.f;
        if self.options.points {
            let params = self.uniform_at(constants::POINT);
            let size = self.extract(params, 0);
            let min = self.extract(params, 1);
            let max = self.extract(params, 2);
            match self
                .scalar_sinks
                .iter()
                .find(|(l, v)| self.point_size_var == Some(*v) && *l != 0)
            {
                Some(&(local, _)) => {
                    // Written by the program: clamped.
                    let v = self.m.emit(op::LOAD, self.f4, &[local]);
                    let x = self.extract(v, 0);
                    let c = self.m.glsl(f, GLSL_F_CLAMP, &[x, min, max]);
                    let cv = self.splat(c);
                    self.m.emit_void(op::STORE, &[local, cv]);
                }
                None => {
                    let p = self.ptr(STORAGE_OUTPUT, f);
                    let iv = self.m.global(p, STORAGE_OUTPUT);
                    self.m
                        .decorate(iv, &[DECORATION_BUILTIN, BUILTIN_POINT_SIZE]);
                    self.interface.push(iv);
                    let c = self.m.glsl(f, GLSL_F_CLAMP, &[size, min, max]);
                    self.m.emit_void(op::STORE, &[iv, c]);
                    self.point_size = true;
                }
            }
        }
        let planes = self.options.clip_planes & 0x3f;
        if planes != 0 {
            let Some(position) = self.position else {
                return Ok(());
            };
            self.m.capability(CAP_CLIP_DISTANCE);
            let count = planes.count_ones();
            let array = self.m.array(f, count);
            let p = self.ptr(STORAGE_OUTPUT, array);
            let var = self.m.global(p, STORAGE_OUTPUT);
            self.m
                .decorate(var, &[DECORATION_BUILTIN, BUILTIN_CLIP_DISTANCE]);
            self.interface.push(var);
            let pos = self.m.emit(op::LOAD, self.f4, &[position]);
            let pf = self.ptr(STORAGE_OUTPUT, f);
            let mut k = 0u32;
            for plane in 0..6u32 {
                if planes & (1 << plane) == 0 {
                    continue;
                }
                let eq = self.uniform_at(constants::CLIP_PLANES.saturating_add(plane));
                let d = self.m.emit(op::DOT, f, &[pos, eq]);
                let index = self.m.constant_u32(k);
                let slot = self.m.emit(op::ACCESS_CHAIN, pf, &[var, index]);
                self.m.emit_void(op::STORE, &[slot, d]);
                k = k.saturating_add(1);
            }
        }
        Ok(())
    }

    fn write_depth(&mut self, z: u32) -> Result<(), TranslateError> {
        let var = self.reg_var(R_DEPTHOUT, 0);
        let v = self.splat(z);
        self.m.emit_void(op::STORE, &[var, v]);
        Ok(())
    }

    /// ps_1_1 to 1_3 `tex t#`: samples stage # with texture coordinates #;
    /// ps_1_4 `texld r#, t#/r#`: samples stage # with the source.
    fn tex_1x(&mut self, ins: &Ins) -> Result<(), TranslateError> {
        let d = Self::dst(ins)?;
        let coord = if self.p.minor >= 4 {
            let s = ins
                .srcs
                .first()
                .ok_or(TranslateError("texld without source".into()))?;
            if s.reg.kind == R_ADDR {
                let v = self.load_input(s.reg)?;
                self.modify(v, s.modifier)?
            } else {
                self.src(s)?
            }
        } else {
            self.load_input(d.reg)?
        };
        let v = self.sample(d.reg.num, coord, 0, None)?;
        self.store(&d, v, None)
    }

    /// `texcoord t#` (ps_1_1 to 1_3: the coordinates, clamped) or `texcrd`
    /// (ps_1_4).
    fn texcoord_1x(&mut self, ins: &Ins) -> Result<(), TranslateError> {
        let d = Self::dst(ins)?;
        let v = if self.p.minor >= 4 {
            let s = ins
                .srcs
                .first()
                .ok_or(TranslateError("texcrd without source".into()))?;
            let v = self.load_input(s.reg)?;
            let sw = s.swizzle.map(u32::from);
            let v = self.shuffle(v, sw);
            self.modify(v, s.modifier)?
        } else {
            let v = self.load_input(d.reg)?;
            let zero = self.f4const([0.0; 4]);
            let one = self.f4const([1.0; 4]);
            let c = self.m.glsl(self.f4, GLSL_F_CLAMP, &[v, zero, one]);
            let x = self.extract(c, 0);
            let y = self.extract(c, 1);
            let z = self.extract(c, 2);
            let one = self.fconst(1.0);
            self.m
                .emit(op::COMPOSITE_CONSTRUCT, self.f4, &[x, y, z, one])
        };
        self.store(&d, v, None)
    }

    /// Fog and alpha test on oC0, then the outputs to the interface.
    fn finish_outputs(&mut self) -> Result<(), TranslateError> {
        let f = self.f;
        let f4 = self.f4;
        if self.stage() == Stage::Pixel
            && let Some(&oc) = self.regs.get(&(R_COLOROUT, 0))
        {
            if self.options.fog != Fog::None && self.p.major < 3 {
                let color = self.m.emit(op::LOAD, f4, &[oc]);
                let params = self.uniform_at(constants::ALPHA_FOG);
                let start = self.extract(params, 1);
                let end = self.extract(params, 2);
                let density = self.extract(params, 3);
                let factor = match self.options.fog {
                    Fog::Vertex => {
                        let var = match self.fog_input {
                            Some(v) => v,
                            None => {
                                // The vertex's fog factor: location 18.
                                let p = self.ptr(STORAGE_INPUT, f4);
                                let var = self.m.global(p, STORAGE_INPUT);
                                self.m.decorate(var, &[DECORATION_LOCATION, 18]);
                                self.interface.push(var);
                                self.input_locations.push(18);
                                self.fog_input = Some(var);
                                var
                            }
                        };
                        let fog = self.m.emit(op::LOAD, f4, &[var]);
                        self.extract(fog, 0)
                    }
                    mode => {
                        let fc = self.frag_coord();
                        let v = self.m.emit(op::LOAD, f4, &[fc]);
                        let z = self.extract(v, 2);
                        match mode {
                            Fog::Linear => {
                                let a = self.m.emit(OP_F_SUB, f, &[end, z]);
                                let b = self.m.emit(OP_F_SUB, f, &[end, start]);
                                self.m.emit(op::F_DIV, f, &[a, b])
                            }
                            Fog::Exp => {
                                let dz = self.m.emit(op::F_MUL, f, &[density, z]);
                                let n = self.m.emit(op::F_NEGATE, f, &[dz]);
                                self.m.glsl(f, GLSL_EXP, &[n])
                            }
                            _ => {
                                let dz = self.m.emit(op::F_MUL, f, &[density, z]);
                                let sq = self.m.emit(op::F_MUL, f, &[dz, dz]);
                                let n = self.m.emit(op::F_NEGATE, f, &[sq]);
                                self.m.glsl(f, GLSL_EXP, &[n])
                            }
                        }
                    }
                };
                let zero = self.fconst(0.0);
                let one = self.fconst(1.0);
                let factor = self.m.glsl(f, GLSL_F_CLAMP, &[factor, zero, one]);
                let fog_color = self.uniform_at(constants::FOG_COLOR);
                let t = self
                    .m
                    .emit(op::COMPOSITE_CONSTRUCT, f4, &[factor, factor, factor, one]);
                let mixed = self.m.glsl(f4, GLSL_F_MIX, &[fog_color, color, t]);
                // Alpha is not fogged.
                let v = self
                    .m
                    .emit(op::VECTOR_SHUFFLE, f4, &[mixed, color, 0, 1, 2, 7]);
                self.m.emit_void(op::STORE, &[oc, v]);
            }
            let func = self.options.alpha_test;
            if func != 0 && func != 8 {
                let color = self.m.emit(op::LOAD, f4, &[oc]);
                let a = self.extract(color, 3);
                let params = self.uniform_at(constants::ALPHA_FOG);
                let r = self.extract(params, 0);
                // Compared as 8-bit values, as Direct3D 9 does.
                let scale = self.fconst(255.0);
                let a8 = self.m.emit(op::F_MUL, f, &[a, scale]);
                let a8 = self.m.glsl(f, GLSL_ROUND, &[a8]);
                let r8 = self.m.emit(op::F_MUL, f, &[r, scale]);
                let r8 = self.m.glsl(f, GLSL_ROUND, &[r8]);
                let pass = match func {
                    1 => self.m.constant_bool(false),
                    c => self.compare(
                        match c {
                            2 => 4,
                            3 => 2,
                            4 => 6,
                            5 => 1,
                            6 => 5,
                            _ => 3,
                        },
                        self.b,
                        a8,
                        r8,
                    )?,
                };
                let fail = self.m.emit(OP_LOGICAL_NOT, self.b, &[pass]);
                self.kill_if(fail);
            }
        }
        if self.stage() == Stage::Vertex {
            self.vertex_extras()?;
        }
        let sinks = self.sinks.clone();
        for (local, var, saturate) in sinks {
            let mut v = self.m.emit(op::LOAD, f4, &[local]);
            if saturate {
                let zero = self.f4const([0.0; 4]);
                let one = self.f4const([1.0; 4]);
                v = self.m.glsl(f4, GLSL_F_CLAMP, &[v, zero, one]);
            }
            self.m.emit_void(op::STORE, &[var, v]);
        }
        for (local, var) in self.scalar_sinks.clone() {
            let v = self.m.emit(op::LOAD, f4, &[local]);
            let x = self.extract(v, 0);
            self.m.emit_void(op::STORE, &[var, x]);
        }
        let _ = OP_LOGICAL_AND;
        let _ = OP_ALL;
        let _ = R_LABEL;
        let _ = R_SAMPLER;
        Ok(())
    }
}

// ---- programs of the device -----------------------------------------------

fn token_dst(kind: u32, num: u32, mask: u32) -> u32 {
    0x8000_0000 | ((kind & 7) << 28) | ((kind & 0x18) << 8) | (mask << 16) | num
}

fn token_src(kind: u32, num: u32, swizzle: u32) -> u32 {
    0x8000_0000 | ((kind & 7) << 28) | ((kind & 0x18) << 8) | (swizzle << 16) | num
}

const XYZW: u32 = 0xe4;

fn ins(opcode: u16, params: &[u32]) -> Vec<u32> {
    let mut v = vec![u32::from(opcode) | (u32::try_from(params.len()).unwrap_or(0) << 24)];
    v.extend_from_slice(params);
    v
}

/// The vertex shader of pretransformed vertices (`POSITIONT`, no vertex
/// shader bound): screen coordinates back to clip space through
/// [`constants::VIEWPORT`] and [`constants::DEPTH`], the other inputs
/// passed on. `inputs`: the draw's (`D3DDECLUSAGE`, index) pairs; fog
/// comes from the specular color's alpha, as Direct3D 9 does.
pub fn pretransformed_vs(inputs: &[(u32, u32)]) -> Vec<u32> {
    let mut w = vec![0xfffe_0300];
    let dcl = |usage: u32, index: u32, kind: u32, num: u32| {
        ins(
            o::DCL,
            &[
                0x8000_0000 | usage | (index << 16),
                token_dst(kind, num, 0xf),
            ],
        )
    };
    let (vp, depth) = (constants::VIEWPORT, constants::DEPTH);
    w.extend(dcl(9, 0, R_INPUT, 0));
    w.extend(dcl(0, 0, R_OUTPUT, 0));
    let mut next_in = 1;
    let mut next_out = 1;
    // (output, input, write mask, swizzle)
    let mut moves = Vec::new();
    for &(usage, index) in inputs {
        let point_size = (usage, index) == (4, 0);
        if usage == 9 || (location(usage, index).is_none() && !point_size) {
            continue;
        }
        w.extend(dcl(usage, index, R_INPUT, next_in));
        w.extend(dcl(usage, index, R_OUTPUT, next_out));
        if point_size {
            // A scalar: its x.
            moves.push((next_out, next_in, 0x1, 0x00));
        } else {
            moves.push((next_out, next_in, 0xf, XYZW));
        }
        if (usage, index) == (10, 1) {
            // Specular alpha: the fog factor.
            next_out = next_out.saturating_add(1);
            w.extend(dcl(11, 0, R_OUTPUT, next_out));
            moves.push((next_out, next_in, 0x1, 0xff));
        }
        next_in = next_in.saturating_add(1);
        next_out = next_out.saturating_add(1);
    }
    // r0.xy = v0.xy * vp.xy + vp.zw; r0.z = v0.z * depth.x + depth.y;
    // r0.w = 1; o0 = r0 / v0.w.
    w.extend(ins(
        o::MAD,
        &[
            token_dst(R_TEMP, 0, 0x3),
            token_src(R_INPUT, 0, XYZW),
            token_src(R_CONST, vp, XYZW),
            token_src(R_CONST, vp, 0xee), // .zwzw
        ],
    ));
    w.extend(ins(
        o::MAD,
        &[
            token_dst(R_TEMP, 0, 0x4),
            token_src(R_INPUT, 0, 0xaa), // .zzzz
            token_src(R_CONST, depth, 0x00),
            token_src(R_CONST, depth, 0x55),
        ],
    ));
    w.extend(ins(
        o::RCP,
        &[token_dst(R_TEMP, 0, 0x8), token_src(R_INPUT, 0, 0xff)],
    ));
    w.extend(ins(
        o::MUL,
        &[
            token_dst(R_OUTPUT, 0, 0x7),
            token_src(R_TEMP, 0, XYZW),
            token_src(R_TEMP, 0, 0xff),
        ],
    ));
    w.extend(ins(
        o::MOV,
        &[token_dst(R_OUTPUT, 0, 0x8), token_src(R_TEMP, 0, 0xff)],
    ));
    for (out, input, mask, swizzle) in moves {
        w.extend(ins(
            o::MOV,
            &[
                token_dst(R_OUTPUT, out, mask),
                token_src(R_INPUT, input, swizzle),
            ],
        ));
    }
    w.push(0x0000_ffff);
    w
}

/// The pixel shader when none is bound: the diffuse color.
pub fn default_ps() -> Vec<u32> {
    let mut w = vec![0xffff_0200];
    w.extend(ins(o::DCL, &[0x8000_0000, token_dst(R_INPUT, 0, 0xf)]));
    w.extend(ins(
        o::MOV,
        &[token_dst(R_COLOROUT, 0, 0xf), token_src(R_INPUT, 0, XYZW)],
    ));
    w.push(0x0000_ffff);
    w
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn validate(name: &str, spirv: &[u32]) {
        if let Some(result) = crate::tests::validate_spirv(name, spirv) {
            result.unwrap_or_else(|e| panic!("{name}: spirv-val: {e}"));
        }
    }

    fn check(name: &str, words: &[u32], options: &Options) -> Translated {
        let t = translate_words(words, options).unwrap_or_else(|e| panic!("{name}: {e}"));
        validate(name, &t.shader.spirv);
        t
    }

    #[test]
    fn the_device_programs_translate() {
        let vs = check(
            "pretransformed",
            &pretransformed_vs(&[(9, 0), (10, 0), (10, 1), (5, 0)]),
            &Options::default(),
        );
        assert_eq!(vs.inputs.len(), 4);
        assert!(vs.inputs.contains(&(0, 9, 0)));
        // Color 0, color 1, fog, texcoord 0.
        let mut outs = vs.shader.outputs.clone();
        outs.sort_unstable();
        assert_eq!(outs, vec![0, 1, 2, 18]);
        let options = Options {
            alpha_test: 7,
            fog: Fog::Linear,
            flat: true,
            ..Options::default()
        };
        check("default ps", &default_ps(), &options);
        check(
            "default ps, vertex fog",
            &default_ps(),
            &Options {
                fog: Fog::Vertex,
                ..Options::default()
            },
        );
    }

    #[test]
    fn shader_model_2_programs_translate() {
        // vs_2_0: dcl_position v0; dcl_color v1; m4x4 oPos, v0, c0;
        // mul oD0, v1, c4.
        let mut vs = vec![0xfffe_0200];
        vs.extend(ins(o::DCL, &[0x8000_0000, token_dst(R_INPUT, 0, 0xf)]));
        vs.extend(ins(o::DCL, &[0x8000_000a, token_dst(R_INPUT, 1, 0xf)]));
        vs.extend(ins(
            o::M4X4,
            &[
                token_dst(R_RASTOUT, 0, 0xf),
                token_src(R_INPUT, 0, XYZW),
                token_src(R_CONST, 0, XYZW),
            ],
        ));
        vs.extend(ins(
            o::MUL,
            &[
                token_dst(R_ATTROUT, 0, 0xf),
                token_src(R_INPUT, 1, XYZW),
                token_src(R_CONST, 4, XYZW),
            ],
        ));
        vs.push(0xffff);
        let t = check("vs_2_0", &vs, &Options::default());
        assert_eq!(t.inputs, vec![(0, 0, 0), (1, 10, 0)]);
        // ps_2_0: def c1; dcl t0; dcl_2d s0; texld r0, t0, s0;
        // mad r0, r0, c0, c1; texkill t0; mov oC0, r0.
        let mut ps = vec![0xffff_0200];
        ps.extend(ins(
            o::DEF,
            &[
                token_dst(R_CONST, 1, 0xf),
                0.5f32.to_bits(),
                0,
                0,
                1.0f32.to_bits(),
            ],
        ));
        ps.extend(ins(o::DCL, &[0x8000_0000, token_dst(R_ADDR, 0, 0xf)]));
        ps.extend(ins(o::DCL, &[0x9000_0000, token_dst(R_SAMPLER, 0, 0xf)]));
        ps.extend(ins(
            o::TEX,
            &[
                token_dst(R_TEMP, 0, 0xf),
                token_src(R_ADDR, 0, XYZW),
                token_src(R_SAMPLER, 0, XYZW),
            ],
        ));
        ps.extend(ins(
            o::MAD,
            &[
                token_dst(R_TEMP, 0, 0xf),
                token_src(R_TEMP, 0, XYZW),
                token_src(R_CONST, 0, XYZW),
                token_src(R_CONST, 1, XYZW),
            ],
        ));
        ps.extend(ins(o::TEXKILL, &[token_dst(R_ADDR, 0, 0xf)]));
        ps.extend(ins(
            o::MOV,
            &[token_dst(R_COLOROUT, 0, 0xf), token_src(R_TEMP, 0, XYZW)],
        ));
        ps.push(0xffff);
        let t = check("ps_2_0", &ps, &Options::default());
        assert_eq!(t.samplers, vec![(0, 2)]);
        assert_eq!(
            t.defs,
            vec![(1, [0.5f32.to_bits(), 0, 0, 1.0f32.to_bits()])]
        );
        assert!(t.shader.inputs.contains(&2));
    }

    #[test]
    fn shader_model_3_flow_control_translates() {
        // ps_3_0: dcl_texcoord v0.xy; defi i0; loop aL, i0;
        // add r0, r0, c[aL]; if_gt r0.x, c0.x; break; endif; endloop;
        // setp_lt p0, r0, c1; (p0) mov r0, c2; mov oC0, r0.
        let mut ps = vec![0xffff_0300];
        ps.extend(ins(o::DCL, &[0x8000_0005, token_dst(R_INPUT, 0, 0x3)]));
        ps.extend(ins(o::DEFI, &[token_dst(R_CONSTINT, 0, 0xf), 4, 0, 1, 0]));
        ps.extend(ins(
            o::LOOP,
            &[token_src(R_LOOP, 0, 0), token_src(R_CONSTINT, 0, XYZW)],
        ));
        ps.extend(ins(
            o::ADD,
            &[
                token_dst(R_TEMP, 0, 0xf),
                token_src(R_TEMP, 0, XYZW),
                token_src(R_CONST, 0, XYZW) | (1 << 13),
                token_src(R_LOOP, 0, 0),
            ],
        ));
        ps.extend(ins(
            o::IFC,
            &[token_src(R_TEMP, 0, 0), token_src(R_CONST, 0, 0)],
        ));
        // ifc's comparison: gt.
        let at = ps.len() - 3;
        ps[at] |= 1 << 16;
        ps.extend(ins(o::BREAK, &[]));
        ps.extend(ins(o::ENDIF, &[]));
        ps.extend(ins(o::ENDLOOP, &[]));
        ps.extend(ins(
            o::SETP,
            &[
                token_dst(R_PREDICATE, 0, 0xf),
                token_src(R_TEMP, 0, XYZW),
                token_src(R_CONST, 1, XYZW),
            ],
        ));
        let at = ps.len() - 4;
        ps[at] |= 4 << 16;
        let mut pred = ins(
            o::MOV,
            &[
                token_dst(R_TEMP, 0, 0xf),
                token_src(R_PREDICATE, 0, XYZW),
                token_src(R_CONST, 2, XYZW),
            ],
        );
        pred[0] |= 1 << 28;
        ps.extend(pred);
        ps.extend(ins(
            o::MOV,
            &[token_dst(R_COLOROUT, 0, 0xf), token_src(R_TEMP, 0, XYZW)],
        ));
        ps.push(0xffff);
        check("ps_3_0 flow", &ps, &Options::default());
    }

    #[test]
    fn shader_model_1_programs_translate() {
        // vs_1_1: dcl_position v0; mov a0.x, v0.x; mov oPos, c[a0.x + 2];
        // mov oT0, v0.
        let mut vs = vec![0xfffe_0101];
        vs.extend([0x1f, 0x8000_0000, token_dst(R_INPUT, 0, 0xf)]);
        vs.extend([0x01, token_dst(R_ADDR, 0, 0x1), token_src(R_INPUT, 0, 0)]);
        vs.extend([
            0x01,
            token_dst(R_RASTOUT, 0, 0xf),
            token_src(R_CONST, 2, XYZW) | (1 << 13),
        ]);
        vs.extend([
            0x01,
            token_dst(R_OUTPUT, 0, 0xf),
            token_src(R_INPUT, 0, XYZW),
        ]);
        vs.push(0xffff);
        check("vs_1_1", &vs, &Options::default());
        // ps_1_1: tex t0; mul r0, t0, v0.
        let mut ps = vec![0xffff_0101];
        ps.extend([0x42, token_dst(R_ADDR, 0, 0xf)]);
        ps.extend([
            0x05,
            token_dst(R_TEMP, 0, 0xf),
            token_src(R_ADDR, 0, XYZW),
            token_src(R_INPUT, 0, XYZW),
        ]);
        ps.push(0xffff);
        let t = check("ps_1_1", &ps, &Options::default());
        assert_eq!(t.samplers, vec![(0, 2)]);
    }

    /// Shader model 1 instructions: no length, every parameter has bit 31.
    fn sm1(version: u32, body: &[&[u32]]) -> Vec<u32> {
        let mut w = vec![version];
        for i in body {
            w.extend_from_slice(i);
        }
        w.push(0xffff);
        w
    }

    #[test]
    fn shader_model_1_texture_addressing_translates() {
        let t = |n| token_dst(R_ADDR, n, 0xf);
        let ts = |n| token_src(R_ADDR, n, XYZW);
        let r0 = token_dst(R_TEMP, 0, 0xf);
        let tex0: &[u32] = &[u32::from(o::TEX), t(0)];
        let programs: Vec<(&str, Vec<u32>)> = vec![
            (
                "texbeml",
                sm1(
                    0xffff_0101,
                    &[tex0, &[u32::from(o::TEXBEML), t(1), ts(0)], &[1, r0, ts(1)]],
                ),
            ),
            (
                "texreg2ar",
                sm1(
                    0xffff_0101,
                    &[
                        tex0,
                        &[u32::from(o::TEXREG2AR), t(1), ts(0)],
                        &[1, r0, ts(1)],
                    ],
                ),
            ),
            (
                "texm3x3spec",
                sm1(
                    0xffff_0101,
                    &[
                        tex0,
                        &[u32::from(o::TEXM3X3PAD), t(1), ts(0)],
                        &[u32::from(o::TEXM3X3PAD), t(2), ts(0)],
                        &[
                            u32::from(o::TEXM3X3SPEC),
                            t(3),
                            ts(0),
                            token_src(R_CONST, 0, XYZW),
                        ],
                        &[1, r0, ts(3)],
                    ],
                ),
            ),
            (
                "texm3x3vspec",
                sm1(
                    0xffff_0101,
                    &[
                        tex0,
                        &[u32::from(o::TEXM3X3PAD), t(1), ts(0)],
                        &[u32::from(o::TEXM3X3PAD), t(2), ts(0)],
                        &[u32::from(o::TEXM3X3VSPEC), t(3), ts(0)],
                        &[1, r0, ts(3)],
                    ],
                ),
            ),
            (
                "texdp3tex",
                sm1(
                    0xffff_0102,
                    &[
                        tex0,
                        &[u32::from(o::TEXDP3TEX), t(1), ts(0)],
                        &[1, r0, ts(1)],
                    ],
                ),
            ),
            (
                "texm3x2depth",
                sm1(
                    0xffff_0103,
                    &[
                        tex0,
                        &[u32::from(o::TEXM3X2PAD), t(1), ts(0)],
                        &[u32::from(o::TEXM3X2DEPTH), t(2), ts(0)],
                        &[1, r0, ts(0)],
                    ],
                ),
            ),
            (
                "texdepth and bem",
                sm1(
                    0xffff_0104,
                    &[
                        &[u32::from(o::TEXCOORD), token_dst(R_TEMP, 5, 0x7), ts(0)],
                        &[u32::from(o::TEXDEPTH), token_dst(R_TEMP, 5, 0xf)],
                        &[u32::from(o::TEX), token_dst(R_TEMP, 1, 0xf), ts(0)],
                        &[
                            u32::from(o::BEM),
                            token_dst(R_TEMP, 0, 0x3),
                            token_src(R_TEMP, 1, XYZW),
                            token_src(R_TEMP, 1, XYZW),
                        ],
                    ],
                ),
            ),
        ];
        for (name, words) in programs {
            check(name, &words, &Options::default());
        }
        // A pad without its end, and an end without its pads, are refused.
        let lone = sm1(
            0xffff_0101,
            &[
                tex0,
                &[u32::from(o::TEXM3X2TEX), t(1), ts(0)],
                &[1, r0, ts(1)],
            ],
        );
        assert!(translate_words(&lone, &Options::default()).is_err());
    }

    #[test]
    fn subroutines_are_inlined() {
        // vs_2_0: defb b0; callnz l0, b0; call l1; ret;
        // label l0; mov oPos, c0; ret; label l1; call l0; ret.
        let label = |n| token_src(R_LABEL, n, XYZW);
        let mut vs = vec![0xfffe_0200];
        vs.extend(ins(o::DEFB, &[token_dst(R_CONSTBOOL, 0, 0xf), 1]));
        vs.extend(ins(o::CALLNZ, &[label(0), token_src(R_CONSTBOOL, 0, 0)]));
        vs.extend(ins(o::CALL, &[label(1)]));
        vs.extend(ins(o::RET, &[]));
        vs.extend(ins(o::LABEL, &[label(0)]));
        vs.extend(ins(
            o::MOV,
            &[token_dst(R_RASTOUT, 0, 0xf), token_src(R_CONST, 0, XYZW)],
        ));
        vs.extend(ins(o::RET, &[]));
        vs.extend(ins(o::LABEL, &[label(1)]));
        vs.extend(ins(o::CALL, &[label(0)]));
        vs.extend(ins(o::RET, &[]));
        vs.push(0xffff);
        check("subroutines", &vs, &Options::default());
        // A subroutine calling itself is refused, not translated forever.
        let mut recursive = vec![0xfffe_0200];
        recursive.extend(ins(o::CALL, &[label(0)]));
        recursive.extend(ins(o::RET, &[]));
        recursive.extend(ins(o::LABEL, &[label(0)]));
        recursive.extend(ins(o::CALL, &[label(0)]));
        recursive.extend(ins(o::RET, &[]));
        recursive.push(0xffff);
        assert!(translate_words(&recursive, &Options::default()).is_err());
    }

    #[test]
    fn shadow_samplers_compare() {
        // ps_2_0: dcl t0; dcl_2d s0; texldp r0, t0, s0; mov oC0, r0.
        let mut ps = vec![0xffff_0200];
        ps.extend(ins(o::DCL, &[0x8000_0000, token_dst(R_ADDR, 0, 0xf)]));
        ps.extend(ins(o::DCL, &[0x9000_0000, token_dst(R_SAMPLER, 0, 0xf)]));
        let mut texldp = ins(
            o::TEX,
            &[
                token_dst(R_TEMP, 0, 0xf),
                token_src(R_ADDR, 0, XYZW),
                token_src(R_SAMPLER, 0, XYZW),
            ],
        );
        texldp[0] |= 1 << 16;
        ps.extend(texldp);
        ps.extend(ins(
            o::MOV,
            &[token_dst(R_COLOROUT, 0, 0xf), token_src(R_TEMP, 0, XYZW)],
        ));
        ps.push(0xffff);
        let options = Options {
            shadow: 1,
            ..Options::default()
        };
        let t = check("shadow", &ps, &options);
        // OpImageSampleDrefImplicitLod.
        assert!(t.shader.spirv.iter().any(|w| *w & 0xffff == 89));
    }

    #[test]
    fn points_sprites_and_clip_planes_translate() {
        let points = Options {
            points: true,
            clip_planes: 0b10_0101,
            ..Options::default()
        };
        // The device's vertex shader, with and without a point size input.
        check("points", &pretransformed_vs(&[(9, 0), (10, 0)]), &points);
        let sized = check(
            "point sizes",
            &pretransformed_vs(&[(9, 0), (4, 0), (5, 0)]),
            &points,
        );
        assert!(sized.writes_point_size);
        let sprite = Options {
            point_sprite: true,
            ..Options::default()
        };
        // ps_1_1: tex t0 (coordinates from the point).
        let ps = sm1(
            0xffff_0101,
            &[
                &[u32::from(o::TEX), token_dst(R_ADDR, 0, 0xf)],
                &[1, token_dst(R_TEMP, 0, 0xf), token_src(R_ADDR, 0, XYZW)],
            ],
        );
        let t = check("sprite", &ps, &sprite);
        assert!(!t.shader.inputs.contains(&2));
    }

    /// Shaders captured from the guest with
    /// `RSVGA_SHADER_DUMP=target/shaders/d3d9` (VMware's fixed-function
    /// programs and applications' ones: not ours, they stay out of the
    /// repository), translated with and without the device's additions.
    #[test]
    fn the_captured_corpus_translates_and_validates() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shaders/d3d9");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("no captured shaders in target/shaders/d3d9: skipped");
            return;
        };
        let extra = Options {
            alpha_test: 7,
            fog: Fog::Linear,
            flat: true,
            ..Options::default()
        };
        let mut failures = Vec::new();
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "d3d9") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let words: Vec<u32> = bytes
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            for options in [Options::default(), extra.clone()] {
                match translate_words(&words, &options) {
                    Err(e) => failures.push(format!("{name}: {e}")),
                    Ok(t) => {
                        if let Some(Err(e)) = crate::tests::validate_spirv(&name, &t.shader.spirv) {
                            failures.push(format!("{name}: {}", e.lines().next().unwrap_or("")));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Corrupted programs are refused or translated, never a panic.
    #[test]
    fn mangled_programs_do_not_panic() {
        let base = pretransformed_vs(&[(9, 0), (10, 0), (5, 0)]);
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut words = base.clone();
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let at = 1 + (seed as usize) % (words.len() - 1);
                words[at] ^= 1 << (seed >> 27);
            }
            let _ = translate_words(&words, &Options::default());
        }
    }

    #[test]
    fn malformed_programs_are_refused() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0x1234_0000, 0xffff]).is_err());
        assert!(decode(&[0xffff_0200]).is_err());
        assert!(decode(&[0xffff_0200, u32::from(o::MOV) | (5 << 24), 0x8000_0000]).is_err());
        assert!(translate_words(&[0xfffe_0400, 0xffff], &Options::default()).is_err());
    }
}
