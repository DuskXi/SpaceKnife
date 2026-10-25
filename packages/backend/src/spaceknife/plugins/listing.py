"""Building a node store from what a scan source listed (paths relative to its root, '/'-separated).

The entries arrive in any order; parent folders are added when missing. The store is built breadth
first so every folder's children are one contiguous id range (the store's invariant), the same as a
hard scan's. A long listing is built more than once while it arrives (sources.py): what came so far
is shown, the final tree replaces it.

An entry carries what a disk scan records (the SDK's ``Listing``): kind (file, folder, link, other),
sizes, times, attribute flags (only those in ``SOURCE_FLAGS``), a file id with its link count and
device, and a folder's own size. The host counts a hard-linked file once: of the names that share a
(device, file id) the first in tree order counts, the others get ``HARDLINK_DUP`` as in a disk scan.
"""

from __future__ import annotations

import os
from collections import deque
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from spaceknife_sdk.models import LISTING_ATTRS

from ..store.nodes import ENTRY_COLUMNS, NAME_ERRORS, Flag, Kind, NodeStore

SOURCE_FLAGS = 0
for _bit in LISTING_ATTRS.values():
    SOURCE_FLAGS |= _bit
"""The flags a source may set (``Listing(attrs=…)``); the host's own (pending, virtual, hard-link
duplicate, mount, gone …) are never taken from a listing."""

# a file: size alloc mtime ctime atime kind flags ino nlink dev
type FileRow = tuple[int, int, int, int, int, int, int, int, int, int]
_NO_ID = (0, 0, 0, 1, 0)  # kind flags ino nlink dev of a file listed without them


@dataclass(slots=True)
class _Dir:
    mtime: int = 0
    size: int = 0  # the folder entry's own size and allocation (not its contents)
    alloc: int = 0
    flags: int = 0
    dirs: dict[str, _Dir] = field(default_factory=dict)
    files: dict[str, tuple[int, ...]] = field(default_factory=dict)  # FileRow (or its first five)


def _ext(name: str) -> str:
    dot = name.rfind(".")
    if dot <= 0 or len(name) - dot > 17:
        return ""
    return name[dot + 1 :].lower()


def _col(cols: Mapping[str, np.ndarray], name: str, n: int, default: int) -> list[int]:
    got = cols.get(name)
    if got is None or len(got) != n:  # a batch of an older SDK: the column is not there
        return [default] * n
    return [int(x) for x in np.asarray(got).tolist()]


class Collector:
    """Gathers a source's batches (the SDK's Listing columns) into a folder tree."""

    def __init__(self) -> None:
        self.root = _Dir()
        self.count = 0

    def _folder(self, parts: list[str]) -> _Dir:
        d = self.root
        for p in parts:
            nxt = d.dirs.get(p)
            if nxt is None:
                nxt = d.dirs[p] = _Dir()
                self.count += 1
            d = nxt
        return d

    def add_batch(self, cols: Mapping[str, np.ndarray]) -> int:
        blob = bytes(cols["paths"])
        lens = np.asarray(cols["path_len"], dtype=np.int64)
        n = len(lens)
        kinds = _col(cols, "kind", n, 0)
        size, alloc = _col(cols, "size", n, 0), _col(cols, "alloc", n, 0)
        mt, ct, at = _col(cols, "mtime", n, 0), _col(cols, "ctime", n, 0), _col(cols, "atime", n, 0)
        flags = _col(cols, "flags", n, 0)
        ino, nlink, dev = _col(cols, "ino", n, 0), _col(cols, "nlink", n, 1), _col(cols, "dev", n, 0)
        pos = 0
        for i, ln in enumerate(lens.tolist()):
            path = blob[pos : pos + ln].decode("utf-8", NAME_ERRORS)
            pos += ln
            parts = [p for p in path.split("/") if p and p not in (".", "..")]
            if not parts:
                continue
            fl = flags[i] & SOURCE_FLAGS
            if kinds[i] == Kind.DIR:
                d = self._folder(parts)
                d.mtime, d.size, d.alloc, d.flags = mt[i], max(0, size[i]), max(0, alloc[i]), fl
            else:
                parent = self._folder(parts[:-1])
                name = parts[-1]
                if name not in parent.files:
                    self.count += 1
                kind = kinds[i] if kinds[i] in (Kind.FILE, Kind.LINK, Kind.OTHER) else Kind.FILE
                parent.files[name] = (
                    max(0, size[i]),
                    max(0, alloc[i]),
                    mt[i],
                    ct[i],
                    at[i],
                    int(kind),
                    fl,
                    ino[i],
                    min(max(nlink[i], 0), 65535),
                    dev[i],
                )
        return self.count


