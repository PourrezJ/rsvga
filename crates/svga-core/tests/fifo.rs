#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    missing_docs
)]

use svga_core::command::{Command, Rect};
use svga_core::{DecodeError, FifoError, Limits, Pass, Snapshot, StopReason, run};
use svga_proto::{cmd, fifo};

const MIN: u32 = fifo::NUM_REGS * 4;
const RING: u32 = 1024;

/// FIFO memory with an empty ring of `ring` bytes right after the registers.
fn new_fifo(ring: u32) -> Vec<u32> {
    let max = MIN + ring;
    let mut mem = vec![0u32; (max / 4) as usize];
    mem[fifo::MIN as usize] = MIN;
    mem[fifo::MAX as usize] = max;
    mem[fifo::NEXT_CMD as usize] = MIN;
    mem[fifo::STOP as usize] = MIN;
    mem
}

fn set_position(mem: &mut [u32], pos: u32) {
    mem[fifo::NEXT_CMD as usize] = pos;
    mem[fifo::STOP as usize] = pos;
}

/// Writes `words` at NEXT_CMD, wrapping like a guest driver, and commits them.
fn push(mem: &mut [u32], words: &[u32]) {
    let (min, max) = (mem[fifo::MIN as usize], mem[fifo::MAX as usize]);
    let mut next = mem[fifo::NEXT_CMD as usize];
    for &word in words {
        mem[(next / 4) as usize] = word;
        next += 4;
        if next == max {
            next = min;
        }
    }
    mem[fifo::NEXT_CMD as usize] = next;
}

fn drain(mem: &mut Vec<u32>, budget: usize) -> (Vec<Command>, Pass) {
    let mut out = Vec::new();
    let pass = run(mem, &Limits::default(), budget, |command| out.push(command));
    (out, pass)
}

fn stop(mem: &[u32]) -> u32 {
    mem[fifo::STOP as usize]
}

#[test]
fn an_empty_ring_does_nothing() {
    let mut mem = new_fifo(RING);
    let (commands, pass) = drain(&mut mem, usize::MAX);
    assert!(commands.is_empty());
    assert_eq!(
        pass,
        Pass {
            executed: 0,
            stop: StopReason::Empty
        }
    );
}

#[test]
fn commands_are_consumed_in_order() {
    let mut mem = new_fifo(RING);
    push(&mut mem, &[cmd::UPDATE, 0, 0, 640, 480]);
    push(&mut mem, &[cmd::FENCE, 42]);
    push(&mut mem, &[cmd::NOP]);
    let (commands, pass) = drain(&mut mem, usize::MAX);
    assert_eq!(
        commands,
        [
            Command::Update(Rect {
                x: 0,
                y: 0,
                width: 640,
                height: 480
            }),
            Command::Fence(42),
            Command::Nop
        ]
    );
    assert_eq!(
        pass,
        Pass {
            executed: 3,
            stop: StopReason::Empty
        }
    );
    assert_eq!(stop(&mem), mem[fifo::NEXT_CMD as usize]);
}

#[test]
fn a_command_may_wrap_around_the_end_of_the_ring() {
    let mut mem = new_fifo(RING);
    set_position(&mut mem, MIN + RING - 8); // two words before MAX
    push(&mut mem, &[cmd::UPDATE, 1, 2, 3, 4]);
    let (commands, pass) = drain(&mut mem, usize::MAX);
    assert_eq!(
        commands,
        [Command::Update(Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4
        })]
    );
    assert_eq!(pass.stop, StopReason::Empty);
    assert_eq!(stop(&mem), MIN + 12);
}

#[test]
fn a_partial_command_is_left_in_place() {
    let mut mem = new_fifo(RING);
    push(&mut mem, &[cmd::FENCE, 1]);
    push(&mut mem, &[cmd::UPDATE, 1, 2]); // the guest has not finished writing
    let (commands, pass) = drain(&mut mem, usize::MAX);
    assert_eq!(commands, [Command::Fence(1)]);
    assert_eq!(
        pass,
        Pass {
            executed: 1,
            stop: StopReason::Incomplete
        }
    );
    assert_eq!(stop(&mem), MIN + 8);

    push(&mut mem, &[3, 4]);
    let (commands, pass) = drain(&mut mem, usize::MAX);
    assert_eq!(
        commands,
        [Command::Update(Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4
        })]
    );
    assert_eq!(pass.stop, StopReason::Empty);
}

