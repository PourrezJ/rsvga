//! Memory shared with the VMM (and therefore with the guest).
//!
//! This is the only module of the server allowed to use `unsafe`. Mappings
//! are accessed exclusively through volatile, bounds-checked loads and
//! stores: no Rust reference into them is ever created, because the guest may
//! change any byte at any time.
//!
//! Two kinds of mappings exist:
//! - [`SharedMem`]: device memory we create (VRAM, FIFO) and hand to QEMU;
//! - [`GuestWindow`]: guest RAM that QEMU hands to us (`DMA_MAP`).
//!
//! Both are backed by files sealed against shrinking: if the other side could
//! truncate the file, touching the mapping would kill this process (SIGBUS).

#![allow(unsafe_code)]

use std::io;
use std::ops::Deref;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use rustix::fs::{
    MemfdFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, fstat, ftruncate, memfd_create,
};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

/// A mapped range, accessed only through volatile, bounds-checked operations.
pub struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is plain shared memory accessed only through volatile
// operations; nothing in it is tied to a thread.
unsafe impl Send for Mapping {}
// SAFETY: as above; concurrent volatile accesses are what shared device
// memory requires anyway.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Maps `len` bytes of `fd` at `offset`, shared.
    ///
    /// # Safety
    /// `fd` must hold at least `offset + len` bytes and be unable to shrink
    /// for the lifetime of the mapping.
    unsafe fn new(fd: BorrowedFd<'_>, offset: u64, len: usize, writable: bool) -> io::Result<Self> {
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty mapping"));
        }
        let prot = if writable {
            ProtFlags::READ | ProtFlags::WRITE
        } else {
            ProtFlags::READ
        };
        // SAFETY: the caller guarantees the file covers the range and cannot
        // shrink; the kernel picks the address.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                prot,
                MapFlags::SHARED,
                fd,
                offset,
            )?
        };
        let ptr = NonNull::new(ptr.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned a null pointer"))?;
        Ok(Self { ptr, len })
    }

    /// Size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether `[offset, offset + size)` lies inside the mapping.
    fn contains(&self, offset: usize, size: usize) -> bool {
        offset.checked_add(size).is_some_and(|end| end <= self.len)
    }

    /// Loads the 32-bit word at word index `word`; 0 if out of bounds.
    pub fn load_u32(&self, word: usize) -> u32 {
        let Some(offset) = word.checked_mul(4) else {
            return 0;
        };
        if !self.contains(offset, 4) {
            return 0;
        }
        // SAFETY: in bounds (checked above) and 4-aligned, because the
        // mapping is page-aligned and `offset` is a multiple of 4.
        unsafe { self.ptr.as_ptr().add(offset).cast::<u32>().read_volatile() }
    }

    /// Stores the 32-bit word at word index `word`; ignored if out of bounds.
    pub fn store_u32(&self, word: usize, value: u32) {
        let Some(offset) = word.checked_mul(4) else {
            return;
        };
        if !self.contains(offset, 4) {
            return;
        }
        // SAFETY: in bounds and aligned, as in `load_u32`.
        unsafe {
            self.ptr
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value)
        }
    }

    /// Copies bytes out of the mapping. Returns `false` (copying nothing) if
    /// the range is out of bounds.
    pub fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        if !self.contains(offset, out.len()) {
            return false;
        }
        for (i, byte) in out.iter_mut().enumerate() {
            // SAFETY: `offset + i < offset + out.len() <= len`.
            *byte = unsafe { self.ptr.as_ptr().add(offset + i).read_volatile() };
        }
        true
    }

    /// Copies bytes into the mapping. Returns `false` (copying nothing) if the
    /// range is out of bounds.
    pub fn write(&self, offset: usize, data: &[u8]) -> bool {
        if !self.contains(offset, data.len()) {
            return false;
        }
        for (i, byte) in data.iter().enumerate() {
            // SAFETY: `offset + i < offset + data.len() <= len`.
            unsafe { self.ptr.as_ptr().add(offset + i).write_volatile(*byte) };
        }
        true
    }

    /// `memmove` inside the mapping. Returns `false` (copying nothing) if
    /// either range is out of bounds.
    pub fn copy_within(&self, src: usize, dst: usize, len: usize) -> bool {
        if !self.contains(src, len) || !self.contains(dst, len) {
            return false;
        }
        let base = self.ptr.as_ptr();
        let copy = |i: usize| {
            // SAFETY: `i < len`, and both ranges were checked above.
            unsafe {
                base.add(dst + i)
                    .write_volatile(base.add(src + i).read_volatile())
            }
        };
        if dst <= src {
            (0..len).for_each(copy);
        } else {
            (0..len).rev().for_each(copy);
        }
        true
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` describe a mapping created in `new`, and no
        // pointer into it outlives `self`.
        let _ = unsafe { munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

/// Device memory we allocate and share with QEMU: a fixed-size memfd.
pub struct SharedMem {
    fd: OwnedFd,
    map: Mapping,
}

impl SharedMem {
    /// Creates `len` bytes of zeroed shared memory, sealed so that no other
    /// process can shrink (or grow) the file.
    pub fn new(name: &str, len: usize) -> io::Result<Self> {
        let fd = memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
        ftruncate(&fd, len as u64)?;
        fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)?;
        // SAFETY: the file has exactly `len` bytes and its size is sealed.
        let map = unsafe { Mapping::new(fd.as_fd(), 0, len, true)? };
        Ok(Self { fd, map })
    }

    /// The file to hand to the VMM for mapping.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Deref for SharedMem {
    type Target = Mapping;
    fn deref(&self) -> &Mapping {
        &self.map
    }
}

/// Guest RAM shared by QEMU through `DMA_MAP`.
pub struct GuestWindow {
    map: Mapping,
}

impl GuestWindow {
    /// Maps `len` bytes at `offset` of a file received from QEMU. Refused
    /// unless the file is sealed against shrinking and covers the range:
    /// QEMU's memfd RAM backends are sealed by default (`seal=on`).
    pub fn map(fd: &OwnedFd, offset: u64, len: u64, writable: bool) -> io::Result<Self> {
        let seals = fcntl_get_seals(fd)
            .map_err(|_| io::Error::other("guest memory is not a sealable memfd"))?;
        if !seals.contains(SealFlags::SHRINK) {
            return Err(io::Error::other(
                "guest memory is not sealed against shrinking",
            ));
        }
        let size = u64::try_from(fstat(fd)?.st_size).unwrap_or(0);
        let end = offset
            .checked_add(len)
            .ok_or_else(|| io::Error::other("window overflows"))?;
        if end > size {
            return Err(io::Error::other("window is larger than its file"));
        }
        let len = usize::try_from(len).map_err(io::Error::other)?;
        // SAFETY: the file covers [offset, offset + len) and cannot shrink.
        let map = unsafe { Mapping::new(fd.as_fd(), offset, len, writable)? };
        Ok(Self { map })
    }
}

impl Deref for GuestWindow {
    type Target = Mapping;
    fn deref(&self) -> &Mapping {
        &self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_enforced() {
        let mem = SharedMem::new("test", 4096).unwrap();
        mem.store_u32(1023, 7);
        assert_eq!(mem.load_u32(1023), 7);
        assert_eq!(mem.load_u32(1024), 0);
        mem.store_u32(usize::MAX, 1);
        assert!(!mem.write(4095, &[1, 2]));
        assert!(!mem.read(usize::MAX, &mut [0]));
        assert!(!mem.copy_within(0, 4000, 200));
    }

    #[test]
    fn overlapping_copies_behave_like_memmove() {
        let mem = SharedMem::new("test", 4096).unwrap();
        mem.write(0, &[1, 2, 3, 4, 5]);
        assert!(mem.copy_within(0, 2, 5));
        let mut out = [0u8; 7];
        mem.read(0, &mut out);
        assert_eq!(out, [1, 2, 1, 2, 3, 4, 5]);
        assert!(mem.copy_within(2, 0, 5));
        mem.read(0, &mut out);
        assert_eq!(out, [1, 2, 3, 4, 5, 4, 5]);
    }

    #[test]
    fn the_size_is_sealed() {
        let mem = SharedMem::new("test", 4096).unwrap();
        assert!(ftruncate(mem.fd(), 0).is_err());
        assert!(SharedMem::new("test", 0).is_err());
    }

    #[test]
    fn guest_windows_must_be_sealed_and_in_bounds() {
        let shared = SharedMem::new("test", 8192).unwrap();
        let fd = shared.fd().try_clone_to_owned().unwrap();
        shared.write(4096, b"guest");
        let window = GuestWindow::map(&fd, 4096, 4096, false).unwrap();
        let mut out = [0u8; 5];
        assert!(window.read(0, &mut out));
        assert_eq!(&out, b"guest");
        assert!(
            GuestWindow::map(&fd, 4096, 8192, false).is_err(),
            "past the end"
        );
        assert!(GuestWindow::map(&fd, u64::MAX - 10, 4096, false).is_err());

        let unsealed = memfd_create("unsealed", MemfdFlags::CLOEXEC).unwrap();
        ftruncate(&unsealed, 4096).unwrap();
        assert!(GuestWindow::map(&unsealed, 0, 4096, false).is_err());
    }
}
