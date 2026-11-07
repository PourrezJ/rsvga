//! The VGPU10 token stream: Direct3D 10/11 tokenized shader programs, as
//! the VMware driver passes them (`VGPU10ShaderTokens.h`).
//!
//! A program is a version token, a length token (in words, both
//! included), then instructions. Each instruction starts with an opcode
//! token holding its length; extended opcode tokens may follow, then
//! operands (self-describing: an operand token says how many index and
//! immediate words follow). Declarations carry trailing raw words (counts,
//! return types, system value names). Words after the program, if any,
//! belong to the VMware signature block, ignored here.

use crate::opcode as op;

/// Shader stage (`VGPU10_PROGRAM_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Pixel shader.
    Pixel,
    /// Vertex shader.
    Vertex,
    /// Geometry shader.
    Geometry,
    /// Hull shader.
    Hull,
    /// Domain shader.
    Domain,
    /// Compute shader.
    Compute,
}

/// Which components an operand selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// No components (labels, samplers...).
    None,
    /// A destination write mask (bit 0: x).
    Mask(u8),
    /// A source swizzle: component read for x, y, z, w.
    Swizzle([u8; 4]),
    /// One component, for every lane.
    Scalar(u8),
}

/// An operand index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Index {
    /// A constant.
    Imm(u32),
    /// The value of a register component.
    Relative(Box<Operand>),
    /// A constant plus a register component.
    ImmRelative(u32, Box<Operand>),
}

/// `VGPU10_OPERAND_MODIFIER_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    /// None.
    None,
    /// Negate.
    Neg,
    /// Absolute value.
    Abs,
    /// Negated absolute value.
    AbsNeg,
}

/// An operand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operand {
    /// `VGPU10_OPERAND_TYPE_*`.
    pub kind: u32,
    /// Components: 0, 1 or 4.
    pub components: u8,
    /// Selection of components.
    pub select: Select,
    /// Indices (register number, constant buffer slot and element...).
    pub indices: Vec<Index>,
    /// Source modifier.
    pub modifier: Modifier,
    /// Immediate values (`IMMEDIATE32`: one or four words).
    pub imm: Vec<u32>,
}

impl Operand {
    /// The first index, if constant.
    pub fn index0(&self) -> Option<u32> {
        match self.indices.first()? {
            Index::Imm(i) => Some(*i),
            _ => None,
        }
    }
}

/// An instruction or declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// `VGPU10_OPCODE_*`.
    pub opcode: u32,
    /// The opcode token, for the opcode-specific bits.
    pub token: u32,
    /// Extended opcode tokens.
    pub extended: Vec<u32>,
    /// Operands.
    pub operands: Vec<Operand>,
    /// Trailing raw words (declaration counts, return types, names; the
    /// payload of `CUSTOMDATA`).
    pub raw: Vec<u32>,
}

impl Instruction {
    /// Result clamped to [0, 1].
    pub fn saturate(&self) -> bool {
        self.token & (1 << 13) != 0
    }
    /// Conditional instructions test for non-zero (else zero).
    pub fn test_nonzero(&self) -> bool {
        self.token & (1 << 18) != 0
    }
    /// Interpolation mode of `DCL_INPUT_PS*` (`VGPU10_INTERPOLATION_*`).
    pub fn interpolation(&self) -> u32 {
        (self.token >> 11) & 0xf
    }
    /// Resource dimension of `DCL_RESOURCE` (`VGPU10_RESOURCE_DIMENSION_*`).
    pub fn resource_dimension(&self) -> u32 {
        (self.token >> 11) & 0x1f
    }
    /// Sampler mode of `DCL_SAMPLER` (`VGPU10_SAMPLER_MODE_*`).
    pub fn sampler_mode(&self) -> u32 {
        (self.token >> 11) & 0xf
    }
    /// `RESINFO` return type (`VGPU10_RESINFO_RETURN_*`).
    pub fn resinfo_return(&self) -> u32 {
        (self.token >> 11) & 0x3
    }
    /// Texel offsets of a sample instruction (`aoffimmi`), signed 4-bit.
    pub fn texel_offsets(&self) -> Option<[i32; 3]> {
        let token = self.extended.iter().find(|t| *t & 0x3f == 1)?;
        let signed = |shift: u32| (((token >> shift) & 0xf) as i32) << 28 >> 28;
        Some([signed(9), signed(13), signed(17)])
    }
}

