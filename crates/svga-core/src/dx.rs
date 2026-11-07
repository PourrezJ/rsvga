//! DirectX context commands (`SVGA_3D_CMD_DX_*`, `svga3d_dx.h`).
//!
//! The VMware DirectX 10/11 driver records its rendering as DX commands in
//! command buffers tagged with a DX context. [`parse`] turns one command
//! (an ID and its payload) into a [`DxCommand`]: every field typed, every
//! array bounded by the Direct3D limits, closed enumerations checked. It
//! does not look at object IDs or surfaces: the device model does.
//!
//! The payload is read byte by byte from a copy (as every command is),
//! little-endian, with the packed C layouts of the VMware headers.
//! `DX_PRESENTBLT`, which needs no context, is decoded with the
//! guest-backed object commands ([`crate::gb`]).

use svga_proto::{cmd3d, dx};

use crate::gb::CopyBox;
use crate::layout::Box3;

/// A shader stage (`SVGA3dShaderType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    /// Vertex shader.
    Vertex,
    /// Pixel shader.
    Pixel,
    /// Geometry shader.
    Geometry,
    /// Hull shader (shader model 5).
    Hull,
    /// Domain shader (shader model 5).
    Domain,
    /// Compute shader (shader model 5).
    Compute,
}

impl ShaderStage {
    /// The stage of an `SVGA3dShaderType` value.
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            dx::shader::VS => Self::Vertex,
            dx::shader::PS => Self::Pixel,
            dx::shader::GS => Self::Geometry,
            dx::shader::HS => Self::Hull,
            dx::shader::DS => Self::Domain,
            dx::shader::CS => Self::Compute,
            _ => return None,
        })
    }
}

/// A vertex buffer binding (`SVGA3dVertexBuffer`, `SVGA3dVertexBuffer_v2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexBuffer {
    /// Buffer surface, or `SVGA3D_INVALID_ID` to unbind.
    pub sid: u32,
    /// Bytes between vertices.
    pub stride: u32,
    /// Byte offset of the first vertex.
    pub offset: u32,
    /// Bytes readable from `offset` ([`WHOLE_SIZE`]: to the end).
    pub size: u32,
}

/// The size of bindings made by the commands that give none: the rest of
/// the buffer.
pub const WHOLE_SIZE: u32 = u32::MAX;

/// Stride, offset and size of a vertex buffer slot
/// (`SVGA3dVertexBufferOffsetAndSize`): the surface stays bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexBufferRange {
    /// Bytes between vertices.
    pub stride: u32,
    /// Byte offset of the first vertex.
    pub offset: u32,
    /// Bytes readable from `offset`.
    pub size: u32,
}

/// A stream-output target (`SVGA3dSoTarget`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoTarget {
    /// Buffer surface, or `SVGA3D_INVALID_ID`.
    pub sid: u32,
    /// Byte offset of the first write.
    pub offset: u32,
    /// Bytes available.
    pub size: u32,
}

/// A viewport (`SVGA3dViewport`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
    /// Depth of the near plane.
    pub min_depth: f32,
    /// Depth of the far plane.
    pub max_depth: f32,
}

/// A scissor rectangle (`SVGASignedRect`), right and bottom excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScissorRect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge.
    pub right: i32,
    /// Bottom edge.
    pub bottom: i32,
}

/// One vertex attribute of an input layout (`SVGA3dInputElementDesc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputElement {
    /// Vertex buffer slot.
    pub slot: u32,
    /// Byte offset in the vertex.
    pub offset: u32,
    /// `SVGA3dSurfaceFormat` of the attribute.
    pub format: u32,
    /// Advances per instance rather than per vertex.
    pub per_instance: bool,
    /// Instances per step (per-instance data).
    pub step_rate: u32,
    /// Vertex shader input register.
    pub register: u32,
}

/// Blending of one render target (`SVGA3dDXBlendStatePerRT`). Factors and
/// operations keep their Direct3D 10 values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlendTarget {
    /// Blending on.
    pub enable: bool,
    /// Source color factor (`D3D10_BLEND`).
    pub src: u8,
    /// Destination color factor.
    pub dest: u8,
    /// Color operation (`D3D10_BLEND_OP`).
    pub op: u8,
    /// Source alpha factor.
    pub src_alpha: u8,
    /// Destination alpha factor.
    pub dest_alpha: u8,
    /// Alpha operation.
    pub op_alpha: u8,
    /// Channels written (`SVGA3D_COLOR_WRITE_ENABLE_*`).
    pub write_mask: u8,
    /// Logic operation instead of blending (Direct3D 11.1).
    pub logic_op_enable: bool,
    /// Logic operation (`D3D11_LOGIC_OP`).
    pub logic_op: u8,
}

/// A blend state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendState {
    /// Alpha to coverage.
    pub alpha_to_coverage: bool,
    /// Each target has its own blending (else target 0 applies to all).
    pub independent: bool,
    /// Per render target.
    pub targets: [BlendTarget; dx::MAX_RENDER_TARGETS],
}

/// Stencil operations of one face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StencilFace {
    /// On stencil failure (`D3D10_STENCIL_OP`).
    pub fail_op: u8,
    /// On depth failure.
    pub depth_fail_op: u8,
    /// On success.
    pub pass_op: u8,
    /// Comparison (`D3D10_COMPARISON_FUNC`).
    pub func: u8,
}

/// A depth-stencil state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthStencilState {
    /// Depth test on.
    pub depth_enable: bool,
    /// Depth writes (`D3D10_DEPTH_WRITE_MASK`).
    pub depth_write_mask: u8,
    /// Depth comparison.
    pub depth_func: u8,
    /// Stencil test on.
    pub stencil_enable: bool,
    /// Stencil for front faces (VMware extension, normally on).
    pub front_enable: bool,
    /// Stencil for back faces (VMware extension, normally on).
    pub back_enable: bool,
    /// Bits read.
    pub stencil_read_mask: u8,
    /// Bits written.
    pub stencil_write_mask: u8,
    /// Front faces.
    pub front: StencilFace,
    /// Back faces.
    pub back: StencilFace,
}

/// A rasterizer state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterizerState {
    /// `D3D10_FILL_MODE`.
    pub fill_mode: u8,
    /// `D3D10_CULL_MODE`.
    pub cull_mode: u8,
    /// Counter-clockwise triangles face the viewer.
    pub front_ccw: bool,
    /// Flat attributes come from the last vertex (VMware extension).
    pub provoking_vertex_last: bool,
    /// Constant depth bias.
    pub depth_bias: i32,
    /// Largest depth bias.
    pub depth_bias_clamp: f32,
    /// Depth bias per unit of slope.
    pub slope_scaled_depth_bias: f32,
    /// Clip against the near and far planes.
    pub depth_clip: bool,
    /// Scissor test on.
    pub scissor: bool,
    /// Multisample rasterization (`SVGA3dMultisampleRastEnable`).
    pub multisample: u8,
    /// Antialiased lines.
    pub antialiased_line: bool,
    /// Line width (VMware extension).
    pub line_width: f32,
    /// Line stipple on (VMware extension).
    pub line_stipple: bool,
    /// Line stipple repeat.
    pub line_stipple_factor: u8,
    /// Line stipple pattern.
    pub line_stipple_pattern: u16,
    /// Forced sample count (Direct3D 11.1, `_V2` command only).
    pub forced_sample_count: u32,
}

/// A sampler state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerState {
    /// `SVGA3D_FILTER_*` bits.
    pub filter: u32,
    /// Addressing on U, V and W (`D3D10_TEXTURE_ADDRESS_MODE`).
    pub address: [u8; 3],
    /// Bias added to the mip level.
    pub mip_lod_bias: f32,
    /// Largest anisotropy.
    pub max_anisotropy: u8,
    /// Comparison of comparison samplers.
    pub comparison: u8,
    /// Border color.
    pub border_color: [f32; 4],
    /// Smallest mip level used.
    pub min_lod: f32,
    /// Largest mip level used.
    pub max_lod: f32,
}

/// What a shader resource view covers (`SVGA3dShaderResourceViewDesc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrvRange {
    /// Elements of a buffer.
    Buffer {
        /// First element.
        first: u32,
        /// Elements.
        count: u32,
    },
    /// Elements of a buffer, possibly raw (`SVGA3D_RESOURCE_BUFFEREX`).
    BufferEx {
        /// First element.
        first: u32,
        /// Elements.
        count: u32,
        /// `SVGA3D_BUFFEREX_SRV_*`.
        flags: u32,
    },
    /// Mips and slices of a texture.
    Texture {
        /// First mip level.
        most_detailed_mip: u32,
        /// Mip levels (`u32::MAX`: all).
        mip_levels: u32,
        /// First array slice (cube: first face).
        first_slice: u32,
        /// Slices.
        slices: u32,
    },
}

/// What a render target view covers (`SVGA3dRenderTargetViewDesc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtvRange {
    /// Elements of a buffer.
    Buffer {
        /// First element.
        first: u32,
        /// Elements.
        count: u32,
    },
    /// One mip level of array slices.
    Texture {
        /// Mip level.
        mip: u32,
        /// First slice.
        first_slice: u32,
        /// Slices.
        slices: u32,
    },
    /// One mip level of depth slices of a 3D texture.
    Texture3d {
        /// Mip level.
        mip: u32,
        /// First depth slice.
        first_w: u32,
        /// Depth slices.
        w_size: u32,
    },
}

/// The surface a view looks at, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewTarget {
    /// Surface.
    pub sid: u32,
    /// Format the view reads or writes (`SVGA3dSurfaceFormat`).
    pub format: u32,
    /// `SVGA3dResourceType`.
    pub dimension: u32,
}

