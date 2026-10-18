"""Columnar node store: the scanned tree as numpy columns plus a heap of UTF-8 names.

Layout invariants (relied upon everywhere):

* Node 0 is a virtual top node; scan roots are its children (depth 1).
* A directory's children are appended together, so they occupy the contiguous id range
  ``[child_start[d], child_start[d] + child_count[d])``.
* Every node's id is greater than its parent's id, so iterating ids in reverse order is a
  valid post-order (used for full re-aggregation).

* Names are appended in id order, each as ``\\0name`` (:mod:`.heap`): consecutive nodes' names
  are adjacent, so the name index searches them in place.

Per-node cost is 89 bytes of columns plus the name and its NUL (docs/memory.md). Columns live in
plain memory, or in self-deleting mapped files once the open scans exceed the memory budget
(:mod:`.mapped`). Nothing here allocates a Python object per node; queries materialise only what
they return.
"""

from __future__ import annotations

import os
import sys
from collections.abc import Iterator
from dataclasses import dataclass
from enum import IntEnum, IntFlag

import numpy as np

from . import mapped
from .heap import NameHeap

NAME_ERRORS = "surrogatepass" if sys.platform == "win32" else "surrogateescape"


class Kind(IntEnum):
    FILE = 0
    DIR = 1
    LINK = 2  # symlink / junction / mount-point reparse: shown, never descended
    OTHER = 3  # device, fifo, socket


class Flag(IntFlag):
    NONE = 0
    ACCESS_DENIED = 1 << 0  # directory could not be listed
    MOUNT = 1 << 1  # another filesystem mounted here; not crossed
    HARDLINK_DUP = 1 << 2  # additional link of an already counted file: contributes 0
    COMPRESSED = 1 << 3
    SPARSE = 1 << 4
    HIDDEN = 1 << 5
    SYSTEM = 1 << 6
    READONLY = 1 << 7
    REPARSE = 1 << 8
    CLOUD = 1 << 9  # cloud placeholder (OneDrive/iCloud): not stored locally
    ERROR = 1 << 10  # could not stat this entry
    PENDING = 1 << 11  # directory not listed yet (scan in progress)
    ARCHIVE = 1 << 12
    ENCRYPTED = 1 << 13
    TEMPORARY = 1 << 14
    NOT_INDEXED = 1 << 15
    OFFLINE = 1 << 16
    VIRTUAL = 1 << 17  # node 0
    ADS = 1 << 18  # has alternate data streams (reserved: not collected yet)


# column name -> dtype. Times are unix seconds (uint32: 1970–2106, 0 = unknown).
COLUMNS: dict[str, np.dtype[np.generic]] = {
    "parent": np.dtype("<i4"),
    "depth": np.dtype("<u2"),
    "kind": np.dtype("<u1"),
    "flags": np.dtype("<u4"),
    "name_off": np.dtype("<u4"),  # widened to <i8 once the names pass 4 GiB
    "name_len": np.dtype("<u2"),
    "ext": np.dtype("<u2"),
    "size": np.dtype("<i8"),
    "alloc": np.dtype("<i8"),
    "mtime": np.dtype("<u4"),
    "ctime": np.dtype("<u4"),
    "atime": np.dtype("<u4"),
    "ino": np.dtype("<u8"),
    "nlink": np.dtype("<u2"),
    "child_start": np.dtype("<i4"),
    "child_count": np.dtype("<i4"),
    "tot_size": np.dtype("<i8"),
    "tot_alloc": np.dtype("<i8"),
    "tot_files": np.dtype("<i4"),
    "tot_dirs": np.dtype("<i4"),
}

ROW_BYTES = sum(dt.itemsize for dt in COLUMNS.values())
_OFF_WIDE = np.dtype("<i8")
OFF_NARROW_LIMIT = 2**32  # name offsets fit uint32 below this many bytes of names

# Columns a scanner provides for new entries (the rest are derived by the store).
ENTRY_COLUMNS = ("kind", "flags", "size", "alloc", "mtime", "ctime", "atime", "ino", "nlink")


@dataclass(frozen=True, slots=True)
class NodeView:
    """A materialised node, for queries and tests."""

    id: int
    name: str
    kind: Kind
    flags: Flag
    parent: int
    depth: int
    size: int
    alloc: int
    tot_size: int
    tot_alloc: int
    tot_files: int
    tot_dirs: int
    mtime: int
    ctime: int
    atime: int
    child_count: int
    ext: str


