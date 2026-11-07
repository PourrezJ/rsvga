//! Legacy 3D contexts: the Direct3D 9 device state that VMware's
//! Direct3D 9 driver keeps on the device ([`svga_core::legacy`]).
//!
//! Each context holds render states, texture stage states, render targets,
//! viewport, scissor and clip planes, the bound shaders and their
//! constants. Constants are kept in the layout of the shaders' uniform
//! block ([`svga_shader::d3d9::constants`]), ready to upload with a draw.
//! Shader objects are global: their bytecode is read from the MOB they are
//! bound to. Clears, draws, blits and mipmap generation run on the GPU
//! ([`Outcome::Gpu`]); without one they are ignored.

use std::collections::HashMap;
use std::sync::Arc;

use svga_core::command::Rect;
use svga_core::gb::ImageId;
use svga_core::legacy::{ConstKind, LegacyCommand, Stage};
use svga_shader::d3d9::constants;

use crate::gb::Gb;
use crate::guest::GuestMemory;

/// No object.
pub const INVALID_ID: u32 = u32::MAX;
/// Render states kept (`SVGA3D_RS_MAX`).
pub const RENDER_STATES: usize = 99;
/// Texture stage states kept (`SVGA3D_TS_MAX`).
pub const TEXTURE_STATES: usize = 33;
/// Texture stages (`SVGA3D_NUM_TEXTURE_UNITS`).
pub const STAGES: usize = 32;
/// Render target slots: depth, stencil, colors 0 to 7.
pub const TARGETS: usize = 10;
/// Most contexts.
const MAX_CONTEXTS: usize = 256;
/// Most shader objects.
const MAX_SHADERS: usize = 1 << 16;
/// Largest shader, in bytes.
const MAX_SHADER_BYTES: u32 = 1 << 20;
/// Words of a stage's constants.
pub const CONSTANT_WORDS: usize = constants::COUNT as usize * 4;

// Render states (`SVGA3dRenderStateName`) the device reads.
#[allow(missing_docs)]
pub mod rs {
    pub const ZENABLE: usize = 1;
    pub const ZWRITEENABLE: usize = 2;
    pub const ALPHATESTENABLE: usize = 3;
    pub const BLENDENABLE: usize = 5;
    pub const FOGENABLE: usize = 6;
    pub const POINTSPRITEENABLE: usize = 11;
    pub const STENCILENABLE: usize = 8;
    pub const STENCILREF: usize = 13;
    pub const STENCILMASK: usize = 14;
    pub const STENCILWRITEMASK: usize = 15;
    pub const FOGSTART: usize = 16;
    pub const FOGEND: usize = 17;
    pub const FOGDENSITY: usize = 18;
    pub const POINTSIZE: usize = 19;
    pub const POINTSIZEMIN: usize = 20;
    pub const POINTSIZEMAX: usize = 21;
    pub const FOGCOLOR: usize = 25;
    pub const CLIPPLANEENABLE: usize = 27;
    pub const FOGMODE: usize = 28;
    pub const FILLMODE: usize = 29;
    pub const SHADEMODE: usize = 30;
    pub const SRCBLEND: usize = 32;
    pub const DSTBLEND: usize = 33;
    pub const BLENDEQUATION: usize = 34;
    pub const CULLMODE: usize = 35;
    pub const ZFUNC: usize = 36;
    pub const ALPHAFUNC: usize = 37;
    pub const STENCILFUNC: usize = 38;
    pub const STENCILFAIL: usize = 39;
    pub const STENCILZFAIL: usize = 40;
    pub const STENCILPASS: usize = 41;
    pub const ALPHAREF: usize = 42;
    pub const FRONTWINDING: usize = 43;
    pub const COLORWRITEENABLE: usize = 47;
    pub const SCISSORTESTENABLE: usize = 55;
    pub const BLENDCOLOR: usize = 56;
    pub const STENCILENABLE2SIDED: usize = 57;
    pub const CCWSTENCILFUNC: usize = 58;
    pub const CCWSTENCILFAIL: usize = 59;
    pub const CCWSTENCILZFAIL: usize = 60;
    pub const CCWSTENCILPASS: usize = 61;
    pub const SLOPESCALEDEPTHBIAS: usize = 63;
    pub const DEPTHBIAS: usize = 64;
    pub const OUTPUTGAMMA: usize = 65;
    pub const MULTISAMPLEANTIALIAS: usize = 85;
    pub const MULTISAMPLEMASK: usize = 86;
    pub const COLORWRITEENABLE1: usize = 90;
    pub const SEPARATEALPHABLENDENABLE: usize = 93;
    pub const SRCBLENDALPHA: usize = 94;
    pub const DSTBLENDALPHA: usize = 95;
    pub const BLENDEQUATIONALPHA: usize = 96;
}

