//! Tests on the host GPU (`RSVGA_GPU` picks another device, e.g.
//! `llvmpipe`). Without Vulkan they say so and pass.

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use super::*;
use svga_core::layout::Size3;
use svga_proto::format;

fn renderer() -> Option<Renderer> {
    match Renderer::new() {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("no GPU for this test: {e}");
            None
        }
    }
}

fn layout(format: u32, width: u32, height: u32, mips: u32, layers: u32) -> SurfaceLayout {
    SurfaceLayout::new(
        format,
        Size3 {
            width,
            height,
            depth: 1,
        },
        mips,
        layers,
        1,
        1 << 30,
    )
    .unwrap()
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + 3) as u8).collect()
}

#[test]
fn bytes_go_to_the_gpu_and_back() {
    let Some(mut r) = renderer() else { return };
    // Two layers, three mips: every subresource at its guest offset.
    let l = layout(format::B8G8R8A8_UNORM, 8, 4, 3, 2);
    r.create(1, &l, 0).unwrap();
    let data = pattern(l.total_bytes() as usize);
    r.upload(1, &data).unwrap();
    let mut back = vec![0u8; data.len()];
    r.download(1, &mut back).unwrap();
    assert_eq!(back, data, "on {}", r.name());
    // Buffers too; and wrong sizes are refused.
    let b = layout(format::BUFFER, 100, 1, 1, 1);
    r.create(2, &b, 0).unwrap();
    let data = pattern(100);
    r.upload(2, &data).unwrap();
    let mut back = vec![0u8; 100];
    r.download(2, &mut back).unwrap();
    assert_eq!(back, data);
    assert!(r.upload(2, &[0; 99]).is_err());
}

#[test]
fn clears_and_copies() {
    let Some(mut r) = renderer() else { return };
    let l = layout(format::B8G8R8A8_UNORM, 4, 4, 1, 1);
    r.create(1, &l, 0).unwrap();
    r.create(2, &l, 0).unwrap();
    r.upload(2, &[0; 64]).unwrap();
    // Red in B, G, R, A byte order.
    r.clear_color(1, 0, 0, 1, [1.0, 0.0, 0.0, 1.0]).unwrap();
    // A 2x2 box of it into the middle of the other image.
    let at = |x, y| At {
        layer: 0,
        mip: 0,
        offset: [x, y, 0],
    };
    r.copy_image(2, at(1, 1), 1, at(0, 0), [2, 2, 1]).unwrap();
    let mut back = vec![0u8; 64];
    r.download(2, &mut back).unwrap();
    let pixel =
        |x: usize, y: usize| u32::from_le_bytes(back[(y * 4 + x) * 4..][..4].try_into().unwrap());
    assert_eq!(pixel(0, 0), 0);
    assert_eq!(pixel(1, 1), 0xffff_0000);
    assert_eq!(pixel(2, 2), 0xffff_0000);
    assert_eq!(pixel(3, 3), 0);
    // Whole copy, then integer clears take integers.
    r.copy_whole(2, 1).unwrap();
    r.download(2, &mut back).unwrap();
    assert!(back.chunks(4).all(|p| p == [0, 0, 0xff, 0xff]));
    let u = layout(format::R32_UINT, 2, 2, 1, 1);
    r.create(3, &u, 0).unwrap();
    r.clear_color(3, 0, 0, 1, [7.0, 0.0, 0.0, 0.0]).unwrap();
    let mut back = vec![0u8; 16];
    r.download(3, &mut back).unwrap();
    assert_eq!(u32::from_le_bytes(back[..4].try_into().unwrap()), 7);
    // Out of range.
    assert!(r.clear_color(1, 1, 0, 1, [0.0; 4]).is_err());
    assert!(r.copy_image(2, at(3, 3), 1, at(0, 0), [2, 2, 1]).is_err());
}

