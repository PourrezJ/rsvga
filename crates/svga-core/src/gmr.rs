//! Guest Memory Regions (GMR2): guest-defined lists of physical pages that
//! the device addresses as `(gmr id, byte offset)`.
//!
//! `SVGA_CMD_DEFINE_GMR2` creates, resizes or destroys a region;
//! `SVGA_CMD_REMAP_GMR2` sets the physical page backing a range of it. This
//! module is the pure bookkeeping: it never touches guest memory itself. A
//! remap whose page list lives in another GMR (`VIA_GMR`) is resolved by the
//! device, which reads the list and passes it here.

use core::fmt;

/// Guest page size.
pub const PAGE_SIZE: u64 = 4096;

/// Marks a page that has not been remapped yet.
const UNMAPPED: u64 = u64::MAX;

/// Why a GMR operation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GmrError {
    /// The id is not below [`GmrTable::max_ids`].
    BadId(u32),
    /// The id has no region.
    Undefined(u32),
    /// Defining this region would exceed the device's page budget.
    TooManyPages,
    /// The page range lies outside the region.
    OutOfRange,
    /// The page list has the wrong number of entries.
    BadPageList,
    /// A byte of the access falls on a page that was never remapped.
    UnmappedPage,
}

impl fmt::Display for GmrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::BadId(id) => write!(f, "GMR id {id} is out of range"),
            Self::Undefined(id) => write!(f, "GMR {id} is not defined"),
            Self::TooManyPages => write!(f, "GMR page budget exceeded"),
            Self::OutOfRange => write!(f, "page range outside the GMR"),
            Self::BadPageList => write!(f, "page list does not match the page count"),
            Self::UnmappedPage => write!(f, "access to a page that was never mapped"),
        }
    }
}

impl std::error::Error for GmrError {}

fn bytes_of(pages: &[u64]) -> u64 {
    (pages.len() as u64).saturating_mul(PAGE_SIZE)
}

/// All GMRs of the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GmrTable {
    regions: Vec<Option<Vec<u64>>>,
    max_pages: u64,
    used_pages: u64,
}

impl GmrTable {
    /// A table accepting ids below `max_ids` and `max_pages` pages in total
    /// (the values advertised in `SVGA_REG_GMR_MAX_IDS` and
    /// `SVGA_REG_GMRS_MAX_PAGES`).
    pub fn new(max_ids: u32, max_pages: u64) -> Self {
        Self {
            regions: vec![None; max_ids as usize],
            max_pages,
            used_pages: 0,
        }
    }

    /// Number of ids.
    pub fn max_ids(&self) -> u32 {
        u32::try_from(self.regions.len()).unwrap_or(u32::MAX)
    }

    fn slot(&mut self, id: u32) -> Result<&mut Option<Vec<u64>>, GmrError> {
        self.regions.get_mut(id as usize).ok_or(GmrError::BadId(id))
    }

    /// `DEFINE_GMR2`: (re)creates region `id` with `num_pages` unmapped
    /// pages, or destroys it when `num_pages` is 0.
    pub fn define(&mut self, id: u32, num_pages: u32) -> Result<(), GmrError> {
        let old = self
            .slot(id)?
            .as_ref()
            .map_or(0, |pages| pages.len() as u64);
        // Invariant: `old <= used_pages <= max_pages`.
        let others = self.used_pages.saturating_sub(old);
        let used = others
            .checked_add(u64::from(num_pages))
            .ok_or(GmrError::TooManyPages)?;
        if used > self.max_pages {
            return Err(GmrError::TooManyPages);
        }
        self.used_pages = used;
        *self.slot(id)? = (num_pages > 0).then(|| vec![UNMAPPED; num_pages as usize]);
        Ok(())
    }

    /// `REMAP_GMR2` with an explicit page list: pages `offset..offset+count`
    /// of region `id` now map to `pages` (one entry per page, or a single
    /// entry repeated when `single` is set).
    pub fn remap(
        &mut self,
        id: u32,
        offset: u32,
        count: u32,
        pages: &[u64],
        single: bool,
    ) -> Result<(), GmrError> {
        let region = self.slot(id)?.as_mut().ok_or(GmrError::Undefined(id))?;
        let start = offset as usize;
        let end = start
            .checked_add(count as usize)
            .ok_or(GmrError::OutOfRange)?;
        let target = region.get_mut(start..end).ok_or(GmrError::OutOfRange)?;
        match (single, pages) {
            (true, [page]) => target.fill(*page),
            (false, list) if list.len() == target.len() => target.copy_from_slice(list),
            _ => return Err(GmrError::BadPageList),
        }
        Ok(())
    }

