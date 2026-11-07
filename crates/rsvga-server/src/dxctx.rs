//! DirectX contexts: what the DX commands define and bind, kept for the
//! renderer.
//!
//! A context has twelve object tables (COTables). The guest gives each a
//! MOB, whose size bounds the object IDs (`ID < MOB size / entry size`);
//! `DX_GROW_COTABLE` moves a table to a larger MOB and keeps its objects,
//! a plain `DX_SET_COTABLE` with valid contents loads them from the MOB.
//! rsvga keeps the objects itself, typed ([`Object`]), and writes them
//! back only on `DX_READBACK_COTABLE`.
//!
//! The bindings (shaders, views, buffers, states, viewports...) are the
//! pipeline state a draw uses. A shader's code is copied out of its MOB
//! when it is bound (`DX_BIND_SHADER`): later guest writes cannot change
//! a shader in use. It is translated to SPIR-V at once ([`svga_shader`],
//! default options); a shader the translator refuses is logged with the
//! reason.
//!
//! Draws, clears and copies between surfaces are the renderer's: the
//! context model accepts them as [`Outcome::Deferred`] and the device
//! hands them to the GPU ([`crate::gpu`]) when there is one. Fences and
//! queries complete at once.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use svga_core::cotable::{self, Object, QUERY_ACTIVE, QUERY_FINISHED, Query};
use svga_core::dx::{
    DxCommand, ScissorRect, ShaderStage, SoTarget, VertexBuffer, Viewport, WHOLE_SIZE,
};
use svga_core::gb::{GbCommand, ImageId};
use svga_core::layout::Box3;
use svga_proto::{INVALID_ID, dx};

use crate::gb::Gb;
use crate::guest::GuestMemory;

/// Limits on DX contexts.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Context IDs are below this (`SVGA3D_DEVCAP_MAX_CONTEXT_IDS`).
    pub contexts: u32,
    /// Bytes of shader code all contexts together may hold.
    pub code_bytes: usize,
    /// Largest shader, in bytes.
    pub shader_bytes: u32,
}

/// What became of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Done.
    Done,
    /// Needs the renderer: accepted, nothing drawn or copied yet.
    Deferred,
}

/// Number of shader stages.
const STAGES: usize = 6;
/// Number of object tables.
const TABLES: usize = dx::cotable::MAX as usize;

/// A constant buffer binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstantBuffer {
    /// Buffer surface (`SVGA3D_INVALID_ID`: none).
    pub sid: u32,
    /// Byte offset.
    pub offset: u32,
    /// Bytes.
    pub size: u32,
}

const NO_CONSTANT_BUFFER: ConstantBuffer = ConstantBuffer {
    sid: INVALID_ID,
    offset: 0,
    size: 0,
};

/// The bindings of one shader stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageBindings {
    /// Shader (`SVGA3D_INVALID_ID`: none).
    pub shader: u32,
    /// Constant buffers.
    pub constant_buffers: [ConstantBuffer; dx::MAX_CONSTBUFFERS],
    /// Shader resource views.
    pub resources: Box<[u32; dx::MAX_SRVIEWS]>,
    /// Samplers.
    pub samplers: [u32; dx::MAX_SAMPLERS],
}

impl Default for StageBindings {
    fn default() -> Self {
        Self {
            shader: INVALID_ID,
            constant_buffers: [NO_CONSTANT_BUFFER; dx::MAX_CONSTBUFFERS],
            resources: Box::new([INVALID_ID; dx::MAX_SRVIEWS]),
            samplers: [INVALID_ID; dx::MAX_SAMPLERS],
        }
    }
}

/// The pipeline state of a context, as its `SET` commands left it.
#[derive(Clone, Debug, PartialEq)]
pub struct Bindings {
    /// Per stage, in `SVGA3dShaderType` order minus one.
    pub stages: [StageBindings; STAGES],
    /// Input layout.
    pub input_layout: u32,
    /// Vertex buffers.
    pub vertex_buffers: [VertexBuffer; dx::MAX_VERTEXBUFFERS],
    /// Index buffer: surface, format, byte offset, bytes readable from
    /// there (`WHOLE_SIZE`: to the end).
    pub index_buffer: (u32, u32, u32, u32),
    /// `SVGA3dPrimitiveType`.
    pub topology: u32,
    /// Render target views.
    pub render_targets: [u32; dx::MAX_RENDER_TARGETS],
    /// Depth-stencil view.
    pub depth_stencil_view: u32,
    /// Blend state, blend factor, sample mask.
    pub blend: (u32, [f32; 4], u32),
    /// Depth-stencil state and stencil reference.
    pub depth_stencil: (u32, u32),
    /// Rasterizer state.
    pub rasterizer: u32,
    /// Viewports.
    pub viewports: Vec<Viewport>,
    /// Scissor rectangles.
    pub scissors: Vec<ScissorRect>,
    /// Stream-output targets.
    pub so_targets: [SoTarget; dx::MAX_SOTARGETS],
    /// Stream-output object.
    pub stream_output: u32,
    /// Predicate query and the value that lets draws through.
    pub predication: (u32, u32),
}

