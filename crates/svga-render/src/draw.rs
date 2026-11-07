//! Draws: Direct3D 10 pipeline state as Vulkan pipelines, recorded with
//! dynamic rendering and push descriptors.
//!
//! A [`Draw`] names everything a draw uses by surface ID and state value;
//! this module turns it into Vulkan objects, cached: shader modules (by
//! SPIR-V contents), descriptor set and pipeline layouts (by bindings),
//! pipelines (by the whole fixed state), image views (per surface) and
//! samplers (by state). Viewports, scissors, the blend factor, the stencil
//! reference and vertex strides are dynamic.
//!
//! Direct3D reads zeros from unbound slots: such slots get "null"
//! resources, a zeroed buffer and 1x1 zeroed images of each kind.

use std::collections::HashMap;

use ash::vk;
use ash::vk::Handle;
use svga_core::dx::{
    BlendState, BlendTarget, DepthStencilState, RasterizerState, SamplerState, ScissorRect,
    StencilFace, Viewport,
};
use svga_shader::{BindingKind, Shader};

use crate::format::{self, Kind};
use crate::{Error, Renderer, Storage, ViewKey, err, range};

/// A vertex attribute (input layout element).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexAttribute {
    /// Vertex shader input register.
    pub location: u32,
    /// Vertex buffer slot.
    pub binding: u32,
    /// `SVGA3dSurfaceFormat`.
    pub format: u32,
    /// Byte offset in the vertex.
    pub offset: u32,
}

/// A vertex buffer slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexBuffer {
    /// Slot.
    pub binding: u32,
    /// Buffer surface (`None`: reads zeros).
    pub sid: Option<u32>,
    /// Byte offset.
    pub offset: u64,
    /// Bytes readable from `offset` (`u64::MAX`: to the end); reads past
    /// them return zeros.
    pub size: u64,
    /// Bytes per vertex.
    pub stride: u32,
    /// Advances per instance.
    pub per_instance: bool,
}

/// A render or depth target: one mip level of array layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    /// Surface.
    pub sid: u32,
    /// View format (`SVGA3dSurfaceFormat`).
    pub format: u32,
    /// Mip level.
    pub mip: u32,
    /// First layer.
    pub first_layer: u32,
}

/// What a descriptor binding gets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resource {
    /// Nothing bound: zeros.
    None,
    /// A constant buffer range.
    Buffer {
        /// Buffer surface.
        sid: u32,
        /// Byte offset.
        offset: u64,
        /// Bytes.
        size: u64,
    },
    /// A sampler.
    Sampler(SamplerState),
    /// Constants given with the draw: element `n` of [`Draw::data`].
    Data(usize),
    /// A shader resource view of a buffer: typed elements.
    TexelBuffer {
        /// Buffer surface.
        sid: u32,
        /// Element format.
        format: u32,
        /// Byte offset of the first element.
        offset: u64,
        /// Bytes.
        size: u64,
    },
    /// A buffer range read or written as words (raw and structured
    /// views: storage buffer).
    StorageBuffer {
        /// Buffer surface.
        sid: u32,
        /// Byte offset.
        offset: u64,
        /// Bytes.
        size: u64,
    },
    /// An unordered-access view of a texture: one mip of layers (storage
    /// image).
    StorageImage {
        /// Surface.
        sid: u32,
        /// View format.
        format: u32,
        /// Mip level.
        mip: u32,
        /// First layer.
        first_layer: u32,
        /// Layers.
        layers: u32,
    },
    /// A shader resource view of a texture.
    Texture {
        /// Surface.
        sid: u32,
        /// View format.
        format: u32,
        /// First mip level.
        first_mip: u32,
        /// Mip levels (`u32::MAX`: all).
        mips: u32,
        /// First layer.
        first_layer: u32,
        /// Layers.
        layers: u32,
    },
}

/// A descriptor binding and what it gets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bound {
    /// Binding number (from the shader).
    pub binding: u32,
    /// What it gets.
    pub resource: Resource,
}

/// The draw call itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// Non-indexed.
    Draw {
        /// Vertices per instance.
        vertices: u32,
        /// Instances.
        instances: u32,
        /// First vertex.
        first_vertex: u32,
        /// First instance.
        first_instance: u32,
    },
    /// Indexed.
    Indexed {
        /// Indices per instance.
        indices: u32,
        /// Instances.
        instances: u32,
        /// First index.
        first_index: u32,
        /// Added to each index.
        base_vertex: i32,
        /// First instance.
        first_instance: u32,
    },
    /// Arguments read from a buffer surface at a byte offset: those of
    /// `Draw`, or of `Indexed` if `indexed` (Direct3D's indirect argument
    /// layouts are Vulkan's).
    Indirect {
        /// Buffer surface.
        sid: u32,
        /// Byte offset.
        offset: u64,
        /// Indexed draw.
        indexed: bool,
    },
}

/// The attachments of an open rendering: a draw with the same ones (that
/// reads none of them and counts no occlusion) continues it, without a
/// barrier or a new rendering. Anything else recorded ends it first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pass {
    colors: Vec<vk::ImageView>,
    depth: vk::ImageView,
    extent: (u32, u32),
    /// Surfaces of the attachments.
    sids: Vec<u32>,
}

/// Everything a draw uses.
#[derive(Clone, Debug)]
pub struct Draw<'a> {
    /// Vertex shader.
    pub vs: &'a Shader,
    /// Pixel shader (`None`: depth only).
    pub ps: Option<&'a Shader>,
    /// Geometry shader.
    pub gs: Option<&'a Shader>,
    /// Input layout.
    pub attributes: Vec<VertexAttribute>,
    /// Vertex buffers of the slots the attributes use.
    pub buffers: Vec<VertexBuffer>,
    /// Index buffer: surface, `R16_UINT` or `R32_UINT`, byte offset.
    pub index: Option<(u32, u32, u64)>,
    /// `SVGA3dPrimitiveType`.
    pub topology: u32,
    /// Rasterizer state.
    pub rasterizer: RasterizerState,
    /// Depth-stencil state and stencil reference.
    pub depth_stencil: (DepthStencilState, u32),
    /// Blend state, blend factor and sample mask.
    pub blend: (BlendState, [f32; 4], u32),
    /// Render targets, by slot.
    pub targets: [Option<Target>; 8],
    /// Depth-stencil target.
    pub depth: Option<Target>,
    /// Viewports.
    pub viewports: Vec<Viewport>,
    /// Scissor rectangles.
    pub scissors: Vec<ScissorRect>,
    /// Descriptors, for both stages.
    pub resources: Vec<Bound>,
    /// Constant data of [`Resource::Data`] bindings.
    pub data: Vec<Vec<u8>>,
    /// Occlusion query slot counting the draw's samples
    /// ([`Renderer::occlusion_slot`]).
    pub occlusion: Option<u32>,
    /// The call.
    pub call: Call,
}

/// A descriptor set layout binding: number, descriptor type, stages (raw
/// Vulkan values).
type LayoutBinding = (u32, i32, u32);

/// Cached Vulkan objects of draws.
#[derive(Default)]
pub(crate) struct Caches {
    modules: HashMap<u64, vk::ShaderModule>,
    layouts: HashMap<Vec<LayoutBinding>, (vk::DescriptorSetLayout, vk::PipelineLayout)>,
    /// Graphics pipelines by their whole state; compute ones by
    /// [`crate::compute`]'s shorter key.
    pub(crate) pipelines: HashMap<Vec<u8>, vk::Pipeline>,
    samplers: HashMap<Vec<u8>, vk::Sampler>,
    /// Null resources: a zeroed buffer, zeroed images and their views.
    null: Option<Null>,
}

pub(crate) struct Null {
    buffer: vk::Buffer,
    /// A view of `buffer` (`R32G32B32A32_SFLOAT`).
    texel_buffer: vk::BufferView,
    /// Where writes to unbound UA views go (never read as zeros).
    sink: vk::Buffer,
    /// A storable view of `sink` (`R32_UINT`).
    sink_texel: vk::BufferView,
    memory: Vec<vk::DeviceMemory>,
    images: Vec<vk::Image>,
    /// By `VkImageViewType` raw value.
    views: HashMap<i32, vk::ImageView>,
    /// Storage images for unbound typed UA views, by view type.
    storage_views: HashMap<i32, vk::ImageView>,
}

/// A shader's hash: the one computed at translation, else its code's.
fn shader_hash(shader: &Shader) -> u64 {
    if shader.hash != 0 {
        shader.hash
    } else {
        hash(&shader.spirv)
    }
}

