//! The Vulkan backend of rsvga: guest surfaces as images and buffers on the
//! host GPU.
//!
//! A surface keeps the layout of its guest copy ([`SurfaceLayout`]): its
//! bytes go to the GPU ([`Renderer::upload`]) and come back
//! ([`Renderer::download`]) without conversion, through host-visible
//! staging buffers (small uploads share the recycled chunks of draw
//! constants). Operations are recorded into one command buffer and
//! submitted at [`Renderer::flush`], or every few hundred draws so that
//! the GPU starts while the rest is recorded; a download flushes first
//! and waits. [`Renderer::read_async`] reads texels back without waiting:
//! the bytes are taken once the GPU is done ([`Renderer::poll`]).
//! Timestamps around each submission measure the GPU's own time
//! ([`Renderer::gpu_time`]).
//!
//! Draws ([`Renderer::draw`], module `draw`) turn Direct3D 10 pipeline
//! state into cached Vulkan pipelines, recorded with dynamic rendering and
//! push descriptors.
//!
//! Every image stays in the `GENERAL` layout and operations are separated
//! by full memory barriers: simple and correct, to be refined when
//! drawing makes it worth it.
//!
//! The GPU is the discrete one with the most local memory, unless
//! `RSVGA_GPU` names another (part of its name, e.g. `6400` or
//! `llvmpipe`).

mod compute;
mod draw;
pub mod format;

pub use compute::{Dispatch, Groups};
pub use draw::{
    Bound, Call, Draw, Resource, StreamOut, Target, VertexAttribute, VertexBuffer, default_blend,
    default_depth_stencil, default_rasterizer, dual_source,
};

use std::collections::HashMap;
use std::ffi::c_void;

use ash::vk;
use svga_core::layout::SurfaceLayout;
use svga_proto::surface_flag;

use crate::format::Kind;

/// A Vulkan or usage error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<vk::Result> for Error {
    fn from(result: vk::Result) -> Self {
        Self(format!("Vulkan: {result}"))
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error(message.into()))
}

/// An image subresource and a position in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    /// Array layer (cube face).
    pub layer: u32,
    /// Mip level.
    pub mip: u32,
    /// Texel position (z: depth slice of a 3D image).
    pub offset: [u32; 3],
}

enum Storage {
    Buffer {
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
        size: u64,
    },
    Image {
        image: vk::Image,
        memory: vk::DeviceMemory,
        format: vk::Format,
        aspect: vk::ImageAspectFlags,
        /// Bytes can be copied as they are (not a converted depth format,
        /// not multisampled).
        copyable: bool,
        three_d: bool,
        /// Usages it was made with (views may have to drop `STORAGE`).
        usage: vk::ImageUsageFlags,
    },
}

/// Key of a cached image view: format, aspect, view type, mips and layers,
/// swizzle (raw Vulkan values).
type ViewKey = (i32, u32, i32, [u32; 4], [i32; 4]);

struct Surface {
    resource: Storage,
    layout: SurfaceLayout,
    /// Mip levels on the GPU: the layout's, or a whole chain behind a
    /// single guest level (`AUTOGENMIPMAPS`).
    mips: u32,
    /// Image views made for draws, destroyed with the surface.
    views: HashMap<ViewKey, vk::ImageView>,
    /// Buffer views (format, offset, bytes), likewise.
    buffer_views: HashMap<(i32, u64, u64), vk::BufferView>,
    /// Bytes stream output wrote to the buffer, kept by the GPU (for
    /// appends and `DrawAuto`), once used.
    counter: Option<(vk::Buffer, vk::DeviceMemory)>,
}

struct Staging {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

/// Submissions in flight at most: the device goes on recording while the
/// GPU runs the previous ones, and waits only to read results back.
const FRAMES: usize = 4;

/// Draws recorded before they are submitted, even in the middle of a
/// command buffer of the guest.
const DRAWS_PER_SUBMISSION: u32 = 512;

/// A submission: its command buffer and fence, and what must live until
/// the GPU is done with it.
struct Frame {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Submitted and not yet waited for.
    busy: bool,
    /// Timestamps were written at its start and end.
    timed: bool,
    /// Its number among submissions (see [`Renderer::submitted`]).
    serial: u64,
    staging: Vec<Staging>,
    chunks: Vec<Chunk>,
}

/// A copy of image texels to host memory ([`Renderer::read_async`]).
struct Read {
    ticket: u64,
    /// The submission that copies: done once `completed` reaches it.
    serial: u64,
    buffer: ReadBuffer,
    /// Bytes read.
    len: usize,
}

/// A mapped host buffer for reads: buffer, memory, address, size.
type ReadBuffer = (vk::Buffer, vk::DeviceMemory, usize, u64);

/// Read buffers kept for the next reads.
const READ_BUFFERS_KEPT: usize = 4;

/// GPU time of submissions, from timestamps written at their start and
/// end (queries `2 i` and `2 i + 1` for frame `i`).
struct Timing {
    pool: vk::QueryPool,
    /// Nanoseconds per tick.
    period: f64,
    /// The bits timestamps hold.
    mask: u64,
    /// Nanoseconds and submissions measured since last taken.
    busy: (f64, u64),
}

/// A mapped chunk of draw constants: buffer, memory, mapping address.
type Chunk = (vk::Buffer, vk::DeviceMemory, usize);

/// The GPU and the surfaces on it.
pub struct Renderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    /// The command buffer being recorded: that of `frames[current]`.
    cmd: vk::CommandBuffer,
    /// Submissions: recorded one at a time, several in flight.
    frames: Vec<Frame>,
    current: usize,
    memory: vk::PhysicalDeviceMemoryProperties,
    name: String,
    d24s8: bool,
    recording: bool,
    staging: Vec<Staging>,
    surfaces: HashMap<u32, Surface>,
    a8: bool,
    features: Features,
    push: Option<ash::khr::push_descriptor::Device>,
    /// Transform feedback (stream output), if the device has it.
    xfb: Option<ash::ext::transform_feedback::Device>,
    draw: draw::Caches,
    /// The occlusion query pool and the slots in use.
    occlusion: Option<(vk::QueryPool, u32)>,
    /// The rendering left open by the last draw, for the next draw with
    /// the same attachments (see `draw.rs`).
    pass: Option<draw::Pass>,
    /// Constants given with draws.
    uniforms: Uniforms,
    /// `minUniformBufferOffsetAlignment`.
    uniform_align: u64,
    /// GPU time measurement, if the queue has timestamps.
    timing: Option<Timing>,
    /// Draws recorded since the last submission.
    draws: u32,
    /// Submissions made, and the last one known to be done (every
    /// submission waits for the ones before it: see `barrier`).
    submitted: u64,
    completed: u64,
    /// Reads not yet taken, in the order started.
    reads: Vec<Read>,
    next_read: u64,
    /// Read buffers free again.
    read_buffers: Vec<ReadBuffer>,
}

/// Bytes of a chunk of [`Uniforms`].
const UNIFORM_CHUNK: u64 = 4 << 20;
/// Chunks kept from one submission to the next.
const UNIFORM_CHUNKS_KEPT: usize = 4;

/// Constants given with draws ([`draw::Resource::Data`]): suballocated
/// from mapped chunks, reused once the GPU is done with a submission.
#[derive(Default)]
struct Uniforms {
    /// Chunks of the submission being recorded; the last is being filled.
    active: Vec<Chunk>,
    /// Bytes used in the last one.
    used: u64,
    /// Chunks free again.
    free: Vec<Chunk>,
}

/// Optional device features in use.
#[derive(Clone, Copy, Debug, Default)]
struct Features {
    /// Push descriptors and dynamic rendering: draws are possible.
    draw: bool,
    /// `BaseVertex`/`BaseInstance` in shaders.
    draw_parameters: bool,
    depth_clamp: bool,
    maintenance5: bool,
    anisotropy: bool,
    /// `MIRROR_CLAMP_TO_EDGE` addressing (Direct3D's `MIRROR_ONCE`).
    mirror_once: bool,
    /// Exact sample counts in occlusion queries.
    precise_occlusion: bool,
    /// Transform feedback (stream output).
    transform_feedback: bool,
}

/// Occlusion queries of draws (see [`Renderer::occlusion_slot`]).
const OCCLUSION_SLOTS: u32 = 1024;

/// Preference of a device: higher is better.
fn score(props: &vk::PhysicalDeviceProperties, memory: &vk::PhysicalDeviceMemoryProperties) -> u64 {
    let local: u64 = memory.memory_heaps[..memory.memory_heap_count as usize]
        .iter()
        .filter(|h| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .map(|h| h.size >> 20)
        .sum();
    let kind = match props.device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => 3,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
        _ => 0,
    };
    (kind << 40) | local.min((1 << 40) - 1)
}

impl Renderer {
    /// Opens the GPU (see the module documentation for which one).
    pub fn new() -> Result<Self, Error> {
        Self::with_device(std::env::var("RSVGA_GPU").ok().as_deref())
    }

