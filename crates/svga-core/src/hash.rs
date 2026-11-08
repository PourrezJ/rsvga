//! A fast hasher for the device's own tables (surfaces, views, pipelines,
//! shader variants), looked up many times per draw: the multiply-rotate
//! hash of the Rust compiler (`rustc-hash`), here in a few lines rather
//! than a dependency. Unlike the standard SipHash it does not resist
//! chosen collisions; keys come from the guest, so a guest could at worst
//! slow down its own device, which it can do anyway with more work.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// A [`HashMap`] with [`FastHasher`].
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;
/// A [`HashSet`] with [`FastHasher`].
pub type FastSet<K> = HashSet<K, BuildHasherDefault<FastHasher>>;

/// Multiply-rotate hashing of 64-bit words.
#[derive(Clone, Copy, Debug, Default)]
pub struct FastHasher {
    hash: u64,
}

/// The multiplier of `rustc-hash` 2.
const K: u64 = 0xf135_7aea_2e62_a9c5;

impl FastHasher {
    fn add(&mut self, word: u64) {
        self.hash = self.hash.wrapping_add(word).wrapping_mul(K);
    }
}

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            let mut w = [0u8; 8];
            w.copy_from_slice(c);
            self.add(u64::from_le_bytes(w));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut w = [0u8; 8];
            w[..rest.len()].copy_from_slice(rest);
            // The length keeps "ab" and "ab\0" apart.
            self.add(u64::from_le_bytes(w) ^ ((rest.len() as u64) << 56));
        }
    }

    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }

    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    fn finish(&self) -> u64 {
        // The high bits are the best mixed: move them to where hash
        // tables take their bucket index.
        self.hash.rotate_left(26)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_work_and_spread_small_keys() {
        let mut m: FastMap<u32, u32> = FastMap::default();
        for i in 0..10_000 {
            m.insert(i, i.wrapping_mul(3));
        }
        assert_eq!(m.get(&4321), Some(&12963));
        // Consecutive ids land in different low bits (bucket indexes).
        let low: FastSet<u64> = (0u32..256)
            .map(|i| {
                let mut h = FastHasher::default();
                h.write_u32(i);
                h.finish() & 0xff
            })
            .collect();
        assert!(low.len() > 128, "{} distinct buckets of 256", low.len());
    }

    #[test]
    fn byte_strings_of_different_lengths_differ() {
        let hash = |b: &[u8]| {
            let mut h = FastHasher::default();
            h.write(b);
            h.finish()
        };
        assert_ne!(hash(b"ab"), hash(b"ab\0"));
        assert_ne!(hash(b"abcdefgh"), hash(b"abcdefgh\0"));
        assert_eq!(hash(b"same"), hash(b"same"));
    }
}
