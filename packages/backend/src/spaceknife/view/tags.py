"""Colour tags (red / yellow / green / blue), persisted per set of scan roots.

Tags are keyed by the path relative to its scan root, so they survive rescans and snapshots.
A folder's tag applies to everything below it (inheritance is evaluated by the filter engine);
the marker is only drawn on the element that carries the tag.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import numpy as np

from spaceknife_protocol.filter import TAG_COLORS, TagColor

from ..filter.engine import TAG_BITS
from ..store.nodes import NodeStore

TAG_INDEX: dict[TagColor, int] = {c: i + 1 for i, c in enumerate(TAG_COLORS)}  # 0 = no tag


class TagStore:
    def __init__(self, file: Path | None = None) -> None:
        self.file = file
        self.tags: dict[tuple[int, str], TagColor] = {}  # (root index, relative path) -> colour
        self.version = 0
        # (version, store size, node -> colour), and the per-node arrays derived from it
        self._resolved: tuple[int, int, dict[int, TagColor]] | None = None
        self._arrays: dict[str, tuple[tuple[object, ...], np.ndarray]] = {}
        if file is not None and file.exists():
            self._load(file)

    # ------------------------------------------------------------- persistence

    def _load(self, file: Path) -> None:
        try:
            data = json.loads(file.read_text(encoding="utf-8"))
            for item in data.get("tags", []):
                color = item["color"]
                if color in TAG_INDEX:
                    self.tags[(int(item["root"]), str(item["path"]))] = color
        except (OSError, ValueError, KeyError, TypeError):
            self.tags = {}

    def save(self) -> None:
        if self.file is None:
            return
        self.file.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "version": 1,
            "tags": [{"root": r, "path": p, "color": c} for (r, p), c in sorted(self.tags.items())],
        }
        tmp = self.file.with_suffix(".tmp")
        tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=1), encoding="utf-8")
        os.replace(tmp, self.file)

    # ------------------------------------------------------------- addressing

    @staticmethod
    def address(store: NodeStore, node: int) -> tuple[int, str] | None:
        """(index of the scan root, path below it) of a node; None for node 0."""
        roots = list(store.roots())
        parts: list[str] = []
        cur = node
        parent = store.cols["parent"]
        while cur > 0 and cur not in roots:
            parts.append(store.name(cur))
            cur = int(parent[cur])
        if cur <= 0:
            return None
        return roots.index(cur), "/".join(reversed(parts))

    def _resolve(self, store: NodeStore) -> dict[int, TagColor]:
        n = len(store)
        if self._resolved is not None and self._resolved[0] == self.version and self._resolved[1] == n:
            return self._resolved[2]
        roots = list(store.roots())
        out: dict[int, TagColor] = {}
        for (r, rel), color in self.tags.items():
            if r >= len(roots):
                continue
            node: int | None = roots[r]
            for part in (p for p in rel.split("/") if p):
                assert node is not None
                node = next((ch for ch in store.children(node) if store.name(ch) == part), None)
                if node is None:
                    break
            if node is not None:
                out[node] = color
        self._resolved = (self.version, n, out)
        return out

    # ------------------------------------------------------------- queries and edits

    def of(self, store: NodeStore, node: int) -> TagColor | None:
        return self._resolve(store).get(node)

    def _array(self, store: NodeStore, values: dict[TagColor, int], which: str) -> np.ndarray:
        key = (which, self.version, len(store), id(store))
        hit = self._arrays.get(which)
        if hit is not None and hit[0] == key:
            return hit[1]
        out = np.zeros(len(store), dtype=np.uint8)
        for node, color in self._resolve(store).items():
            out[node] = values[color]
        out.flags.writeable = False
        self._arrays[which] = (key, out)
        return out

    def bits(self, store: NodeStore) -> np.ndarray:
        """Own tag bits per node (``TAG_BITS``), for the filter engine (read-only, cached)."""
        return self._array(store, TAG_BITS, "bits")

    def index(self, store: NodeStore) -> np.ndarray:
        """Own tag per node as 0 (none) or 1–4 (red, yellow, green, blue), for draw lists."""
        return self._array(store, TAG_INDEX, "index")

    def set(self, store: NodeStore, node: int, color: TagColor | None) -> bool:
        addr = self.address(store, node)
        if addr is None:
            return False
        if color is None:
            changed = self.tags.pop(addr, None) is not None
        else:
            changed = self.tags.get(addr) != color
            self.tags[addr] = color
        if changed:
            self.version += 1
        return changed

    def clear_subtree(self, store: NodeStore, node: int) -> int:
        """Remove the tag of ``node`` and of everything below it (Ctrl+0). Returns how many."""
        addr = self.address(store, node)
        if addr is None:
            return 0
        root, rel = addr
        prefix = rel + "/" if rel else ""
        drop = [k for k in self.tags if k[0] == root and (k[1] == rel or k[1].startswith(prefix))]
        for k in drop:
            del self.tags[k]
        if drop:
            self.version += 1
        return len(drop)
