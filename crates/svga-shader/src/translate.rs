//! VGPU10 programs to SPIR-V for Vulkan.
//!
//! Shader model 4 registers are untyped 32-bit lanes; here every register
//! (`r#`, `v#`, `o#`, `x#[]`, constant buffer elements) is a `vec4` of
//! floats, and integer instructions reinterpret the bits (`OpBitcast`).
//! Inputs are copied into function variables on entry and outputs out of
//! them before each return, so partial writes and reads stay simple.
//! Flow control (`if`, `loop`, `break`, `ret`...) becomes SPIR-V structured
//! constructs.
//!
//! Bindings, all in descriptor set 0: [`binding`] gives each stage 256
//! numbers: constant buffers from 0, samplers from 16, shader resources
//! from 32.
//!
//! Supported now: vertex and pixel shaders of shader model 4.0 and 4.1,
//! their arithmetic, comparisons, conversions, flow control (without
//! `switch` and subroutines), `sample`, `sample_l`, `sample_b`, `sample_d`,
//! `ld` and `resinfo`. Anything else is refused with a reason.

use std::collections::HashMap;

use crate::decode::{Index, Instruction, Modifier, Operand, Program, Select, Stage};
use crate::opcode as o;
use crate::spirv::{Module, Scalar, glsl, op};

/// Why a program cannot be translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateError(pub String);

impl core::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

fn unsupported<T>(what: impl core::fmt::Display) -> Result<T, TranslateError> {
    Err(TranslateError(format!("unsupported: {what}")))
}

/// What the pipeline provides that the shader cannot tell.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Options {
    /// Type of each vertex shader input register (from the vertex formats).
    pub inputs: [Scalar; 32],
    /// Type of each pixel shader render target (from the view formats).
    pub outputs: [Scalar; 8],
}

impl Default for Options {
    fn default() -> Self {
        Self {
            inputs: [Scalar::Float; 32],
            outputs: [Scalar::Float; 8],
        }
    }
}

/// What a descriptor binding holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// Constant buffer `slot` of `vec4s` elements (uniform buffer).
    ConstantBuffer {
        /// Slot.
        slot: u32,
        /// Elements.
        vec4s: u32,
    },
    /// Sampler `slot`.
    Sampler {
        /// Slot.
        slot: u32,
    },
    /// Shader resource `slot` (sampled image or uniform texel buffer).
    Resource {
        /// Slot.
        slot: u32,
        /// `VGPU10_RESOURCE_DIMENSION_*`.
        dimension: u32,
    },
}

/// A descriptor the shader uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// What it holds.
    pub kind: BindingKind,
    /// Binding number in set 0.
    pub binding: u32,
}

/// A translated shader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shader {
    /// The SPIR-V module.
    pub spirv: Vec<u32>,
    /// Stage.
    pub stage: Stage,
    /// Descriptors used.
    pub bindings: Vec<Binding>,
    /// Input locations (vertex shader: vertex attributes).
    pub inputs: Vec<u32>,
    /// Output locations (pixel shader: render targets).
    pub outputs: Vec<u32>,
}

/// Binding number of a descriptor: 256 per stage.
pub fn binding(stage: Stage, kind: &BindingKind) -> u32 {
    let base: u32 = match stage {
        Stage::Vertex => 0,
        Stage::Pixel => 256,
        Stage::Geometry => 512,
        Stage::Hull => 768,
        Stage::Domain => 1024,
        Stage::Compute => 1280,
    };
    let offset = match *kind {
        BindingKind::ConstantBuffer { slot, .. } => slot,
        BindingKind::Sampler { slot } => 16u32.saturating_add(slot),
        BindingKind::Resource { slot, .. } => 32u32.saturating_add(slot),
    };
    base.saturating_add(offset.min(255))
}

// SPIR-V enumerants.
const STORAGE_INPUT: u32 = 1;
const STORAGE_UNIFORM: u32 = 2;
const STORAGE_OUTPUT: u32 = 3;
const STORAGE_UNIFORM_CONSTANT: u32 = 0;
const STORAGE_FUNCTION: u32 = 7;
const DECORATION_BLOCK: u32 = 2;
const DECORATION_ARRAY_STRIDE: u32 = 6;
const DECORATION_BUILTIN: u32 = 11;
const DECORATION_NO_PERSPECTIVE: u32 = 13;
const DECORATION_FLAT: u32 = 14;
const DECORATION_CENTROID: u32 = 16;
const DECORATION_SAMPLE: u32 = 17;
const DECORATION_LOCATION: u32 = 30;
const DECORATION_BINDING: u32 = 33;
const DECORATION_DESCRIPTOR_SET: u32 = 34;
const DECORATION_OFFSET: u32 = 35;
const BUILTIN_POSITION: u32 = 0;
const BUILTIN_PRIMITIVE_ID: u32 = 7;
const BUILTIN_FRAG_COORD: u32 = 15;
const BUILTIN_FRONT_FACING: u32 = 17;
const BUILTIN_SAMPLE_ID: u32 = 18;
const BUILTIN_FRAG_DEPTH: u32 = 22;
const BUILTIN_VERTEX_INDEX: u32 = 42;
const BUILTIN_INSTANCE_INDEX: u32 = 43;
const BUILTIN_BASE_VERTEX: u32 = 4424;
const BUILTIN_BASE_INSTANCE: u32 = 4425;
const CAP_SHADER: u32 = 1;
const CAP_GEOMETRY: u32 = 2;
const CAP_SAMPLED_1D: u32 = 43;
const CAP_SAMPLED_BUFFER: u32 = 46;
const CAP_IMAGE_QUERY: u32 = 50;
const CAP_DERIVATIVE_CONTROL: u32 = 51;
const CAP_SAMPLE_RATE_SHADING: u32 = 35;
const CAP_DRAW_PARAMETERS: u32 = 4427;
const MODEL_VERTEX: u32 = 0;
const MODEL_FRAGMENT: u32 = 4;
const MODE_ORIGIN_UPPER_LEFT: u32 = 7;
const MODE_DEPTH_REPLACING: u32 = 12;
const MODE_EARLY_FRAGMENT_TESTS: u32 = 9;
const IMAGE_BIAS: u32 = 0x1;
const IMAGE_LOD: u32 = 0x2;
const IMAGE_GRAD: u32 = 0x4;
const IMAGE_CONST_OFFSET: u32 = 0x8;

// Operand types (`VGPU10_OPERAND_TYPE_*`).
const T_TEMP: u32 = 0;
const T_INPUT: u32 = 1;
const T_OUTPUT: u32 = 2;
const T_INDEXABLE_TEMP: u32 = 3;
const T_IMMEDIATE32: u32 = 4;
const T_CONSTANT_BUFFER: u32 = 8;
const T_IMMEDIATE_CONSTANT_BUFFER: u32 = 9;
const T_INPUT_PRIMITIVEID: u32 = 11;
const T_OUTPUT_DEPTH: u32 = 12;
const T_NULL: u32 = 13;

/// How an input register is filled at entry.
enum InputSource {
    /// A `vec4` interface variable of this scalar kind.
    Plain(u32, Scalar),
    /// A float `vec4` builtin (fragment coordinates: w becomes 1/w).
    FragCoord(u32),
    /// An integer builtin, minus another (vertex/instance index minus base).
    IndexMinusBase(u32, u32),
    /// A signed builtin.
    IntBuiltin(u32),
    /// The front-facing boolean, as ~0 or 0.
    FrontFacing(u32),
}

struct Resource {
    var: u32,
    image_ty: u32,
    dimension: u32,
    ret: Scalar,
}

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
    },
}

