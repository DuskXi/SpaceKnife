"""Build small NodeStores from nested dicts (tests only).

``{"name": size}`` is a file (its extension comes from the name); ``{"name": {...}}`` is a folder.
"""

from __future__ import annotations

from typing import Any

import numpy as np

from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore


def ext_of(name: str) -> str:
    dot = name.rfind(".")
    if dot <= 0 or len(name) - dot > 17:
        return ""
    return name[dot + 1 :].lower()


def build(
    spec: dict[str, Any], root: str = "/root", *, cluster: int = 1, pending: tuple[str, ...] = ()
) -> NodeStore:
    """Store with one scan root; file allocation = size rounded up to ``cluster``."""
    store = NodeStore()
    root_id = store.add_root(root)
    queue: list[tuple[int, dict[str, Any], str]] = [(root_id, spec, "")]
    while queue:
        node, children, prefix = queue.pop(0)
        names = list(children)
        n = len(names)
        entries = {k: np.zeros(n, dtype=np.int64) for k in ENTRY_COLUMNS}
        exts = np.zeros(n, dtype=np.uint16)
        for i, name in enumerate(names):
            value = children[name]
            if isinstance(value, dict):
                entries["kind"][i] = Kind.DIR
            else:
                entries["kind"][i] = Kind.FILE
                entries["size"][i] = value
                entries["alloc"][i] = -(-value // cluster) * cluster
                exts[i] = store.ext_id(ext_of(name))
        first = store.ingest_names(node, [nm.encode() for nm in names], entries, exts)
        for i, name in enumerate(names):
            value = children[name]
            if isinstance(value, dict):
                path = f"{prefix}/{name}"
                if path.lstrip("/") in pending:
                    continue  # stays PENDING with no children
                queue.append((first + i, value, path))
    for path in pending:
        node = find(store, path)
        assert store.cols["flags"][node] & Flag.PENDING
    return store


def find(store: NodeStore, path: str) -> int:
    node = next(iter(store.roots()))
    for part in [p for p in path.split("/") if p]:
        node = next(ch for ch in store.children(node) if store.name(ch) == part)
    return node
