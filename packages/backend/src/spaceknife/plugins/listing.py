"""Building a node store from what a scan source listed (paths relative to its root, '/'-separated).

The entries arrive in any order; parent folders are added when missing. The store is built breadth
first so every folder's children are one contiguous id range (the store's invariant), the same as a
hard scan's. Thin (docs/plugins.md, 「scan_source」): the whole listing first, then the tree.
"""

from __future__ import annotations

import os
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from ..store.nodes import ENTRY_COLUMNS, NAME_ERRORS, Flag, Kind, NodeStore


@dataclass(slots=True)
class _Dir:
    mtime: int = 0
    dirs: dict[str, _Dir] = field(default_factory=dict)
    files: dict[str, tuple[int, int, int, int, int]] = field(default_factory=dict)  # size alloc m c a


def _ext(name: str) -> str:
    dot = name.rfind(".")
    if dot <= 0 or len(name) - dot > 17:
        return ""
    return name[dot + 1 :].lower()


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
        kinds = np.asarray(cols["kind"])
        size, alloc = np.asarray(cols["size"]), np.asarray(cols["alloc"])
        mt, ct, at = np.asarray(cols["mtime"]), np.asarray(cols["ctime"]), np.asarray(cols["atime"])
        pos = 0
        for i, ln in enumerate(lens.tolist()):
            path = blob[pos : pos + ln].decode("utf-8", NAME_ERRORS)
            pos += ln
            parts = [p for p in path.split("/") if p and p not in (".", "..")]
            if not parts:
                continue
            if int(kinds[i]) == 1:
                d = self._folder(parts)
                d.mtime = int(mt[i])
            else:
                parent = self._folder(parts[:-1])
                name = parts[-1]
                if name not in parent.files:
                    self.count += 1
                parent.files[name] = (int(size[i]), int(alloc[i]), int(mt[i]), int(ct[i]), int(at[i]))
        return self.count


def build(root_name: str, top: _Dir, *, virtual: bool) -> NodeStore:
    """The store of one root: breadth first, folders then files of each folder by name."""
    store = NodeStore()
    root = store.add_root(root_name, flags=Flag.PENDING | (Flag.VIRTUAL if virtual else Flag.NONE))
    store.set_root_stats(root, mtime=top.mtime)
    queue: list[tuple[int, _Dir]] = [(root, top)]
    while queue:
        node, d = queue.pop(0)
        dir_names = sorted(d.dirs)
        file_names = sorted(d.files)
        names = dir_names + file_names
        n = len(names)
        entries = {k: np.zeros(n, dtype=np.int64) for k in ENTRY_COLUMNS}
        exts = np.zeros(n, dtype=np.uint16)
        for i, name in enumerate(dir_names):
            entries["kind"][i] = Kind.DIR
            entries["mtime"][i] = d.dirs[name].mtime
        for j, name in enumerate(file_names):
            i = len(dir_names) + j
            s, a, m, c, t = d.files[name]
            entries["kind"][i] = Kind.FILE
            entries["size"][i] = s
            entries["alloc"][i] = a
            entries["mtime"][i] = m
            entries["ctime"][i] = c
            entries["atime"][i] = t
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
