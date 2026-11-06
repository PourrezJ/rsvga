//! The FIFO command ring shared with the guest.
//!
//! FIFO memory starts with registers ([`svga_proto::fifo`]) followed by a ring
//! of commands between byte offsets `MIN` and `MAX`. The guest writes commands
//! and advances `NEXT_CMD`; the device consumes them and advances `STOP`.
//!
//! The guest can rewrite FIFO memory at any time, so each processing pass
//! starts by taking a validated [`Snapshot`] of the ring registers, and only
//! the snapshot is trusted for the rest of the pass.

use core::fmt;

use svga_proto::fifo;

use crate::command::Command;
use crate::decode::{Decode, DecodeError, Limits, WordSource, decode};

/// Access to FIFO memory, one little-endian 32-bit word at a time.
///
/// The real implementation reads a shared mapping with volatile accesses;
/// tests and fuzzers use plain slices. Implementations must not panic on an
/// out-of-range index (reads return 0, writes are ignored), although the ring
/// logic never produces one.
pub trait FifoMemory {
    /// Size of FIFO memory, in words.
    fn len_words(&self) -> usize;
    /// Reads the word at `index`.
    fn load(&self, index: usize) -> u32;
    /// Writes the word at `index`.
    fn store(&mut self, index: usize, value: u32);
}

impl FifoMemory for [u32] {
    fn len_words(&self) -> usize {
        self.len()
    }

    fn load(&self, index: usize) -> u32 {
        self.get(index).copied().unwrap_or(0)
    }

    fn store(&mut self, index: usize, value: u32) {
        if let Some(word) = self.get_mut(index) {
            *word = value;
        }
    }
}

impl FifoMemory for Vec<u32> {
    fn len_words(&self) -> usize {
        self.as_slice().len_words()
    }

    fn load(&self, index: usize) -> u32 {
        self.as_slice().load(index)
    }

    fn store(&mut self, index: usize, value: u32) {
        self.as_mut_slice().store(index, value);
    }
}

/// Smallest ring the device accepts, in bytes.
pub const MIN_RING_BYTES: u32 = 1024;

/// Why the ring registers were rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FifoError {
    /// FIFO memory cannot even hold the four mandatory registers.
    MemoryTooSmall,
    /// `MIN`, `MAX`, `NEXT_CMD` or `STOP` is not a multiple of 4.
    Misaligned,
    /// `MIN` leaves no room for the mandatory registers.
    MinTooSmall(u32),
    /// `MAX` is not above `MIN`.
    MaxNotAboveMin {
        /// Guest `MIN`.
        min: u32,
        /// Guest `MAX`.
        max: u32,
    },
    /// `MAX` lies beyond FIFO memory.
    MaxBeyondMemory(u32),
    /// The ring is smaller than [`MIN_RING_BYTES`].
    RingTooSmall(u32),
    /// `NEXT_CMD` is outside `[MIN, MAX)`.
    NextOutOfRange(u32),
    /// `STOP` is outside `[MIN, MAX)`.
    StopOutOfRange(u32),
}

impl fmt::Display for FifoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MemoryTooSmall => write!(f, "FIFO memory is too small"),
            Self::Misaligned => write!(f, "FIFO ring registers are not 4-byte aligned"),
            Self::MinTooSmall(min) => write!(f, "FIFO MIN {min:#x} overlaps the registers"),
            Self::MaxNotAboveMin { min, max } => {
                write!(f, "FIFO MAX {max:#x} is not above MIN {min:#x}")
            }
            Self::MaxBeyondMemory(max) => write!(f, "FIFO MAX {max:#x} is beyond FIFO memory"),
            Self::RingTooSmall(size) => write!(f, "FIFO ring of {size} bytes is too small"),
            Self::NextOutOfRange(next) => write!(f, "FIFO NEXT_CMD {next:#x} is out of range"),
            Self::StopOutOfRange(stop) => write!(f, "FIFO STOP {stop:#x} is out of range"),
        }
    }
}

impl std::error::Error for FifoError {}

/// Validated copy of the ring registers, in bytes from the start of FIFO
/// memory. Invariants: all four values are multiples of 4,
/// `16 <= min < max <= memory size`, `max - min >= MIN_RING_BYTES`, and
/// `next`, `stop` lie in `[min, max)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    min: u32,
    max: u32,
    next: u32,
    stop: u32,
}

const MANDATORY_BYTES: u32 = fifo::MANDATORY_REGS * 4;

impl Snapshot {
    /// Reads and validates the ring registers.
    pub fn capture<M: FifoMemory + ?Sized>(mem: &M) -> Result<Self, FifoError> {
        let mandatory_words = usize::try_from(fifo::MANDATORY_REGS).unwrap_or(usize::MAX);
        if mem.len_words() < mandatory_words {
            return Err(FifoError::MemoryTooSmall);
        }
        let reg = |index: u32| mem.load(usize::try_from(index).unwrap_or(usize::MAX));
        let (min, max) = (reg(fifo::MIN), reg(fifo::MAX));
        let (next, stop) = (reg(fifo::NEXT_CMD), reg(fifo::STOP));

        if (min | max | next | stop) % 4 != 0 {
            return Err(FifoError::Misaligned);
        }
        if min < MANDATORY_BYTES {
            return Err(FifoError::MinTooSmall(min));
        }
        if max <= min {
            return Err(FifoError::MaxNotAboveMin { min, max });
        }
        let memory_bytes = u64::try_from(mem.len_words())
            .unwrap_or(u64::MAX)
            .saturating_mul(4);
        if u64::from(max) > memory_bytes {
            return Err(FifoError::MaxBeyondMemory(max));
        }
        let ring = max.wrapping_sub(min); // max > min, checked above
        if ring < MIN_RING_BYTES {
            return Err(FifoError::RingTooSmall(ring));
        }
        if !(min..max).contains(&next) {
            return Err(FifoError::NextOutOfRange(next));
        }
        if !(min..max).contains(&stop) {
            return Err(FifoError::StopOutOfRange(stop));
        }
        Ok(Self {
            min,
            max,
            next,
            stop,
        })
    }