/// A depth-stencil view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthStencilView {
    /// Surface, format and dimension.
    pub target: ViewTarget,
    /// Mip level.
    pub mip: u32,
    /// First array slice.
    pub first_slice: u32,
    /// Slices.
    pub slices: u32,
    /// `SVGA3D_DXDSVIEW_CREATE_READ_ONLY_*`.
    pub flags: u8,
}

/// One stream-output declaration (`SVGA3dStreamOutputDeclarationEntry`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOutputDecl {
    /// Output slot.
    pub slot: u32,
    /// Shader output register.
    pub register: u32,
    /// Components written.
    pub mask: u8,
    /// Stream (shader model 5).
    pub stream: u32,
}

/// A decoded DX command. Context IDs (`cid`) appear only where the command
/// names a context; the others apply to the context of their command
/// buffer.
#[derive(Clone, Debug, PartialEq)]
pub enum DxCommand {
    /// Creates a context.
    DefineContext {
        /// Context.
        cid: u32,
    },
    /// Destroys a context.
    DestroyContext {
        /// Context.
        cid: u32,
    },
    /// Gives a context the MOB its state is saved to; `valid_contents`:
    /// load the state from it.
    BindContext {
        /// Context.
        cid: u32,
        /// MOB (`SVGA3D_INVALID_ID`: unbind).
        mob: u32,
        /// The MOB holds a saved state.
        valid_contents: bool,
    },
    /// Saves a context's state to its MOB.
    ReadbackContext {
        /// Context.
        cid: u32,
    },
    /// Forgets a context's state.
    InvalidateContext {
        /// Context.
        cid: u32,
    },
    /// Binds a range of a buffer as constant buffer `slot`.
    SetSingleConstantBuffer {
        /// Stage.
        stage: ShaderStage,
        /// Slot.
        slot: u32,
        /// Buffer surface (`SVGA3D_INVALID_ID`: unbind).
        sid: u32,
        /// Byte offset.
        offset: u32,
        /// Bytes.
        size: u32,
    },
    /// Moves the constant buffer `slot` of a stage (`SET_*S_CONSTANT_BUFFER_OFFSET`).
    SetConstantBufferOffset {
        /// Stage.
        stage: ShaderStage,
        /// Slot.
        slot: u32,
        /// Byte offset.
        offset: u32,
    },
    /// Binds shader resource views from slot `start`.
    SetShaderResources {
        /// Stage.
        stage: ShaderStage,
        /// First slot.
        start: u32,
        /// Views (`SVGA3D_INVALID_ID`: unbind).
        views: Vec<u32>,
    },
    /// Binds a shader.
    SetShader {
        /// Stage.
        stage: ShaderStage,
        /// Shader (`SVGA3D_INVALID_ID`: none).
        shader: u32,
    },
    /// Binds samplers from slot `start`.
    SetSamplers {
        /// Stage.
        stage: ShaderStage,
        /// First slot.
        start: u32,
        /// Samplers.
        samplers: Vec<u32>,
    },
    /// Draws vertices.
    Draw {
        /// Vertices.
        vertex_count: u32,
        /// First vertex.
        start_vertex: u32,
    },
    /// Draws indexed vertices.
    DrawIndexed {
        /// Indices.
        index_count: u32,
        /// First index.
        start_index: u32,
        /// Added to each index.
        base_vertex: i32,
    },
    /// Draws instances.
    DrawInstanced {
        /// Vertices per instance.
        vertex_count: u32,
        /// Instances.
        instance_count: u32,
        /// First vertex.
        start_vertex: u32,
        /// First instance.
        start_instance: u32,
    },
    /// Draws indexed instances.
    DrawIndexedInstanced {
        /// Indices per instance.
        index_count: u32,
        /// Instances.
        instance_count: u32,
        /// First index.
        start_index: u32,
        /// Added to each index.
        base_vertex: i32,
        /// First instance.
        start_instance: u32,
    },
    /// Draws what stream output wrote.
    DrawAuto,
    /// Binds an input layout.
    SetInputLayout {
        /// Element layout (`SVGA3D_INVALID_ID`: none).
        layout: u32,
    },
    /// Binds vertex buffers from slot `start` (`_V2`: with sizes).
    SetVertexBuffers {
        /// First slot.
        start: u32,
        /// Bindings.
        buffers: Vec<VertexBuffer>,
    },
    /// Changes stride, offset and size of the vertex buffers bound from
    /// slot `start`.
    SetVertexBuffersOffsetAndSize {
        /// First slot.
        start: u32,
        /// New ranges.
        ranges: Vec<VertexBufferRange>,
    },
    /// Binds the index buffer (`_V2`: with a size).
    SetIndexBuffer {
        /// Buffer surface (`SVGA3D_INVALID_ID`: none).
        sid: u32,
        /// `R16_UINT` or `R32_UINT`.
        format: u32,
        /// Byte offset.
        offset: u32,
        /// Bytes readable from `offset` ([`WHOLE_SIZE`]: to the end).
        size: u32,
    },
    /// Changes format, offset and size of the bound index buffer.
    SetIndexBufferOffsetAndSize {
        /// `R16_UINT` or `R32_UINT`.
        format: u32,
        /// Byte offset.
        offset: u32,
        /// Bytes readable from `offset`.
        size: u32,
    },
    /// Sets the primitive topology (`SVGA3dPrimitiveType`).
    SetTopology {
        /// Topology.
        topology: u32,
    },
    /// Binds the depth-stencil view and render target views.
    SetRenderTargets {
        /// Depth-stencil view (`SVGA3D_INVALID_ID`: none).
        depth_stencil: u32,
        /// Render target views, from slot 0.
        targets: Vec<u32>,
    },
    /// Binds a blend state.
    SetBlendState {
        /// Blend state (`SVGA3D_INVALID_ID`: default).
        id: u32,
        /// Constant blend factor.
        factor: [f32; 4],
        /// Samples written.
        sample_mask: u32,
    },
    /// Binds a depth-stencil state.
    SetDepthStencilState {
        /// State (`SVGA3D_INVALID_ID`: default).
        id: u32,
        /// Stencil reference.
        stencil_ref: u32,
    },
    /// Binds a rasterizer state.
    SetRasterizerState {
        /// State (`SVGA3D_INVALID_ID`: default).
        id: u32,
    },
    /// Creates a query (`SVGA3dQueryType`, `SVGA3D_DXQUERY_FLAG_*`).
    DefineQuery {
        /// Query.
        id: u32,
        /// Type.
        kind: u32,
        /// Flags.
        flags: u32,
    },
    /// Destroys a query.
    DestroyQuery {
        /// Query.
        id: u32,
    },
    /// Gives a query the MOB its result goes to.
    BindQuery {
        /// Query.
        id: u32,
        /// MOB.
        mob: u32,
    },
    /// Sets where in its MOB a query's result goes.
    SetQueryOffset {
        /// Query.
        id: u32,
        /// Byte offset.
        offset: u32,
    },
    /// Starts a query.
    BeginQuery {
        /// Query.
        id: u32,
    },
    /// Ends a query.
    EndQuery {
        /// Query.
        id: u32,
    },
    /// Writes a query's result to its MOB.
    ReadbackQuery {
        /// Query.
        id: u32,
    },
    /// Moves a query's result to another MOB.
    MoveQuery {
        /// Query.
        id: u32,
        /// MOB.
        mob: u32,
        /// Byte offset.
        offset: u32,
    },
    /// Binds every query of a context to a MOB.
    BindAllQuery {
        /// Context.
        cid: u32,
        /// MOB.
        mob: u32,
    },
    /// Writes every query result of a context.
    ReadbackAllQuery {
        /// Context.
        cid: u32,
    },
    /// Draws only if a predicate query has `value`.
    SetPredication {
        /// Query (`SVGA3D_INVALID_ID`: none).
        query: u32,
        /// Predicate value.
        value: u32,
    },
    /// Binds stream-output targets.
    SetSoTargets {
        /// Targets, from slot 0.
        targets: Vec<SoTarget>,
    },
    /// Sets the viewports.
    SetViewports {
        /// Viewports.
        viewports: Vec<Viewport>,
    },
    /// Sets the scissor rectangles.
    SetScissorRects {
        /// Rectangles.
        rects: Vec<ScissorRect>,
    },
    /// Clears a render target view.
    ClearRenderTargetView {
        /// View.
        view: u32,
        /// Color.
        color: [f32; 4],
    },
    /// Clears a depth-stencil view (`SVGA3D_CLEAR_DEPTH`, `SVGA3D_CLEAR_STENCIL`).
    ClearDepthStencilView {
        /// View.
        view: u32,
        /// What to clear.
        flags: u16,
        /// Stencil value.
        stencil: u16,
        /// Depth value.
        depth: f32,
    },
    /// Copies a box between subresources, unless predicated off.
    PredCopyRegion {
        /// Destination surface.
        dest: u32,
        /// Destination subresource.
        dest_sub: u32,
        /// Source surface.
        src: u32,
        /// Source subresource.
        src_sub: u32,
        /// Destination box and source corner.
        copy: CopyBox,
    },
    /// Copies a whole surface, unless predicated off.
    PredCopy {
        /// Destination surface.
        dest: u32,
        /// Source surface.
        src: u32,
    },
    /// Resolves a multisampled subresource (`PRED_` variant: unless predicated off).
    ResolveCopy {
        /// Predicated variant.
        predicated: bool,
        /// Destination surface.
        dest: u32,
        /// Destination subresource.
        dest_sub: u32,
        /// Source surface.
        src: u32,
        /// Source subresource.
        src_sub: u32,
        /// Format the resolve uses.
        format: u32,
    },
    /// Copies a box with format conversion and scaling, unless predicated off.
    PredConvertRegion {
        /// Destination surface.
        dest: u32,
        /// Destination subresource.
        dest_sub: u32,
        /// Destination box.
        dest_box: Box3,
        /// Source surface.
        src: u32,
        /// Source subresource.
        src_sub: u32,
        /// Source box.
        src_box: Box3,
    },
    /// Copies a whole surface with format conversion, unless predicated off.
    PredConvert {
        /// Destination surface.
        dest: u32,
        /// Source surface.
        src: u32,
    },
    /// Generates the mips of a shader resource view.
    GenMips {
        /// View.
        view: u32,
    },
    /// Uploads a box of a subresource from its MOB.
    UpdateSubResource {
        /// Surface.
        sid: u32,
        /// Subresource.
        sub: u32,
        /// Box.
        area: Box3,
    },
    /// Writes a subresource back to its MOB.
    ReadbackSubResource {
        /// Surface.
        sid: u32,
        /// Subresource.
        sub: u32,
    },
    /// Forgets the device copy of a subresource.
    InvalidateSubResource {
        /// Surface.
        sid: u32,
        /// Subresource.
        sub: u32,
    },
    /// Creates a shader resource view.
    DefineShaderResourceView {
        /// View.
        id: u32,
        /// Surface, format, dimension.
        target: ViewTarget,
        /// Range.
        range: SrvRange,
    },
    /// Destroys a shader resource view.
    DestroyShaderResourceView {
        /// View.
        id: u32,
    },
    /// Creates a render target view.
    DefineRenderTargetView {
        /// View.
        id: u32,
        /// Surface, format, dimension.
        target: ViewTarget,
        /// Range.
        range: RtvRange,
    },
    /// Destroys a render target view.
    DestroyRenderTargetView {
        /// View.
        id: u32,
    },
    /// Creates a depth-stencil view (both command versions).
    DefineDepthStencilView {
        /// View.
        id: u32,
        /// View description.
        view: DepthStencilView,
    },
    /// Destroys a depth-stencil view.
    DestroyDepthStencilView {
        /// View.
        id: u32,
    },
    /// Creates an input layout.
    DefineElementLayout {
        /// Layout.
        id: u32,
        /// Elements.
        elements: Vec<InputElement>,
    },
    /// Destroys an input layout.
    DestroyElementLayout {
        /// Layout.
        id: u32,
    },
    /// Creates a blend state.
    DefineBlendState {
        /// State.
        id: u32,
        /// Description.
        state: BlendState,
    },
    /// Destroys a blend state.
    DestroyBlendState {
        /// State.
        id: u32,
    },
    /// Creates a depth-stencil state.
    DefineDepthStencilState {
        /// State.
        id: u32,
        /// Description.
        state: DepthStencilState,
    },
    /// Destroys a depth-stencil state.
    DestroyDepthStencilState {
        /// State.
        id: u32,
    },
    /// Creates a rasterizer state (both command versions).
    DefineRasterizerState {
        /// State.
        id: u32,
        /// Description.
        state: RasterizerState,
    },
    /// Destroys a rasterizer state.
    DestroyRasterizerState {
        /// State.
        id: u32,
    },
    /// Creates a sampler state.
    DefineSamplerState {
        /// State.
        id: u32,
        /// Description.
        state: SamplerState,
    },
    /// Destroys a sampler state.
    DestroySamplerState {
        /// State.
        id: u32,
    },
    /// Creates a shader of `size` bytes; its code comes with `BindShader`.
    DefineShader {
        /// Shader.
        id: u32,
        /// Stage.
        stage: ShaderStage,
        /// Bytes of code (with signatures).
        size: u32,
    },
    /// Destroys a shader.
    DestroyShader {
        /// Shader.
        id: u32,
    },
    /// Gives a shader its code: `size` bytes at `offset` in a MOB.
    BindShader {
        /// Context.
        cid: u32,
        /// Shader.
        shader: u32,
        /// MOB (`SVGA3D_INVALID_ID`: unbind).
        mob: u32,
        /// Byte offset.
        offset: u32,
    },
    /// Gives every shader of a context its code, from a MOB.
    BindAllShader {
        /// Context.
        cid: u32,
        /// MOB.
        mob: u32,
    },
    /// Like `BindAllShader`, if the shaders currently use `test_mob`.
    CondBindAllShader {
        /// Context.
        cid: u32,
        /// MOB the shaders must use now.
        test_mob: u32,
        /// New MOB.
        mob: u32,
    },
    /// Creates a stream-output object.
    DefineStreamOutput {
        /// Object.
        id: u32,
        /// Declarations.
        decls: Vec<StreamOutputDecl>,
        /// Bytes per vertex of each target.
        strides: [u32; dx::MAX_SOTARGETS],
        /// Stream sent to the rasterizer.
        rasterized_stream: u32,
    },
    /// Destroys a stream-output object.
    DestroyStreamOutput {
        /// Object.
        id: u32,
    },
    /// Binds a stream-output object.
    SetStreamOutput {
        /// Object (`SVGA3D_INVALID_ID`: none).
        id: u32,
    },
    /// Gives a context the MOB of one of its object tables;
    /// `valid_size` bytes of it hold objects to load (`GROW`: the table
    /// moves to a larger MOB).
    SetCoTable {
        /// Context.
        cid: u32,
        /// MOB.
        mob: u32,
        /// `SVGACOTableType`.
        table: u32,
        /// Valid bytes.
        valid_size: u32,
        /// `DX_GROW_COTABLE`.
        grow: bool,
    },
    /// Writes an object table back to its MOB.
    ReadbackCoTable {
        /// Context.
        cid: u32,
        /// `SVGACOTableType`.
        table: u32,
    },
    /// Copies bytes between buffers.
    BufferCopy {
        /// Destination buffer.
        dest: u32,
        /// Source buffer.
        src: u32,
        /// Destination offset.
        dest_x: u32,
        /// Source offset.
        src_x: u32,
        /// Bytes.
        width: u32,
    },
    /// Fills a box of a subresource from a buffer surface (`PRED_`:
    /// unless predicated off).
    TransferFromBuffer {
        /// Predicated variant.
        predicated: bool,
        /// Source buffer.
        src: u32,
        /// Byte offset of the first row.
        src_offset: u32,
        /// Bytes between rows.
        src_pitch: u32,
        /// Bytes between depth slices.
        src_slice_pitch: u32,
        /// Destination surface.
        dest: u32,
        /// Destination subresource.
        dest_sub: u32,
        /// Destination box.
        dest_box: Box3,
    },
    /// Copies a box between surfaces, then writes the destination back.
    SurfaceCopyAndReadback {
        /// Source surface.
        src: u32,
        /// Destination surface.
        dest: u32,
        /// Destination box and source corner.
        copy: CopyBox,
    },
    /// Writes `value` into a MOB once the previous commands are done.
    MobFence64 {
        /// Value.
        value: u64,
        /// MOB.
        mob: u32,
        /// Byte offset.
        offset: u32,
    },
    /// A performance hint.
    Hint {
        /// `SVGADXHintId`.
        hint: u32,
    },
    /// Uploads bytes `x..x+width` of a buffer from its MOB.
    BufferUpdate {
        /// Buffer.
        sid: u32,
        /// First byte.
        x: u32,
        /// Bytes.
        width: u32,
    },
}