impl Default for Bindings {
    fn default() -> Self {
        let no_vb = VertexBuffer {
            sid: INVALID_ID,
            stride: 0,
            offset: 0,
            size: WHOLE_SIZE,
        };
        let no_so = SoTarget {
            sid: INVALID_ID,
            offset: 0,
            size: 0,
        };
        Self {
            stages: Default::default(),
            input_layout: INVALID_ID,
            vertex_buffers: [no_vb; dx::MAX_VERTEXBUFFERS],
            index_buffer: (INVALID_ID, 0, 0, WHOLE_SIZE),
            topology: 0,
            render_targets: [INVALID_ID; dx::MAX_RENDER_TARGETS],
            depth_stencil_view: INVALID_ID,
            blend: (INVALID_ID, [1.0; 4], u32::MAX),
            depth_stencil: (INVALID_ID, 0),
            rasterizer: INVALID_ID,
            viewports: Vec::new(),
            scissors: Vec::new(),
            so_targets: [no_so; dx::MAX_SOTARGETS],
            stream_output: INVALID_ID,
            predication: (INVALID_ID, 0),
        }
    }
}

/// One object table.
#[derive(Debug, Default)]
struct Table {
    mob: Option<u32>,
    /// Entries the MOB holds: object IDs are below.
    capacity: u32,
    objects: BTreeMap<u32, Object>,
}

/// A DX context.
#[derive(Debug, Default)]
pub struct Context {
    /// MOB its state is saved to.
    pub mob: Option<u32>,
    tables: [Table; TABLES],
    /// Pipeline state.
    pub bindings: Bindings,
    /// Code of the bound shaders.
    code: HashMap<u32, Arc<[u8]>>,
    /// Their translations (or why there is none).
    spirv: HashMap<u32, Result<Arc<svga_shader::Shader>, String>>,
}

impl Context {
    /// Object `id` of table `table`.
    pub fn object(&self, table: u32, id: u32) -> Option<&Object> {
        self.tables.get(table as usize)?.objects.get(&id)
    }

    /// The code of shader `id`, once bound.
    pub fn shader_code(&self, id: u32) -> Option<&Arc<[u8]>> {
        self.code.get(&id)
    }

    /// The SPIR-V of shader `id`, once bound (an error: why there is none).
    pub fn shader_spirv(&self, id: u32) -> Option<&Result<Arc<svga_shader::Shader>, String>> {
        self.spirv.get(&id)
    }

    fn query_mut(&mut self, id: u32) -> Result<&mut Query, String> {
        match self.tables[dx::cotable::DXQUERY as usize]
            .objects
            .get_mut(&id)
        {
            Some(Object::Query(q)) => Ok(q),
            _ => Err(format!("query {id} is not defined")),
        }
    }
}

/// The DX contexts of the device.
pub struct DxContexts {
    limits: Limits,
    contexts: HashMap<u32, Context>,
    code_bytes: usize,
    /// Shaders translated to SPIR-V, and refused, since the start.
    pub translated: u64,
    /// See `translated`.
    pub refused: u64,
    epoch: Instant,
}

fn stage_index(stage: ShaderStage) -> usize {
    match stage {
        ShaderStage::Vertex => 0,
        ShaderStage::Pixel => 1,
        ShaderStage::Geometry => 2,
        ShaderStage::Hull => 3,
        ShaderStage::Domain => 4,
        ShaderStage::Compute => 5,
    }
}

/// `start..start + len` within `max` slots, else an error.
fn slots(what: &str, start: u32, len: usize, max: usize) -> Result<std::ops::Range<usize>, String> {
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    match start.checked_add(len) {
        Some(end) if end <= max => Ok(start..end),
        _ => Err(format!("{what} {start}+{len} beyond {max} slots")),
    }
}