    /// Opens the device whose name contains `name` (any case), or the best
    /// one.
    pub fn with_device(name: Option<&str>) -> Result<Self, Error> {
        // SAFETY: loads the system Vulkan loader; nothing else runs its
        // initialization concurrently in this process.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| Error(format!("no Vulkan: {e}")))?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"rsvga")
            .api_version(vk::API_VERSION_1_3);
        // SAFETY: valid create info, no layers nor extensions.
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app),
                None,
            )
        }?;
        // A clone keeps the loader loaded until the instance is destroyed
        // on failure (`open` drops the entry it was given).
        match Self::open(entry.clone(), instance, name) {
            Ok(renderer) => Ok(renderer),
            Err((instance, error)) => {
                // SAFETY: nothing was created from the instance, and the
                // loader its functions come from is still loaded.
                unsafe { instance.destroy_instance(None) };
                drop(entry);
                Err(error)
            }
        }
    }

    #[allow(clippy::result_large_err)]
    fn open(
        entry: ash::Entry,
        instance: ash::Instance,
        name: Option<&str>,
    ) -> Result<Self, (ash::Instance, Error)> {
        let fail = |instance, message: String| Err((instance, Error(message)));
        // SAFETY: a live instance.
        let devices = match unsafe { instance.enumerate_physical_devices() } {
            Ok(d) => d,
            Err(e) => return fail(instance, format!("Vulkan: {e}")),
        };
        let mut best: Option<(u64, vk::PhysicalDevice, String)> = None;
        for physical in devices {
            // SAFETY: a device of this instance.
            let props = unsafe { instance.get_physical_device_properties(physical) };
            // SAFETY: as above.
            let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
            let device_name = props
                .device_name_as_c_str()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default();
            if props.api_version < vk::API_VERSION_1_3 {
                continue;
            }
            if let Some(wanted) = name
                && !device_name.to_lowercase().contains(&wanted.to_lowercase())
            {
                continue;
            }
            let s = score(&props, &memory);
            if best.as_ref().is_none_or(|(b, ..)| s > *b) {
                best = Some((s, physical, device_name));
            }
        }
        let Some((_, physical, device_name)) = best else {
            return fail(
                instance,
                format!(
                    "no Vulkan 1.3 device{}",
                    name.map_or(String::new(), |n| format!(" named {n}"))
                ),
            );
        };
        // SAFETY: a device of this instance.
        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let Some(family) = families
            .iter()
            .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        else {
            return fail(instance, format!("{device_name}: no graphics queue"));
        };
        let Ok(family) = u32::try_from(family) else {
            return fail(instance, format!("{device_name}: queue family {family}"));
        };
        let priorities = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        // What Direct3D 10/11 needs, where the device has it.
        // SAFETY: a device of this instance.
        let extensions =
            unsafe { instance.enumerate_device_extension_properties(physical) }.unwrap_or_default();
        let has = |name: &std::ffi::CStr| {
            extensions
                .iter()
                .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
        };
        let push = has(ash::khr::push_descriptor::NAME);
        let maintenance5 = has(ash::khr::maintenance5::NAME);
        let xfb_ext = has(ash::ext::transform_feedback::NAME);
        let mut xfb_features = vk::PhysicalDeviceTransformFeedbackFeaturesEXT::default();
        let mut v11 = vk::PhysicalDeviceVulkan11Features::default();
        let mut v12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut v13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut m5 = vk::PhysicalDeviceMaintenance5FeaturesKHR::default();
        let mut query = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut v11)
            .push_next(&mut v12)
            .push_next(&mut v13);
        if maintenance5 {
            query = query.push_next(&mut m5);
        }
        if xfb_ext {
            query = query.push_next(&mut xfb_features);
        }
        // SAFETY: a device of this instance; the chain is well formed.
        unsafe { instance.get_physical_device_features2(physical, &mut query) };
        let have = query.features;
        let core = vk::PhysicalDeviceFeatures {
            robust_buffer_access: have.robust_buffer_access,
            geometry_shader: have.geometry_shader,
            independent_blend: have.independent_blend,
            dual_src_blend: have.dual_src_blend,
            logic_op: have.logic_op,
            depth_clamp: have.depth_clamp,
            depth_bias_clamp: have.depth_bias_clamp,
            fill_mode_non_solid: have.fill_mode_non_solid,
            sampler_anisotropy: have.sampler_anisotropy,
            sample_rate_shading: have.sample_rate_shading,
            image_cube_array: have.image_cube_array,
            shader_clip_distance: have.shader_clip_distance,
            occlusion_query_precise: have.occlusion_query_precise,
            // Shader model 5: UA views in every stage, typed views of any
            // format, gathers with offsets.
            vertex_pipeline_stores_and_atomics: have.vertex_pipeline_stores_and_atomics,
            fragment_stores_and_atomics: have.fragment_stores_and_atomics,
            shader_storage_image_read_without_format: have.shader_storage_image_read_without_format,
            shader_storage_image_write_without_format: have
                .shader_storage_image_write_without_format,
            shader_image_gather_extended: have.shader_image_gather_extended,
            tessellation_shader: have.tessellation_shader,
            ..Default::default()
        };
        let features = Features {
            draw: push && v13.dynamic_rendering == vk::TRUE,
            draw_parameters: v11.shader_draw_parameters == vk::TRUE,
            depth_clamp: have.depth_clamp == vk::TRUE,
            maintenance5: maintenance5 && m5.maintenance5 == vk::TRUE,
            anisotropy: have.sampler_anisotropy == vk::TRUE,
            precise_occlusion: have.occlusion_query_precise == vk::TRUE,
            mirror_once: v12.sampler_mirror_clamp_to_edge == vk::TRUE,
            transform_feedback: xfb_ext && xfb_features.transform_feedback == vk::TRUE,
        };
        let mut v11_on = vk::PhysicalDeviceVulkan11Features::default()
            .shader_draw_parameters(features.draw_parameters);
        let mut v12_on = vk::PhysicalDeviceVulkan12Features::default()
            .sampler_mirror_clamp_to_edge(features.mirror_once);
        let mut v13_on = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(v13.dynamic_rendering == vk::TRUE);
        let mut m5_on = vk::PhysicalDeviceMaintenance5FeaturesKHR::default().maintenance5(true);
        let mut names = Vec::new();
        if push {
            names.push(ash::khr::push_descriptor::NAME.as_ptr());
        }
        let mut info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_features(&core)
            .push_next(&mut v11_on)
            .push_next(&mut v12_on)
            .push_next(&mut v13_on);
        if features.maintenance5 {
            names.push(ash::khr::maintenance5::NAME.as_ptr());
            info = info.push_next(&mut m5_on);
        }
        let mut xfb_on =
            vk::PhysicalDeviceTransformFeedbackFeaturesEXT::default().transform_feedback(true);
        if features.transform_feedback {
            names.push(ash::ext::transform_feedback::NAME.as_ptr());
            info = info.push_next(&mut xfb_on);
        }
        let info = info.enabled_extension_names(&names);
        // SAFETY: a valid device create info for this physical device: only
        // supported extensions and features.
        let device = match unsafe { instance.create_device(physical, &info, None) } {
            Ok(d) => d,
            Err(e) => return fail(instance, format!("{device_name}: {e}")),
        };
        // SAFETY: the queue was requested at device creation.
        let queue = unsafe { device.get_device_queue(family, 0) };
        let objects = (|| -> Result<_, vk::Result> {
            // SAFETY: valid create infos on a live device.
            unsafe {
                let pool = device.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )?;
                let cmds = device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(u32::try_from(FRAMES).unwrap_or(1)),
                )?;
                let mut frames = Vec::new();
                for cmd in cmds {
                    frames.push(Frame {
                        cmd,
                        fence: device.create_fence(&vk::FenceCreateInfo::default(), None)?,
                        busy: false,
                        timed: false,
                        serial: 0,
                        staging: Vec::new(),
                        chunks: Vec::new(),
                    });
                }
                Ok((pool, frames))
            }
        })();
        let (pool, frames) = match objects {
            Ok(o) => o,
            Err(e) => {
                // SAFETY: the device is idle and owns nothing else.
                unsafe { device.destroy_device(None) };
                return fail(instance, format!("{device_name}: {e}"));
            }
        };
        // SAFETY: a device of this instance.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        // SAFETY: as above.
        let d24s8 = unsafe {
            instance.get_physical_device_format_properties(physical, vk::Format::D24_UNORM_S8_UINT)
        }
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT);
        // A8_UNORM (maintenance5): DWM's glyph atlases keep their format.
        let a8 = features.maintenance5 && {
            // SAFETY: as above.
            let props = unsafe {
                instance.get_physical_device_format_properties(physical, vk::Format::A8_UNORM_KHR)
            };
            props.optimal_tiling_features.contains(
                vk::FormatFeatureFlags::SAMPLED_IMAGE
                    | vk::FormatFeatureFlags::COLOR_ATTACHMENT
                    | vk::FormatFeatureFlags::TRANSFER_SRC
                    | vk::FormatFeatureFlags::TRANSFER_DST,
            )
        };
        let push = features
            .draw
            .then(|| ash::khr::push_descriptor::Device::new(&instance, &device));
        let xfb = features
            .transform_feedback
            .then(|| ash::ext::transform_feedback::Device::new(&instance, &device));
        // SAFETY: a device of this instance.
        let limits = unsafe { instance.get_physical_device_properties(physical) }.limits;
        let uniform_align = limits.min_uniform_buffer_offset_alignment.max(16);
        let bits = families
            .get(family as usize)
            .map_or(0, |f| f.timestamp_valid_bits);
        let timing = (bits > 0 && limits.timestamp_period > 0.0)
            .then(|| {
                let info = vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(u32::try_from(FRAMES * 2).unwrap_or(2));
                // SAFETY: a valid create info on a live device.
                unsafe { device.create_query_pool(&info, None) }.ok()
            })
            .flatten()
            .map(|pool| Timing {
                pool,
                period: f64::from(limits.timestamp_period),
                mask: u64::MAX
                    .checked_shr(64u32.saturating_sub(bits))
                    .unwrap_or(u64::MAX),
                busy: (0.0, 0),
            });
        Ok(Self {
            _entry: entry,
            instance,
            physical,
            device,
            queue,
            pool,
            cmd: frames[0].cmd,
            frames,
            current: 0,
            memory,
            name: device_name,
            d24s8,
            a8,
            features,
            push,
            xfb,
            draw: draw::Caches::default(),
            occlusion: None,
            pass: None,
            uniforms: Uniforms::default(),
            uniform_align,
            timing,
            draws: 0,
            submitted: 0,
            completed: 0,
            reads: Vec::new(),
            next_read: 0,
            read_buffers: Vec::new(),
            recording: false,
            staging: Vec::new(),
            surfaces: HashMap::new(),
        })
    }

    /// The device's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether surfaces of `format` can live on the GPU (planar YUV
    /// formats, for one, cannot).
    pub fn format_supported(&self, format: u32) -> bool {
        format::kind(format, self.d24s8, self.a8).is_some()
    }

    /// Whether surface `sid` is on the GPU.
    pub fn has(&self, sid: u32) -> bool {
        self.surfaces.contains_key(&sid)
    }

    /// Whether the bytes of surface `sid` can go to the GPU and back
    /// ([`Renderer::upload`], [`Renderer::download`]): not for converted
    /// depth formats nor multisampled images.
    pub fn copyable(&self, sid: u32) -> bool {
        self.surfaces.get(&sid).is_some_and(|s| match s.resource {
            Storage::Buffer { .. } => true,
            Storage::Image { copyable, .. } => copyable,
        })
    }

    fn memory_type(&self, bits: u32, wanted: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.memory.memory_type_count).find(|&i| {
            bits & (1 << i) != 0
                && self.memory.memory_types[i as usize]
                    .property_flags
                    .contains(wanted)
        })
    }

    fn allocate(
        &self,
        requirements: vk::MemoryRequirements,
        wanted: &[vk::MemoryPropertyFlags],
    ) -> Result<vk::DeviceMemory, Error> {
        let Some(index) = wanted
            .iter()
            .find_map(|w| self.memory_type(requirements.memory_type_bits, *w))
        else {
            return err("no suitable GPU memory type");
        };
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(index);
        // SAFETY: a valid allocation info on a live device.
        Ok(unsafe { self.device.allocate_memory(&info, None) }?)
    }

    /// The command buffer being recorded, begun if needed, outside any
    /// rendering (an open one ends).
    fn cmd(&mut self) -> Result<vk::CommandBuffer, Error> {
        self.end_pass();
        if !self.recording {
            // This slot's previous submission must be done.
            self.retire(self.current)?;
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: the buffer is not in use (retired above).
            unsafe { self.device.begin_command_buffer(self.cmd, &begin) }?;
            self.recording = true;
            if let Some(t) = &self.timing {
                let first = u32::try_from(self.current.saturating_mul(2)).unwrap_or(0);
                // SAFETY: recording, outside any rendering; the queries
                // belong to this frame, whose last use was retired.
                unsafe {
                    self.device.cmd_reset_query_pool(self.cmd, t.pool, first, 2);
                    self.device.cmd_write_timestamp(
                        self.cmd,
                        vk::PipelineStageFlags::TOP_OF_PIPE,
                        t.pool,
                        first,
                    );
                }
                self.frames[self.current].timed = true;
            }
        }
        Ok(self.cmd)
    }

    /// Ends the rendering a draw left open.
    fn end_pass(&mut self) {
        if self.pass.take().is_some() && self.recording {
            // SAFETY: recording, inside the rendering the last draw began.
            unsafe { self.device.cmd_end_rendering(self.cmd) };
        }
    }

    /// Orders everything recorded so far before what follows.
    fn barrier(&mut self) -> Result<vk::CommandBuffer, Error> {
        let cmd = self.cmd()?;
        let all = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
        // SAFETY: recording; a global barrier has no resource to check.
        unsafe {
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[all],
                &[],
                &[],
            );
        }
        Ok(cmd)
    }

    /// A slot of the occlusion query pool for one draw ([`Draw::occlusion`]),
    /// or `None` when all are in use: [`Self::occlusion_collect`] frees
    /// them.
    pub fn occlusion_slot(&mut self) -> Result<Option<u32>, Error> {
        if self.occlusion.is_none() {
            let info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::OCCLUSION)
                .query_count(OCCLUSION_SLOTS);
            // SAFETY: a valid create info on a live device.
            let pool = unsafe { self.device.create_query_pool(&info, None) }?;
            self.occlusion = Some((pool, 0));
        }
        let Some((_, used)) = self.occlusion.as_mut() else {
            return Ok(None);
        };
        if *used >= OCCLUSION_SLOTS {
            return Ok(None);
        }
        let slot = *used;
        *used = used.saturating_add(1);
        Ok(Some(slot))
    }

    /// Occlusion slots handed out since the last [`Self::occlusion_read`].
    pub fn occlusion_used(&self) -> u32 {
        self.occlusion.map_or(0, |(_, used)| used)
    }

    /// Waits for the draws of the occlusion slots in use, and returns the
    /// samples each passed; the slots are free again.
    pub fn occlusion_read(&mut self) -> Result<Vec<u64>, Error> {
        let Some((pool, used)) = self.occlusion else {
            return Ok(Vec::new());
        };
        if used == 0 {
            return Ok(Vec::new());
        }
        self.wait_idle()?;
        let mut results = vec![0u64; used as usize];
        // SAFETY: slots 0..used were reset and written by submitted draws,
        // which flush waited for.
        unsafe {
            self.device.get_query_pool_results(
                pool,
                0,
                &mut results,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )
        }?;
        self.occlusion = Some((pool, 0));
        Ok(results)
    }

    /// Submits what was recorded, without waiting for the GPU: what it
    /// needs lives until its submission is retired.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.end_pass();
        if !self.recording {
            return Ok(());
        }
        self.recording = false;
        self.draws = 0;
        let i = self.current;
        if let Some(t) = &self.timing
            && self.frames[i].timed
        {
            let last = u32::try_from(i.saturating_mul(2).saturating_add(1)).unwrap_or(1);
            // SAFETY: recording, outside any rendering (ended above).
            unsafe {
                self.device.cmd_write_timestamp(
                    self.cmd,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    t.pool,
                    last,
                );
            }
        }
        let cmds = [self.cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&cmds);
        // SAFETY: the buffer was begun by `cmd`; the fence is unsignaled
        // (reset when retired).
        let result = unsafe {
            self.device.end_command_buffer(self.cmd).and_then(|()| {
                self.device
                    .queue_submit(self.queue, &[submit], self.frames[i].fence)
            })
        };
        let frame = &mut self.frames[i];
        frame.busy = result.is_ok();
        if frame.busy {
            self.submitted = self.submitted.saturating_add(1);
            frame.serial = self.submitted;
        }
        frame.staging.append(&mut self.staging);
        frame.chunks.append(&mut self.uniforms.active);
        self.uniforms.used = 0;
        if result.is_err() {
            // Nothing will signal the fence: free now (the GPU never saw it).
            self.free_frame(i);
        }
        self.current = i.wrapping_add(1) % FRAMES;
        self.cmd = self.frames[self.current].cmd;
        Ok(result?)
    }

    /// Waits for submission `i` if it is in flight, then frees what it
    /// needed.
    fn retire(&mut self, i: usize) -> Result<(), Error> {
        let frame = &self.frames[i];
        if frame.busy {
            // SAFETY: the fence belongs to this submission.
            unsafe {
                self.device
                    .wait_for_fences(&[frame.fence], true, u64::MAX)?;
                self.device.reset_fences(&[frame.fence])?;
                self.device
                    .reset_command_buffer(frame.cmd, vk::CommandBufferResetFlags::empty())?;
            }
            self.frames[i].busy = false;
            self.completed = self.completed.max(self.frames[i].serial);
            if self.frames[i].timed
                && let Some(t) = self.timing.as_mut()
            {
                let mut stamps = [0u64; 2];
                let first = u32::try_from(i.saturating_mul(2)).unwrap_or(0);
                // SAFETY: both queries were written by this submission,
                // which is done.
                let read = unsafe {
                    self.device.get_query_pool_results(
                        t.pool,
                        first,
                        &mut stamps,
                        vk::QueryResultFlags::TYPE_64,
                    )
                };
                if read.is_ok() {
                    let ticks = stamps[1].wrapping_sub(stamps[0]) & t.mask;
                    #[allow(clippy::cast_precision_loss, clippy::arithmetic_side_effects)]
                    {
                        t.busy.0 += ticks as f64 * t.period;
                    }
                    t.busy.1 = t.busy.1.saturating_add(1);
                }
            }
        }
        self.frames[i].timed = false;
        self.free_frame(i);
        Ok(())
    }

    /// The GPU time of the submissions finished since the last call, and
    /// their number (none without timestamps).
    pub fn gpu_time(&mut self) -> (std::time::Duration, u64) {
        let Some(t) = self.timing.as_mut() else {
            return (std::time::Duration::ZERO, 0);
        };
        let (nanos, count) = std::mem::take(&mut t.busy);
        (
            std::time::Duration::from_secs_f64(nanos.max(0.0) / 1e9),
            count,
        )
    }

    /// Retires the submissions the GPU has finished, without waiting.
    pub fn poll(&mut self) -> Result<(), Error> {
        for i in 0..FRAMES {
            if !self.frames[i].busy {
                continue;
            }
            // SAFETY: the fence belongs to this submission.
            if unsafe { self.device.get_fence_status(self.frames[i].fence) }? {
                self.retire(i)?;
            }
        }
        Ok(())
    }

    /// Starts copying the texels of `extent` at `at` in image `sid` to
    /// host memory, rows tightly packed, without waiting for the GPU: the
    /// copy runs with the next submission. [`Self::take_read`] gives the
    /// bytes, by the returned ticket.
    pub fn read_async(&mut self, sid: u32, at: At, extent: [u32; 3]) -> Result<u64, Error> {
        self.check_box(sid, at, extent)?;
        let surface = self.surface(sid)?;
        let Storage::Image {
            image,
            aspect,
            copyable,
            ..
        } = surface.resource
        else {
            return err(format!("surface {sid} is a buffer"));
        };
        let desc = surface.layout.desc();
        if !copyable || (desc.block_width, desc.block_height, desc.block_depth) != (1, 1, 1) {
            return err(format!("surface {sid} cannot be read texel by texel"));
        }
        let len = u64::from(extent[0])
            .saturating_mul(u64::from(extent[1]))
            .saturating_mul(u64::from(extent[2]))
            .saturating_mul(u64::from(desc.bytes_per_block));
        let Ok(bytes) = usize::try_from(len) else {
            return err("read too large");
        };
        let buffer = self.read_buffer(len)?;
        let region = vk::BufferImageCopy::default()
            .image_subresource(layers_of(aspect, at.mip, at.layer, 1))
            .image_offset(vk::Offset3D {
                x: i32::try_from(at.offset[0]).unwrap_or(0),
                y: i32::try_from(at.offset[1]).unwrap_or(0),
                z: i32::try_from(at.offset[2]).unwrap_or(0),
            })
            .image_extent(vk::Extent3D {
                width: extent[0],
                height: extent[1],
                depth: extent[2],
            });
        let cmd = match self.barrier() {
            Ok(cmd) => cmd,
            Err(e) => {
                self.recycle(buffer);
                return Err(e);
            }
        };
        let to_host = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ);
        // SAFETY: recording; the region lies within the image (checked),
        // the buffer holds `len` bytes.
        unsafe {
            if len > 0 {
                self.device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::GENERAL,
                    buffer.0,
                    &[region],
                );
            }
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[to_host],
                &[],
                &[],
            );
        }
        let ticket = self.next_read;
        self.next_read = self.next_read.wrapping_add(1);
        self.reads.push(Read {
            ticket,
            serial: self.submitted.saturating_add(1),
            buffer,
            len: bytes,
        });
        Ok(ticket)
    }

    /// Whether read `ticket` is done (as of the last [`Self::poll`]).
    pub fn read_done(&self, ticket: u64) -> bool {
        self.reads
            .iter()
            .find(|r| r.ticket == ticket)
            .is_some_and(|r| r.serial <= self.completed)
    }

    /// Gives the bytes of read `ticket` to `f`, then frees them: `None`
    /// if the GPU has not finished it, unless `wait`, which submits and
    /// waits for it.
    pub fn take_read<R>(
        &mut self,
        ticket: u64,
        wait: bool,
        f: impl FnOnce(&[u8]) -> R,
    ) -> Result<Option<R>, Error> {
        let Some(pos) = self.reads.iter().position(|r| r.ticket == ticket) else {
            return err(format!("no read {ticket}"));
        };
        let serial = self.reads[pos].serial;
        if serial > self.completed && wait {
            if serial > self.submitted {
                self.flush()?;
            }
            for i in 0..FRAMES {
                if self.frames[i].busy && self.frames[i].serial <= serial {
                    self.retire(i)?;
                }
            }
        }
        if serial > self.completed {
            return Ok(None);
        }
        let read = self.reads.remove(pos);
        // SAFETY: the submission that copied is done and made its writes
        // visible to the host (barrier above); the buffer is mapped at its
        // address for at least `len` bytes and nothing else uses it.
        let bytes = unsafe { std::slice::from_raw_parts(read.buffer.2 as *const u8, read.len) };
        let out = f(bytes);
        self.recycle(read.buffer);
        Ok(Some(out))
    }

    /// A free read buffer of at least `size` bytes.
    fn read_buffer(&mut self, size: u64) -> Result<ReadBuffer, Error> {
        if let Some(pos) = self
            .read_buffers
            .iter()
            .position(|b| b.3 >= size && b.3 / 2 <= size)
        {
            return Ok(self.read_buffers.swap_remove(pos));
        }
        let (buffer, ptr) = self.staging(size)?;
        // It outlives submissions: out of the staging list.
        let Some(staging) = self.staging.pop() else {
            return err("read buffer lost");
        };
        Ok((buffer, staging.memory, ptr as usize, size))
    }

    /// Keeps a read buffer for later reads, or frees it.
    fn recycle(&mut self, buffer: ReadBuffer) {
        self.read_buffers.push(buffer);
        if self.read_buffers.len() > READ_BUFFERS_KEPT {
            let (buffer, memory, ..) = self.read_buffers.remove(0);
            // SAFETY: no read uses it (taken or never submitted with it).
            unsafe {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
        }
    }

    /// Frees the staging buffers of submission `i`; its constant chunks
    /// are free again.
    fn free_frame(&mut self, i: usize) {
        for s in std::mem::take(&mut self.frames[i].staging) {
            // SAFETY: the GPU is done with them (retired or never used).
            unsafe {
                self.device.destroy_buffer(s.buffer, None);
                self.device.free_memory(s.memory, None);
            }
        }
        let chunks = std::mem::take(&mut self.frames[i].chunks);
        self.uniforms.free.extend(chunks);
        while self.uniforms.free.len() > UNIFORM_CHUNKS_KEPT {
            if let Some((buffer, memory, _)) = self.uniforms.free.pop() {
                // SAFETY: as above.
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                    self.device.free_memory(memory, None);
                }
            }
        }
    }

    /// Submits what was recorded and waits for every submission.
    pub fn wait_idle(&mut self) -> Result<(), Error> {
        self.flush()?;
        for i in 0..FRAMES {
            self.retire(i)?;
        }
        Ok(())
    }

    /// Whether submissions may still be running.
    fn in_flight(&self) -> bool {
        self.recording || self.frames.iter().any(|f| f.busy)
    }

    /// A host-visible buffer of `size` bytes, mapped; freed at the next
    /// flush.
    fn staging(&mut self, size: u64) -> Result<(vk::Buffer, *mut c_void), Error> {
        self.staging_usage(size, vk::BufferUsageFlags::empty())
    }

    /// Room for `bytes` of draw constants: buffer and offset, valid until
    /// the next submission completes.
    pub(crate) fn uniform_slice(&mut self, bytes: &[u8]) -> Result<(vk::Buffer, u64), Error> {
        self.chunk_slice(bytes, self.uniform_align)
    }

    /// [`Self::uniform_slice`] at a multiple of `align`, for constants and
    /// small uploads alike.
    fn chunk_slice(&mut self, bytes: &[u8], align: u64) -> Result<(vk::Buffer, u64), Error> {
        let size = bytes.len() as u64;
        if size > UNIFORM_CHUNK {
            return err(format!("{size} bytes in a {UNIFORM_CHUNK}-byte chunk"));
        }
        loop {
            let u = &mut self.uniforms;
            let offset = u.used.next_multiple_of(align.max(1));
            if let Some(&(buffer, _, address)) = u.active.last()
                && offset.saturating_add(size) <= UNIFORM_CHUNK
            {
                // SAFETY: the chunk is mapped for its life at `address`,
                // holds UNIFORM_CHUNK bytes, and this range is not in use
                // by the GPU (chunks of a submission are reused only once
                // it is retired).
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        (address as *mut u8).add(usize::try_from(offset).unwrap_or(0)),
                        bytes.len(),
                    );
                }
                u.used = offset.saturating_add(size);
                return Ok((buffer, offset));
            }
            u.used = 0;
            if let Some(chunk) = u.free.pop() {
                u.active.push(chunk);
                continue;
            }
            let (buffer, ptr) =
                self.staging_usage(UNIFORM_CHUNK, vk::BufferUsageFlags::UNIFORM_BUFFER)?;
            // The chunk outlives submissions: out of the staging list.
            let Some(staging) = self.staging.pop() else {
                return err("uniform chunk lost");
            };
            self.uniforms
                .active
                .push((buffer, staging.memory, ptr as usize));
        }
    }

    /// [`Self::staging`], also usable as `usage`.
    pub(crate) fn staging_usage(
        &mut self,
        size: u64,
        usage: vk::BufferUsageFlags,
    ) -> Result<(vk::Buffer, *mut c_void), Error> {
        let info = vk::BufferCreateInfo::default()
            .size(size.max(1))
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST | usage);
        // SAFETY: a valid create info on a live device.
        let buffer = unsafe { self.device.create_buffer(&info, None) }?;
        // SAFETY: a buffer of this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory = match self.allocate(
            requirements,
            &[
                vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT
                    | vk::MemoryPropertyFlags::HOST_CACHED,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ],
        ) {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: unused buffer.
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err(e);
            }
        };
        self.staging.push(Staging { buffer, memory });
        // SAFETY: fresh memory for this buffer; mapped once, for its life.
        let ptr = unsafe {
            self.device.bind_buffer_memory(buffer, memory, 0)?;
            self.device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())?
        };
        Ok((buffer, ptr))
    }

    // ---- surfaces -------------------------------------------------------

    /// Creates the GPU resource of surface `sid` (replacing any), its
    /// contents undefined. `flags`: `SVGA3D_SURFACE_*`.
    pub fn create(&mut self, sid: u32, layout: &SurfaceLayout, flags: u64) -> Result<(), Error> {
        self.destroy(sid)?;
        let stored = if flags & surface_flag::BIND_DEPTH_STENCIL != 0 {
            format::depth_storage(layout.format())
        } else {
            layout.format()
        };
        let Some(kind) = format::kind(stored, self.d24s8, self.a8) else {
            return err(format!(
                "surface format {} has no GPU format",
                layout.format()
            ));
        };
        let resource = match kind {
            Kind::Buffer => self.create_buffer(layout.total_bytes())?,
            Kind::Color(f) | Kind::Depth(f) | Kind::DepthConverted(f) => {
                self.create_image(layout, flags, kind, f)?
            }
        };
        self.surfaces.insert(
            sid,
            Surface {
                mips: match resource {
                    Storage::Buffer { .. } => 1,
                    Storage::Image { .. } => gpu_mips(layout, flags),
                },
                resource,
                layout: layout.clone(),
                views: HashMap::new(),
                buffer_views: HashMap::new(),
                counter: None,
            },
        );
        Ok(())
    }

    fn create_buffer(&mut self, size: u64) -> Result<Storage, Error> {
        use vk::BufferUsageFlags as U;
        let info = vk::BufferCreateInfo::default().size(size.max(4)).usage(
            U::TRANSFER_SRC
                | U::TRANSFER_DST
                | U::VERTEX_BUFFER
                | U::INDEX_BUFFER
                | U::UNIFORM_BUFFER
                | U::UNIFORM_TEXEL_BUFFER
                | U::STORAGE_BUFFER
                | U::STORAGE_TEXEL_BUFFER
                | U::INDIRECT_BUFFER
                | if self.features.transform_feedback {
                    U::TRANSFORM_FEEDBACK_BUFFER_EXT
                } else {
                    U::empty()
                },
        );
        // SAFETY: a valid create info on a live device.
        let buffer = unsafe { self.device.create_buffer(&info, None) }?;
        // SAFETY: a buffer of this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory = self
            .allocate(requirements, &[vk::MemoryPropertyFlags::DEVICE_LOCAL])
            .and_then(|memory| {
                // SAFETY: fresh memory sized for this buffer.
                unsafe { self.device.bind_buffer_memory(buffer, memory, 0) }
                    .map(|()| memory)
                    .map_err(Error::from)
            });
        match memory {
            Ok(memory) => Ok(Storage::Buffer {
                buffer,
                memory,
                size,
            }),
            Err(e) => {
                // SAFETY: unused buffer.
                unsafe { self.device.destroy_buffer(buffer, None) };
                Err(e)
            }
        }
    }

    fn create_image(
        &mut self,
        layout: &SurfaceLayout,
        flags: u64,
        kind: Kind,
        format: vk::Format,
    ) -> Result<Storage, Error> {
        let size = layout.size();
        let three_d = flags & surface_flag::VOLUME != 0 || size.depth > 1;
        let image_type = if three_d {
            vk::ImageType::TYPE_3D
        } else if flags & surface_flag::ONE_D != 0 && size.height == 1 {
            vk::ImageType::TYPE_1D
        } else {
            vk::ImageType::TYPE_2D
        };
        let samples = match layout.samples() {
            1 => vk::SampleCountFlags::TYPE_1,
            2 => vk::SampleCountFlags::TYPE_2,
            4 => vk::SampleCountFlags::TYPE_4,
            8 => vk::SampleCountFlags::TYPE_8,
            16 => vk::SampleCountFlags::TYPE_16,
            n => return err(format!("{n} samples per pixel")),
        };
        // SAFETY: a device of this instance.
        let features = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        }
        .optimal_tiling_features;
        use vk::FormatFeatureFlags as F;
        use vk::ImageUsageFlags as U;
        if !features.contains(F::TRANSFER_SRC | F::TRANSFER_DST) {
            return err(format!("{format:?} cannot be copied on {}", self.name));
        }
        let mut usage = U::TRANSFER_SRC | U::TRANSFER_DST;
        if features.contains(F::SAMPLED_IMAGE) {
            usage |= U::SAMPLED;
        }
        let depth = matches!(kind, Kind::Depth(_) | Kind::DepthConverted(_));
        if depth && features.contains(F::DEPTH_STENCIL_ATTACHMENT) {
            usage |= U::DEPTH_STENCIL_ATTACHMENT;
        }
        if !depth && features.contains(F::COLOR_ATTACHMENT) {
            usage |= U::COLOR_ATTACHMENT;
        }
        let mut create_flags = vk::ImageCreateFlags::empty();
        if !depth {
            create_flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
        }
        // Unordered-access views: storage images, in the view's format
        // (sRGB formats are not storable themselves, their UNORM twins
        // are: the usage then only needs to suit some view format).
        if flags & surface_flag::BIND_UAVIEW != 0 && !depth {
            usage |= U::STORAGE;
            if !features.contains(F::STORAGE_IMAGE) {
                create_flags |= vk::ImageCreateFlags::EXTENDED_USAGE;
            }
        }
        let layers = if three_d { 1 } else { layout.layers() };
        if flags & surface_flag::CUBEMAP != 0
            && image_type == vk::ImageType::TYPE_2D
            && size.width == size.height
            && layers % 6 == 0
        {
            create_flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
        }
        let info = vk::ImageCreateInfo::default()
            .flags(create_flags)
            .image_type(image_type)
            .format(format)
            .extent(vk::Extent3D {
                width: size.width.max(1),
                height: size.height.max(1),
                depth: if three_d { size.depth.max(1) } else { 1 },
            })
            .mip_levels(gpu_mips(layout, flags))
            .array_layers(layers.max(1))
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: a valid create info on a live device.
        let image = unsafe { self.device.create_image(&info, None) }?;
        // SAFETY: an image of this device.
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let memory = self
            .allocate(requirements, &[vk::MemoryPropertyFlags::DEVICE_LOCAL])
            .and_then(|memory| {
                // SAFETY: fresh memory sized for this image.
                unsafe { self.device.bind_image_memory(image, memory, 0) }
                    .map(|()| memory)
                    .map_err(Error::from)
            });
        let memory = match memory {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: unused image.
                unsafe { self.device.destroy_image(image, None) };
                return Err(e);
            }
        };
        let aspect = match format {
            vk::Format::D16_UNORM | vk::Format::D32_SFLOAT => vk::ImageAspectFlags::DEPTH,
            _ if depth => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
            _ => vk::ImageAspectFlags::COLOR,
        };
        // Into GENERAL for its whole life.
        let cmd = self.cmd()?;
        let to_general = vk::ImageMemoryBarrier::default()
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range(
                aspect,
                0,
                vk::REMAINING_MIP_LEVELS,
                0,
                vk::REMAINING_ARRAY_LAYERS,
            ));
        // SAFETY: recording; the barrier names a live image.
        unsafe {
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_general],
            );
        }
        Ok(Storage::Image {
            image,
            memory,
            format,
            aspect,
            copyable: layout.samples() == 1
                && (kind == Kind::Color(format)
                    || matches!(format, vk::Format::D16_UNORM | vk::Format::D32_SFLOAT)),
            three_d,
            usage,
        })
    }

    /// Frees the GPU resource of surface `sid`, if any.
    pub fn destroy(&mut self, sid: u32) -> Result<(), Error> {
        if !self.surfaces.contains_key(&sid) {
            return Ok(());
        }
        // Recorded or running work may use it.
        if self.in_flight() {
            self.wait_idle()?;
        }
        if let Some(surface) = self.surfaces.remove(&sid) {
            // SAFETY: the GPU is idle (waited); the handles are ours.
            unsafe {
                for view in surface.views.into_values() {
                    self.device.destroy_image_view(view, None);
                }
                for view in surface.buffer_views.into_values() {
                    self.device.destroy_buffer_view(view, None);
                }
                if let Some((buffer, memory)) = surface.counter {
                    self.device.destroy_buffer(buffer, None);
                    self.device.free_memory(memory, None);
                }
                match surface.resource {
                    Storage::Buffer { buffer, memory, .. } => {
                        self.device.destroy_buffer(buffer, None);
                        self.device.free_memory(memory, None);
                    }
                    Storage::Image { image, memory, .. } => {
                        self.device.destroy_image(image, None);
                        self.device.free_memory(memory, None);
                    }
                }
            }
        }
        Ok(())
    }

    fn surface(&self, sid: u32) -> Result<&Surface, Error> {
        self.surfaces
            .get(&sid)
            .ok_or_else(|| Error(format!("surface {sid} is not on the GPU")))
    }

    /// Buffer-image regions of every subresource of a copyable image, at
    /// the offsets of its guest layout.
    fn whole_regions(surface: &Surface) -> Result<Vec<vk::BufferImageCopy>, Error> {
        let Storage::Image {
            aspect,
            copyable,
            three_d,
            ..
        } = surface.resource
        else {
            return err("not an image");
        };
        if !copyable {
            return err("this image's bytes differ from the guest's");
        }
        let layout = &surface.layout;
        let layers = if three_d { 1 } else { layout.layers() };
        let mut regions = Vec::new();
        for layer in 0..layers {
            for mip in 0..layout.mip_levels() {
                let offset = layout
                    .image_offset(layer, mip)
                    .ok_or_else(|| Error("image outside its layout".into()))?;
                let size = layout.mip_size(mip);
                regions.push(
                    vk::BufferImageCopy::default()
                        .buffer_offset(offset)
                        .image_subresource(layers_of(aspect, mip, layer, 1))
                        .image_extent(vk::Extent3D {
                            width: size.width,
                            height: size.height,
                            depth: size.depth,
                        }),
                );
            }
        }
        Ok(regions)
    }

    /// Replaces the whole contents of surface `sid` with `data`, in its
    /// guest layout.
    pub fn upload(&mut self, sid: u32, data: &[u8]) -> Result<(), Error> {
        let surface = self.surface(sid)?;
        if data.len() as u64 != surface.layout.total_bytes() {
            return err(format!(
                "surface {sid}: {} bytes for a {}-byte layout",
                data.len(),
                surface.layout.total_bytes()
            ));
        }
        let block = u64::from(surface.layout.desc().bytes_per_block).max(1);
        let target = match &surface.resource {
            Storage::Buffer { buffer, .. } => Ok(*buffer),
            Storage::Image { image, .. } => Err((*image, Self::whole_regions(surface)?)),
        };
        let len = data.len() as u64;
        // Small uploads share the chunks of the submission (copies start on
        // a multiple of the texel block); large ones get their own buffer.
        let (staging, base) = if len > 0 && len <= UNIFORM_CHUNK {
            let align = lcm(self.uniform_align, block);
            self.chunk_slice(data, align)?
        } else {
            let (staging, ptr) = self.staging(len)?;
            // SAFETY: the mapping holds at least `data.len()` bytes and
            // nothing else accesses it until the copy is submitted.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast::<u8>(), data.len()) };
            (staging, 0)
        };
        let target = target.map_err(|(image, mut regions)| {
            for r in &mut regions {
                r.buffer_offset = r.buffer_offset.saturating_add(base);
            }
            (image, regions)
        });
        let cmd = self.barrier()?;
        // SAFETY: recording; source and destination are live and large
        // enough (checked against the layout above).
        unsafe {
            match target {
                Ok(buffer) => {
                    let region = vk::BufferCopy::default().src_offset(base).size(len);
                    if !data.is_empty() {
                        self.device.cmd_copy_buffer(cmd, staging, buffer, &[region]);
                    }
                }
                Err((image, regions)) => self.device.cmd_copy_buffer_to_image(
                    cmd,
                    staging,
                    image,
                    vk::ImageLayout::GENERAL,
                    &regions,
                ),
            }
        }
        Ok(())
    }

    /// Reads the whole contents of surface `sid` into `out`, in its guest
    /// layout. Waits for the GPU.
    pub fn download(&mut self, sid: u32, out: &mut [u8]) -> Result<(), Error> {
        let surface = self.surface(sid)?;
        if out.len() as u64 != surface.layout.total_bytes() {
            return err(format!(
                "surface {sid}: {} bytes for a {}-byte layout",
                out.len(),
                surface.layout.total_bytes()
            ));
        }
        let source = match &surface.resource {
            Storage::Buffer { buffer, .. } => Ok(*buffer),
            Storage::Image { image, .. } => Err((*image, Self::whole_regions(surface)?)),
        };
        let (staging, ptr) = self.staging(out.len() as u64)?;
        let cmd = self.barrier()?;
        // SAFETY: recording; source and destination are live and large
        // enough.
        unsafe {
            match source {
                Ok(buffer) => {
                    if !out.is_empty() {
                        let region = vk::BufferCopy::default().size(out.len() as u64);
                        self.device.cmd_copy_buffer(cmd, buffer, staging, &[region]);
                    }
                }
                Err((image, regions)) => self.device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::GENERAL,
                    staging,
                    &regions,
                ),
            }
        }
        // The staging buffer lives until this flush completes: copy first.
        let cmd = self.cmd()?;
        let to_host = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ);
        // SAFETY: recording; a global barrier.
        unsafe {
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[to_host],
                &[],
                &[],
            );
        }
        self.submit_and_read(ptr, out)
    }

    fn submit_and_read(&mut self, ptr: *mut c_void, out: &mut [u8]) -> Result<(), Error> {
        // Submit, wait (this submission's fence covers every earlier one),
        // read the staging memory, and only then free it.
        let i = self.current;
        self.flush()?;
        // SAFETY: the fence belongs to submission `i`, just made.
        unsafe {
            self.device
                .wait_for_fences(&[self.frames[i].fence], true, u64::MAX)?;
            // The GPU finished writing the mapping (host barrier recorded),
            // which holds `out.len()` bytes.
            std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), out.as_mut_ptr(), out.len());
        }
        for j in 0..FRAMES {
            self.retire(j)?;
        }
        Ok(())
    }

    // ---- operations -----------------------------------------------------

    fn image(
        &self,
        sid: u32,
    ) -> Result<(vk::Image, vk::Format, vk::ImageAspectFlags, bool), Error> {
        match self.surface(sid)?.resource {
            Storage::Image {
                image,
                format,
                aspect,
                three_d,
                ..
            } => Ok((image, format, aspect, three_d)),
            Storage::Buffer { .. } => err(format!("surface {sid} is a buffer")),
        }
    }

    fn buffer(&self, sid: u32) -> Result<(vk::Buffer, u64), Error> {
        match self.surface(sid)?.resource {
            Storage::Buffer { buffer, size, .. } => Ok((buffer, size)),
            Storage::Image { .. } => err(format!("surface {sid} is an image")),
        }
    }

    /// Clears `layers` array layers from `first_layer` of mip level `mip`
    /// of a color image (all depth slices of a 3D image).
    pub fn clear_color(
        &mut self,
        sid: u32,
        mip: u32,
        first_layer: u32,
        layers: u32,
        color: [f32; 4],
    ) -> Result<(), Error> {
        let (image, format, aspect, three_d) = self.image(sid)?;
        if aspect != vk::ImageAspectFlags::COLOR {
            return err(format!("surface {sid} is not a color image"));
        }
        let layout = &self.surface(sid)?.layout;
        let all_layers = if three_d { 1 } else { layout.layers() };
        let (first_layer, layers) = if three_d {
            (0, 1)
        } else {
            (first_layer, layers)
        };
        if mip >= layout.mip_levels()
            || first_layer
                .checked_add(layers)
                .is_none_or(|end| end > all_layers)
            || layers == 0
        {
            return err(format!(
                "surface {sid}: no mip {mip} layers {first_layer}+{layers}"
            ));
        }
        let value = format::clear_value(format, color);
        let cmd = self.barrier()?;
        // SAFETY: recording; the range lies within the live image.
        unsafe {
            self.device.cmd_clear_color_image(
                cmd,
                image,
                vk::ImageLayout::GENERAL,
                &value,
                &[range(aspect, mip, 1, first_layer, layers)],
            );
        }
        Ok(())
    }

    /// [`Self::clear_color`] with `color`, or with integer `bits` when
    /// `color` is `None`.
    fn clear_image_value(
        &mut self,
        sid: u32,
        mip: u32,
        first_layer: u32,
        layers: u32,
        color: Option<[f32; 4]>,
        bits: [u32; 4],
    ) -> Result<(), Error> {
        if let Some(color) = color {
            return self.clear_color(sid, mip, first_layer, layers, color);
        }
        let (image, _, aspect, three_d) = self.image(sid)?;
        let layout = &self.surface(sid)?.layout;
        let all_layers = if three_d { 1 } else { layout.layers() };
        let (first_layer, layers) = if three_d {
            (0, 1)
        } else {
            (
                first_layer,
                layers.min(all_layers.saturating_sub(first_layer)),
            )
        };
        if mip >= layout.mip_levels() || layers == 0 || aspect != vk::ImageAspectFlags::COLOR {
            return err(format!(
                "surface {sid}: no color mip {mip} layers {first_layer}+{layers}"
            ));
        }
        let cmd = self.barrier()?;
        // SAFETY: recording; the range lies within the live image.
        unsafe {
            self.device.cmd_clear_color_image(
                cmd,
                image,
                vk::ImageLayout::GENERAL,
                &vk::ClearColorValue { uint32: bits },
                &[range(aspect, mip, 1, first_layer, layers)],
            );
        }
        Ok(())
    }

    /// Fills `size` bytes of buffer surface `sid` from `offset` with
    /// `word` (both rounded down to whole words; `u64::MAX`: to the end).
    pub fn fill_buffer(
        &mut self,
        sid: u32,
        offset: u64,
        size: u64,
        word: u32,
    ) -> Result<(), Error> {
        let (buffer, total) = self.buffer(sid)?;
        let offset = offset & !3;
        if offset >= total {
            return Ok(());
        }
        let size = size.min(total.saturating_sub(offset)) & !3;
        if size == 0 {
            return Ok(());
        }
        let cmd = self.barrier()?;
        // SAFETY: recording outside any rendering; the range lies within
        // the live buffer, in whole words.
        unsafe { self.device.cmd_fill_buffer(cmd, buffer, offset, size, word) };
        Ok(())
    }

    /// Clears one mip of layers of a color image with integers, as
    /// `ClearUnorderedAccessViewUint` does: integer formats take them as
    /// they are, 32-bit float formats as bits, normalized ones their low
    /// bits as a fraction of the channel's range.
    pub fn clear_uint(
        &mut self,
        sid: u32,
        mip: u32,
        first_layer: u32,
        layers: u32,
        value: [u32; 4],
    ) -> Result<(), Error> {
        let (_, format, _, _) = self.image(sid)?;
        let color = if format::is_integer(format) {
            None
        } else {
            let channel = match format {
                vk::Format::R8_UNORM
                | vk::Format::R8G8_UNORM
                | vk::Format::R8G8B8A8_UNORM
                | vk::Format::B8G8R8A8_UNORM => Some(0xffu16),
                vk::Format::R16_UNORM
                | vk::Format::R16G16_UNORM
                | vk::Format::R16G16B16A16_UNORM => Some(0xffff),
                _ => None,
            };
            Some(match channel {
                Some(max) => value.map(|v| {
                    let low = u16::try_from(v & u32::from(max)).unwrap_or(max);
                    f32::from(low) / f32::from(max)
                }),
                None => value.map(f32::from_bits),
            })
        };
        self.clear_image_value(sid, mip, first_layer, layers, color, value)
    }

    /// Copies a box of `extent` texels between two images (3D images: z is
    /// a depth slice, layers are 0).
    pub fn copy_image(
        &mut self,
        dest: u32,
        to: At,
        src: u32,
        from: At,
        extent: [u32; 3],
    ) -> Result<(), Error> {
        let (dst_image, _, dst_aspect, _) = self.image(dest)?;
        let (src_image, _, src_aspect, _) = self.image(src)?;
        if dst_aspect != src_aspect {
            return err("copy between color and depth images");
        }
        let fits = |sid: u32, at: At| -> Result<bool, Error> {
            let layout = &self.surface(sid)?.layout;
            let size = layout.mip_size(at.mip);
            let inside = |o: u32, e: u32, s: u32| o.checked_add(e).is_some_and(|end| end <= s);
            Ok(at.mip < layout.mip_levels()
                && at.layer < layout.layers()
                && inside(at.offset[0], extent[0], size.width)
                && inside(at.offset[1], extent[1], size.height)
                && inside(at.offset[2], extent[2], size.depth))
        };
        if !fits(dest, to)? || !fits(src, from)? {
            return err("copy box outside an image");
        }
        if extent.contains(&0) {
            return Ok(());
        }
        let offset = |o: [u32; 3]| vk::Offset3D {
            x: o[0] as i32,
            y: o[1] as i32,
            z: o[2] as i32,
        };
        let region = vk::ImageCopy::default()
            .src_subresource(layers_of(src_aspect, from.mip, from.layer, 1))
            .src_offset(offset(from.offset))
            .dst_subresource(layers_of(dst_aspect, to.mip, to.layer, 1))
            .dst_offset(offset(to.offset))
            .extent(vk::Extent3D {
                width: extent[0],
                height: extent[1],
                depth: extent[2],
            });
        let cmd = self.barrier()?;
        // SAFETY: recording; both regions lie within live images.
        unsafe {
            self.device.cmd_copy_image(
                cmd,
                src_image,
                vk::ImageLayout::GENERAL,
                dst_image,
                vk::ImageLayout::GENERAL,
                &[region],
            );
        }
        Ok(())
    }

    /// Copies every subresource of `src` to `dest`, which has the same
    /// shape (images) or at least its size (buffers).
    pub fn copy_whole(&mut self, dest: u32, src: u32) -> Result<(), Error> {
        if let (Ok((d, dsize)), Ok((s, ssize))) = (self.buffer(dest), self.buffer(src)) {
            return self.copy_buffer_raw(d, 0, s, 0, ssize.min(dsize));
        }
        let layout = self.surface(src)?.layout.clone();
        let other = &self.surface(dest)?.layout;
        if (other.size(), other.mip_levels(), other.layers())
            != (layout.size(), layout.mip_levels(), layout.layers())
        {
            return err("whole copy between surfaces of different shapes");
        }
        for layer in 0..layout.layers() {
            for mip in 0..layout.mip_levels() {
                let size = layout.mip_size(mip);
                let at = At {
                    layer,
                    mip,
                    offset: [0; 3],
                };
                self.copy_image(dest, at, src, at, [size.width, size.height, size.depth])?;
            }
        }
        Ok(())
    }

    /// Copies `size` bytes between buffers.
    pub fn copy_buffer(
        &mut self,
        dest: u32,
        dest_offset: u64,
        src: u32,
        src_offset: u64,
        size: u64,
    ) -> Result<(), Error> {
        let (d, dsize) = self.buffer(dest)?;
        let (s, ssize) = self.buffer(src)?;
        let inside = |o: u64, total: u64| o.checked_add(size).is_some_and(|end| end <= total);
        if !inside(dest_offset, dsize) || !inside(src_offset, ssize) {
            return err("buffer copy outside a buffer");
        }
        self.copy_buffer_raw(d, dest_offset, s, src_offset, size)
    }

    fn copy_buffer_raw(
        &mut self,
        d: vk::Buffer,
        dest_offset: u64,
        s: vk::Buffer,
        src_offset: u64,
        size: u64,
    ) -> Result<(), Error> {
        if size == 0 {
            return Ok(());
        }
        let cmd = self.barrier()?;
        let region = vk::BufferCopy::default()
            .src_offset(src_offset)
            .dst_offset(dest_offset)
            .size(size);
        // SAFETY: recording; both ranges lie within live buffers.
        unsafe { self.device.cmd_copy_buffer(cmd, s, d, &[region]) };
        Ok(())
    }

    /// Fills a box of an image from a buffer whose rows start every
    /// `row_pitch` bytes and slices every `slice_pitch` bytes from
    /// `src_offset`.
    #[allow(clippy::too_many_arguments)]
    pub fn copy_buffer_to_image(
        &mut self,
        dest: u32,
        to: At,
        extent: [u32; 3],
        src: u32,
        src_offset: u64,
        row_pitch: u32,
        slice_pitch: u32,
    ) -> Result<(), Error> {
        let (image, _, aspect, _) = self.image(dest)?;
        let (buffer, size) = self.buffer(src)?;
        let layout = &self.surface(dest)?.layout;
        let desc = layout.desc();
        // Vulkan counts the buffer in texels: pitches must be whole blocks.
        let (bpb, bw, bh) = (desc.bytes_per_block, desc.block_width, desc.block_height);
        let Some(row_length) = (bpb != 0 && row_pitch.is_multiple_of(bpb))
            .then(|| row_pitch.checked_div(bpb)?.checked_mul(bw))
            .flatten()
        else {
            return err(format!(
                "row pitch {row_pitch} is not a whole number of blocks"
            ));
        };
        let image_height = if extent[2] > 1 {
            let Some(rows) = (row_pitch != 0 && slice_pitch.is_multiple_of(row_pitch))
                .then(|| slice_pitch.checked_div(row_pitch)?.checked_mul(bh))
                .flatten()
            else {
                return err(format!(
                    "slice pitch {slice_pitch} is not a whole number of rows"
                ));
            };
            rows
        } else {
            0
        };
        let mip_size = layout.mip_size(to.mip);
        let inside = |o: u32, e: u32, s: u32| o.checked_add(e).is_some_and(|end| end <= s);
        if to.mip >= layout.mip_levels()
            || to.layer >= layout.layers()
            || !inside(to.offset[0], extent[0], mip_size.width)
            || !inside(to.offset[1], extent[1], mip_size.height)
            || !inside(to.offset[2], extent[2], mip_size.depth)
        {
            return err("transfer box outside the image");
        }
        // Bytes the copy reads from the buffer.
        let rows = u64::from(extent[1].div_ceil(bh.max(1)));
        let blocks = u64::from(extent[0].div_ceil(bw.max(1)));
        let last_slice =
            u64::from(extent[2].saturating_sub(1)).saturating_mul(u64::from(slice_pitch));
        let needed = rows
            .saturating_sub(1)
            .saturating_mul(u64::from(row_pitch))
            .saturating_add(blocks.saturating_mul(u64::from(bpb)))
            .saturating_add(last_slice);
        if src_offset.checked_add(needed).is_none_or(|end| end > size) {
            return err("transfer reads beyond the buffer");
        }
        if extent.contains(&0) {
            return Ok(());
        }
        let region = vk::BufferImageCopy::default()
            .buffer_offset(src_offset)
            .buffer_row_length(row_length)
            .buffer_image_height(image_height)
            .image_subresource(layers_of(aspect, to.mip, to.layer, 1))
            .image_offset(vk::Offset3D {
                x: to.offset[0] as i32,
                y: to.offset[1] as i32,
                z: to.offset[2] as i32,
            })
            .image_extent(vk::Extent3D {
                width: extent[0],
                height: extent[1],
                depth: extent[2],
            });
        let cmd = self.barrier()?;
        // SAFETY: recording; the buffer range and image box were checked.
        unsafe {
            self.device.cmd_copy_buffer_to_image(
                cmd,
                buffer,
                image,
                vk::ImageLayout::GENERAL,
                &[region],
            );
        }
        Ok(())
    }

    // ---- resolves, blits, mipmaps ----------------------------------------

    fn format_features(&self, format: vk::Format) -> vk::FormatFeatureFlags {
        // SAFETY: a device of this instance.
        unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        }
        .optimal_tiling_features
    }

    /// Checks that `extent` texels from `at` lie within surface `sid`.
    fn check_box(&self, sid: u32, at: At, extent: [u32; 3]) -> Result<(), Error> {
        let layout = &self.surface(sid)?.layout;
        let size = layout.mip_size(at.mip);
        let inside = |o: u32, e: u32, s: u32| o.checked_add(e).is_some_and(|end| end <= s);
        if at.mip < layout.mip_levels()
            && at.layer < layout.layers()
            && inside(at.offset[0], extent[0], size.width)
            && inside(at.offset[1], extent[1], size.height)
            && inside(at.offset[2], extent[2], size.depth)
        {
            Ok(())
        } else {
            err(format!("box outside surface {sid}"))
        }
    }

    /// Resolves subresource `from` of multisampled color image `src` into
    /// `to` of single-sampled `dest`, of the same format.
    pub fn resolve(&mut self, dest: u32, to: At, src: u32, from: At) -> Result<(), Error> {
        let (dst_image, dst_format, dst_aspect, _) = self.image(dest)?;
        let (src_image, src_format, src_aspect, _) = self.image(src)?;
        if dst_aspect != vk::ImageAspectFlags::COLOR || src_aspect != dst_aspect {
            return err("resolving a depth image");
        }
        if dst_format != src_format {
            return err(format!("resolving {src_format:?} into {dst_format:?}"));
        }
        if self.surface(src)?.layout.samples() < 2 || self.surface(dest)?.layout.samples() != 1 {
            return err("resolve from a single-sampled or into a multisampled image");
        }
        let a = self.surface(src)?.layout.mip_size(from.mip);
        let b = self.surface(dest)?.layout.mip_size(to.mip);
        let extent = [a.width.min(b.width), a.height.min(b.height), 1];
        self.check_box(
            src,
            At {
                offset: [0; 3],
                ..from
            },
            extent,
        )?;
        self.check_box(
            dest,
            At {
                offset: [0; 3],
                ..to
            },
            extent,
        )?;
        let region = vk::ImageResolve::default()
            .src_subresource(layers_of(src_aspect, from.mip, from.layer, 1))
            .dst_subresource(layers_of(dst_aspect, to.mip, to.layer, 1))
            .extent(vk::Extent3D {
                width: extent[0],
                height: extent[1],
                depth: 1,
            });
        let cmd = self.barrier()?;
        // SAFETY: recording; both subresources exist (checked), formats
        // match, the source is multisampled and the destination is not.
        unsafe {
            self.device.cmd_resolve_image(
                cmd,
                src_image,
                vk::ImageLayout::GENERAL,
                dst_image,
                vk::ImageLayout::GENERAL,
                &[region],
            );
        }
        Ok(())
    }

    /// Copies box `src_extent` at `from` of `src` onto box `dest_extent`
    /// at `to` of `dest`, scaling and converting formats; `linear` filters
    /// when scaling.
    #[allow(clippy::too_many_arguments)]
    pub fn blit(
        &mut self,
        dest: u32,
        to: At,
        dest_extent: [u32; 3],
        src: u32,
        from: At,
        src_extent: [u32; 3],
        linear: bool,
    ) -> Result<(), Error> {
        let (dst_image, dst_format, dst_aspect, _) = self.image(dest)?;
        let (src_image, src_format, src_aspect, _) = self.image(src)?;
        if dst_aspect != src_aspect {
            return err("blit between color and depth images");
        }
        let src_features = self.format_features(src_format);
        if !src_features.contains(vk::FormatFeatureFlags::BLIT_SRC)
            || !self
                .format_features(dst_format)
                .contains(vk::FormatFeatureFlags::BLIT_DST)
        {
            return err(format!(
                "no blit from {src_format:?} to {dst_format:?} on {}",
                self.name
            ));
        }
        if self.surface(src)?.layout.samples() != 1 || self.surface(dest)?.layout.samples() != 1 {
            return err("blit of a multisampled image");
        }
        self.check_box(src, from, src_extent)?;
        self.check_box(dest, to, dest_extent)?;
        if src_extent.contains(&0) || dest_extent.contains(&0) {
            return Ok(());
        }
        let filter = if linear
            && src_aspect == vk::ImageAspectFlags::COLOR
            && src_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR)
        {
            vk::Filter::LINEAR
        } else {
            vk::Filter::NEAREST
        };
        let corners = |at: At, e: [u32; 3]| {
            let o = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
            [
                vk::Offset3D {
                    x: o(at.offset[0]),
                    y: o(at.offset[1]),
                    z: o(at.offset[2]),
                },
                vk::Offset3D {
                    x: o(at.offset[0].saturating_add(e[0])),
                    y: o(at.offset[1].saturating_add(e[1])),
                    z: o(at.offset[2].saturating_add(e[2])),
                },
            ]
        };
        let region = vk::ImageBlit::default()
            .src_subresource(layers_of(src_aspect, from.mip, from.layer, 1))
            .src_offsets(corners(from, src_extent))
            .dst_subresource(layers_of(dst_aspect, to.mip, to.layer, 1))
            .dst_offsets(corners(to, dest_extent));
        let cmd = self.barrier()?;
        // SAFETY: recording; both boxes lie within live single-sampled
        // images whose formats support blits.
        unsafe {
            self.device.cmd_blit_image(
                cmd,
                src_image,
                vk::ImageLayout::GENERAL,
                dst_image,
                vk::ImageLayout::GENERAL,
                &[region],
                filter,
            );
        }
        Ok(())
    }

    /// Fills mip levels `first_mip + 1 ..` of `mips` levels (`u32::MAX`:
    /// all), for `layers` layers from `first_layer`, each from the level
    /// above it with a linear (box) filter, as `GenerateMips` does.
    pub fn generate_mips(
        &mut self,
        sid: u32,
        first_mip: u32,
        mips: u32,
        first_layer: u32,
        layers: u32,
    ) -> Result<(), Error> {
        let (image, format, aspect, three_d) = self.image(sid)?;
        if aspect != vk::ImageAspectFlags::COLOR {
            return err("mipmaps of a depth image");
        }
        let needed = vk::FormatFeatureFlags::BLIT_SRC
            | vk::FormatFeatureFlags::BLIT_DST
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
        if !self.format_features(format).contains(needed) {
            return err(format!("{format:?} cannot be filtered on {}", self.name));
        }
        let surface = self.surface(sid)?;
        let (layout, total) = (surface.layout.clone(), surface.mips);
        let last = first_mip.saturating_add(mips.max(1)).min(total);
        let (first_layer, layers) = if three_d {
            (0, 1)
        } else {
            let first = first_layer.min(layout.layers().saturating_sub(1));
            (
                first,
                layers.max(1).min(layout.layers().saturating_sub(first)),
            )
        };
        let corner = |mip: u32| {
            let s = layout.mip_size(mip);
            let o = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
            [
                vk::Offset3D::default(),
                vk::Offset3D {
                    x: o(s.width),
                    y: o(s.height),
                    z: o(s.depth),
                },
            ]
        };
        for mip in first_mip..last.saturating_sub(1) {
            let next = mip.saturating_add(1);
            let region = vk::ImageBlit::default()
                .src_subresource(layers_of(aspect, mip, first_layer, layers))
                .src_offsets(corner(mip))
                .dst_subresource(layers_of(aspect, next, first_layer, layers))
                .dst_offsets(corner(next));
            // Each level reads the one written just before.
            let cmd = self.barrier()?;
            // SAFETY: recording; levels and layers lie within the live
            // image, whose format supports filtered blits.
            unsafe {
                self.device.cmd_blit_image(
                    cmd,
                    image,
                    vk::ImageLayout::GENERAL,
                    image,
                    vk::ImageLayout::GENERAL,
                    &[region],
                    vk::Filter::LINEAR,
                );
            }
        }
        Ok(())
    }
}