/// A decoded program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// Stage.
    pub stage: Stage,
    /// Shader model major version.
    pub major: u8,
    /// Shader model minor version.
    pub minor: u8,
    /// Declarations and instructions, in order.
    pub instructions: Vec<Instruction>,
}

/// Why a program cannot be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer words than the header or an instruction announces.
    Truncated {
        /// Word offset.
        at: usize,
    },
    /// An instruction of length 0, or one whose operands overrun it.
    BadLength {
        /// Word offset.
        at: usize,
    },
    /// An operand this decoder does not know (n-component, 64-bit index).
    BadOperand {
        /// Word offset.
        at: usize,
    },
    /// Relative indices nested too deep.
    TooDeep,
    /// An unknown program type or version.
    BadVersion(u32),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated { at } => write!(f, "shader truncated at word {at}"),
            Self::BadLength { at } => write!(f, "bad instruction length at word {at}"),
            Self::BadOperand { at } => write!(f, "unsupported operand at word {at}"),
            Self::TooDeep => f.write_str("relative indices nested too deep"),
            Self::BadVersion(v) => write!(f, "unknown shader version token {v:#x}"),
        }
    }
}

/// Declarations whose words after the opcode token are all raw.
fn raw_only(opcode: u32) -> bool {
    matches!(
        opcode,
        op::DCL_TEMPS
            | op::DCL_INDEXABLE_TEMP
            | op::DCL_MAX_OUTPUT_VERTEX_COUNT
            | op::DCL_GS_INSTANCE_COUNT
            | op::DCL_THREAD_GROUP
            | op::DCL_HS_MAX_TESSFACTOR
            | op::DCL_HS_FORK_PHASE_INSTANCE_COUNT
            | op::DCL_HS_JOIN_PHASE_INSTANCE_COUNT
            | op::DCL_FUNCTION_BODY
            | op::DCL_FUNCTION_TABLE
            | op::DCL_INTERFACE
    )
}

/// Declarations: one operand (if any words), then raw words.
fn declaration(opcode: u32) -> bool {
    matches!(opcode, op::DCL_RESOURCE..=op::DCL_GLOBAL_FLAGS)
        || matches!(opcode, op::DCL_STREAM..=op::DCL_RESOURCE_STRUCTURED)
        || opcode == op::DCL_GS_INSTANCE_COUNT
}

/// Deepest nesting of relative indices.
const MAX_DEPTH: u32 = 4;

struct Reader<'a> {
    words: &'a [u32],
    at: usize,
    end: usize,
}

impl Reader<'_> {
    fn word(&mut self) -> Result<u32, DecodeError> {
        if self.at >= self.end {
            return Err(DecodeError::BadLength { at: self.at });
        }
        let w = *self
            .words
            .get(self.at)
            .ok_or(DecodeError::Truncated { at: self.at })?;
        self.at = self.at.saturating_add(1);
        Ok(w)
    }

    fn operand(&mut self, depth: u32) -> Result<Operand, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::TooDeep);
        }
        let start = self.at;
        let token = self.word()?;
        let (components, select) = match token & 3 {
            0 => (0, Select::None),
            1 => (1, Select::Scalar(0)),
            2 => {
                let select = match (token >> 2) & 3 {
                    0 => Select::Mask(((token >> 4) & 0xf) as u8),
                    1 => Select::Swizzle([
                        ((token >> 4) & 3) as u8,
                        ((token >> 6) & 3) as u8,
                        ((token >> 8) & 3) as u8,
                        ((token >> 10) & 3) as u8,
                    ]),
                    2 => Select::Scalar(((token >> 4) & 3) as u8),
                    _ => return Err(DecodeError::BadOperand { at: start }),
                };
                (4, select)
            }
            _ => return Err(DecodeError::BadOperand { at: start }),
        };
        let kind = (token >> 12) & 0xff;
        let dimension = (token >> 20) & 3;
        let mut modifier = Modifier::None;
        let mut extended = token >> 31 != 0;
        while extended {
            let ext = self.word()?;
            if ext & 0x3f == 1 {
                modifier = match (ext >> 6) & 0xff {
                    1 => Modifier::Neg,
                    2 => Modifier::Abs,
                    3 => Modifier::AbsNeg,
                    _ => Modifier::None,
                };
            }
            extended = ext >> 31 != 0;
        }
        let mut indices = Vec::new();
        for i in 0..dimension {
            let representation = (token >> 22u32.saturating_add(i.saturating_mul(3))) & 7;
            indices.push(match representation {
                0 => Index::Imm(self.word()?),
                2 => Index::Relative(Box::new(self.operand(depth.saturating_add(1))?)),
                3 => {
                    let base = self.word()?;
                    Index::ImmRelative(base, Box::new(self.operand(depth.saturating_add(1))?))
                }
                _ => return Err(DecodeError::BadOperand { at: start }),
            });
        }
        let mut imm = Vec::new();
        let count = match kind {
            // IMMEDIATE32
            4 => usize::from(components.max(1)),
            // IMMEDIATE64
            5 => usize::from(components.max(1)).saturating_mul(2),
            _ => 0,
        };
        for _ in 0..count {
            imm.push(self.word()?);
        }
        Ok(Operand {
            kind,
            components,
            select,
            indices,
            modifier,
            imm,
        })
    }
}