    /// Size of region `id` in bytes.
    pub fn size(&self, id: u32) -> Result<u64, GmrError> {
        let region = self.regions.get(id as usize).ok_or(GmrError::BadId(id))?;
        let pages = region.as_ref().ok_or(GmrError::Undefined(id))?;
        Ok(bytes_of(pages))
    }

    /// Splits `len` bytes at `offset` of region `id` into guest-physical
    /// pieces, each inside a single page.
    pub fn translate(&self, id: u32, offset: u64, len: u64) -> Result<Vec<(u64, u64)>, GmrError> {
        let region = self.regions.get(id as usize).ok_or(GmrError::BadId(id))?;
        let pages = region.as_ref().ok_or(GmrError::Undefined(id))?;
        let end = offset.checked_add(len).ok_or(GmrError::OutOfRange)?;
        if end > bytes_of(pages) {
            return Err(GmrError::OutOfRange);
        }
        let mut pieces: Vec<(u64, u64)> = Vec::new();
        let mut at = offset;
        while at < end {
            let index = usize::try_from(at / PAGE_SIZE).map_err(|_| GmrError::OutOfRange)?;
            let page = *pages.get(index).ok_or(GmrError::OutOfRange)?;
            if page == UNMAPPED {
                return Err(GmrError::UnmappedPage);
            }
            let within = at % PAGE_SIZE;
            // `within < PAGE_SIZE` and `at < end`: neither subtraction wraps.
            let take = PAGE_SIZE.saturating_sub(within).min(end.saturating_sub(at));
            let gpa = page
                .checked_mul(PAGE_SIZE)
                .and_then(|base| base.checked_add(within))
                .ok_or(GmrError::OutOfRange)?;
            match pieces.last_mut() {
                // Merge physically contiguous pages into one piece.
                Some((last_gpa, last_len)) if last_gpa.checked_add(*last_len) == Some(gpa) => {
                    *last_len = last_len.saturating_add(take);
                }
                _ => pieces.push((gpa, take)),
            }
            at = at.saturating_add(take);
        }
        Ok(pieces)
    }

    /// Forgets every region (device reset).
    pub fn clear(&mut self) {
        self.regions.iter_mut().for_each(|slot| *slot = None);
        self.used_pages = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn define_remap_translate() {
        let mut table = GmrTable::new(8, 100);
        table.define(1, 3).unwrap();
        assert_eq!(table.size(1), Ok(3 * PAGE_SIZE));
        table.remap(1, 0, 3, &[10, 11, 20], false).unwrap();
        assert_eq!(
            table.translate(1, 4000, 200),
            Ok(vec![(10 * PAGE_SIZE + 4000, 200)]),
            "pages 10 and 11 are contiguous: one piece"
        );
        assert_eq!(
            table.translate(1, PAGE_SIZE + 4090, 10),
            Ok(vec![(11 * PAGE_SIZE + 4090, 6), (20 * PAGE_SIZE, 4)])
        );
        assert_eq!(
            table.translate(1, 3 * PAGE_SIZE - 1, 2),
            Err(GmrError::OutOfRange)
        );
        assert_eq!(table.translate(1, u64::MAX, 2), Err(GmrError::OutOfRange));
    }

    #[test]
    fn unmapped_pages_and_single_ppn() {
        let mut table = GmrTable::new(8, 100);
        table.define(2, 4).unwrap();
        assert_eq!(table.translate(2, 0, 1), Err(GmrError::UnmappedPage));
        table.remap(2, 1, 3, &[7], true).unwrap();
        assert_eq!(
            table.translate(2, PAGE_SIZE, 1),
            Ok(vec![(7 * PAGE_SIZE, 1)])
        );
        assert_eq!(
            table.remap(2, 0, 2, &[1], false),
            Err(GmrError::BadPageList)
        );
        assert_eq!(
            table.remap(2, 3, 2, &[1, 2], false),
            Err(GmrError::OutOfRange)
        );
        assert_eq!(
            table.remap(5, 0, 1, &[1], false),
            Err(GmrError::Undefined(5))
        );
        assert_eq!(
            table.remap(2, u32::MAX, 2, &[1, 2], false),
            Err(GmrError::OutOfRange)
        );
    }

    #[test]
    fn page_budget_ids_and_destroy() {
        let mut table = GmrTable::new(4, 10);
        table.define(0, 6).unwrap();
        assert_eq!(table.define(1, 5), Err(GmrError::TooManyPages));
        table.define(0, 9).unwrap(); // resizing reuses its own pages
        table.define(0, 0).unwrap();
        assert_eq!(table.size(0), Err(GmrError::Undefined(0)));
        table.define(1, 10).unwrap();
        assert_eq!(table.define(4, 1), Err(GmrError::BadId(4)));
        table.clear();
        table.define(2, 10).unwrap();
    }
}