class NodeStore:
    def __init__(self, capacity: int = 1 << 14) -> None:
        self._n = 0
        self._cap = max(16, capacity)
        self.mapped = False  # columns and new name chunks in file-backed memory (see .mapped)
        self._heap = NameHeap()
        self.cols: dict[str, np.ndarray] = {k: self._zeros(self._cap, dt) for k, dt in COLUMNS.items()}
        mapped.POLICY.register(self)
        self.version = 0  # bumped by every change made through this API (caches key on it)
        self.ext_table: list[str] = [""]
        self._ext_index: dict[str, int] = {"": 0}
        self._append_one(parent=-1, depth=0, kind=Kind.DIR, flags=Flag.VIRTUAL, name=b"")

    # ------------------------------------------------------------------ basics

    def __len__(self) -> int:
        return self._n

    @property
    def names_blob(self) -> bytes:
        """The names as one ``\\0name`` run per node in id order; pairs with :meth:`export_columns`."""
        return self._heap.image()

    def names_parts(self) -> list[np.ndarray]:
        """:attr:`names_blob` in pieces, without joining them (snapshot writing)."""
        return list(self._heap.parts())

    def names_run(self, a: int, b: int) -> tuple[int, np.ndarray, np.ndarray]:
        """Zero-copy view of the names of nodes ``a`` up to ``b`` (or fewer: one heap chunk).

        Returns ``(stop, run, sep)``: ``run`` is ``\\0name\\0name…`` for nodes ``a`` to ``stop - 1``,
        ``sep`` the position of each name's NUL in it. Safe while a scan appends (chunks never move).
        """
        c = self.cols
        off = c["name_off"][a:b].astype(np.int64)
        ln = c["name_len"][a:b].astype(np.int64)
        cut = int(np.searchsorted(off, self._heap.chunk_end(int(off[0])), side="left"))
        off, ln = off[:cut], ln[:cut]
        start, stop = int(off[0]) - 1, int(off[-1] + ln[-1])
        sep = off - 1 - start
        return (
            a + cut,
            self._heap.view(start, stop),
            sep.astype(np.int32 if stop - start < 2**31 else np.int64),
        )

    def _zeros(self, count: int, dtype: np.dtype[np.generic]) -> np.ndarray:
        if self.mapped:
            try:
                return mapped.mapped_zeros(count, dtype)
            except OSError:  # no room for the file: plain memory still works
                self._unmap()
        return np.zeros(count, dtype=dtype)

    def _map(self) -> None:
        """From now on, new columns and name chunks go to file-backed memory."""
        self.mapped = True
        self._heap.alloc = mapped.mapped_zeros

    def _unmap(self) -> None:
        self.mapped = False
        self._heap.alloc = NameHeap().alloc

    def ram_bytes(self) -> int:
        """Bytes held in plain memory: columns (with headroom) and name chunks."""
        total = sum(a.nbytes for a in self.cols.values() if not mapped.is_mapped(a))
        return total + self._heap.reserved_in_ram()

    def col(self, name: str) -> np.ndarray:
        """Read-only view of a column trimmed to the live length."""
        view = self.cols[name][: self._n]
        view.flags.writeable = False
        return view

    def _grow(self, need: int) -> None:
        if self._n + need <= self._cap:
            return
        cap = self._cap
        while self._n + need > cap:
            cap *= 2
        if not self.mapped and mapped.POLICY.should_map(self, cap * ROW_BYTES + self._heap.reserved()):
            self._map()
        for key, arr in self.cols.items():
            new = self._zeros(cap, arr.dtype)
            new[: self._n] = arr[: self._n]
            self.cols[key] = new
        self._cap = cap

    def compact(self) -> int:
        """Give back the growth headroom (up to half of every column) once nothing is added any more.

        Copies one column at a time, so the extra memory is one column at most. Readers holding an
        old column keep a valid, equal copy. Returns the bytes freed.
        """
        n = max(16, self._n)
        if self._cap <= n:
            return 0
        freed = 0
        for key, arr in self.cols.items():
            freed += (len(arr) - n) * arr.itemsize
            new = self._zeros(n, arr.dtype)
            new[:] = arr[:n]
            self.cols[key] = new
        self._cap = n
        return freed

    def ext_id(self, ext: str) -> int:
        idx = self._ext_index.get(ext)
        if idx is None:
            if len(self.ext_table) >= 65535:
                return 0
            idx = len(self.ext_table)
            self.ext_table.append(ext)
            self._ext_index[ext] = idx
        return idx

    def _append_one(self, *, parent: int, depth: int, kind: Kind, flags: Flag, name: bytes) -> int:
        self._grow(1)
        i = self._n
        c = self.cols
        c["parent"][i] = parent
        c["depth"][i] = depth
        c["kind"][i] = kind
        c["flags"][i] = flags
        off = self._heap.append_one(name)
        self._fit_offsets(off + len(name))
        c = self.cols
        c["name_off"][i] = off
        c["name_len"][i] = len(name)
        self._n += 1
        self.version += 1
        return i

    # ------------------------------------------------------------------ building

    def add_root(self, path: str, *, kind: Kind = Kind.DIR, flags: Flag = Flag.PENDING) -> int:
        """Add a scan root (a child of node 0). Roots must be added before any listing is ingested."""
        if self.cols["child_count"][0] and self._n != 1 + int(self.cols["child_count"][0]):
            raise RuntimeError("roots must be added before scanning starts")
        root = self._append_one(parent=0, depth=1, kind=kind, flags=flags, name=encode_name(path))
        self.cols["child_start"][0] = 1
        self.cols["child_count"][0] = root
        if kind == Kind.DIR:
            self._bump_dirs(0, 1)
        return root

    def set_root_stats(self, root: int, *, mtime: int = 0, ctime: int = 0, atime: int = 0) -> None:
        self.cols["mtime"][root] = mtime
        self.cols["ctime"][root] = ctime
        self.cols["atime"][root] = atime

    def ingest_names(
        self,
        dir_id: int,
        names: list[bytes],
        entries: dict[str, np.ndarray],
        ext_ids: np.ndarray | None = None,
    ) -> int:
        """Convenience wrapper of :meth:`ingest_listing` taking a list of encoded names."""
        lens = np.fromiter((len(n) for n in names), dtype=np.int64, count=len(names))
        return self.ingest_listing(dir_id, b"".join(names), lens, entries, ext_ids)

    def ingest_listing(
        self,
        dir_id: int,
        name_blob: bytes,
        name_lens: np.ndarray,
        entries: dict[str, np.ndarray],
        ext_ids: np.ndarray | None = None,
    ) -> int:
        """Append one directory's entries as its children; returns the first new id.

        ``name_blob`` is the entries' UTF-8 names concatenated, ``name_lens`` their byte lengths.
        ``entries`` holds the :data:`ENTRY_COLUMNS` arrays. ``ext_ids`` index :attr:`ext_table`.
        """
        c = self.cols
        if c["kind"][dir_id] != Kind.DIR:
            raise ValueError(f"node {dir_id} is not a directory")
        if c["child_count"][dir_id]:
            raise ValueError(f"directory {dir_id} was already listed")
        count = len(name_lens)
        start = self._n
        c["flags"][dir_id] &= ~np.uint32(Flag.PENDING)
        self.version += 1
        if count == 0:
            c["child_start"][dir_id] = start
            return start
        self._grow(count)
        end = start + count
        sl = slice(start, end)
        c["parent"][sl] = dir_id
        c["depth"][sl] = int(c["depth"][dir_id]) + 1
        for key in ENTRY_COLUMNS:
            c[key][sl] = entries[key]
        c["ext"][sl] = ext_ids if ext_ids is not None else 0
        lengths = np.asarray(name_lens, dtype=np.int64)
        offsets = self._heap.append(name_blob, lengths)
        self._fit_offsets(int(offsets[-1] + lengths[-1]))
        c = self.cols
        c["name_off"][sl] = offsets
        c["name_len"][sl] = lengths
        kinds = c["kind"][sl]
        is_dir = kinds == Kind.DIR
        c["flags"][sl] = np.where(is_dir, c["flags"][sl] | np.uint32(Flag.PENDING), c["flags"][sl])
        # Publish order for readers in other threads (renders, summaries): the rows, then their
        # count of the store, then the range start, the range count last. A reader that sees a
        # non-zero count therefore sees the start too; see child_range() for the reading side.
        self._n = end
        c["child_start"][dir_id] = start
        c["child_count"][dir_id] = count

        # Directories' own allocation (POSIX directory blocks) counts toward the parent; logical size is 0.
        counted = (c["flags"][sl] & np.uint32(Flag.HARDLINK_DUP)) == 0
        size = int(c["size"][sl][counted].sum())
        alloc = int(c["alloc"][sl][counted].sum())
        files = int(np.count_nonzero(kinds == Kind.FILE))
        dirs = int(np.count_nonzero(is_dir))
        self._propagate(dir_id, size, alloc, files, dirs)
        return start

    def _propagate(self, node: int, size: int, alloc: int, files: int, dirs: int) -> None:
        c = self.cols
        parent = c["parent"]
        tot_size, tot_alloc, tot_files, tot_dirs = (
            c["tot_size"],
            c["tot_alloc"],
            c["tot_files"],
            c["tot_dirs"],
        )
        while node >= 0:
            tot_size[node] += size
            tot_alloc[node] += alloc
            tot_files[node] += files
            tot_dirs[node] += dirs
            node = int(parent[node])

    def _bump_dirs(self, node: int, dirs: int) -> None:
        self._propagate(node, 0, 0, 0, dirs)

    def mark(self, node: int, flags: Flag) -> None:
        self.cols["flags"][node] |= np.uint32(flags)
        self.version += 1

    def clear(self, node: int, flags: Flag) -> None:
        self.cols["flags"][node] &= ~np.uint32(flags)
        self.version += 1

    def reaggregate(self) -> None:
        """Recompute all subtree totals from scratch (vectorised, one pass per depth level)."""
        n = self._n
        c = self.cols
        kind = c["kind"][:n]
        flags = c["flags"][:n]
        counted = (flags & np.uint32(Flag.HARDLINK_DUP)) == 0
        # A node contributes its own bytes to its parent; a directory's totals cover its descendants
        # only (exactly what incremental ingest maintains).
        # Only the four results are node-sized; everything else is per level (the end of a big scan
        # is the memory peak, keep the temporaries small).
        size, alloc = c["size"][:n], c["alloc"][:n]
        sub_size = np.zeros(n, dtype=np.int64)
        sub_alloc = np.zeros(n, dtype=np.int64)
        sub_files = np.zeros(n, dtype=np.int32)
        sub_dirs = np.zeros(n, dtype=np.int32)
        depth = c["depth"][:n]
        parent = c["parent"][:n]
        for d in range(int(depth.max()) if n else 0, 0, -1):
            idx = np.nonzero(depth == d)[0]
            p = parent[idx]
            keep = counted[idx]
            k = kind[idx]
            np.add.at(sub_size, p, sub_size[idx] + np.where(keep, size[idx], 0))
            np.add.at(sub_alloc, p, sub_alloc[idx] + np.where(keep, alloc[idx], 0))
            np.add.at(sub_files, p, sub_files[idx] + (k == Kind.FILE))
            np.add.at(sub_dirs, p, sub_dirs[idx] + (k == Kind.DIR))
        self.version += 1
        c["tot_size"][:n] = sub_size
        c["tot_alloc"][:n] = sub_alloc
        c["tot_files"][:n] = sub_files
        c["tot_dirs"][:n] = sub_dirs

    # ------------------------------------------------------------------ queries

    def roots(self) -> range:
        return self.children(0)

    def children(self, node: int) -> range:
        return range(*self.child_range(node))

    def child_range(self, node: int) -> tuple[int, int]:
        """``(start, stop)`` of a folder's children; safe while a scan adds listings in another thread.

        The count is read before the start (ingest publishes them the other way round), and a range
        that does not lie after its folder and inside the store reads as empty (children always come
        after their folder): a folder being listed at this very moment is simply not listed yet.
        Reading the start first could pair a fresh count with the old start 0 — a range over the
        top of the tree, which a subtree walk would then revisit again and again.
        """
        c = self.cols
        count = int(c["child_count"][node])
        if count <= 0:
            return 0, 0
        start = int(c["child_start"][node])
        if start <= node or start + count > self._n:
            return 0, 0
        return start, start + count

    def _fit_offsets(self, end: int) -> None:
        """Widen ``name_off`` to 64 bits before an offset past 4 GiB is stored."""
        col = self.cols["name_off"]
        if end >= OFF_NARROW_LIMIT and col.dtype != _OFF_WIDE:
            wide = self._zeros(len(col), _OFF_WIDE)
            wide[: self._n] = col[: self._n]
            self.cols["name_off"] = wide

    def name_bytes(self, node: int) -> bytes:
        c = self.cols
        return self._heap.get(int(c["name_off"][node]), int(c["name_len"][node]))

    def name(self, node: int) -> str:
        return decode_name(self.name_bytes(node))

    def path(self, node: int) -> str:
        parts: list[str] = []
        parent = self.cols["parent"]
        while node > 0:
            parts.append(self.name(node))
            node = int(parent[node])
        if not parts:
            return ""
        parts.reverse()
        return os.path.join(*parts)

    def ancestors(self, node: int) -> Iterator[int]:
        parent = self.cols["parent"]
        node = int(parent[node])
        while node > 0:
            yield node
            node = int(parent[node])

    def sorted_children(self, node: int, by: str = "size", descending: bool = True) -> np.ndarray:
        ids = np.arange(*self._range_bounds(node), dtype=np.int64)
        if by == "size":
            key = self.cols["tot_alloc"][ids] + np.where(
                self.cols["kind"][ids] == Kind.DIR, 0, self.cols["alloc"][ids]
            )
            order = np.argsort(-key if descending else key, kind="stable")
            return ids[order]
        if by == "name":
            names = [self.name(int(i)).casefold() for i in ids]
            by_name = sorted(range(len(ids)), key=names.__getitem__, reverse=descending)
            return ids[np.asarray(by_name, dtype=np.int64)] if by_name else ids
        raise ValueError(f"unknown sort key {by!r}")

    def _range_bounds(self, node: int) -> tuple[int, int]:
        return self.child_range(node)

    def effective_size(self, node: int) -> tuple[int, int]:
        """(logical, allocated) bytes of a node: its subtree totals for dirs, own size otherwise."""
        c = self.cols
        if c["kind"][node] == Kind.DIR:
            return int(c["tot_size"][node]), int(c["tot_alloc"][node])
        if c["flags"][node] & Flag.HARDLINK_DUP:
            return 0, 0
        return int(c["size"][node]), int(c["alloc"][node])

    def view(self, node: int) -> NodeView:
        c = self.cols
        size, alloc = self.effective_size(node)
        return NodeView(
            id=node,
            name=self.name(node),
            kind=Kind(int(c["kind"][node])),
            flags=Flag(int(c["flags"][node])),
            parent=int(c["parent"][node]),
            depth=int(c["depth"][node]),
            size=int(c["size"][node]),
            alloc=int(c["alloc"][node]),
            tot_size=size,
            tot_alloc=alloc,
            tot_files=int(c["tot_files"][node]),
            tot_dirs=int(c["tot_dirs"][node]),
            mtime=int(c["mtime"][node]),
            ctime=int(c["ctime"][node]),
            atime=int(c["atime"][node]),
            child_count=int(c["child_count"][node]),
            ext=self.ext_table[int(c["ext"][node])],
        )

    def find(self, path: str) -> int | None:
        """Node id for an absolute path inside one of the roots, or None."""
        target = os.path.normcase(os.path.normpath(path))
        for root in self.roots():
            root_path = os.path.normcase(os.path.normpath(self.name(root)))
            if target == root_path:
                return root
            prefix = root_path if root_path.endswith(os.sep) else root_path + os.sep
            if not target.startswith(prefix):
                continue
            node = root
            for part in target[len(prefix) :].split(os.sep):
                match = next(
                    (ch for ch in self.children(node) if os.path.normcase(self.name(ch)) == part), None
                )
                if match is None:
                    return None
                node = match
            return node
        return None

    def adopt(self, other: NodeStore) -> None:
        """Take over the content of a store built aside (an MFT scan); ``other`` is not used again.

        Views and caches keep this object: the version moves past both stores', so everything keyed
        on it is rebuilt. Name indexes must be dropped by the caller (``filter.names.forget``).
        """
        self.cols = other.cols
        self._heap = other._heap
        self._n = other._n
        self._cap = other._cap
        self.mapped = other.mapped
        self.ext_table = other.ext_table
        self._ext_index = other._ext_index
        self.version = max(self.version, other.version) + 1

    def names_size(self) -> int:
        """Bytes of names in use (each name plus its NUL)."""
        return self._heap.used()

    def memory_bytes(self) -> int:
        return sum(arr[: self._n].nbytes for arr in self.cols.values()) + self._heap.used()

    # ------------------------------------------------------------------ (de)serialisation helpers

    def export_columns(self) -> dict[str, np.ndarray]:
        """The live columns; ``name_off`` points into :attr:`names_blob` (from_columns(separated=True))."""
        out = {k: arr[: self._n] for k, arr in self.cols.items()}
        out["name_off"] = self._heap.rebase(out["name_off"])
        return out

    @classmethod
    def from_columns(
        cls,
        columns: dict[str, np.ndarray],
        names: bytes | memoryview | np.ndarray,
        ext_table: list[str],
        *,
        separated: bool = False,
    ) -> NodeStore:
        """A store from whole columns (snapshots, the MFT reader).

        ``separated``: ``names`` is a ``\\0name`` image in id order, as :attr:`names_blob` writes
        it (used without copying). Otherwise it is a plain concatenation that ``name_off`` and
        ``name_len`` index in any order (the names are laid out anew).
        """
        n = len(columns["parent"])
        store = cls.__new__(cls)
        store._n = n
        store.version = 0
        store._cap = max(16, n)
        store.mapped = False
        store._heap = NameHeap()
        image_len = len(names) if separated else int(np.asarray(columns["name_len"]).sum(dtype=np.int64)) + n
        if mapped.POLICY.should_map(None, store._cap * ROW_BYTES + image_len):
            store._map()
        store.cols = {}
        for key, dtype in COLUMNS.items():
            if key == "name_off" and image_len >= OFF_NARROW_LIMIT:
                dtype = _OFF_WIDE
            col = columns.get(key)
            if col is not None and not store.mapped and _adoptable(col, dtype, store._cap):
                store.cols[key] = col  # a fresh column of the right shape (MFT builds): no copy
                continue
            arr = store._zeros(store._cap, dtype)
            if col is not None:
                arr[:n] = col
            store.cols[key] = arr
        if separated:
            image: bytes | memoryview | np.ndarray = names
        else:
            image, off = _separate(names, columns["name_off"], columns["name_len"])
            store.cols["name_off"][:n] = off
        if store.mapped:  # keep the names out of private memory too
            copy = mapped.mapped_zeros(len(image))
            copy[:] = np.frombuffer(image, dtype=np.uint8) if not isinstance(image, np.ndarray) else image
            image = copy
        alloc = store._heap.alloc
        store._heap = NameHeap.frozen(image, alloc)
        mapped.POLICY.register(store)
        store.ext_table = list(ext_table) or [""]
        store._ext_index = {e: i for i, e in enumerate(store.ext_table)}
        return store


