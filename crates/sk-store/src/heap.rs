//! The names of a node store: `\0name` runs in fixed-size chunks that never move.
//!
//! A name lives at a global offset (just past its NUL). Names are appended in node-id order, so
//! the names of consecutive nodes are adjacent — `\0a\0b\0c` — inside a chunk, which is exactly
//! what a name index searches. Chunks are never resized or rewritten, so readers hold zero-copy
//! slices into them while a scan keeps appending past the published names. A chunk's unused tail
//! is the only waste; chunks double from `first_chunk` (64 KiB) up to `max_chunk` (8 MiB). Global
//! offsets count the unused tails too: chunk `k + 1` starts at `base[k] + cap[k]`.

use std::sync::Arc;

use crate::mapped::Buf;

pub const CHUNK: usize = 8 << 20;
pub const FIRST_CHUNK: usize = 64 << 10;

/// One chunk: a buffer and its global offset.
#[derive(Clone)]
pub(crate) struct Chunk {
    pub(crate) buf: Arc<Buf>,
    pub(crate) base: u64,
}

impl Chunk {
    #[inline]
    pub(crate) fn cap(&self) -> usize {
        self.buf.bytes()
    }
    #[inline]
    pub(crate) fn end(&self) -> u64 {
        self.base + self.buf.bytes() as u64
    }
}

/// Index of the chunk holding global offset `off`.
#[inline]
pub(crate) fn chunk_of(chunks: &[Chunk], off: u64) -> usize {
    chunks.partition_point(|c| c.base <= off).saturating_sub(1)
}

/// The writing side of a heap (owned by the store's append state).
pub(crate) struct HeapWriter {
    pub(crate) chunks: Vec<Chunk>,
    /// bytes written into the last chunk
    pub(crate) used: usize,
    pub(crate) first_chunk: usize,
    pub(crate) max_chunk: usize,
}

impl HeapWriter {
    pub(crate) fn new(first_chunk: usize, max_chunk: usize) -> Self {
        HeapWriter {
            chunks: Vec::new(),
            used: 0,
            first_chunk,
            max_chunk,
        }
    }

    /// A heap over an existing image (a loaded snapshot, a compacted store): one full chunk, so
    /// later names go to new chunks.
    pub(crate) fn frozen(image: Buf, first_chunk: usize, max_chunk: usize) -> Self {
        let mut heap = HeapWriter::new(first_chunk, max_chunk);
        if image.bytes() > 0 {
            heap.used = image.bytes();
            heap.chunks.push(Chunk {
                buf: Arc::new(image),
                base: 0,
            });
        }
        heap
    }

    /// One past the highest global offset in use.
    pub(crate) fn end(&self) -> u64 {
        self.chunks.last().map_or(0, |c| c.base + self.used as u64)
    }

    /// Bytes reserved by the chunks.
    pub(crate) fn reserved(&self) -> usize {
        self.chunks.iter().map(|c| c.cap()).sum()
    }

    pub(crate) fn reserved_in_ram(&self) -> usize {
        self.chunks.iter().map(|c| c.buf.ram()).sum()
    }

    /// Room for `need` bytes: a new chunk when the last one is full. Returns whether a chunk was
    /// added (the chunk list must be published again).
    pub(crate) fn room(&mut self, need: usize, alloc: &mut dyn FnMut(usize) -> Buf) -> bool {
        if let Some(last) = self.chunks.last()
            && self.used + need <= last.cap()
        {
            return false;
        }
        let (base, size) = match self.chunks.last() {
            Some(last) => (
                last.end(),
                (2 * last.cap()).clamp(self.first_chunk, self.max_chunk),
            ),
            None => (0, self.first_chunk),
        };
        let buf = alloc(size.max(need));
        self.chunks.push(Chunk {
            buf: Arc::new(buf),
            base,
        });
        self.used = 0;
        true
    }

    /// Writes `\0name` for every name of `blob` (lengths `lens`) into the last chunk, which
    /// [`HeapWriter::room`] made large enough; returns the global offset of the first name.
    ///
    /// # Safety
    /// Nobody may read the bytes past `used` of the last chunk (they are unpublished).
    pub(crate) unsafe fn write(&mut self, blob: &[u8], lens: &[u16]) -> u64 {
        let chunk = self.chunks.last().expect("room() first");
        let need = blob.len() + lens.len();
        assert!(self.used + need <= chunk.cap());
        let first = chunk.base + self.used as u64 + 1;
        // SAFETY: within the chunk (asserted), past every published byte (caller's contract).
        unsafe {
            let mut dst = chunk.buf.ptr().add(self.used);
            let mut src = blob.as_ptr();
            for &l in lens {
                dst.write(0);
                std::ptr::copy_nonoverlapping(src, dst.add(1), l as usize);
                dst = dst.add(1 + l as usize);
                src = src.add(l as usize);
            }
        }
        self.used += need;
        first
    }
}