// Texture stage states (`SVGA3dTextureStateName`) the device reads.
#[allow(missing_docs)]
pub mod ts {
    pub const BIND_TEXTURE: usize = 1;
    pub const ADDRESSU: usize = 8;
    pub const ADDRESSV: usize = 9;
    pub const MIPFILTER: usize = 10;
    pub const MAGFILTER: usize = 11;
    pub const MINFILTER: usize = 12;
    pub const BORDERCOLOR: usize = 13;
    pub const BUMPENVMAT00: usize = 17;
    pub const TEXTURE_MIPMAP_LEVEL: usize = 21;
    pub const TEXTURE_LOD_BIAS: usize = 22;
    pub const TEXTURE_ANISOTROPIC_LEVEL: usize = 23;
    pub const ADDRESSW: usize = 24;
    pub const GAMMA: usize = 25;
    pub const BUMPENVLSCALE: usize = 26;
    pub const BUMPENVLOFFSET: usize = 27;
}

/// A shader object.
#[derive(Clone, Debug)]
pub struct Shader9 {
    /// Stage.
    pub stage: Stage,
    /// Bytecode size, in bytes.
    pub size: u32,
    /// Bytecode, once bound to a MOB.
    pub code: Option<Arc<[u32]>>,
}

/// A legacy context.
#[derive(Clone, Debug)]
pub struct Context9 {
    /// Backing MOB (unused: the state stays on the device).
    pub mob: u32,
    /// Render states, by `SVGA3dRenderStateName`.
    pub rs: [u32; RENDER_STATES],
    /// Texture stage states, by stage then `SVGA3dTextureStateName`.
    pub ts: Vec<[u32; TEXTURE_STATES]>,
    /// Render targets, by `SVGA3dRenderTargetType`.
    pub targets: [ImageId; TARGETS],
    /// Viewport.
    pub viewport: Rect,
    /// Depth range.
    pub zrange: (f32, f32),
    /// Scissor rectangle.
    pub scissor: Rect,
    /// User clip planes (clip space).
    pub clip_planes: [[f32; 4]; 6],
    /// Vertex and pixel shaders (`INVALID_ID`: none).
    pub shaders: [u32; 2],
    /// Constants of the vertex and pixel stages, in uniform block layout.
    pub constants: [Vec<u32>; 2],
}

