//! Compute dispatches: a compute shader, its descriptors (pushed, like
//! draws') and its thread groups, given or read from a buffer.

use ash::vk;
use ash::vk::Handle;
use svga_shader::Shader;

use crate::draw::{Bound, Write};
use crate::{Error, Renderer, err};

/// How many thread groups a dispatch runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Groups {
    /// These.
    Count([u32; 3]),
    /// Three `uint`s of a buffer surface, at a byte offset.
    Indirect {
        /// Buffer surface.
        sid: u32,
        /// Byte offset.
        offset: u64,
    },
}

/// Everything a dispatch uses.
#[derive(Clone, Debug)]
pub struct Dispatch<'a> {
    /// Compute shader.
    pub cs: &'a Shader,
    /// Descriptors.
    pub resources: Vec<Bound>,
    /// Constant data of `Resource::Data` bindings.
    pub data: Vec<Vec<u8>>,
    /// Thread groups.
    pub groups: Groups,
}

impl Renderer {
    /// Records `dispatch`. Every surface it names must be on the GPU.
    pub fn dispatch(&mut self, dispatch: &Dispatch<'_>) -> Result<(), Error> {
        if !self.features.draw {
            return err(format!(
                "{} cannot dispatch (no push descriptors)",
                self.name
            ));
        }
        if self.draws >= super::DRAWS_PER_SUBMISSION {
            self.flush()?;
        }
        self.draws = self.draws.saturating_add(1);
        let (_, layout) = self.layout_for(&[(dispatch.cs, vk::ShaderStageFlags::COMPUTE)])?;
        let pipeline = self.compute_pipeline(dispatch.cs, layout)?;
        let writes = self.descriptors_for(&[dispatch.cs], &dispatch.resources, &dispatch.data)?;
        let indirect = match dispatch.groups {
            Groups::Indirect { sid, offset } => Some((self.buffer(sid)?.0, offset)),
            Groups::Count(_) => None,
        };
        // Everything before is visible to the shader, and its writes to
        // what follows (the next command's barrier).
        let cmd = self.barrier()?;
        let push = self
            .push
            .as_ref()
            .ok_or_else(|| Error("no push descriptors".into()))?;
        // SAFETY: recording outside any rendering; the pipeline, layout and
        // descriptors are live and match the shader.
        unsafe {
            self.device
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
            if !writes.is_empty() {
                push.cmd_push_descriptor_set(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &writes.iter().map(Write::as_vk).collect::<Vec<_>>(),
                );
            }
            match (dispatch.groups, indirect) {
                (_, Some((buffer, offset))) => {
                    self.device.cmd_dispatch_indirect(cmd, buffer, offset)
                }
                (Groups::Count([x, y, z]), None) => self.device.cmd_dispatch(cmd, x, y, z),
                (Groups::Indirect { .. }, None) => {}
            }
        }
        Ok(())
    }

    /// The compute pipeline of `cs` with `layout`, cached.
    fn compute_pipeline(
        &mut self,
        cs: &Shader,
        layout: vk::PipelineLayout,
    ) -> Result<vk::Pipeline, Error> {
        let module = self.module(cs)?;
        let mut key = b"compute\0".to_vec();
        key.extend(module.as_raw().to_le_bytes());
        key.extend(layout.as_raw().to_le_bytes());
        if let Some(p) = self.draw.pipelines.get(&key) {
            return Ok(*p);
        }
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"main");
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(layout);
        // SAFETY: a valid create info on a live device.
        let pipeline = unsafe {
            self.device
                .create_compute_pipelines(vk::PipelineCache::null(), &[info], None)
        }
        .map_err(|(_, e)| Error::from(e))?
        .into_iter()
        .next()
        .ok_or_else(|| Error("no compute pipeline".into()))?;
        self.draw.pipelines.insert(key, pipeline);
        Ok(pipeline)
    }
}
