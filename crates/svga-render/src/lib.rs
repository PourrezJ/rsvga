//! The Vulkan backend of rsvga: guest surfaces as images and buffers on the
//! host GPU.
//!
//! A surface keeps the layout of its guest copy ([`SurfaceLayout`]): its
//! bytes go to the GPU ([`Renderer::upload`]) and come back
//! ([`Renderer::download`]) without conversion, through host-visible
//! staging buffers. Operations are recorded into one command buffer and
//! submitted at [`Renderer::flush`] (a download flushes first).
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

mod draw;
pub mod format;

pub use draw::{
    Bound, Call, Draw, Resource, Target, VertexAttribute, VertexBuffer, default_blend,
    default_depth_stencil, default_rasterizer,
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
    },
}

/// Key of a cached image view: format, aspect, view type, mips and layers,
/// swizzle (raw Vulkan values).
type ViewKey = (i32, u32, i32, [u32; 4], [i32; 4]);

struct Surface {
    resource: Storage,
    layout: SurfaceLayout,
    /// Image views made for draws, destroyed with the surface.
    views: HashMap<ViewKey, vk::ImageView>,
    /// Buffer views (format, offset, bytes), likewise.
    buffer_views: HashMap<(i32, u64, u64), vk::BufferView>,
}

struct Staging {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

/// The GPU and the surfaces on it.
pub struct Renderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    memory: vk::PhysicalDeviceMemoryProperties,
    name: String,
    d24s8: bool,
    recording: bool,
    staging: Vec<Staging>,
    surfaces: HashMap<u32, Surface>,
    a8: bool,
    features: Features,
    push: Option<ash::khr::push_descriptor::Device>,
    draw: draw::Caches,
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
}

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
        match Self::open(entry, instance, name) {
            Ok(renderer) => Ok(renderer),
            Err((instance, error)) => {
                // SAFETY: nothing was created from the instance.
                unsafe { instance.destroy_instance(None) };
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
            ..Default::default()
        };
        let features = Features {
            draw: push && v13.dynamic_rendering == vk::TRUE,
            draw_parameters: v11.shader_draw_parameters == vk::TRUE,
            depth_clamp: have.depth_clamp == vk::TRUE,
            maintenance5: maintenance5 && m5.maintenance5 == vk::TRUE,
            anisotropy: have.sampler_anisotropy == vk::TRUE,
            mirror_once: v12.sampler_mirror_clamp_to_edge == vk::TRUE,
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
                let cmd = device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1),
                )?[0];
                let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
                Ok((pool, cmd, fence))
            }
        })();
        let (pool, cmd, fence) = match objects {
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
        Ok(Self {
            _entry: entry,
            instance,
            physical,
            device,
            queue,
            pool,
            cmd,
            fence,
            memory,
            name: device_name,
            d24s8,
            a8,
            features,
            push,
            draw: draw::Caches::default(),
            recording: false,
            staging: Vec::new(),
            surfaces: HashMap::new(),
        })
    }

    /// The device's name.
    pub fn name(&self) -> &str {
        &self.name
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

    /// The command buffer being recorded, begun if needed.
    fn cmd(&mut self) -> Result<vk::CommandBuffer, Error> {
        if !self.recording {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: the buffer is not in use (flush waited for it).
            unsafe { self.device.begin_command_buffer(self.cmd, &begin) }?;
            self.recording = true;
        }
        Ok(self.cmd)
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

    /// Submits what was recorded and waits for the GPU.
    pub fn flush(&mut self) -> Result<(), Error> {
        if !self.recording {
            return Ok(());
        }
        self.recording = false;
        let cmds = [self.cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&cmds);
        // SAFETY: the buffer was begun by `cmd`; the fence is unsignaled.
        let result = unsafe {
            self.device
                .end_command_buffer(self.cmd)
                .and_then(|()| self.device.queue_submit(self.queue, &[submit], self.fence))
                .and_then(|()| self.device.wait_for_fences(&[self.fence], true, u64::MAX))
                .and_then(|()| self.device.reset_fences(&[self.fence]))
                .and_then(|()| {
                    self.device
                        .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())
                })
        };
        for s in self.staging.drain(..) {
            // SAFETY: the GPU is done with the staging buffers.
            unsafe {
                self.device.destroy_buffer(s.buffer, None);
                self.device.free_memory(s.memory, None);
            }
        }
        Ok(result?)
    }

    /// A host-visible buffer of `size` bytes, mapped; freed at the next
    /// flush.
    fn staging(&mut self, size: u64) -> Result<(vk::Buffer, *mut c_void), Error> {
        let info = vk::BufferCreateInfo::default()
            .size(size.max(1))
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST);
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
                resource,
                layout: layout.clone(),
                views: HashMap::new(),
                buffer_views: HashMap::new(),
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
                | U::INDIRECT_BUFFER,
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
            .mip_levels(layout.mip_levels().max(1))
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
        })
    }

    /// Frees the GPU resource of surface `sid`, if any.
    pub fn destroy(&mut self, sid: u32) -> Result<(), Error> {
        if !self.surfaces.contains_key(&sid) {
            return Ok(());
        }
        // Recorded work may use it.
        self.flush()?;
        if let Some(surface) = self.surfaces.remove(&sid) {
            // SAFETY: the GPU is idle (flushed); the handles are ours.
            unsafe {
                for view in surface.views.into_values() {
                    self.device.destroy_image_view(view, None);
                }
                for view in surface.buffer_views.into_values() {
                    self.device.destroy_buffer_view(view, None);
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
        let target = match &surface.resource {
            Storage::Buffer { buffer, .. } => Ok(*buffer),
            Storage::Image { image, .. } => Err((*image, Self::whole_regions(surface)?)),
        };
        let (staging, ptr) = self.staging(data.len() as u64)?;
        // SAFETY: the mapping holds at least `data.len()` bytes and nothing
        // else accesses it until the copy is submitted.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast::<u8>(), data.len()) };
        let cmd = self.barrier()?;
        // SAFETY: recording; source and destination are live and large
        // enough (checked against the layout above).
        unsafe {
            match target {
                Ok(buffer) => {
                    let region = vk::BufferCopy::default().size(data.len() as u64);
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
        // Flush, but read the staging memory before it is freed.
        self.recording = false;
        let cmds = [self.cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&cmds);
        // SAFETY: the buffer was begun by `cmd`; the fence is unsignaled.
        let result = unsafe {
            self.device
                .end_command_buffer(self.cmd)
                .and_then(|()| self.device.queue_submit(self.queue, &[submit], self.fence))
                .and_then(|()| self.device.wait_for_fences(&[self.fence], true, u64::MAX))
                .and_then(|()| self.device.reset_fences(&[self.fence]))
                .and_then(|()| {
                    self.device
                        .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())
                })
        };
        if result.is_ok() {
            // SAFETY: the GPU finished writing the mapping (fence waited,
            // host barrier recorded), which holds `out.len()` bytes.
            unsafe { std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), out.as_mut_ptr(), out.len()) };
        }
        for s in self.staging.drain(..) {
            // SAFETY: the GPU is done with the staging buffers.
            unsafe {
                self.device.destroy_buffer(s.buffer, None);
                self.device.free_memory(s.memory, None);
            }
        }
        Ok(result?)
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
        let _ = self.flush();
        let sids: Vec<u32> = self.surfaces.keys().copied().collect();
        for sid in sids {
            let _ = self.destroy(sid);
        }
        self.destroy_draw_caches();
        // SAFETY: the GPU is idle and every object below is ours.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(test)]
mod tests;