impl DxContexts {
    /// No contexts.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            contexts: HashMap::new(),
            code_bytes: 0,
            translated: 0,
            refused: 0,
            epoch: Instant::now(),
        }
    }

    /// Forgets every context (device reset).
    pub fn reset(&mut self) {
        self.contexts.clear();
        self.code_bytes = 0;
    }

    /// Context `cid`.
    pub fn context(&self, cid: u32) -> Option<&Context> {
        self.contexts.get(&cid)
    }

    fn context_mut(&mut self, cid: u32) -> Result<&mut Context, String> {
        self.contexts
            .get_mut(&cid)
            .ok_or_else(|| format!("DX context {cid} is not defined"))
    }

    fn drop_code(&mut self, cid: u32, shader: u32) {
        if let Some(context) = self.contexts.get_mut(&cid) {
            context.spirv.remove(&shader);
            if let Some(code) = context.code.remove(&shader) {
                self.code_bytes = self.code_bytes.saturating_sub(code.len());
            }
        }
    }

    /// Copies the code of shader `shader` out of its MOB.
    fn load_code(
        &mut self,
        cid: u32,
        shader: u32,
        gb: &Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        self.drop_code(cid, shader);
        let context = self.context_mut(cid)?;
        let Some(Object::Shader(s)) = context.object(dx::cotable::DXSHADER, shader).cloned() else {
            return Err(format!("shader {shader} is not defined"));
        };
        if s.mob == INVALID_ID {
            return Ok(());
        }
        if s.size > self.limits.shader_bytes {
            return Err(format!("shader {shader} of {} bytes is too large", s.size));
        }
        let size = usize::try_from(s.size).map_err(|e| e.to_string())?;
        if self.code_bytes.saturating_add(size) > self.limits.code_bytes {
            return Err(format!("no room for the {size} bytes of shader {shader}"));
        }
        let mut code = vec![0u8; size];
        gb.read_mob(guest, s.mob, u64::from(s.offset), &mut code)?;
        dump_shader(s.stage, &code);
        self.code_bytes = self.code_bytes.saturating_add(size);
        let words: Vec<u32> = code
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let spirv = svga_shader::translate_words(&words, &svga_shader::Options::default())
            .map(Arc::new)
            .map_err(|e| e.to_string());
        match &spirv {
            Ok(_) => self.translated = self.translated.saturating_add(1),
            Err(e) => {
                self.refused = self.refused.saturating_add(1);
                crate::log!("shader {shader} ({:?}) not translated: {e}", s.stage);
            }
        }
        let context = self.context_mut(cid)?;
        context.code.insert(shader, code.into());
        context.spirv.insert(shader, spirv);
        Ok(())
    }

    /// Writes the result of a finished query to its MOB slot
    /// (`SVGA3dQueryResult`: size, state, result). Until the renderer
    /// measures them, results say "visible, nothing counted": occlusion
    /// queries and predicates report a sample, timestamps come from the
    /// host clock, statistics are zero.
    fn write_query(&self, query: &Query, gb: &Gb, guest: &GuestMemory) -> Result<(), String> {
        if query.state != QUERY_FINISHED || query.mob == INVALID_ID {
            return Ok(());
        }
        let now = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let result: Vec<u8> = match query.kind {
            dx::query::OCCLUSION | dx::query::OCCLUSIONPREDICATE => 1u32.to_le_bytes().to_vec(),
            dx::query::OCCLUSION64 => 1u64.to_le_bytes().to_vec(),
            dx::query::TIMESTAMP => now.to_le_bytes().to_vec(),
            dx::query::TIMESTAMPDISJOINT => {
                let mut r = 1_000_000_000u64.to_le_bytes().to_vec();
                r.extend(0u32.to_le_bytes());
                r
            }
            dx::query::PIPELINESTATS => vec![0; 88],
            dx::query::STREAMOUTPUTSTATS
            | dx::query::SOSTATS_STREAM0
            | dx::query::SOSTATS_STREAM1
            | dx::query::SOSTATS_STREAM2
            | dx::query::SOSTATS_STREAM3 => vec![0; 16],
            _ => vec![0; 4],
        };
        const SUCCEEDED: u32 = 1;
        let base = u64::from(query.offset);
        gb.write_mob(guest, query.mob, base.saturating_add(8), &result)?;
        gb.write_mob(
            guest,
            query.mob,
            base.saturating_add(4),
            &SUCCEEDED.to_le_bytes(),
        )
    }

    /// Executes a DX command; `current` is the DX context of its command
    /// buffer. Errors are the guest's: the command is skipped.
    pub fn execute(
        &mut self,
        command: DxCommand,
        current: Option<u32>,
        gb: &mut Gb,
        guest: &GuestMemory,
    ) -> Result<Outcome, String> {
        use DxCommand as C;
        let cid = || current.ok_or_else(|| "DX command outside a DX context".to_string());
        match command {
            // ---- contexts ---------------------------------------------------
            C::DefineContext { cid } => {
                if cid >= self.limits.contexts {
                    return Err(format!("DX context {cid} beyond {}", self.limits.contexts));
                }
                self.destroy_context(cid);
                self.contexts.insert(cid, Context::default());
            }
            C::DestroyContext { cid } => self.destroy_context(cid),
            C::BindContext {
                cid,
                mob,
                valid_contents,
            } => {
                let context = self.context_mut(cid)?;
                context.mob = (mob != INVALID_ID).then_some(mob);
                if valid_contents && mob != INVALID_ID {
                    return Err(format!(
                        "DX context {cid}: restoring a saved state is not supported"
                    ));
                }
            }
            C::ReadbackContext { cid } => {
                self.context_mut(cid)?;
                return Err(format!(
                    "DX context {cid}: saving its state is not supported"
                ));
            }
            C::InvalidateContext { cid } => self.context_mut(cid)?.bindings = Bindings::default(),

            // ---- object tables ----------------------------------------------
            C::SetCoTable {
                cid,
                mob,
                table,
                valid_size,
                grow,
            } => self.set_cotable(cid, mob, table, valid_size, grow, gb, guest)?,
            C::ReadbackCoTable { cid, table } => {
                let context = self.context_mut(cid)?;
                let t = &context.tables[table as usize];
                let (Some(mob), Some(size)) = (t.mob, cotable::entry_size(table)) else {
                    return Err(format!("DX context {cid}: table {table} has no MOB"));
                };
                for (id, object) in &t.objects {
                    let at = u64::from(*id).saturating_mul(size as u64);
                    gb.write_mob(guest, mob, at, &cotable::encode(*id, object))?;
                }
            }

            // ---- objects ----------------------------------------------------
            command @ (C::DefineRenderTargetView { .. }
            | C::DefineDepthStencilView { .. }
            | C::DefineShaderResourceView { .. }
            | C::DefineElementLayout { .. }
            | C::DefineBlendState { .. }
            | C::DefineDepthStencilState { .. }
            | C::DefineRasterizerState { .. }
            | C::DefineSamplerState { .. }
            | C::DefineStreamOutput { .. }
            | C::DefineQuery { .. }
            | C::DefineShader { .. }) => {
                let current = cid()?;
                let Some((id, object)) = Object::defined_by(&command) else {
                    return Err("not a definition".into());
                };
                let table = object.table();
                if table == dx::cotable::DXSHADER {
                    self.drop_code(current, id);
                }
                let t = &mut self.context_mut(current)?.tables[table as usize];
                if id >= t.capacity {
                    return Err(format!(
                        "object {id} of table {table} beyond its {} entries",
                        t.capacity
                    ));
                }
                t.objects.insert(id, object);
            }
            C::DestroyRenderTargetView { id } => self.destroy(cid()?, dx::cotable::RTVIEW, id)?,
            C::DestroyDepthStencilView { id } => self.destroy(cid()?, dx::cotable::DSVIEW, id)?,
            C::DestroyShaderResourceView { id } => {
                self.destroy(cid()?, dx::cotable::SRVIEW, id)?;
            }
            C::DestroyElementLayout { id } => {
                self.destroy(cid()?, dx::cotable::ELEMENTLAYOUT, id)?;
            }
            C::DestroyBlendState { id } => self.destroy(cid()?, dx::cotable::BLENDSTATE, id)?,
            C::DestroyDepthStencilState { id } => {
                self.destroy(cid()?, dx::cotable::DEPTHSTENCIL, id)?;
            }
            C::DestroyRasterizerState { id } => {
                self.destroy(cid()?, dx::cotable::RASTERIZERSTATE, id)?;
            }
            C::DestroySamplerState { id } => self.destroy(cid()?, dx::cotable::SAMPLER, id)?,
            C::DestroyStreamOutput { id } => self.destroy(cid()?, dx::cotable::STREAMOUTPUT, id)?,
            C::DestroyQuery { id } => self.destroy(cid()?, dx::cotable::DXQUERY, id)?,
            C::DestroyShader { id } => {
                let current = cid()?;
                self.drop_code(current, id);
                self.destroy(current, dx::cotable::DXSHADER, id)?;
            }

            // ---- shader code ------------------------------------------------
            C::BindShader {
                cid,
                shader,
                mob,
                offset,
            } => {
                match self.context_mut(cid)?.tables[dx::cotable::DXSHADER as usize]
                    .objects
                    .get_mut(&shader)
                {
                    Some(Object::Shader(s)) => {
                        s.mob = mob;
                        s.offset = offset;
                    }
                    _ => return Err(format!("shader {shader} is not defined")),
                }
                self.load_code(cid, shader, gb, guest)?;
            }
            C::BindAllShader { cid, mob } => self.rebind_shaders(cid, None, mob, gb, guest)?,
            C::CondBindAllShader { cid, test_mob, mob } => {
                self.rebind_shaders(cid, Some(test_mob), mob, gb, guest)?;
            }

            // ---- bindings ---------------------------------------------------
            C::SetShader { stage, shader } => {
                self.bindings(cid()?)?.stages[stage_index(stage)].shader = shader;
            }
            C::SetShaderResources {
                stage,
                start,
                views,
            } => {
                let b = self.bindings(cid()?)?;
                let range = slots("shader resources", start, views.len(), dx::MAX_SRVIEWS)?;
                b.stages[stage_index(stage)].resources[range].copy_from_slice(&views);
            }
            C::SetSamplers {
                stage,
                start,
                samplers,
            } => {
                let b = self.bindings(cid()?)?;
                let range = slots("samplers", start, samplers.len(), dx::MAX_SAMPLERS)?;
                b.stages[stage_index(stage)].samplers[range].copy_from_slice(&samplers);
            }
            C::SetSingleConstantBuffer {
                stage,
                slot,
                sid,
                offset,
                size,
            } => {
                let b = self.bindings(cid()?)?;
                let range = slots("constant buffer", slot, 1, dx::MAX_CONSTBUFFERS)?;
                b.stages[stage_index(stage)].constant_buffers[range.start] =
                    ConstantBuffer { sid, offset, size };
            }
            C::SetConstantBufferOffset {
                stage,
                slot,
                offset,
            } => {
                let b = self.bindings(cid()?)?;
                let range = slots("constant buffer", slot, 1, dx::MAX_CONSTBUFFERS)?;
                b.stages[stage_index(stage)].constant_buffers[range.start].offset = offset;
            }
            C::SetInputLayout { layout } => self.bindings(cid()?)?.input_layout = layout,
            C::SetVertexBuffers { start, buffers } => {
                let b = self.bindings(cid()?)?;
                let range = slots(
                    "vertex buffers",
                    start,
                    buffers.len(),
                    dx::MAX_VERTEXBUFFERS,
                )?;
                b.vertex_buffers[range].copy_from_slice(&buffers);
            }
            C::SetVertexBuffersOffsetAndSize { start, ranges } => {
                let b = self.bindings(cid()?)?;
                let range = slots("vertex buffers", start, ranges.len(), dx::MAX_VERTEXBUFFERS)?;
                for (vb, r) in b.vertex_buffers[range].iter_mut().zip(ranges) {
                    vb.stride = r.stride;
                    vb.offset = r.offset;
                    vb.size = r.size;
                }
            }
            C::SetIndexBuffer {
                sid,
                format,
                offset,
                size,
            } => self.bindings(cid()?)?.index_buffer = (sid, format, offset, size),
            C::SetIndexBufferOffsetAndSize {
                format,
                offset,
                size,
            } => {
                let b = self.bindings(cid()?)?;
                b.index_buffer = (b.index_buffer.0, format, offset, size);
            }
            C::SetTopology { topology } => self.bindings(cid()?)?.topology = topology,
            C::SetRenderTargets {
                depth_stencil,
                targets,
            } => {
                let b = self.bindings(cid()?)?;
                b.depth_stencil_view = depth_stencil;
                b.render_targets = [INVALID_ID; dx::MAX_RENDER_TARGETS];
                b.render_targets[..targets.len()].copy_from_slice(&targets);
            }
            C::SetBlendState {
                id,
                factor,
                sample_mask,
            } => self.bindings(cid()?)?.blend = (id, factor, sample_mask),
            C::SetDepthStencilState { id, stencil_ref } => {
                self.bindings(cid()?)?.depth_stencil = (id, stencil_ref);
            }
            C::SetRasterizerState { id } => self.bindings(cid()?)?.rasterizer = id,
            C::SetViewports { viewports } => self.bindings(cid()?)?.viewports = viewports,
            C::SetScissorRects { rects } => self.bindings(cid()?)?.scissors = rects,
            C::SetSoTargets { targets } => {
                let b = self.bindings(cid()?)?;
                b.so_targets = [SoTarget {
                    sid: INVALID_ID,
                    offset: 0,
                    size: 0,
                }; dx::MAX_SOTARGETS];
                b.so_targets[..targets.len()].copy_from_slice(&targets);
            }
            C::SetStreamOutput { id } => self.bindings(cid()?)?.stream_output = id,
            C::SetPredication { query, value } => {
                self.bindings(cid()?)?.predication = (query, value);
            }

            // ---- queries ----------------------------------------------------
            C::BindQuery { id, mob } => self.context_mut(cid()?)?.query_mut(id)?.mob = mob,
            C::SetQueryOffset { id, offset } => {
                self.context_mut(cid()?)?.query_mut(id)?.offset = offset;
            }
            C::BeginQuery { id } => self.context_mut(cid()?)?.query_mut(id)?.state = QUERY_ACTIVE,
            C::EndQuery { id } => {
                let query = self.context_mut(cid()?)?.query_mut(id)?;
                query.state = QUERY_FINISHED;
                let query = *query;
                self.write_query(&query, gb, guest)?;
            }
            C::ReadbackQuery { id } => {
                let query = *self.context_mut(cid()?)?.query_mut(id)?;
                self.write_query(&query, gb, guest)?;
            }
            C::MoveQuery { id, mob, offset } => {
                let query = self.context_mut(cid()?)?.query_mut(id)?;
                query.mob = mob;
                query.offset = offset;
                let query = *query;
                self.write_query(&query, gb, guest)?;
            }
            C::BindAllQuery { cid, mob } => {
                for object in self.context_mut(cid)?.tables[dx::cotable::DXQUERY as usize]
                    .objects
                    .values_mut()
                {
                    if let Object::Query(q) = object {
                        q.mob = mob;
                    }
                }
            }
            C::ReadbackAllQuery { cid } => {
                let queries: Vec<Query> = self.context_mut(cid)?.tables
                    [dx::cotable::DXQUERY as usize]
                    .objects
                    .values()
                    .filter_map(|o| match o {
                        Object::Query(q) => Some(*q),
                        _ => None,
                    })
                    .collect();
                for query in &queries {
                    self.write_query(query, gb, guest)?;
                }
            }

            // ---- transfers between a surface and its MOB --------------------
            C::BufferUpdate { sid, x, width } => {
                let area = Box3 {
                    x,
                    y: 0,
                    z: 0,
                    w: width,
                    h: 1,
                    d: 1,
                };
                gb.execute(
                    GbCommand::UpdateImage {
                        image: image(sid, 0, 0),
                        area,
                    },
                    guest,
                )?;
            }
            C::UpdateSubResource { sid, sub, area } => {
                let image = gb.subresource(sid, sub)?;
                gb.execute(GbCommand::UpdateImage { image, area }, guest)?;
            }
            C::ReadbackSubResource { sid, sub } => {
                let image = gb.subresource(sid, sub)?;
                gb.execute(GbCommand::ReadbackImage { image }, guest)?;
            }
            C::InvalidateSubResource { .. } | C::Hint { .. } => {}
            C::MobFence64 { value, mob, offset } => {
                gb.write_mob(guest, mob, u64::from(offset), &value.to_le_bytes())?;
            }

            // ---- rendering: with the renderer ---------------------------------
            C::Draw { .. }
            | C::DrawIndexed { .. }
            | C::DrawInstanced { .. }
            | C::DrawIndexedInstanced { .. }
            | C::DrawAuto
            | C::ClearRenderTargetView { .. }
            | C::ClearDepthStencilView { .. }
            | C::PredCopyRegion { .. }
            | C::PredCopy { .. }
            | C::ResolveCopy { .. }
            | C::PredConvertRegion { .. }
            | C::PredConvert { .. }
            | C::GenMips { .. }
            | C::BufferCopy { .. }
            | C::TransferFromBuffer { .. }
            | C::SurfaceCopyAndReadback { .. } => {
                cid()?;
                return Ok(Outcome::Deferred);
            }
        }
        Ok(Outcome::Done)
    }

    fn bindings(&mut self, cid: u32) -> Result<&mut Bindings, String> {
        Ok(&mut self.context_mut(cid)?.bindings)
    }

    fn destroy_context(&mut self, cid: u32) {
        if let Some(context) = self.contexts.remove(&cid) {
            let freed: usize = context.code.values().map(|c| c.len()).sum();
            self.code_bytes = self.code_bytes.saturating_sub(freed);
        }
    }

    fn destroy(&mut self, cid: u32, table: u32, id: u32) -> Result<(), String> {
        self.context_mut(cid)?.tables[table as usize]
            .objects
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| format!("object {id} of table {table} is not defined"))
    }

    #[allow(clippy::too_many_arguments)]
    fn set_cotable(
        &mut self,
        cid: u32,
        mob: u32,
        table: u32,
        valid_size: u32,
        grow: bool,
        gb: &Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let entry = cotable::entry_size(table).ok_or_else(|| format!("no table {table}"))? as u32;
        let size = if mob == INVALID_ID {
            0
        } else {
            gb.mob_size(mob)
                .ok_or_else(|| format!("MOB {mob} is not defined"))?
        };
        if valid_size > size {
            return Err(format!(
                "table {table}: {valid_size} valid bytes in a {size}-byte MOB"
            ));
        }
        let valid = valid_size / entry;
        let t = &mut self.context_mut(cid)?.tables[table as usize];
        t.mob = (mob != INVALID_ID).then_some(mob);
        t.capacity = size / entry;
        if grow {
            // The device moves the valid part: its objects stay.
            t.objects.retain(|id, _| *id < valid);
        } else {
            t.objects.clear();
        }
        if !grow && valid > 0 {
            self.load_table(cid, mob, table, valid, gb, guest)?;
        }
        if table == dx::cotable::DXSHADER {
            let shaders: Vec<u32> = self.context_mut(cid)?.tables[table as usize]
                .objects
                .keys()
                .copied()
                .collect();
            let stale: Vec<u32> = self
                .context_mut(cid)?
                .code
                .keys()
                .copied()
                .filter(|id| !shaders.contains(id))
                .collect();
            for id in stale {
                self.drop_code(cid, id);
            }
        }
        Ok(())
    }

    /// Loads the first `valid` entries of a table from its MOB. Entries that
    /// do not decode are left out (unused IDs hold whatever the guest left).
    fn load_table(
        &mut self,
        cid: u32,
        mob: u32,
        table: u32,
        valid: u32,
        gb: &Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let entry = cotable::entry_size(table).unwrap_or(0);
        let mut bytes = vec![0u8; entry];
        let mut loaded = Vec::new();
        for id in 0..valid {
            gb.read_mob(
                guest,
                mob,
                u64::from(id).saturating_mul(entry as u64),
                &mut bytes,
            )?;
            if let Ok(object) = cotable::decode(table, &bytes) {
                loaded.push((id, object));
            }
        }
        let t = &mut self.context_mut(cid)?.tables[table as usize];
        t.objects.extend(loaded);
        if table == dx::cotable::DXSHADER {
            for id in 0..valid {
                // A shader whose code cannot be read stays defined, codeless.
                let _ = self.load_code(cid, id, gb, guest);
            }
        }
        Ok(())
    }

    fn rebind_shaders(
        &mut self,
        cid: u32,
        test_mob: Option<u32>,
        mob: u32,
        gb: &Gb,
        guest: &GuestMemory,
    ) -> Result<(), String> {
        let mut moved = Vec::new();
        for (id, object) in
            &mut self.context_mut(cid)?.tables[dx::cotable::DXSHADER as usize].objects
        {
            if let Object::Shader(s) = object
                && test_mob.is_none_or(|test| s.mob == test)
            {
                s.mob = mob;
                moved.push(*id);
            }
        }
        for id in moved {
            self.load_code(cid, id, gb, guest)?;
        }
        Ok(())
    }
}

