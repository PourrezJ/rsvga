//! A small SPIR-V module builder: just what the translator emits.
//!
//! Instructions go to the section they belong to (capabilities, entry
//! point, decorations, types and globals, function body); types and
//! constants are created once and reused.

use std::collections::HashMap;

/// SPIR-V opcodes used here.
#[allow(missing_docs)]
pub mod op {
    pub const EXT_INST_IMPORT: u16 = 11;
    pub const MEMORY_MODEL: u16 = 14;
    pub const ENTRY_POINT: u16 = 15;
    pub const EXECUTION_MODE: u16 = 16;
    pub const CAPABILITY: u16 = 17;
    pub const TYPE_VOID: u16 = 19;
    pub const TYPE_BOOL: u16 = 20;
    pub const TYPE_INT: u16 = 21;
    pub const TYPE_FLOAT: u16 = 22;
    pub const TYPE_VECTOR: u16 = 23;
    pub const TYPE_IMAGE: u16 = 25;
    pub const TYPE_SAMPLER: u16 = 26;
    pub const TYPE_SAMPLED_IMAGE: u16 = 27;
    pub const TYPE_ARRAY: u16 = 28;
    pub const TYPE_RUNTIME_ARRAY: u16 = 29;
    pub const TYPE_STRUCT: u16 = 30;
    pub const TYPE_POINTER: u16 = 32;
    pub const TYPE_FUNCTION: u16 = 33;
    pub const CONSTANT_TRUE: u16 = 41;
    pub const CONSTANT_FALSE: u16 = 42;
    pub const CONSTANT: u16 = 43;
    pub const CONSTANT_COMPOSITE: u16 = 44;
    pub const FUNCTION: u16 = 54;
    pub const FUNCTION_END: u16 = 56;
    pub const VARIABLE: u16 = 59;
    pub const LOAD: u16 = 61;
    pub const STORE: u16 = 62;
    pub const IMAGE_TEXEL_POINTER: u16 = 60;
    pub const ACCESS_CHAIN: u16 = 65;
    pub const ARRAY_LENGTH: u16 = 68;
    pub const DECORATE: u16 = 71;
    pub const MEMBER_DECORATE: u16 = 72;
    pub const VECTOR_SHUFFLE: u16 = 79;
    pub const COMPOSITE_CONSTRUCT: u16 = 80;
    pub const COMPOSITE_EXTRACT: u16 = 81;
    pub const SAMPLED_IMAGE: u16 = 86;
    pub const IMAGE_SAMPLE_IMPLICIT_LOD: u16 = 87;
    pub const IMAGE_SAMPLE_EXPLICIT_LOD: u16 = 88;
    pub const IMAGE_SAMPLE_DREF_IMPLICIT_LOD: u16 = 89;
    pub const IMAGE_SAMPLE_DREF_EXPLICIT_LOD: u16 = 90;
    pub const IMAGE_FETCH: u16 = 95;
    pub const IMAGE_GATHER: u16 = 96;
    pub const IMAGE_DREF_GATHER: u16 = 97;
    pub const IMAGE_READ: u16 = 98;
    pub const IMAGE_WRITE: u16 = 99;
    pub const IMAGE_QUERY_SIZE_LOD: u16 = 103;
    pub const IMAGE_QUERY_SIZE: u16 = 104;
    pub const IMAGE_QUERY_LEVELS: u16 = 106;
    pub const CONVERT_F_TO_U: u16 = 109;
    pub const CONVERT_F_TO_S: u16 = 110;
    pub const CONVERT_S_TO_F: u16 = 111;
    pub const CONVERT_U_TO_F: u16 = 112;
    pub const BITCAST: u16 = 124;
    pub const S_NEGATE: u16 = 126;
    pub const F_NEGATE: u16 = 127;
    pub const I_ADD: u16 = 128;
    pub const I_SUB: u16 = 130;
    pub const F_ADD: u16 = 129;
    pub const I_MUL: u16 = 132;
    pub const F_MUL: u16 = 133;
    pub const U_DIV: u16 = 134;
    pub const F_DIV: u16 = 136;
    pub const U_MOD: u16 = 137;
    pub const DOT: u16 = 148;
    pub const I_ADD_CARRY: u16 = 149;
    pub const I_SUB_BORROW: u16 = 150;
    pub const U_MUL_EXTENDED: u16 = 151;
    pub const S_MUL_EXTENDED: u16 = 152;
    pub const ANY: u16 = 154;
    pub const SELECT: u16 = 169;
    pub const I_EQUAL: u16 = 170;
    pub const I_NOT_EQUAL: u16 = 171;
    pub const U_GREATER_THAN_EQUAL: u16 = 174;
    pub const S_GREATER_THAN_EQUAL: u16 = 175;
    pub const U_LESS_THAN: u16 = 176;
    pub const S_LESS_THAN: u16 = 177;
    pub const F_ORD_EQUAL: u16 = 180;
    pub const F_UNORD_NOT_EQUAL: u16 = 183;
    pub const F_ORD_LESS_THAN: u16 = 184;
    pub const F_ORD_GREATER_THAN_EQUAL: u16 = 190;
    pub const SHIFT_RIGHT_LOGICAL: u16 = 194;
    pub const SHIFT_RIGHT_ARITHMETIC: u16 = 195;
    pub const SHIFT_LEFT_LOGICAL: u16 = 196;
    pub const BITWISE_OR: u16 = 197;
    pub const BITWISE_XOR: u16 = 198;
    pub const BITWISE_AND: u16 = 199;
    pub const NOT: u16 = 200;
    pub const BIT_REVERSE: u16 = 204;
    pub const BIT_COUNT: u16 = 205;
    pub const DPDX: u16 = 207;
    pub const DPDY: u16 = 208;
    pub const DPDX_FINE: u16 = 210;
    pub const DPDY_FINE: u16 = 211;
    pub const DPDX_COARSE: u16 = 213;
    pub const DPDY_COARSE: u16 = 214;
    pub const EMIT_VERTEX: u16 = 218;
    pub const END_PRIMITIVE: u16 = 219;
    pub const CONTROL_BARRIER: u16 = 224;
    pub const MEMORY_BARRIER: u16 = 225;
    pub const ATOMIC_EXCHANGE: u16 = 229;
    pub const ATOMIC_COMPARE_EXCHANGE: u16 = 230;
    pub const ATOMIC_I_ADD: u16 = 234;
    pub const ATOMIC_S_MIN: u16 = 236;
    pub const ATOMIC_U_MIN: u16 = 237;
    pub const ATOMIC_S_MAX: u16 = 238;
    pub const ATOMIC_U_MAX: u16 = 239;
    pub const ATOMIC_AND: u16 = 240;
    pub const ATOMIC_OR: u16 = 241;
    pub const ATOMIC_XOR: u16 = 242;
    pub const LOOP_MERGE: u16 = 246;
    pub const SELECTION_MERGE: u16 = 247;
    pub const LABEL: u16 = 248;
    pub const BRANCH: u16 = 249;
    pub const BRANCH_CONDITIONAL: u16 = 250;
    pub const KILL: u16 = 252;
    pub const RETURN: u16 = 253;
    pub const EXT_INST: u16 = 12;
}