/// A DX command that cannot be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DxError {
    /// Shorter than its fixed part.
    Short {
        /// Command ID.
        id: u32,
        /// Payload bytes.
        bytes: u32,
        /// Bytes needed.
        needed: u32,
    },
    /// Its array part is not a whole number of elements.
    Ragged {
        /// Command ID.
        id: u32,
        /// Bytes of the array part.
        bytes: u32,
        /// Bytes per element.
        element: u32,
    },
    /// More array elements than Direct3D allows.
    TooMany {
        /// Command ID.
        id: u32,
        /// Elements.
        count: u32,
        /// Largest count.
        max: u32,
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

impl core::fmt::Display for DxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Short { id, bytes, needed } => {
                write!(f, "DX command {id} has {bytes} bytes, {needed} needed")
            }
            Self::Ragged { id, bytes, element } => write!(
                f,
                "DX command {id}: {bytes} array bytes are not a multiple of {element}"
            ),
            Self::TooMany { id, count, max } => {
                write!(f, "DX command {id}: {count} elements, at most {max}")
            }
            Self::BadValue { id, field, value } => {
                write!(f, "DX command {id}: {field} = {value} is not valid")
            }
        }
    }
}

/// Fixed bytes, then bytes per array element (0: no array) and the
/// largest element count, of each command decoded here.
fn layout(id: u32) -> Option<(usize, usize, usize)> {
    use cmd3d::*;
    let none = |fixed| Some((fixed, 0, 0));
    match id {
        DX_DEFINE_CONTEXT | DX_DESTROY_CONTEXT | DX_READBACK_CONTEXT | DX_INVALIDATE_CONTEXT => {
            none(4)
        }
        DX_BIND_CONTEXT => none(12),
        DX_SET_SINGLE_CONSTANT_BUFFER => none(20),
        DX_SET_SHADER_RESOURCES => Some((8, 4, dx::MAX_SRVIEWS)),
        DX_SET_SHADER => none(8),
        DX_SET_SAMPLERS => Some((8, 4, dx::MAX_SAMPLERS)),
        DX_DRAW => none(8),
        DX_DRAW_INDEXED => none(12),
        DX_DRAW_INSTANCED => none(16),
        DX_DRAW_INDEXED_INSTANCED => none(20),
        DX_DRAW_AUTO => none(4),
        DX_SET_INPUT_LAYOUT => none(4),
        DX_SET_VERTEX_BUFFERS => Some((4, 12, dx::MAX_VERTEXBUFFERS)),
        DX_SET_VERTEX_BUFFERS_V2 => Some((4, 16, dx::MAX_VERTEXBUFFERS)),
        DX_SET_VERTEX_BUFFERS_OFFSET_AND_SIZE => Some((4, 12, dx::MAX_VERTEXBUFFERS)),
        DX_SET_INDEX_BUFFER => none(12),
        DX_SET_INDEX_BUFFER_V2 => none(16),
        DX_SET_INDEX_BUFFER_OFFSET_AND_SIZE => none(12),
        DX_SET_TOPOLOGY => none(4),
        DX_SET_RENDERTARGETS => Some((4, 4, dx::MAX_RENDER_TARGETS)),
        DX_SET_BLEND_STATE => none(24),
        DX_SET_DEPTHSTENCIL_STATE => none(8),
        DX_SET_RASTERIZER_STATE => none(4),
        DX_DEFINE_QUERY => none(12),
        DX_DESTROY_QUERY | DX_BEGIN_QUERY | DX_END_QUERY | DX_READBACK_QUERY => none(4),
        DX_BIND_QUERY | DX_SET_QUERY_OFFSET => none(8),
        DX_MOVE_QUERY => none(12),
        DX_BIND_ALL_QUERY => none(8),
        DX_READBACK_ALL_QUERY => none(4),
        DX_SET_PREDICATION => none(8),
        DX_SET_SOTARGETS => Some((4, 12, dx::MAX_SOTARGETS)),
        DX_SET_VIEWPORTS => Some((4, 24, dx::MAX_VIEWPORTS)),
        DX_SET_SCISSORRECTS => Some((4, 16, dx::MAX_SCISSORRECTS)),
        DX_CLEAR_RENDERTARGET_VIEW => none(20),
        DX_CLEAR_DEPTHSTENCIL_VIEW => none(12),
        DX_PRED_COPY_REGION => none(52),
        DX_PRED_COPY | DX_PRED_CONVERT => none(8),
        DX_RESOLVE_COPY | DX_PRED_RESOLVE_COPY => none(20),
        DX_PRED_CONVERT_REGION => none(64),
        DX_GENMIPS => none(4),
        DX_UPDATE_SUBRESOURCE => none(32),
        DX_READBACK_SUBRESOURCE | DX_INVALIDATE_SUBRESOURCE => none(8),
        DX_DEFINE_SHADERRESOURCE_VIEW => none(32),
        DX_DEFINE_RENDERTARGET_VIEW => none(28),
        DX_DEFINE_DEPTHSTENCIL_VIEW | DX_DEFINE_DEPTHSTENCIL_VIEW_V2 => none(32),
        DX_DEFINE_ELEMENTLAYOUT => Some((4, 24, dx::MAX_ELEMENTS)),
        DX_DEFINE_BLEND_STATE => none(104),
        DX_DEFINE_DEPTHSTENCIL_STATE => none(20),
        DX_DEFINE_RASTERIZER_STATE => none(32),
        DX_DEFINE_RASTERIZER_STATE_V2 => none(36),
        DX_DEFINE_SAMPLER_STATE => none(44),
        DX_DESTROY_SHADERRESOURCE_VIEW
        | DX_DESTROY_RENDERTARGET_VIEW
        | DX_DESTROY_DEPTHSTENCIL_VIEW
        | DX_DESTROY_ELEMENTLAYOUT
        | DX_DESTROY_BLEND_STATE
        | DX_DESTROY_DEPTHSTENCIL_STATE
        | DX_DESTROY_RASTERIZER_STATE
        | DX_DESTROY_SAMPLER_STATE
        | DX_DESTROY_SHADER
        | DX_DESTROY_STREAMOUTPUT
        | DX_SET_STREAMOUTPUT => none(4),
        DX_DEFINE_SHADER => none(12),
        DX_BIND_SHADER => none(16),
        DX_BIND_ALL_SHADER => none(8),
        DX_COND_BIND_ALL_SHADER => none(12),
        // soid, count, 64 declarations of 16 bytes, 4 strides, stream.
        DX_DEFINE_STREAMOUTPUT => none(1052),
        DX_SET_COTABLE | DX_GROW_COTABLE => none(16),
        DX_READBACK_COTABLE => none(8),
        DX_BUFFER_COPY => none(20),
        DX_TRANSFER_FROM_BUFFER | DX_PRED_TRANSFER_FROM_BUFFER => none(48),
        DX_SURFACE_COPY_AND_READBACK => none(44),
        DX_MOB_FENCE_64 => none(16),
        DX_HINT => none(4),
        DX_BUFFER_UPDATE => none(12),
        DX_SET_VS_CONSTANT_BUFFER_OFFSET
        | DX_SET_PS_CONSTANT_BUFFER_OFFSET
        | DX_SET_GS_CONSTANT_BUFFER_OFFSET
        | DX_SET_HS_CONSTANT_BUFFER_OFFSET
        | DX_SET_DS_CONSTANT_BUFFER_OFFSET
        | DX_SET_CS_CONSTANT_BUFFER_OFFSET => none(8),
        _ => None,
    }
}

