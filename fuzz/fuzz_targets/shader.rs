//! Arbitrary shader programs: decoding and translating must never panic,
//! and a translated module starts with the SPIR-V header.
#![no_main]

use libfuzzer_sys::fuzz_target;
use svga_shader::{Options, translate_words};

fuzz_target!(|data: &[u8]| {
    let mut words: Vec<u32> = data
        .chunks(4)
        .map(|chunk| {
            let mut bytes = [0u8; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(bytes)
        })
        .collect();
    // Most inputs get a plausible header: vertex or pixel, SM 4.x, the
    // length of the words.
    if let [version, length, ..] = words.as_mut_slice() {
        *version = (*version & 0x0001_0001) | 0x40;
        *length = (*length % 4 == 0)
            .then_some(*length)
            .unwrap_or(u32::try_from(data.len() / 4).unwrap_or(0));
    }
    if let Ok(shader) = translate_words(&words, &Options::default()) {
        assert_eq!(shader.spirv.first(), Some(&0x0723_0203));
    }
});