struct Tx<'a> {
    m: Module,
    stage: Stage,
    options: &'a Options,
    f4: u32,
    u4: u32,
    i4: u32,
    b4: u32,
    temps: Vec<u32>,
    indexable: HashMap<u32, (u32, u32)>,
    inputs: HashMap<u32, u32>,
    input_sources: Vec<(u32, InputSource)>,
    outputs: HashMap<u32, u32>,
    output_sinks: Vec<(u32, u32, Scalar)>,
    depth: Option<(u32, u32)>,
    interface: Vec<u32>,
    cbs: HashMap<u32, u32>,
    samplers: HashMap<u32, u32>,
    resources: HashMap<u32, Resource>,
    icb: Option<(u32, u32)>,
    flow: Vec<Flow>,
    bindings: Vec<Binding>,
    input_locations: Vec<u32>,
    output_locations: Vec<u32>,
    early_tests: bool,
}

/// Translates `program` to SPIR-V.
pub fn translate(program: &Program, options: &Options) -> Result<Shader, TranslateError> {
    if !matches!(program.stage, Stage::Vertex | Stage::Pixel) {
        return unsupported(format_args!("{:?} shaders", program.stage));
    }
    let mut m = Module::new();
    m.capability(CAP_SHADER);
    let f4 = m.vector(Scalar::Float, 4);
    let u4 = m.vector(Scalar::Uint, 4);
    let i4 = m.vector(Scalar::Int, 4);
    let b4 = m.vector(Scalar::Bool, 4);
    let mut tx = Tx {
        m,
        stage: program.stage,
        options,
        f4,
        u4,
        i4,
        b4,
        temps: Vec::new(),
        indexable: HashMap::new(),
        inputs: HashMap::new(),
        input_sources: Vec::new(),
        outputs: HashMap::new(),
        output_sinks: Vec::new(),
        depth: None,
        interface: Vec::new(),
        cbs: HashMap::new(),
        samplers: HashMap::new(),
        resources: HashMap::new(),
        icb: None,
        flow: Vec::new(),
        bindings: Vec::new(),
        input_locations: Vec::new(),
        output_locations: Vec::new(),
        early_tests: false,
    };
    let function = tx.m.id();
    let entry = tx.m.id();
    tx.m.emit_void(op::LABEL, &[entry]);
    for i in &program.instructions {
        tx.declare(i)?;
    }
    tx.load_inputs();
    for i in &program.instructions {
        tx.instruction(i)?;
    }
    if !tx.flow.is_empty() {
        return Err(TranslateError("unterminated if or loop".into()));
    }
    tx.ret();
    let (model, mut modes): (u32, Vec<Vec<u32>>) = match tx.stage {
        Stage::Vertex => (MODEL_VERTEX, Vec::new()),
        _ => (MODEL_FRAGMENT, vec![vec![MODE_ORIGIN_UPPER_LEFT]]),
    };
    if tx.depth.is_some() {
        modes.push(vec![MODE_DEPTH_REPLACING]);
    }
    if tx.early_tests && tx.stage == Stage::Pixel {
        modes.push(vec![MODE_EARLY_FRAGMENT_TESTS]);
    }
    let mode_refs: Vec<&[u32]> = modes.iter().map(Vec::as_slice).collect();
    let interface = tx.interface.clone();
    tx.m.entry_point(model, function, &interface, &mode_refs);
    Ok(Shader {
        spirv: tx.m.finish(function),
        stage: program.stage,
        bindings: tx.bindings,
        inputs: tx.input_locations,
        outputs: tx.output_locations,
    })
}

fn swizzle_of(select: Select) -> [u32; 4] {
    match select {
        Select::Swizzle(s) => s.map(u32::from),
        Select::Scalar(c) => [u32::from(c); 4],
        Select::Mask(_) | Select::None => [0, 1, 2, 3],
    }
}

fn mask_of(operand: &Operand) -> u8 {
    match operand.select {
        Select::Mask(m) => m,
        Select::Scalar(c) => 1 << c,
        _ => 0xf,
    }
}

/// Coordinates a sample instruction reads, and whether the image is
/// arrayed, for `VGPU10_RESOURCE_DIMENSION_*`.
fn dimension_info(dimension: u32) -> Option<(u32, u32, bool, bool)> {
    // (SPIR-V Dim, coordinate count, arrayed, multisampled)
    Some(match dimension {
        1 => (5, 1, false, false), // buffer
        2 => (0, 1, false, false), // 1D
        3 => (1, 2, false, false), // 2D
        4 => (1, 2, false, true),  // 2D MS
        5 => (2, 3, false, false), // 3D
        6 => (3, 3, false, false), // cube
        7 => (0, 2, true, false),  // 1D array
        8 => (1, 3, true, false),  // 2D array
        9 => (1, 3, true, true),   // 2D MS array
        10 => (3, 4, true, false), // cube array
        _ => return None,
    })
}

