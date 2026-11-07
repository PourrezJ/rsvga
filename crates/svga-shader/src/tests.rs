//! Translation tests. Generated SPIR-V goes through `spirv-val` (Vulkan 1.3
//! rules) when it is installed. DWM's own shaders, if captured with
//! `RSVGA_SHADER_DUMP=target/shaders/dwm` (they are Microsoft's code and
//! stay out of the repository), are all translated and validated too.

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use super::*;
use crate::opcode as op;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs `spirv-val` on `spirv`; `None` if it is not installed.
fn validate(name: &str, spirv: &[u32]) -> Option<Result<(), String>> {
    let path = std::env::temp_dir().join(format!(
        "rsvga-{}-{}.spv",
        std::process::id(),
        name.replace(['/', ' '], "_")
    ));
    let bytes: Vec<u8> = spirv.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(&path, bytes).ok()?;
    let out = Command::new("spirv-val")
        .args(["--target-env", "vulkan1.3"])
        .arg(&path)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&path);
    Some(if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    })
}

fn check(name: &str, words: &[u32]) -> Shader {
    let shader =
        translate_words(words, &Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
    if let Some(result) = validate(name, &shader.spirv) {
        result.unwrap_or_else(|e| panic!("{name}: spirv-val: {e}"));
    }
    shader
}

// Operand tokens.
const fn operand(kind: u32, comps_select: u32, dims: u32) -> u32 {
    comps_select | (kind << 12) | (dims << 20)
}
const MASK: u32 = 2; // 4 components, mask mode (OR the mask << 4)
const SWZ: u32 = 2 | (1 << 2) | (0xe4 << 4); // .xyzw
const fn sel(c: u32) -> u32 {
    2 | (2 << 2) | (c << 4)
}

fn instr(opcode: u32, rest: &[u32]) -> Vec<u32> {
    let mut v = vec![opcode | (((rest.len() + 1) as u32) << 24)];
    v.extend_from_slice(rest);
    v
}

fn program(stage: u32, body: &[Vec<u32>]) -> Vec<u32> {
    let mut words = vec![(stage << 16) | 0x40, 0];
    for b in body {
        words.extend(b);
    }
    words[1] = words.len() as u32;
    words
}

const SIV_POSITION: u32 = 1;

#[test]
fn a_vertex_shader_with_position_and_constants() {
    let o_all = operand(2, MASK | (0xf << 4), 1);
    let v = operand(1, SWZ, 1);
    let r_x = operand(0, MASK | (1 << 4), 1);
    let cb = operand(8, SWZ, 2);
    let words = program(
        1,
        &[
            instr(op::DCL_CONSTANT_BUFFER, &[cb, 0, 4]),
            instr(op::DCL_INPUT, &[operand(1, MASK | (0xf << 4), 1), 0]),
            instr(op::DCL_OUTPUT_SIV, &[o_all, 0, SIV_POSITION]),
            instr(op::DCL_OUTPUT, &[o_all, 1]),
            instr(op::DCL_TEMPS, &[1]),
            // dp4 r0.x, v0, cb0[0]; mov o0, v0; mul o1, v0, r0.xxxx
            instr(op::DP4, &[r_x, 0, v, 0, cb, 0, 0]),
            instr(op::MOV, &[o_all, 0, v, 0]),
            instr(op::MUL, &[o_all, 1, v, 0, operand(0, sel(0), 1), 0]),
            instr(op::RET, &[]),
        ],
    );
    let shader = check("vs", &words);
    assert_eq!(shader.stage, Stage::Vertex);
    assert_eq!(shader.inputs, [0]);
    assert_eq!(shader.outputs, [1], "position is a builtin");
    assert_eq!(
        shader.bindings,
        [Binding {
            kind: BindingKind::ConstantBuffer { slot: 0, vec4s: 4 },
            binding: 0
        }]
    );
}

#[test]
fn a_pixel_shader_that_samples_branches_and_loops() {
    let o0 = operand(2, MASK | (0xf << 4), 1);
    let v1 = operand(1, SWZ, 1);
    let r0 = operand(0, MASK | (0xf << 4), 1);
    let r0x = operand(0, sel(0), 1);
    let t0 = operand(7, SWZ, 1);
    let s0 = operand(6, 0, 1);
    let imm1 = operand(4, 1, 0); // one-component immediate
    let words = program(
        0,
        &[
            instr(op::DCL_SAMPLER, &[s0, 0]),
            vec![op::DCL_RESOURCE | (3 << 11) | (4 << 24), t0, 0, 0x5555],
            instr(
                op::DCL_INPUT_PS | (2 << 11),
                &[operand(1, MASK | (3 << 4), 1), 1],
            ),
            instr(op::DCL_OUTPUT, &[o0, 0]),
            instr(op::DCL_TEMPS, &[1]),
            // sample r0, v1.xyzw, t0, s0
            instr(op::SAMPLE, &[r0, 0, v1, 1, t0, 0, s0, 0]),
            // if_nz r0.x ; mul_sat r0, r0, l(2.0) ; else ; mov r0, l(0) ; endif
            instr(op::IF | (1 << 18), &[r0x, 0]),
            instr(
                op::MUL | (1 << 13),
                &[r0, 0, r0x, 0, imm1, 2.0f32.to_bits()],
            ),
            instr(op::ELSE, &[]),
            instr(op::MOV, &[r0, 0, imm1, 0]),
            instr(op::ENDIF, &[]),
            // loop ; breakc_nz r0.x ; add r0, r0, l(1.0) ; endloop
            instr(op::LOOP, &[]),
            instr(op::BREAKC | (1 << 18), &[r0x, 0]),
            instr(op::ADD, &[r0, 0, r0x, 0, imm1, 1.0f32.to_bits()]),
            instr(op::ENDLOOP, &[]),
            // discard_z r0.x ; ige / movc / itof round trip
            instr(op::DISCARD, &[r0x, 0]),
            instr(op::IGE, &[r0, 0, r0x, 0, imm1, 3]),
            instr(op::MOVC, &[o0, 0, r0x, 0, v1, 1, r0x, 0]),
            instr(op::RET, &[]),
        ],
    );
    let shader = check("ps", &words);
    assert_eq!(shader.outputs, [0]);
    assert!(
        shader.bindings.iter().any(|b| b.binding == 256 + 16),
        "sampler s0"
    );
    assert!(
        shader.bindings.iter().any(|b| b.binding == 256 + 32),
        "texture t0"
    );
}

#[test]
fn malformed_programs_are_refused_not_panicking() {
    // Undeclared registers, unbalanced flow control, unknown opcodes.
    let r0 = operand(0, MASK | (0xf << 4), 1);
    assert!(
        translate_words(
            &program(1, &[instr(op::MOV, &[r0, 0, r0, 0])]),
            &Options::default()
        )
        .is_err()
    );
    assert!(translate_words(&program(1, &[instr(op::ENDIF, &[])]), &Options::default()).is_err());
    assert!(translate_words(&program(1, &[instr(op::LOOP, &[])]), &Options::default()).is_err());
    assert!(translate_words(&program(1, &[instr(op::EMIT, &[])]), &Options::default()).is_err());
    assert!(
        translate_words(&program(2, &[]), &Options::default()).is_err(),
        "geometry shaders"
    );
    // Declarations without their operand (found by fuzzing).
    for dcl in [
        op::DCL_OUTPUT,
        op::DCL_INPUT,
        op::DCL_CONSTANT_BUFFER,
        op::DCL_SAMPLER,
        op::DCL_RESOURCE,
    ] {
        assert!(translate_words(&program(1, &[instr(dcl, &[])]), &Options::default()).is_err());
    }
    // Arbitrary words never panic.
    let mut seed = 0x1234_5678u32;
    for _ in 0..2000 {
        let len = (seed % 40) as usize + 2;
        let mut words = vec![0x0001_0040, len as u32];
        for _ in 2..len {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            words.push(seed);
        }
        let _ = translate_words(&words, &Options::default());
    }
}

fn corpus() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shaders/dwm");
    dir.is_dir().then_some(dir)
}

#[test]
fn the_dwm_corpus_translates_and_validates() {
    let Some(dir) = corpus() else {
        eprintln!("no captured shaders in target/shaders/dwm: skipped");
        return;
    };
    let mut failures = Vec::new();
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        count += 1;
        match translate_words(&words, &Options::default()) {
            Err(e) => failures.push(format!("{name}: {e}")),
            Ok(shader) => {
                // RSVGA_SPV_DUMP=DIR keeps the SPIR-V, to inspect it.
                if let Some(out) = std::env::var_os("RSVGA_SPV_DUMP") {
                    let bytes: Vec<u8> =
                        shader.spirv.iter().flat_map(|w| w.to_le_bytes()).collect();
                    let _ = std::fs::create_dir_all(&out);
                    let _ = std::fs::write(Path::new(&out).join(format!("{name}.spv")), bytes);
                }
                if let Some(Err(e)) = validate(&name, &shader.spirv) {
                    failures.push(format!(
                        "{name}: spirv-val: {}",
                        e.lines().next().unwrap_or("")
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {count} shaders failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("{count} DWM shaders translated and validated");
}
