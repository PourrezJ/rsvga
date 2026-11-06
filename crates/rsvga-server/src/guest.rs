//! Guest physical memory, as announced by QEMU with `DMA_MAP`.
//!
//! Each window is mapped when QEMU shares a sealed file for it; windows
//! without a usable file are remembered (so unmaps match) but cannot be
//! accessed. Every access is bounds-checked and may span several windows.

use std::os::fd::OwnedFd;

use vfio_user::Errno;

use crate::log;
use crate::shm::GuestWindow;

struct Window {
    address: u64,
    size: u64,
    mapping: Option<GuestWindow>,
}

impl Window {
    fn end(&self) -> u64 {
        self.address.saturating_add(self.size)
    }
}

/// The DMA windows of the current client.
#[derive(Default)]
pub struct GuestMemory {
    windows: Vec<Window>,
}

impl GuestMemory {
    /// Records a window and maps it if `fd` allows. Overlaps are refused.
    pub fn add(
        &mut self,
        address: u64,
        size: u64,
        fd: Option<OwnedFd>,
        offset: u64,
        writable: bool,
    ) -> Result<(), Errno> {
        let end = address.checked_add(size).ok_or(Errno::EINVAL)?;
        if size == 0 {
            return Err(Errno::EINVAL);
        }
        if self
            .windows
            .iter()
            .any(|w| address < w.end() && w.address < end)
        {
            return Err(Errno::EEXIST);
        }
        let mapping = fd.and_then(|fd| match GuestWindow::map(&fd, offset, size, writable) {
            Ok(mapping) => Some(mapping),
            Err(error) => {
                log!("DMA window {address:#x}+{size:#x} not mapped: {error}");
                None
            }
        });
        self.windows.push(Window {
            address,
            size,
            mapping,
        });
        self.windows.sort_by_key(|w| w.address);
        Ok(())
    }

    /// Forgets the window that exactly matches; false if there is none.
    pub fn remove(&mut self, address: u64, size: u64) -> bool {
        let before = self.windows.len();
        self.windows
            .retain(|w| !(w.address == address && w.size == size));
        self.windows.len() != before
    }

    /// Splits `[gpa, gpa + len)` into (window, offset, length) pieces, or
    /// `None` if any byte is not covered by a mapped window.
    fn pieces(&self, gpa: u64, len: usize) -> Option<Vec<(&GuestWindow, usize, usize)>> {
        let mut pieces = Vec::new();
        let (mut at, mut left) = (gpa, u64::try_from(len).ok()?);
        while left > 0 {
            let window = self
                .windows
                .iter()
                .find(|w| w.address <= at && at < w.end())?;
            let mapping = window.mapping.as_ref()?;
            let offset = at - window.address;
            let take = left.min(window.end() - at);
            pieces.push((
                mapping,
                usize::try_from(offset).ok()?,
                usize::try_from(take).ok()?,
            ));
            at = at.checked_add(take)?;
            left -= take;
        }
        Some(pieces)
    }

    /// Copies guest memory into `out`; false (with `out` unspecified) if
    /// any byte is not mapped.
    pub fn read(&self, gpa: u64, out: &mut [u8]) -> bool {
        let Some(pieces) = self.pieces(gpa, out.len()) else {
            return false;
        };
        let mut done = 0;
        for (mapping, offset, len) in pieces {
            if !mapping.read(offset, &mut out[done..done + len]) {
                return false;
            }
            done += len;
        }
        true
    }

    /// Copies `data` into guest memory; false if any byte is not mapped (in
    /// which case nothing has been written).
    pub fn write(&self, gpa: u64, data: &[u8]) -> bool {
        let Some(pieces) = self.pieces(gpa, data.len()) else {
            return false;
        };
        let mut done = 0;
        for (mapping, offset, len) in pieces {
            if !mapping.write(offset, &data[done..done + len]) {
                return false;
            }
            done += len;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::SharedMem;

    fn sealed(len: usize) -> (SharedMem, OwnedFd) {
        let mem = SharedMem::new("guest-test", len).unwrap();
        let fd = mem.fd().try_clone_to_owned().unwrap();
        (mem, fd)
    }

    #[test]
    fn accesses_span_adjacent_windows() {
        let (low, low_fd) = sealed(4096);
        let (high, high_fd) = sealed(4096);
        let mut memory = GuestMemory::default();
        memory.add(0x1000, 0x1000, Some(low_fd), 0, true).unwrap();
        memory.add(0x2000, 0x1000, Some(high_fd), 0, true).unwrap();
        assert!(memory.write(0x1ffe, &[1, 2, 3, 4]));
        let (mut a, mut b) = ([0u8; 2], [0u8; 2]);
        low.read(4094, &mut a);
        high.read(0, &mut b);
        assert_eq!((a, b), ([1, 2], [3, 4]));
        let mut out = [0u8; 4];
        assert!(memory.read(0x1ffe, &mut out));
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    fn holes_overlaps_and_unmapped_windows() {
        let (_mem, fd) = sealed(4096);
        let mut memory = GuestMemory::default();
        memory.add(0x1000, 0x1000, Some(fd), 0, true).unwrap();
        memory.add(0x4000, 0x1000, None, 0, true).unwrap();
        assert_eq!(
            memory.add(0x1800, 0x1000, None, 0, true),
            Err(Errno::EEXIST)
        );
        assert_eq!(memory.add(u64::MAX, 2, None, 0, true), Err(Errno::EINVAL));
        assert!(!memory.read(0x1fff, &mut [0; 2]), "runs into a hole");
        assert!(!memory.read(0x4000, &mut [0; 1]), "window without a file");
        assert!(!memory.remove(0x4000, 0x800));
        assert!(memory.remove(0x4000, 0x1000));
    }
}
