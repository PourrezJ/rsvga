//! Direct3D 10/11 shaders, as the VMware driver sends them (VGPU10 token
//! streams), translated to SPIR-V for Vulkan.
//!
//! [`decode`] reads the tokens into instructions and operands, refusing
//! malformed or overlong programs; [`translate`] emits a SPIR-V module and
//! the descriptors and interface locations it uses. Everything here treats
//! the program as hostile input: safe Rust, bounded, never panics.

pub mod decode;
pub mod opcode;
pub mod spirv;
pub mod translate;

pub use decode::{DecodeError, Program, Stage, decode};
pub use spirv::Scalar;
pub use translate::{Binding, BindingKind, Options, Shader, TranslateError, binding, translate};

/// Decodes then translates a shader's words (its code; anything after the
/// program's length, such as the VMware signature block, is ignored).
pub fn translate_words(words: &[u32], options: &Options) -> Result<Shader, TranslateError> {
    let program = decode(words).map_err(|e| TranslateError(e.to_string()))?;
    translate(&program, options)
}

#[cfg(test)]
mod tests;
