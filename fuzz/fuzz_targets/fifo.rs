//! Whole FIFO passes over arbitrary memory. The first input word picks the
//! mode: raw ring registers, or registers forced into a valid shape so that
//! the fuzzer spends its time in the decoder rather than in validation.
#![no_main]

use libfuzzer_sys::fuzz_target;
use svga_core::{Limits, Snapshot, StopReason, run};
use svga_proto::fifo;

const MIN_WORDS: usize = 4 + 1024 / 4 + 4;

fuzz_target!(|data: &[u8]| {
    let mut words: Vec<u32> = data
        .chunks(4)
        .map(|chunk| {
            let mut bytes = [0u8; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(bytes)
        })
        .collect();
    if words.len() < 2 {
        return;
    }
    let mode = words.remove(0);
    if mode & 1 == 0 {
        if words.len() < MIN_WORDS {
            words.resize(MIN_WORDS, 0);
        }
        let bytes = (words.len() * 4) as u32;
        let min = (16 + words[0] % (bytes - 16 - 1024)) & !3;
        let max = (min + 1024 + words[1] % (bytes - min - 1024 + 1)) & !3;
        let ring = max - min;
        words[fifo::MIN as usize] = min;
        words[fifo::MAX as usize] = max;
        words[fifo::NEXT_CMD as usize] = min + (words[2] % ring & !3);
        words[fifo::STOP as usize] = min + (words[3] % ring & !3);
    }

    let before = words.clone();
    let snap = Snapshot::capture(&words);
    let budget = ((mode >> 1) % 256) as usize;
    let mut handled = 0usize;
    let pass = run(&mut words, &Limits::default(), budget, |_| handled += 1);

    assert_eq!(pass.executed, handled);
    assert!(pass.executed <= budget);
    for (index, (old, new)) in before.iter().zip(&words).enumerate() {
        if index != fifo::STOP as usize {
            assert_eq!(old, new);
        }
    }
    match snap {
        Err(error) => assert_eq!(pass.stop, StopReason::BadConfig(error)),
        Ok(snap) => {
            let stop = words[fifo::STOP as usize];
            assert_eq!(stop % 4, 0);
            assert!((snap.min()..snap.max()).contains(&stop));
            if pass.stop == StopReason::Empty {
                assert_eq!(stop, snap.next());
            }
        }
    }
});
