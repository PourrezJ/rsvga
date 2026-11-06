//! Deterministic stress test: random rings filled with streams of well-formed
//! commands, then truncated or corrupted at random. Complements the
//! cargo-fuzz target, which explores the same space with coverage guidance.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    missing_docs
)]

use svga_core::{Limits, Snapshot, StopReason, run};
use svga_proto::{cmd, fifo, remap_gmr2};

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn small(&mut self, n: u64) -> u32 {
        self.below(n) as u32
    }
    fn word(&mut self) -> u32 {
        self.next() as u32
    }
    fn words(&mut self, n: usize) -> Vec<u32> {
        (0..n).map(|_| self.word()).collect()
    }
}

/// One well-formed command, with random arguments.
fn command(rng: &mut XorShift) -> Vec<u32> {
    let mut out = Vec::new();
    match rng.below(16) {
        0 => out.extend([cmd::UPDATE, rng.word(), rng.word(), rng.word(), rng.word()]),
        1 => {
            out.push(cmd::RECT_COPY);
            out.extend(rng.words(6));
        }
        2 => out.extend([cmd::FENCE, rng.word()]),
        3 => out.push(cmd::NOP),
        4 => out.extend([cmd::DESTROY_SCREEN, rng.small(8)]),
        5 => out.extend([cmd::DEFINE_GMR2, rng.small(64), rng.word()]),
        6 => {
            out.push(cmd::BLIT_GMRFB_TO_SCREEN);
            out.extend(rng.words(7));
        }
        7 => {
            let (w, h) = (rng.small(17), rng.small(17));
            let (and_depth, xor_depth) = (
                [1, 32][rng.small(2) as usize],
                [1, 32][rng.small(2) as usize],
            );
            out.extend([
                cmd::DEFINE_CURSOR,
                0,
                rng.small(16),
                rng.small(16),
                w,
                h,
                and_depth,
                xor_depth,
            ]);
            for depth in [and_depth, xor_depth] {
                let words = (w * depth).div_ceil(32) * h;
                out.extend(rng.words(words as usize));
            }
        }
        8 => {
            let (w, h) = (rng.small(17), rng.small(17));
            out.extend([cmd::DEFINE_ALPHA_CURSOR, 0, 0, 0, w, h]);
            out.extend(rng.words((w * h) as usize));
        }
        9 => {
            let size = rng.small(65);
            out.extend([cmd::ESCAPE, rng.word(), size]);
            out.extend(rng.words(size.div_ceil(4) as usize));
        }
        10 => {
            let size = [28, 40, 44][rng.small(3) as usize];
            out.extend([cmd::DEFINE_SCREEN, size]);
            out.extend(rng.words((size / 4 - 1) as usize));
        }
        11 => {
            let flags = rng.small(8);
            let pages = rng.small(9);
            out.extend([cmd::REMAP_GMR2, rng.small(64), flags, rng.word(), pages]);
            let payload = if flags & remap_gmr2::VIA_GMR != 0 {
                2
            } else {
                let entries = if flags & remap_gmr2::SINGLE_PPN != 0 {
                    1
                } else {
                    pages
                };
                entries * if flags & remap_gmr2::PPN64 != 0 { 2 } else { 1 }
            };
            out.extend(rng.words(payload as usize));
        }
        12 => out.extend([cmd::ANNOTATION_FILL, rng.word()]),
        13 => out.extend([cmd::UPDATE_VERBOSE, 0, 0, 1, 1, rng.word()]),
        _ => {
            let size = rng.small(129);
            out.extend([1040 + rng.small(263), size]);
            out.extend(rng.words(size.div_ceil(4) as usize));
        }
    }
    out
}

#[derive(Default)]
struct Coverage {
    empty: usize,
    incomplete: usize,
    budget: usize,
    invalid: usize,
    bad_config: usize,
    executed: usize,
}

