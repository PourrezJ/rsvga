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
