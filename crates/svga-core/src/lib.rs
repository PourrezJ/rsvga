//! Guest-facing logic of the rsvga device, independent of any transport.
//!
//! Everything in this crate treats guest data as hostile: it is safe Rust
//! only (`unsafe` is forbidden workspace-wide), never panics on guest input,
//! and bounds every size it derives from guest values.

pub mod cmdbuf;
pub mod command;
pub mod cotable;
pub mod decode;
pub mod dx;
pub mod fifo;
pub mod gb;
pub mod gmr;
pub mod layout;
pub mod legacy;
pub mod mob;
pub mod regs;
pub mod screen;

pub use command::Command;
pub use decode::{Decode, DecodeError, Limits, WordSource, decode};
pub use fifo::{FifoError, FifoMemory, Pass, Snapshot, StopReason, run};
pub use regs::{Config, Effect, Mode, Registers};
