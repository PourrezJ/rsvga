//! Geometry shaders: per-vertex input arrays (`v[vertex][register]`),
//! `emit` and `cut`, the primitive and vertex count declarations, the
//! instance ID, and the layer and viewport outputs.
//!
//! Outputs are written to the interface at each `emit` (Direct3D's output
//! registers hold the vertex being built), not at `ret`.

use super::*;

pub(super) const MODEL_GEOMETRY: u32 = 3;
const MODE_INVOCATIONS: u32 = 0;
const MODE_OUTPUT_VERTICES: u32 = 26;
const BUILTIN_INVOCATION_ID: u32 = 8;
pub(super) const BUILTIN_LAYER: u32 = 9;
pub(super) const BUILTIN_VIEWPORT_INDEX: u32 = 10;
pub(super) const T_GS_INSTANCE_ID: u32 = 37;

/// What a geometry shader declares about its primitives.
#[derive(Default)]
pub(super) struct Geometry {
    /// `VGPU10_PRIMITIVE_*` of the input.
    input: u32,
    /// `VGPU10_PRIMITIVE_TOPOLOGY_*` of the output.
    output: u32,
    /// Most vertices emitted per invocation.
    max_vertices: u32,
    /// Invocations per primitive (`dcl_gs_instance_count`).
    instances: u32,
}

impl Tx<'_> {
    /// The geometry shader declarations (`true` if `i` was one).
    pub(super) fn declare_geometry(&mut self, i: &Instruction) -> Result<bool, TranslateError> {
        match i.opcode {
            o::DCL_GS_INPUT_PRIMITIVE => self.geometry.input = (i.token >> 11) & 0x3f,
            o::DCL_GS_OUTPUT_PRIMITIVE_TOPOLOGY => self.geometry.output = (i.token >> 11) & 0x7f,
            o::DCL_MAX_OUTPUT_VERTEX_COUNT => {
                let n = i.raw.first().copied().unwrap_or(0);
                if n == 0 || n > 1024 {
                    return unsupported(format_args!("{n} output vertices"));
                }
                self.geometry.max_vertices = n;
            }
            o::DCL_GS_INSTANCE_COUNT => {
                let n = i.raw.first().copied().unwrap_or(1);
                if n == 0 || n > 32 {
                    return unsupported(format_args!("{n} geometry shader instances"));
                }
                self.geometry.instances = n;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// An input of every vertex of the primitive: `register` of `vertices`
    /// vertices (`name`: its system value, 1 for the position).
    pub(super) fn declare_vertex_input(
        &mut self,
        vertices: u32,
        register: u32,
        name: u32,
    ) -> Result<(), TranslateError> {
        if self.vertex_inputs.contains_key(&register) {
            return Ok(());
        }
        if vertices == 0 || vertices > 32 {
            return unsupported(format_args!("{vertices} input vertices"));
        }
        let array = self.m.array(self.f4, vertices);
        let decoration = if name == 1 {
            [DECORATION_BUILTIN, BUILTIN_POSITION]
        } else {
            self.input_locations.push(register);
            [DECORATION_LOCATION, register]
        };
        let var = self.input_var(array, &[&decoration]);
        let p = self.ptr(self.reg_class, array);
        let local = self.reg_var(p);
        self.vertex_inputs.insert(register, local);
        self.input_sources
            .push((local, InputSource::Array(var, array)));
        Ok(())
    }

    /// `vGSInstanceID`.
    pub(super) fn declare_instance_id(&mut self) {
        if self.system.contains_key(&T_GS_INSTANCE_ID) {
            return;
        }
        let int = self.m.scalar(Scalar::Int);
        let var = self.input_var(int, &[&[DECORATION_BUILTIN, BUILTIN_INVOCATION_ID]]);
        let local = self.register_var();
        self.system.insert(T_GS_INSTANCE_ID, local);
        self.input_sources
            .push((local, InputSource::IntBuiltin(var)));
    }

    /// A pointer to `v[vertex][register]`.
    pub(super) fn vertex_input(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        let (Some(vertex), Some(Index::Imm(register))) =
            (operand.indices.first(), operand.indices.get(1))
        else {
            return Err(TranslateError(format!(
                "per-vertex input {:?}",
                operand.indices
            )));
        };
        let local = *self
            .vertex_inputs
            .get(register)
            .ok_or_else(|| TranslateError(format!("input v[][{register}] is not declared")))?;
        let vertex = self.index(vertex)?;
        let p = self.ptr(self.reg_class, self.f4);
        Ok(self.m.emit(op::ACCESS_CHAIN, p, &[local, vertex]))
    }

    /// `emit`, `cut`, `emitthencut` and their `_stream` forms (stream 0).
    pub(super) fn emit_or_cut(&mut self, i: &Instruction) -> Result<(), TranslateError> {
        if self.stage != Stage::Geometry {
            return unsupported("emit and cut outside geometry shaders");
        }
        if matches!(
            i.opcode,
            o::EMIT_STREAM | o::CUT_STREAM | o::EMITTHENCUT_STREAM
        ) && self.operand(i, 0)?.index0() != Some(0)
        {
            return unsupported("geometry shader streams other than 0");
        }
        if matches!(
            i.opcode,
            o::EMIT | o::EMITTHENCUT | o::EMIT_STREAM | o::EMITTHENCUT_STREAM
        ) {
            self.write_outputs();
            self.m.emit_void(op::EMIT_VERTEX, &[]);
        }
        if matches!(
            i.opcode,
            o::CUT | o::EMITTHENCUT | o::CUT_STREAM | o::EMITTHENCUT_STREAM
        ) {
            self.m.emit_void(op::END_PRIMITIVE, &[]);
        }
        Ok(())
    }

    /// The execution modes of a geometry shader.
    pub(super) fn geometry_modes(&self) -> Result<Vec<Vec<u32>>, TranslateError> {
        let g = &self.geometry;
        let input = match g.input {
            1 => 19, // InputPoints
            2 => 20, // InputLines
            3 => 22, // Triangles
            6 => 21, // InputLinesAdjacency
            7 => 23, // InputTrianglesAdjacency
            other => return unsupported(format_args!("geometry shader input primitive {other}")),
        };
        let output = match g.output {
            1 => 27,     // OutputPoints
            2 | 3 => 28, // OutputLineStrip
            4 | 5 => 29, // OutputTriangleStrip
            other => return unsupported(format_args!("geometry shader output topology {other}")),
        };
        Ok(vec![
            vec![input],
            vec![output],
            vec![MODE_OUTPUT_VERTICES, g.max_vertices.max(1)],
            vec![MODE_INVOCATIONS, g.instances.max(1)],
        ])
    }
}