def build(root_name: str, top: _Dir, *, virtual: bool, measure: str = "alloc") -> NodeStore:
    """The store of one root: breadth first, folders then files of each folder by name. ``measure``:
    what the views measure areas by (``"size"``: a source with ``size = "size"``)."""
    store = NodeStore()
    store.measure = measure
    root = store.add_root(root_name, flags=Flag.PENDING | (Flag.VIRTUAL if virtual else Flag.NONE))
    store.set_root_stats(root, mtime=top.mtime)
    seen: set[tuple[int, int]] = set()  # (device, file id) of hard-linked files counted already
    dup = int(Flag.HARDLINK_DUP)
    queue: deque[tuple[int, _Dir]] = deque([(root, top)])
    while queue:
        node, d = queue.popleft()
        dir_names = sorted(d.dirs)
        file_names = sorted(d.files)
        names = dir_names + file_names
        n = len(names)
        entries = {k: np.zeros(n, dtype=np.uint64 if k == "ino" else np.int64) for k in ENTRY_COLUMNS}
        exts = np.zeros(n, dtype=np.uint16)
        for i, name in enumerate(dir_names):
            sub = d.dirs[name]
            entries["kind"][i] = Kind.DIR
            entries["mtime"][i] = sub.mtime
            entries["size"][i] = sub.size
            entries["alloc"][i] = sub.alloc
            entries["flags"][i] = sub.flags
            entries["nlink"][i] = 1
        for j, name in enumerate(file_names):
            i = len(dir_names) + j
            row = d.files[name]
            s, a, m, c, t = row[:5]
            kind, fl, ino, nlink, dev = row[5:10] if len(row) >= 10 else _NO_ID
            if kind == Kind.FILE and ino and nlink > 1:
                key = (dev, ino)
                if key in seen:
                    fl |= dup  # another name of a file counted already: adds no bytes
                else:
                    seen.add(key)
            entries["kind"][i] = kind
            entries["size"][i] = s
            entries["alloc"][i] = a
            entries["mtime"][i] = m
            entries["ctime"][i] = c
            entries["atime"][i] = t
            entries["flags"][i] = fl
            entries["ino"][i] = ino
            entries["nlink"][i] = nlink
            exts[i] = store.ext_id(_ext(name))
        first = store.ingest_names(node, [nm.encode("utf-8", NAME_ERRORS) for nm in names], entries, exts)
        for i, name in enumerate(dir_names):
            queue.append((first + i, d.dirs[name]))
    return store


def from_spec(spec: Mapping[str, Any], root: str, now: int) -> tuple[NodeStore, list[tuple[str, bytes]]]:
    """A store from a nested dict (MockHost / tests): ``name: size | (size, mtime) | bytes |
    (bytes, mtime) | dict``.

    ``mtime`` is unix seconds or ``"YYYY-MM-DD"``; ``bytes`` are file contents (returned, relative
    path -> content, for the caller to write under ``root``, with that modification time)."""
    import datetime as dt

    top = _Dir()
    contents: list[tuple[str, bytes]] = []

    def when(v: Any) -> int:
        if isinstance(v, str):
            return int(dt.datetime.fromisoformat(v).replace(tzinfo=dt.UTC).timestamp())
        return int(v)

    def fill(d: _Dir, items: Mapping[str, Any], prefix: str) -> None:
        for name, value in items.items():
            if isinstance(value, Mapping):
                sub = d.dirs[name] = _Dir(mtime=now)
                fill(sub, value, f"{prefix}{name}/")
            elif isinstance(value, bytes | bytearray):
                d.files[name] = (len(value), len(value), now, now, now)
                contents.append((prefix + name, bytes(value)))
            elif isinstance(value, tuple) and value and isinstance(value[0], bytes | bytearray):
                data = bytes(value[0])  # (contents, mtime)
                mtime = when(value[1]) if len(value) > 1 else now
                d.files[name] = (len(data), len(data), mtime, mtime, mtime)
                contents.append((prefix + name, data))
            elif isinstance(value, tuple):
                size = int(value[0])
                mtime = when(value[1]) if len(value) > 1 else now
                d.files[name] = (size, size, mtime, mtime, mtime)
            else:
                d.files[name] = (int(value), int(value), now, now, now)

    fill(top, spec, "")
    return build(root, top, virtual=False), contents


def path_remap(old: NodeStore, new: NodeStore) -> np.ndarray:
    """For each node of ``old``, the node of ``new`` at the same path (-1: not there any more).

    Roots pair up in order; below them names are matched folder by folder, shallow first (a
    source's tree listed again: views keep their place, history and selection).
    """
    n = len(old)
    out = np.full(n, -1, dtype=np.int64)
    if n == 0:
        return out
    out[0] = 0
    for a, b in zip(old.roots(), new.roots(), strict=False):
        out[a] = b
    parent = old.cols["parent"]
    index: dict[int, dict[str, int]] = {}
    for node in np.argsort(old.cols["depth"][:n], kind="stable").tolist():
        if out[node] >= 0 or node == 0:
            continue
        target = int(out[int(parent[node])])
        if target < 0:
            continue
        names = index.get(target)
        if names is None:
            names = index[target] = {new.name(c): c for c in new.children(target)}
        out[node] = names.get(old.name(node), -1)
    return out


def iter_paths(store: NodeStore) -> Iterable[str]:
    for i in range(1, len(store)):
        yield store.path(i).replace(os.sep, "/")


__all__ = ["Collector", "build", "from_spec", "path_remap"]