impl Context9 {
    fn new() -> Self {
        let none = ImageId {
            sid: INVALID_ID,
            face: 0,
            mipmap: 0,
        };
        let mut ts = vec![[0u32; TEXTURE_STATES]; STAGES];
        for stage in &mut ts {
            stage[ts::BIND_TEXTURE] = INVALID_ID;
        }
        // Direct3D 9's defaults (the driver sends every state on creation
        // anyway).
        let mut states = [0; RENDER_STATES];
        for (name, value) in [
            (rs::ZWRITEENABLE, 1),
            (rs::STENCILMASK, u32::MAX),
            (rs::STENCILWRITEMASK, u32::MAX),
            (rs::FOGEND, 1.0f32.to_bits()),
            (rs::FOGDENSITY, 1.0f32.to_bits()),
            (rs::POINTSIZE, 1.0f32.to_bits()),
            (rs::POINTSIZEMIN, 1.0f32.to_bits()),
            (rs::POINTSIZEMAX, 64.0f32.to_bits()),
            (rs::FILLMODE, 0x0004_0003),
            (rs::SHADEMODE, 2),
            (rs::SRCBLEND, 2),
            (rs::DSTBLEND, 1),
            (rs::BLENDEQUATION, 1),
            (rs::CULLMODE, 3),
            (rs::ZFUNC, 4),
            (rs::ALPHAFUNC, 8),
            (rs::STENCILFUNC, 8),
            (rs::STENCILFAIL, 1),
            (rs::STENCILZFAIL, 1),
            (rs::STENCILPASS, 1),
            (rs::FRONTWINDING, 1),
            (rs::COLORWRITEENABLE, 0xf),
            (rs::CCWSTENCILFUNC, 8),
            (rs::CCWSTENCILFAIL, 1),
            (rs::CCWSTENCILZFAIL, 1),
            (rs::CCWSTENCILPASS, 1),
            (rs::MULTISAMPLEMASK, u32::MAX),
            (rs::COLORWRITEENABLE1, 0xf),
            (rs::COLORWRITEENABLE1 + 1, 0xf),
            (rs::COLORWRITEENABLE1 + 2, 0xf),
            (rs::SRCBLENDALPHA, 2),
            (rs::DSTBLENDALPHA, 1),
            (rs::BLENDEQUATIONALPHA, 1),
        ] {
            states[name] = value;
        }
        Self {
            mob: INVALID_ID,
            rs: states,
            ts,
            targets: [none; TARGETS],
            viewport: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            zrange: (0.0, 1.0),
            scissor: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            clip_planes: [[0.0; 4]; 6],
            shaders: [INVALID_ID; 2],
            constants: [vec![0; CONSTANT_WORDS], vec![0; CONSTANT_WORDS]],
        }
    }

    /// A render state's value (0 if out of range).
    pub fn state(&self, name: usize) -> u32 {
        self.rs.get(name).copied().unwrap_or(0)
    }

    /// A render state's value as a float.
    pub fn state_f32(&self, name: usize) -> f32 {
        f32::from_bits(self.state(name))
    }

    /// A texture stage state's value (0 if out of range).
    pub fn texture_state(&self, stage: usize, name: usize) -> u32 {
        self.ts
            .get(stage)
            .and_then(|s| s.get(name))
            .copied()
            .unwrap_or(0)
    }
}

/// What the caller still has to do with a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing.
    Done,
    /// Run it on the GPU (clears, draws, blits, mipmaps).
    Gpu,
}

/// Every legacy context and shader object.
#[derive(Default)]
pub struct Legacy {
    contexts: HashMap<u32, Context9>,
    shaders: HashMap<u32, Shader9>,
}

impl Legacy {
    /// Forgets everything (device reset).
    pub fn reset(&mut self) {
        self.contexts.clear();
        self.shaders.clear();
    }

    /// A context.
    pub fn context(&self, cid: u32) -> Option<&Context9> {
        self.contexts.get(&cid)
    }

    /// A shader object.
    pub fn shader(&self, shid: u32) -> Option<&Shader9> {
        self.shaders.get(&shid)
    }

    fn context_mut(&mut self, cid: u32) -> Result<&mut Context9, String> {
        self.contexts
            .get_mut(&cid)
            .ok_or_else(|| format!("context {cid} is not defined"))
    }