/// `GLSL.std.450` extended instructions used here.
#[allow(missing_docs)]
pub mod glsl {
    pub const ROUND_EVEN: u32 = 2;
    pub const TRUNC: u32 = 3;
    pub const F_ABS: u32 = 4;
    pub const FLOOR: u32 = 8;
    pub const CEIL: u32 = 9;
    pub const FRACT: u32 = 10;
    pub const SIN: u32 = 13;
    pub const COS: u32 = 14;
    pub const EXP2: u32 = 29;
    pub const LOG2: u32 = 30;
    pub const SQRT: u32 = 31;
    pub const INVERSE_SQRT: u32 = 32;
    pub const U_MIN: u32 = 38;
    pub const S_MIN: u32 = 39;
    pub const U_MAX: u32 = 41;
    pub const S_MAX: u32 = 42;
    pub const PACK_HALF_2X16: u32 = 58;
    pub const UNPACK_HALF_2X16: u32 = 62;
    pub const FIND_I_LSB: u32 = 73;
    pub const FIND_S_MSB: u32 = 74;
    pub const FIND_U_MSB: u32 = 75;
    pub const N_MIN: u32 = 79;
    pub const N_MAX: u32 = 80;
    pub const N_CLAMP: u32 = 81;
}

/// Scalar kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scalar {
    /// 32-bit float.
    Float,
    /// 32-bit unsigned.
    Uint,
    /// 32-bit signed.
    Int,
    /// Boolean.
    Bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TypeKey {
    Void,
    Scalar(Scalar),
    Vector(Scalar, u32),
    Pointer(u32, u32),
    Array(u32, u32),
    Function(u32),
    Image(Vec<u32>),
    Sampler,
    SampledImage(u32),
}