fn hash(words: &[u32]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        (h ^ u64::from(w)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Fixed state as bytes: the pipeline cache key.
#[derive(Default)]
struct Key(Vec<u8>);

impl Key {
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
}

fn blend_factor(f: u8) -> vk::BlendFactor {
    use vk::BlendFactor as B;
    // `D3D10_BLEND` (= `SVGA3dBlendOp`); color factors read alpha in alpha
    // blending, as in Vulkan.
    match f {
        1 => B::ZERO,
        3 => B::SRC_COLOR,
        4 => B::ONE_MINUS_SRC_COLOR,
        5 | 12 => B::SRC_ALPHA,
        6 | 13 => B::ONE_MINUS_SRC_ALPHA,
        7 => B::DST_ALPHA,
        8 => B::ONE_MINUS_DST_ALPHA,
        9 => B::DST_COLOR,
        10 => B::ONE_MINUS_DST_COLOR,
        11 => B::SRC_ALPHA_SATURATE,
        14 => B::CONSTANT_COLOR,
        15 => B::ONE_MINUS_CONSTANT_COLOR,
        16 => B::SRC1_COLOR,
        17 => B::ONE_MINUS_SRC1_COLOR,
        18 => B::SRC1_ALPHA,
        19 => B::ONE_MINUS_SRC1_ALPHA,
        20 => B::CONSTANT_ALPHA,
        21 => B::ONE_MINUS_CONSTANT_ALPHA,
        _ => B::ONE,
    }
}

/// `SVGA3D_BLENDEQ_*`, `SVGA3D_COMPARISON_*`, `SVGA3D_STENCILOP_*`,
/// `SVGA3D_CULL_*` and `SVGA3D_TEX_ADDRESS_*` are their Vulkan values plus
/// one; 0 (invalid) takes `default`.
fn minus_one(value: u8, default: i32) -> i32 {
    if value == 0 {
        default
    } else {
        i32::from(value).saturating_sub(1)
    }
}

fn topology(t: u32) -> Option<(vk::PrimitiveTopology, bool)> {
    use vk::PrimitiveTopology as P;
    // (topology, primitive restart: strips)
    Some(match t {
        1 => (P::TRIANGLE_LIST, false),
        2 => (P::POINT_LIST, false),
        3 => (P::LINE_LIST, false),
        4 => (P::LINE_STRIP, true),
        5 => (P::TRIANGLE_STRIP, true),
        6 => (P::TRIANGLE_FAN, false),
        7 => (P::LINE_LIST_WITH_ADJACENCY, false),
        8 => (P::LINE_STRIP_WITH_ADJACENCY, true),
        9 => (P::TRIANGLE_LIST_WITH_ADJACENCY, false),
        10 => (P::TRIANGLE_STRIP_WITH_ADJACENCY, true),
        _ => return None,
    })
}

/// Direct3D's rasterizer state when none is bound.
pub fn default_rasterizer() -> RasterizerState {
    RasterizerState {
        fill_mode: 3,
        cull_mode: 3,
        front_ccw: false,
        provoking_vertex_last: false,
        depth_bias: 0,
        depth_bias_clamp: 0.0,
        slope_scaled_depth_bias: 0.0,
        depth_clip: true,
        scissor: false,
        multisample: 0,
        antialiased_line: false,
        line_width: 1.0,
        line_stipple: false,
        line_stipple_factor: 0,
        line_stipple_pattern: 0,
        forced_sample_count: 0,
    }
}

/// Direct3D's depth-stencil state when none is bound: depth test (less)
/// and writes, no stencil.
pub fn default_depth_stencil() -> DepthStencilState {
    let face = StencilFace {
        fail_op: 1,
        depth_fail_op: 1,
        pass_op: 1,
        func: 8,
    };
    DepthStencilState {
        depth_enable: true,
        depth_write_mask: 1,
        depth_func: 2,
        stencil_enable: false,
        front_enable: true,
        back_enable: true,
        stencil_read_mask: 0xff,
        stencil_write_mask: 0xff,
        front: face,
        back: face,
    }
}

/// Direct3D's blend state when none is bound: no blending, all channels
/// written.
pub fn default_blend() -> BlendState {
    let target = BlendTarget {
        enable: false,
        src: 2,
        dest: 1,
        op: 1,
        src_alpha: 2,
        dest_alpha: 1,
        op_alpha: 1,
        write_mask: 0xf,
        logic_op_enable: false,
        logic_op: 0,
    };
    BlendState {
        alpha_to_coverage: false,
        independent: false,
        targets: [target; 8],
    }
}

impl Renderer {
    pub(crate) fn module(&mut self, shader: &Shader) -> Result<vk::ShaderModule, Error> {
        let key = shader_hash(shader);
        if let Some(m) = self.draw.modules.get(&key) {
            return Ok(*m);
        }
        let info = vk::ShaderModuleCreateInfo::default().code(&shader.spirv);
        // SAFETY: SPIR-V from the translator (validated in its tests).
        let module = unsafe { self.device.create_shader_module(&info, None) }?;
        self.draw.modules.insert(key, module);
        Ok(module)
    }

    fn layout(
        &mut self,
        draw: &Draw<'_>,
    ) -> Result<(vk::DescriptorSetLayout, vk::PipelineLayout), Error> {
        let stages: Vec<(&Shader, vk::ShaderStageFlags)> =
            std::iter::once((draw.vs, vk::ShaderStageFlags::VERTEX))
                .chain(draw.gs.map(|gs| (gs, vk::ShaderStageFlags::GEOMETRY)))
                .chain(draw.ps.map(|ps| (ps, vk::ShaderStageFlags::FRAGMENT)))
                .collect();
        self.layout_for(&stages)
    }

    /// The descriptor set and pipeline layouts of shaders, by stage.
    pub(crate) fn layout_for(
        &mut self,
        stages: &[(&Shader, vk::ShaderStageFlags)],
    ) -> Result<(vk::DescriptorSetLayout, vk::PipelineLayout), Error> {
        let mut bindings: Vec<LayoutBinding> = Vec::new();
        for (shader, stage) in stages {
            for b in &shader.bindings {
                let ty = descriptor_type(b.kind);
                bindings.push((b.binding, ty.as_raw(), stage.as_raw()));
            }
        }
        bindings.sort_unstable();
        if let Some(l) = self.draw.layouts.get(&bindings) {
            return Ok(*l);
        }
        let descs: Vec<vk::DescriptorSetLayoutBinding<'_>> = bindings
            .iter()
            .map(|&(binding, ty, stages)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(binding)
                    .descriptor_type(vk::DescriptorType::from_raw(ty))
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::from_raw(stages))
            })
            .collect();
        let info = vk::DescriptorSetLayoutCreateInfo::default()
            .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR)
            .bindings(&descs);
        // SAFETY: valid create infos on a live device.
        let set = unsafe { self.device.create_descriptor_set_layout(&info, None) }?;
        let sets = [set];
        // SAFETY: as above.
        let layout = unsafe {
            self.device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&sets),
                None,
            )
        }?;
        self.draw.layouts.insert(bindings, (set, layout));
        Ok((set, layout))
    }

    fn view_format(&self, format: u32) -> Result<(vk::Format, vk::ImageAspectFlags), Error> {
        match format::kind(format, self.d24s8, self.a8) {
            Some(Kind::Color(f)) => Ok((f, vk::ImageAspectFlags::COLOR)),
            Some(Kind::Depth(f) | Kind::DepthConverted(f)) => {
                let aspect = match f {
                    vk::Format::D16_UNORM | vk::Format::D32_SFLOAT => vk::ImageAspectFlags::DEPTH,
                    _ => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
                };
                Ok((f, aspect))
            }
            _ => err(format!("view format {format} has no GPU format")),
        }
    }

    /// An image view of a surface (cached with the surface).
    #[allow(clippy::too_many_arguments)]
    fn image_view(
        &mut self,
        sid: u32,
        format: vk::Format,
        aspect: vk::ImageAspectFlags,
        view_type: vk::ImageViewType,
        mips: (u32, u32),
        layers: (u32, u32),
        swizzle: vk::ComponentMapping,
    ) -> Result<vk::ImageView, Error> {
        let surface = self
            .surfaces
            .get(&sid)
            .ok_or_else(|| Error(format!("surface {sid} is not on the GPU")))?;
        let Storage::Image {
            image,
            format: stored,
            aspect: stored_aspect,
            usage,
            ..
        } = surface.resource
        else {
            return err(format!("surface {sid} is a buffer"));
        };
        // A color view of a depth image (`R32_FLOAT` of a `D32_FLOAT`
        // texture) reads its depth.
        let (format, aspect) = if stored_aspect.contains(vk::ImageAspectFlags::DEPTH)
            && aspect == vk::ImageAspectFlags::COLOR
        {
            (stored, vk::ImageAspectFlags::DEPTH)
        } else {
            (format, aspect)
        };
        let total_mips = surface.mips;
        let total_layers = surface.layout.layers();
        let first_mip = mips.0.min(total_mips.saturating_sub(1));
        let mip_count = mips.1.min(total_mips.saturating_sub(first_mip)).max(1);
        let (first_layer, layer_count) = if surface.layout.size().depth > 1 {
            (0, 1)
        } else {
            let first = layers.0.min(total_layers.saturating_sub(1));
            (
                first,
                layers.1.min(total_layers.saturating_sub(first)).max(1),
            )
        };
        let key: ViewKey = (
            format.as_raw(),
            aspect.as_raw(),
            view_type.as_raw(),
            [first_mip, mip_count, first_layer, layer_count],
            [
                swizzle.r.as_raw(),
                swizzle.g.as_raw(),
                swizzle.b.as_raw(),
                swizzle.a.as_raw(),
            ],
        );
        if let Some(v) = surface.views.get(&key) {
            return Ok(*v);
        }
        // A storage image's views inherit that usage, which formats that
        // cannot be stored (sRGB) refuse: such views leave it out.
        let storable = usage.contains(vk::ImageUsageFlags::STORAGE) && {
            // SAFETY: a device of this instance.
            let features = unsafe {
                self.instance
                    .get_physical_device_format_properties(self.physical, format)
            };
            features
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::STORAGE_IMAGE)
        };
        let mut view_usage =
            vk::ImageViewUsageCreateInfo::default().usage(usage & !vk::ImageUsageFlags::STORAGE);
        let mut info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(view_type)
            .format(format)
            .components(swizzle)
            .subresource_range(range(
                aspect,
                first_mip,
                mip_count,
                first_layer,
                layer_count,
            ));
        if usage.contains(vk::ImageUsageFlags::STORAGE) && !storable {
            info = info.push_next(&mut view_usage);
        }
        // SAFETY: a live image of this device; mutable format images accept
        // views of their class.
        let view = unsafe { self.device.create_image_view(&info, None) }?;
        if let Some(s) = self.surfaces.get_mut(&sid) {
            s.views.insert(key, view);
        }
        Ok(view)
    }

    /// A view of `size` bytes of buffer surface `sid` from `offset`, as
    /// elements of `format` (cached with the surface).
    fn buffer_view(
        &mut self,
        sid: u32,
        format: u32,
        offset: u64,
        size: u64,
    ) -> Result<vk::BufferView, Error> {
        let Some(Kind::Color(vk_format)) = format::kind(format, false, false) else {
            return err(format!("buffer view format {format}"));
        };
        let (buffer, total) = self.buffer(sid)?;
        // SAFETY: a device of this instance.
        let features = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, vk_format)
        }
        .buffer_features;
        if !features.contains(vk::FormatFeatureFlags::UNIFORM_TEXEL_BUFFER) {
            return err(format!("{vk_format:?} texel buffers on {}", self.name));
        }
        let element = u64::from(svga_proto::format::desc(format).map_or(0, |d| d.bytes_per_block));
        // Whole elements within the buffer.
        let offset = offset.min(total);
        let size = size.min(total.saturating_sub(offset));
        let size = size
            .checked_div(element)
            .unwrap_or(0)
            .saturating_mul(element);
        if size == 0 {
            return Ok(self.null()?.texel_buffer);
        }
        let key = (vk_format.as_raw(), offset, size);
        if let Some(v) = self.surface(sid)?.buffer_views.get(&key) {
            return Ok(*v);
        }
        let info = vk::BufferViewCreateInfo::default()
            .buffer(buffer)
            .format(vk_format)
            .offset(offset)
            .range(size);
        // SAFETY: a live buffer with texel buffer usage; the range lies
        // within it.
        let view = unsafe { self.device.create_buffer_view(&info, None) }?;
        if let Some(s) = self.surfaces.get_mut(&sid) {
            s.buffer_views.insert(key, view);
        }
        Ok(view)
    }

    fn sampler(&mut self, s: &SamplerState) -> Result<vk::Sampler, Error> {
        let mut key = Key::default();
        key.u32(s.filter)
            .u32(u32::from_le_bytes([
                s.address[0],
                s.address[1],
                s.address[2],
                s.comparison,
            ]))
            .u32(s.mip_lod_bias.to_bits())
            .u32(u32::from(s.max_anisotropy));
        for c in s.border_color {
            key.u32(c.to_bits());
        }
        key.u32(s.min_lod.to_bits()).u32(s.max_lod.to_bits());
        if let Some(v) = self.draw.samplers.get(&key.0) {
            return Ok(*v);
        }
        let filter = |bit: u32| {
            if s.filter & bit != 0 {
                vk::Filter::LINEAR
            } else {
                vk::Filter::NEAREST
            }
        };
        let mirror_once = self.features.mirror_once;
        let address = |a: u8| match a {
            5 if !mirror_once => vk::SamplerAddressMode::MIRRORED_REPEAT,
            _ => vk::SamplerAddressMode::from_raw(minus_one(a, 0)),
        };
        let anisotropic = s.filter & (1 << 6) != 0 && self.features.anisotropy;
        let compare = s.filter & (1 << 7) != 0;
        // The nearest standard border color (custom ones need an extension).
        let border = match s.border_color {
            [r, g, b, a] if r == 0.0 && g == 0.0 && b == 0.0 && a == 0.0 => {
                vk::BorderColor::FLOAT_TRANSPARENT_BLACK
            }
            [r, g, b, _] if r >= 0.5 && g >= 0.5 && b >= 0.5 => vk::BorderColor::FLOAT_OPAQUE_WHITE,
            _ => vk::BorderColor::FLOAT_OPAQUE_BLACK,
        };
        let info = vk::SamplerCreateInfo::default()
            .mag_filter(filter(1 << 2))
            .min_filter(filter(1 << 4))
            .mipmap_mode(if s.filter & 1 != 0 {
                vk::SamplerMipmapMode::LINEAR
            } else {
                vk::SamplerMipmapMode::NEAREST
            })
            .address_mode_u(address(s.address[0]))
            .address_mode_v(address(s.address[1]))
            .address_mode_w(address(s.address[2]))
            .mip_lod_bias(s.mip_lod_bias.clamp(-15.99, 15.99))
            .anisotropy_enable(anisotropic)
            .max_anisotropy(f32::from(s.max_anisotropy.clamp(1, 16)))
            .compare_enable(compare)
            .compare_op(vk::CompareOp::from_raw(minus_one(s.comparison, 7)))
            .min_lod(s.min_lod.max(0.0))
            .max_lod(if s.max_lod.is_nan() {
                1000.0
            } else {
                s.max_lod.max(s.min_lod.max(0.0))
            })
            .border_color(border);
        // SAFETY: a valid create info on a live device.
        let sampler = unsafe { self.device.create_sampler(&info, None) }?;
        self.draw.samplers.insert(key.0, sampler);
        Ok(sampler)
    }

    /// The null resources, created on first use.
    fn null(&mut self) -> Result<&Null, Error> {
        if self.draw.null.is_none() {
            let null = self.create_null()?;
            self.draw.null = Some(null);
        }
        self.draw
            .null
            .as_ref()
            .ok_or_else(|| Error("no null resources".into()))
    }

    fn create_null(&mut self) -> Result<Null, Error> {
        use vk::ImageViewType as T;
        let Storage::Buffer { buffer, memory, .. } = self.create_buffer(65_536)? else {
            return err("null buffer");
        };
        let info = vk::BufferViewCreateInfo::default()
            .buffer(buffer)
            .format(vk::Format::R32G32B32A32_SFLOAT)
            .range(vk::WHOLE_SIZE);
        // SAFETY: a view of a live buffer with texel buffer usage.
        let texel_buffer = unsafe { self.device.create_buffer_view(&info, None) }?;
        let Storage::Buffer {
            buffer: sink,
            memory: sink_memory,
            ..
        } = self.create_buffer(65_536)?
        else {
            return err("null UA view buffer");
        };
        let info = vk::BufferViewCreateInfo::default()
            .buffer(sink)
            .format(vk::Format::R32_UINT)
            .range(vk::WHOLE_SIZE);
        // SAFETY: a view of a live buffer with storage texel buffer usage.
        let sink_texel = unsafe { self.device.create_buffer_view(&info, None) }?;
        let mut null = Null {
            buffer,
            texel_buffer,
            sink,
            sink_texel,
            memory: vec![memory, sink_memory],
            images: Vec::new(),
            views: HashMap::new(),
            storage_views: HashMap::new(),
        };
        let cmd = self.cmd()?;
        // SAFETY: recording; the whole live buffer.
        unsafe {
            self.device
                .cmd_fill_buffer(cmd, buffer, 0, vk::WHOLE_SIZE, 0)
        };
        let shapes = [
            (
                vk::ImageType::TYPE_1D,
                vk::ImageCreateFlags::empty(),
                1u32,
                &[T::TYPE_1D, T::TYPE_1D_ARRAY][..],
            ),
            (
                vk::ImageType::TYPE_2D,
                vk::ImageCreateFlags::CUBE_COMPATIBLE,
                6,
                &[T::TYPE_2D, T::TYPE_2D_ARRAY, T::CUBE, T::CUBE_ARRAY][..],
            ),
            (
                vk::ImageType::TYPE_3D,
                vk::ImageCreateFlags::empty(),
                1,
                &[T::TYPE_3D][..],
            ),
        ];
        // Sampled images read as zeros, then storage images for unbound
        // UA views (apart: writes must not reach the zeros).
        let kinds = [
            (vk::ImageUsageFlags::SAMPLED, false),
            (vk::ImageUsageFlags::STORAGE, true),
        ];
        for ((ty, flags, layers, view_types), (usage, storage)) in kinds
            .iter()
            .flat_map(|k| shapes.iter().map(move |s| (*s, *k)))
        {
            let info = vk::ImageCreateInfo::default()
                .flags(flags)
                .image_type(ty)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width: 1,
                    height: 1,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(layers)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage | vk::ImageUsageFlags::TRANSFER_DST);
            // SAFETY: a valid create info on a live device.
            let image = unsafe { self.device.create_image(&info, None) }?;
            null.images.push(image);
            // SAFETY: an image of this device.
            let requirements = unsafe { self.device.get_image_memory_requirements(image) };
            let memory = self.allocate(requirements, &[vk::MemoryPropertyFlags::DEVICE_LOCAL])?;
            null.memory.push(memory);
            // SAFETY: fresh memory sized for the image.
            unsafe { self.device.bind_image_memory(image, memory, 0) }?;
            let whole = range(vk::ImageAspectFlags::COLOR, 0, 1, 0, layers);
            let to_general = vk::ImageMemoryBarrier::default()
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(whole);
            // SAFETY: recording; the image is live.
            unsafe {
                self.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_general],
                );
                self.device.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::GENERAL,
                    &vk::ClearColorValue { float32: [0.0; 4] },
                    &[whole],
                );
            }
            for view_type in view_types {
                let layer_count = match *view_type {
                    T::CUBE | T::CUBE_ARRAY => 6,
                    _ => 1,
                };
                let info = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(*view_type)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .subresource_range(range(vk::ImageAspectFlags::COLOR, 0, 1, 0, layer_count));
                // SAFETY: a view of a live image within its range.
                let view = unsafe { self.device.create_image_view(&info, None) }?;
                if storage {
                    null.storage_views.insert(view_type.as_raw(), view);
                } else {
                    null.views.insert(view_type.as_raw(), view);
                }
            }
        }
        Ok(null)
    }

    /// Records `draw`. Every surface it names must be on the GPU.
    pub fn draw(&mut self, draw: &Draw<'_>) -> Result<(), Error> {
        if !self.features.draw {
            return err(format!(
                "{} cannot draw (no push descriptors or dynamic rendering)",
                self.name
            ));
        }
        let Some((vk_topology, restart)) = topology(draw.topology) else {
            return err(format!("topology {}", draw.topology));
        };
        // Many draws go to the GPU in pieces, so that it works while the
        // rest is recorded (before anything of this draw is).
        if self.draws >= super::DRAWS_PER_SUBMISSION {
            self.flush()?;
        }
        self.draws = self.draws.saturating_add(1);
        // Attachments and the render area.
        let mut extent = (u32::MAX, u32::MAX);
        let mut samples = 1u32;
        let mut color_formats = [vk::Format::UNDEFINED; 8];
        let mut color_views = [vk::ImageView::null(); 8];
        let used_targets = draw
            .targets
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |i| i.saturating_add(1));
        for (slot, target) in draw.targets.iter().enumerate().take(used_targets) {
            let Some(t) = target else { continue };
            let (f, aspect) = self.view_format(t.format)?;
            let layout = &self.surface(t.sid)?.layout;
            let size = layout.mip_size(t.mip);
            extent = (extent.0.min(size.width), extent.1.min(size.height));
            samples = layout.samples();
            color_formats[slot] = f;
            color_views[slot] = self.image_view(
                t.sid,
                f,
                aspect,
                vk::ImageViewType::TYPE_2D,
                (t.mip, 1),
                (t.first_layer, 1),
                vk::ComponentMapping::default(),
            )?;
        }
        let mut depth_format = vk::Format::UNDEFINED;
        let mut depth_view = vk::ImageView::null();
        let mut depth_aspect = vk::ImageAspectFlags::empty();
        if let Some(t) = draw.depth {
            let (f, aspect) = self.view_format(t.format)?;
            let layout = &self.surface(t.sid)?.layout;
            let size = layout.mip_size(t.mip);
            extent = (extent.0.min(size.width), extent.1.min(size.height));
            samples = layout.samples();
            depth_format = f;
            depth_aspect = aspect;
            depth_view = self.image_view(
                t.sid,
                f,
                aspect,
                vk::ImageViewType::TYPE_2D,
                (t.mip, 1),
                (t.first_layer, 1),
                vk::ComponentMapping::default(),
            )?;
        }
        if extent.0 == u32::MAX {
            return err("draw without targets");
        }
        let (_, pipeline_layout) = self.layout(draw)?;
        let pipeline = self.pipeline(
            draw,
            pipeline_layout,
            vk_topology,
            restart,
            &color_formats[..used_targets],
            depth_format,
            depth_aspect,
            samples,
        )?;
        let writes = self.descriptors(draw)?;
        // Record, continuing the open rendering when possible.
        let pass = Pass {
            colors: color_views[..used_targets].to_vec(),
            depth: depth_view,
            extent,
            sids: draw
                .targets
                .iter()
                .flatten()
                .chain(draw.depth.iter())
                .map(|t| t.sid)
                .collect(),
        };
        let reads_attachment = draw.resources.iter().any(|b| match b.resource {
            Resource::Texture { sid, .. } | Resource::TexelBuffer { sid, .. } => {
                pass.sids.contains(&sid)
            }
            _ => false,
        });
        let continues = self.pass.as_ref() == Some(&pass)
            && self.recording
            && !reads_attachment
            && draw.occlusion.is_none();
        let cmd = if continues { self.cmd } else { self.barrier()? };
        let colors: Vec<vk::RenderingAttachmentInfo<'_>> = color_views[..used_targets]
            .iter()
            .map(|v| {
                vk::RenderingAttachmentInfo::default()
                    .image_view(*v)
                    .image_layout(vk::ImageLayout::GENERAL)
                    .load_op(vk::AttachmentLoadOp::LOAD)
                    .store_op(vk::AttachmentStoreOp::STORE)
            })
            .collect();
        let depth_attachment = vk::RenderingAttachmentInfo::default()
            .image_view(depth_view)
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE);
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D {
                width: extent.0.max(1),
                height: extent.1.max(1),
            },
        };
        let mut rendering = vk::RenderingInfo::default()
            .render_area(area)
            .layer_count(1)
            .color_attachments(&colors);
        if depth_view != vk::ImageView::null() {
            if depth_aspect.contains(vk::ImageAspectFlags::DEPTH) {
                rendering = rendering.depth_attachment(&depth_attachment);
            }
            if depth_aspect.contains(vk::ImageAspectFlags::STENCIL) {
                rendering = rendering.stencil_attachment(&depth_attachment);
            }
        }
        // The first viewport (others need a geometry shader to select
        // them). Direct3D's y axis points up: flip it.
        let v = draw.viewports.first().copied().unwrap_or(Viewport {
            x: 0.0,
            y: 0.0,
            width: extent.0 as f32,
            height: extent.1 as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        });
        if !(v.width > 0.0 && v.height > 0.0) {
            return Ok(());
        }
        let viewports = [vk::Viewport {
            x: v.x,
            y: v.y + v.height,
            width: v.width,
            height: -v.height,
            min_depth: v.min_depth.clamp(0.0, 1.0),
            max_depth: v.max_depth.clamp(0.0, 1.0),
        }];
        let scissors = [match draw.scissors.first() {
            Some(s) if draw.rasterizer.scissor => {
                let x = s.left.clamp(0, i32::MAX / 2);
                let y = s.top.clamp(0, i32::MAX / 2);
                vk::Rect2D {
                    offset: vk::Offset2D { x, y },
                    extent: vk::Extent2D {
                        width: u32::try_from(s.right.min(i32::MAX / 2).saturating_sub(x))
                            .unwrap_or(0),
                        height: u32::try_from(s.bottom.min(i32::MAX / 2).saturating_sub(y))
                            .unwrap_or(0),
                    },
                }
            }
            _ => area,
        }];
        let push = self
            .push
            .as_ref()
            .ok_or_else(|| Error("no push descriptors".into()))?;
        let null_buffer = self.draw.null.as_ref().map(|n| n.buffer);
        let occlusion = match (draw.occlusion, self.occlusion) {
            (Some(slot), Some((pool, _))) => Some((pool, slot)),
            _ => None,
        };
        let precise = if self.features.precise_occlusion {
            vk::QueryControlFlags::PRECISE
        } else {
            vk::QueryControlFlags::empty()
        };
        // SAFETY: recording; every object is live and matches the pipeline
        // (formats, bindings, vertex inputs).
        unsafe {
            if !continues {
                if let Some((pool, slot)) = occlusion {
                    // Outside the rendering, as Vulkan requires.
                    self.device.cmd_reset_query_pool(cmd, pool, slot, 1);
                }
                self.device.cmd_begin_rendering(cmd, &rendering);
            }
            // Open from here: whatever happens next ends it properly.
            self.pass = Some(pass.clone());
            if let Some((pool, slot)) = occlusion {
                self.device.cmd_begin_query(cmd, pool, slot, precise);
            }
            self.device
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
            self.device.cmd_set_viewport_with_count(cmd, &viewports);
            self.device.cmd_set_scissor_with_count(cmd, &scissors);
            self.device.cmd_set_blend_constants(cmd, &draw.blend.1);
            self.device.cmd_set_stencil_reference(
                cmd,
                vk::StencilFaceFlags::FRONT_AND_BACK,
                draw.depth_stencil.1 & 0xff,
            );
            if !writes.is_empty() {
                push.cmd_push_descriptor_set(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline_layout,
                    0,
                    &writes.iter().map(Write::as_vk).collect::<Vec<_>>(),
                );
            }
            for vb in &draw.buffers {
                let (buffer, offset, size) = match vb.sid.and_then(|sid| self.buffer(sid).ok()) {
                    Some((b, total)) if vb.offset < total => {
                        (b, vb.offset, vb.size.min(total.saturating_sub(vb.offset)))
                    }
                    _ => (
                        null_buffer.ok_or_else(|| Error("no null buffer".into()))?,
                        0,
                        vk::WHOLE_SIZE,
                    ),
                };
                self.device.cmd_bind_vertex_buffers2(
                    cmd,
                    vb.binding,
                    &[buffer],
                    &[offset],
                    Some(&[size]),
                    Some(&[u64::from(vb.stride)]),
                );
            }
            match draw.call {
                Call::Draw {
                    vertices,
                    instances,
                    first_vertex,
                    first_instance,
                } => self
                    .device
                    .cmd_draw(cmd, vertices, instances, first_vertex, first_instance),
                Call::Indirect {
                    sid,
                    offset,
                    indexed: false,
                } => {
                    let (buffer, _) = self.buffer(sid)?;
                    self.device.cmd_draw_indirect(cmd, buffer, offset, 1, 0);
                }
                Call::Indexed { .. } | Call::Indirect { indexed: true, .. } => {
                    let Some((sid, format, offset)) = draw.index else {
                        if let Some((pool, slot)) = occlusion {
                            self.device.cmd_end_query(cmd, pool, slot);
                        }
                        self.device.cmd_end_rendering(cmd);
                        self.pass = None;
                        return err("indexed draw without an index buffer");
                    };
                    let (buffer, _) = self.buffer(sid)?;
                    let ty = if format == svga_proto::format::R16_UINT {
                        vk::IndexType::UINT16
                    } else {
                        vk::IndexType::UINT32
                    };
                    self.device.cmd_bind_index_buffer(cmd, buffer, offset, ty);
                    match draw.call {
                        Call::Indexed {
                            indices,
                            instances,
                            first_index,
                            base_vertex,
                            first_instance,
                        } => self.device.cmd_draw_indexed(
                            cmd,
                            indices,
                            instances,
                            first_index,
                            base_vertex,
                            first_instance,
                        ),
                        Call::Indirect { sid, offset, .. } => {
                            let (args, _) = self.buffer(sid)?;
                            self.device
                                .cmd_draw_indexed_indirect(cmd, args, offset, 1, 0);
                        }
                        Call::Draw { .. } => {}
                    }
                }
            }
            if let Some((pool, slot)) = occlusion {
                self.device.cmd_end_query(cmd, pool, slot);
                // A query's draw stands alone.
                self.device.cmd_end_rendering(cmd);
                self.pass = None;
            }
        }
        Ok(())
    }

    /// Descriptor writes of a draw, with null resources for empty slots.
    fn descriptors(&mut self, draw: &Draw<'_>) -> Result<Vec<Write>, Error> {
        let shaders: Vec<&Shader> = std::iter::once(draw.vs)
            .chain(draw.gs)
            .chain(draw.ps)
            .collect();
        self.descriptors_for(&shaders, &draw.resources, &draw.data)
    }

    /// The descriptor writes of shaders' bindings, from `resources`
    /// (`Resource::Data(n)`: `data[n]`).
    pub(crate) fn descriptors_for(
        &mut self,
        shaders: &[&Shader],
        resources: &[Bound],
        data: &[Vec<u8>],
    ) -> Result<Vec<Write>, Error> {
        self.null()?;
        let mut writes = Vec::new();
        let declared: Vec<svga_shader::Binding> = shaders
            .iter()
            .flat_map(|s| s.bindings.iter().copied())
            .collect();
        for b in declared {
            let given = resources
                .iter()
                .find(|r| r.binding == b.binding)
                .map_or(Resource::None, |r| r.resource);
            let write = match (b.kind, given) {
                (BindingKind::ConstantBuffer { .. }, Resource::Buffer { sid, offset, size }) => {
                    match self.buffer(sid) {
                        Ok((buffer, total)) if offset < total => Write::Buffer(
                            b.binding,
                            vk::DescriptorBufferInfo {
                                buffer,
                                offset,
                                range: size.min(total.saturating_sub(offset)).clamp(1, 65_536),
                            },
                        ),
                        _ => self.null_buffer(b.binding)?,
                    }
                }
                (BindingKind::ConstantBuffer { .. }, Resource::Data(n)) => {
                    match data.get(n).filter(|d| !d.is_empty()) {
                        Some(bytes) => {
                            let bytes = &bytes[..bytes.len().min(65_536)];
                            let (buffer, offset) = self.uniform_slice(bytes)?;
                            Write::Buffer(
                                b.binding,
                                vk::DescriptorBufferInfo {
                                    buffer,
                                    offset,
                                    range: bytes.len() as u64,
                                },
                            )
                        }
                        None => self.null_buffer(b.binding)?,
                    }
                }
                (BindingKind::ConstantBuffer { .. }, _) => self.null_buffer(b.binding)?,
                (BindingKind::Sampler { .. }, Resource::Sampler(s)) => {
                    Write::sampler(b.binding, self.sampler(&s)?)
                }
                (BindingKind::Sampler { .. }, _) => {
                    let s = default_sampler();
                    Write::sampler(b.binding, self.sampler(&s)?)
                }
                (
                    BindingKind::BufferResource { .. } | BindingKind::UaView { dimension: 0, .. },
                    given,
                ) => {
                    let ua = matches!(b.kind, BindingKind::UaView { .. });
                    let range = match given {
                        Resource::StorageBuffer { sid, offset, size } => self
                            .buffer(sid)
                            .ok()
                            .filter(|&(_, total)| offset < total)
                            .map(|(buffer, total)| vk::DescriptorBufferInfo {
                                buffer,
                                offset,
                                range: size.min(total.saturating_sub(offset)).max(4),
                            }),
                        _ => None,
                    };
                    let null = self.null()?;
                    let range = range.unwrap_or(vk::DescriptorBufferInfo {
                        buffer: if ua { null.sink } else { null.buffer },
                        offset: 0,
                        range: vk::WHOLE_SIZE,
                    });
                    Write::StorageBuffer(b.binding, range)
                }
                (BindingKind::UaView { dimension: 1, .. }, given) => {
                    let view = match given {
                        Resource::TexelBuffer {
                            sid,
                            format,
                            offset,
                            size,
                        } if self.buffer(sid).is_ok() => {
                            self.buffer_view(sid, format, offset, size)?
                        }
                        _ => self.null()?.sink_texel,
                    };
                    Write::StorageTexelBuffer(b.binding, view)
                }
                (BindingKind::UaView { dimension, .. }, given) => {
                    let view_type = match dimension {
                        2 => vk::ImageViewType::TYPE_1D,
                        5 => vk::ImageViewType::TYPE_3D,
                        7 => vk::ImageViewType::TYPE_1D_ARRAY,
                        8 => vk::ImageViewType::TYPE_2D_ARRAY,
                        _ => vk::ImageViewType::TYPE_2D,
                    };
                    let view = match given {
                        Resource::StorageImage {
                            sid,
                            format,
                            mip,
                            first_layer,
                            layers,
                        } if self.has(sid) => {
                            let (f, aspect) = self.view_format(format)?;
                            let layers = match view_type {
                                vk::ImageViewType::TYPE_1D
                                | vk::ImageViewType::TYPE_2D
                                | vk::ImageViewType::TYPE_3D => 1,
                                _ => layers,
                            };
                            self.image_view(
                                sid,
                                f,
                                aspect,
                                view_type,
                                (mip, 1),
                                (first_layer, layers),
                                vk::ComponentMapping::default(),
                            )?
                        }
                        _ => {
                            let null = self.null()?;
                            *null.storage_views.get(&view_type.as_raw()).ok_or_else(|| {
                                Error(format!("no null storage view {view_type:?}"))
                            })?
                        }
                    };
                    Write::StorageImage(
                        b.binding,
                        vk::DescriptorImageInfo::default()
                            .image_view(view)
                            .image_layout(vk::ImageLayout::GENERAL),
                    )
                }
                (BindingKind::Resource { dimension: 1, .. }, given) => {
                    let view = match given {
                        Resource::TexelBuffer {
                            sid,
                            format,
                            offset,
                            size,
                        } if self.buffer(sid).is_ok() => {
                            self.buffer_view(sid, format, offset, size)?
                        }
                        _ => self.null()?.texel_buffer,
                    };
                    Write::TexelBuffer(b.binding, view)
                }
                (BindingKind::Resource { dimension, .. }, given) => {
                    let view_type = match dimension {
                        2 => vk::ImageViewType::TYPE_1D,
                        5 => vk::ImageViewType::TYPE_3D,
                        6 => vk::ImageViewType::CUBE,
                        7 => vk::ImageViewType::TYPE_1D_ARRAY,
                        8 | 9 => vk::ImageViewType::TYPE_2D_ARRAY,
                        10 => vk::ImageViewType::CUBE_ARRAY,
                        _ => vk::ImageViewType::TYPE_2D,
                    };
                    let view = match given {
                        Resource::Texture {
                            sid,
                            format,
                            first_mip,
                            mips,
                            first_layer,
                            layers,
                        } if self.has(sid) => {
                            let (f, aspect) = self.view_format(format)?;
                            // Depth views read depth only.
                            let aspect = if aspect.contains(vk::ImageAspectFlags::DEPTH) {
                                vk::ImageAspectFlags::DEPTH
                            } else {
                                aspect
                            };
                            let layers = match view_type {
                                vk::ImageViewType::CUBE => 6,
                                vk::ImageViewType::TYPE_1D | vk::ImageViewType::TYPE_2D => 1,
                                _ => layers,
                            };
                            self.image_view(
                                sid,
                                f,
                                aspect,
                                view_type,
                                (first_mip, mips),
                                (first_layer, layers),
                                swizzle(format),
                            )?
                        }
                        _ => {
                            let null = self.null()?;
                            *null
                                .views
                                .get(&view_type.as_raw())
                                .ok_or_else(|| Error(format!("no null view {view_type:?}")))?
                        }
                    };
                    Write::image(b.binding, view)
                }
            };
            writes.push(write);
        }
        Ok(writes)
    }

    fn null_buffer(&mut self, binding: u32) -> Result<Write, Error> {
        let buffer = self.null()?.buffer;
        Ok(Write::Buffer(
            binding,
            vk::DescriptorBufferInfo {
                buffer,
                offset: 0,
                range: 65_536,
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn pipeline(
        &mut self,
        draw: &Draw<'_>,
        layout: vk::PipelineLayout,
        topology: vk::PrimitiveTopology,
        restart: bool,
        color_formats: &[vk::Format],
        depth_format: vk::Format,
        depth_aspect: vk::ImageAspectFlags,
        samples: u32,
    ) -> Result<vk::Pipeline, Error> {
        let vs = self.module(draw.vs)?;
        let ps = draw.ps.map(|ps| self.module(ps)).transpose()?;
        let gs = draw.gs.map(|gs| self.module(gs)).transpose()?;
        let r = &draw.rasterizer;
        let (ds, _) = &draw.depth_stencil;
        let (blend, _, sample_mask) = &draw.blend;
        // The key: everything fixed in the pipeline.
        let mut key = Key::default();
        key.u64(shader_hash(draw.vs))
            .u64(draw.ps.map_or(0, shader_hash))
            .u64(draw.gs.map_or(0, shader_hash))
            .u64(layout.as_raw())
            .u32(u32::try_from(topology.as_raw()).unwrap_or(0))
            .u32(u32::from(restart));
        for a in &draw.attributes {
            key.u32(a.location)
                .u32(a.binding)
                .u32(a.format)
                .u32(a.offset);
        }
        for b in &draw.buffers {
            key.u32(b.binding).u32(u32::from(b.per_instance));
        }
        key.u32(u32::from_le_bytes([
            r.fill_mode,
            r.cull_mode,
            u8::from(r.front_ccw),
            u8::from(r.depth_clip),
        ]))
        .u32(r.depth_bias as u32)
        .u32(r.depth_bias_clamp.to_bits())
        .u32(r.slope_scaled_depth_bias.to_bits())
        .u32(u32::from(r.provoking_vertex_last));
        key.u32(u32::from_le_bytes([
            u8::from(ds.depth_enable),
            ds.depth_write_mask,
            ds.depth_func,
            u8::from(ds.stencil_enable),
        ]))
        .u32(u32::from_le_bytes([
            ds.stencil_read_mask,
            ds.stencil_write_mask,
            0,
            0,
        ]));
        for f in [ds.front, ds.back] {
            key.u32(u32::from_le_bytes([
                f.fail_op,
                f.depth_fail_op,
                f.pass_op,
                f.func,
            ]));
        }
        key.u32(u32::from(blend.alpha_to_coverage) | u32::from(blend.independent) << 1)
            .u32(*sample_mask);
        for t in &blend.targets {
            key.u32(u32::from_le_bytes([
                u8::from(t.enable),
                t.src,
                t.dest,
                t.op,
            ]))
            .u32(u32::from_le_bytes([
                t.src_alpha,
                t.dest_alpha,
                t.op_alpha,
                t.write_mask,
            ]));
        }
        for f in color_formats {
            key.u32(u32::try_from(f.as_raw()).unwrap_or(0));
        }
        key.u32(u32::try_from(depth_format.as_raw()).unwrap_or(0))
            .u32(samples);
        if let Some(p) = self.draw.pipelines.get(&key.0) {
            return Ok(*p);
        }
        // Vertex input.
        let attributes: Vec<vk::VertexInputAttributeDescription> = draw
            .attributes
            .iter()
            .map(|a| -> Result<_, Error> {
                let Some(Kind::Color(format)) = format::kind(a.format, false, false) else {
                    return err(format!("vertex format {}", a.format));
                };
                Ok(vk::VertexInputAttributeDescription {
                    location: a.location,
                    binding: a.binding,
                    format,
                    offset: a.offset,
                })
            })
            .collect::<Result<_, _>>()?;
        let bindings: Vec<vk::VertexInputBindingDescription> = draw
            .buffers
            .iter()
            .map(|b| vk::VertexInputBindingDescription {
                binding: b.binding,
                stride: 0,
                input_rate: if b.per_instance {
                    vk::VertexInputRate::INSTANCE
                } else {
                    vk::VertexInputRate::VERTEX
                },
            })
            .collect();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(topology)
            .primitive_restart_enable(restart);
        let viewport = vk::PipelineViewportStateCreateInfo::default();
        let depth_bias = r.depth_bias != 0 || r.slope_scaled_depth_bias != 0.0;
        let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
            .depth_clamp_enable(!r.depth_clip && self.features.depth_clamp)
            .polygon_mode(match r.fill_mode {
                1 => vk::PolygonMode::POINT,
                2 => vk::PolygonMode::LINE,
                _ => vk::PolygonMode::FILL,
            })
            .cull_mode(vk::CullModeFlags::from_raw(
                u32::try_from(minus_one(r.cull_mode, 2)).unwrap_or(2),
            ))
            .front_face(if r.front_ccw {
                vk::FrontFace::COUNTER_CLOCKWISE
            } else {
                vk::FrontFace::CLOCKWISE
            })
            .depth_bias_enable(depth_bias)
            .depth_bias_constant_factor(r.depth_bias as f32)
            .depth_bias_clamp(r.depth_bias_clamp)
            .depth_bias_slope_factor(r.slope_scaled_depth_bias)
            .line_width(1.0);
        let sample_count = vk::SampleCountFlags::from_raw(samples.max(1));
        let masks = [*sample_mask];
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(sample_count)
            .sample_mask(&masks)
            .alpha_to_coverage_enable(blend.alpha_to_coverage);
        let stencil = |f: &svga_core::dx::StencilFace| vk::StencilOpState {
            fail_op: vk::StencilOp::from_raw(minus_one(f.fail_op, 0)),
            pass_op: vk::StencilOp::from_raw(minus_one(f.pass_op, 0)),
            depth_fail_op: vk::StencilOp::from_raw(minus_one(f.depth_fail_op, 0)),
            compare_op: vk::CompareOp::from_raw(minus_one(f.func, 7)),
            compare_mask: u32::from(ds.stencil_read_mask),
            write_mask: u32::from(ds.stencil_write_mask),
            reference: 0,
        };
        let has_depth = depth_aspect.contains(vk::ImageAspectFlags::DEPTH);
        let has_stencil = depth_aspect.contains(vk::ImageAspectFlags::STENCIL);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(ds.depth_enable && has_depth)
            .depth_write_enable(ds.depth_write_mask != 0 && has_depth)
            .depth_compare_op(vk::CompareOp::from_raw(minus_one(ds.depth_func, 1)))
            .stencil_test_enable(ds.stencil_enable && has_stencil)
            .front(stencil(&ds.front))
            .back(stencil(&ds.back));
        let attachments: Vec<vk::PipelineColorBlendAttachmentState> = (0..color_formats.len())
            .map(|i| {
                let t = if blend.independent {
                    blend.targets[i]
                } else {
                    blend.targets[0]
                };
                vk::PipelineColorBlendAttachmentState {
                    blend_enable: vk::Bool32::from(t.enable),
                    src_color_blend_factor: blend_factor(t.src),
                    dst_color_blend_factor: blend_factor(t.dest),
                    color_blend_op: vk::BlendOp::from_raw(minus_one(t.op, 0)),
                    src_alpha_blend_factor: blend_factor(t.src_alpha),
                    dst_alpha_blend_factor: blend_factor(t.dest_alpha),
                    alpha_blend_op: vk::BlendOp::from_raw(minus_one(t.op_alpha, 0)),
                    color_write_mask: vk::ColorComponentFlags::from_raw(u32::from(
                        t.write_mask & 0xf,
                    )),
                }
            })
            .collect();
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic = [
            vk::DynamicState::VIEWPORT_WITH_COUNT,
            vk::DynamicState::SCISSOR_WITH_COUNT,
            vk::DynamicState::BLEND_CONSTANTS,
            vk::DynamicState::STENCIL_REFERENCE,
            vk::DynamicState::VERTEX_INPUT_BINDING_STRIDE,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(color_formats)
            .depth_attachment_format(if has_depth {
                depth_format
            } else {
                vk::Format::UNDEFINED
            })
            .stencil_attachment_format(if has_stencil {
                depth_format
            } else {
                vk::Format::UNDEFINED
            });
        let mut stages = vec![
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs)
                .name(c"main"),
        ];
        if let Some(gs) = gs {
            stages.push(
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::GEOMETRY)
                    .module(gs)
                    .name(c"main"),
            );
        }
        if let Some(ps) = ps {
            stages.push(
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(ps)
                    .name(c"main"),
            );
        }
        let info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: a complete create info with live modules and layout.
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        }
        .map_err(|(_, e)| Error::from(e))?[0];
        self.draw.pipelines.insert(key.0, pipeline);
        Ok(pipeline)
    }

    /// Clears rectangles (x, y, width, height) of render targets, as
    /// Direct3D 9's `Clear` does: `color` in every color target, `depth`
    /// and `stencil` in the depth-stencil target when given.
    pub fn clear_rects(
        &mut self,
        targets: &[Option<Target>; 8],
        depth: Option<Target>,
        color: Option<[f32; 4]>,
        depth_stencil: (Option<f32>, Option<u8>),
        rects: &[(u32, u32, u32, u32)],
    ) -> Result<(), Error> {
        let mut extent = (u32::MAX, u32::MAX);
        let mut colors = Vec::new();
        let mut attachments = Vec::new();
        for t in targets.iter().flatten() {
            let (f, aspect) = self.view_format(t.format)?;
            let size = self.surface(t.sid)?.layout.mip_size(t.mip);
            extent = (extent.0.min(size.width), extent.1.min(size.height));
            let view = self.image_view(
                t.sid,
                f,
                aspect,
                vk::ImageViewType::TYPE_2D,
                (t.mip, 1),
                (t.first_layer, 1),
                vk::ComponentMapping::default(),
            )?;
            if let Some(c) = color {
                attachments.push(
                    vk::ClearAttachment::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .color_attachment(u32::try_from(colors.len()).unwrap_or(0))
                        .clear_value(vk::ClearValue {
                            color: format::clear_value(f, c),
                        }),
                );
            }
            colors.push(view);
        }
        let mut depth_view = vk::ImageView::null();
        let mut depth_aspect = vk::ImageAspectFlags::empty();
        if let Some(t) = depth {
            let (f, aspect) = self.view_format(t.format)?;
            let size = self.surface(t.sid)?.layout.mip_size(t.mip);
            extent = (extent.0.min(size.width), extent.1.min(size.height));
            depth_aspect = aspect;
            depth_view = self.image_view(
                t.sid,
                f,
                aspect,
                vk::ImageViewType::TYPE_2D,
                (t.mip, 1),
                (t.first_layer, 1),
                vk::ComponentMapping::default(),
            )?;
            let mut clear = vk::ImageAspectFlags::empty();
            if depth_stencil.0.is_some() && aspect.contains(vk::ImageAspectFlags::DEPTH) {
                clear |= vk::ImageAspectFlags::DEPTH;
            }
            if depth_stencil.1.is_some() && aspect.contains(vk::ImageAspectFlags::STENCIL) {
                clear |= vk::ImageAspectFlags::STENCIL;
            }
            if !clear.is_empty() {
                attachments.push(
                    vk::ClearAttachment::default()
                        .aspect_mask(clear)
                        .clear_value(vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: depth_stencil.0.unwrap_or(0.0).clamp(0.0, 1.0),
                                stencil: u32::from(depth_stencil.1.unwrap_or(0)),
                            },
                        }),
                );
            }
        }
        if extent.0 == u32::MAX || attachments.is_empty() {
            return Ok(());
        }
        let clip: Vec<vk::ClearRect> = rects
            .iter()
            .filter_map(|&(x, y, w, h)| {
                let x1 = x.saturating_add(w).min(extent.0);
                let y1 = y.saturating_add(h).min(extent.1);
                (x < x1 && y < y1).then(|| vk::ClearRect {
                    rect: vk::Rect2D {
                        offset: vk::Offset2D {
                            x: x as i32,
                            y: y as i32,
                        },
                        extent: vk::Extent2D {
                            width: x1.saturating_sub(x),
                            height: y1.saturating_sub(y),
                        },
                    },
                    base_array_layer: 0,
                    layer_count: 1,
                })
            })
            .collect();
        if clip.is_empty() {
            return Ok(());
        }
        let color_infos: Vec<vk::RenderingAttachmentInfo<'_>> = colors
            .iter()
            .map(|v| {
                vk::RenderingAttachmentInfo::default()
                    .image_view(*v)
                    .image_layout(vk::ImageLayout::GENERAL)
                    .load_op(vk::AttachmentLoadOp::LOAD)
                    .store_op(vk::AttachmentStoreOp::STORE)
            })
            .collect();
        let depth_info = vk::RenderingAttachmentInfo::default()
            .image_view(depth_view)
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE);
        let mut rendering = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: vk::Extent2D {
                    width: extent.0.max(1),
                    height: extent.1.max(1),
                },
            })
            .layer_count(1)
            .color_attachments(&color_infos);
        if depth_view != vk::ImageView::null() {
            if depth_aspect.contains(vk::ImageAspectFlags::DEPTH) {
                rendering = rendering.depth_attachment(&depth_info);
            }
            if depth_aspect.contains(vk::ImageAspectFlags::STENCIL) {
                rendering = rendering.stencil_attachment(&depth_info);
            }
        }
        let cmd = self.barrier()?;
        // SAFETY: recording; the views are live and the rectangles lie
        // within the render area.
        unsafe {
            self.device.cmd_begin_rendering(cmd, &rendering);
            self.device.cmd_clear_attachments(cmd, &attachments, &clip);
            self.device.cmd_end_rendering(cmd);
        }
        Ok(())
    }

    /// Clears a depth-stencil view: `flags` bit 1 depth, bit 2 stencil
    /// (`SVGA3D_CLEAR_*`).
    pub fn clear_depth_stencil(
        &mut self,
        target: Target,
        flags: u16,
        depth: f32,
        stencil: u8,
    ) -> Result<(), Error> {
        let (image, _, aspect, _) = self.image(target.sid)?;
        let mut clear = vk::ImageAspectFlags::empty();
        if flags & 2 != 0 && aspect.contains(vk::ImageAspectFlags::DEPTH) {
            clear |= vk::ImageAspectFlags::DEPTH;
        }
        if flags & 4 != 0 && aspect.contains(vk::ImageAspectFlags::STENCIL) {
            clear |= vk::ImageAspectFlags::STENCIL;
        }
        if clear.is_empty() {
            return Ok(());
        }
        let layers = self.surface(target.sid)?.layout.layers();
        let cmd = self.barrier()?;
        let value = vk::ClearDepthStencilValue {
            depth: depth.clamp(0.0, 1.0),
            stencil: u32::from(stencil),
        };
        // SAFETY: recording; the range lies within the live image.
        unsafe {
            self.device.cmd_clear_depth_stencil_image(
                cmd,
                image,
                vk::ImageLayout::GENERAL,
                &value,
                &[range(
                    clear,
                    target.mip,
                    1,
                    target.first_layer.min(layers.saturating_sub(1)),
                    1,
                )],
            );
        }
        Ok(())
    }

    pub(crate) fn destroy_draw_caches(&mut self) {
        let c = std::mem::take(&mut self.draw);
        // SAFETY: the GPU is idle (called from drop after a flush).
        unsafe {
            for p in c.pipelines.into_values() {
                self.device.destroy_pipeline(p, None);
            }
            for (set, layout) in c.layouts.into_values() {
                self.device.destroy_pipeline_layout(layout, None);
                self.device.destroy_descriptor_set_layout(set, None);
            }
            for m in c.modules.into_values() {
                self.device.destroy_shader_module(m, None);
            }
            for s in c.samplers.into_values() {
                self.device.destroy_sampler(s, None);
            }
            if let Some(n) = c.null {
                for v in n.views.into_values().chain(n.storage_views.into_values()) {
                    self.device.destroy_image_view(v, None);
                }
                for i in n.images {
                    self.device.destroy_image(i, None);
                }
                self.device.destroy_buffer_view(n.texel_buffer, None);
                self.device.destroy_buffer(n.buffer, None);
                self.device.destroy_buffer_view(n.sink_texel, None);
                self.device.destroy_buffer(n.sink, None);
                for m in n.memory {
                    self.device.free_memory(m, None);
                }
            }
        }
    }
}

