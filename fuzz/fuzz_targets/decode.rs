//! Arbitrary words fed straight to the decoder: it must never panic, and a
//! decoded command can never be longer than its input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use svga_core::{Decode, Limits, decode};

fuzz_target!(|data: &[u8]| {
    let words: Vec<u32> = data
        .chunks(4)
        .map(|chunk| {
            let mut bytes = [0u8; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(bytes)
        })
        .collect();
    if let Decode::Ready { words: used, .. } = decode(words.as_slice(), &Limits::default()) {
        assert!(used >= 1 && used <= words.len());
    }
});