impl Tx<'_> {
    fn ptr(&mut self, class: u32, ty: u32) -> u32 {
        self.m.pointer(class, ty)
    }

    fn vec(&mut self, s: Scalar) -> u32 {
        match s {
            Scalar::Float => self.f4,
            Scalar::Uint => self.u4,
            Scalar::Int => self.i4,
            Scalar::Bool => self.b4,
        }
    }

    fn input_var(&mut self, ty: u32, decorations: &[&[u32]]) -> u32 {
        let p = self.ptr(STORAGE_INPUT, ty);
        let var = self.m.global(p, STORAGE_INPUT);
        for d in decorations {
            self.m.decorate(var, d);
        }
        self.interface.push(var);
        var
    }

    fn output_var(&mut self, ty: u32, decorations: &[&[u32]]) -> u32 {
        let p = self.ptr(STORAGE_OUTPUT, ty);
        let var = self.m.global(p, STORAGE_OUTPUT);
        for d in decorations {
            self.m.decorate(var, d);
        }
        self.interface.push(var);
        var
    }

    fn register_var(&mut self) -> u32 {
        let p = self.ptr(STORAGE_FUNCTION, self.f4);
        self.m.local(p)
    }

    // ---- declarations ---------------------------------------------------

    fn declare(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let reg = |i: &Instruction| -> Result<u32, TranslateError> {
            i.operands
                .first()
                .and_then(Operand::index0)
                .ok_or_else(|| TranslateError(format!("declaration {} without register", i.opcode)))
        };
        match i.opcode {
            o::DCL_TEMPS => {
                let count = i.raw.first().copied().unwrap_or(0);
                if count > 4096 {
                    return unsupported(format_args!("{count} temporaries"));
                }
                for _ in 0..count {
                    let v = self.register_var();
                    self.temps.push(v);
                }
            }
            o::DCL_INDEXABLE_TEMP => {
                let (reg, len) = (
                    i.raw.first().copied().unwrap_or(0),
                    i.raw.get(1).copied().unwrap_or(0),
                );
                if len == 0 || len > 4096 {
                    return unsupported(format_args!("indexable temporary of {len} registers"));
                }
                let array = self.m.array(self.f4, len);
                let p = self.ptr(STORAGE_FUNCTION, array);
                let var = self.m.local(p);
                self.indexable.insert(reg, (var, len));
            }
            o::DCL_GLOBAL_FLAGS => self.early_tests = i.token & (1 << 13) != 0,
            o::DCL_CONSTANT_BUFFER => {
                let operand = self.operand(i, 0)?;
                let (Some(Index::Imm(slot)), Some(Index::Imm(len))) =
                    (operand.indices.first(), operand.indices.get(1))
                else {
                    return unsupported("constant buffer declaration");
                };
                let (slot, len) = (*slot, (*len).clamp(1, 4096));
                let array = self.m.array_unique(self.f4, len);
                self.m.decorate(array, &[DECORATION_ARRAY_STRIDE, 16]);
                let block = self.m.structure(&[array]);
                self.m.decorate(block, &[DECORATION_BLOCK]);
                self.m.member_decorate(block, 0, &[DECORATION_OFFSET, 0]);
                let p = self.ptr(STORAGE_UNIFORM, block);
                let var = self.m.global(p, STORAGE_UNIFORM);
                let kind = BindingKind::ConstantBuffer { slot, vec4s: len };
                self.bind(var, kind);
                self.cbs.insert(slot, var);
            }
            o::DCL_SAMPLER => {
                let slot = reg(i)?;
                let ty = self.m.sampler();
                let p = self.ptr(STORAGE_UNIFORM_CONSTANT, ty);
                let var = self.m.global(p, STORAGE_UNIFORM_CONSTANT);
                self.bind(var, BindingKind::Sampler { slot });
                self.samplers.insert(slot, var);
            }
            o::DCL_RESOURCE => {
                let slot = reg(i)?;
                let dimension = i.resource_dimension();
                let Some((dim, _, arrayed, ms)) = dimension_info(dimension) else {
                    return unsupported(format_args!("resource dimension {dimension}"));
                };
                let ret = match i.raw.first().map(|t| t & 0xf) {
                    Some(3) => Scalar::Int,
                    Some(4) => Scalar::Uint,
                    _ => Scalar::Float,
                };
                match dim {
                    0 => self.m.capability(CAP_SAMPLED_1D),
                    5 => self.m.capability(CAP_SAMPLED_BUFFER),
                    _ => {}
                }
                let sampled = self.m.scalar(ret);
                let image_ty =
                    self.m
                        .image(&[sampled, dim, 0, u32::from(arrayed), u32::from(ms), 1, 0]);
                let p = self.ptr(STORAGE_UNIFORM_CONSTANT, image_ty);
                let var = self.m.global(p, STORAGE_UNIFORM_CONSTANT);
                self.bind(var, BindingKind::Resource { slot, dimension });
                self.resources.insert(
                    slot,
                    Resource {
                        var,
                        image_ty,
                        dimension,
                        ret,
                    },
                );
            }
            o::DCL_INPUT | o::DCL_INPUT_PS => {
                let operand = self.operand(i, 0)?;
                if operand.kind == T_INPUT_PRIMITIVEID {
                    return self.declare_builtin_input(u32::MAX, 7);
                }
                let r = reg(i)?;
                if self.inputs.contains_key(&r) {
                    return Ok(());
                }
                let ty = if self.stage == Stage::Vertex {
                    *self
                        .options
                        .inputs
                        .get(r as usize)
                        .unwrap_or(&Scalar::Float)
                } else {
                    Scalar::Float
                };
                let vty = self.vec(ty);
                let mut decorations: Vec<Vec<u32>> = vec![vec![DECORATION_LOCATION, r]];
                if i.opcode == o::DCL_INPUT_PS {
                    match i.interpolation() {
                        1 => decorations.push(vec![DECORATION_FLAT]),
                        3 => decorations.push(vec![DECORATION_CENTROID]),
                        4 => decorations.push(vec![DECORATION_NO_PERSPECTIVE]),
                        5 => {
                            decorations.push(vec![DECORATION_NO_PERSPECTIVE]);
                            decorations.push(vec![DECORATION_CENTROID]);
                        }
                        6 | 7 => {
                            self.m.capability(CAP_SAMPLE_RATE_SHADING);
                            decorations.push(vec![DECORATION_SAMPLE]);
                            if i.interpolation() == 7 {
                                decorations.push(vec![DECORATION_NO_PERSPECTIVE]);
                            }
                        }
                        _ => {}
                    }
                }
                let refs: Vec<&[u32]> = decorations.iter().map(Vec::as_slice).collect();
                let var = self.input_var(vty, &refs);
                let local = self.register_var();
                self.inputs.insert(r, local);
                self.input_sources
                    .push((local, InputSource::Plain(var, ty)));
                self.input_locations.push(r);
            }
            o::DCL_INPUT_SGV | o::DCL_INPUT_SIV | o::DCL_INPUT_PS_SGV | o::DCL_INPUT_PS_SIV => {
                let name = i.raw.first().copied().unwrap_or(0) & 0xffff;
                let r = reg(i)?;
                self.declare_builtin_input(r, name)?;
            }
            o::DCL_OUTPUT | o::DCL_OUTPUT_SGV | o::DCL_OUTPUT_SIV => {
                let operand = self.operand(i, 0)?;
                if operand.kind == T_OUTPUT_DEPTH {
                    let f = self.m.scalar(Scalar::Float);
                    let var = self.output_var(f, &[&[DECORATION_BUILTIN, BUILTIN_FRAG_DEPTH]]);
                    let local = self.register_var();
                    self.depth = Some((var, local));
                    return Ok(());
                }
                if operand.kind != T_OUTPUT {
                    return unsupported(format_args!("output operand type {}", operand.kind));
                }
                let r = reg(i)?;
                if self.outputs.contains_key(&r) {
                    return Ok(());
                }
                let name = if i.opcode == o::DCL_OUTPUT {
                    0
                } else {
                    i.raw.first().copied().unwrap_or(0) & 0xffff
                };
                let local = self.register_var();
                self.outputs.insert(r, local);
                let (var, ty) = if name == 1 && self.stage == Stage::Vertex {
                    let var = self.output_var(self.f4, &[&[DECORATION_BUILTIN, BUILTIN_POSITION]]);
                    (var, Scalar::Float)
                } else {
                    // Other system values travel as plain varyings for now.
                    let ty = if self.stage == Stage::Pixel {
                        *self
                            .options
                            .outputs
                            .get(r as usize)
                            .unwrap_or(&Scalar::Float)
                    } else {
                        Scalar::Float
                    };
                    let vty = self.vec(ty);
                    let var = self.output_var(vty, &[&[DECORATION_LOCATION, r]]);
                    self.output_locations.push(r);
                    (var, ty)
                };
                self.output_sinks.push((var, local, ty));
            }
            o::CUSTOMDATA if (i.token >> 11) == 3 => {
                // Immediate constant buffer: vec4s of raw bits.
                let len = u32::try_from(i.raw.len() / 4).unwrap_or(0);
                if len == 0 {
                    return Ok(());
                }
                let elements: Vec<u32> = i
                    .raw
                    .chunks_exact(4)
                    .map(|c| self.m.constant_vector(Scalar::Float, c))
                    .collect();
                let array = self.m.array(self.f4, len);
                let value = self.m.id();
                // A constant array, stored into a variable at entry.
                let p = self.ptr(STORAGE_FUNCTION, array);
                let var = self.m.local(p);
                let mut operands = vec![array, value];
                operands.extend(elements);
                self.composite_constant(operands);
                self.m.emit_void(op::STORE, &[var, value]);
                self.icb = Some((var, len));
            }
            o::DCL_INDEX_RANGE
            | o::DCL_GS_INPUT_PRIMITIVE
            | o::DCL_GS_OUTPUT_PRIMITIVE_TOPOLOGY
            | o::DCL_MAX_OUTPUT_VERTEX_COUNT => {}
            _ => {}
        }
        Ok(())
    }

    fn composite_constant(&mut self, operands: Vec<u32>) {
        // The builder's composite cache is keyed by constituents; arrays of
        // vectors are rare enough to emit directly.
        self.m.raw_global(op::CONSTANT_COMPOSITE, &operands);
    }

    fn declare_builtin_input(&mut self, r: u32, name: u32) -> Result<(), TranslateError> {
        let int = self.m.scalar(Scalar::Int);
        let source = match (self.stage, name) {
            (Stage::Vertex, 6 | 8) => {
                self.m.capability(CAP_DRAW_PARAMETERS);
                let (index, base) = if name == 6 {
                    (BUILTIN_VERTEX_INDEX, BUILTIN_BASE_VERTEX)
                } else {
                    (BUILTIN_INSTANCE_INDEX, BUILTIN_BASE_INSTANCE)
                };
                let a = self.input_var(int, &[&[DECORATION_BUILTIN, index]]);
                let b = self.input_var(int, &[&[DECORATION_BUILTIN, base]]);
                InputSource::IndexMinusBase(a, b)
            }
            (Stage::Pixel, 1) => {
                let v = self.input_var(self.f4, &[&[DECORATION_BUILTIN, BUILTIN_FRAG_COORD]]);
                InputSource::FragCoord(v)
            }
            (Stage::Pixel, 9) => {
                let b = self.m.scalar(Scalar::Bool);
                let v = self.input_var(b, &[&[DECORATION_BUILTIN, BUILTIN_FRONT_FACING]]);
                InputSource::FrontFacing(v)
            }
            (Stage::Pixel, 7) => {
                self.m.capability(CAP_GEOMETRY);
                let v = self.input_var(
                    int,
                    &[
                        &[DECORATION_BUILTIN, BUILTIN_PRIMITIVE_ID],
                        &[DECORATION_FLAT],
                    ],
                );
                InputSource::IntBuiltin(v)
            }
            (Stage::Pixel, 10) => {
                self.m.capability(CAP_SAMPLE_RATE_SHADING);
                let v = self.input_var(
                    int,
                    &[&[DECORATION_BUILTIN, BUILTIN_SAMPLE_ID], &[DECORATION_FLAT]],
                );
                InputSource::IntBuiltin(v)
            }
            (stage, name) => {
                return unsupported(format_args!("system value {name} as {stage:?} input"));
            }
        };
        // A system value may share a register with others: same variable.
        let local = match self.inputs.get(&r) {
            Some(l) => *l,
            None => {
                let l = self.register_var();
                self.inputs.insert(r, l);
                l
            }
        };
        self.input_sources.push((local, source));
        Ok(())
    }

    fn bind(&mut self, var: u32, kind: BindingKind) {
        let b = binding(self.stage, &kind);
        self.m.decorate(var, &[DECORATION_DESCRIPTOR_SET, 0]);
        self.m.decorate(var, &[DECORATION_BINDING, b]);
        self.bindings.push(Binding { kind, binding: b });
    }

    /// Copies interface inputs into the input registers.
    fn load_inputs(&mut self) {
        let sources = std::mem::take(&mut self.input_sources);
        for (local, source) in &sources {
            let value = match *source {
                InputSource::Plain(var, ty) => {
                    let vty = self.vec(ty);
                    let v = self.m.emit(op::LOAD, vty, &[var]);
                    self.as_register(v, ty)
                }
                InputSource::FragCoord(var) => {
                    let v = self.m.emit(op::LOAD, self.f4, &[var]);
                    // Direct3D gives w, Vulkan 1/w.
                    let f = self.m.scalar(Scalar::Float);
                    let w = self.m.emit(op::COMPOSITE_EXTRACT, f, &[v, 3]);
                    let one = self.m.constant_f32_bits(1.0f32.to_bits());
                    let inv = self.m.emit(op::F_DIV, f, &[one, w]);
                    let x = self.m.emit(op::COMPOSITE_EXTRACT, f, &[v, 0]);
                    let y = self.m.emit(op::COMPOSITE_EXTRACT, f, &[v, 1]);
                    let z = self.m.emit(op::COMPOSITE_EXTRACT, f, &[v, 2]);
                    self.m
                        .emit(op::COMPOSITE_CONSTRUCT, self.f4, &[x, y, z, inv])
                }
                InputSource::IndexMinusBase(a, b) => {
                    let int = self.m.scalar(Scalar::Int);
                    let a = self.m.emit(op::LOAD, int, &[a]);
                    let b = self.m.emit(op::LOAD, int, &[b]);
                    let diff = self.m.emit(op::I_SUB, int, &[a, b]);
                    self.splat_int(diff)
                }
                InputSource::IntBuiltin(var) => {
                    let int = self.m.scalar(Scalar::Int);
                    let v = self.m.emit(op::LOAD, int, &[var]);
                    self.splat_int(v)
                }
                InputSource::FrontFacing(var) => {
                    let b = self.m.scalar(Scalar::Bool);
                    let u = self.m.scalar(Scalar::Uint);
                    let v = self.m.emit(op::LOAD, b, &[var]);
                    let ones = self.m.constant_u32(u32::MAX);
                    let zero = self.m.constant_u32(0);
                    let s = self.m.emit(op::SELECT, u, &[v, ones, zero]);
                    let u4 = self.m.emit(op::COMPOSITE_CONSTRUCT, self.u4, &[s, s, s, s]);
                    self.m.emit(op::BITCAST, self.f4, &[u4])
                }
            };
            self.m.emit_void(op::STORE, &[*local, value]);
        }
    }

    fn splat_int(&mut self, v: u32) -> u32 {
        let i4 = self.m.emit(op::COMPOSITE_CONSTRUCT, self.i4, &[v, v, v, v]);
        self.m.emit(op::BITCAST, self.f4, &[i4])
    }

    /// Copies output registers to the interface, then returns.
    fn ret(&mut self) {
        let sinks: Vec<(u32, u32, Scalar)> = self.output_sinks.clone();
        for (var, local, ty) in sinks {
            let v = self.m.emit(op::LOAD, self.f4, &[local]);
            let v = self.typed(v, ty);
            self.m.emit_void(op::STORE, &[var, v]);
        }
        if let Some((var, local)) = self.depth {
            let f = self.m.scalar(Scalar::Float);
            let v = self.m.emit(op::LOAD, self.f4, &[local]);
            let x = self.m.emit(op::COMPOSITE_EXTRACT, f, &[v, 0]);
            self.m.emit_void(op::STORE, &[var, x]);
        }
        self.m.emit_void(op::RETURN, &[]);
    }

    /// Starts a new block (after a terminator).
    fn block(&mut self) -> u32 {
        let label = self.m.id();
        self.m.emit_void(op::LABEL, &[label]);
        label
    }

    fn as_register(&mut self, v: u32, ty: Scalar) -> u32 {
        if ty == Scalar::Float {
            v
        } else {
            self.m.emit(op::BITCAST, self.f4, &[v])
        }
    }

    fn typed(&mut self, v: u32, ty: Scalar) -> u32 {
        if ty == Scalar::Float {
            v
        } else {
            let t = self.vec(ty);
            self.m.emit(op::BITCAST, t, &[v])
        }
    }

    // ---- operands -------------------------------------------------------

    /// The `uint` value of an index.
    fn index(&mut self, index: &Index) -> Result<u32, TranslateError> {
        let u = self.m.scalar(Scalar::Uint);
        Ok(match index {
            Index::Imm(i) => self.m.constant_u32(*i),
            Index::Relative(reg) | Index::ImmRelative(_, reg) => {
                let v = self.src(reg, Scalar::Uint)?;
                let x = self.m.emit(op::COMPOSITE_EXTRACT, u, &[v, 0]);
                match index {
                    Index::ImmRelative(base, _) => {
                        let base = self.m.constant_u32(*base);
                        self.m.emit(op::I_ADD, u, &[base, x])
                    }
                    _ => x,
                }
            }
        })
    }

    /// A pointer to the `vec4` register an operand names.
    fn register(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        let missing =
            |what: &str| TranslateError(format!("{what} {:?} is not declared", operand.indices));
        match operand.kind {
            T_TEMP => {
                let r = operand.index0().ok_or_else(|| missing("temporary"))?;
                self.temps
                    .get(r as usize)
                    .copied()
                    .ok_or_else(|| missing("temporary"))
            }
            T_INPUT => {
                let r = operand.index0().ok_or_else(|| missing("input"))?;
                self.inputs.get(&r).copied().ok_or_else(|| missing("input"))
            }
            T_OUTPUT => {
                let r = operand.index0().ok_or_else(|| missing("output"))?;
                self.outputs
                    .get(&r)
                    .copied()
                    .ok_or_else(|| missing("output"))
            }
            T_OUTPUT_DEPTH => self
                .depth
                .map(|(_, local)| local)
                .ok_or_else(|| missing("depth output")),
            T_INPUT_PRIMITIVEID => self
                .inputs
                .get(&u32::MAX)
                .copied()
                .ok_or_else(|| missing("primitive ID")),
            T_INDEXABLE_TEMP => {
                let r = operand
                    .index0()
                    .ok_or_else(|| missing("indexable temporary"))?;
                let (var, _) = *self
                    .indexable
                    .get(&r)
                    .ok_or_else(|| missing("indexable temporary"))?;
                let index = operand
                    .indices
                    .get(1)
                    .ok_or_else(|| missing("indexable temporary"))?;
                let i = self.index(index)?;
                let p = self.ptr(STORAGE_FUNCTION, self.f4);
                Ok(self.m.emit(op::ACCESS_CHAIN, p, &[var, i]))
            }
            T_CONSTANT_BUFFER => {
                let slot = operand.index0().ok_or_else(|| missing("constant buffer"))?;
                let var = *self
                    .cbs
                    .get(&slot)
                    .ok_or_else(|| missing("constant buffer"))?;
                let index = operand
                    .indices
                    .get(1)
                    .ok_or_else(|| missing("constant buffer"))?;
                let i = self.index(index)?;
                let zero = self.m.constant_u32(0);
                let p = self.ptr(STORAGE_UNIFORM, self.f4);
                Ok(self.m.emit(op::ACCESS_CHAIN, p, &[var, zero, i]))
            }
            T_IMMEDIATE_CONSTANT_BUFFER => {
                let (var, _) = self
                    .icb
                    .ok_or_else(|| missing("immediate constant buffer"))?;
                let index = operand
                    .indices
                    .first()
                    .ok_or_else(|| missing("immediate constant buffer"))?;
                let i = self.index(index)?;
                let p = self.ptr(STORAGE_FUNCTION, self.f4);
                Ok(self.m.emit(op::ACCESS_CHAIN, p, &[var, i]))
            }
            other => unsupported(format_args!("operand type {other}")),
        }
    }

    /// A source operand as a `vec4` of `ty`, swizzled and modified.
    fn src(&mut self, operand: &Operand, ty: Scalar) -> Result<u32, TranslateError> {
        let vty = self.vec(ty);
        let value = if operand.kind == T_IMMEDIATE32 {
            let bits: [u32; 4] = match operand.imm.as_slice() {
                [x] => [*x; 4],
                [x, y, z, w] => [*x, *y, *z, *w],
                _ => return unsupported("immediate operand"),
            };
            let v = self.m.constant_vector(ty, &bits);
            // Immediates take no swizzle beyond their own order.
            return Ok(self.modify(v, operand.modifier, ty));
        } else {
            let p = self.register(operand)?;
            let v = self.m.emit(op::LOAD, self.f4, &[p]);
            self.typed(v, ty)
        };
        let s = swizzle_of(operand.select);
        let value = if s == [0, 1, 2, 3] {
            value
        } else {
            self.m.emit(
                op::VECTOR_SHUFFLE,
                vty,
                &[value, value, s[0], s[1], s[2], s[3]],
            )
        };
        Ok(self.modify(value, operand.modifier, ty))
    }

    fn modify(&mut self, v: u32, modifier: Modifier, ty: Scalar) -> u32 {
        let vty = self.vec(ty);
        let abs = |tx: &mut Self, v| match ty {
            Scalar::Float => tx.m.glsl(vty, glsl::F_ABS, &[v]),
            _ => tx.m.glsl(vty, 5, &[v]), // SAbs
        };
        let neg = |tx: &mut Self, v| match ty {
            Scalar::Float => tx.m.emit(op::F_NEGATE, vty, &[v]),
            _ => tx.m.emit(op::S_NEGATE, vty, &[v]),
        };
        match modifier {
            Modifier::None => v,
            Modifier::Neg => neg(self, v),
            Modifier::Abs => abs(self, v),
            Modifier::AbsNeg => {
                let a = abs(self, v);
                neg(self, a)
            }
        }
    }

    /// Writes `value` (a `vec4` of `ty`) to a destination operand.
    fn store(
        &mut self,
        dst: &Operand,
        value: u32,
        ty: Scalar,
        saturate: bool,
    ) -> Result<(), TranslateError> {
        if dst.kind == T_NULL {
            return Ok(());
        }
        let mut value = value;
        if saturate && ty == Scalar::Float {
            let zero = self.m.constant_vector(Scalar::Float, &[0; 4]);
            let one = self
                .m
                .constant_vector(Scalar::Float, &[1.0f32.to_bits(); 4]);
            value = self.m.glsl(self.f4, glsl::N_CLAMP, &[value, zero, one]);
        }
        let value = self.as_register(value, ty);
        let p = self.register(dst)?;
        let mask = mask_of(dst);
        let stored = if mask == 0xf {
            value
        } else {
            let old = self.m.emit(op::LOAD, self.f4, &[p]);
            let lane = |i: u32| if mask & (1 << i) != 0 { i } else { i | 4 };
            self.m.emit(
                op::VECTOR_SHUFFLE,
                self.f4,
                &[value, old, lane(0), lane(1), lane(2), lane(3)],
            )
        };
        self.m.emit_void(op::STORE, &[p, stored]);
        Ok(())
    }

    fn operand<'b>(&self, i: &'b Instruction, n: usize) -> Result<&'b Operand, TranslateError> {
        i.operands
            .get(n)
            .ok_or_else(|| TranslateError(format!("instruction {} lacks operand {n}", i.opcode)))
    }

    /// `dst = f(src...)` with every operand of type `ty`.
    fn alu(
        &mut self,
        i: &Instruction,
        ty: Scalar,
        sources: usize,
        f: impl FnOnce(&mut Self, &[u32]) -> u32,
    ) -> Result<(), TranslateError> {
        let mut args = Vec::with_capacity(sources);
        for n in 1..=sources {
            let operand = self.operand(i, n)?;
            args.push(self.src(operand, ty)?);
        }
        let result = f(self, &args);
        let dst = self.operand(i, 0)?;
        self.store(dst, result, ty, i.saturate())
    }

    /// A comparison: `dst = cmp(a, b) ? ~0 : 0`.
    fn compare(&mut self, i: &Instruction, ty: Scalar, opcode: u16) -> Result<(), TranslateError> {
        let a = self.src(self.operand(i, 1)?, ty)?;
        let b = self.src(self.operand(i, 2)?, ty)?;
        let c = self.m.emit(opcode, self.b4, &[a, b]);
        let r = self.mask_from_bools(c);
        let dst = self.operand(i, 0)?;
        self.store(dst, r, Scalar::Uint, false)
    }

    fn mask_from_bools(&mut self, c: u32) -> u32 {
        let ones = self.m.constant_vector(Scalar::Uint, &[u32::MAX; 4]);
        let zeros = self.m.constant_vector(Scalar::Uint, &[0; 4]);
        self.m.emit(op::SELECT, self.u4, &[c, ones, zeros])
    }

    /// The boolean of a conditional instruction's operand `n` (component
    /// selected), tested for zero or non-zero.
    fn condition(&mut self, i: &Instruction, n: usize) -> Result<u32, TranslateError> {
        let v = self.src(self.operand(i, n)?, Scalar::Uint)?;
        let u = self.m.scalar(Scalar::Uint);
        let b = self.m.scalar(Scalar::Bool);
        let x = self.m.emit(op::COMPOSITE_EXTRACT, u, &[v, 0]);
        let zero = self.m.constant_u32(0);
        let opcode = if i.test_nonzero() {
            op::I_NOT_EQUAL
        } else {
            op::I_EQUAL
        };
        Ok(self.m.emit(opcode, b, &[x, zero]))
    }

    fn splat(&mut self, ty: Scalar, s: u32) -> u32 {
        let vty = self.vec(ty);
        self.m.emit(op::COMPOSITE_CONSTRUCT, vty, &[s, s, s, s])
    }

    fn dot(&mut self, i: &Instruction, n: u32) -> Result<(), TranslateError> {
        let a = self.src(self.operand(i, 1)?, Scalar::Float)?;
        let b = self.src(self.operand(i, 2)?, Scalar::Float)?;
        let f = self.m.scalar(Scalar::Float);
        let (a, b) = if n == 4 {
            (a, b)
        } else {
            let t = self.m.vector(Scalar::Float, n);
            let lanes: Vec<u32> = (0..n).collect();
            let mut oa = vec![a, a];
            oa.extend(&lanes);
            let mut ob = vec![b, b];
            ob.extend(&lanes);
            (
                self.m.emit(op::VECTOR_SHUFFLE, t, &oa),
                self.m.emit(op::VECTOR_SHUFFLE, t, &ob),
            )
        };
        let d = self.m.emit(op::DOT, f, &[a, b]);
        let r = self.splat(Scalar::Float, d);
        let dst = self.operand(i, 0)?;
        self.store(dst, r, Scalar::Float, i.saturate())
    }

    // ---- resources ------------------------------------------------------

    fn resource(&self, operand: &Operand) -> Result<&Resource, TranslateError> {
        let slot = operand.index0().unwrap_or(u32::MAX);
        self.resources
            .get(&slot)
            .ok_or_else(|| TranslateError(format!("resource t{slot} is not declared")))
    }

    /// The first `n` lanes of a `vec4` (a scalar for 1).
    fn first(&mut self, v: u32, ty: Scalar, n: u32) -> u32 {
        if n == 4 {
            return v;
        }
        let t = self.m.vector(ty, n);
        if n == 1 {
            return self.m.emit(op::COMPOSITE_EXTRACT, t, &[v, 0]);
        }
        let mut operands = vec![v, v];
        operands.extend(0..n);
        self.m.emit(op::VECTOR_SHUFFLE, t, &operands)
    }

    fn sample(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        if self.stage != Stage::Pixel && matches!(i.opcode, o::SAMPLE | o::SAMPLE_B) {
            return unsupported("implicit-lod sampling outside pixel shaders");
        }
        let resource_op = self.operand(i, 2)?;
        let sampler_op = self.operand(i, 3)?;
        let (var, image_ty, dimension, ret) = {
            let r = self.resource(resource_op)?;
            (r.var, r.image_ty, r.dimension, r.ret)
        };
        let Some((_, coords, _, false)) = dimension_info(dimension).filter(|d| d.0 != 5) else {
            return unsupported(format_args!("sampling resource dimension {dimension}"));
        };
        let slot = sampler_op.index0().unwrap_or(u32::MAX);
        let sampler_var = *self
            .samplers
            .get(&slot)
            .ok_or_else(|| TranslateError(format!("sampler s{slot} is not declared")))?;
        let address = self.src(self.operand(i, 1)?, Scalar::Float)?;
        let coord = self.first(address, Scalar::Float, coords);
        let image = self.m.emit(op::LOAD, image_ty, &[var]);
        let sampler_ty = self.m.sampler();
        let sampler = self.m.emit(op::LOAD, sampler_ty, &[sampler_var]);
        let sampled_ty = self.m.sampled_image(image_ty);
        let sampled = self
            .m
            .emit(op::SAMPLED_IMAGE, sampled_ty, &[image, sampler]);
        let f = self.m.scalar(Scalar::Float);
        let mut mask = 0u32;
        let mut extra = Vec::new();
        let opcode = match i.opcode {
            o::SAMPLE => op::IMAGE_SAMPLE_IMPLICIT_LOD,
            o::SAMPLE_B => {
                mask |= IMAGE_BIAS;
                let b = self.src(self.operand(i, 4)?, Scalar::Float)?;
                extra.push(self.m.emit(op::COMPOSITE_EXTRACT, f, &[b, 0]));
                op::IMAGE_SAMPLE_IMPLICIT_LOD
            }
            o::SAMPLE_L => {
                mask |= IMAGE_LOD;
                let l = self.src(self.operand(i, 4)?, Scalar::Float)?;
                extra.push(self.m.emit(op::COMPOSITE_EXTRACT, f, &[l, 0]));
                op::IMAGE_SAMPLE_EXPLICIT_LOD
            }
            o::SAMPLE_D => {
                mask |= IMAGE_GRAD;
                // Gradients have the coordinates' components, without the
                // array index.
                let arrayed = dimension_info(dimension).is_some_and(|d| d.2);
                let gn = coords.saturating_sub(u32::from(arrayed)).max(1);
                let dx = self.src(self.operand(i, 4)?, Scalar::Float)?;
                let dy = self.src(self.operand(i, 5)?, Scalar::Float)?;
                extra.push(self.first(dx, Scalar::Float, gn));
                extra.push(self.first(dy, Scalar::Float, gn));
                op::IMAGE_SAMPLE_EXPLICIT_LOD
            }
            _ => return unsupported(format_args!("sample opcode {}", i.opcode)),
        };
        if let Some(offsets) = i.texel_offsets().filter(|o| *o != [0; 3]) {
            let n = match dimension {
                2 | 7 => 1,
                5 => 3,
                3 | 8 => 2,
                _ => 0,
            };
            if n > 0 {
                mask |= IMAGE_CONST_OFFSET;
                let parts: Vec<u32> = offsets[..n].iter().map(|v| *v as u32).collect();
                extra.push(self.m.constant_vector(Scalar::Int, &parts));
            }
        }
        let rty = self.vec(ret);
        let mut operands = vec![sampled, coord];
        if mask != 0 {
            operands.push(mask);
            operands.extend(extra);
        }
        let result = self.m.emit(opcode, rty, &operands);
        let s = swizzle_of(resource_op.select);
        let result = self.m.emit(
            op::VECTOR_SHUFFLE,
            rty,
            &[result, result, s[0], s[1], s[2], s[3]],
        );
        let dst = self.operand(i, 0)?;
        self.store(dst, result, ret, i.saturate())
    }

    fn load(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let resource_op = self.operand(i, 2)?;
        let (var, image_ty, dimension, ret) = {
            let r = self.resource(resource_op)?;
            (r.var, r.image_ty, r.dimension, r.ret)
        };
        let Some((dim, coords, _, ms)) = dimension_info(dimension) else {
            return unsupported(format_args!("ld from dimension {dimension}"));
        };
        if ms {
            return unsupported("ld_ms");
        }
        let address = self.src(self.operand(i, 1)?, Scalar::Int)?;
        let coord = self.first(address, Scalar::Int, coords);
        let image = self.m.emit(op::LOAD, image_ty, &[var]);
        let rty = self.vec(ret);
        let mut operands = vec![image, coord];
        if dim != 5 {
            let int = self.m.scalar(Scalar::Int);
            let lod = self.m.emit(op::COMPOSITE_EXTRACT, int, &[address, 3]);
            operands.extend([IMAGE_LOD, lod]);
        }
        let result = self.m.emit(op::IMAGE_FETCH, rty, &operands);
        let s = swizzle_of(resource_op.select);
        let result = self.m.emit(
            op::VECTOR_SHUFFLE,
            rty,
            &[result, result, s[0], s[1], s[2], s[3]],
        );
        let dst = self.operand(i, 0)?;
        self.store(dst, result, ret, i.saturate())
    }

    fn resinfo(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        self.m.capability(CAP_IMAGE_QUERY);
        let resource_op = self.operand(i, 2)?;
        let (var, image_ty, dimension) = {
            let r = self.resource(resource_op)?;
            (r.var, r.image_ty, r.dimension)
        };
        let Some((dim, coords, arrayed, ms)) = dimension_info(dimension) else {
            return unsupported(format_args!("resinfo on dimension {dimension}"));
        };
        if dim == 5 || ms {
            return unsupported("resinfo on buffers or multisampled images");
        }
        // Size components: cube maps report 2, arrays one more.
        let size_n = match dim {
            3 => 2u32.saturating_add(u32::from(arrayed)),
            _ => coords,
        };
        let int = self.m.scalar(Scalar::Int);
        let mip = self.src(self.operand(i, 1)?, Scalar::Int)?;
        let mip = self.m.emit(op::COMPOSITE_EXTRACT, int, &[mip, 0]);
        let image = self.m.emit(op::LOAD, image_ty, &[var]);
        let size_ty = self.m.vector(Scalar::Int, size_n);
        let size = self
            .m
            .emit(op::IMAGE_QUERY_SIZE_LOD, size_ty, &[image, mip]);
        let levels = self.m.emit(op::IMAGE_QUERY_LEVELS, int, &[image]);
        let zero = self.m.constant_i32(0);
        let mut lanes = Vec::new();
        for n in 0..3 {
            lanes.push(if n < size_n {
                if size_n == 1 {
                    size
                } else {
                    self.m.emit(op::COMPOSITE_EXTRACT, int, &[size, n])
                }
            } else {
                zero
            });
        }
        lanes.push(levels);
        let v = self.m.emit(op::COMPOSITE_CONSTRUCT, self.i4, &lanes);
        let (result, ty) = match i.resinfo_return() {
            2 => (v, Scalar::Int),
            r => {
                let f = self.m.emit(op::CONVERT_S_TO_F, self.f4, &[v]);
                if r == 1 {
                    let one = self
                        .m
                        .constant_vector(Scalar::Float, &[1.0f32.to_bits(); 4]);
                    let rcp = self.m.emit(op::F_DIV, self.f4, &[one, f]);
                    // The mip count stays as it is.
                    (
                        self.m
                            .emit(op::VECTOR_SHUFFLE, self.f4, &[rcp, f, 0, 1, 2, 7]),
                        Scalar::Float,
                    )
                } else {
                    (f, Scalar::Float)
                }
            }
        };
        let s = swizzle_of(resource_op.select);
        let vty = self.vec(ty);
        let result = self.m.emit(
            op::VECTOR_SHUFFLE,
            vty,
            &[result, result, s[0], s[1], s[2], s[3]],
        );
        let dst = self.operand(i, 0)?;
        self.store(dst, result, ty, i.saturate())
    }

    // ---- instructions ---------------------------------------------------

    fn instruction(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        use Scalar::{Float as F, Int as I, Uint as U};
        let f4 = self.f4;
        let i4 = self.i4;
        let u4 = self.u4;
        match i.opcode {
            o::ADD => self.alu(i, F, 2, |t, a| t.m.emit(op::F_ADD, f4, &[a[0], a[1]]))?,
            o::MUL => self.alu(i, F, 2, |t, a| t.m.emit(op::F_MUL, f4, &[a[0], a[1]]))?,
            o::DIV => self.alu(i, F, 2, |t, a| t.m.emit(op::F_DIV, f4, &[a[0], a[1]]))?,
            o::MAD => self.alu(i, F, 3, |t, a| {
                let p = t.m.emit(op::F_MUL, f4, &[a[0], a[1]]);
                t.m.emit(op::F_ADD, f4, &[p, a[2]])
            })?,
            o::MIN => self.alu(i, F, 2, |t, a| t.m.glsl(f4, glsl::N_MIN, &[a[0], a[1]]))?,
            o::MAX => self.alu(i, F, 2, |t, a| t.m.glsl(f4, glsl::N_MAX, &[a[0], a[1]]))?,
            o::MOV => self.alu(i, F, 1, |_, a| a[0])?,
            o::FRC => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::FRACT, &[a[0]]))?,
            o::ROUND_NE => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::ROUND_EVEN, &[a[0]]))?,
            o::ROUND_NI => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::FLOOR, &[a[0]]))?,
            o::ROUND_PI => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::CEIL, &[a[0]]))?,
            o::ROUND_Z => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::TRUNC, &[a[0]]))?,
            o::RSQ => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::INVERSE_SQRT, &[a[0]]))?,
            o::SQRT => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::SQRT, &[a[0]]))?,
            o::EXP => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::EXP2, &[a[0]]))?,
            o::LOG => self.alu(i, F, 1, |t, a| t.m.glsl(f4, glsl::LOG2, &[a[0]]))?,
            o::RCP => self.alu(i, F, 1, |t, a| {
                let one = t.m.constant_vector(Scalar::Float, &[1.0f32.to_bits(); 4]);
                t.m.emit(op::F_DIV, f4, &[one, a[0]])
            })?,
            o::DP2 => self.dot(i, 2)?,
            o::DP3 => self.dot(i, 3)?,
            o::DP4 => self.dot(i, 4)?,
            o::DERIV_RTX
            | o::DERIV_RTY
            | o::DERIV_RTX_COARSE
            | o::DERIV_RTY_COARSE
            | o::DERIV_RTX_FINE
            | o::DERIV_RTY_FINE => {
                if self.stage != Stage::Pixel {
                    return unsupported("derivatives outside pixel shaders");
                }
                let opcode = match i.opcode {
                    o::DERIV_RTX => op::DPDX,
                    o::DERIV_RTY => op::DPDY,
                    o::DERIV_RTX_COARSE => op::DPDX_COARSE,
                    o::DERIV_RTY_COARSE => op::DPDY_COARSE,
                    o::DERIV_RTX_FINE => op::DPDX_FINE,
                    _ => op::DPDY_FINE,
                };
                if !matches!(i.opcode, o::DERIV_RTX | o::DERIV_RTY) {
                    self.m.capability(CAP_DERIVATIVE_CONTROL);
                }
                self.alu(i, F, 1, |t, a| t.m.emit(opcode, f4, &[a[0]]))?;
            }
            o::SINCOS => {
                let a = self.src(self.operand(i, 2)?, F)?;
                let s = self.m.glsl(f4, glsl::SIN, &[a]);
                let c = self.m.glsl(f4, glsl::COS, &[a]);
                let (d0, d1) = (self.operand(i, 0)?, self.operand(i, 1)?);
                self.store(d0, s, F, i.saturate())?;
                self.store(d1, c, F, i.saturate())?;
            }
            o::MOVC => {
                let c = self.src(self.operand(i, 1)?, U)?;
                let a = self.src(self.operand(i, 2)?, F)?;
                let b = self.src(self.operand(i, 3)?, F)?;
                let zeros = self.m.constant_vector(Scalar::Uint, &[0; 4]);
                let cond = self.m.emit(op::I_NOT_EQUAL, self.b4, &[c, zeros]);
                let r = self.m.emit(op::SELECT, f4, &[cond, a, b]);
                self.store(self.operand(i, 0)?, r, F, i.saturate())?;
            }
            o::EQ => self.compare(i, F, op::F_ORD_EQUAL)?,
            o::NE => self.compare(i, F, op::F_UNORD_NOT_EQUAL)?,
            o::LT => self.compare(i, F, op::F_ORD_LESS_THAN)?,
            o::GE => self.compare(i, F, op::F_ORD_GREATER_THAN_EQUAL)?,
            o::IEQ => self.compare(i, I, op::I_EQUAL)?,
            o::INE => self.compare(i, I, op::I_NOT_EQUAL)?,
            o::ILT => self.compare(i, I, op::S_LESS_THAN)?,
            o::IGE => self.compare(i, I, op::S_GREATER_THAN_EQUAL)?,
            o::ULT => self.compare(i, U, op::U_LESS_THAN)?,
            o::UGE => self.compare(i, U, op::U_GREATER_THAN_EQUAL)?,
            o::IADD => self.alu(i, I, 2, |t, a| t.m.emit(op::I_ADD, i4, &[a[0], a[1]]))?,
            o::IMAD | o::UMAD => self.alu(i, I, 3, |t, a| {
                let p = t.m.emit(op::I_MUL, i4, &[a[0], a[1]]);
                t.m.emit(op::I_ADD, i4, &[p, a[2]])
            })?,
            o::IMUL | o::UMUL => {
                if self.operand(i, 0)?.kind != T_NULL {
                    return unsupported("the high half of imul/umul");
                }
                let a = self.src(self.operand(i, 2)?, I)?;
                let b = self.src(self.operand(i, 3)?, I)?;
                let r = self.m.emit(op::I_MUL, i4, &[a, b]);
                self.store(self.operand(i, 1)?, r, I, false)?;
            }
            o::UDIV => {
                // Direct3D: dividing by 0 gives ~0 for both results.
                let a = self.src(self.operand(i, 2)?, U)?;
                let b = self.src(self.operand(i, 3)?, U)?;
                let zeros = self.m.constant_vector(Scalar::Uint, &[0; 4]);
                let ones = self.m.constant_vector(Scalar::Uint, &[u32::MAX; 4]);
                let is_zero = self.m.emit(op::I_EQUAL, self.b4, &[b, zeros]);
                let q = self.m.emit(op::U_DIV, u4, &[a, b]);
                let r = self.m.emit(op::U_MOD, u4, &[a, b]);
                let q = self.m.emit(op::SELECT, u4, &[is_zero, ones, q]);
                let r = self.m.emit(op::SELECT, u4, &[is_zero, ones, r]);
                self.store(self.operand(i, 0)?, q, U, false)?;
                self.store(self.operand(i, 1)?, r, U, false)?;
            }
            o::INEG => self.alu(i, I, 1, |t, a| t.m.emit(op::S_NEGATE, i4, &[a[0]]))?,
            o::NOT => self.alu(i, U, 1, |t, a| t.m.emit(op::NOT, u4, &[a[0]]))?,
            o::AND => self.alu(i, U, 2, |t, a| t.m.emit(op::BITWISE_AND, u4, &[a[0], a[1]]))?,
            o::OR => self.alu(i, U, 2, |t, a| t.m.emit(op::BITWISE_OR, u4, &[a[0], a[1]]))?,
            o::XOR => self.alu(i, U, 2, |t, a| t.m.emit(op::BITWISE_XOR, u4, &[a[0], a[1]]))?,
            o::ISHL | o::ISHR | o::USHR => {
                let opcode = match i.opcode {
                    o::ISHL => op::SHIFT_LEFT_LOGICAL,
                    o::ISHR => op::SHIFT_RIGHT_ARITHMETIC,
                    _ => op::SHIFT_RIGHT_LOGICAL,
                };
                let ty = if i.opcode == o::ISHR { I } else { U };
                self.alu(i, ty, 2, |t, a| {
                    // Direct3D uses the low 5 bits of the count.
                    let vty = t.vec(ty);
                    let mask = t.m.constant_vector(ty, &[31; 4]);
                    let count = t.m.emit(op::BITWISE_AND, vty, &[a[1], mask]);
                    t.m.emit(opcode, vty, &[a[0], count])
                })?;
            }
            o::IMAX => self.alu(i, I, 2, |t, a| t.m.glsl(i4, glsl::S_MAX, &[a[0], a[1]]))?,
            o::IMIN => self.alu(i, I, 2, |t, a| t.m.glsl(i4, glsl::S_MIN, &[a[0], a[1]]))?,
            o::UMAX => self.alu(i, U, 2, |t, a| t.m.glsl(u4, glsl::U_MAX, &[a[0], a[1]]))?,
            o::UMIN => self.alu(i, U, 2, |t, a| t.m.glsl(u4, glsl::U_MIN, &[a[0], a[1]]))?,
            o::ITOF | o::UTOF => {
                let ty = if i.opcode == o::ITOF { I } else { U };
                let opcode = if ty == I {
                    op::CONVERT_S_TO_F
                } else {
                    op::CONVERT_U_TO_F
                };
                let a = self.src(self.operand(i, 1)?, ty)?;
                let r = self.m.emit(opcode, f4, &[a]);
                self.store(self.operand(i, 0)?, r, F, i.saturate())?;
            }
            o::FTOI | o::FTOU => {
                let (ty, opcode) = if i.opcode == o::FTOI {
                    (I, op::CONVERT_F_TO_S)
                } else {
                    (U, op::CONVERT_F_TO_U)
                };
                let a = self.src(self.operand(i, 1)?, F)?;
                let vty = self.vec(ty);
                let r = self.m.emit(opcode, vty, &[a]);
                self.store(self.operand(i, 0)?, r, ty, false)?;
            }
            o::SAMPLE | o::SAMPLE_L | o::SAMPLE_B | o::SAMPLE_D => self.sample(i)?,
            o::LD => self.load(i)?,
            o::RESINFO => self.resinfo(i)?,
            o::IF => {
                let cond = self.condition(i, 0)?;
                let (then, otherwise, merge) = (self.m.id(), self.m.id(), self.m.id());
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
            o::ELSE => match self.flow.last_mut() {
                Some(Flow::If {
                    merge,
                    otherwise,
                    has_else,
                }) if !*has_else => {
                    *has_else = true;
                    let (merge, otherwise) = (*merge, *otherwise);
                    self.m.emit_void(op::BRANCH, &[merge]);
                    self.m.emit_void(op::LABEL, &[otherwise]);
                }
                _ => return Err(TranslateError("else outside an if".into())),
            },
            o::ENDIF => match self.flow.pop() {
                Some(Flow::If {
                    merge,
                    otherwise,
                    has_else,
                }) => {
                    self.m.emit_void(op::BRANCH, &[merge]);
                    if !has_else {
                        self.m.emit_void(op::LABEL, &[otherwise]);
                        self.m.emit_void(op::BRANCH, &[merge]);
                    }
                    self.m.emit_void(op::LABEL, &[merge]);
                }
                _ => return Err(TranslateError("endif outside an if".into())),
            },
            o::LOOP => {
                let (header, body, next, merge) =
                    (self.m.id(), self.m.id(), self.m.id(), self.m.id());
                self.m.emit_void(op::BRANCH, &[header]);
                self.m.emit_void(op::LABEL, &[header]);
                self.m.emit_void(op::LOOP_MERGE, &[merge, next, 0]);
                self.m.emit_void(op::BRANCH, &[body]);
                self.m.emit_void(op::LABEL, &[body]);
                self.flow.push(Flow::Loop {
                    header,
                    merge,
                    next,
                });
            }
            o::ENDLOOP => match self.flow.pop() {
                Some(Flow::Loop {
                    header,
                    merge,
                    next,
                }) => {
                    self.m.emit_void(op::BRANCH, &[next]);
                    self.m.emit_void(op::LABEL, &[next]);
                    self.m.emit_void(op::BRANCH, &[header]);
                    self.m.emit_void(op::LABEL, &[merge]);
                }
                _ => return Err(TranslateError("endloop outside a loop".into())),
            },
            o::BREAK | o::CONTINUE => {
                let target = self.loop_target(i.opcode == o::BREAK)?;
                self.m.emit_void(op::BRANCH, &[target]);
                self.block();
            }
            o::BREAKC | o::CONTINUEC => {
                let target = self.loop_target(i.opcode == o::BREAKC)?;
                let cond = self.condition(i, 0)?;
                let (go, stay) = (self.m.id(), self.m.id());
                self.m.emit_void(op::SELECTION_MERGE, &[stay, 0]);
                self.m.emit_void(op::BRANCH_CONDITIONAL, &[cond, go, stay]);
                self.m.emit_void(op::LABEL, &[go]);
                self.m.emit_void(op::BRANCH, &[target]);
                self.m.emit_void(op::LABEL, &[stay]);
            }
            o::RET => {
                self.ret();
                self.block();
            }
            o::RETC | o::DISCARD => {
                let cond = self.condition(i, 0)?;
                let (go, stay) = (self.m.id(), self.m.id());
                self.m.emit_void(op::SELECTION_MERGE, &[stay, 0]);
                self.m.emit_void(op::BRANCH_CONDITIONAL, &[cond, go, stay]);
                self.m.emit_void(op::LABEL, &[go]);
                if i.opcode == o::RETC {
                    self.ret();
                } else {
                    if self.stage != Stage::Pixel {
                        return unsupported("discard outside pixel shaders");
                    }
                    self.m.emit_void(op::KILL, &[]);
                }
                self.m.emit_void(op::LABEL, &[stay]);
            }
            o::NOP | o::CUSTOMDATA | o::LABEL => {}
            opcode if (o::DCL_RESOURCE..=o::DCL_GLOBAL_FLAGS).contains(&opcode) => {}
            opcode => return unsupported(format_args!("opcode {opcode}")),
        }
        Ok(())
    }

    fn loop_target(&self, exit: bool) -> Result<u32, TranslateError> {
        self.flow
            .iter()
            .rev()
            .find_map(|f| match f {
                Flow::Loop { merge, next, .. } => Some(if exit { *merge } else { *next }),
                Flow::If { .. } => None,
            })
            .ok_or_else(|| TranslateError("break or continue outside a loop".into()))
    }
}
