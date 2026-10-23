"""Which copy of each group stays (docs/dupes.md §8.2): roles and the reason for each keeper.

Order of precedence, earlier first:

1. always kept: protected or program paths (``F_NEEDED``), names the delete basket cannot take
   (``F_PINNED``: a trailing dot or space), and physical files that may only be kept: several names
   (hard links, or one file seen through two paths), hard links outside the scans, network files;
2. the preferred folders or volumes, in the user's order;
3. the base rule: oldest (birth time; the modification time where the file system has none),
   newest (modification time) or shortest path;
4. folder-consistent (on by default): groups whose two copies sit in the same pair of folders
   (P, Q), when the pair holds two or more such groups, keep the copy on the same side;
5. across volumes, one copy per volume (on by default);
6. ties: the shorter path, then the smaller path;
7. manual choices (group digest + path), which override 2–5 but never 1.

Invariants enforced here, not by the interface: every group keeps at least one copy; with
``per_volume`` every volume of the group keeps one; needed and pinned copies are always kept.
"""

from __future__ import annotations

import os
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Literal

from . import codes as C

Rule = Literal["oldest", "newest", "shortest"]


@dataclass(slots=True)
class KeepOptions:
    rule: Rule = "oldest"
    prefer: list[str] = field(default_factory=list)
    folder_consistent: bool = True
    per_volume: bool = True
    overrides: dict[tuple[bytes, str], Literal["keep", "extra"]] = field(default_factory=dict)


@dataclass(slots=True)
class UnitView:
    """One physical unit of a group as the keep rules see it."""

    rows: list[int]  # member rows, the representative first
    path: str
    vol: int
    btime: int
    mtime: int
    forced: bool  # may only be kept
    forced_why: int = C.WHY_NEEDED


def _norm(p: str) -> str:
    return os.path.normcase(os.path.normpath(p))


def _prefer_rank(path: str, prefer: Sequence[str]) -> int:
    key = _norm(path)
    for i, p in enumerate(prefer):
        base = _norm(p).rstrip("\\/")
        if (
            key == base
            or key.startswith(base + os.sep)
            or (len(base) == 2 and base[1] == ":" and key.startswith(base))
        ):
            return i
    return len(prefer)


def _rule_key(u: UnitView, rule: Rule) -> tuple[int, int, str]:
    if rule == "newest":
        k = -u.mtime
    elif rule == "shortest":
        k = len(u.path)
    else:
        k = u.btime if u.btime > 0 else u.mtime
    return (k, len(u.path), u.path)


def choose(
    units: Sequence[UnitView], opts: KeepOptions, *, digest: bytes | None, side: dict[str, int] | None = None
) -> list[tuple[int, int]]:
    """(role, why) per unit. ``side``: folder-consistent preference (folder path -> rank, lower wins)."""
    n = len(units)
    out: list[tuple[int, int]] = [(C.ROLE_EXTRA, 0)] * n
    kept = [False] * n
    for i, u in enumerate(units):
        if u.forced:
            out[i] = (C.ROLE_KEEP, u.forced_why)
            kept[i] = True
    manual_keep = {i for i, u in enumerate(units) if opts.overrides.get((digest or b"", u.path)) == "keep"}
    manual_extra = {i for i, u in enumerate(units) if opts.overrides.get((digest or b"", u.path)) == "extra"}
    for i in manual_keep:
        if not kept[i]:
            out[i] = (C.ROLE_KEEP, C.WHY_MANUAL)
            kept[i] = True

    def best(cands: list[int]) -> tuple[int, int] | None:
        pool = [i for i in cands if i not in manual_extra] or cands
        if not pool:
            return None
        if opts.prefer:
            ranks = {i: _prefer_rank(units[i].path, opts.prefer) for i in pool}
            top = min(ranks.values())
            if top < len(opts.prefer):
                pool = [i for i in pool if ranks[i] == top]
                if len(pool) == 1:
                    return pool[0], C.WHY_PREFER
        if side:
            ranks2 = {i: side.get(os.path.dirname(units[i].path), 1 << 30) for i in pool}
            top2 = min(ranks2.values())
            if top2 < (1 << 30) and sum(1 for v in ranks2.values() if v == top2) < len(pool):
                pool = [i for i in pool if ranks2[i] == top2]
                if len(pool) == 1:
                    return pool[0], C.WHY_FOLDER
        pick = min(pool, key=lambda i: _rule_key(units[i], opts.rule))
        return pick, C.WHY_RULE

    vols = sorted({u.vol for u in units})
    if opts.per_volume and len(vols) > 1:
        for v in vols:
            members = [i for i, u in enumerate(units) if u.vol == v]
            if any(kept[i] for i in members):
                continue
            b = best(members)
            if b is not None:
                i, why = b
                out[i] = (C.ROLE_KEEP, why if why != C.WHY_RULE or v == vols[0] else C.WHY_VOLUME)
                kept[i] = True
    if not any(kept):
        b = best(list(range(n)))
        if b is not None:
            out[b[0]] = (C.ROLE_KEEP, b[1])
            kept[b[0]] = True
    return out


def folder_sides(
    pairs: dict[tuple[str, str], int], dirs_info: dict[str, tuple[int, int]], opts: KeepOptions
) -> dict[tuple[str, str], str]:
    """For each pair of folders holding two or more two-copy groups, the folder that keeps.

    ``dirs_info``: folder -> (oldest birth time of its members, newest mtime) in the groups."""
    out: dict[tuple[str, str], str] = {}
    for (a, b), n in pairs.items():
        if n < 2:
            continue
        if opts.prefer:
            ra, rb = _prefer_rank(a + os.sep + "x", opts.prefer), _prefer_rank(b + os.sep + "x", opts.prefer)
            if ra != rb:
                out[(a, b)] = a if ra < rb else b
                continue
        ia, ib = dirs_info.get(a, (0, 0)), dirs_info.get(b, (0, 0))
        if opts.rule == "newest":
            ka, kb = -ia[1], -ib[1]
        elif opts.rule == "shortest":
            ka, kb = len(a), len(b)
        else:
            ka, kb = ia[0], ib[0]
        if ka == kb:
            ka, kb = len(a), len(b)
        if ka == kb:
            out[(a, b)] = min(a, b)
        else:
            out[(a, b)] = a if ka < kb else b
    return out