/// A SPIR-V module in construction.
#[derive(Default)]
pub struct Module {
    next: u32,
    capabilities: Vec<u32>,
    imports: Vec<u32>,
    entry: Vec<u32>,
    modes: Vec<u32>,
    decorations: Vec<u32>,
    globals: Vec<u32>,
    /// The function body.
    pub code: Vec<u32>,
    /// Variables of the function, emitted in its first block.
    pub locals: Vec<u32>,
    types: HashMap<TypeKey, u32>,
    constants: HashMap<(u32, Vec<u32>), u32>,
    glsl: u32,
}

fn push(out: &mut Vec<u32>, opcode: u16, operands: &[u32]) {
    let count = u32::try_from(operands.len().saturating_add(1)).unwrap_or(u32::MAX);
    out.push((count << 16) | u32::from(opcode));
    out.extend_from_slice(operands);
}

/// A string as SPIR-V literal words.
fn string(s: &str) -> Vec<u32> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

impl Module {
    /// An empty module with GLSL.std.450 imported.
    pub fn new() -> Self {
        let mut m = Self {
            next: 1,
            ..Default::default()
        };
        m.glsl = m.id();
        let mut operands = vec![m.glsl];
        operands.extend(string("GLSL.std.450"));
        push(&mut m.imports, op::EXT_INST_IMPORT, &operands);
        m
    }

