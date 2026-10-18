"""The names of a node store: ``\\0name`` runs in fixed-size chunks that never move.

A name lives at a global offset (just past its NUL). Names are appended in node-id order, so the
names of consecutive nodes are adjacent — ``\\0a\\0b\\0c`` — inside a chunk, which is exactly what
the name index searches. Chunks are never resized or rewritten, so the index holds zero-copy views
into them while a scan keeps appending. A chunk's unused tail is the only waste; chunks double
from ``FIRST_CHUNK`` up to ``CHUNK``, so a small scan reserves little.

Chunks come from an allocator: plain memory, or file-backed memory in the memory-mapped mode
(:mod:`spaceknife.store.mapped`).
"""

from __future__ import annotations

import bisect
from collections.abc import Callable, Iterator

import numpy as np

CHUNK = 8 << 20
FIRST_CHUNK = 64 << 10

Allocator = Callable[[int], np.ndarray]  # zero-filled, writable uint8 array of a size


def _ram(size: int) -> np.ndarray:
    return np.zeros(size, dtype=np.uint8)


class NameHeap:
    def __init__(self, alloc: Allocator | None = None) -> None:
        self.alloc: Allocator = alloc or _ram
        self._chunks: list[np.ndarray] = []
        self._bases: list[int] = []  # global offset of each chunk's first byte
        self._used: list[int] = []  # bytes written into each chunk

    @classmethod
    def frozen(cls, image: bytes | memoryview | np.ndarray, alloc: Allocator | None = None) -> NameHeap:
        """A heap over an existing ``\\0name`` image (a loaded snapshot), without copying it."""
        heap = cls(alloc)
        arr = image if isinstance(image, np.ndarray) else np.frombuffer(image, dtype=np.uint8)
        if len(arr):
            heap._chunks.append(arr)
            heap._bases.append(0)
            heap._used.append(len(arr))  # full: later names go to new chunks
        return heap

    # ------------------------------------------------------------------ size

    @property
    def end(self) -> int:
        """One past the highest global offset in use."""
        return self._bases[-1] + self._used[-1] if self._chunks else 0

    def used(self) -> int:
        return sum(self._used)

    def reserved(self) -> int:
        return sum(len(c) for c in self._chunks)

    def reserved_in_ram(self) -> int:
        """Chunk bytes in plain memory (file-backed chunks are not counted)."""
        from .mapped import is_mapped

        return sum(len(c) for c in self._chunks if not is_mapped(c))

    # ------------------------------------------------------------------ writing

    def _room(self, need: int) -> tuple[np.ndarray, int, int]:
        """The chunk to write ``need`` bytes into: (chunk, its base, write position)."""
        if (
            self._chunks
            and self._used[-1] + need <= len(self._chunks[-1])
            and self._chunks[-1].flags.writeable
        ):
            return self._chunks[-1], self._bases[-1], self._used[-1]
        base = self._bases[-1] + len(self._chunks[-1]) if self._chunks else 0
        # chunks double from FIRST_CHUNK up to CHUNK: a small scan does not reserve megabytes
        size = min(CHUNK, max(FIRST_CHUNK, 2 * len(self._chunks[-1]))) if self._chunks else FIRST_CHUNK
        chunk = self.alloc(max(size, need))
        self._chunks.append(chunk)
        self._bases.append(base)
        self._used.append(0)
        return chunk, base, 0

    def append(self, blob: bytes | memoryview, lens: np.ndarray) -> np.ndarray:
        """Append names given as one concatenated blob and their lengths; their offsets (int64)."""
        lens = np.asarray(lens, dtype=np.int64)
        count = len(lens)
        if count == 0:
            return np.empty(0, dtype=np.int64)
        need = len(blob) + count
        chunk, base, pos = self._room(need)
        rel = np.cumsum(lens + 1) - lens  # each name's start inside the run (after its NUL)
        out = chunk[pos : pos + need]
        out[rel - 1] = 0
        keep = np.ones(need, dtype=bool)
        keep[rel - 1] = False
        out[keep] = np.frombuffer(blob, dtype=np.uint8)
        self._used[-1] = pos + need
        starts: np.ndarray = rel + (base + pos)
        return starts

    def append_one(self, name: bytes) -> int:
        chunk, base, pos = self._room(len(name) + 1)
        chunk[pos] = 0
        chunk[pos + 1 : pos + 1 + len(name)] = np.frombuffer(name, dtype=np.uint8)
        self._used[-1] = pos + 1 + len(name)
        return base + pos + 1

    # ------------------------------------------------------------------ reading

    def _chunk_of(self, off: int) -> int:
        return bisect.bisect_right(self._bases, off) - 1

    def get(self, off: int, length: int) -> bytes:
        k = self._chunk_of(off)
        start = off - self._bases[k]
        return self._chunks[k][start : start + length].tobytes()

    def chunk_end(self, off: int) -> int:
        """Global offset just past the written part of the chunk holding ``off``."""
        k = self._chunk_of(off)
        return self._bases[k] + self._used[k]

    def view(self, start: int, stop: int) -> np.ndarray:
        """Read-only zero-copy view of ``[start, stop)``, which must lie in one chunk."""
        k = self._chunk_of(start)
        base = self._bases[k]
        if stop > base + self._used[k]:
            raise ValueError("a heap view cannot span two chunks")
        out = self._chunks[k][start - base : stop - base]
        out.flags.writeable = False
        return out

    # ------------------------------------------------------------------ export

    def parts(self) -> Iterator[np.ndarray]:
        """The written part of every chunk, in order: the image is their concatenation."""
        for chunk, used in zip(self._chunks, self._used, strict=True):
            if used:
                yield chunk[:used]

    def image(self) -> bytes:
        """The heap without gaps: one ``\\0name`` run per node in id order."""
        return b"".join(p.tobytes() for p in self.parts())

    def rebase(self, off: np.ndarray) -> np.ndarray:
        """Offsets of :meth:`image` for global offsets (the gaps between chunks removed)."""
        if len(self._chunks) <= 1:
            return off
        bases = np.asarray(self._bases, dtype=np.int64)
        packed = np.concatenate(([0], np.cumsum(self._used[:-1], dtype=np.int64)))
        k = np.searchsorted(bases, off, side="right") - 1
        return np.asarray(off, dtype=np.int64) - bases[k] + packed[k]


__all__ = ["CHUNK", "Allocator", "NameHeap"]
