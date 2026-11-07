//! Shader model 5 memory: raw and structured buffers (`t#`, `u#`),
//! group-shared memory (`g#`), typed unordered-access views, atomics,
//! barriers and `bufinfo`.
//!
//! Raw and structured buffers become storage buffers of 32-bit words
//! (`struct { uint words[]; }`), group-shared memory a `Workgroup` array of
//! words: both are addressed in words, from byte offsets (raw) or from an
//! element index and a byte offset in the element (structured). Reading
//! a resource as words is how Direct3D defines these accesses: the
//! resource's swizzle picks, for each destination component, the word at
//! that many words after the address. Typed views are storage images or
//! storage texel buffers, of an unknown format (the view's), except those
//! used by atomics, which Direct3D limits to `R32_UINT` and `R32_SINT`.

use super::*;

const STORAGE_WORKGROUP: u32 = 4;
const STORAGE_IMAGE: u32 = 11;
pub(super) const STORAGE_BUFFER: u32 = 12;
const DECORATION_NON_WRITABLE: u32 = 24;
const CAP_IMAGE_1D: u32 = 44;
const CAP_IMAGE_BUFFER: u32 = 47;
const CAP_STORAGE_IMAGE_READ_WITHOUT_FORMAT: u32 = 55;
const CAP_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT: u32 = 56;
const FORMAT_R32I: u32 = 24;
const FORMAT_R32UI: u32 = 33;
const SCOPE_DEVICE: u32 = 1;
const SCOPE_WORKGROUP: u32 = 2;
const SEMANTICS_ACQUIRE_RELEASE: u32 = 0x8;
const SEMANTICS_UNIFORM_MEMORY: u32 = 0x40;
const SEMANTICS_WORKGROUP_MEMORY: u32 = 0x100;
const SEMANTICS_IMAGE_MEMORY: u32 = 0x800;
/// Most words of group-shared memory (Direct3D allows 32 KiB).
const MAX_TGSM_WORDS: u32 = 8192;

/// A buffer read or written as words.
pub(super) struct Words {
    var: u32,
    /// `StorageBuffer` or `Workgroup`.
    class: u32,
    /// Bytes per element (0: raw).
    stride: u32,
}

/// Where an atomic operation or a typed access points.
enum Target {
    /// A word of a buffer or of group-shared memory.
    Word(u32, u32),
    /// A texel of a typed view (pointer, scalar type).
    Texel(u32, Scalar),
}

/// The scalar a typed view holds, from its return type token.
fn return_scalar(token: u32) -> Scalar {
    match token & 0xf {
        3 => Scalar::Int,
        4 => Scalar::Uint,
        _ => Scalar::Float,
    }
}

