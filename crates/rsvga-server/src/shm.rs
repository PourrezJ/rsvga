//! Memory shared with the VMM (and therefore with the guest).
//!
//! This is the only module of the server allowed to use `unsafe`. The
//! mapping is accessed exclusively through volatile, bounds-checked loads and
//! stores: no Rust reference into it is ever created, because the guest may
//! change any byte at any time.

#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

/// A shared, fixed-size memfd mapped read-write in this process.
pub struct SharedMem {
    fd: OwnedFd,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is plain shared memory accessed only through volatile
// operations; nothing in it is tied to a thread.
unsafe impl Send for SharedMem {}
// SAFETY: as above; concurrent volatile accesses are what shared device
// memory requires anyway.
unsafe impl Sync for SharedMem {}

impl SharedMem {
    /// Creates `len` bytes of zeroed shared memory. The size is sealed, so no
    /// other process can shrink the file and make our mapping fault.
    pub fn new(name: &str, len: usize) -> io::Result<Self> {
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty shared memory",
            ));
        }
        let fd = memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
        ftruncate(&fd, len as u64)?;
        fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)?;
        // SAFETY: we map a file we just created with exactly `len` bytes and
        // sealed its size; the kernel picks the address.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        };
        let ptr = NonNull::new(ptr.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned a null pointer"))?;
        Ok(Self { fd, ptr, len })
    }

    /// The file to hand to the VMM for mapping.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
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

impl Drop for SharedMem {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` describe the mapping created in `new`, and no
        // pointer into it outlives `self`.
        let _ = unsafe { munmap(self.ptr.as_ptr().cast(), self.len) };
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
    }
}