/// Little-endian fields of a packed structure, read in order. Reads past
/// the end give 0 (callers check the length first).
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        if let Some(src) = self.bytes.get(self.at..self.at.saturating_add(N)) {
            out.copy_from_slice(src);
        }
        self.at = self.at.saturating_add(N);
        out
    }

    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn bool(&mut self) -> bool {
        self.u8() != 0
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn f32x4(&mut self) -> [f32; 4] {
        [self.f32(), self.f32(), self.f32(), self.f32()]
    }
    fn skip(&mut self, n: usize) {
        self.at = self.at.saturating_add(n);
    }
    fn box3(&mut self) -> Box3 {
        Box3 {
            x: self.u32(),
            y: self.u32(),
            z: self.u32(),
            w: self.u32(),
            h: self.u32(),
            d: self.u32(),
        }
    }
    fn copy_box(&mut self) -> CopyBox {
        let dest = self.box3();
        CopyBox {
            dest,
            src: (self.u32(), self.u32(), self.u32()),
        }
    }
}

/// Decodes a DX command whose payload is `size` bytes (`payload` holds
/// them, padded to whole words). `None`: not a DX command decoded here.
pub fn parse(id: u32, size: u32, payload: &[u32]) -> Option<Result<DxCommand, DxError>> {
    let (fixed, element, max) = layout(id)?;
    let bytes: Vec<u8> = payload.iter().flat_map(|w| w.to_le_bytes()).collect();
    let len = usize::try_from(size).unwrap_or(usize::MAX).min(bytes.len());
    let bytes = &bytes[..len];
    let short = DxError::Short {
        id,
        bytes: size,
        needed: u32::try_from(fixed).unwrap_or(u32::MAX),
    };
    let Some(tail) = bytes.get(fixed..) else {
        return Some(Err(short));
    };
    let elements: Vec<&[u8]> = if element == 0 {
        Vec::new()
    } else {
        if tail.len().checked_rem(element) != Some(0) {
            return Some(Err(DxError::Ragged {
                id,
                bytes: u32::try_from(tail.len()).unwrap_or(u32::MAX),
                element: u32::try_from(element).unwrap_or(u32::MAX),
            }));
        }
        let count = tail.len().checked_div(element).unwrap_or(0);
        if count > max {
            return Some(Err(DxError::TooMany {
                id,
                count: u32::try_from(count).unwrap_or(u32::MAX),
                max: u32::try_from(max).unwrap_or(u32::MAX),
            }));
        }
        tail.chunks_exact(element).collect()
    };
    Some(decode(id, Fields::new(bytes), &elements))
}

fn stage(id: u32, value: u32) -> Result<ShaderStage, DxError> {
    ShaderStage::from_raw(value).ok_or(DxError::BadValue {
        id,
        field: "shader type",
        value,
    })
}

fn ids(elements: &[&[u8]]) -> Vec<u32> {
    elements.iter().map(|e| Fields::new(e).u32()).collect()
}

