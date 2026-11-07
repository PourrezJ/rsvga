//! Memory objects (MOBs): guest memory described by a page table.
//!
//! `SVGA_3D_CMD_DEFINE_GB_MOB64` (and the object tables) give a root page
//! number, a size and a depth (`SVGAMobFormat`):
//!
//! - depth 0 (`PT_0`, `PT64_0`) and `RANGE`: the data starts at the root
//!   page and is physically contiguous;
//! - depth 1: the root page holds the page numbers of the data pages;
//! - depth 2: the root page holds the page numbers of depth-1 pages.
//!
//! Page numbers are 32-bit (`PT_*`) or 64-bit (`PT64_*`). The page list is
//! read once, when the MOB is defined, into device memory: later changes of
//! the guest page tables cannot affect it.

use svga_proto::mobfmt;

/// Size of a guest page.
pub const PAGE_SIZE: u64 = 4096;
/// [`PAGE_SIZE`] as a buffer length.
pub const PAGE_BYTES: usize = 4096;

/// Why a MOB cannot be mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobError {
    /// Unknown `SVGAMobFormat`.
    UnknownFormat(u32),
    /// More pages than the device accepts, or than the table can describe.
    TooLarge(u64),
    /// A page-table page is not in guest RAM.
    Unreadable(u64),
}

impl core::fmt::Display for MobError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownFormat(format) => write!(f, "unknown MOB format {format}"),
            Self::TooLarge(pages) => write!(f, "MOB of {pages} pages is too large"),
            Self::Unreadable(ppn) => write!(f, "page table page {ppn:#x} is not in guest RAM"),
        }
    }
}

/// Reads guest page `ppn` into `out`; false if it is not guest RAM.
pub trait PageReader {
    /// Reads one page.
    fn read_page(&mut self, ppn: u64, out: &mut [u8; PAGE_BYTES]) -> bool;
}

impl<F: FnMut(u64, &mut [u8; PAGE_BYTES]) -> bool> PageReader for F {
    fn read_page(&mut self, ppn: u64, out: &mut [u8; PAGE_BYTES]) -> bool {
        self(ppn, out)
    }
}

/// Reads up to `count` page numbers from page table page `ppn`.
fn table_entries(
    reader: &mut impl PageReader,
    ppn: u64,
    wide: bool,
    count: usize,
    out: &mut Vec<u64>,
) -> Result<(), MobError> {
    let mut page = [0u8; PAGE_BYTES];
    if !reader.read_page(ppn, &mut page) {
        return Err(MobError::Unreadable(ppn));
    }
    if wide {
        out.extend(
            page.chunks_exact(8)
                .take(count)
                .map(|c| u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])),
        );
    } else {
        out.extend(
            page.chunks_exact(4)
                .take(count)
                .map(|c| u64::from(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))),
        );
    }
    Ok(())
}