#[test]
fn buffers_copy_into_buffers_and_images() {
    let Some(mut r) = renderer() else { return };
    let b = layout(format::BUFFER, 64, 1, 1, 1);
    r.create(1, &b, 0).unwrap();
    r.create(2, &b, 0).unwrap();
    let data = pattern(64);
    r.upload(1, &data).unwrap();
    r.upload(2, &[0; 64]).unwrap();
    r.copy_buffer(2, 8, 1, 0, 16).unwrap();
    let mut back = vec![0u8; 64];
    r.download(2, &mut back).unwrap();
    assert_eq!(&back[8..24], &data[..16]);
    assert!(r.copy_buffer(2, 60, 1, 0, 16).is_err(), "past the end");
    // Rows of 16 bytes (4 pixels) into a 2x2 box of a 4x4 image.
    let l = layout(format::R8G8B8A8_UNORM, 4, 4, 1, 1);
    r.create(3, &l, 0).unwrap();
    r.upload(3, &[0; 64]).unwrap();
    let at = At {
        layer: 0,
        mip: 0,
        offset: [2, 2, 0],
    };
    r.copy_buffer_to_image(3, at, [2, 2, 1], 1, 0, 16, 0)
        .unwrap();
    r.download(3, &mut back).unwrap();
    assert_eq!(&back[(2 * 4 + 2) * 4..][..8], &data[..8]);
    assert_eq!(&back[(3 * 4 + 2) * 4..][..8], &data[16..24]);
    assert!(
        r.copy_buffer_to_image(3, at, [2, 2, 1], 1, 0, 15, 0)
            .is_err(),
        "ragged pitch"
    );
    // Two rows of 16 bytes from offset 48 need 48 + 16 + 8 = 72 > 64 bytes.
    assert!(
        r.copy_buffer_to_image(3, at, [2, 2, 1], 1, 48, 16, 0)
            .is_err(),
        "beyond the buffer"
    );
}

#[test]
fn surfaces_come_and_go() {
    let Some(mut r) = renderer() else { return };
    let l = layout(format::R8G8B8A8_UNORM, 16, 16, 5, 6);
    r.create(9, &l, svga_proto::surface_flag::CUBEMAP).unwrap();
    assert!(r.has(9));
    r.clear_color(9, 4, 5, 1, [0.5; 4]).unwrap();
    r.destroy(9).unwrap();
    assert!(!r.has(9));
    assert!(r.clear_color(9, 0, 0, 1, [0.0; 4]).is_err());
    // Formats without a GPU twin are refused.
    assert!(
        r.create(10, &layout(format::YUY2, 16, 16, 1, 1), 0)
            .is_err()
    );
}

// ---- draws ----------------------------------------------------------------

/// VGPU10 tokens, as in svga-shader's tests.
mod tokens {
    pub const fn operand(kind: u32, comps_select: u32, dims: u32) -> u32 {
        comps_select | (kind << 12) | (dims << 20)
    }
    pub const ALL: u32 = 2 | (0xf << 4); // mask .xyzw
    pub const SWZ: u32 = 2 | (1 << 2) | (0xe4 << 4); // swizzle .xyzw
    pub fn instr(opcode: u32, rest: &[u32]) -> Vec<u32> {
        let mut v = vec![opcode | (((rest.len() + 1) as u32) << 24)];
        v.extend_from_slice(rest);
        v
    }
    pub fn program(stage: u32, body: &[Vec<u32>]) -> Vec<u32> {
        let mut words = vec![(stage << 16) | 0x40, 0];
        for b in body {
            words.extend(b);
        }
        words[1] = words.len() as u32;
        words
    }
}