/// Mip levels of a surface's image: the layout's, or a whole chain behind
/// a single level with `AUTOGENMIPMAPS`.
fn gpu_mips(layout: &SurfaceLayout, flags: u64) -> u32 {
    let size = layout.size();
    if flags & surface_flag::AUTOGENMIPMAPS != 0 && layout.mip_levels() <= 1 {
        let largest = size.width.max(size.height).max(size.depth).max(1);
        32u32.saturating_sub(largest.leading_zeros())
    } else {
        layout.mip_levels().max(1)
    }
}

fn range(
    aspect: vk::ImageAspectFlags,
    mip: u32,
    mips: u32,
    layer: u32,
    layers: u32,
) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(aspect)
        .base_mip_level(mip)
        .level_count(mips)
        .base_array_layer(layer)
        .layer_count(layers)
}

/// Least common multiple of two sizes (alignments).
fn lcm(a: u64, b: u64) -> u64 {
    let (mut x, mut y) = (a.max(1), b.max(1));
    while let Some(r) = x.checked_rem(y)
        && y != 0
    {
        (x, y) = (y, r);
    }
    a.max(1)
        .checked_div(x)
        .unwrap_or(1)
        .saturating_mul(b.max(1))
}

fn layers_of(
    aspect: vk::ImageAspectFlags,
    mip: u32,
    layer: u32,
    count: u32,
) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(aspect)
        .mip_level(mip)
        .base_array_layer(layer)
        .layer_count(count)
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.wait_idle();
        let sids: Vec<u32> = self.surfaces.keys().copied().collect();
        for sid in sids {
            let _ = self.destroy(sid);
        }
        self.destroy_draw_caches();
        // SAFETY: the GPU is idle and every object below is ours.
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some((pool, _)) = self.occlusion.take() {
                self.device.destroy_query_pool(pool, None);
            }
            if let Some(t) = self.timing.take() {
                self.device.destroy_query_pool(t.pool, None);
            }
            let reads = std::mem::take(&mut self.reads)
                .into_iter()
                .map(|r| r.buffer);
            for (buffer, memory, ..) in reads.chain(std::mem::take(&mut self.read_buffers)) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
            let uniforms = std::mem::take(&mut self.uniforms);
            for (buffer, memory, _) in uniforms.active.into_iter().chain(uniforms.free) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
            }
            for frame in &self.frames {
                self.device.destroy_fence(frame.fence, None);
            }
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(test)]
mod tests;
