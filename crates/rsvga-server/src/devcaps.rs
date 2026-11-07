//! 3D device capabilities (`SVGA3D_DEVCAP_*`).
//!
//! Drivers read them through `SVGA_REG_DEV_CAP` (with guest-backed
//! objects) or from a record in the FIFO 3D caps area. rsvga has no 3D yet:
//! its table is empty, so `SVGA3D_DEVCAP_3D` reads 0 everywhere. A 3D table
//! is only used to probe drivers (`RSVGA_PROBE_CAPS` with `SVGA_CAP_3D`).

use svga_proto::{devcap, fifo, fifo_caps_record};

/// The table, indexed by devcap: empty without 3D.
pub fn table(three_d: bool) -> Vec<u32> {
    if !three_d {
        return Vec::new();
    }
    let mut table = vec![0u32; devcap::MAX as usize];
    for (index, value) in [
        (devcap::THREE_D, 1),
        (devcap::VERTEX_SHADER, 1),
        (devcap::FRAGMENT_SHADER, 1),
        (devcap::MAX_RENDER_TARGETS, 8),
        (devcap::MAX_SIMULTANEOUS_RENDER_TARGETS, 8),
        (devcap::MAX_TEXTURE_WIDTH, 16384),
        (devcap::MAX_TEXTURE_HEIGHT, 16384),
        (devcap::MAX_CONTEXT_IDS, 256),
        (devcap::MAX_SURFACE_IDS, 32768),
        (devcap::DXCONTEXT, 1),
        (devcap::DX_MAX_VERTEXBUFFERS, 32),
        (devcap::DX_MAX_CONSTANT_BUFFERS, 14),
        (devcap::SM41, 1),
    ] {
        table[index as usize] = value;
    }
    table
}

/// Words to store at [`fifo::CAPS_3D`]: one `DEVCAPS` record holding the
/// non-zero entries, then an empty record that ends the list. Entries that
/// do not fit in the area are left out (the guest then reads them as 0).
pub fn fifo_record(table: &[u32]) -> Vec<u32> {
    let area = (fifo::CAPS_3D_LAST - fifo::CAPS_3D + 1) as usize;
    // Two header words, then pairs, then the two-word terminator.
    let room = (area - 4) / 2;
    let pairs: Vec<(u32, u32)> = (0u32..)
        .zip(table.iter().copied())
        .filter(|&(_, value)| value != 0)
        .take(room)
        .collect();
    if pairs.is_empty() {
        return vec![0, 0];
    }
    let length = 2 + 2 * pairs.len();
    let mut words = Vec::with_capacity(length + 2);
    words.push(u32::try_from(length).unwrap_or(u32::MAX));
    words.push(fifo_caps_record::DEVCAPS);
    for (index, value) in pairs {
        words.extend([index, value]);
    }
    words.extend([0, 0]);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_3d_means_an_empty_table_and_record() {
        assert!(table(false).is_empty());
        assert_eq!(fifo_record(&table(false)), [0, 0]);
    }

    #[test]
    fn the_fifo_record_lists_non_zero_entries() {
        let mut t = vec![0u32; 10];
        t[0] = 1;
        t[7] = 9;
        assert_eq!(
            fifo_record(&t),
            [6, fifo_caps_record::DEVCAPS, 0, 1, 7, 9, 0, 0]
        );
        let full = table(true);
        assert_eq!(full[devcap::THREE_D as usize], 1);
        let record = fifo_record(&full);
        assert!(record.len() <= (fifo::CAPS_3D_LAST - fifo::CAPS_3D + 1) as usize);
        // Never more than the area, even with every entry set.
        let crowded = vec![1u32; devcap::MAX as usize];
        assert!(fifo_record(&crowded).len() <= (fifo::CAPS_3D_LAST - fifo::CAPS_3D + 1) as usize);
    }
}
