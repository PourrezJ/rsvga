//! Stream output, as Vulkan transform feedback: the last stage before the
//! rasterizer writes each captured output register again, to a variable of
//! just the captured components, decorated with its buffer, the buffer's
//! stride and its offset in the vertex. Those variables take locations the
//! shader does not use, from the top, so the next stage never reads them;
//! a captured whole position is the `Position` builtin itself.

use super::*;

const CAP_TRANSFORM_FEEDBACK: u32 = 53;
pub(super) const MODE_XFB: u32 = 11;
const DECORATION_XFB_BUFFER: u32 = 36;
const DECORATION_XFB_STRIDE: u32 = 37;

/// One captured output: `mask` components (contiguous) of register
/// `register`, to buffer `buffer` at byte `offset` of each vertex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Capture {
    /// Stream-output buffer (0 to 3).
    pub buffer: u32,
    /// Output register.
    pub register: u32,
    /// Components (bit 0: x).
    pub mask: u8,
    /// Byte offset in the buffer's vertex.
    pub offset: u32,
}

impl Tx<'_> {
    /// Declares the captures of `options` (after the outputs).
    pub(super) fn declare_captures(&mut self) -> Result<(), TranslateError> {
        let captures = self.options.stream_output.clone();
        if captures.is_empty() {
            return Ok(());
        }
        self.m.capability(CAP_TRANSFORM_FEEDBACK);
        let strides = self.options.so_strides;
        let mut free = (0..32u32)
            .rev()
            .filter(|l| !self.output_locations.contains(l))
            .collect::<Vec<_>>()
            .into_iter();
        for c in captures {
            let stride = strides.get(c.buffer as usize).copied().unwrap_or(0);
            if c.buffer >= 4 || stride == 0 || c.offset % 4 != 0 {
                return unsupported(format_args!("stream output to buffer {}", c.buffer));
            }
            let Some(&local) = self.outputs.get(&c.register) else {
                // Not written by the shader: nothing to capture.
                continue;
            };
            let first = c.mask.trailing_zeros().min(4);
            let count = c.mask.count_ones();
            // Contiguous components: shifted down, the mask is 2^count - 1.
            let run = u32::from(c.mask).checked_shr(first).unwrap_or(0);
            if count == 0 || run.checked_add(1) != 1u32.checked_shl(count) {
                return unsupported(format_args!("stream output of components {:#x}", c.mask));
            }
            let xfb = [
                [DECORATION_XFB_BUFFER, c.buffer],
                [DECORATION_XFB_STRIDE, stride],
                [DECORATION_OFFSET, c.offset],
            ];
            if Some(c.register) == self.position_register && c.mask == 0xf {
                if let Some(var) = self.position_var {
                    for d in &xfb {
                        self.m.decorate(var, d);
                    }
                    continue;
                }
            }
            let location = free
                .next()
                .ok_or_else(|| TranslateError("no location left for stream output".into()))?;
            let ty = if count == 1 {
                self.m.scalar(Scalar::Float)
            } else {
                self.m.vector(Scalar::Float, count)
            };
            let var = self.output_var(
                ty,
                &[&[DECORATION_LOCATION, location], &xfb[0], &xfb[1], &xfb[2]],
            );
            self.xfb_sinks.push((var, local, first, count));
        }
        Ok(())
    }

    /// Writes the captured registers to their variables.
    pub(super) fn write_captures(&mut self) {
        let f = self.m.scalar(Scalar::Float);
        for (var, local, first, count) in self.xfb_sinks.clone() {
            let v = self.m.emit(op::LOAD, self.f4, &[local]);
            let value = if count == 1 {
                self.m.emit(op::COMPOSITE_EXTRACT, f, &[v, first])
            } else {
                let ty = self.m.vector(Scalar::Float, count);
                let mut operands = vec![v, v];
                operands.extend(first..first.saturating_add(count));
                self.m.emit(op::VECTOR_SHUFFLE, ty, &operands)
            };
            self.m.emit_void(op::STORE, &[var, value]);
        }
    }
}