    /// First byte of the ring.
    pub fn min(&self) -> u32 {
        self.min
    }

    /// One past the last byte of the ring.
    pub fn max(&self) -> u32 {
        self.max
    }

    /// Where the guest will write its next command.
    pub fn next(&self) -> u32 {
        self.next
    }

    /// Where the device will read its next command.
    pub fn stop(&self) -> u32 {
        self.stop
    }

    /// Size of the ring in bytes.
    pub fn ring_bytes(&self) -> u32 {
        self.max.wrapping_sub(self.min)
    }

    /// Bytes written by the guest and not yet consumed.
    pub fn pending_bytes(&self) -> u32 {
        // All values lie in [min, max), so none of these can wrap.
        if self.next >= self.stop {
            self.next.wrapping_sub(self.stop)
        } else {
            self.ring_bytes()
                .wrapping_sub(self.stop.wrapping_sub(self.next))
        }
    }

    /// Whether FIFO register `index` exists, i.e. lies below the ring.
    pub fn has_register(&self, index: u32) -> bool {
        u64::from(index).saturating_add(1).saturating_mul(4) <= u64::from(self.min)
    }

    /// Advances byte offset `pos` (inside the ring) by `bytes`, wrapping at
    /// `MAX`. `bytes` must not exceed the ring size.
    fn advance(&self, pos: u32, bytes: u32) -> u32 {
        let target = u64::from(pos).saturating_add(u64::from(bytes));
        let wrapped = if target >= u64::from(self.max) {
            target.saturating_sub(u64::from(self.ring_bytes()))
        } else {
            target
        };
        u32::try_from(wrapped).unwrap_or(self.min)
    }
}

/// The unconsumed part of the ring, seen as a [`WordSource`].
struct RingReader<'m, M: ?Sized> {
    mem: &'m M,
    snap: Snapshot,
    pos: u32,
    pending: u32,
}

impl<M: FifoMemory + ?Sized> WordSource for RingReader<'_, M> {
    fn available(&self) -> usize {
        usize::try_from(self.pending / 4).unwrap_or(0)
    }

    fn capacity(&self) -> usize {
        // NEXT_CMD == STOP means "empty", so one word always stays free.
        usize::try_from((self.snap.ring_bytes() / 4).saturating_sub(1)).unwrap_or(0)
    }

    fn peek(&self, offset: usize) -> Option<u32> {
        if offset >= self.available() {
            return None;
        }
        let bytes = u32::try_from(offset).ok()?.checked_mul(4)?;
        let at = self.snap.advance(self.pos, bytes);
        self.mem.load(usize::try_from(at / 4).ok()?).into()
    }
}

/// Why a processing pass ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// Every pending command was processed.
    Empty,
    /// The next command is only partly written; retry later.
    Incomplete,
    /// The command budget of the pass was used up; more work is pending.
    Budget,
    /// A command was rejected. The device must stop processing the FIFO and
    /// raise [`svga_proto::irq::ERROR`].
    Invalid(DecodeError),
    /// The ring registers are invalid. Same consequence as `Invalid`.
    BadConfig(FifoError),
}

/// Outcome of [`run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pass {
    /// Commands handed to the handler.
    pub executed: usize,
    /// Why the pass ended.
    pub stop: StopReason,
}

/// Processes at most `budget` commands from the ring.
///
/// Each command is decoded from a private copy, `STOP` is advanced past it,
/// and only then is it handed to `handle`. Commands written by the guest
/// after the snapshot are left for the next pass.
pub fn run<M, F>(mem: &mut M, limits: &Limits, budget: usize, mut handle: F) -> Pass
where
    M: FifoMemory + ?Sized,
    F: FnMut(Command),
{
    let snap = match Snapshot::capture(mem) {
        Ok(snap) => snap,
        Err(error) => {
            return Pass {
                executed: 0,
                stop: StopReason::BadConfig(error),
            };
        }
    };
    let stop_index = usize::try_from(fifo::STOP).unwrap_or(usize::MAX);
    let mut pos = snap.stop;
    let mut pending = snap.pending_bytes();
    let mut executed = 0usize;

    loop {
        if pending == 0 {
            return Pass {
                executed,
                stop: StopReason::Empty,
            };
        }
        if executed >= budget {
            return Pass {
                executed,
                stop: StopReason::Budget,
            };
        }
        let reader = RingReader {
            mem: &*mem,
            snap,
            pos,
            pending,
        };
        match decode(&reader, limits) {
            Decode::Ready { command, words } => {
                // `words <= capacity < ring_bytes / 4`, so this fits in u32.
                let bytes = u32::try_from(words).unwrap_or(u32::MAX).saturating_mul(4);
                pos = snap.advance(pos, bytes);
                pending = pending.saturating_sub(bytes);
                mem.store(stop_index, pos);
                executed = executed.saturating_add(1);
                handle(command);
            }
            Decode::Incomplete => {
                return Pass {
                    executed,
                    stop: StopReason::Incomplete,
                };
            }
            Decode::Invalid(error) => {
                return Pass {
                    executed,
                    stop: StopReason::Invalid(error),
                };
            }
        }
    }
}
