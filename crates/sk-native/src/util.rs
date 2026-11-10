//! Small shared helpers.

#![allow(dead_code)] // each feature uses a part

use std::hash::{BuildHasherDefault, Hasher};

/// A fast, non-cryptographic hasher (the Fx hash of rustc) for keys that are not chosen by an
/// adversary: `(dev, ino)` pairs and file extensions. SipHash costs several times more per key.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf));
        }
    }

    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.add(v as u64);
    }

    #[inline]
    fn write_u16(&mut self, v: u16) {
        self.add(v as u64);
    }

    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.add(v as u64);
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }

    #[inline]
    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuild>;
pub type FxHashSet<K> = std::collections::HashSet<K, FxBuild>;

/// `ext.lower()` as Python computes it on a name decoded with surrogatepass (Windows) or
/// surrogateescape (POSIX): bytes that are not UTF-8 (lone surrogates, foreign encodings) stay.
pub fn lower_ext(raw: &[u8]) -> Vec<u8> {
    if raw.is_ascii() {
        return raw.to_ascii_lowercase();
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_lowercase().into_bytes(),
        Err(_) => {
            let mut out = Vec::with_capacity(raw.len());
            for chunk in raw.utf8_chunks() {
                out.extend_from_slice(chunk.valid().to_lowercase().as_bytes());
                out.extend_from_slice(chunk.invalid());
            }
            out
        }
    }
}