/// VS: `o0 (position) = v0; o1 = v1`. PS: `o0 = v1 * cb0[0]`.
fn color_shaders() -> (svga_shader::Shader, svga_shader::Shader) {
    use svga_shader::opcode as op;
    use tokens::*;
    let o = operand(2, ALL, 1);
    let v = operand(1, SWZ, 1);
    let vs = program(
        1,
        &[
            instr(op::DCL_INPUT, &[operand(1, ALL, 1), 0]),
            instr(op::DCL_INPUT, &[operand(1, ALL, 1), 1]),
            instr(op::DCL_OUTPUT_SIV, &[o, 0, 1]),
            instr(op::DCL_OUTPUT, &[o, 1]),
            instr(op::MOV, &[o, 0, v, 0]),
            instr(op::MOV, &[o, 1, v, 1]),
            instr(op::RET, &[]),
        ],
    );
    let cb = operand(8, SWZ, 2);
    let ps = program(
        0,
        &[
            instr(op::DCL_CONSTANT_BUFFER, &[cb, 0, 1]),
            instr(op::DCL_INPUT_PS | (2 << 11), &[operand(1, ALL, 1), 1]),
            instr(op::DCL_OUTPUT, &[o, 0]),
            instr(op::MUL, &[o, 0, v, 1, cb, 0, 0]),
            instr(op::RET, &[]),
        ],
    );
    let options = svga_shader::Options::default();
    (
        svga_shader::translate_words(&vs, &options).unwrap(),
        svga_shader::translate_words(&ps, &options).unwrap(),
    )
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[test]
fn a_triangle_is_drawn_upright_with_its_constants() {
    let Some(mut r) = renderer() else { return };
    let (vs, ps) = color_shaders();
    // An 8x8 black target.
    let target = layout(format::B8G8R8A8_UNORM, 8, 8, 1, 1);
    r.create(1, &target, 0).unwrap();
    r.upload(1, &[0; 256]).unwrap();
    // The top-left half of the target, clockwise on screen (Direct3D's
    // front faces): top-left, top-right, bottom-left; yellow.
    let vertices = floats(&[
        -1.0, 1.0, 0.0, 1.0, /* color */ 1.0, 1.0, 0.0, 1.0, //
        1.0, 1.0, 0.0, 1.0, /* color */ 1.0, 1.0, 0.0, 1.0, //
        -1.0, -1.0, 0.0, 1.0, /* color */ 1.0, 1.0, 0.0, 1.0,
    ]);
    r.create(2, &layout(format::BUFFER, 96, 1, 1, 1), 0)
        .unwrap();
    r.upload(2, &vertices).unwrap();
    // cb0[0] halves green: orange.
    r.create(3, &layout(format::BUFFER, 16, 1, 1, 1), 0)
        .unwrap();
    r.upload(3, &floats(&[1.0, 0.5, 1.0, 1.0])).unwrap();
    let float4 = format::R32G32B32A32_FLOAT;
    let mut targets = [None; 8];
    targets[0] = Some(Target {
        sid: 1,
        format: format::B8G8R8A8_UNORM,
        mip: 0,
        first_layer: 0,
    });
    let draw = Draw {
        vs: &vs,
        ps: Some(&ps),
        attributes: vec![
            VertexAttribute {
                location: 0,
                binding: 0,
                format: float4,
                offset: 0,
            },
            VertexAttribute {
                location: 1,
                binding: 0,
                format: float4,
                offset: 16,
            },
        ],
        buffers: vec![VertexBuffer {
            binding: 0,
            sid: Some(2),
            offset: 0,
            stride: 32,
            per_instance: false,
        }],
        index: None,
        topology: 1,
        rasterizer: default_rasterizer(),
        depth_stencil: (default_depth_stencil(), 0),
        blend: (default_blend(), [0.0; 4], u32::MAX),
        targets,
        depth: None,
        viewports: vec![svga_core::dx::Viewport {
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 8.0,
            min_depth: 0.0,
            max_depth: 1.0,
        }],
        scissors: Vec::new(),
        resources: vec![Bound {
            binding: 256,
            resource: Resource::Buffer {
                sid: 3,
                offset: 0,
                size: 16,
            },
        }],
        call: Call::Draw {
            vertices: 3,
            instances: 1,
            first_vertex: 0,
            first_instance: 0,
        },
    };
    if let Err(e) = r.draw(&draw) {
        eprintln!("{} cannot draw: {e}", r.name());
        return;
    }
    let mut back = vec![0u8; 256];
    r.download(1, &mut back).unwrap();
    let pixel = |x: usize, y: usize| back[(y * 8 + x) * 4..][..4].to_vec();
    let orange = vec![0, 128, 255, 255]; // B, G, R, A
    assert_eq!(pixel(0, 0), orange, "top-left, on {}", r.name());
    assert_eq!(pixel(6, 0), orange, "top-right: not upside down");
    assert_eq!(pixel(0, 6), orange, "bottom-left");
    assert_eq!(pixel(7, 7), [0; 4], "bottom-right stays black");
    // The same triangle counter-clockwise is a back face: culled.
    r.upload(1, &[0; 256]).unwrap();
    let mut reversed = vertices.clone();
    reversed[32..64].copy_from_slice(&vertices[64..96]);
    reversed[64..96].copy_from_slice(&vertices[32..64]);
    r.upload(2, &reversed).unwrap();
    r.draw(&draw).unwrap();
    r.download(1, &mut back).unwrap();
    assert!(back.iter().all(|&b| b == 0), "back faces are culled");
}

/// VS: `o0 (position) = v0; o1 = v1`. PS: `o0 = sample(t0, s0, v1.xy)`.
fn texture_shaders() -> (svga_shader::Shader, svga_shader::Shader) {
    use svga_shader::opcode as op;
    use tokens::*;
    let o = operand(2, ALL, 1);
    let v = operand(1, SWZ, 1);
    let vs = program(
        1,
        &[
            instr(op::DCL_INPUT, &[operand(1, ALL, 1), 0]),
            instr(op::DCL_INPUT, &[operand(1, ALL, 1), 1]),
            instr(op::DCL_OUTPUT_SIV, &[o, 0, 1]),
            instr(op::DCL_OUTPUT, &[o, 1]),
            instr(op::MOV, &[o, 0, v, 0]),
            instr(op::MOV, &[o, 1, v, 1]),
            instr(op::RET, &[]),
        ],
    );
    let t0 = operand(7, SWZ, 1);
    let s0 = operand(6, 0, 1);
    let ps = program(
        0,
        &[
            instr(op::DCL_SAMPLER, &[s0, 0]),
            // Texture2D, float return type.
            instr(op::DCL_RESOURCE | (3 << 11), &[t0, 0, 0x5555]),
            instr(
                op::DCL_INPUT_PS | (2 << 11),
                &[operand(1, 2 | (3 << 4), 1), 1],
            ),
            instr(op::DCL_OUTPUT, &[o, 0]),
            instr(op::SAMPLE, &[o, 0, v, 1, t0, 0, s0, 0]),
            instr(op::RET, &[]),
        ],
    );
    let options = svga_shader::Options::default();
    (
        svga_shader::translate_words(&vs, &options).unwrap(),
        svga_shader::translate_words(&ps, &options).unwrap(),
    )
}

#[test]
fn an_indexed_textured_quad_behind_and_in_front_of_depth() {
    let Some(mut r) = renderer() else { return };
    let (vs, ps) = texture_shaders();
    let target = layout(format::B8G8R8A8_UNORM, 4, 4, 1, 1);
    r.create(1, &target, 0).unwrap();
    r.upload(1, &[0; 64]).unwrap();
    // 2x2 texture: red, green / blue, white (B, G, R, A bytes).
    r.create(2, &layout(format::B8G8R8A8_UNORM, 2, 2, 1, 1), 0)
        .unwrap();
    #[rustfmt::skip]
    let texels = [0, 0, 255, 255,  0, 255, 0, 255,  255, 0, 0, 255,  255, 255, 255, 255];
    r.upload(2, &texels).unwrap();
    // Depth, typeless as Direct3D creates readable depth textures.
    r.create(
        3,
        &layout(format::R32_TYPELESS, 4, 4, 1, 1),
        svga_proto::surface_flag::BIND_DEPTH_STENCIL,
    )
    .unwrap();
    let depth = Target {
        sid: 3,
        format: format::D32_FLOAT,
        mip: 0,
        first_layer: 0,
    };
    r.clear_depth_stencil(depth, 2, 0.5, 0).unwrap();
    // A full quad at depth z: position, texture coordinates.
    let quad = |z: f32| {
        floats(&[
            -1.0, 1.0, z, 1.0, 0.0, 0.0, 0.0, 0.0, //
            1.0, 1.0, z, 1.0, 1.0, 0.0, 0.0, 0.0, //
            -1.0, -1.0, z, 1.0, 0.0, 1.0, 0.0, 0.0, //
            1.0, -1.0, z, 1.0, 1.0, 1.0, 0.0, 0.0,
        ])
    };
    r.create(4, &layout(format::BUFFER, 128, 1, 1, 1), 0)
        .unwrap();
    // Two clockwise triangles, 16-bit indices.
    let indices: Vec<u8> = [0u16, 1, 2, 2, 1, 3]
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    r.create(5, &layout(format::BUFFER, 12, 1, 1, 1), 0)
        .unwrap();
    r.upload(5, &indices).unwrap();
    let float4 = format::R32G32B32A32_FLOAT;
    let mut targets = [None; 8];
    targets[0] = Some(Target {
        sid: 1,
        format: format::B8G8R8A8_UNORM,
        mip: 0,
        first_layer: 0,
    });
    let point = svga_core::dx::SamplerState {
        filter: 0,
        address: [3; 3],
        mip_lod_bias: 0.0,
        max_anisotropy: 1,
        comparison: 1,
        border_color: [0.0; 4],
        min_lod: 0.0,
        max_lod: f32::MAX,
    };
    let draw = Draw {
        vs: &vs,
        ps: Some(&ps),
        attributes: vec![
            VertexAttribute {
                location: 0,
                binding: 0,
                format: float4,
                offset: 0,
            },
            VertexAttribute {
                location: 1,
                binding: 0,
                format: float4,
                offset: 16,
            },
        ],
        buffers: vec![VertexBuffer {
            binding: 0,
            sid: Some(4),
            offset: 0,
            stride: 32,
            per_instance: false,
        }],
        index: Some((5, format::R16_UINT, 0)),
        topology: 1,
        rasterizer: default_rasterizer(),
        depth_stencil: (default_depth_stencil(), 0),
        blend: (default_blend(), [0.0; 4], u32::MAX),
        targets,
        depth: Some(depth),
        viewports: vec![svga_core::dx::Viewport {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
            min_depth: 0.0,
            max_depth: 1.0,
        }],
        scissors: Vec::new(),
        resources: vec![
            Bound {
                binding: 256 + 16,
                resource: Resource::Sampler(point),
            },
            Bound {
                binding: 256 + 32,
                resource: Resource::Texture {
                    sid: 2,
                    format: format::B8G8R8A8_UNORM,
                    first_mip: 0,
                    mips: u32::MAX,
                    first_layer: 0,
                    layers: 1,
                },
            },
        ],
        call: Call::Indexed {
            indices: 6,
            instances: 1,
            first_index: 0,
            base_vertex: 0,
            first_instance: 0,
        },
    };
    // Behind the cleared depth (0.75 > 0.5): nothing.
    r.upload(4, &quad(0.75)).unwrap();
    if let Err(e) = r.draw(&draw) {
        eprintln!("{} cannot draw: {e}", r.name());
        return;
    }
    let mut back = vec![0u8; 64];
    r.download(1, &mut back).unwrap();
    assert!(back.iter().all(|&b| b == 0), "hidden, on {}", r.name());
    // In front (0.25): the texture, one texel per 2x2 pixels.
    r.upload(4, &quad(0.25)).unwrap();
    r.draw(&draw).unwrap();
    r.download(1, &mut back).unwrap();
    let pixel = |x: usize, y: usize| back[(y * 4 + x) * 4..][..4].to_vec();
    assert_eq!(pixel(0, 0), [0, 0, 255, 255], "red");
    assert_eq!(pixel(3, 0), [0, 255, 0, 255], "green");
    assert_eq!(pixel(0, 3), [255, 0, 0, 255], "blue");
    assert_eq!(pixel(3, 3), [255; 4], "white");
    // The depth buffer now holds 0.25, in the guest's layout.
    let mut depths = vec![0u8; 64];
    r.download(3, &mut depths).unwrap();
    assert!(
        depths
            .chunks_exact(4)
            .all(|d| f32::from_le_bytes([d[0], d[1], d[2], d[3]]) == 0.25)
    );
}

#[test]
fn a_pixel_shader_reads_a_buffer_view() {
    use svga_shader::opcode as op;
    use tokens::*;
    let Some(mut r) = renderer() else { return };
    let (vs, _) = color_shaders();
    // ld o0, l(0, 0, 0, 0), t0 (a Buffer<float4>).
    let t0 = operand(7, SWZ, 1);
    let o = operand(2, ALL, 1);
    let ps = program(
        0,
        &[
            instr(op::DCL_RESOURCE | (1 << 11), &[t0, 0, 0x5555]),
            instr(op::DCL_OUTPUT, &[o, 0]),
            instr(op::LD, &[o, 0, operand(4, 2, 0), 0, 0, 0, 0, t0, 0]),
            instr(op::RET, &[]),
        ],
    );
    let ps = svga_shader::translate_words(&ps, &svga_shader::Options::default()).unwrap();
    r.create(1, &layout(format::B8G8R8A8_UNORM, 2, 2, 1, 1), 0)
        .unwrap();
    r.upload(1, &[0; 16]).unwrap();
    // A full-screen triangle (colors unused).
    let mut vertices = Vec::new();
    for (x, y) in [(-1.0f32, 1.0f32), (3.0, 1.0), (-1.0, -3.0)] {
        vertices.extend(floats(&[x, y, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]));
    }
    r.create(2, &layout(format::BUFFER, 96, 1, 1, 1), 0)
        .unwrap();
    r.upload(2, &vertices).unwrap();
    // Red, green, blue; the view starts at green.
    r.create(3, &layout(format::BUFFER, 48, 1, 1, 1), 0)
        .unwrap();
    r.upload(
        3,
        &floats(&[1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0]),
    )
    .unwrap();
    let mut targets = [None; 8];
    targets[0] = Some(Target {
        sid: 1,
        format: format::B8G8R8A8_UNORM,
        mip: 0,
        first_layer: 0,
    });
    let float4 = format::R32G32B32A32_FLOAT;
    let attribute = |location, offset| VertexAttribute {
        location,
        binding: 0,
        format: float4,
        offset,
    };
    let draw = Draw {
        vs: &vs,
        ps: Some(&ps),
        attributes: vec![attribute(0, 0), attribute(1, 16)],
        buffers: vec![VertexBuffer {
            binding: 0,
            sid: Some(2),
            offset: 0,
            stride: 32,
            per_instance: false,
        }],
        index: None,
        topology: 1,
        rasterizer: default_rasterizer(),
        depth_stencil: (default_depth_stencil(), 0),
        blend: (default_blend(), [0.0; 4], u32::MAX),
        targets,
        depth: None,
        viewports: Vec::new(),
        scissors: Vec::new(),
        resources: vec![Bound {
            binding: 256 + 32,
            resource: Resource::TexelBuffer {
                sid: 3,
                format: float4,
                offset: 16,
                size: 32,
            },
        }],
        call: Call::Draw {
            vertices: 3,
            instances: 1,
            first_vertex: 0,
            first_instance: 0,
        },
    };
    if let Err(e) = r.draw(&draw) {
        eprintln!("{} cannot draw: {e}", r.name());
        return;
    }
    let mut back = vec![0u8; 16];
    r.download(1, &mut back).unwrap();
    assert!(
        back.chunks(4).all(|p| p == [0, 255, 0, 255]),
        "green on {}: {back:?}",
        r.name()
    );
}