/// Sampler of an unbound slot (Direct3D's default: linear, clamp).
fn default_sampler() -> SamplerState {
    SamplerState {
        filter: 1 | (1 << 2) | (1 << 4),
        address: [3, 3, 3],
        mip_lod_bias: 0.0,
        max_anisotropy: 1,
        comparison: 1,
        border_color: [1.0; 4],
        min_lod: f32::MIN,
        max_lod: f32::MAX,
    }
}

/// View swizzles: X8 formats read alpha as one; Direct3D 9 depth
/// textures read their depth in every component.
fn swizzle(format: u32) -> vk::ComponentMapping {
    use svga_proto::format as f;
    if matches!(
        format,
        f::Z_D16 | f::Z_D24S8 | f::Z_D24X8 | f::Z_DF16 | f::Z_DF24 | f::Z_D24S8_INT
    ) {
        let r = vk::ComponentSwizzle::R;
        return vk::ComponentMapping {
            r,
            g: r,
            b: r,
            a: r,
        };
    }
    let one_alpha = matches!(
        format,
        f::X8R8G8B8
            | f::B8G8R8X8_UNORM
            | f::B8G8R8X8_UNORM_SRGB
            | f::B8G8R8X8_TYPELESS
            | f::X1R5G5B5
    );
    vk::ComponentMapping {
        a: if one_alpha {
            vk::ComponentSwizzle::ONE
        } else {
            vk::ComponentSwizzle::IDENTITY
        },
        ..Default::default()
    }
}

