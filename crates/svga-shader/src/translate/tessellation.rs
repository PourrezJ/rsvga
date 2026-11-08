//! Hull and domain shaders, as Vulkan tessellation control and evaluation
//! shaders.
//!
//! A hull shader is phases: the control point phase, run once per output
//! control point (`vOutputControlPointID`), then fork and join phases, run
//! once per patch and per instance of their own, writing the patch
//! constants (the tessellation factors among them). Each phase becomes a
//! function of the control shader (a phase's `ret` ends the phase); the
//! entry point calls the control point phase (or copies the control points
//! through when the shader has none), waits for every invocation, then the
//! first invocation runs the patch phases, instance by instance, and
//! writes the tessellation levels and the other patch constants. Patch
//! constants are kept in a register array until then: phases index it
//! (`o[r0.x + 4]`) and join phases read what fork phases wrote (`vpc#`).
//! Registers are private to the invocation rather than function-local
//! (`Tx::reg_class`), since the phases share them.
//!
//! A domain shader reads the domain point (`vDomain`), the control points
//! (`vicp[n][r]`) and the patch constants (`vpc#`): tessellation factors
//! from the tessellation level builtins, the others from patch inputs.
//!
//! Positions travel as the `Position` builtin wherever the signatures name
//! them (VMware's driver appends the input, output and patch constant
//! signatures to each shader), as vertex shaders write them.

use super::*;

pub(super) const MODEL_TESS_CONTROL: u32 = 1;
pub(super) const MODEL_TESS_EVALUATION: u32 = 2;
const CAP_TESSELLATION: u32 = 3;
const DECORATION_PATCH: u32 = 15;
const BUILTIN_INVOCATION_ID: u32 = 8;
const BUILTIN_TESS_LEVEL_OUTER: u32 = 11;
const BUILTIN_TESS_LEVEL_INNER: u32 = 12;
const BUILTIN_TESS_COORD: u32 = 13;
const SCOPE_WORKGROUP: u32 = 2;
const SCOPE_INVOCATION: u32 = 4;
pub(super) const T_OUTPUT_CONTROL_POINT_ID: u32 = 22;
pub(super) const T_INPUT_FORK_INSTANCE_ID: u32 = 23;
pub(super) const T_INPUT_JOIN_INSTANCE_ID: u32 = 24;
pub(super) const T_INPUT_CONTROL_POINT: u32 = 25;
pub(super) const T_OUTPUT_CONTROL_POINT: u32 = 26;
pub(super) const T_INPUT_PATCH_CONSTANT: u32 = 27;
pub(super) const T_INPUT_DOMAIN_POINT: u32 = 28;
/// Registers of the patch constant array.
const PATCH_REGISTERS: u32 = 32;

/// Which part of a hull shader is being translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum Phase {
    /// Declarations before the first phase (or a domain shader).
    #[default]
    None,
    /// The control point phase.
    ControlPoint,
    /// A fork or join phase.
    Patch,
}

/// Tessellation state of a hull or domain shader.
#[derive(Default)]
pub(super) struct Tess {
    /// `VGPU10_TESSELLATOR_DOMAIN_*`.
    domain: u32,
    /// `VGPU10_TESSELLATOR_PARTITIONING_*`.
    partitioning: u32,
    /// `VGPU10_TESSELLATOR_OUTPUT_PRIMITIVE_*`.
    output_primitive: u32,
    /// Control points in (domain shader: from the hull shader).
    input_points: u32,
    /// Control points out (hull shader).
    output_points: u32,
    /// The phase being translated.
    pub(super) phase: Phase,
    /// Per-control-point outputs (hull shader): arrays by register.
    outputs: HashMap<u32, u32>,
    /// The patch constant registers (`vec4[32]`).
    patch: Option<u32>,
    /// System values of patch constant registers, and their components.
    patch_names: HashMap<u32, (u32, u32)>,
    /// The fork or join instance of the phase being run.
    instance: Option<u32>,
    /// The `InvocationId` input (hull shader).
    invocation: Option<u32>,
}