    /// Executes a command on the state; `gpu`: whether a GPU runs the
    /// commands of [`Outcome::Gpu`].
    pub fn execute(
        &mut self,
        command: &LegacyCommand,
        gb: &mut Gb,
        guest: &GuestMemory,
        gpu: bool,
    ) -> Result<Outcome, String> {
        use LegacyCommand as C;
        match command {
            C::DefineContext { cid } => {
                if !self.contexts.contains_key(cid) && self.contexts.len() >= MAX_CONTEXTS {
                    return Err(format!("too many contexts (context {cid})"));
                }
                self.contexts.insert(*cid, Context9::new());
            }
            C::DestroyContext { cid } => {
                self.contexts.remove(cid);
            }
            C::BindContext { cid, mob, .. } => self.context_mut(*cid)?.mob = *mob,
            C::ReadbackContext { cid } | C::InvalidateContext { cid } => {
                self.context_mut(*cid)?;
            }
            C::DefineShader { shid, stage, size } => {
                if *size > MAX_SHADER_BYTES {
                    return Err(format!("shader {shid}: {size} bytes"));
                }
                if !self.shaders.contains_key(shid) && self.shaders.len() >= MAX_SHADERS {
                    return Err(format!("too many shaders (shader {shid})"));
                }
                self.shaders.insert(
                    *shid,
                    Shader9 {
                        stage: *stage,
                        size: *size,
                        code: None,
                    },
                );
            }
            C::DestroyShader { shid } => {
                self.shaders.remove(shid);
            }
            C::BindShader { shid, mob, offset } => {
                let shader = self
                    .shaders
                    .get_mut(shid)
                    .ok_or_else(|| format!("shader {shid} is not defined"))?;
                if *mob == INVALID_ID {
                    shader.code = None;
                    return Ok(Outcome::Done);
                }
                let mut bytes = vec![0u8; shader.size as usize];
                gb.read_mob(guest, *mob, u64::from(*offset), &mut bytes)
                    .map_err(|e| format!("shader {shid}: {e}"))?;
                dump_shader(shader.stage, &bytes);
                let words: Vec<u32> = bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                // The same code keeps its allocation: translations are
                // cached by it.
                if shader.code.as_deref() != Some(&words[..]) {
                    shader.code = Some(words.into());
                }
            }
            C::DefineInlineShader {
                shid, stage, code, ..
            } => {
                if !self.shaders.contains_key(shid) && self.shaders.len() >= MAX_SHADERS {
                    return Err(format!("too many shaders (shader {shid})"));
                }
                self.shaders.insert(
                    *shid,
                    Shader9 {
                        stage: *stage,
                        size: u32::try_from(code.len() * 4).unwrap_or(u32::MAX),
                        code: Some(code.clone().into()),
                    },
                );
            }
            C::DestroyInlineShader { shid, .. } => {
                self.shaders.remove(shid);
            }
            C::SetShader { cid, stage, shid } => {
                self.context_mut(*cid)?.shaders[stage_index(*stage)] = *shid;
            }
            C::SetShaderConst {
                cid,
                stage,
                kind,
                start,
                values,
            } => {
                let c = self.context_mut(*cid)?;
                set_constants(&mut c.constants[stage_index(*stage)], *kind, *start, values)?;
            }
            C::SetRenderStates { cid, states } => {
                let c = self.context_mut(*cid)?;
                for &(name, value) in states {
                    if let Some(slot) = c.rs.get_mut(name as usize) {
                        *slot = value;
                    }
                }
            }
            C::SetTextureStates { cid, states } => {
                let c = self.context_mut(*cid)?;
                for s in states {
                    if let Some(slot) =
                        c.ts.get_mut(s.stage as usize)
                            .and_then(|t| t.get_mut(s.name as usize))
                    {
                        *slot = s.value;
                    }
                }
            }
            C::SetRenderTarget { cid, slot, image } => {
                let c = self.context_mut(*cid)?;
                let target = c
                    .targets
                    .get_mut(*slot as usize)
                    .ok_or_else(|| format!("render target slot {slot}"))?;
                *target = *image;
            }
            C::SetViewport { cid, rect } => self.context_mut(*cid)?.viewport = *rect,
            C::SetZRange { cid, min, max } => self.context_mut(*cid)?.zrange = (*min, *max),
            C::SetScissorRect { cid, rect } => self.context_mut(*cid)?.scissor = *rect,
            C::SetClipPlane { cid, index, plane } => {
                let c = self.context_mut(*cid)?;
                if let Some(slot) = c.clip_planes.get_mut(*index as usize) {
                    *slot = *plane;
                }
            }
            C::SetTransform { cid, .. }
            | C::SetMaterial { cid, .. }
            | C::SetLightData { cid, .. }
            | C::SetLightEnabled { cid, .. } => {
                // The driver emulates the fixed-function pipeline with
                // shaders; these only matter to the host's own one.
                self.context_mut(*cid)?;
            }
            C::BeginQuery { cid, kind } => {
                self.context_mut(*cid)?;
                if *kind != 0 {
                    return Err(format!("query type {kind}"));
                }
                if gpu {
                    return Ok(Outcome::Gpu);
                }
            }
            C::EndQuery {
                cid, mob, offset, ..
            } => {
                self.context_mut(*cid)?;
                if gpu {
                    return Ok(Outcome::Gpu);
                }
                // Nothing is drawn without a GPU: report samples anyway, so
                // that nothing is culled.
                write_query(gb, guest, *mob, *offset, 1)?;
            }
            C::WaitForQuery { cid, .. } => {
                self.context_mut(*cid)?;
                if gpu {
                    return Ok(Outcome::Gpu);
                }
            }
            C::Clear { cid, .. } | C::DrawPrimitives { cid, .. } => {
                self.context_mut(*cid)?;
                return Ok(Outcome::Gpu);
            }
            C::StretchBlt { .. } | C::GenerateMipmaps { .. } => return Ok(Outcome::Gpu),
        }
        Ok(Outcome::Done)
    }
}