    /// A fresh result ID.
    pub fn id(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    /// Declares a capability (once).
    pub fn capability(&mut self, capability: u32) {
        let words = [(2 << 16) | u32::from(op::CAPABILITY), capability];
        if !self.capabilities.chunks_exact(2).any(|c| c == words) {
            self.capabilities.extend(words);
        }
    }

    /// `OpEntryPoint` and its execution modes.
    pub fn entry_point(&mut self, model: u32, function: u32, interface: &[u32], modes: &[&[u32]]) {
        let mut operands = vec![model, function];
        operands.extend(string("main"));
        operands.extend_from_slice(interface);
        push(&mut self.entry, op::ENTRY_POINT, &operands);
        for mode in modes {
            let mut operands = vec![function];
            operands.extend_from_slice(mode);
            push(&mut self.modes, op::EXECUTION_MODE, &operands);
        }
    }

    /// `OpDecorate`.
    pub fn decorate(&mut self, target: u32, decoration: &[u32]) {
        let mut operands = vec![target];
        operands.extend_from_slice(decoration);
        push(&mut self.decorations, op::DECORATE, &operands);
    }

    /// `OpMemberDecorate`.
    pub fn member_decorate(&mut self, target: u32, member: u32, decoration: &[u32]) {
        let mut operands = vec![target, member];
        operands.extend_from_slice(decoration);
        push(&mut self.decorations, op::MEMBER_DECORATE, &operands);
    }

    fn ty(&mut self, key: TypeKey, opcode: u16, operands: &[u32]) -> u32 {
        if let Some(id) = self.types.get(&key) {
            return *id;
        }
        let id = self.id();
        let mut all = vec![id];
        all.extend_from_slice(operands);
        push(&mut self.globals, opcode, &all);
        self.types.insert(key, id);
        id
    }

    /// `void`.
    pub fn void(&mut self) -> u32 {
        self.ty(TypeKey::Void, op::TYPE_VOID, &[])
    }

    /// A scalar type.
    pub fn scalar(&mut self, s: Scalar) -> u32 {
        match s {
            Scalar::Float => self.ty(TypeKey::Scalar(s), op::TYPE_FLOAT, &[32]),
            Scalar::Uint => self.ty(TypeKey::Scalar(s), op::TYPE_INT, &[32, 0]),
            Scalar::Int => self.ty(TypeKey::Scalar(s), op::TYPE_INT, &[32, 1]),
            Scalar::Bool => self.ty(TypeKey::Scalar(s), op::TYPE_BOOL, &[]),
        }
    }

    /// A vector type (`n` = 1: the scalar).
    pub fn vector(&mut self, s: Scalar, n: u32) -> u32 {
        let scalar = self.scalar(s);
        if n == 1 {
            return scalar;
        }
        self.ty(TypeKey::Vector(s, n), op::TYPE_VECTOR, &[scalar, n])
    }

    /// A pointer type.
    pub fn pointer(&mut self, class: u32, pointee: u32) -> u32 {
        self.ty(
            TypeKey::Pointer(class, pointee),
            op::TYPE_POINTER,
            &[class, pointee],
        )
    }

    /// An array type of `len` elements.
    pub fn array(&mut self, element: u32, len: u32) -> u32 {
        let length = self.constant_u32(len);
        self.ty(
            TypeKey::Array(element, len),
            op::TYPE_ARRAY,
            &[element, length],
        )
    }

    /// An array type of its own (never shared: for explicitly laid out
    /// blocks, whose decorations other uses must not see).
    pub fn array_unique(&mut self, element: u32, len: u32) -> u32 {
        let length = self.constant_u32(len);
        let id = self.id();
        push(&mut self.globals, op::TYPE_ARRAY, &[id, element, length]);
        id
    }

    /// A raw instruction in the types and globals section.
    pub fn raw_global(&mut self, opcode: u16, operands: &[u32]) {
        push(&mut self.globals, opcode, operands);
    }

    /// A struct type (never shared: decorations differ).
    pub fn structure(&mut self, members: &[u32]) -> u32 {
        let id = self.id();
        let mut all = vec![id];
        all.extend_from_slice(members);
        push(&mut self.globals, op::TYPE_STRUCT, &all);
        id
    }

    /// `void()`.
    pub fn function_type(&mut self) -> u32 {
        let void = self.void();
        self.ty(TypeKey::Function(void), op::TYPE_FUNCTION, &[void])
    }

    /// An image type: sampled type, then `OpTypeImage` operands.
    pub fn image(&mut self, operands: &[u32]) -> u32 {
        self.ty(TypeKey::Image(operands.to_vec()), op::TYPE_IMAGE, operands)
    }

    /// `OpTypeSampler`.
    pub fn sampler(&mut self) -> u32 {
        self.ty(TypeKey::Sampler, op::TYPE_SAMPLER, &[])
    }

    /// `OpTypeSampledImage` of `image`.
    pub fn sampled_image(&mut self, image: u32) -> u32 {
        self.ty(
            TypeKey::SampledImage(image),
            op::TYPE_SAMPLED_IMAGE,
            &[image],
        )
    }

    /// A constant of type `ty` with these value words (composites: their
    /// constituents).
    fn constant(&mut self, ty: u32, opcode: u16, words: Vec<u32>) -> u32 {
        if let Some(id) = self.constants.get(&(ty, words.clone())) {
            return *id;
        }
        let id = self.id();
        let mut all = vec![ty, id];
        all.extend_from_slice(&words);
        push(&mut self.globals, opcode, &all);
        self.constants.insert((ty, words), id);
        id
    }

    /// A 32-bit unsigned constant.
    pub fn constant_u32(&mut self, value: u32) -> u32 {
        let ty = self.scalar(Scalar::Uint);
        self.constant(ty, op::CONSTANT, vec![value])
    }

    /// A 32-bit signed constant.
    pub fn constant_i32(&mut self, value: i32) -> u32 {
        let ty = self.scalar(Scalar::Int);
        self.constant(ty, op::CONSTANT, vec![value as u32])
    }

    /// A float constant with these bits.
    pub fn constant_f32_bits(&mut self, bits: u32) -> u32 {
        let ty = self.scalar(Scalar::Float);
        self.constant(ty, op::CONSTANT, vec![bits])
    }

    /// A vector constant of scalars of kind `s` (bits).
    pub fn constant_vector(&mut self, s: Scalar, bits: &[u32]) -> u32 {
        let parts: Vec<u32> = bits
            .iter()
            .map(|b| match s {
                Scalar::Float => self.constant_f32_bits(*b),
                Scalar::Int => self.constant_i32(*b as i32),
                _ => self.constant_u32(*b),
            })
            .collect();
        if parts.len() == 1 {
            return parts[0];
        }
        let ty = self.vector(s, u32::try_from(bits.len()).unwrap_or(4));
        self.constant(ty, op::CONSTANT_COMPOSITE, parts)
    }

    /// A boolean constant.
    pub fn constant_bool(&mut self, value: bool) -> u32 {
        let ty = self.scalar(Scalar::Bool);
        let opcode = if value {
            op::CONSTANT_TRUE
        } else {
            op::CONSTANT_FALSE
        };
        self.constant(ty, opcode, Vec::new())
    }

    /// A global `OpVariable`.
    pub fn global(&mut self, pointer: u32, class: u32) -> u32 {
        let id = self.id();
        push(&mut self.globals, op::VARIABLE, &[pointer, id, class]);
        id
    }

    /// A function-local `OpVariable` (`Function` storage class).
    pub fn local(&mut self, pointer: u32) -> u32 {
        let id = self.id();
        push(&mut self.locals, op::VARIABLE, &[pointer, id, 7]);
        id
    }

    /// An instruction with a result, in the function body.
    pub fn emit(&mut self, opcode: u16, ty: u32, operands: &[u32]) -> u32 {
        let id = self.id();
        let mut all = vec![ty, id];
        all.extend_from_slice(operands);
        push(&mut self.code, opcode, &all);
        id
    }

    /// An instruction without a result, in the function body.
    pub fn emit_void(&mut self, opcode: u16, operands: &[u32]) {
        push(&mut self.code, opcode, operands);
    }

    /// A `GLSL.std.450` instruction.
    pub fn glsl(&mut self, ty: u32, instruction: u32, operands: &[u32]) -> u32 {
        let mut all = vec![self.glsl, instruction];
        all.extend_from_slice(operands);
        self.emit(op::EXT_INST, ty, &all)
    }

    /// The finished module (`function`: the entry point's ID; `body`:
    /// starts with its first label).
    pub fn finish(mut self, function: u32) -> Vec<u32> {
        let void = self.void();
        let fn_ty = self.function_type();
        let mut words = vec![0x0723_0203, 0x0001_0300, 0, self.next, 0];
        words.extend(&self.capabilities);
        words.extend(&self.imports);
        // Logical addressing, GLSL450 memory model.
        push(&mut words, op::MEMORY_MODEL, &[0, 1]);
        words.extend(&self.entry);
        words.extend(&self.modes);
        words.extend(&self.decorations);
        words.extend(&self.globals);
        push(&mut words, op::FUNCTION, &[void, function, 0, fn_ty]);
        // The first label, then the variables, then the rest of the body.
        let (label, rest) = self.code.split_at(2.min(self.code.len()));
        words.extend(label);
        words.extend(&self.locals);
        words.extend(rest);
        push(&mut words, op::FUNCTION_END, &[]);
        words[3] = self.next;
        words
    }
}
