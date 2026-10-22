"""``host.query``: the rows a plugin asks for, selected on the host (``TreeView.query``).

Runs on the compute pool under the read lock: a filter expression (files, as in the filter box), a
subtree, a kind, a minimum size, then the columns asked for of what is left — sorted, cut to the
limit and sent as one binary frame (ids, numeric columns, names and paths as blobs).
"""

from __future__ import annotations

import os
from collections.abc import Mapping, Sequence
from typing import Any

import numpy as np

from spaceknife_protocol.binary import encode_frame
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.filter_syntax import parse
from spaceknife_sdk.tree import COLUMNS, KIND_NAMES, QUERY_COLUMNS

from ..filter.engine import FilterContext, evaluate
from ..filter.vocab import platform_vocab
from ..layout.basis import Basis
from ..store.nodes import NAME_ERRORS, Flag, Kind, NodeStore
from ..store.subtree import subtree_chunks
from .runtime.base import RuntimeCallError

MAX_LIMIT = 1_000_000


def _effective(store: NodeStore, ids: np.ndarray, logical: bool) -> np.ndarray:
    c = store.cols
    is_dir = c["kind"][ids] == Kind.DIR
    dup = (c["flags"][ids] & np.uint32(Flag.HARDLINK_DUP)) != 0
    own = c["size" if logical else "alloc"][ids]
    tot = c["tot_size" if logical else "tot_alloc"][ids]
    out: np.ndarray = np.where(is_dir, tot, np.where(dup, 0, own)).astype(np.int64)
    return out


def column(store: NodeStore, basis: Basis | None, name: str, ids: np.ndarray) -> np.ndarray:
    if name == "bytes":
        return _effective(store, ids, False)
    if name == "logical":
        return _effective(store, ids, True)
    if name == "cls":
        if basis is None:
            return np.zeros(len(ids), dtype=np.uint8)
        return basis.cls_of(ids)
    out: np.ndarray = store.cols[name][ids]
    return out


def node_dict(store: NodeStore, basis: Basis | None, node: int) -> dict[str, Any]:
    c = store.cols
    v = store.view(node)
    cls = ""
    if basis is not None:
        idx = int(basis.cls_of(np.array([node], dtype=np.int64))[0])
        ids = basis.class_ids
        cls = ids[idx] if idx < len(ids) else ""
    return {
        "id": node,
        "name": v.name,
        "path": store.path(node),
        "kind": int(v.kind),
        "size": v.tot_size,
        "alloc": v.tot_alloc,
        "files": v.tot_files,
        "dirs": v.tot_dirs,
        "mtime": v.mtime,
        "ctime": v.ctime,
        "atime": v.atime,
        "ext": v.ext,
        "parent": v.parent,
        "depth": v.depth,
        "child_count": v.child_count,
        "flags": int(c["flags"][node]),
        "cls": cls,
    }


def run(
    store: NodeStore,
    basis: Basis | None,
    p: Mapping[str, Any],
    *,
    tags: np.ndarray | None = None,
    sets: Mapping[str, np.ndarray] | None = None,
) -> tuple[dict[str, Any], list[bytes]]:
    n = len(store)
    cols: Sequence[str] = list(p.get("columns") or ["bytes"])
    bad = [c for c in cols if c not in QUERY_COLUMNS]
    if bad:
        raise RuntimeCallError(ErrorCode.INVALID_PARAMS, f"unknown columns: {bad}")
    limit = max(0, min(int(p.get("limit") or 100_000), MAX_LIMIT))
    flags = store.cols["flags"][:n]
    keep = (flags & np.uint32(Flag.GONE | Flag.VIRTUAL)) == 0
    keep[0] = False
    under = p.get("under")
    if under is not None:
        under = int(under)
        if not 0 <= under < n:
            raise RuntimeCallError(ErrorCode.UNKNOWN_NODE, f"node {under} does not exist")
        inside = np.zeros(n, dtype=bool)
        for chunk in subtree_chunks(store, under):
            inside[chunk[chunk < n]] = True
        keep &= inside
    kind = p.get("kind")
    if kind is not None:
        k = KIND_NAMES.get(str(kind))
        if k is None:
            raise RuntimeCallError(ErrorCode.INVALID_PARAMS, f"kind: one of {', '.join(KIND_NAMES)}")
        keep &= store.cols["kind"][:n] == int(k)
    where = p.get("where")
    if where:
        parsed = parse(str(where), None, platform_vocab())
        if parsed.normal is None:
            errors = [d.code for d in parsed.diagnostics if d.severity == "error"]
            raise RuntimeCallError(ErrorCode.INVALID_PARAMS, f"invalid filter {where!r}: {', '.join(errors)}")
        if not parsed.normal.empty:
            res = evaluate(store, parsed.normal, FilterContext.current(tags=tags, sets=sets))
            match = np.zeros(n, dtype=bool)
            match[: res.n] = res.match
            keep &= match
    ids = np.flatnonzero(keep).astype(np.int64)
    min_size = p.get("min_size")
    if min_size is not None and len(ids):
        ids = ids[_effective(store, ids, False) >= int(min_size)]
    total = len(ids)
    sort = p.get("sort")
    if sort:
        desc = str(sort).startswith("-")
        key = str(sort).lstrip("-+")
        if key not in QUERY_COLUMNS or key in ("name", "path"):
            if key in ("name", "path"):
                text = store.name if key == "name" else store.path
                names = [text(int(i)).casefold() for i in ids]
                order = sorted(range(len(ids)), key=names.__getitem__, reverse=desc)
                ids = ids[np.asarray(order, dtype=np.int64)] if order else ids
            else:
                raise RuntimeCallError(ErrorCode.INVALID_PARAMS, f"cannot sort by {key!r}")
        else:
            values = column(store, basis, key, ids)
            ranks = np.argsort(-values if desc else values, kind="stable")
            ids = ids[ranks]
    ids = ids[:limit]
    out: dict[str, np.ndarray] = {"id": ids.astype(np.int64)}
    blobs: dict[str, bytes] = {}
    for name in cols:
        if name in ("name", "path"):
            texts = [store.name(int(i)) if name == "name" else store.path(int(i)) for i in ids]
            enc = [t.encode("utf-8", NAME_ERRORS) for t in texts]
            blobs[name] = b"".join(enc)
            out[f"{name}_len"] = np.fromiter((len(e) for e in enc), dtype=np.uint32, count=len(enc))
        elif name in COLUMNS or name in ("bytes", "logical"):
            arr = column(store, basis, name, ids)
            out[name] = np.ascontiguousarray(arr)
    frame = encode_frame(stream="plugin.query", layout="query.v1", columns=out, blobs=blobs)
    return {"total": total, "count": len(ids)}, [frame]


def find(store: NodeStore, path: str) -> int | None:
    """A node by absolute path, or relative to the only root."""
    found = store.find(path)
    if found is not None:
        return found
    roots = list(store.roots())
    if len(roots) == 1 and not os.path.isabs(path):
        return store.find(os.path.join(store.name(roots[0]), path.replace("/", os.sep)))
    return None


__all__ = ["MAX_LIMIT", "column", "find", "node_dict", "run"]