impl Tx<'_> {
    /// `struct { uint words[]; }`, decorated as a storage buffer block.
    fn words_block(&mut self) -> u32 {
        if let Some(block) = self.words_block {
            return block;
        }
        let u = self.m.scalar(Scalar::Uint);
        let array = self.m.id();
        self.m.raw_global(op::TYPE_RUNTIME_ARRAY, &[array, u]);
        self.m.decorate(array, &[DECORATION_ARRAY_STRIDE, 4]);
        let block = self.m.structure(&[array]);
        self.m.decorate(block, &[DECORATION_BLOCK]);
        self.m.member_decorate(block, 0, &[DECORATION_OFFSET, 0]);
        self.words_block = Some(block);
        block
    }

    /// `dcl_resource_raw`, `dcl_resource_structured`, `dcl_uav_raw`,
    /// `dcl_uav_structured`, `dcl_tgsm_raw` and `dcl_tgsm_structured`.
    pub(super) fn declare_words(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let operand = self.operand(i, 0)?;
        let slot = operand
            .index0()
            .ok_or_else(|| TranslateError("buffer declaration without register".into()))?;
        let kind = operand.kind;
        let stride = match i.opcode {
            o::DCL_RESOURCE_STRUCTURED | o::DCL_UAV_STRUCTURED | o::DCL_TGSM_STRUCTURED => {
                let stride = i.raw.first().copied().unwrap_or(0);
                if stride == 0 || stride % 4 != 0 || stride > 2048 {
                    return unsupported(format_args!("structure stride {stride}"));
                }
                stride
            }
            _ => 0,
        };
        if kind == T_TGSM && self.stage != Stage::Compute {
            return unsupported("group-shared memory outside compute shaders");
        }
        let words = if kind == T_TGSM {
            let bytes = match i.opcode {
                o::DCL_TGSM_RAW => i.raw.first().copied().unwrap_or(0),
                _ => stride.saturating_mul(i.raw.get(1).copied().unwrap_or(0)),
            };
            let len = bytes / 4;
            if len == 0 || len > MAX_TGSM_WORDS {
                return unsupported(format_args!("{bytes} bytes of group-shared memory"));
            }
            let u = self.m.scalar(Scalar::Uint);
            let array = self.m.array(u, len);
            let p = self.ptr(STORAGE_WORKGROUP, array);
            let var = self.m.global(p, STORAGE_WORKGROUP);
            Words {
                var,
                class: STORAGE_WORKGROUP,
                stride,
            }
        } else {
            let block = self.words_block();
            let p = self.ptr(STORAGE_BUFFER, block);
            let var = self.m.global(p, STORAGE_BUFFER);
            if kind == T_RESOURCE {
                self.m.decorate(var, &[DECORATION_NON_WRITABLE]);
                self.bind(var, BindingKind::BufferResource { slot });
            } else {
                self.bind(var, BindingKind::UaView { slot, dimension: 0 });
            }
            Words {
                var,
                class: STORAGE_BUFFER,
                stride,
            }
        };
        self.words.insert((kind, slot), words);
        Ok(())
    }

    /// `dcl_uav_typed`: a storage image or texel buffer.
    pub(super) fn declare_typed_uav(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let operand = self.operand(i, 0)?;
        let slot = operand
            .index0()
            .ok_or_else(|| TranslateError("UA view declaration without register".into()))?;
        let dimension = i.resource_dimension();
        let Some((dim, _, arrayed, false)) = dimension_info(dimension) else {
            return unsupported(format_args!("UA view dimension {dimension}"));
        };
        let ret = return_scalar(i.raw.first().copied().unwrap_or(5));
        let format = if self.atomic_uavs.contains(&slot) {
            if ret == Scalar::Int {
                FORMAT_R32I
            } else {
                FORMAT_R32UI
            }
        } else {
            self.m.capability(CAP_STORAGE_IMAGE_READ_WITHOUT_FORMAT);
            self.m.capability(CAP_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT);
            0
        };
        match dim {
            0 => self.m.capability(CAP_IMAGE_1D),
            5 => self.m.capability(CAP_IMAGE_BUFFER),
            _ => {}
        }
        let sampled = self.m.scalar(ret);
        let image_ty = self
            .m
            .image(&[sampled, dim, 0, u32::from(arrayed), 0, 2, format]);
        let p = self.ptr(STORAGE_UNIFORM_CONSTANT, image_ty);
        let var = self.m.global(p, STORAGE_UNIFORM_CONSTANT);
        self.bind(var, BindingKind::UaView { slot, dimension });
        self.typed_uavs.insert(
            slot,
            Resource {
                var,
                image_ty,
                dimension,
                ret,
            },
        );
        Ok(())
    }

    fn words_of(&self, operand: &Operand) -> Result<&Words, TranslateError> {
        let slot = operand.index0().unwrap_or(u32::MAX);
        self.words.get(&(operand.kind, slot)).ok_or_else(|| {
            TranslateError(format!(
                "buffer {slot} of operand type {} is not declared as raw or structured",
                operand.kind
            ))
        })
    }

    /// The first component of a source operand, as a `uint`.
    fn scalar_src(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        let v = self.src(operand, Scalar::Uint)?;
        let u = self.m.scalar(Scalar::Uint);
        Ok(self.m.emit(op::COMPOSITE_EXTRACT, u, &[v, 0]))
    }

    /// The word index of byte `offset` of element `index` (raw: `index`
    /// is `None` and `offset` counts from the start).
    fn word_index(&mut self, index: Option<u32>, offset: u32, stride: u32) -> u32 {
        let u = self.m.scalar(Scalar::Uint);
        let bytes = match index {
            Some(index) if stride != 0 => {
                let s = self.m.constant_u32(stride);
                let base = self.m.emit(op::I_MUL, u, &[index, s]);
                self.m.emit(op::I_ADD, u, &[base, offset])
            }
            _ => offset,
        };
        let two = self.m.constant_u32(2);
        self.m.emit(op::SHIFT_RIGHT_LOGICAL, u, &[bytes, two])
    }

    /// A pointer to word `index + delta` of a buffer.
    fn word_ptr(&mut self, words: (u32, u32), index: u32, delta: u32) -> u32 {
        let (var, class) = words;
        let u = self.m.scalar(Scalar::Uint);
        let index = if delta == 0 {
            index
        } else {
            let d = self.m.constant_u32(delta);
            self.m.emit(op::I_ADD, u, &[index, d])
        };
        let p = self.ptr(class, u);
        if class == STORAGE_WORKGROUP {
            self.m.emit(op::ACCESS_CHAIN, p, &[var, index])
        } else {
            let zero = self.m.constant_u32(0);
            self.m.emit(op::ACCESS_CHAIN, p, &[var, zero, index])
        }
    }

    /// `ld_raw` and `ld_structured`.
    pub(super) fn load_words(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let structured = i.opcode == o::LD_STRUCTURED;
        let resource_at = if structured { 3 } else { 2 };
        let resource_op = self.operand(i, resource_at)?;
        let (var, class, stride) = {
            let w = self.words_of(resource_op)?;
            (w.var, w.class, w.stride)
        };
        let index = if structured {
            Some(self.scalar_src(self.operand(i, 1)?)?)
        } else {
            None
        };
        let offset_at = if structured { 2 } else { 1 };
        let offset = self.scalar_src(self.operand(i, offset_at)?)?;
        let base = self.word_index(index, offset, stride);
        let dst = self.operand(i, 0)?;
        let mask = mask_of(dst);
        let s = swizzle_of(resource_op.select);
        let u = self.m.scalar(Scalar::Uint);
        let zero = self.m.constant_u32(0);
        let mut loaded: HashMap<u32, u32> = HashMap::new();
        let mut parts = [zero; 4];
        for (c, part) in parts.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            let word = s[c];
            let value = match loaded.get(&word) {
                Some(v) => *v,
                None => {
                    let p = self.word_ptr((var, class), base, word);
                    let v = self.m.emit(op::LOAD, u, &[p]);
                    loaded.insert(word, v);
                    v
                }
            };
            *part = value;
        }
        let u4 = self.u4;
        let v = self.m.emit(op::COMPOSITE_CONSTRUCT, u4, &parts);
        self.store(dst, v, Scalar::Uint, false)
    }

    /// `store_raw` and `store_structured`: the destination's mask (`x`,
    /// `xy`, `xyz` or `xyzw`) says how many words are written.
    pub(super) fn store_words(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let structured = i.opcode == o::STORE_STRUCTURED;
        let dst = self.operand(i, 0)?;
        let (var, class, stride) = {
            let w = self.words_of(dst)?;
            (w.var, w.class, w.stride)
        };
        let index = if structured {
            Some(self.scalar_src(self.operand(i, 1)?)?)
        } else {
            None
        };
        let offset = self.scalar_src(self.operand(i, if structured { 2 } else { 1 })?)?;
        let value = self.src(
            self.operand(i, if structured { 3 } else { 2 })?,
            Scalar::Uint,
        )?;
        let base = self.word_index(index, offset, stride);
        let mask = mask_of(dst);
        let u = self.m.scalar(Scalar::Uint);
        for c in 0..4u32 {
            if mask & (1 << c) == 0 {
                continue;
            }
            let p = self.word_ptr((var, class), base, c);
            let x = self.m.emit(op::COMPOSITE_EXTRACT, u, &[value, c]);
            self.m.emit_void(op::STORE, &[p, x]);
        }
        Ok(())
    }

    fn typed_uav(&self, operand: &Operand) -> Result<(u32, u32, u32, Scalar), TranslateError> {
        let slot = operand.index0().unwrap_or(u32::MAX);
        let r = self
            .typed_uavs
            .get(&slot)
            .ok_or_else(|| TranslateError(format!("typed UA view u{slot} is not declared")))?;
        Ok((r.var, r.image_ty, r.dimension, r.ret))
    }

    /// Texel coordinates of a typed view (signed integers).
    fn texel_coords(&mut self, address: &Operand, dimension: u32) -> Result<u32, TranslateError> {
        let Some((_, coords, _, _)) = dimension_info(dimension) else {
            return unsupported(format_args!("UA view dimension {dimension}"));
        };
        let a = self.src(address, Scalar::Int)?;
        Ok(self.first(a, Scalar::Int, coords))
    }

    /// `ld_uav_typed`.
    pub(super) fn load_typed(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let uav = self.operand(i, 2)?;
        let (var, image_ty, dimension, ret) = self.typed_uav(uav)?;
        let coord = self.texel_coords(self.operand(i, 1)?, dimension)?;
        let image = self.m.emit(op::LOAD, image_ty, &[var]);
        let rty = self.vec(ret);
        let texel = self.m.emit(op::IMAGE_READ, rty, &[image, coord]);
        self.finish_sample(i, uav, texel, ret)
    }

    /// `store_uav_typed`: every component goes (the view's format keeps
    /// what it has).
    pub(super) fn store_typed(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let uav = self.operand(i, 0)?;
        let (var, image_ty, dimension, ret) = self.typed_uav(uav)?;
        let coord = self.texel_coords(self.operand(i, 1)?, dimension)?;
        let value = self.src(self.operand(i, 2)?, ret)?;
        let image = self.m.emit(op::LOAD, image_ty, &[var]);
        self.m.emit_void(op::IMAGE_WRITE, &[image, coord, value]);
        Ok(())
    }

    /// What an atomic operation's destination and address point to.
    fn atomic_target(
        &mut self,
        dst: &Operand,
        address: &Operand,
    ) -> Result<Target, TranslateError> {
        if dst.kind == T_UAV
            && self
                .typed_uavs
                .contains_key(&dst.index0().unwrap_or(u32::MAX))
        {
            let (var, _, dimension, ret) = self.typed_uav(dst)?;
            let coord = self.texel_coords(address, dimension)?;
            let scalar = self.m.scalar(ret);
            let p = self.ptr(STORAGE_IMAGE, scalar);
            let zero = self.m.constant_u32(0);
            let texel = self.m.emit(op::IMAGE_TEXEL_POINTER, p, &[var, coord, zero]);
            return Ok(Target::Texel(texel, ret));
        }
        let (var, class, stride) = {
            let w = self.words_of(dst)?;
            (w.var, w.class, w.stride)
        };
        let a = self.src(address, Scalar::Uint)?;
        let u = self.m.scalar(Scalar::Uint);
        let x = self.m.emit(op::COMPOSITE_EXTRACT, u, &[a, 0]);
        let base = if stride == 0 {
            self.word_index(None, x, 0)
        } else {
            let y = self.m.emit(op::COMPOSITE_EXTRACT, u, &[a, 1]);
            self.word_index(Some(x), y, stride)
        };
        let p = self.word_ptr((var, class), base, 0);
        let scope = if class == STORAGE_WORKGROUP {
            SCOPE_WORKGROUP
        } else {
            SCOPE_DEVICE
        };
        Ok(Target::Word(p, scope))
    }

    /// `atomic_*` and `imm_atomic_*` (which also return the old value).
    pub(super) fn atomic(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let returns = (o::IMM_ATOMIC_IADD..=o::IMM_ATOMIC_UMIN).contains(&i.opcode);
        if matches!(i.opcode, o::IMM_ATOMIC_ALLOC | o::IMM_ATOMIC_CONSUME) {
            return unsupported("append and consume buffers");
        }
        let first = usize::from(returns);
        let dst = self.operand(i, first)?;
        let address = self.operand(i, first.saturating_add(1))?;
        let target = self.atomic_target(dst, address)?;
        let (pointer, scope, ty) = match target {
            Target::Word(p, scope) => (p, scope, Scalar::Uint),
            Target::Texel(p, ty) => (p, SCOPE_DEVICE, ty),
        };
        let scalar = self.m.scalar(ty);
        let value_at = first.saturating_add(2);
        let read = |tx: &mut Self, n: usize| -> Result<u32, TranslateError> {
            let v = tx.src(tx.operand(i, n)?, ty)?;
            Ok(tx.m.emit(op::COMPOSITE_EXTRACT, scalar, &[v, 0]))
        };
        let value = read(self, value_at)?;
        let scope_id = self.m.constant_u32(scope);
        let relaxed = self.m.constant_u32(0);
        let opcode = match i.opcode {
            o::ATOMIC_AND | o::IMM_ATOMIC_AND => op::ATOMIC_AND,
            o::ATOMIC_OR | o::IMM_ATOMIC_OR => op::ATOMIC_OR,
            o::ATOMIC_XOR | o::IMM_ATOMIC_XOR => op::ATOMIC_XOR,
            o::ATOMIC_IADD | o::IMM_ATOMIC_IADD => op::ATOMIC_I_ADD,
            o::ATOMIC_IMAX | o::IMM_ATOMIC_IMAX => op::ATOMIC_S_MAX,
            o::ATOMIC_IMIN | o::IMM_ATOMIC_IMIN => op::ATOMIC_S_MIN,
            o::ATOMIC_UMAX | o::IMM_ATOMIC_UMAX => op::ATOMIC_U_MAX,
            o::ATOMIC_UMIN | o::IMM_ATOMIC_UMIN => op::ATOMIC_U_MIN,
            o::IMM_ATOMIC_EXCH => op::ATOMIC_EXCHANGE,
            o::ATOMIC_CMP_STORE | o::IMM_ATOMIC_CMP_EXCH => op::ATOMIC_COMPARE_EXCHANGE,
            other => return unsupported(format_args!("atomic opcode {other}")),
        };
        let old = if opcode == op::ATOMIC_COMPARE_EXCHANGE {
            // Operands: compare value, then the value written.
            let new = read(self, value_at.saturating_add(1))?;
            self.m.emit(
                opcode,
                scalar,
                &[pointer, scope_id, relaxed, relaxed, new, value],
            )
        } else {
            self.m
                .emit(opcode, scalar, &[pointer, scope_id, relaxed, value])
        };
        if returns {
            let v = self.splat(ty, old);
            self.store(self.operand(i, 0)?, v, ty, false)?;
        }
        Ok(())
    }

    /// `sync`: a barrier for the threads of a group and/or a memory
    /// barrier.
    pub(super) fn sync(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let threads = i.token & (1 << 11) != 0;
        let shared = i.token & (1 << 12) != 0;
        let uav_group = i.token & (1 << 13) != 0;
        let uav_global = i.token & (1 << 14) != 0;
        if (threads || shared || uav_group) && self.stage != Stage::Compute {
            return unsupported("group barriers outside compute shaders");
        }
        let mut semantics = 0;
        if shared {
            semantics |= SEMANTICS_WORKGROUP_MEMORY;
        }
        if uav_group || uav_global {
            semantics |= SEMANTICS_UNIFORM_MEMORY | SEMANTICS_IMAGE_MEMORY;
        }
        if semantics != 0 {
            semantics |= SEMANTICS_ACQUIRE_RELEASE;
        }
        let memory_scope = if uav_global {
            SCOPE_DEVICE
        } else {
            SCOPE_WORKGROUP
        };
        let memory_scope = self.m.constant_u32(memory_scope);
        let semantics = self.m.constant_u32(semantics);
        if threads {
            let workgroup = self.m.constant_u32(SCOPE_WORKGROUP);
            self.m
                .emit_void(op::CONTROL_BARRIER, &[workgroup, memory_scope, semantics]);
        } else {
            self.m
                .emit_void(op::MEMORY_BARRIER, &[memory_scope, semantics]);
        }
        Ok(())
    }

    /// `bufinfo`: elements of a buffer view (raw: bytes), in every
    /// component.
    pub(super) fn bufinfo(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        let resource = self.operand(i, 1)?;
        let u = self.m.scalar(Scalar::Uint);
        let count = if let Ok(w) = self.words_of(resource) {
            let (var, stride) = (w.var, w.stride);
            let words = self.m.emit(op::ARRAY_LENGTH, u, &[var, 0]);
            let four = self.m.constant_u32(4);
            let bytes = self.m.emit(op::I_MUL, u, &[words, four]);
            if stride == 0 {
                bytes
            } else {
                let s = self.m.constant_u32(stride);
                self.m.emit(op::U_DIV, u, &[bytes, s])
            }
        } else {
            let (var, image_ty) = if resource.kind == T_UAV {
                let (var, image_ty, _, _) = self.typed_uav(resource)?;
                (var, image_ty)
            } else {
                let r = self.resource(resource)?;
                (r.var, r.image_ty)
            };
            self.m.capability(CAP_IMAGE_QUERY);
            let image = self.m.emit(op::LOAD, image_ty, &[var]);
            let int = self.m.scalar(Scalar::Int);
            let size = self.m.emit(op::IMAGE_QUERY_SIZE, int, &[image]);
            self.m.emit(op::BITCAST, u, &[size])
        };
        let v = self.splat(Scalar::Uint, count);
        self.store(self.operand(i, 0)?, v, Scalar::Uint, false)
    }
}