def _adoptable(col: np.ndarray | None, dtype: np.dtype[np.generic], length: int) -> bool:
    """A column array the store can own as it is: right type and length, writable, not a view."""
    return (
        isinstance(col, np.ndarray)
        and col.dtype == dtype
        and col.ndim == 1
        and len(col) == length
        and col.base is None
        and col.flags.c_contiguous
        and col.flags.writeable
    )


def _separate(
    names: bytes | memoryview | np.ndarray, name_off: np.ndarray, name_len: np.ndarray
) -> tuple[np.ndarray, np.ndarray]:
    """A ``\\0name`` image in id order from names concatenated in any order, and its offsets."""
    src = names if isinstance(names, np.ndarray) else np.frombuffer(names, dtype=np.uint8)
    off = np.asarray(name_off, dtype=np.int64)
    ln = np.asarray(name_len, dtype=np.int64)
    n = len(ln)
    new_off = np.cumsum(ln + 1) - ln  # each name's start in the image (after its NUL)
    image = np.zeros(int(ln.sum()) + n, dtype=np.uint8)
    step = 1 << 18  # nodes per block: bounds the temporaries to a few MB per block
    for a in range(0, n, step):
        b = min(n, a + step)
        o, ln_b, d = off[a:b], ln[a:b], new_off[a:b]
        total = int(ln_b.sum())
        if not total:
            continue
        lo, hi = int(d[0]) - 1, int(d[-1] + ln_b[-1])
        if np.array_equal(o[1:], o[:-1] + ln_b[:-1]):  # back to back in id order (format 1): insert NULs
            keep = np.ones(hi - lo, dtype=bool)
            keep[d - 1 - lo] = False
            image[lo:hi][keep] = src[int(o[0]) : int(o[0]) + total]
            continue
        rel = np.arange(total, dtype=np.int64) - np.repeat(np.cumsum(ln_b) - ln_b, ln_b)
        image[np.repeat(d, ln_b) + rel] = src[np.repeat(o, ln_b) + rel]
    return image, new_off


def encode_name(name: str) -> bytes:
    return name.encode("utf-8", NAME_ERRORS)


def decode_name(raw: bytes) -> str:
    return raw.decode("utf-8", NAME_ERRORS)