/// A descriptor write, owning its info until recorded.
pub(crate) enum Write {
    Buffer(u32, vk::DescriptorBufferInfo),
    Sampler(u32, vk::DescriptorImageInfo),
    Image(u32, vk::DescriptorImageInfo),
    TexelBuffer(u32, vk::BufferView),
    StorageBuffer(u32, vk::DescriptorBufferInfo),
    StorageImage(u32, vk::DescriptorImageInfo),
    StorageTexelBuffer(u32, vk::BufferView),
}

/// The descriptor type of a shader binding.
fn descriptor_type(kind: BindingKind) -> vk::DescriptorType {
    use vk::DescriptorType as D;
    match kind {
        BindingKind::ConstantBuffer { .. } => D::UNIFORM_BUFFER,
        BindingKind::Sampler { .. } => D::SAMPLER,
        BindingKind::Resource { dimension: 1, .. } => D::UNIFORM_TEXEL_BUFFER,
        BindingKind::Resource { .. } => D::SAMPLED_IMAGE,
        BindingKind::BufferResource { .. } | BindingKind::UaView { dimension: 0, .. } => {
            D::STORAGE_BUFFER
        }
        BindingKind::UaView { dimension: 1, .. } => D::STORAGE_TEXEL_BUFFER,
        BindingKind::UaView { .. } => D::STORAGE_IMAGE,
    }
}

impl Write {
    fn sampler(binding: u32, sampler: vk::Sampler) -> Self {
        Self::Sampler(binding, vk::DescriptorImageInfo::default().sampler(sampler))
    }

    fn image(binding: u32, view: vk::ImageView) -> Self {
        Self::Image(
            binding,
            vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::GENERAL),
        )
    }

    pub(crate) fn as_vk(&self) -> vk::WriteDescriptorSet<'_> {
        let w = vk::WriteDescriptorSet::default();
        match self {
            Self::Buffer(binding, info) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(std::slice::from_ref(info)),
            Self::Sampler(binding, info) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(std::slice::from_ref(info)),
            Self::Image(binding, info) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(std::slice::from_ref(info)),
            Self::TexelBuffer(binding, view) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
                .texel_buffer_view(std::slice::from_ref(view)),
            Self::StorageBuffer(binding, info) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(info)),
            Self::StorageImage(binding, info) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(std::slice::from_ref(info)),
            Self::StorageTexelBuffer(binding, view) => w
                .dst_binding(*binding)
                .descriptor_type(vk::DescriptorType::STORAGE_TEXEL_BUFFER)
                .texel_buffer_view(std::slice::from_ref(view)),
        }
    }
}
