//! Guest-facing logic of the rsvga device, independent of any transport.
//!
//! Everything in this crate treats guest data as hostile: it is safe Rust
//! only (`unsafe` is forbidden workspace-wide), never panics on guest input,
//! and bounds every size it derives from guest values.

pub mod command;
pub mod decode;
pub mod fifo;

pub use command::Command;
pub use decode::{Decode, DecodeError, Limits, WordSource, decode};
pub use fifo::{FifoError, FifoMemory, Pass, Snapshot, StopReason, run};