/// Diagnostics: with `RSVGA_SHADER_DUMP=DIR`, each distinct shader bound is
/// saved as `DIR/<stage>-<hash>.vgpu10` (code and signatures, as the guest
/// gave them), to study and test the translator.
fn dump_shader(stage: ShaderStage, code: &[u8]) {
    let Some(dir) = std::env::var_os("RSVGA_SHADER_DUMP") else {
        return;
    };
    // FNV-1a: a stable name, so identical shaders share a file.
    let hash = code.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let path = std::path::Path::new(&dir).join(format!("{stage:?}-{hash:016x}.vgpu10"));
    if !path.exists()
        && let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, code))
    {
        crate::log!("shader dump {}: {e}", path.display());
    }
}

fn image(sid: u32, face: u32, mipmap: u32) -> ImageId {
    ImageId { sid, face, mipmap }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::gb::Limits as GbLimits;
    use crate::shm::SharedMem;
    use std::os::fd::OwnedFd;
    use svga_proto::mobfmt;

    const RAM_BASE: u64 = 0x10_0000;
    const PAGE: u64 = 4096;

    /// 64 KiB of guest RAM with MOB `n` (n = 1..) on page `n`, one page each.
    fn setup(mobs: u32) -> (SharedMem, GuestMemory, Gb, DxContexts) {
        let ram = SharedMem::new("ram", 0x10000).unwrap();
        let fd: OwnedFd = ram.fd().try_clone_to_owned().unwrap();
        let mut guest = GuestMemory::default();
        guest.add(RAM_BASE, 0x10000, Some(fd), 0, true).unwrap();
        let mut gb = Gb::new(GbLimits {
            mob_pages: 64,
            mob_size: 1 << 20,
            surface_bytes: 1 << 20,
            max_width: 1024,
            max_height: 1024,
        });
        for id in 1..=mobs {
            gb.execute(
                GbCommand::DefineMob {
                    id,
                    pt_depth: mobfmt::RANGE,
                    base: RAM_BASE / PAGE + u64::from(id),
                    size: 4096,
                },
                &guest,
            )
            .unwrap();
        }
        let dx = DxContexts::new(Limits {
            contexts: 8,
            code_bytes: 4096,
            shader_bytes: 2048,
        });
        (ram, guest, gb, dx)
    }

    fn mob_offset(mob: u32) -> usize {
        (u64::from(mob) * PAGE) as usize
    }

    struct Run<'a> {
        dx: &'a mut DxContexts,
        gb: &'a mut Gb,
        guest: &'a GuestMemory,
    }

    impl Run<'_> {
        fn ok(&mut self, current: Option<u32>, c: DxCommand) -> Outcome {
            self.dx.execute(c, current, self.gb, self.guest).unwrap()
        }
        fn err(&mut self, current: Option<u32>, c: DxCommand) -> String {
            self.dx
                .execute(c, current, self.gb, self.guest)
                .unwrap_err()
        }
    }

    fn set_table(cid: u32, mob: u32, table: u32, valid_size: u32, grow: bool) -> DxCommand {
        DxCommand::SetCoTable {
            cid,
            mob,
            table,
            valid_size,
            grow,
        }
    }

    #[test]
    fn contexts_tables_and_objects() {
        let (_ram, guest, mut gb, mut dx) = setup(4);
        let mut r = Run {
            dx: &mut dx,
            gb: &mut gb,
            guest: &guest,
        };
        r.ok(None, DxCommand::DefineContext { cid: 1 });
        assert!(
            r.err(None, DxCommand::DefineContext { cid: 8 })
                .contains("beyond")
        );
        // Element layouts: 1024-byte entries, 4 in a page.
        r.ok(None, set_table(1, 1, dx::cotable::ELEMENTLAYOUT, 0, false));
        let layout = |id| DxCommand::DefineElementLayout {
            id,
            elements: vec![],
        };
        r.ok(Some(1), layout(3));
        assert!(r.err(Some(1), layout(4)).contains("beyond its 4 entries"));
        assert!(r.err(None, layout(0)).contains("outside a DX context"));
        // Growing to a larger MOB keeps the objects of the valid part.
        r.gb.execute(
            GbCommand::DefineMob {
                id: 9,
                pt_depth: mobfmt::RANGE,
                base: RAM_BASE / PAGE + 6,
                size: 8192,
            },
            r.guest,
        )
        .unwrap();
        r.ok(
            None,
            set_table(1, 9, dx::cotable::ELEMENTLAYOUT, 4096, true),
        );
        r.ok(Some(1), layout(7));
        let context = r.dx.context(1).unwrap();
        assert!(context.object(dx::cotable::ELEMENTLAYOUT, 3).is_some());
        assert!(context.object(dx::cotable::ELEMENTLAYOUT, 7).is_some());
        // Destroy, then a destroyed object is gone.
        r.ok(Some(1), DxCommand::DestroyElementLayout { id: 3 });
        assert!(
            r.err(Some(1), DxCommand::DestroyElementLayout { id: 3 })
                .contains("not defined")
        );
    }

    #[test]
    fn tables_are_saved_to_and_loaded_from_their_mob() {
        let (ram, guest, mut gb, mut dx) = setup(4);
        let mut r = Run {
            dx: &mut dx,
            gb: &mut gb,
            guest: &guest,
        };
        r.ok(None, DxCommand::DefineContext { cid: 0 });
        r.ok(
            None,
            set_table(0, 2, dx::cotable::RASTERIZERSTATE, 0, false),
        );
        let state = svga_core::dx::RasterizerState {
            fill_mode: 3,
            cull_mode: 2,
            front_ccw: false,
            provoking_vertex_last: false,
            depth_bias: 0,
            depth_bias_clamp: 0.0,
            slope_scaled_depth_bias: 0.0,
            depth_clip: true,
            scissor: true,
            multisample: 0,
            antialiased_line: false,
            line_width: 1.0,
            line_stipple: false,
            line_stipple_factor: 0,
            line_stipple_pattern: 0,
            forced_sample_count: 0,
        };
        r.ok(Some(0), DxCommand::DefineRasterizerState { id: 5, state });
        r.ok(
            None,
            DxCommand::ReadbackCoTable {
                cid: 0,
                table: dx::cotable::RASTERIZERSTATE,
            },
        );
        let mut entry = [0u8; 32];
        assert!(ram.read(mob_offset(2) + 5 * 32, &mut entry));
        assert_eq!(entry[0], 3, "fill mode of entry 5");
        // A new context loads the table from the same MOB.
        r.ok(None, DxCommand::DefineContext { cid: 2 });
        r.ok(
            None,
            set_table(2, 2, dx::cotable::RASTERIZERSTATE, 6 * 32, false),
        );
        assert_eq!(
            r.dx.context(2)
                .unwrap()
                .object(dx::cotable::RASTERIZERSTATE, 5),
            Some(&Object::RasterizerState(state))
        );
    }

    #[test]
    fn shader_code_is_copied_when_bound() {
        let (ram, guest, mut gb, mut dx) = setup(4);
        let mut r = Run {
            dx: &mut dx,
            gb: &mut gb,
            guest: &guest,
        };
        r.ok(None, DxCommand::DefineContext { cid: 0 });
        r.ok(None, set_table(0, 1, dx::cotable::DXSHADER, 0, false));
        r.ok(
            Some(0),
            DxCommand::DefineShader {
                id: 2,
                stage: ShaderStage::Pixel,
                size: 8,
            },
        );
        assert!(ram.write(mob_offset(3) + 16, &[1, 2, 3, 4, 5, 6, 7, 8]));
        let bind = |offset| DxCommand::BindShader {
            cid: 0,
            shader: 2,
            mob: 3,
            offset,
        };
        r.ok(None, bind(16));
        assert!(ram.write(mob_offset(3) + 16, &[0; 8]), "later writes");
        assert_eq!(
            r.dx.context(0).unwrap().shader_code(2).map(|c| c.to_vec()),
            Some(vec![1, 2, 3, 4, 5, 6, 7, 8])
        );
        // Not a shader: kept, but not translated.
        assert!(matches!(
            r.dx.context(0).unwrap().shader_spirv(2),
            Some(Err(_))
        ));
        assert_eq!((r.dx.translated, r.dx.refused), (0, 1));
        // Code outside its MOB, or beyond the budget, is refused.
        assert!(r.err(None, bind(4092)).contains("beyond"));
        r.ok(
            Some(0),
            DxCommand::DefineShader {
                id: 3,
                stage: ShaderStage::Vertex,
                size: 4000,
            },
        );
        assert!(
            r.err(
                None,
                DxCommand::BindShader {
                    cid: 0,
                    shader: 3,
                    mob: 3,
                    offset: 0
                }
            )
            .contains("too large")
        );
        // A real (empty) vertex shader: version, length, ret.
        r.ok(
            Some(0),
            DxCommand::DefineShader {
                id: 4,
                stage: ShaderStage::Vertex,
                size: 12,
            },
        );
        let words: Vec<u8> = [0x0001_0040u32, 3, 0x0100_003e]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        assert!(ram.write(mob_offset(3) + 64, &words));
        r.ok(
            None,
            DxCommand::BindShader {
                cid: 0,
                shader: 4,
                mob: 3,
                offset: 64,
            },
        );
        let spirv =
            r.dx.context(0)
                .unwrap()
                .shader_spirv(4)
                .cloned()
                .unwrap()
                .unwrap();
        assert_eq!(spirv.spirv[0], 0x0723_0203);
        assert_eq!(r.dx.translated, 1);
        r.ok(Some(0), DxCommand::DestroyShader { id: 4 });
        r.ok(Some(0), DxCommand::DestroyShader { id: 2 });
        assert!(r.dx.context(0).unwrap().shader_code(2).is_none());
        assert_eq!(r.dx.code_bytes, 0);
    }

    #[test]
    fn bindings_fences_and_queries() {
        let (ram, guest, mut gb, mut dx) = setup(4);
        let mut r = Run {
            dx: &mut dx,
            gb: &mut gb,
            guest: &guest,
        };
        r.ok(None, DxCommand::DefineContext { cid: 0 });
        // Vertex and index buffers, then new ranges: the surfaces stay.
        r.ok(
            Some(0),
            DxCommand::SetVertexBuffers {
                start: 3,
                buffers: vec![VertexBuffer {
                    sid: 40,
                    stride: 16,
                    offset: 0,
                    size: WHOLE_SIZE,
                }],
            },
        );
        r.ok(
            Some(0),
            DxCommand::SetVertexBuffersOffsetAndSize {
                start: 3,
                ranges: vec![svga_core::dx::VertexBufferRange {
                    stride: 20,
                    offset: 64,
                    size: 200,
                }],
            },
        );
        r.ok(
            Some(0),
            DxCommand::SetIndexBuffer {
                sid: 41,
                format: 89,
                offset: 0,
                size: WHOLE_SIZE,
            },
        );
        r.ok(
            Some(0),
            DxCommand::SetIndexBufferOffsetAndSize {
                format: 77,
                offset: 8,
                size: 400,
            },
        );
        {
            let b = &r.dx.context(0).unwrap().bindings;
            assert_eq!(
                b.vertex_buffers[3],
                VertexBuffer {
                    sid: 40,
                    stride: 20,
                    offset: 64,
                    size: 200
                }
            );
            assert_eq!(b.index_buffer, (41, 77, 8, 400));
        }
        r.ok(
            Some(0),
            DxCommand::SetShaderResources {
                stage: ShaderStage::Pixel,
                start: 126,
                views: vec![7, 8],
            },
        );
        assert!(
            r.err(
                Some(0),
                DxCommand::SetShaderResources {
                    stage: ShaderStage::Pixel,
                    start: 127,
                    views: vec![7, 8],
                }
            )
            .contains("beyond")
        );
        r.ok(
            Some(0),
            DxCommand::SetRenderTargets {
                depth_stencil: 4,
                targets: vec![1, 2],
            },
        );
        let b = &r.dx.context(0).unwrap().bindings;
        assert_eq!(b.stages[1].resources[126..], [7, 8]);
        assert_eq!(b.render_targets[..3], [1, 2, INVALID_ID]);
        assert_eq!(b.depth_stencil_view, 4);
        // Draws wait for the renderer.
        assert_eq!(
            r.ok(
                Some(0),
                DxCommand::Draw {
                    vertex_count: 3,
                    start_vertex: 0
                }
            ),
            Outcome::Deferred
        );
        // A 64-bit fence lands in its MOB.
        r.ok(
            None,
            DxCommand::MobFence64 {
                value: 0x1122_3344_5566_7788,
                mob: 4,
                offset: 8,
            },
        );
        let mut fence = [0u8; 8];
        assert!(ram.read(mob_offset(4) + 8, &mut fence));
        assert_eq!(u64::from_le_bytes(fence), 0x1122_3344_5566_7788);
        // A query ends and reports success in its slot.
        r.ok(None, set_table(0, 1, dx::cotable::DXQUERY, 0, false));
        r.ok(
            Some(0),
            DxCommand::DefineQuery {
                id: 1,
                kind: dx::query::OCCLUSION,
                flags: 0,
            },
        );
        r.ok(None, DxCommand::BindAllQuery { cid: 0, mob: 3 });
        r.ok(Some(0), DxCommand::SetQueryOffset { id: 1, offset: 32 });
        r.ok(Some(0), DxCommand::BeginQuery { id: 1 });
        r.ok(Some(0), DxCommand::EndQuery { id: 1 });
        let mut slot = [0u8; 12];
        assert!(ram.read(mob_offset(3) + 32, &mut slot));
        assert_eq!(
            &slot[4..12],
            &[1, 0, 0, 0, 1, 0, 0, 0],
            "succeeded, 1 sample"
        );
    }
}
