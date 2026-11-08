//! Translation tests. Generated SPIR-V goes through `spirv-val` (Vulkan 1.3
//! rules) when it is installed. Captured shaders, if any (for instance
//! DWM's with `RSVGA_SHADER_DUMP=target/shaders/dwm`, or those of the guest
//! test programs in `target/shaders/d3dtest`; they stay out of the
//! repository), are all translated and validated too.

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

/// [`validate`] for the other test modules.
pub(crate) fn validate_spirv(name: &str, spirv: &[u32]) -> Option<Result<(), String>> {
    validate(name, spirv)
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
    // Group barriers belong to compute shaders.
    assert!(
        translate_words(
            &program(0, &[instr(op::SYNC | (1 << 11), &[])]),
            &Options::default()
        )
        .is_err()
    );
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

/// Folders of captured Direct3D 10/11 shaders: every folder of
/// `target/shaders` but `d3d9` (`dwm`: DWM's; `d3dtest`: the guest test
/// program's, which cover shader model 5).
fn corpora() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shaders");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "d3d9"))
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn captured_shaders_translate_and_validate() {
    let dirs = corpora();
    if dirs.is_empty() {
        eprintln!("no captured shaders in target/shaders: skipped");
        return;
    }
    let mut failures = Vec::new();
    let mut count = 0;
    for dir in &dirs {
        for entry in std::fs::read_dir(dir).unwrap() {
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
    }
    assert!(
        failures.is_empty(),
        "{} of {count} shaders failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("{count} captured shaders translated and validated");
}

#[test]
fn a_geometry_shader_emits_a_strip_from_a_point() {
    // dcl_input_siv v[1][0].xyzw, position; points in, triangle strip out;
    // two vertices emitted from the point, then a cut.
    let all = MASK | (0xf << 4);
    let v00 = operand(1, SWZ, 2);
    let o0 = operand(2, all, 1);
    let words = program(
        2,
        &[
            instr(op::DCL_INPUT_SIV, &[operand(1, all, 2), 1, 0, SIV_POSITION]),
            instr(op::DCL_GS_INPUT_PRIMITIVE | (1 << 11), &[]),
            instr(op::DCL_GS_OUTPUT_PRIMITIVE_TOPOLOGY | (5 << 11), &[]),
            instr(op::DCL_OUTPUT_SIV, &[o0, 0, SIV_POSITION]),
            instr(op::DCL_MAX_OUTPUT_VERTEX_COUNT, &[2]),
            instr(op::MOV, &[o0, 0, v00, 0, 0]),
            instr(op::EMIT, &[]),
            instr(op::MOV, &[o0, 0, v00, 0, 0]),
            instr(op::EMITTHENCUT, &[]),
            instr(op::RET, &[]),
        ],
    );
    let shader = check("geometry", &words);
    assert_eq!(shader.stage, Stage::Geometry);
    // OpEmitVertex twice, OpEndPrimitive once.
    let count = |opcode: u32| {
        shader
            .spirv
            .iter()
            .filter(|&&w| w == (1 << 16) | opcode)
            .count()
    };
    assert_eq!((count(218), count(219)), (2, 1));
}

/// VMware's signature block: inputs, outputs, patch constants, as
/// (register, system value, mask).
fn signatures(
    inputs: &[(u32, u32, u32)],
    outputs: &[(u32, u32, u32)],
    patch: &[(u32, u32, u32)],
) -> Vec<u32> {
    let mut words = vec![
        0x08a9_2d12,
        inputs.len() as u32,
        outputs.len() as u32,
        patch.len() as u32,
    ];
    for &(register, name, mask) in inputs.iter().chain(outputs).chain(patch) {
        words.extend([register, name, mask, 0, 0]);
    }
    words
}

const TRI_EDGES: [(u32, u32, u32); 4] = [(0, 17, 1), (1, 18, 1), (2, 19, 1), (3, 20, 1)];

#[test]
fn a_hull_shader_runs_its_phases_and_writes_the_levels() {
    // Triangles of 3 control points, copied through (no control point
    // phase); a fork phase of 3 instances writes the edges through an
    // index register, another the inside factor.
    let x = MASK | (1 << 4);
    let r0x = operand(0, x, 1);
    let mut body = vec![
        instr(op::HS_DECLS, &[]),
        instr(op::DCL_INPUT_CONTROL_POINT_COUNT | (3 << 11), &[]),
        instr(op::DCL_OUTPUT_CONTROL_POINT_COUNT | (3 << 11), &[]),
        instr(op::DCL_TESS_DOMAIN | (2 << 11), &[]),
        instr(op::DCL_TESS_PARTITIONING | (1 << 11), &[]),
        instr(op::DCL_TESS_OUTPUT_PRIMITIVE | (3 << 11), &[]),
        instr(op::HS_FORK_PHASE, &[]),
        instr(op::DCL_HS_FORK_PHASE_INSTANCE_COUNT, &[3]),
        instr(op::DCL_INPUT, &[operand(23, 0, 0)]),
    ];
    for (r, name) in [(0, 17), (1, 18), (2, 19)] {
        body.push(instr(op::DCL_OUTPUT_SIV, &[operand(2, x, 1), r, name]));
    }
    body.extend([
        instr(op::DCL_TEMPS, &[1]),
        instr(op::MOV, &[r0x, 0, operand(23, sel(0), 0)]),
        // mov o[r0.x].x, l(4.0)
        instr(
            op::MOV,
            &[
                operand(2, x, 1) | (2 << 22),
                operand(0, sel(0), 1),
                0,
                operand(4, 1, 0),
                0x4080_0000,
            ],
        ),
        instr(op::RET, &[]),
        instr(op::HS_FORK_PHASE, &[]),
        instr(op::DCL_HS_FORK_PHASE_INSTANCE_COUNT, &[1]),
        instr(op::DCL_OUTPUT_SIV, &[operand(2, x, 1), 3, 20]),
        instr(
            op::MOV,
            &[operand(2, x, 1), 3, operand(4, 1, 0), 0x4080_0000],
        ),
        instr(op::RET, &[]),
    ]);
    let mut words = program(3, &body);
    words.extend(signatures(&[(0, 0, 0xf)], &[(0, 0, 0xf)], &TRI_EDGES));
    let shader = check("hull", &words);
    assert_eq!(shader.stage, Stage::Hull);
    let count = |opcode: u32, len: u32| {
        shader
            .spirv
            .iter()
            .filter(|&&w| w == (len << 16) | opcode)
            .count()
    };
    // Four phase calls (OpFunctionCall: result type, result, function).
    assert_eq!(count(57, 4), 4, "phase calls");
    // TessLevelOuter (11) and TessLevelInner (12) are written.
    let builtins: Vec<u32> = shader
        .spirv
        .windows(4)
        .filter(|w| w[0] == (4 << 16) | 71 && w[2] == 11)
        .map(|w| w[3])
        .collect();
    assert!(
        builtins.contains(&11) && builtins.contains(&12),
        "{builtins:?}"
    );
}

#[test]
fn a_domain_shader_reads_the_domain_point_and_control_points() {
    let all = MASK | (0xf << 4);
    let r0 = operand(0, all, 1);
    let mut words = program(
        4,
        &[
            instr(op::DCL_INPUT_CONTROL_POINT_COUNT | (3 << 11), &[]),
            instr(op::DCL_TESS_DOMAIN | (2 << 11), &[]),
            instr(op::DCL_INPUT, &[operand(28, MASK | (7 << 4), 0)]),
            instr(op::DCL_INPUT, &[operand(25, all, 2), 3, 0]),
            instr(op::DCL_OUTPUT_SIV, &[operand(2, all, 1), 0, SIV_POSITION]),
            instr(op::DCL_TEMPS, &[1]),
            // add r0, vicp[2][0], vDomain.xyzx; mov o0, r0
            instr(
                op::ADD,
                &[
                    r0,
                    0,
                    operand(25, SWZ, 2),
                    2,
                    0,
                    operand(28, 2 | (1 << 2) | (0x24 << 4), 0),
                ],
            ),
            instr(op::MOV, &[operand(2, all, 1), 0, operand(0, SWZ, 1), 0]),
            instr(op::RET, &[]),
        ],
    );
    words.extend(signatures(&[(0, 0, 0xf)], &[(0, 1, 0xf)], &TRI_EDGES));
    let shader = check("domain", &words);
    assert_eq!(shader.stage, Stage::Domain);
    // The TessCoord builtin (13) is read.
    assert!(
        shader
            .spirv
            .windows(4)
            .any(|w| w[0] == (4 << 16) | 71 && w[2] == 11 && w[3] == 13),
        "TessCoord"
    );
}

#[test]
fn stream_output_captures_registers_and_the_position() {
    // vs: o0 (position) = v0; o1 = v1. Captured: o1.xy at 0 and the
    // position at 8 in buffer 0 (24-byte vertices).
    let all = MASK | (0xf << 4);
    let words = program(
        1,
        &[
            instr(op::DCL_INPUT, &[operand(1, all, 1), 0]),
            instr(op::DCL_INPUT, &[operand(1, all, 1), 1]),
            instr(op::DCL_OUTPUT_SIV, &[operand(2, all, 1), 0, SIV_POSITION]),
            instr(op::DCL_OUTPUT, &[operand(2, all, 1), 1]),
            instr(op::MOV, &[operand(2, all, 1), 0, operand(1, SWZ, 1), 0]),
            instr(op::MOV, &[operand(2, all, 1), 1, operand(1, SWZ, 1), 1]),
            instr(op::RET, &[]),
        ],
    );
    let options = Options {
        stream_output: vec![
            Capture {
                buffer: 0,
                register: 1,
                mask: 0x3,
                offset: 0,
            },
            Capture {
                buffer: 0,
                register: 0,
                mask: 0xf,
                offset: 8,
            },
        ],
        so_strides: [24, 0, 0, 0],
        ..Options::default()
    };
    let shader = translate_words(&words, &options).unwrap();
    if let Some(result) = validate("xfb", &shader.spirv) {
        result.unwrap_or_else(|e| panic!("spirv-val: {e}"));
    }
    // OpExecutionMode main Xfb, and two Offset decorations.
    assert!(
        shader
            .spirv
            .windows(3)
            .any(|w| w[0] == (3 << 16) | 16 && w[2] == 11)
    );
    let offsets = shader
        .spirv
        .windows(4)
        .filter(|w| w[0] == (4 << 16) | 71 && w[2] == 35)
        .count();
    assert_eq!(offsets, 2);
    // Other locations are left alone: o1 keeps location 1.
    assert_eq!(shader.outputs, [1]);
}

#[test]
fn multisampled_textures_load_samples_and_report_their_size() {
    // dcl_resource_texture2dms t0; ld_ms o0, l(1, 2, 0, 0), t0, l(3);
    // resinfo_uint o0, l(0), t0.
    let all = MASK | (0xf << 4);
    let t0 = operand(7, SWZ, 1);
    let o0 = operand(2, all, 1);
    let words = program(
        0,
        &[
            instr(op::DCL_RESOURCE | (4 << 11), &[t0, 0, 0x5555]),
            instr(op::DCL_OUTPUT, &[o0, 0]),
            instr(
                op::LD_MS,
                &[
                    o0,
                    0,
                    operand(4, 2, 0),
                    1,
                    2,
                    0,
                    0,
                    t0,
                    0,
                    operand(4, 1, 0),
                    3,
                ],
            ),
            instr(
                op::RESINFO | (2 << 11),
                &[o0, 0, operand(4, 1, 0), 0, t0, 0],
            ),
            instr(op::RET, &[]),
        ],
    );
    let shader = check("ld_ms", &words);
    // OpImageFetch (7 words) with the Sample operand (0x40), then
    // OpImageQuerySize.
    assert!(
        shader
            .spirv
            .windows(7)
            .any(|w| w[0] == (7 << 16) | 95 && w[5] == 0x40)
    );
    assert!(shader.spirv.contains(&((4 << 16) | 104)));
}
