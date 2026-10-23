"""The last check before an extra copy is deleted (docs/dupes.md §8.5).

A ``dupes`` basket item carries a :class:`Guard` per member: the member's identity and stamps as
verified, and its group's keepers with theirs. Right before executing (after the basket's own
checks: missing, protected, held open, names reaching another item), :func:`check` looks again:

- the member must be the file that was verified (identity, size, both modification times and,
  where there is one, the USN), else ``changed``;
- at least one keeper must be intact: it exists, is the file that was verified, is not the
  member itself (names swapped), is not to be deleted by this execution or anything else in the
  basket, and (with the lock check on) is not held open by another program. If none is:
  ``last_copy`` when every keeper is to be deleted too, ``keeper_busy`` when one is held open,
  ``keeper_changed`` otherwise;
- optionally (``recheck_content``) both are read again, with the job's reads (access times kept,
  any name the OS takes, stamps checked around the read), and must still be identical.

"To be deleted" is decided by identity as well as by path: the same folder reached by two paths
(a symlinked or bind-mounted folder, SUBST, an 8.3 name, a firmlink) may have been searched twice
with different keepers; a keeper whose identity (volume, file id) is any extra copy about to go, or
whose folder (or one above) is, by its identity, a folder about to go, counts as lost.

Any failing member is left alone; the others of the item go ahead.
"""

from __future__ import annotations

import os
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass

from ..i18n import Texts
from . import codes as C

TEXT = Texts(
    {
        "changed": "核对之后这个文件变过，没有动",
        "keeper_changed": "保留的那份变了或不在了，这一组没有动",
        "last_copy": "这一组保留的那份也在要删的东西里，没有动",
        "keeper_busy": "保留的那份正被别的程序打开（可能在改写），这一组没有动",
    }
)


@dataclass(frozen=True, slots=True)
class Stamped:
    path: str
    dev: int
    fid_lo: int
    fid_hi: int
    size: int
    mtime: int
    change: int
    usn: int = 0

    def same(self, row: Mapping[str, int]) -> bool:
        if int(row["reason"]) != C.OK:
            return False
        now = (
            int(row["dev"]),
            int(row["fid_lo"]),
            int(row["fid_hi"]),
            int(row["size"]),
            int(row["mtime"]),
            int(row["change"]),
        )
        if now != (self.dev, self.fid_lo, self.fid_hi, self.size, self.mtime, self.change):
            return False
        usn = int(row.get("usn", 0))
        return not (self.usn and usn and usn != self.usn)

    def same_file(self, other: Stamped) -> bool:
        return (self.dev, self.fid_lo, self.fid_hi) == (other.dev, other.fid_lo, other.fid_hi)

    @property
    def ident(self) -> tuple[int, int, int]:
        return (self.dev, self.fid_lo, self.fid_hi)


@dataclass(frozen=True, slots=True)
class Elsewhere:
    """What the rest of the basket deletes, besides this execution's paths."""

    covers: Callable[[str], bool]  # an item covers this path (it or a folder above)
    members: Callable[[], set[tuple[int, int, int]]]  # identities of extra copies in dupes items
    plain: Callable[[], list[str]]  # paths of the other items


@dataclass(frozen=True, slots=True)
class Guard:
    member: Stamped
    keepers: tuple[Stamped, ...]
    ops: str = "native"  # which reads stamped it: the same are used to check
    recheck_content: bool = False
    keep_atime: str = "skip"  # recheck_content: as the job read (skip a file whose access time moves)


def _norm(p: str) -> str:
    return os.path.normcase(os.path.normpath(p))


def _below_any(path: str, doomed: set[str]) -> bool:
    key = _norm(path)
    while True:
        if key in doomed:
            return True
        up = os.path.dirname(key)
        if up == key:
            return False
        key = up


def _digests(ops_kind: str, files: Sequence[Stamped], keep_atime: str = "skip") -> list[bytes | None]:
    """Full digests of ``files`` with the job's reads: the access time is kept where the OS
    allows, names are passed as the OS takes them, and a file whose identity or stamps differ
    from the guard's (before or after the read) gives None."""
    import numpy as np

    from .reads import FallbackOps, default_ops

    ops = default_ops() if ops_kind == "native" else FallbackOps()
    out: list[bytes | None] = []
    reader = ops.reader(unbuffered=False, keep_atime=keep_atime, small_whole=0, lease_s=0.0, chunk=C.CHUNK)
    try:
        for i, f in enumerate(files):
            keys = np.array([i], dtype=np.uint64)
            res = reader.probe(
                keys,
                [f.path],
                np.array([f.dev], dtype=np.uint64),
                np.array([f.fid_lo], dtype=np.uint64),
                np.array([f.fid_hi], dtype=np.uint64),
                np.array([f.size], dtype=np.int64),
                np.zeros(1, dtype=np.uint32),
                True,
            )
            if int(res["reason"][0]) != C.OK or (int(res["mtime"][0]), int(res["change"][0])) != (
                f.mtime,
                f.change,
            ):
                reader.drop(None)
                out.append(None)
                continue
            if int(res["has_full"][0]):
                out.append(bytes(res["full"][0:32]))
                reader.drop(None)
                continue
            digest: bytes | None = None
            while True:
                st = reader.step(keys, np.array([f.size], dtype=np.uint64), 64 << 20, 1.0)
                if int(st["reason"][0]) != C.OK:
                    break
                if int(st["at"][0]) == 2:
                    digest = bytes(st["fin"][0:32])
                    break
            reader.drop(None)
            out.append(digest)
    finally:
        reader.drop(None)
    return out