/// The data page numbers of a MOB of `size` bytes, at most `max_pages`.
pub fn pages(
    format: u32,
    base: u64,
    size: u32,
    max_pages: u64,
    reader: &mut impl PageReader,
) -> Result<Vec<u64>, MobError> {
    let count = u64::from(size).div_ceil(PAGE_SIZE);
    if count > max_pages {
        return Err(MobError::TooLarge(count));
    }
    let n = usize::try_from(count).map_err(|_| MobError::TooLarge(count))?;
    let (depth, wide) = match format {
        mobfmt::PT_0 | mobfmt::PT64_0 | mobfmt::RANGE => (0, false),
        mobfmt::PT_1 => (1, false),
        mobfmt::PT_2 => (2, false),
        mobfmt::PT64_1 => (1, true),
        mobfmt::PT64_2 => (2, true),
        mobfmt::EMPTY => return Ok(Vec::new()),
        other => return Err(MobError::UnknownFormat(other)),
    };
    let per_page = if wide { 512 } else { 1024 };
    let mut out = Vec::with_capacity(n);
    match depth {
        0 => {
            let mut ppn = base;
            for _ in 0..n {
                out.push(ppn);
                ppn = ppn.checked_add(1).ok_or(MobError::TooLarge(count))?;
            }
        }
        1 => {
            if n > per_page {
                return Err(MobError::TooLarge(count));
            }
            table_entries(reader, base, wide, n, &mut out)?;
        }
        _ => {
            let tables = n.div_ceil(per_page);
            if tables > per_page {
                return Err(MobError::TooLarge(count));
            }
            let mut level1 = Vec::with_capacity(tables);
            table_entries(reader, base, wide, tables, &mut level1)?;
            for table in level1 {
                let left = n.saturating_sub(out.len()).min(per_page);
                table_entries(reader, table, wide, left, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// Splits `len` bytes at `offset` of a MOB into guest physical pieces
/// `(address, length)`; `None` if the range leaves the MOB.
pub fn pieces(pages: &[u64], offset: u64, len: u64) -> Option<Vec<(u64, u64)>> {
    let end = offset.checked_add(len)?;
    if end > u64::try_from(pages.len()).ok()?.checked_mul(PAGE_SIZE)? {
        return None;
    }
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut at = offset;
    while at < end {
        let index = usize::try_from(at / PAGE_SIZE).ok()?;
        let in_page = at % PAGE_SIZE;
        let take = PAGE_SIZE
            .saturating_sub(in_page)
            .min(end.saturating_sub(at));
        let gpa = pages
            .get(index)?
            .checked_mul(PAGE_SIZE)?
            .checked_add(in_page)?;
        // Merge with the previous piece when the pages are contiguous.
        match out.last_mut() {
            Some((start, length)) if start.checked_add(*length) == Some(gpa) => {
                *length = length.saturating_add(take);
            }
            _ => out.push((gpa, take)),
        }
        at = at.saturating_add(take);
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Guest RAM made of page tables only.
    struct Tables(HashMap<u64, Vec<u64>>, bool);

    impl PageReader for Tables {
        fn read_page(&mut self, ppn: u64, out: &mut [u8; 4096]) -> bool {
            let Some(entries) = self.0.get(&ppn) else {
                return false;
            };
            out.fill(0);
            for (i, e) in entries.iter().enumerate() {
                if self.1 {
                    out[i * 8..i * 8 + 8].copy_from_slice(&e.to_le_bytes());
                } else {
                    out[i * 4..i * 4 + 4].copy_from_slice(&(*e as u32).to_le_bytes());
                }
            }
            true
        }
    }

    #[test]
    fn depth_zero_and_range_are_contiguous() {
        let mut none = Tables(HashMap::new(), true);
        assert_eq!(
            pages(mobfmt::PT64_0, 0x100, 4096, 16, &mut none),
            Ok(vec![0x100])
        );
        assert_eq!(
            pages(mobfmt::RANGE, 0x100, 8193, 16, &mut none),
            Ok(vec![0x100, 0x101, 0x102])
        );
        assert_eq!(pages(mobfmt::EMPTY, 0, 0, 16, &mut none), Ok(vec![]));
        assert_eq!(
            pages(99, 0, 1, 16, &mut none),
            Err(MobError::UnknownFormat(99))
        );
    }

    #[test]
    fn one_and_two_levels() {
        let mut t = Tables(HashMap::from([(7, vec![0x50, 0x20, 0x30])]), true);
        assert_eq!(
            pages(mobfmt::PT64_1, 7, 3 * 4096, 64, &mut t),
            Ok(vec![0x50, 0x20, 0x30])
        );
        // 600 pages: two level-1 tables (512 + 88) under the root.
        let first: Vec<u64> = (1000..1512).collect();
        let second: Vec<u64> = (2000..2088).collect();
        let mut t = Tables(
            HashMap::from([(9, vec![10, 11]), (10, first), (11, second)]),
            true,
        );
        let list = pages(mobfmt::PT64_2, 9, 600 * 4096, 1024, &mut t).unwrap();
        assert_eq!(list.len(), 600);
        assert_eq!(
            (list[0], list[511], list[512], list[599]),
            (1000, 1511, 2000, 2087)
        );
        // 32-bit entries.
        let mut t = Tables(HashMap::from([(3, vec![0x44, 0x45])]), false);
        assert_eq!(
            pages(mobfmt::PT_1, 3, 8192, 64, &mut t),
            Ok(vec![0x44, 0x45])
        );
    }

    #[test]
    fn hostile_tables_are_refused() {
        let mut t = Tables(HashMap::new(), true);
        assert_eq!(
            pages(mobfmt::PT64_1, 7, 4096, 64, &mut t),
            Err(MobError::Unreadable(7))
        );
        assert_eq!(
            pages(mobfmt::PT64_1, 7, 513 * 4096, 1024, &mut t),
            Err(MobError::TooLarge(513)),
            "a single level-1 page holds 512 entries"
        );
        assert_eq!(
            pages(mobfmt::PT64_0, 0, u32::MAX, 64, &mut t),
            Err(MobError::TooLarge(1 << 20))
        );
    }

    #[test]
    fn ranges_split_on_page_boundaries() {
        let list = [5, 6, 9];
        assert_eq!(pieces(&list, 100, 50), Some(vec![(5 * 4096 + 100, 50)]));
        // 96 bytes of page 5, all of page 6 (contiguous: merged), 808 of page 9.
        assert_eq!(
            pieces(&list, 4000, 5000),
            Some(vec![(5 * 4096 + 4000, 96 + 4096), (9 * 4096, 808)])
        );
        assert_eq!(pieces(&list, 3 * 4096 - 1, 2), None, "past the end");
        assert_eq!(pieces(&list, 0, 0), Some(vec![]));
    }
}
