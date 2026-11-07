//! Legacy (Direct3D 9) context commands and Direct3D 9 shader programs
//! from arbitrary bytes: decoding and translating must never panic,
//! decoded draws stay within the protocol limits, and a translated program
//! starts with the SPIR-V header.
#![no_main]

use libfuzzer_sys::fuzz_target;
use svga_core::legacy::{LegacyCommand, MAX_PRIMITIVE_RANGES, MAX_VERTEX_ARRAYS, parse};
use svga_shader::d3d9::{Fog, Options, translate_words};

fuzz_target!(|data: &[u8]| {
    let [lo, flags, rest @ ..] = data else {
        return;
    };
    let mut words: Vec<u32> = rest
        .chunks(4)
        .map(|chunk| {
            let mut bytes = [0u8; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(bytes)
        })
        .collect();
    // A command of the legacy range (1040 to 1139).
    let id = 1040 + u32::from(*lo) % 100;
    if let Some(Ok(LegacyCommand::DrawPrimitives { decls, ranges, .. })) = parse(id, &words) {
        assert!(decls.len() <= MAX_VERTEX_ARRAYS && ranges.len() <= MAX_PRIMITIVE_RANGES);
    }
    // The same words as a program, most with a plausible version token.
    if let Some(version) = words.first_mut() {
        if flags & 1 == 0 {
            let models = [0xfffe_0101, 0xfffe_0200, 0xfffe_0300, 0xffff_0104, 0xffff_0200, 0xffff_0300];
            *version = models[usize::from(flags >> 1) % models.len()];
        }
    }
    let options = Options {
        alpha_test: flags >> 4,
        fog: if flags & 2 != 0 { Fog::Linear } else { Fog::None },
        flat: flags & 4 != 0,
        ..Options::default()
    };
    if let Ok(t) = translate_words(&words, &options) {
        assert_eq!(t.shader.spirv.first(), Some(&0x0723_0203));
    }
});