/// Where a Direct3D tessellation factor goes: outer (`false`) or inner
/// level, and its index there.
fn level(name: u32) -> Option<(bool, u32)> {
    Some(match name {
        // Quad edges u = 0, v = 0, u = 1, v = 1; insides u, v.
        11..=14 => (false, name.saturating_sub(11)),
        15 | 16 => (true, name.saturating_sub(15)),
        // Triangle edges u = 0, v = 0, w = 0; inside.
        17..=19 => (false, name.saturating_sub(17)),
        20 => (true, 0),
        // Isolines: detail (segments per line), density (lines).
        21 => (false, 1),
        22 => (false, 0),
        _ => return None,
    })
}

/// The first component of a mask.
fn first_component(mask: u32) -> u32 {
    (0..4).find(|c| mask & (1 << c) != 0).unwrap_or(0)
}

impl Tx<'_> {
    /// The tessellation declarations (`true` if `i` was one).
    pub(super) fn declare_tessellation(&mut self, i: &Instruction) -> Result<bool, TranslateError> {
        let field = |bits: u32| (i.token >> 11) & bits;
        match i.opcode {
            o::DCL_TESS_DOMAIN => self.tess.domain = field(0x3),
            o::DCL_TESS_PARTITIONING => self.tess.partitioning = field(0x7),
            o::DCL_TESS_OUTPUT_PRIMITIVE => self.tess.output_primitive = field(0x7),
            o::DCL_INPUT_CONTROL_POINT_COUNT => self.tess.input_points = field(0x3f),
            o::DCL_OUTPUT_CONTROL_POINT_COUNT => self.tess.output_points = field(0x3f),
            o::DCL_HS_MAX_TESSFACTOR
            | o::DCL_HS_FORK_PHASE_INSTANCE_COUNT
            | o::DCL_HS_JOIN_PHASE_INSTANCE_COUNT
            | o::HS_DECLS => {}
            o::HS_CONTROL_POINT_PHASE => self.tess.phase = Phase::ControlPoint,
            o::HS_FORK_PHASE | o::HS_JOIN_PHASE => self.tess.phase = Phase::Patch,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The system value of input register `r`, from the input signature.
    fn input_name(&self, r: u32) -> u32 {
        self.signatures
            .inputs
            .iter()
            .find(|e| e.register == r)
            .map_or(0, |e| e.name)
    }

    /// The tessellation inputs of `dcl_input*` (`true` if it was one).
    pub(super) fn declare_tessellation_input(
        &mut self,
        operand: &Operand,
    ) -> Result<bool, TranslateError> {
        match operand.kind {
            T_INPUT_CONTROL_POINT => {
                let [Index::Imm(vertices), Index::Imm(r)] = operand.indices.as_slice() else {
                    return unsupported("control point input");
                };
                let name = if self.input_name(*r) == 1 { 1 } else { 0 };
                self.declare_vertex_input(*vertices, *r, name)?;
            }
            T_INPUT_PATCH_CONSTANT | T_OUTPUT_CONTROL_POINT => {}
            T_INPUT_FORK_INSTANCE_ID | T_INPUT_JOIN_INSTANCE_ID => {
                if self.tess.instance.is_none() {
                    let var = self.register_var();
                    self.tess.instance = Some(var);
                }
            }
            T_OUTPUT_CONTROL_POINT_ID => {
                if !self.system.contains_key(&T_OUTPUT_CONTROL_POINT_ID) {
                    let var = self.invocation_id();
                    let local = self.register_var();
                    self.system.insert(T_OUTPUT_CONTROL_POINT_ID, local);
                    self.input_sources
                        .push((local, InputSource::IntBuiltin(var)));
                }
            }
            T_INPUT_DOMAIN_POINT => {
                if !self.system.contains_key(&T_INPUT_DOMAIN_POINT) {
                    let f3 = self.m.vector(Scalar::Float, 3);
                    let var = self.input_var(f3, &[&[DECORATION_BUILTIN, BUILTIN_TESS_COORD]]);
                    let local = self.register_var();
                    self.system.insert(T_INPUT_DOMAIN_POINT, local);
                    self.input_sources.push((local, InputSource::Float3(var)));
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The `InvocationId` builtin (hull shaders).
    fn invocation_id(&mut self) -> u32 {
        if let Some(v) = self.tess.invocation {
            return v;
        }
        let int = self.m.scalar(Scalar::Int);
        let v = self.input_var(int, &[&[DECORATION_BUILTIN, BUILTIN_INVOCATION_ID]]);
        self.tess.invocation = Some(v);
        v
    }

    /// The patch constant registers (`vec4[32]`).
    fn patch_registers(&mut self) -> u32 {
        if let Some(p) = self.tess.patch {
            return p;
        }
        let array = self.m.array(self.f4, PATCH_REGISTERS);
        let p = self.ptr(self.reg_class, array);
        let var = self.reg_var(p);
        self.tess.patch = Some(var);
        var
    }

    /// A hull shader output declaration: a per-control-point output array
    /// in the control point phase, a patch constant register in the
    /// others.
    pub(super) fn declare_hull_output(
        &mut self,
        r: u32,
        name: u32,
        mask: u32,
    ) -> Result<(), TranslateError> {
        if self.tess.phase == Phase::Patch {
            if r >= PATCH_REGISTERS {
                return unsupported(format_args!("patch constant o{r}"));
            }
            self.patch_registers();
            if name != 0 || !self.tess.patch_names.contains_key(&r) {
                self.tess.patch_names.insert(r, (name, mask));
            }
            return Ok(());
        }
        self.declare_point_output(r, name == 1)
    }

    /// The per-control-point output array of register `r`.
    fn declare_point_output(&mut self, r: u32, position: bool) -> Result<(), TranslateError> {
        if self.tess.outputs.contains_key(&r) {
            return Ok(());
        }
        let points = self.tess.output_points;
        if points == 0 || points > 32 {
            return unsupported(format_args!("{points} output control points"));
        }
        let array = self.m.array(self.f4, points);
        let var = if position {
            self.output_var(array, &[&[DECORATION_BUILTIN, BUILTIN_POSITION]])
        } else {
            self.output_locations.push(r);
            self.output_var(array, &[&[DECORATION_LOCATION, r]])
        };
        self.tess.outputs.insert(r, var);
        Ok(())
    }

    /// Pointers to the tessellation registers: hull shader outputs, the
    /// control points written, patch constants, the instance ID.
    pub(super) fn tessellation_register(
        &mut self,
        operand: &Operand,
    ) -> Option<Result<u32, TranslateError>> {
        Some(match operand.kind {
            T_OUTPUT if self.stage == Stage::Hull => self.hull_output(operand),
            T_OUTPUT_CONTROL_POINT => self.point_output(operand),
            T_INPUT_PATCH_CONSTANT => self.patch_register(operand.indices.first()),
            T_INPUT_CONTROL_POINT => self.vertex_input(operand),
            T_INPUT_FORK_INSTANCE_ID | T_INPUT_JOIN_INSTANCE_ID => self
                .tess
                .instance
                .ok_or_else(|| TranslateError("instance ID outside a fork or join phase".into())),
            T_OUTPUT_CONTROL_POINT_ID | T_INPUT_DOMAIN_POINT => {
                self.system.get(&operand.kind).copied().ok_or_else(|| {
                    TranslateError(format!("input {} is not declared", operand.kind))
                })
            }
            _ => return None,
        })
    }

    /// `o#` of a hull shader: this invocation's control point, or a patch
    /// constant register.
    fn hull_output(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        if self.tess.phase == Phase::Patch {
            return self.patch_register(operand.indices.first());
        }
        let r = operand
            .index0()
            .ok_or_else(|| TranslateError("indexed control point output".into()))?;
        let var = *self
            .tess
            .outputs
            .get(&r)
            .ok_or_else(|| TranslateError(format!("output o{r} is not declared")))?;
        let int = self.m.scalar(Scalar::Int);
        let invocation = self.invocation_id();
        let id = self.m.emit(op::LOAD, int, &[invocation]);
        let p = self.ptr(STORAGE_OUTPUT, self.f4);
        Ok(self.m.emit(op::ACCESS_CHAIN, p, &[var, id]))
    }

    /// `vocp[n][r]`: control point `n` as the control point phase wrote it.
    fn point_output(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        let (Some(vertex), Some(Index::Imm(r))) = (operand.indices.first(), operand.indices.get(1))
        else {
            return unsupported("output control point operand");
        };
        let var = *self.tess.outputs.get(r).ok_or_else(|| {
            TranslateError(format!("output control point register {r} is not declared"))
        })?;
        let vertex = self.index(vertex)?;
        let p = self.ptr(STORAGE_OUTPUT, self.f4);
        Ok(self.m.emit(op::ACCESS_CHAIN, p, &[var, vertex]))
    }

    /// A patch constant register, possibly indexed.
    fn patch_register(&mut self, index: Option<&Index>) -> Result<u32, TranslateError> {
        let index = index.ok_or_else(|| TranslateError("patch constant without index".into()))?;
        let array = self.patch_registers();
        let i = self.index(index)?;
        // Out-of-range indices read and write the last register.
        let u = self.m.scalar(Scalar::Uint);
        let last = self.m.constant_u32(PATCH_REGISTERS.saturating_sub(1));
        let i = self.m.glsl(u, glsl::U_MIN, &[i, last]);
        let p = self.ptr(self.reg_class, self.f4);
        Ok(self.m.emit(op::ACCESS_CHAIN, p, &[array, i]))
    }

    /// The hull shader's code: its phases as functions, called by the
    /// entry point (whose first block holds the input copies already).
    pub(super) fn hull_body(&mut self, program: &Program) -> Result<(), TranslateError> {
        // Phases, with their instance counts.
        let mut phases: Vec<(Phase, u32, Vec<&Instruction>)> = Vec::new();
        for i in &program.instructions {
            match i.opcode {
                o::HS_CONTROL_POINT_PHASE => phases.push((Phase::ControlPoint, 1, Vec::new())),
                o::HS_FORK_PHASE | o::HS_JOIN_PHASE => phases.push((Phase::Patch, 1, Vec::new())),
                o::DCL_HS_FORK_PHASE_INSTANCE_COUNT | o::DCL_HS_JOIN_PHASE_INSTANCE_COUNT => {
                    if let Some(last) = phases.last_mut() {
                        last.1 = i.raw.first().copied().unwrap_or(1);
                    }
                }
                _ => {
                    if let Some(last) = phases.last_mut() {
                        last.2.push(i);
                    }
                }
            }
        }
        if phases.iter().any(|p| p.1 == 0 || p.1 > 32) {
            return unsupported("phase instance count");
        }
        // Each phase as a function.
        let mut functions = Vec::new();
        for (phase, count, body) in &phases {
            self.tess.phase = *phase;
            let f = self.m.start_function();
            for i in body {
                self.instruction(i)?;
            }
            if !self.flow.is_empty() {
                return Err(TranslateError("unterminated if or loop in a phase".into()));
            }
            self.m.emit_void(op::RETURN, &[]);
            self.m.end_function();
            functions.push((*phase, *count, f));
        }
        self.tess.phase = Phase::None;
        // The control points: their phase, or copied through.
        match functions.iter().find(|f| f.0 == Phase::ControlPoint) {
            Some(&(_, _, f)) => self.m.call(f),
            None => self.copy_control_points()?,
        }
        // Every control point is written before the patch phases read them.
        let workgroup = self.m.constant_u32(SCOPE_WORKGROUP);
        let invocation = self.m.constant_u32(SCOPE_INVOCATION);
        let none = self.m.constant_u32(0);
        self.m
            .emit_void(op::CONTROL_BARRIER, &[workgroup, invocation, none]);
        // The first invocation runs the patch phases.
        let int = self.m.scalar(Scalar::Int);
        let b = self.m.scalar(Scalar::Bool);
        let id_var = self.invocation_id();
        let id = self.m.emit(op::LOAD, int, &[id_var]);
        let zero = self.m.constant_i32(0);
        let first = self.m.emit(op::I_EQUAL, b, &[id, zero]);
        let (then, merge) = (self.m.id(), self.m.id());
        self.m.emit_void(op::SELECTION_MERGE, &[merge, 0]);
        self.m
            .emit_void(op::BRANCH_CONDITIONAL, &[first, then, merge]);
        self.m.emit_void(op::LABEL, &[then]);
        for &(phase, count, f) in &functions {
            if phase != Phase::Patch {
                continue;
            }
            for n in 0..count {
                if let Some(instance) = self.tess.instance {
                    let v = self.m.constant_vector(Scalar::Uint, &[n; 4]);
                    let v = self.m.emit(op::BITCAST, self.f4, &[v]);
                    self.m.emit_void(op::STORE, &[instance, v]);
                }
                self.m.call(f);
            }
        }
        self.write_patch_constants()?;
        self.m.emit_void(op::BRANCH, &[merge]);
        self.m.emit_void(op::LABEL, &[merge]);
        Ok(())
    }

    /// Without a control point phase, the output control points are the
    /// input ones: every register of the output signature (or else of the
    /// input one) is copied.
    fn copy_control_points(&mut self) -> Result<(), TranslateError> {
        let entries = if self.signatures.outputs.is_empty() {
            self.signatures.inputs.clone()
        } else {
            self.signatures.outputs.clone()
        };
        if self.tess.output_points == 0 {
            self.tess.output_points = self.tess.input_points;
        }
        let int = self.m.scalar(Scalar::Int);
        for e in entries {
            if e.register >= 32 {
                continue;
            }
            let position = e.name == 1;
            if !self.vertex_inputs.contains_key(&e.register) {
                let points = self.tess.input_points.max(1);
                self.declare_vertex_input(points, e.register, u32::from(position))?;
                // Copied in here: the entry's input copies are done.
                self.load_inputs();
            }
            self.declare_point_output(e.register, position)?;
            let (Some(&input), Some(&output)) = (
                self.vertex_inputs.get(&e.register),
                self.tess.outputs.get(&e.register),
            ) else {
                continue;
            };
            let invocation = self.invocation_id();
            let id = self.m.emit(op::LOAD, int, &[invocation]);
            let pi = self.ptr(self.reg_class, self.f4);
            let src = self.m.emit(op::ACCESS_CHAIN, pi, &[input, id]);
            let v = self.m.emit(op::LOAD, self.f4, &[src]);
            let po = self.ptr(STORAGE_OUTPUT, self.f4);
            let dst = self.m.emit(op::ACCESS_CHAIN, po, &[output, id]);
            self.m.emit_void(op::STORE, &[dst, v]);
        }
        Ok(())
    }

    /// The tessellation levels and the other patch constants, from the
    /// patch constant registers.
    fn write_patch_constants(&mut self) -> Result<(), TranslateError> {
        let Some(registers) = self.tess.patch else {
            return Ok(());
        };
        let f = self.m.scalar(Scalar::Float);
        let outer_ty = self.m.array(f, 4);
        let inner_ty = self.m.array(f, 2);
        let mut outer = None;
        let mut inner = None;
        let mut names: Vec<(u32, (u32, u32))> = self
            .tess
            .patch_names
            .iter()
            .map(|(r, n)| (*r, *n))
            .collect();
        names.sort_unstable();
        for (r, (name, mask)) in names {
            let pr = self.ptr(self.reg_class, self.f4);
            let index = self.m.constant_u32(r);
            let p = self.m.emit(op::ACCESS_CHAIN, pr, &[registers, index]);
            let v = self.m.emit(op::LOAD, self.f4, &[p]);
            if let Some((is_inner, slot)) = level(name) {
                let var = if is_inner {
                    *inner.get_or_insert_with(|| {
                        self.output_var(
                            inner_ty,
                            &[
                                &[DECORATION_BUILTIN, BUILTIN_TESS_LEVEL_INNER],
                                &[DECORATION_PATCH],
                            ],
                        )
                    })
                } else {
                    *outer.get_or_insert_with(|| {
                        self.output_var(
                            outer_ty,
                            &[
                                &[DECORATION_BUILTIN, BUILTIN_TESS_LEVEL_OUTER],
                                &[DECORATION_PATCH],
                            ],
                        )
                    })
                };
                let x = self
                    .m
                    .emit(op::COMPOSITE_EXTRACT, f, &[v, first_component(mask)]);
                let pf = self.ptr(STORAGE_OUTPUT, f);
                let slot = self.m.constant_u32(slot);
                let dst = self.m.emit(op::ACCESS_CHAIN, pf, &[var, slot]);
                self.m.emit_void(op::STORE, &[dst, x]);
            } else if name == 0 {
                let var =
                    self.output_var(self.f4, &[&[DECORATION_LOCATION, r], &[DECORATION_PATCH]]);
                self.m.emit_void(op::STORE, &[var, v]);
            }
        }
        Ok(())
    }

    /// A domain shader's patch constants: the tessellation levels and the
    /// patch inputs of its patch constant signature, copied into the
    /// patch constant registers at the start.
    pub(super) fn load_patch_constants(&mut self) -> Result<(), TranslateError> {
        let entries = self.signatures.patch.clone();
        if entries.is_empty() {
            return Ok(());
        }
        let registers = self.patch_registers();
        let f = self.m.scalar(Scalar::Float);
        let outer_ty = self.m.array(f, 4);
        let inner_ty = self.m.array(f, 2);
        let mut outer = None;
        let mut inner = None;
        for e in entries {
            if e.register >= PATCH_REGISTERS {
                continue;
            }
            let index = self.m.constant_u32(e.register);
            let pr = self.ptr(self.reg_class, self.f4);
            let p = self.m.emit(op::ACCESS_CHAIN, pr, &[registers, index]);
            if let Some((is_inner, slot)) = level(e.name) {
                let var = if is_inner {
                    *inner.get_or_insert_with(|| {
                        self.input_var(
                            inner_ty,
                            &[
                                &[DECORATION_BUILTIN, BUILTIN_TESS_LEVEL_INNER],
                                &[DECORATION_PATCH],
                            ],
                        )
                    })
                } else {
                    *outer.get_or_insert_with(|| {
                        self.input_var(
                            outer_ty,
                            &[
                                &[DECORATION_BUILTIN, BUILTIN_TESS_LEVEL_OUTER],
                                &[DECORATION_PATCH],
                            ],
                        )
                    })
                };
                let pf = self.ptr(STORAGE_INPUT, f);
                let slot = self.m.constant_u32(slot);
                let src = self.m.emit(op::ACCESS_CHAIN, pf, &[var, slot]);
                let x = self.m.emit(op::LOAD, f, &[src]);
                let old = self.m.emit(op::LOAD, self.f4, &[p]);
                let c = first_component(e.mask);
                let v = self.m.emit(op::COMPOSITE_INSERT, self.f4, &[x, old, c]);
                self.m.emit_void(op::STORE, &[p, v]);
            } else if e.name == 0 {
                let var = self.input_var(
                    self.f4,
                    &[&[DECORATION_LOCATION, e.register], &[DECORATION_PATCH]],
                );
                let v = self.m.emit(op::LOAD, self.f4, &[var]);
                self.m.emit_void(op::STORE, &[p, v]);
            }
        }
        Ok(())
    }

    /// The execution modes of a hull (`true`) or domain shader.
    pub(super) fn tessellation_modes(
        &mut self,
        hull: bool,
    ) -> Result<Vec<Vec<u32>>, TranslateError> {
        self.m.capability(CAP_TESSELLATION);
        let t = &self.tess;
        let domain = match t.domain {
            1 => 25, // Isolines
            2 => 22, // Triangles
            3 => 24, // Quads
            other => return unsupported(format_args!("tessellator domain {other}")),
        };
        let mut modes = vec![vec![domain]];
        if hull {
            modes.push(vec![26, t.output_points.max(1)]); // OutputVertices
            modes.push(vec![match t.partitioning {
                3 => 3, // SpacingFractionalOdd
                4 => 2, // SpacingFractionalEven
                _ => 1, // SpacingEqual (integer, pow2)
            }]);
            match t.output_primitive {
                1 => modes.push(vec![10]), // PointMode
                3 => modes.push(vec![4]),  // VertexOrderCw
                4 => modes.push(vec![5]),  // VertexOrderCcw
                _ => {}
            }
        }
        Ok(modes)
    }
}