#[test]
fn random_fifos_never_break_the_ring_invariants() {
    let mut rng = XorShift(0x5eed_1234_abcd_0001);
    let limits = Limits {
        max_cursor_dimension: 16,
        ..Limits::default()
    };
    let mut cov = Coverage::default();

    for _ in 0..20_000 {
        let words = 300 + rng.below(1500) as usize;
        let mut mem: Vec<u32> = rng.words(words);

        if rng.below(10) != 0 {
            // A valid ring, at a random place in memory.
            let bytes = (words * 4) as u64;
            let min = (16 + rng.below(bytes - 1024 - 16)) & !3;
            let max = (min + 1024 + rng.below(bytes - min - 1024 + 1)).min(bytes) & !3;
            let ring = max - min;
            let stop = min + (rng.below(ring) & !3);
            mem[fifo::MIN as usize] = min as u32;
            mem[fifo::MAX as usize] = max as u32;
            mem[fifo::STOP as usize] = stop as u32;

            // Fill it with commands, leaving one word free.
            let mut stream = Vec::new();
            loop {
                let next = command(&mut rng);
                if ((stream.len() + next.len()) as u64) * 4 >= ring {
                    break;
                }
                stream.extend(next);
                if rng.below(6) == 0 {
                    break;
                }
            }
            match rng.below(10) {
                0 | 1 if !stream.is_empty() => {
                    let at = rng.below(stream.len() as u64) as usize;
                    stream[at] = rng.word(); // corruption
                }
                2 if !stream.is_empty() => {
                    let keep = rng.below(stream.len() as u64) as usize;
                    stream.truncate(keep); // guest still writing
                }
                _ => {}
            }
            let mut pos = stop;
            for word in &stream {
                mem[(pos / 4) as usize] = *word;
                pos += 4;
                if pos == max {
                    pos = min;
                }
            }
            mem[fifo::NEXT_CMD as usize] = pos as u32;
        }

        let before = mem.clone();
        let snap = Snapshot::capture(&mem);
        let budget = rng.below(64) as usize;
        let mut handled = 0usize;
        let pass = run(&mut mem, &limits, budget, |_| handled += 1);

        assert_eq!(pass.executed, handled);
        assert!(pass.executed <= budget);
        cov.executed += pass.executed;
        match pass.stop {
            StopReason::Empty => cov.empty += 1,
            StopReason::Incomplete => cov.incomplete += 1,
            StopReason::Budget => cov.budget += 1,
            StopReason::Invalid(_) => cov.invalid += 1,
            StopReason::BadConfig(_) => cov.bad_config += 1,
        }
        for (index, (old, new)) in before.iter().zip(&mem).enumerate() {
            if index != fifo::STOP as usize {
                assert_eq!(old, new, "word {index} was modified");
            }
        }

        match snap {
            Err(error) => assert_eq!(pass.stop, StopReason::BadConfig(error)),
            Ok(snap) => {
                let stop = mem[fifo::STOP as usize];
                assert_eq!(stop % 4, 0);
                assert!((snap.min()..snap.max()).contains(&stop));
                let consumed = if stop >= snap.stop() {
                    stop - snap.stop()
                } else {
                    snap.ring_bytes() - (snap.stop() - stop)
                };
                assert!(consumed <= snap.pending_bytes());
                if pass.stop == StopReason::Empty {
                    assert_eq!(stop, snap.next());
                }
                if pass.executed == 0 {
                    assert_eq!(stop, snap.stop());
                }
            }
        }
    }

    // The generator must keep exercising every outcome, or the test is hollow.
    let Coverage {
        empty,
        incomplete,
        budget,
        invalid,
        bad_config,
        executed,
    } = cov;
    eprintln!(
        "empty {empty}, incomplete {incomplete}, budget {budget}, invalid {invalid}, \
         bad config {bad_config}, commands executed {executed}"
    );
    for (name, count) in [
        ("empty", empty),
        ("incomplete", incomplete),
        ("budget", budget),
        ("invalid", invalid),
        ("bad config", bad_config),
    ] {
        assert!(count >= 500, "only {count} passes ended with {name}");
    }
    assert!(executed >= 50_000, "only {executed} commands executed");
}