fn decode(id: u32, mut f: Fields<'_>, elements: &[&[u8]]) -> Result<DxCommand, DxError> {
    use DxCommand as C;
    use cmd3d::*;
    Ok(match id {
        DX_DEFINE_CONTEXT => C::DefineContext { cid: f.u32() },
        DX_DESTROY_CONTEXT => C::DestroyContext { cid: f.u32() },
        DX_BIND_CONTEXT => C::BindContext {
            cid: f.u32(),
            mob: f.u32(),
            valid_contents: f.u32() != 0,
        },
        DX_READBACK_CONTEXT => C::ReadbackContext { cid: f.u32() },
        DX_INVALIDATE_CONTEXT => C::InvalidateContext { cid: f.u32() },
        DX_SET_SINGLE_CONSTANT_BUFFER => {
            let slot = f.u32();
            C::SetSingleConstantBuffer {
                stage: stage(id, f.u32())?,
                slot,
                sid: f.u32(),
                offset: f.u32(),
                size: f.u32(),
            }
        }
        DX_SET_VS_CONSTANT_BUFFER_OFFSET
        | DX_SET_PS_CONSTANT_BUFFER_OFFSET
        | DX_SET_GS_CONSTANT_BUFFER_OFFSET
        | DX_SET_HS_CONSTANT_BUFFER_OFFSET
        | DX_SET_DS_CONSTANT_BUFFER_OFFSET
        | DX_SET_CS_CONSTANT_BUFFER_OFFSET => C::SetConstantBufferOffset {
            stage: match id {
                DX_SET_VS_CONSTANT_BUFFER_OFFSET => ShaderStage::Vertex,
                DX_SET_PS_CONSTANT_BUFFER_OFFSET => ShaderStage::Pixel,
                DX_SET_GS_CONSTANT_BUFFER_OFFSET => ShaderStage::Geometry,
                DX_SET_HS_CONSTANT_BUFFER_OFFSET => ShaderStage::Hull,
                DX_SET_DS_CONSTANT_BUFFER_OFFSET => ShaderStage::Domain,
                _ => ShaderStage::Compute,
            },
            slot: f.u32(),
            offset: f.u32(),
        },
        DX_SET_SHADER_RESOURCES => {
            let start = f.u32();
            C::SetShaderResources {
                stage: stage(id, f.u32())?,
                start,
                views: ids(elements),
            }
        }
        DX_SET_SHADER => {
            let shader = f.u32();
            C::SetShader {
                stage: stage(id, f.u32())?,
                shader,
            }
        }
        DX_SET_SAMPLERS => {
            let start = f.u32();
            C::SetSamplers {
                stage: stage(id, f.u32())?,
                start,
                samplers: ids(elements),
            }
        }
        DX_DRAW => C::Draw {
            vertex_count: f.u32(),
            start_vertex: f.u32(),
        },
        DX_DRAW_INDEXED => C::DrawIndexed {
            index_count: f.u32(),
            start_index: f.u32(),
            base_vertex: f.i32(),
        },
        DX_DRAW_INSTANCED => C::DrawInstanced {
            vertex_count: f.u32(),
            instance_count: f.u32(),
            start_vertex: f.u32(),
            start_instance: f.u32(),
        },
        DX_DRAW_INDEXED_INSTANCED => C::DrawIndexedInstanced {
            index_count: f.u32(),
            instance_count: f.u32(),
            start_index: f.u32(),
            base_vertex: f.i32(),
            start_instance: f.u32(),
        },
        DX_DRAW_AUTO => C::DrawAuto,
        DX_SET_INPUT_LAYOUT => C::SetInputLayout { layout: f.u32() },
        DX_SET_VERTEX_BUFFERS | DX_SET_VERTEX_BUFFERS_V2 => C::SetVertexBuffers {
            start: f.u32(),
            buffers: elements
                .iter()
                .map(|e| {
                    let mut e = Fields::new(e);
                    VertexBuffer {
                        sid: e.u32(),
                        stride: e.u32(),
                        offset: e.u32(),
                        size: if id == DX_SET_VERTEX_BUFFERS_V2 {
                            e.u32()
                        } else {
                            WHOLE_SIZE
                        },
                    }
                })
                .collect(),
        },
        DX_SET_VERTEX_BUFFERS_OFFSET_AND_SIZE => C::SetVertexBuffersOffsetAndSize {
            start: f.u32(),
            ranges: elements
                .iter()
                .map(|e| {
                    let mut e = Fields::new(e);
                    VertexBufferRange {
                        stride: e.u32(),
                        offset: e.u32(),
                        size: e.u32(),
                    }
                })
                .collect(),
        },
        DX_SET_INDEX_BUFFER | DX_SET_INDEX_BUFFER_V2 => C::SetIndexBuffer {
            sid: f.u32(),
            format: f.u32(),
            offset: f.u32(),
            size: if id == DX_SET_INDEX_BUFFER_V2 {
                f.u32()
            } else {
                WHOLE_SIZE
            },
        },
        DX_SET_INDEX_BUFFER_OFFSET_AND_SIZE => C::SetIndexBufferOffsetAndSize {
            format: f.u32(),
            offset: f.u32(),
            size: f.u32(),
        },
        DX_SET_TOPOLOGY => C::SetTopology { topology: f.u32() },
        DX_SET_RENDERTARGETS => C::SetRenderTargets {
            depth_stencil: f.u32(),
            targets: ids(elements),
        },
        DX_SET_BLEND_STATE => C::SetBlendState {
            id: f.u32(),
            factor: f.f32x4(),
            sample_mask: f.u32(),
        },
        DX_SET_DEPTHSTENCIL_STATE => C::SetDepthStencilState {
            id: f.u32(),
            stencil_ref: f.u32(),
        },
        DX_SET_RASTERIZER_STATE => C::SetRasterizerState { id: f.u32() },
        DX_DEFINE_QUERY => {
            let query = f.u32();
            let kind = f.u32();
            if kind >= dx::query::MAX {
                return Err(DxError::BadValue {
                    id,
                    field: "query type",
                    value: kind,
                });
            }
            C::DefineQuery {
                id: query,
                kind,
                flags: f.u32(),
            }
        }
        DX_DESTROY_QUERY => C::DestroyQuery { id: f.u32() },
        DX_BIND_QUERY => C::BindQuery {
            id: f.u32(),
            mob: f.u32(),
        },
        DX_SET_QUERY_OFFSET => C::SetQueryOffset {
            id: f.u32(),
            offset: f.u32(),
        },
        DX_BEGIN_QUERY => C::BeginQuery { id: f.u32() },
        DX_END_QUERY => C::EndQuery { id: f.u32() },
        DX_READBACK_QUERY => C::ReadbackQuery { id: f.u32() },
        DX_MOVE_QUERY => C::MoveQuery {
            id: f.u32(),
            mob: f.u32(),
            offset: f.u32(),
        },
        DX_BIND_ALL_QUERY => C::BindAllQuery {
            cid: f.u32(),
            mob: f.u32(),
        },
        DX_READBACK_ALL_QUERY => C::ReadbackAllQuery { cid: f.u32() },
        DX_SET_PREDICATION => C::SetPredication {
            query: f.u32(),
            value: f.u32(),
        },
        DX_SET_SOTARGETS => C::SetSoTargets {
            targets: elements
                .iter()
                .map(|e| {
                    let mut e = Fields::new(e);
                    SoTarget {
                        sid: e.u32(),
                        offset: e.u32(),
                        size: e.u32(),
                    }
                })
                .collect(),
        },
        DX_SET_VIEWPORTS => C::SetViewports {
            viewports: elements
                .iter()
                .map(|e| {
                    let mut e = Fields::new(e);
                    Viewport {
                        x: e.f32(),
                        y: e.f32(),
                        width: e.f32(),
                        height: e.f32(),
                        min_depth: e.f32(),
                        max_depth: e.f32(),
                    }
                })
                .collect(),
        },
        DX_SET_SCISSORRECTS => C::SetScissorRects {
            rects: elements
                .iter()
                .map(|e| {
                    let mut e = Fields::new(e);
                    ScissorRect {
                        left: e.i32(),
                        top: e.i32(),
                        right: e.i32(),
                        bottom: e.i32(),
                    }
                })
                .collect(),
        },
        DX_CLEAR_RENDERTARGET_VIEW => C::ClearRenderTargetView {
            view: f.u32(),
            color: f.f32x4(),
        },
        DX_CLEAR_DEPTHSTENCIL_VIEW => {
            let flags = f.u16();
            let stencil = f.u16();
            C::ClearDepthStencilView {
                view: f.u32(),
                flags,
                stencil,
                depth: f.f32(),
            }
        }
        DX_PRED_COPY_REGION => C::PredCopyRegion {
            dest: f.u32(),
            dest_sub: f.u32(),
            src: f.u32(),
            src_sub: f.u32(),
            copy: f.copy_box(),
        },
        DX_PRED_COPY => C::PredCopy {
            dest: f.u32(),
            src: f.u32(),
        },
        DX_RESOLVE_COPY | DX_PRED_RESOLVE_COPY => C::ResolveCopy {
            predicated: id == DX_PRED_RESOLVE_COPY,
            dest: f.u32(),
            dest_sub: f.u32(),
            src: f.u32(),
            src_sub: f.u32(),
            format: f.u32(),
        },
        DX_PRED_CONVERT_REGION => C::PredConvertRegion {
            dest: f.u32(),
            dest_sub: f.u32(),
            dest_box: f.box3(),
            src: f.u32(),
            src_sub: f.u32(),
            src_box: f.box3(),
        },
        DX_PRED_CONVERT => C::PredConvert {
            dest: f.u32(),
            src: f.u32(),
        },
        DX_GENMIPS => C::GenMips { view: f.u32() },
        DX_UPDATE_SUBRESOURCE => C::UpdateSubResource {
            sid: f.u32(),
            sub: f.u32(),
            area: f.box3(),
        },
        DX_READBACK_SUBRESOURCE => C::ReadbackSubResource {
            sid: f.u32(),
            sub: f.u32(),
        },
        DX_INVALIDATE_SUBRESOURCE => C::InvalidateSubResource {
            sid: f.u32(),
            sub: f.u32(),
        },
        DX_DEFINE_SHADERRESOURCE_VIEW => {
            let view = f.u32();
            let target = view_target(&mut f);
            let (a, b, c, d) = (f.u32(), f.u32(), f.u32(), f.u32());
            let range = match target.dimension {
                dx::resource::BUFFER => SrvRange::Buffer { first: a, count: b },
                dx::resource::BUFFEREX => SrvRange::BufferEx {
                    first: a,
                    count: b,
                    flags: c,
                },
                dx::resource::TEXTURE1D
                | dx::resource::TEXTURE2D
                | dx::resource::TEXTURE3D
                | dx::resource::TEXTURECUBE => SrvRange::Texture {
                    most_detailed_mip: a,
                    first_slice: b,
                    mip_levels: c,
                    slices: d,
                },
                other => return Err(bad_dimension(id, other)),
            };
            C::DefineShaderResourceView {
                id: view,
                target,
                range,
            }
        }
        DX_DEFINE_RENDERTARGET_VIEW => {
            let view = f.u32();
            let target = view_target(&mut f);
            let (a, b, c) = (f.u32(), f.u32(), f.u32());
            let range = match target.dimension {
                dx::resource::BUFFER => RtvRange::Buffer { first: a, count: b },
                dx::resource::TEXTURE1D | dx::resource::TEXTURE2D | dx::resource::TEXTURECUBE => {
                    RtvRange::Texture {
                        mip: a,
                        first_slice: b,
                        slices: c,
                    }
                }
                dx::resource::TEXTURE3D => RtvRange::Texture3d {
                    mip: a,
                    first_w: b,
                    w_size: c,
                },
                other => return Err(bad_dimension(id, other)),
            };
            C::DefineRenderTargetView {
                id: view,
                target,
                range,
            }
        }
        DX_DEFINE_DEPTHSTENCIL_VIEW | DX_DEFINE_DEPTHSTENCIL_VIEW_V2 => {
            let view = f.u32();
            let target = view_target(&mut f);
            if !matches!(
                target.dimension,
                dx::resource::TEXTURE1D | dx::resource::TEXTURE2D | dx::resource::TEXTURECUBE
            ) {
                return Err(bad_dimension(id, target.dimension));
            }
            C::DefineDepthStencilView {
                id: view,
                view: DepthStencilView {
                    target,
                    mip: f.u32(),
                    first_slice: f.u32(),
                    slices: f.u32(),
                    flags: f.u8(),
                },
            }
        }
        DX_DEFINE_ELEMENTLAYOUT => C::DefineElementLayout {
            id: f.u32(),
            elements: elements
                .iter()
                .map(|e| {
                    let mut e = Fields::new(e);
                    InputElement {
                        slot: e.u32(),
                        offset: e.u32(),
                        format: e.u32(),
                        per_instance: e.u32() == dx::INPUT_PER_INSTANCE_DATA,
                        step_rate: e.u32(),
                        register: e.u32(),
                    }
                })
                .collect(),
        },
        DX_DEFINE_BLEND_STATE => {
            let state = f.u32();
            let alpha_to_coverage = f.bool();
            let independent = f.bool();
            f.skip(2);
            let mut targets = [BlendTarget::default(); dx::MAX_RENDER_TARGETS];
            for t in &mut targets {
                *t = BlendTarget {
                    enable: f.bool(),
                    src: f.u8(),
                    dest: f.u8(),
                    op: f.u8(),
                    src_alpha: f.u8(),
                    dest_alpha: f.u8(),
                    op_alpha: f.u8(),
                    write_mask: f.u8(),
                    logic_op_enable: f.bool(),
                    logic_op: f.u8(),
                };
                f.skip(2);
            }
            C::DefineBlendState {
                id: state,
                state: BlendState {
                    alpha_to_coverage,
                    independent,
                    targets,
                },
            }
        }
        DX_DEFINE_DEPTHSTENCIL_STATE => {
            let state = f.u32();
            let face = |f: &mut Fields<'_>| StencilFace {
                fail_op: f.u8(),
                depth_fail_op: f.u8(),
                pass_op: f.u8(),
                func: f.u8(),
            };
            C::DefineDepthStencilState {
                id: state,
                state: DepthStencilState {
                    depth_enable: f.bool(),
                    depth_write_mask: f.u8(),
                    depth_func: f.u8(),
                    stencil_enable: f.bool(),
                    front_enable: f.bool(),
                    back_enable: f.bool(),
                    stencil_read_mask: f.u8(),
                    stencil_write_mask: f.u8(),
                    front: face(&mut f),
                    back: face(&mut f),
                },
            }
        }
        DX_DEFINE_RASTERIZER_STATE | DX_DEFINE_RASTERIZER_STATE_V2 => {
            let state = f.u32();
            let mut s = RasterizerState {
                fill_mode: f.u8(),
                cull_mode: f.u8(),
                front_ccw: f.bool(),
                provoking_vertex_last: f.bool(),
                depth_bias: f.i32(),
                depth_bias_clamp: f.f32(),
                slope_scaled_depth_bias: f.f32(),
                depth_clip: f.bool(),
                scissor: f.bool(),
                multisample: f.u8(),
                antialiased_line: f.bool(),
                line_width: f.f32(),
                line_stipple: f.bool(),
                line_stipple_factor: f.u8(),
                line_stipple_pattern: f.u16(),
                forced_sample_count: 0,
            };
            if id == DX_DEFINE_RASTERIZER_STATE_V2 {
                s.forced_sample_count = f.u32();
            }
            C::DefineRasterizerState {
                id: state,
                state: s,
            }
        }
        DX_DEFINE_SAMPLER_STATE => {
            let state = f.u32();
            let filter = f.u32();
            let address = [f.u8(), f.u8(), f.u8()];
            f.skip(1);
            let mip_lod_bias = f.f32();
            let max_anisotropy = f.u8();
            let comparison = f.u8();
            f.skip(2);
            C::DefineSamplerState {
                id: state,
                state: SamplerState {
                    filter,
                    address,
                    mip_lod_bias,
                    max_anisotropy,
                    comparison,
                    border_color: f.f32x4(),
                    min_lod: f.f32(),
                    max_lod: f.f32(),
                },
            }
        }
        DX_DESTROY_SHADERRESOURCE_VIEW => C::DestroyShaderResourceView { id: f.u32() },
        DX_DESTROY_RENDERTARGET_VIEW => C::DestroyRenderTargetView { id: f.u32() },
        DX_DESTROY_DEPTHSTENCIL_VIEW => C::DestroyDepthStencilView { id: f.u32() },
        DX_DESTROY_ELEMENTLAYOUT => C::DestroyElementLayout { id: f.u32() },
        DX_DESTROY_BLEND_STATE => C::DestroyBlendState { id: f.u32() },
        DX_DESTROY_DEPTHSTENCIL_STATE => C::DestroyDepthStencilState { id: f.u32() },
        DX_DESTROY_RASTERIZER_STATE => C::DestroyRasterizerState { id: f.u32() },
        DX_DESTROY_SAMPLER_STATE => C::DestroySamplerState { id: f.u32() },
        DX_DEFINE_SHADER => {
            let shader = f.u32();
            C::DefineShader {
                id: shader,
                stage: stage(id, f.u32())?,
                size: f.u32(),
            }
        }
        DX_DESTROY_SHADER => C::DestroyShader { id: f.u32() },
        DX_BIND_SHADER => C::BindShader {
            cid: f.u32(),
            shader: f.u32(),
            mob: f.u32(),
            offset: f.u32(),
        },
        DX_BIND_ALL_SHADER => C::BindAllShader {
            cid: f.u32(),
            mob: f.u32(),
        },
        DX_COND_BIND_ALL_SHADER => C::CondBindAllShader {
            cid: f.u32(),
            test_mob: f.u32(),
            mob: f.u32(),
        },
        DX_DEFINE_STREAMOUTPUT => {
            let so = f.u32();
            let count = f.u32();
            let declared = usize::try_from(count).unwrap_or(usize::MAX);
            if declared > dx::MAX_STREAMOUT_DECLS {
                return Err(DxError::TooMany {
                    id,
                    count,
                    max: u32::try_from(dx::MAX_STREAMOUT_DECLS).unwrap_or(u32::MAX),
                });
            }
            let mut decls = Vec::with_capacity(declared);
            for _ in 0..dx::MAX_STREAMOUT_DECLS {
                let decl = StreamOutputDecl {
                    slot: f.u32(),
                    register: f.u32(),
                    mask: f.u8(),
                    stream: {
                        f.skip(3);
                        f.u32()
                    },
                };
                if decls.len() < declared {
                    decls.push(decl);
                }
            }
            C::DefineStreamOutput {
                id: so,
                decls,
                strides: [f.u32(), f.u32(), f.u32(), f.u32()],
                rasterized_stream: f.u32(),
            }
        }
        DX_DESTROY_STREAMOUTPUT => C::DestroyStreamOutput { id: f.u32() },
        DX_SET_STREAMOUTPUT => C::SetStreamOutput { id: f.u32() },
        DX_SET_COTABLE | DX_GROW_COTABLE => {
            let cid = f.u32();
            let mob = f.u32();
            let table = cotable(id, f.u32())?;
            C::SetCoTable {
                cid,
                mob,
                table,
                valid_size: f.u32(),
                grow: id == DX_GROW_COTABLE,
            }
        }
        DX_READBACK_COTABLE => {
            let cid = f.u32();
            C::ReadbackCoTable {
                cid,
                table: cotable(id, f.u32())?,
            }
        }
        DX_BUFFER_COPY => C::BufferCopy {
            dest: f.u32(),
            src: f.u32(),
            dest_x: f.u32(),
            src_x: f.u32(),
            width: f.u32(),
        },
        DX_TRANSFER_FROM_BUFFER | DX_PRED_TRANSFER_FROM_BUFFER => C::TransferFromBuffer {
            predicated: id == DX_PRED_TRANSFER_FROM_BUFFER,
            src: f.u32(),
            src_offset: f.u32(),
            src_pitch: f.u32(),
            src_slice_pitch: f.u32(),
            dest: f.u32(),
            dest_sub: f.u32(),
            dest_box: f.box3(),
        },
        DX_SURFACE_COPY_AND_READBACK => C::SurfaceCopyAndReadback {
            src: f.u32(),
            dest: f.u32(),
            copy: f.copy_box(),
        },
        DX_MOB_FENCE_64 => C::MobFence64 {
            value: f.u64(),
            mob: f.u32(),
            offset: f.u32(),
        },
        DX_HINT => C::Hint { hint: f.u32() },
        DX_BUFFER_UPDATE => C::BufferUpdate {
            sid: f.u32(),
            x: f.u32(),
            width: f.u32(),
        },
        _ => {
            return Err(DxError::BadValue {
                id,
                field: "command",
                value: id,
            });
        }
    })
}

