"""Path search and path input for the palette (UI-W4-26…29, UI-W4-33, UI-W4-37).

Names are searched in tiers — exact name, name prefix, inside the name, scattered letters — and
each tier is ordered by size, largest first (UI-W4-28). Everything runs over the NUL-joined name
segments of :class:`~spaceknife.filter.names.NameIndex`, so a search over a million names stays in
C regex scans and numpy.
"""

from __future__ import annotations

import os
import re
import string
import sys
from dataclasses import dataclass
from pathlib import PurePath
from typing import Literal

import numpy as np

from spaceknife_protocol.filter_syntax import normalize_glob

from ..filter.names import char_regex, name_index
from ..store.nodes import Kind, NodeStore
from .fuzzy import score

_DRIVE = re.compile(r"^[A-Za-z]:(?:[\\/]|$)")
SEPARATORS = "\\/" if sys.platform == "win32" else "/"
_ASCII_LOWER = str.maketrans(string.ascii_uppercase, string.ascii_lowercase)  # keeps every offset


@dataclass(frozen=True, slots=True)
class PathHit:
    node: int
    name: str
    parent: str
    is_dir: bool
    alloc: int
    hits: list[tuple[int, int]]
    tier: int
    complete: str


def looks_like_path(text: str) -> bool:
    """Drive letter, UNC, absolute POSIX path or home-relative (UI-W4-33, W4 待定 5)."""
    t = text.strip()
    return bool(_DRIVE.match(t)) or t.startswith("\\\\") or t.startswith("/") or t.startswith("~")


def expand(text: str) -> str:
    t = os.path.expanduser(text.strip())
    if sys.platform == "win32":
        t = t.replace("/", "\\")
    return t


def trimmed(path: str) -> str:
    """Without trailing separators, except for a root (a drive root or ``/``)."""
    out = path.rstrip(SEPARATORS)
    if not out or (len(out) == 2 and out[1] == ":"):
        return path[: len(out) + 1] if len(path) > len(out) else path
    return out


def split_last(path: str) -> tuple[str, str]:
    """('C:\\Users\\lenny\\', 'Vi') for 'C:\\Users\\lenny\\Vi'."""
    cut = max(path.rfind(s) for s in SEPARATORS)
    return path[: cut + 1], path[cut + 1 :]


def _with_sep(path: str) -> str:
    return path if path.endswith(tuple(SEPARATORS)) else path + os.sep


def node_path(store: NodeStore, node: int) -> str:
    return store.path(node)


def seed_text(store: NodeStore, node: int) -> str:
    """What a selection seeds the path search with: its name. A scan root is stored under its full
    path and seeds with its last segment (``D:\\Photos`` -> ``Photos``; a volume root with nothing)."""
    name = store.name(node)
    return PurePath(name).name if int(store.cols["parent"][node]) == 0 else name


# ------------------------------------------------------------------ name search


def _escape(q: str) -> str:
    return "".join("\\" + ch if ch in "\\*?" else ch for ch in q)


def _subsequence(q: str) -> tuple[re.Pattern[bytes], bool]:
    gap = rb"[^\x00]*?"
    lower = q.isascii()
    parts = [re.escape(ch.lower().encode("ascii")) for ch in q] if lower else [char_regex(ch) for ch in q]
    return re.compile(gap.join(parts), re.DOTALL), lower


def _top(ids: np.ndarray, sizes: np.ndarray, k: int) -> np.ndarray:
    if k <= 0 or not ids.size:
        return ids[:0]
    s = sizes[ids]
    if ids.size > k:
        part = np.argpartition(-s, k - 1)[:k]
        ids, s = ids[part], s[part]
    return ids[np.lexsort((ids, -s))]


def _hits(name: str, query: str, tier: int) -> list[tuple[int, int]]:
    """Highlighted runs of ``name`` (index runs within it) for a hit of ``tier``."""
    if tier == 0:
        return [(0, len(name))]
    if tier == 1:
        return [(0, min(len(query), len(name)))]
    if tier == 2:
        # ASCII-only lower-casing, as the index matches; str.lower() can change lengths ("İ": 2 chars)
        at = name.translate(_ASCII_LOWER).find(query.translate(_ASCII_LOWER))
        if at >= 0:
            return [(at, min(at + len(query), len(name)))]
    got = score(query, name)
    return got[1] if got else []


def search(
    store: NodeStore,
    sizes: np.ndarray,
    query: str,
    limit: int,
    *,
    exclude: int | None = None,
    contains_only: bool = False,
    literal: bool = False,
) -> tuple[list[PathHit], int]:
    """Best ``limit`` nodes whose name matches ``query`` and the number of all hits.

    ``literal``: ``*`` and ``?`` are plain characters (a selection's name as the seed).
    """
    q = query.strip()
    n = min(len(store), len(sizes))  # sizes may be a moment older than a growing store
    if not q or n <= 1:
        return [], 0
    index = name_index(store)
    wild = not literal and any(ch in q for ch in "*?")
    tiers: list[np.ndarray]
    disjoint = True
    if wild:  # the user typed a glob: one tier, matched like a filter mask
        tiers = [index.match(normalize_glob(q), n)]
    elif q.isascii():
        # scattered letters only from three characters on: two scattered letters match almost anything
        tiers = list(index.ascii_tiers(q, n, scattered=not contains_only and len(q) >= 3))
    else:
        disjoint = False
        lit = _escape(q)
        tiers = [
            index.match(normalize_glob(lit), n),
            index.match(normalize_glob(lit + "*"), n),
            index.match(normalize_glob("*" + lit + "*"), n),
        ]
        if not contains_only and len(q) > 1:
            regex, lower = _subsequence(q)
            tiers.append(index.search("seq:" + q, regex, lower=lower, n=n))
    seen = np.zeros(0, dtype=np.int64)
    picked: list[tuple[int, int]] = []
    total = 0
    for tier, ids in enumerate(tiers):
        ids = ids[ids > 0]
        if exclude is not None:
            ids = ids[ids != exclude]
        if not disjoint:
            if seen.size:
                ids = np.setdiff1d(ids, seen, assume_unique=True)
            seen = np.union1d(seen, ids)
        total += int(ids.size)
        for node in _top(ids, sizes, limit - len(picked)).tolist():
            picked.append((node, 1 if wild else tier))
    return [_hit(store, sizes, node, q, tier, glob=wild) for node, tier in picked], total


