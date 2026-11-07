//! DX context commands (and guest-backed object commands) and context
//! table entries from arbitrary bytes: the decoders must never panic,
//! decoded arrays stay within the Direct3D limits, and a decoded entry
//! encodes back to the same bytes after a round trip.
#![no_main]

use libfuzzer_sys::fuzz_target;
use svga_core::cotable;
use svga_core::dx::{DxCommand, parse};
use svga_proto::{cmd3d, dx};

fuzz_target!(|data: &[u8]| {
    let [lo, hi, size_lo, size_hi, rest @ ..] = data else {
        return;
    };
    // An ID in the 3D range, a size that may disagree with the payload.
    let id = cmd3d::BASE + u32::from(u16::from_le_bytes([*lo, *hi])) % (cmd3d::MAX - cmd3d::BASE);
    let size = u32::from(u16::from_le_bytes([*size_lo, *size_hi]));
    let words: Vec<u32> = rest
        .chunks(4)
        .map(|chunk| {
            let mut bytes = [0u8; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(bytes)
        })
        .collect();
    let _ = svga_core::gb::parse(id, &words);
    // The same bytes as an entry of a context table.
    let table = u32::from(*lo) % (dx::cotable::MAX + 1);
    if let Ok(object) = cotable::decode(table, rest) {
        let bytes = cotable::encode(1, &object);
        let again = cotable::decode(table, &bytes).expect("an encoded entry decodes");
        assert_eq!(cotable::encode(1, &again), bytes, "{object:?}");
    }
    if let Some(Ok(command)) = parse(id, size, &words) {
        let bounded = match &command {
            DxCommand::SetShaderResources { views, .. } => views.len() <= dx::MAX_SRVIEWS,
            DxCommand::SetSamplers { samplers, .. } => samplers.len() <= dx::MAX_SAMPLERS,
            DxCommand::SetVertexBuffers { buffers, .. } => buffers.len() <= dx::MAX_VERTEXBUFFERS,
            DxCommand::SetRenderTargets { targets, .. } => targets.len() <= dx::MAX_RENDER_TARGETS,
            DxCommand::SetSoTargets { targets } => targets.len() <= dx::MAX_SOTARGETS,
            DxCommand::SetViewports { viewports } => viewports.len() <= dx::MAX_VIEWPORTS,
            DxCommand::SetScissorRects { rects } => rects.len() <= dx::MAX_SCISSORRECTS,
            DxCommand::DefineElementLayout { elements, .. } => elements.len() <= dx::MAX_ELEMENTS,
            DxCommand::DefineStreamOutput { decls, .. } => decls.len() <= dx::MAX_STREAMOUT_DECLS,
            _ => true,
        };
        assert!(bounded, "{command:?}");
    }
});