fn stage_index(stage: Stage) -> usize {
    match stage {
        Stage::Vertex => 0,
        Stage::Pixel => 1,
    }
}

/// Stores constants in the uniform block layout.
fn set_constants(
    block: &mut [u32],
    kind: ConstKind,
    start: u32,
    values: &[u32],
) -> Result<(), String> {
    let (base, registers, per) = match kind {
        ConstKind::Float => (constants::FLOATS, 256, 4),
        ConstKind::Int => (constants::INTS, 16, 4),
        ConstKind::Bool => (constants::BOOLS, 16, 1),
    };
    let count = values.len() / per;
    if start as usize + count > registers {
        return Err(format!(
            "{kind:?} constants {start}+{count} beyond {registers}"
        ));
    }
    for (n, value) in values.chunks_exact(per).enumerate() {
        let register = start as usize + n;
        match kind {
            ConstKind::Bool => {
                // Four per vec4.
                let word = base as usize * 4 + register;
                block[word] = u32::from(value[0] != 0);
            }
            _ => {
                let word = (base as usize + register) * 4;
                block[word..word + 4].copy_from_slice(value);
            }
        }
    }
    Ok(())
}

/// Writes an `SVGA3dQueryResult` (succeeded) into a MOB.
pub(crate) fn write_query(
    gb: &Gb,
    guest: &GuestMemory,
    mob: u32,
    offset: u32,
    result: u32,
) -> Result<(), String> {
    write_query_state(gb, guest, mob, offset, 1, result)
}

/// Writes an `SVGA3dQueryResult` with `state` (`SVGA3dQueryState`: 0
/// pending, 1 succeeded) into a MOB.
pub(crate) fn write_query_state(
    gb: &Gb,
    guest: &GuestMemory,
    mob: u32,
    offset: u32,
    state: u32,
    result: u32,
) -> Result<(), String> {
    let words = [12u32, state, result];
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    gb.write_mob(guest, mob, u64::from(offset), &bytes)
}

/// Diagnostics: with `RSVGA_SHADER_DUMP=DIR`, each distinct Direct3D 9
/// shader bound is saved as `DIR/<stage>-<hash>.d3d9`.
fn dump_shader(stage: Stage, code: &[u8]) {
    let Some(dir) = std::env::var_os("RSVGA_SHADER_DUMP") else {
        return;
    };
    let hash = code.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let path = std::path::Path::new(&dir).join(format!("{stage:?}-{hash:016x}.d3d9"));
    if !path.exists()
        && let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, code))
    {
        crate::log!("shader dump {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_land_in_the_uniform_block_layout() {
        let mut block = vec![0u32; CONSTANT_WORDS];
        set_constants(&mut block, ConstKind::Float, 2, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(&block[8..16], &[1, 2, 3, 4, 5, 6, 7, 8]);
        set_constants(&mut block, ConstKind::Int, 1, &[9, 0, 1, 0]).unwrap();
        let ints = (constants::INTS as usize + 1) * 4;
        assert_eq!(&block[ints..ints + 4], &[9, 0, 1, 0]);
        set_constants(&mut block, ConstKind::Bool, 5, &[1, 0, 7]).unwrap();
        let bools = constants::BOOLS as usize * 4;
        assert_eq!(&block[bools + 5..bools + 8], &[1, 0, 1]);
        assert!(set_constants(&mut block, ConstKind::Float, 255, &[0; 8]).is_err());
        assert!(set_constants(&mut block, ConstKind::Bool, 15, &[0, 0]).is_err());
    }
}