def _folder_ids(paths: Sequence[str]) -> set[tuple[int, int]]:
    """(st_dev, st_ino) of each path as it is (a link is itself, not what it points to)."""
    out: set[tuple[int, int]] = set()
    for p in paths:
        try:
            st = os.lstat(p)
        except (OSError, ValueError):
            continue
        out.add((st.st_dev, st.st_ino))
    return out


class _Reach:
    """Whether a path, or a folder above it, is by identity one of ``ids``."""

    def __init__(self, ids: set[tuple[int, int]]) -> None:
        self.ids = ids
        self._seen: dict[str, bool] = {}

    def lost(self, path: str) -> bool:
        if not self.ids:
            return False
        chain: list[str] = []
        key = os.path.abspath(path)
        hit = False
        while True:
            known = self._seen.get(key)
            if known is not None:
                hit = known
                break
            chain.append(key)
            try:
                st = os.stat(key)  # followed: the folder this path really goes through
                if (st.st_dev, st.st_ino) in self.ids:
                    hit = True
                    break
            except (OSError, ValueError):
                pass
            up = os.path.dirname(key)
            if up == key:
                break
            key = up
        for k in chain:
            self._seen[k] = hit
        return hit


def check(
    paths: Sequence[str],
    guards: Mapping[str, Guard],
    *,
    also_doomed: Callable[[str], bool] | Elsewhere | None = None,
    lockers: Callable[[list[str]], Mapping[str, list[str]]] | None = None,
) -> dict[int, tuple[str, str]]:
    """Per index of ``paths`` that has a guard and fails it: (OpCode, message).

    ``also_doomed``: what else the basket deletes (an :class:`Elsewhere`, or only whether a path is
    covered)."""
    from .reads import FallbackOps, default_ops

    idx = [i for i, p in enumerate(paths) if p in guards]
    if not idx:
        return {}
    doomed = {_norm(p) for p in paths}
    covers = also_doomed.covers if isinstance(also_doomed, Elsewhere) else also_doomed
    # identities of every extra copy to go: this execution's, the rest of the basket's
    gone_ids: set[tuple[int, int, int]] = {guards[paths[i]].member.ident for i in idx}
    plain = [p for p in paths if p not in guards]
    if isinstance(also_doomed, Elsewhere):
        gone_ids |= also_doomed.members()
        plain += also_doomed.plain()
    reach = _Reach(_folder_ids(plain))
    ops_by = {"native": default_ops(), "python": FallbackOps()}
    out: dict[int, tuple[str, str]] = {}
    # one metadata query per name: members, then every keeper they name
    want: dict[str, list[str]] = {}
    for i in idx:
        g = guards[paths[i]]
        names = want.setdefault(g.ops, [])
        names.append(g.member.path)
        names.extend(k.path for k in g.keepers)
    now: dict[tuple[str, str], dict[str, int]] = {}
    for kind, names in want.items():
        uniq = list(dict.fromkeys(names))
        res = ops_by.get(kind, ops_by["python"]).identity(uniq, True)
        for j, p in enumerate(uniq):
            now[(kind, p)] = {k: int(v[j]) for k, v in res.items()}
    for i in idx:  # what the member paths reach now goes too
        g = guards[paths[i]]
        row = now[(g.ops, g.member.path)]
        if int(row["reason"]) == C.OK:
            gone_ids.add((int(row["dev"]), int(row["fid_lo"]), int(row["fid_hi"])))
    busy: Mapping[str, list[str]] = {}
    if lockers is not None:
        keeper_paths = list(dict.fromkeys(k.path for i in idx for k in guards[paths[i]].keepers))
        busy = lockers(keeper_paths)
    for i in idx:
        g = guards[paths[i]]
        if not g.member.same(now[(g.ops, g.member.path)]):
            out[i] = ("changed", TEXT["changed"])
            continue
        reasons: list[str] = []
        ok = False
        for k in g.keepers:
            if k.same_file(g.member):
                reasons.append("keeper_changed")  # names swapped: the keeper is this very file
                continue
            if (
                _below_any(k.path, doomed)
                or (covers is not None and covers(k.path))
                or k.ident in gone_ids
                or reach.lost(k.path)
            ):
                reasons.append("last_copy")
                continue
            if not k.same(now[(g.ops, k.path)]):
                reasons.append("keeper_changed")
                continue
            if busy.get(k.path):
                reasons.append("keeper_busy")
                continue
            if g.recheck_content:
                a, b = _digests(g.ops, [g.member, k], g.keep_atime)
                if a is None or a != b:
                    reasons.append("keeper_changed")
                    continue
            ok = True
            break
        if ok:
            continue
        code = (
            "last_copy"
            if reasons and all(r == "last_copy" for r in reasons)
            else ("keeper_busy" if "keeper_busy" in reasons else "keeper_changed")
        )
        out[i] = (code, TEXT[code])
    return out