fn view_target(f: &mut Fields<'_>) -> ViewTarget {
    ViewTarget {
        sid: f.u32(),
        format: f.u32(),
        dimension: f.u32(),
    }
}

fn bad_dimension(id: u32, value: u32) -> DxError {
    DxError::BadValue {
        id,
        field: "resource dimension",
        value,
    }
}

fn cotable(id: u32, value: u32) -> Result<u32, DxError> {
    if value < dx::cotable::MAX {
        Ok(value)
    } else {
        Err(DxError::BadValue {
            id,
            field: "object table",
            value,
        })
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use svga_proto::{INVALID_ID, cmd3d::*};

    /// Packs little-endian fields into payload words.
    #[derive(Default)]
    struct Bytes(Vec<u8>);

    impl Bytes {
        fn u8(mut self, v: u8) -> Self {
            self.0.push(v);
            self
        }
        fn u16(mut self, v: u16) -> Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn u32(mut self, v: u32) -> Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn f32(self, v: f32) -> Self {
            self.u32(v.to_bits())
        }
        fn words(&self) -> (u32, Vec<u32>) {
            let words = self
                .0
                .chunks(4)
                .map(|c| {
                    let mut w = [0u8; 4];
                    w[..c.len()].copy_from_slice(c);
                    u32::from_le_bytes(w)
                })
                .collect();
            (self.0.len() as u32, words)
        }
    }

    fn run(id: u32, b: Bytes) -> Result<DxCommand, DxError> {
        let (size, words) = b.words();
        parse(id, size, &words).expect("a DX command")
    }

    fn words(w: &[u32]) -> Bytes {
        w.iter().fold(Bytes::default(), |b, &v| b.u32(v))
    }

    #[test]
    fn contexts_tables_and_fences() {
        assert_eq!(
            run(DX_DEFINE_CONTEXT, words(&[3])),
            Ok(DxCommand::DefineContext { cid: 3 })
        );
        assert_eq!(
            run(DX_BIND_CONTEXT, words(&[3, 9, 1])),
            Ok(DxCommand::BindContext {
                cid: 3,
                mob: 9,
                valid_contents: true
            })
        );
        assert_eq!(
            run(DX_GROW_COTABLE, words(&[3, 10, 2, 4096])),
            Ok(DxCommand::SetCoTable {
                cid: 3,
                mob: 10,
                table: 2,
                valid_size: 4096,
                grow: true
            })
        );
        assert_eq!(
            run(DX_SET_COTABLE, words(&[3, 10, 12, 0])),
            Err(DxError::BadValue {
                id: DX_SET_COTABLE,
                field: "object table",
                value: 12
            })
        );
        assert_eq!(
            run(DX_MOB_FENCE_64, words(&[0x89ab_cdef, 0x0123_4567, 5, 64])),
            Ok(DxCommand::MobFence64 {
                value: 0x0123_4567_89ab_cdef,
                mob: 5,
                offset: 64
            })
        );
        assert_eq!(
            run(DX_SET_PS_CONSTANT_BUFFER_OFFSET, words(&[2, 256])),
            Ok(DxCommand::SetConstantBufferOffset {
                stage: ShaderStage::Pixel,
                slot: 2,
                offset: 256
            })
        );
    }

    #[test]
    fn bindings_with_arrays() {
        assert_eq!(
            run(
                DX_SET_SHADER_RESOURCES,
                words(&[4, dx::shader::PS, 7, INVALID_ID])
            ),
            Ok(DxCommand::SetShaderResources {
                stage: ShaderStage::Pixel,
                start: 4,
                views: vec![7, INVALID_ID]
            })
        );
        assert_eq!(
            run(DX_SET_VERTEX_BUFFERS, words(&[1, 20, 16, 0, 21, 8, 64])),
            Ok(DxCommand::SetVertexBuffers {
                start: 1,
                buffers: vec![
                    VertexBuffer {
                        sid: 20,
                        stride: 16,
                        offset: 0,
                        size: WHOLE_SIZE
                    },
                    VertexBuffer {
                        sid: 21,
                        stride: 8,
                        offset: 64,
                        size: WHOLE_SIZE
                    }
                ]
            })
        );
        // The WDDM 2.0 driver's variants, with sizes.
        assert_eq!(
            run(DX_SET_VERTEX_BUFFERS_V2, words(&[2, 20, 16, 32, 96])),
            Ok(DxCommand::SetVertexBuffers {
                start: 2,
                buffers: vec![VertexBuffer {
                    sid: 20,
                    stride: 16,
                    offset: 32,
                    size: 96
                }]
            })
        );
        assert_eq!(
            run(
                DX_SET_VERTEX_BUFFERS_OFFSET_AND_SIZE,
                words(&[0, 12, 0, 48])
            ),
            Ok(DxCommand::SetVertexBuffersOffsetAndSize {
                start: 0,
                ranges: vec![VertexBufferRange {
                    stride: 12,
                    offset: 0,
                    size: 48
                }]
            })
        );
        assert_eq!(
            run(DX_SET_INDEX_BUFFER_V2, words(&[5, 89, 8, 12])),
            Ok(DxCommand::SetIndexBuffer {
                sid: 5,
                format: 89,
                offset: 8,
                size: 12
            })
        );
        assert_eq!(
            run(DX_SET_INDEX_BUFFER_OFFSET_AND_SIZE, words(&[77, 4, 400])),
            Ok(DxCommand::SetIndexBufferOffsetAndSize {
                format: 77,
                offset: 4,
                size: 400
            })
        );
        assert_eq!(
            run(DX_SET_RENDERTARGETS, words(&[INVALID_ID, 1, 2])),
            Ok(DxCommand::SetRenderTargets {
                depth_stencil: INVALID_ID,
                targets: vec![1, 2]
            })
        );
        let vp = words(&[0])
            .f32(0.0)
            .f32(0.0)
            .f32(800.0)
            .f32(600.0)
            .f32(0.0)
            .f32(1.0);
        assert_eq!(
            run(DX_SET_VIEWPORTS, vp),
            Ok(DxCommand::SetViewports {
                viewports: vec![Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                    min_depth: 0.0,
                    max_depth: 1.0
                }]
            })
        );
        let sc = words(&[0, (-5i32) as u32, 0, 100, 50]);
        assert_eq!(
            run(DX_SET_SCISSORRECTS, sc),
            Ok(DxCommand::SetScissorRects {
                rects: vec![ScissorRect {
                    left: -5,
                    top: 0,
                    right: 100,
                    bottom: 50
                }]
            })
        );
        assert_eq!(
            run(DX_SET_SHADER, words(&[INVALID_ID, dx::shader::VS])),
            Ok(DxCommand::SetShader {
                stage: ShaderStage::Vertex,
                shader: INVALID_ID
            })
        );
    }

    #[test]
    fn draws_and_clears() {
        assert_eq!(
            run(DX_DRAW_INDEXED, words(&[6, 3, (-2i32) as u32])),
            Ok(DxCommand::DrawIndexed {
                index_count: 6,
                start_index: 3,
                base_vertex: -2
            })
        );
        assert_eq!(
            run(DX_DRAW_INDEXED_INSTANCED, words(&[6, 2, 0, 0, 1])),
            Ok(DxCommand::DrawIndexedInstanced {
                index_count: 6,
                instance_count: 2,
                start_index: 0,
                base_vertex: 0,
                start_instance: 1
            })
        );
        let clear = words(&[4]).f32(1.0).f32(0.5).f32(0.0).f32(1.0);
        assert_eq!(
            run(DX_CLEAR_RENDERTARGET_VIEW, clear),
            Ok(DxCommand::ClearRenderTargetView {
                view: 4,
                color: [1.0, 0.5, 0.0, 1.0]
            })
        );
        let ds = Bytes::default().u16(3).u16(0x80).u32(9).f32(1.0);
        assert_eq!(
            run(DX_CLEAR_DEPTHSTENCIL_VIEW, ds),
            Ok(DxCommand::ClearDepthStencilView {
                view: 9,
                flags: 3,
                stencil: 0x80,
                depth: 1.0
            })
        );
        let copy = words(&[1, 0, 2, 0, 10, 20, 0, 4, 4, 1, 1, 2, 0]);
        assert_eq!(
            run(DX_PRED_COPY_REGION, copy),
            Ok(DxCommand::PredCopyRegion {
                dest: 1,
                dest_sub: 0,
                src: 2,
                src_sub: 0,
                copy: CopyBox {
                    dest: Box3 {
                        x: 10,
                        y: 20,
                        z: 0,
                        w: 4,
                        h: 4,
                        d: 1
                    },
                    src: (1, 2, 0)
                }
            })
        );
    }

    #[test]
    fn views() {
        let srv = words(&[5, 30, 141, dx::resource::TEXTURE2D, 0, 0, u32::MAX, 1]);
        assert_eq!(
            run(DX_DEFINE_SHADERRESOURCE_VIEW, srv),
            Ok(DxCommand::DefineShaderResourceView {
                id: 5,
                target: ViewTarget {
                    sid: 30,
                    format: 141,
                    dimension: dx::resource::TEXTURE2D
                },
                range: SrvRange::Texture {
                    most_detailed_mip: 0,
                    first_slice: 0,
                    mip_levels: u32::MAX,
                    slices: 1
                }
            })
        );
        let rtv = words(&[6, 30, 141, dx::resource::TEXTURE3D, 1, 2, 3]);
        assert_eq!(
            run(DX_DEFINE_RENDERTARGET_VIEW, rtv),
            Ok(DxCommand::DefineRenderTargetView {
                id: 6,
                target: ViewTarget {
                    sid: 30,
                    format: 141,
                    dimension: dx::resource::TEXTURE3D
                },
                range: RtvRange::Texture3d {
                    mip: 1,
                    first_w: 2,
                    w_size: 3
                }
            })
        );
        let dsv = words(&[7, 31, 80, dx::resource::TEXTURE2D, 0, 0, 1])
            .u8(1)
            .u8(0)
            .u16(0);
        assert_eq!(
            run(DX_DEFINE_DEPTHSTENCIL_VIEW, dsv),
            Ok(DxCommand::DefineDepthStencilView {
                id: 7,
                view: DepthStencilView {
                    target: ViewTarget {
                        sid: 31,
                        format: 80,
                        dimension: dx::resource::TEXTURE2D
                    },
                    mip: 0,
                    first_slice: 0,
                    slices: 1,
                    flags: 1
                }
            })
        );
        let bad = words(&[7, 31, 80, dx::resource::BUFFER, 0, 0, 1, 0]);
        assert!(matches!(
            run(DX_DEFINE_DEPTHSTENCIL_VIEW, bad),
            Err(DxError::BadValue { value: 1, .. })
        ));
    }

    #[test]
    fn states_with_packed_bytes() {
        // Blend: target 0 blends (src alpha, inv src alpha, add), writes all.
        let mut blend = words(&[2]).u8(0).u8(1).u16(0);
        for rt in 0..8 {
            blend = if rt == 0 {
                blend.u8(1).u8(5).u8(6).u8(1).u8(2).u8(1).u8(1).u8(0xf)
            } else {
                blend.u8(0).u8(2).u8(1).u8(1).u8(2).u8(1).u8(1).u8(0xf)
            };
            blend = blend.u8(0).u8(0).u16(0);
        }
        let Ok(DxCommand::DefineBlendState { id: 2, state }) = run(DX_DEFINE_BLEND_STATE, blend)
        else {
            panic!("blend state");
        };
        assert!(state.independent && !state.alpha_to_coverage);
        assert_eq!(
            state.targets[0],
            BlendTarget {
                enable: true,
                src: 5,
                dest: 6,
                op: 1,
                src_alpha: 2,
                dest_alpha: 1,
                op_alpha: 1,
                write_mask: 0xf,
                logic_op_enable: false,
                logic_op: 0
            }
        );
        assert!(!state.targets[7].enable);

        let ds = words(&[4])
            .u8(1)
            .u8(1)
            .u8(2)
            .u8(0)
            .u8(1)
            .u8(1)
            .u8(0xff)
            .u8(0xff)
            .u8(1)
            .u8(1)
            .u8(1)
            .u8(8)
            .u8(1)
            .u8(1)
            .u8(3)
            .u8(8);
        let Ok(DxCommand::DefineDepthStencilState { id: 4, state }) =
            run(DX_DEFINE_DEPTHSTENCIL_STATE, ds)
        else {
            panic!("depth-stencil state");
        };
        assert!(state.depth_enable && !state.stencil_enable);
        assert_eq!((state.depth_func, state.back.pass_op), (2, 3));

        let rs = words(&[6])
            .u8(3)
            .u8(1)
            .u8(0)
            .u8(1)
            .u32((-1i32) as u32)
            .f32(0.0)
            .f32(2.0)
            .u8(1)
            .u8(0)
            .u8(0)
            .u8(0)
            .f32(1.0)
            .u8(0)
            .u8(0)
            .u16(0)
            .u32(4);
        let Ok(DxCommand::DefineRasterizerState { id: 6, state }) =
            run(DX_DEFINE_RASTERIZER_STATE_V2, rs)
        else {
            panic!("rasterizer state");
        };
        assert_eq!(
            (state.fill_mode, state.depth_bias, state.forced_sample_count),
            (3, -1, 4)
        );
        assert!(state.provoking_vertex_last && state.depth_clip);

        let sampler = words(&[8, 0x15])
            .u8(1)
            .u8(3)
            .u8(4)
            .u8(0)
            .f32(-0.5)
            .u8(16)
            .u8(2)
            .u16(0)
            .f32(1.0)
            .f32(0.0)
            .f32(0.0)
            .f32(1.0)
            .f32(0.0)
            .f32(13.0);
        let Ok(DxCommand::DefineSamplerState { id: 8, state }) =
            run(DX_DEFINE_SAMPLER_STATE, sampler)
        else {
            panic!("sampler state");
        };
        assert_eq!(
            state,
            SamplerState {
                filter: 0x15,
                address: [1, 3, 4],
                mip_lod_bias: -0.5,
                max_anisotropy: 16,
                comparison: 2,
                border_color: [1.0, 0.0, 0.0, 1.0],
                min_lod: 0.0,
                max_lod: 13.0
            }
        );
    }

    #[test]
    fn layouts_shaders_and_stream_output() {
        let layout = words(&[3, 0, 0, 141, 0, 0, 0, 1, 16, 125, 1, 1, 1]);
        assert_eq!(
            run(DX_DEFINE_ELEMENTLAYOUT, layout),
            Ok(DxCommand::DefineElementLayout {
                id: 3,
                elements: vec![
                    InputElement {
                        slot: 0,
                        offset: 0,
                        format: 141,
                        per_instance: false,
                        step_rate: 0,
                        register: 0
                    },
                    InputElement {
                        slot: 1,
                        offset: 16,
                        format: 125,
                        per_instance: true,
                        step_rate: 1,
                        register: 1
                    }
                ]
            })
        );
        assert_eq!(
            run(DX_DEFINE_SHADER, words(&[11, dx::shader::GS, 512])),
            Ok(DxCommand::DefineShader {
                id: 11,
                stage: ShaderStage::Geometry,
                size: 512
            })
        );
        assert_eq!(
            run(DX_DEFINE_SHADER, words(&[11, 7, 512])),
            Err(DxError::BadValue {
                id: DX_DEFINE_SHADER,
                field: "shader type",
                value: 7
            })
        );
        // Two declarations used out of 64, then strides and the stream.
        let mut so = words(&[9, 2]);
        for i in 0..64u32 {
            so = so.u32(i).u32(i + 10).u8(0xf).u8(0).u16(0).u32(0);
        }
        so = so.u32(16).u32(0).u32(0).u32(0).u32(0);
        let Ok(DxCommand::DefineStreamOutput {
            id: 9,
            decls,
            strides,
            rasterized_stream: 0,
        }) = run(DX_DEFINE_STREAMOUTPUT, so)
        else {
            panic!("stream output");
        };
        assert_eq!(decls.len(), 2);
        assert_eq!(
            (decls[1].slot, decls[1].register, decls[1].mask),
            (1, 11, 0xf)
        );
        assert_eq!(strides, [16, 0, 0, 0]);
    }

    #[test]
    fn hostile_payloads_are_refused() {
        // Too short, ragged arrays, too many elements.
        assert_eq!(
            run(DX_BIND_SHADER, words(&[1, 2])),
            Err(DxError::Short {
                id: DX_BIND_SHADER,
                bytes: 8,
                needed: 16
            })
        );
        assert_eq!(
            run(DX_SET_VERTEX_BUFFERS, words(&[0, 1, 2])),
            Err(DxError::Ragged {
                id: DX_SET_VERTEX_BUFFERS,
                bytes: 8,
                element: 12
            })
        );
        assert_eq!(
            run(DX_SET_RENDERTARGETS, words(&[0; 10])),
            Err(DxError::TooMany {
                id: DX_SET_RENDERTARGETS,
                count: 9,
                max: 8
            })
        );
        let mut so = words(&[9, 65]);
        so.0.resize(1052, 0);
        assert!(matches!(
            run(DX_DEFINE_STREAMOUTPUT, so),
            Err(DxError::TooMany { count: 65, .. })
        ));
        // The size announced counts, not the padding: 3 bytes of a 4-byte
        // command are too short even in a whole word.
        assert!(matches!(
            parse(DX_SET_TOPOLOGY, 3, &[1]),
            Some(Err(DxError::Short { .. }))
        ));
        // Not decoded here.
        assert_eq!(parse(DX_PRESENTBLT, 68, &[0; 17]), None);
        assert_eq!(parse(cmd3d::SURFACE_COPY, 24, &[0; 6]), None);
    }

    #[test]
    fn every_known_id_decodes_zeroes_or_refuses_cleanly() {
        for id in cmd3d::DX_DEFINE_CONTEXT..cmd3d::MAX {
            let Some((fixed, element, _)) = layout(id) else {
                continue;
            };
            let bytes = fixed + element;
            let words = vec![0u32; bytes.div_ceil(4)];
            match parse(id, bytes as u32, &words) {
                Some(Ok(_)) | Some(Err(DxError::BadValue { .. })) => {}
                other => panic!("command {id}: {other:?}"),
            }
        }
    }
}