def _hit(
    store: NodeStore, sizes: np.ndarray, node: int, query: str, tier: int, *, glob: bool = False
) -> PathHit:
    c = store.cols
    name = store.name(node)
    is_dir = bool(c["kind"][node] == Kind.DIR)
    parent = int(c["parent"][node])
    full = store.path(node)
    hits = [] if tier == 4 or glob else _hits(name, query, tier)
    return PathHit(
        node=node,
        name=name,
        parent=store.path(parent) if parent > 0 else "",
        is_dir=is_dir,
        alloc=int(sizes[node]),
        hits=hits,
        tier=tier,
        complete=_with_sep(full) if is_dir else full,
    )


def children(
    store: NodeStore, sizes: np.ndarray, folder: int, partial: str, limit: int
) -> tuple[list[PathHit], int]:
    """A folder's children for a path typed up to ``partial`` (UI-W4-37): prefix, inside, scattered."""
    kids = np.fromiter(store.children(folder), dtype=np.int64)
    kids = kids[kids < len(sizes)]  # listed after ``sizes`` was taken (scan running): no size yet
    if not partial:
        top = _top(kids, sizes, limit)
        return [_hit(store, sizes, int(k), "", 4) for k in top.tolist()], int(kids.size)
    ranked: list[tuple[int, int, int]] = []  # (tier, -size, node)
    p = partial.lower()
    for k in kids.tolist():
        name = store.name(k).lower()
        if name == p:
            tier = 0
        elif name.startswith(p):
            tier = 1
        elif p in name:
            tier = 2
        elif score(partial, name) is not None:
            tier = 3
        else:
            continue
        ranked.append((tier, -int(sizes[k]), k))
    ranked.sort()
    return [_hit(store, sizes, k, partial, t) for t, _, k in ranked[:limit]], len(ranked)


# ------------------------------------------------------------------ the input as a path


@dataclass(frozen=True, slots=True)
class PathInput:
    state: Literal["in_scan", "outside", "missing"]
    path: str
    node: int | None
    is_dir: bool
    alloc: int | None
    complete: str | None


def _sized(store: NodeStore | None, sizes: np.ndarray | None, path: str) -> int | None:
    """The node at ``path`` once it has a size: one added after ``sizes`` was taken (scan running)
    counts as not scanned yet."""
    node = store.find(path) if store is not None and sizes is not None else None
    return node if node is not None and sizes is not None and node < len(sizes) else None


def resolve(store: NodeStore | None, sizes: np.ndarray | None, text: str) -> PathInput:
    path = expand(text)
    node = _sized(store, sizes, path)
    complete = completion(store, sizes, path)
    if node is not None and store is not None and sizes is not None:
        is_dir = bool(store.cols["kind"][node] == Kind.DIR)
        return PathInput("in_scan", path, node, is_dir, int(sizes[node]), complete)
    try:
        exists = os.path.exists(path)
        is_dir = os.path.isdir(path)
    except OSError:
        exists, is_dir = False, False
    return PathInput("outside" if exists else "missing", path, None, is_dir, None, complete)


def completion(store: NodeStore | None, sizes: np.ndarray | None, path: str) -> str | None:
    """The path completed by one segment (Tab), folders with a trailing separator."""
    folder, partial = split_last(path)
    if not folder:
        return None
    if not partial:
        return None
    p = partial.lower()
    node = _sized(store, sizes, folder)
    if node is not None and store is not None and sizes is not None and store.cols["kind"][node] == Kind.DIR:
        bound = len(sizes)
        cands = [k for k in store.children(node) if k < bound and store.name(k).lower().startswith(p)]
        if not cands:
            return None
        kind = store.cols["kind"]
        best = min(cands, key=lambda k: (kind[k] != Kind.DIR, -int(sizes[k]), store.name(k).lower()))
        name = store.name(best)
        return folder + name + (os.sep if kind[best] == Kind.DIR else "")
    try:
        with os.scandir(folder) as it:
            found = [
                (not e.is_dir(follow_symlinks=False), e.name.lower(), e.name, e.is_dir())
                for e in it
                if e.name.lower().startswith(p)
            ]
    except OSError:
        return None
    if not found:
        return None
    _, _, name, is_dir = min(found)
    return folder + name + (os.sep if is_dir else "")


__all__ = [
    "PathHit",
    "PathInput",
    "children",
    "completion",
    "expand",
    "looks_like_path",
    "resolve",
    "search",
    "seed_text",
    "split_last",
]