#[test]
fn an_invalid_command_stops_the_fifo_without_being_consumed() {
    let mut mem = new_fifo(RING);
    push(&mut mem, &[cmd::NOP]);
    push(&mut mem, &[2, 0, 0]);
    push(&mut mem, &[cmd::NOP]);
    let (commands, pass) = drain(&mut mem, usize::MAX);
    assert_eq!(commands, [Command::Nop]);
    assert_eq!(
        pass.stop,
        StopReason::Invalid(DecodeError::UnknownCommand(2))
    );
    assert_eq!(stop(&mem), MIN + 4);
}

#[test]
fn a_command_larger_than_the_ring_is_rejected() {
    let mut mem = new_fifo(RING);
    push(&mut mem, &[cmd::ESCAPE, 0, 2000]);
    let (_, pass) = drain(&mut mem, usize::MAX);
    assert!(matches!(
        pass.stop,
        StopReason::Invalid(DecodeError::CommandTooLarge { .. })
    ));
}

#[test]
fn the_budget_bounds_a_pass() {
    let mut mem = new_fifo(RING);
    for _ in 0..10 {
        push(&mut mem, &[cmd::NOP]);
    }
    let (_, pass) = drain(&mut mem, 3);
    assert_eq!(
        pass,
        Pass {
            executed: 3,
            stop: StopReason::Budget
        }
    );
    let (_, pass) = drain(&mut mem, usize::MAX);
    assert_eq!(
        pass,
        Pass {
            executed: 7,
            stop: StopReason::Empty
        }
    );
}

#[test]
fn only_stop_is_written() {
    let mut mem = new_fifo(RING);
    push(&mut mem, &[cmd::UPDATE, 1, 2, 3, 4]);
    let before = mem.clone();
    drain(&mut mem, usize::MAX);
    for (index, (old, new)) in before.iter().zip(&mem).enumerate() {
        if index != fifo::STOP as usize {
            assert_eq!(old, new, "word {index} changed");
        }
    }
}

type Corruption = fn(&mut Vec<u32>);

#[test]
fn ring_registers_are_validated() {
    let cases: [(Corruption, FifoError); 8] = [
        (|m| m[fifo::NEXT_CMD as usize] += 2, FifoError::Misaligned),
        (|m| m[fifo::MIN as usize] = 12, FifoError::MinTooSmall(12)),
        (
            |m| m[fifo::MAX as usize] = MIN,
            FifoError::MaxNotAboveMin { min: MIN, max: MIN },
        ),
        (
            |m| m[fifo::MAX as usize] = MIN + RING + 4,
            FifoError::MaxBeyondMemory(MIN + RING + 4),
        ),
        (
            |m| m[fifo::MAX as usize] = MIN + 512,
            FifoError::RingTooSmall(512),
        ),
        (
            |m| m[fifo::NEXT_CMD as usize] = MIN + RING,
            FifoError::NextOutOfRange(MIN + RING),
        ),
        (|m| m[fifo::STOP as usize] = 0, FifoError::StopOutOfRange(0)),
        (|m| m.truncate(3), FifoError::MemoryTooSmall),
    ];
    for (corrupt, expected) in cases {
        let mut mem = new_fifo(RING);
        corrupt(&mut mem);
        assert_eq!(Snapshot::capture(&mem), Err(expected));
        let (_, pass) = drain(&mut mem, usize::MAX);
        assert_eq!(pass.stop, StopReason::BadConfig(expected));
    }
}

#[test]
fn registers_exist_only_below_min() {
    let mem = new_fifo(RING);
    let snap = Snapshot::capture(&mem).unwrap();
    assert!(snap.has_register(fifo::BUSY));
    assert!(!snap.has_register(fifo::NUM_REGS));

    let mut small = new_fifo(RING);
    small[fifo::MIN as usize] = 16;
    let snap = Snapshot::capture(&small).unwrap();
    assert!(snap.has_register(fifo::STOP));
    assert!(!snap.has_register(fifo::CAPABILITIES));
}