/// Decodes a program.
pub fn decode(words: &[u32]) -> Result<Program, DecodeError> {
    let version = *words.first().ok_or(DecodeError::Truncated { at: 0 })?;
    let length = *words.get(1).ok_or(DecodeError::Truncated { at: 1 })? as usize;
    let stage = match version >> 16 {
        0 => Stage::Pixel,
        1 => Stage::Vertex,
        2 => Stage::Geometry,
        3 => Stage::Hull,
        4 => Stage::Domain,
        5 => Stage::Compute,
        _ => return Err(DecodeError::BadVersion(version)),
    };
    let major = ((version >> 4) & 0xf) as u8;
    let minor = (version & 0xf) as u8;
    if !(4..=5).contains(&major) {
        return Err(DecodeError::BadVersion(version));
    }
    if length < 2 || length > words.len() {
        return Err(DecodeError::Truncated { at: length });
    }
    let mut instructions = Vec::new();
    let mut at = 2usize;
    while at < length {
        let token = words[at];
        let opcode = token & 0x7ff;
        let len = if opcode == op::CUSTOMDATA {
            *words
                .get(at.saturating_add(1))
                .ok_or(DecodeError::Truncated { at })? as usize
        } else {
            ((token >> 24) & 0x7f) as usize
        };
        let end = at.checked_add(len).ok_or(DecodeError::BadLength { at })?;
        if len == 0 || end > length {
            return Err(DecodeError::BadLength { at });
        }
        let mut r = Reader {
            words,
            at: at.saturating_add(1),
            end,
        };
        let mut instruction = Instruction {
            opcode,
            token,
            extended: Vec::new(),
            operands: Vec::new(),
            raw: Vec::new(),
        };
        if opcode == op::CUSTOMDATA {
            r.word()?; // the length
            while r.at < end {
                instruction.raw.push(r.word()?);
            }
        } else {
            let mut extended = token >> 31 != 0;
            while extended {
                let ext = r.word()?;
                instruction.extended.push(ext);
                extended = ext >> 31 != 0;
            }
            if raw_only(opcode) {
                while r.at < end {
                    instruction.raw.push(r.word()?);
                }
            } else if declaration(opcode) {
                if r.at < end && opcode != op::DCL_GLOBAL_FLAGS {
                    instruction.operands.push(r.operand(0)?);
                }
                while r.at < end {
                    instruction.raw.push(r.word()?);
                }
            } else {
                while r.at < end {
                    instruction.operands.push(r.operand(0)?);
                }
            }
        }
        instructions.push(instruction);
        at = end;
    }
    Ok(Program {
        stage,
        major,
        minor,
        instructions,
    })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    /// Operand tokens of the tests.
    const TEMP_MASK_X: u32 = 0x0010_0012; // r#.x, 4 components, mask mode
    const INPUT_XYZW: u32 = 0x0010_1e46; // v#.xyzw swizzle (identity)
    const IMM4: u32 = 0x0000_4e46; // 4 immediate words, swizzle .xyzw
    const CB_2D: u32 = 0x0020_8e46; // cb#[#].xyzw

    fn instr(opcode: u32, rest: &[u32]) -> Vec<u32> {
        let mut v = vec![opcode | (((rest.len() + 1) as u32) << 24)];
        v.extend_from_slice(rest);
        v
    }

    fn program(stage: u32, body: &[Vec<u32>]) -> Vec<u32> {
        let mut words = vec![(stage << 16) | 0x40, 0];
        for b in body {
            words.extend(b);
        }
        words[1] = words.len() as u32;
        words
    }

    #[test]
    fn a_small_vertex_shader() {
        let words = program(
            1,
            &[
                instr(op::DCL_TEMPS, &[2]),
                instr(op::DCL_CONSTANT_BUFFER, &[CB_2D, 0, 4]),
                // mov r0.x, v1.xyzw
                instr(op::MOV, &[TEMP_MASK_X, 0, INPUT_XYZW, 1]),
                // add r1.x, l(1, 2, 3, 4), cb0[3].xyzw
                instr(op::ADD, &[TEMP_MASK_X, 1, IMM4, 1, 2, 3, 4, CB_2D, 0, 3]),
                instr(op::RET, &[]),
            ],
        );
        let p = decode(&words).unwrap();
        assert_eq!((p.stage, p.major, p.minor), (Stage::Vertex, 4, 0));
        assert_eq!(p.instructions.len(), 5);
        assert_eq!(p.instructions[0].raw, [2]);
        let cb = &p.instructions[1].operands[0];
        assert_eq!((cb.kind, cb.indices.len()), (8, 2));
        let mov = &p.instructions[2];
        assert_eq!(mov.operands[0].select, Select::Mask(1));
        assert_eq!(mov.operands[1].select, Select::Swizzle([0, 1, 2, 3]));
        assert_eq!(mov.operands[1].index0(), Some(1));
        let add = &p.instructions[3];
        assert_eq!(add.operands[1].imm, [1, 2, 3, 4]);
        assert_eq!(add.operands[2].indices, [Index::Imm(0), Index::Imm(3)]);
    }

    #[test]
    fn relative_indices_modifiers_and_extended_tokens() {
        // mov r0.x, -|cb0[r1.x + 2].xyzw| : extended operand, relative index.
        let cb_rel = 0x8000_0000 | 0x0020_8e46 | (3 << 25); // index1: imm + relative
        let scalar_r1x = 0x0010_000a; // r#.x select-1
        let words = program(
            0,
            &[instr(
                op::MOV,
                &[TEMP_MASK_X, 0, cb_rel, (3 << 6) | 1, 0, 2, scalar_r1x, 1],
            )],
        );
        let p = decode(&words).unwrap();
        let src = &p.instructions[0].operands[1];
        assert_eq!(src.modifier, Modifier::AbsNeg);
        let Index::ImmRelative(2, reg) = &src.indices[1] else {
            panic!("{:?}", src.indices);
        };
        assert_eq!((reg.kind, reg.select), (0, Select::Scalar(0)));
        // sample with aoffimmi(-1, 2, 0): an extended opcode token.
        let offsets = 1 | (0xf << 9) | (2 << 13);
        let sample = vec![op::SAMPLE | (1 << 31) | (2 << 24), offsets];
        let p = decode(&program(0, &[sample])).unwrap();
        assert_eq!(p.instructions[0].texel_offsets(), Some([-1, 2, 0]));
    }

    #[test]
    fn hostile_programs_are_refused() {
        assert!(decode(&[]).is_err());
        assert_eq!(
            decode(&[0x0009_0040, 2]),
            Err(DecodeError::BadVersion(0x0009_0040))
        );
        assert!(
            decode(&[0x0001_0040, 9]).is_err(),
            "length beyond the words"
        );
        // An instruction of length 0, one overrunning the program.
        assert!(decode(&[0x0001_0040, 3, op::RET]).is_err());
        assert!(decode(&[0x0001_0040, 3, op::RET | (5 << 24)]).is_err());
        // Operands overrunning their instruction.
        assert!(decode(&program(1, &[instr(op::MOV, &[TEMP_MASK_X])])).is_err());
        // Relative indices nested forever.
        let rel = 0x0010_000a | (1 << 20) | (2 << 22);
        let mut body = vec![op::MOV | (40 << 24), TEMP_MASK_X, 0];
        body.extend(std::iter::repeat_n(rel, 37));
        assert_eq!(decode(&program(1, &[body])), Err(DecodeError::TooDeep));
    }
}
