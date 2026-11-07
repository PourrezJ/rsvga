//! Shader model 5 integer and conversion instructions, with Direct3D's
//! exact results: bit counts and scans, bit field extraction and
//! insertion, half floats, carries and borrows, 64-bit products, swaps.

use super::*;

impl Tx<'_> {
    /// `firstbit_hi`, `firstbit_shi`, `firstbit_lo`: Direct3D counts the
    /// high scans from the most significant bit, and returns ~0 when no
    /// bit is found.
    fn first_bit(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let (ty, find) = match i.opcode {
            o::FIRSTBIT_HI => (Scalar::Uint, glsl::FIND_U_MSB),
            o::FIRSTBIT_SHI => (Scalar::Int, glsl::FIND_S_MSB),
            _ => (Scalar::Uint, glsl::FIND_I_LSB),
        };
        let a = self.src(self.operand(i, 1)?, ty)?;
        let vty = self.vec(ty);
        let found = self.m.glsl(vty, find, &[a]);
        let found = self.typed_bits(found, ty, Scalar::Uint);
        let result = if i.opcode == o::FIRSTBIT_LO {
            // FindILsb gives -1 (~0) for 0, as Direct3D does.
            found
        } else {
            let u4 = self.u4;
            let none = self.m.constant_vector(Scalar::Uint, &[u32::MAX; 4]);
            let thirty_one = self.m.constant_vector(Scalar::Uint, &[31; 4]);
            let from_top = self.m.emit(op::I_SUB, u4, &[thirty_one, found]);
            let missing = self.m.emit(op::I_EQUAL, self.b4, &[found, none]);
            self.m.emit(op::SELECT, u4, &[missing, none, from_top])
        };
        self.store(self.operand(i, 0)?, result, Scalar::Uint, false)
    }

    /// Reinterprets a `vec4` of `from` as `to`.
    fn typed_bits(&mut self, v: u32, from: Scalar, to: Scalar) -> u32 {
        if from == to {
            v
        } else {
            let t = self.vec(to);
            self.m.emit(op::BITCAST, t, &[v])
        }
    }

    /// `ubfe` and `ibfe`: `width` bits at `offset` (both modulo 32), with
    /// Direct3D's rules when they run past bit 31 or `width` is 0.
    fn bit_extract(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let signed = i.opcode == o::IBFE;
        let ty = if signed { Scalar::Int } else { Scalar::Uint };
        let u4 = self.u4;
        let b4 = self.b4;
        let width = self.src(self.operand(i, 1)?, Scalar::Uint)?;
        let offset = self.src(self.operand(i, 2)?, Scalar::Uint)?;
        let value = self.src(self.operand(i, 3)?, ty)?;
        let low5 = self.m.constant_vector(Scalar::Uint, &[31; 4]);
        let width = self.m.emit(op::BITWISE_AND, u4, &[width, low5]);
        let offset = self.m.emit(op::BITWISE_AND, u4, &[offset, low5]);
        let thirty_two = self.m.constant_vector(Scalar::Uint, &[32; 4]);
        let end = self.m.emit(op::I_ADD, u4, &[width, offset]);
        let inside = self.m.emit(op::U_LESS_THAN, b4, &[end, thirty_two]);
        let left = self.m.emit(op::I_SUB, u4, &[thirty_two, end]);
        let right = self.m.emit(op::I_SUB, u4, &[thirty_two, width]);
        let vty = self.vec(ty);
        let shift_right = if signed {
            op::SHIFT_RIGHT_ARITHMETIC
        } else {
            op::SHIFT_RIGHT_LOGICAL
        };
        let up = self.m.emit(op::SHIFT_LEFT_LOGICAL, vty, &[value, left]);
        let field = self.m.emit(shift_right, vty, &[up, right]);
        let tail = self.m.emit(shift_right, vty, &[value, offset]);
        let r = self.m.emit(op::SELECT, vty, &[inside, field, tail]);
        let zero = self.m.constant_vector(Scalar::Uint, &[0; 4]);
        let empty = self.m.emit(op::I_EQUAL, b4, &[width, zero]);
        let zero = self.m.constant_vector(ty, &[0; 4]);
        let r = self.m.emit(op::SELECT, vty, &[empty, zero, r]);
        self.store(self.operand(i, 0)?, r, ty, false)
    }

    /// `bfi`: `width` low bits of `insert` placed at `offset` in `base`.
    fn bit_insert(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let u4 = self.u4;
        let width = self.src(self.operand(i, 1)?, Scalar::Uint)?;
        let offset = self.src(self.operand(i, 2)?, Scalar::Uint)?;
        let insert = self.src(self.operand(i, 3)?, Scalar::Uint)?;
        let base = self.src(self.operand(i, 4)?, Scalar::Uint)?;
        let low5 = self.m.constant_vector(Scalar::Uint, &[31; 4]);
        let width = self.m.emit(op::BITWISE_AND, u4, &[width, low5]);
        let offset = self.m.emit(op::BITWISE_AND, u4, &[offset, low5]);
        let one = self.m.constant_vector(Scalar::Uint, &[1; 4]);
        let bit = self.m.emit(op::SHIFT_LEFT_LOGICAL, u4, &[one, width]);
        let ones = self.m.emit(op::I_SUB, u4, &[bit, one]);
        let mask = self.m.emit(op::SHIFT_LEFT_LOGICAL, u4, &[ones, offset]);
        let moved = self.m.emit(op::SHIFT_LEFT_LOGICAL, u4, &[insert, offset]);
        let kept = self.m.emit(op::BITWISE_AND, u4, &[moved, mask]);
        let hole = self.m.emit(op::NOT, u4, &[mask]);
        let rest = self.m.emit(op::BITWISE_AND, u4, &[base, hole]);
        let r = self.m.emit(op::BITWISE_OR, u4, &[kept, rest]);
        self.store(self.operand(i, 0)?, r, Scalar::Uint, false)
    }

    /// `f32tof16` and `f16tof32`, a component at a time.
    fn half(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let to_half = i.opcode == o::F32TOF16;
        let (from, to) = if to_half {
            (Scalar::Float, Scalar::Uint)
        } else {
            (Scalar::Uint, Scalar::Float)
        };
        let a = self.src(self.operand(i, 1)?, from)?;
        let f = self.m.scalar(Scalar::Float);
        let u = self.m.scalar(Scalar::Uint);
        let f2 = self.m.vector(Scalar::Float, 2);
        let zero = self.m.constant_f32_bits(0);
        let mut parts = [0u32; 4];
        for (c, part) in (0u32..).zip(parts.iter_mut()) {
            *part = if to_half {
                let x = self.m.emit(op::COMPOSITE_EXTRACT, f, &[a, c]);
                let pair = self.m.emit(op::COMPOSITE_CONSTRUCT, f2, &[x, zero]);
                self.m.glsl(u, glsl::PACK_HALF_2X16, &[pair])
            } else {
                let x = self.m.emit(op::COMPOSITE_EXTRACT, u, &[a, c]);
                let pair = self.m.glsl(f2, glsl::UNPACK_HALF_2X16, &[x]);
                self.m.emit(op::COMPOSITE_EXTRACT, f, &[pair, 0])
            };
        }
        let vty = self.vec(to);
        let r = self.m.emit(op::COMPOSITE_CONSTRUCT, vty, &parts);
        self.store(self.operand(i, 0)?, r, to, i.saturate() && !to_half)
    }

    /// `uaddc`, `usubb`, and the full products of `umul` and `imul`: two
    /// destinations from one SPIR-V result structure.
    fn pair(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let (ty, opcode) = match i.opcode {
            o::UADDC => (Scalar::Uint, op::I_ADD_CARRY),
            o::USUBB => (Scalar::Uint, op::I_SUB_BORROW),
            o::UMUL => (Scalar::Uint, op::U_MUL_EXTENDED),
            _ => (Scalar::Int, op::S_MUL_EXTENDED),
        };
        let a = self.src(self.operand(i, 2)?, ty)?;
        let b = self.src(self.operand(i, 3)?, ty)?;
        let vty = self.vec(ty);
        let result_ty = self.m.structure(&[vty, vty]);
        let r = self.m.emit(opcode, result_ty, &[a, b]);
        let low = self.m.emit(op::COMPOSITE_EXTRACT, vty, &[r, 0]);
        let high = self.m.emit(op::COMPOSITE_EXTRACT, vty, &[r, 1]);
        // Products: high half first; additions: the sum first.
        let (first, second) = if matches!(i.opcode, o::UMUL | o::IMUL) {
            (high, low)
        } else {
            (low, high)
        };
        self.store(self.operand(i, 0)?, first, ty, false)?;
        self.store(self.operand(i, 1)?, second, ty, false)
    }

    /// `swapc`: `dst0 = c ? b : a`, `dst1 = c ? a : b`.
    fn swap(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let c = self.src(self.operand(i, 2)?, Scalar::Uint)?;
        let a = self.src(self.operand(i, 3)?, Scalar::Float)?;
        let b = self.src(self.operand(i, 4)?, Scalar::Float)?;
        let zeros = self.m.constant_vector(Scalar::Uint, &[0; 4]);
        let cond = self.m.emit(op::I_NOT_EQUAL, self.b4, &[c, zeros]);
        let f4 = self.f4;
        let first = self.m.emit(op::SELECT, f4, &[cond, b, a]);
        let second = self.m.emit(op::SELECT, f4, &[cond, a, b]);
        self.store(self.operand(i, 0)?, first, Scalar::Float, false)?;
        self.store(self.operand(i, 1)?, second, Scalar::Float, false)
    }

    /// The shader model 5 instructions of this module, if `i` is one.
    pub(super) fn bits(&mut self, i: &Instruction) -> Result<bool, TranslateError> {
        let u4 = self.u4;
        match i.opcode {
            o::COUNTBITS => self.alu(i, Scalar::Uint, 1, |t, a| {
                t.m.emit(op::BIT_COUNT, u4, &[a[0]])
            })?,
            o::BFREV => self.alu(i, Scalar::Uint, 1, |t, a| {
                t.m.emit(op::BIT_REVERSE, u4, &[a[0]])
            })?,
            o::FIRSTBIT_HI | o::FIRSTBIT_SHI | o::FIRSTBIT_LO => self.first_bit(i)?,
            o::UBFE | o::IBFE => self.bit_extract(i)?,
            o::BFI => self.bit_insert(i)?,
            o::F32TOF16 | o::F16TOF32 => self.half(i)?,
            o::UADDC | o::USUBB => self.pair(i)?,
            o::UMUL | o::IMUL if self.operand(i, 0)?.kind != T_NULL => self.pair(i)?,
            o::SWAPC => self.swap(i)?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}
