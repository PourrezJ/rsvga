//! Command buffer execution (`SVGA_CAP_COMMAND_BUFFERS`).
//!
//! Buffers run synchronously, inside the guest's write to
//! `SVGA_REG_COMMAND_LOW`: when the write returns, the status is already in
//! the header. Drivers read it right away (device-context commands must
//! complete that way) and otherwise wait for `SVGA_IRQFLAG_COMMAND_BUFFER`.
//!
//! A context stops when one of its buffers fails, as on the real device:
//! the driver then preempts what it queued meanwhile and restarts the
//! context through the device context. Buffers submitted to a stopped
//! context wait in its queue, with status `NONE`.

use std::collections::VecDeque;

use svga_core::cmdbuf::{
    self, DeviceCommand, ERROR_OFFSET_OFFSET, Header, Location, STATUS_OFFSET,
};
use svga_proto::{cb, irq};

use crate::device::{State, Svga, trace, trace_level};
use crate::log;

/// One regular context.
#[derive(Debug)]
struct Context {
    running: bool,
    /// Headers submitted while stopped, oldest first.
    queued: VecDeque<u64>,
}

impl Default for Context {
    /// Contexts accept buffers from reset: drivers start them explicitly
    /// anyway, and a stopped context would only stall one that does not.
    fn default() -> Self {
        Self {
            running: true,
            queued: VecDeque::new(),
        }
    }
}

/// Command buffer contexts and counters.
#[derive(Debug, Default)]
pub struct CommandBuffers {
    contexts: [Context; cb::context::MAX as usize],
    /// Buffers that completed.
    pub completed: u64,
    /// Buffers that failed (header or command error).
    pub failed: u64,
}

impl CommandBuffers {
    /// Back to the reset state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn context(&mut self, id: u32) -> Option<&mut Context> {
        self.contexts.get_mut(usize::try_from(id).ok()?)
    }
}

fn read_header(state: &State, pa: u64) -> Option<Header> {
    let mut raw = [0u8; cb::HEADER_BYTES as usize];
    state.guest.read(pa, &mut raw).then(|| Header::parse(&raw))
}

/// The buffer contents as words, or `None` if they are not in guest RAM.
fn read_contents(state: &State, header: &Header) -> Option<Vec<u32>> {
    let Location::Physical(base) = header.location else {
        return None; // MOBs come with guest-backed objects
    };
    let mut bytes = vec![0u8; usize::try_from(header.length).ok()?];
    if !state.guest.read(base, &mut bytes) {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Writes the outcome into the header, then raises the interrupt.
fn finish(state: &mut State, pa: u64, header: &Header, status: u32, error_offset: u32) {
    // errorOffset first: the guest polls status.
    let written = state.guest.write(
        pa.saturating_add(ERROR_OFFSET_OFFSET),
        &error_offset.to_le_bytes(),
    ) && state
        .guest
        .write(pa.saturating_add(STATUS_OFFSET), &status.to_le_bytes());
    if !written {
        log!("command buffer header at {pa:#x} vanished from guest memory");
    }
    if status == cb::status::COMPLETED {
        state.cb.completed = state.cb.completed.saturating_add(1);
    } else {
        state.cb.failed = state.cb.failed.saturating_add(1);
    }
    if header.wants_irq() {
        let mask = state.regs.irq_mask();
        state.irq.raise(irq::COMMAND_BUFFER, mask);
    }
}

impl Svga {
    /// Handles a write to `SVGA_REG_COMMAND_LOW`.
    pub(crate) fn submit_command_buffer(&self, state: &mut State, pa: u64, context: u32) {
        let Some(header) = read_header(state, pa) else {
            log!("command buffer header at {pa:#x} is not in guest memory");
            let mask = state.regs.irq_mask();
            state.irq.raise(irq::ERROR, mask);
            return;
        };
        if context == cb::context::DEVICE {
            return self.run_device(state, pa, &header);
        }
        let Some(ctx) = state.cb.context(context) else {
            trace(format!("command buffer for unknown context {context}"));
            return finish(state, pa, &header, cb::status::SUBMISSION_ERROR, 0);
        };
        if !ctx.running {
            if ctx.queued.len() >= cb::MAX_QUEUED_PER_CONTEXT {
                return finish(state, pa, &header, cb::status::QUEUE_FULL, 0);
            }
            ctx.queued.push_back(pa);
            return;
        }
        self.run_buffer(state, pa, &header, context);
    }

    /// Runs the commands of a regular context's buffer.
    fn run_buffer(&self, state: &mut State, pa: u64, header: &Header, context: u32) {
        let contents = header
            .validate(cb::MAX_SIZE_DEFAULT)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                read_contents(state, header).ok_or_else(|| "contents not in guest RAM".into())
            });
        let words = match contents {
            Ok(words) => words,
            Err(error) => {
                log!("command buffer refused: {error}");
                return finish(state, pa, header, cb::status::CB_HEADER_ERROR, 0);
            }
        };
        let limits = cmdbuf::buffer_limits(&self.limits);
        let (entries, error) = cmdbuf::decode_buffer(&words, header.offset, &limits);
        if trace_level() >= 1 {
            trace(format!(
                "command buffer, context {context}, {} bytes, {} commands",
                header.length,
                entries.len()
            ));
        }
        // DX commands apply to the context the header names.
        let dx_context = (header.flags & cb::flag::DX_CONTEXT != 0).then_some(header.dx_context);
        for entry in entries {
            self.execute(state, entry.command, dx_context);
        }
        if let Some(gpu) = state.gpu.as_mut() {
            gpu.flush();
        }
        match error {
            None => finish(state, pa, header, cb::status::COMPLETED, 0),
            Some((offset, error)) => {
                log!("command buffer error at offset {offset}: {error}; context {context} stops");
                if let Some(ctx) = state.cb.context(context) {
                    ctx.running = false;
                }
                finish(state, pa, header, cb::status::COMMAND_ERROR, offset);
            }
        }
    }

    /// Runs a device-context buffer: start, stop and preempt contexts.
    fn run_device(&self, state: &mut State, pa: u64, header: &Header) {
        let words = header
            .validate(cb::MAX_SIZE_DEFAULT)
            .ok()
            .and_then(|()| read_contents(state, header));
        let Some(words) = words else {
            log!("device context buffer refused");
            return finish(state, pa, header, cb::status::CB_HEADER_ERROR, 0);
        };
        let start = usize::try_from(header.offset.div_euclid(4)).unwrap_or(usize::MAX);
        let commands = match cmdbuf::decode_device(words.get(start..).unwrap_or_default()) {
            Ok(commands) => commands,
            Err(error) => {
                log!("unknown device context command {}", error.id);
                let offset = header.offset.saturating_add(error.offset);
                return finish(state, pa, header, cb::status::COMMAND_ERROR, offset);
            }
        };
        for (offset, command) in commands {
            trace(format!("device context: {command:?}"));
            if !self.device_command(state, command) {
                log!("device context command for an unknown context: {command:?}");
                let offset = header.offset.saturating_add(offset);
                return finish(state, pa, header, cb::status::COMMAND_ERROR, offset);
            }
        }
        finish(state, pa, header, cb::status::COMPLETED, 0);
    }

    /// Applies one device-context command; false if its context is unknown.
    fn device_command(&self, state: &mut State, command: DeviceCommand) -> bool {
        let (context, start) = match command {
            DeviceCommand::Nop => return true,
            DeviceCommand::StartStop { enable, context } => (context, Some(enable)),
            DeviceCommand::StartQueue { context } => (context, Some(true)),
            DeviceCommand::AsyncStopQueue { context } => (context, Some(false)),
            DeviceCommand::Preempt {
                context,
                ignore_id_zero,
            } => {
                return self.preempt(state, context, ignore_id_zero);
            }
            DeviceCommand::EmptyQueue { context } => return self.preempt(state, context, false),
        };
        let Some(ctx) = state.cb.context(context) else {
            return false;
        };
        if let Some(enable) = start {
            ctx.running = enable;
        }
        // A started context runs what it queued while stopped.
        while state.cb.context(context).is_some_and(|c| c.running) {
            let Some(pa) = state.cb.context(context).and_then(|c| c.queued.pop_front()) else {
                break;
            };
            if let Some(header) = read_header(state, pa) {
                self.run_buffer(state, pa, &header, context);
            }
        }
        true
    }

    /// Hands queued buffers back to the guest with status `PREEMPTED`.
    fn preempt(&self, state: &mut State, context: u32, ignore_id_zero: bool) -> bool {
        let Some(ctx) = state.cb.context(context) else {
            return false;
        };
        let queued: Vec<u64> = ctx.queued.drain(..).collect();
        for pa in queued {
            let Some(header) = read_header(state, pa) else {
                continue;
            };
            if ignore_id_zero && header.id == 0 {
                if let Some(ctx) = state.cb.context(context) {
                    ctx.queued.push_back(pa);
                }
                continue;
            }
            finish(state, pa, &header, cb::status::PREEMPTED, 0);
        }
        true
    }
}
