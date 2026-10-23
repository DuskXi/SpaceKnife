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
- optionally (``recheck_content``) both are read again and must still be identical.

Any failing member is left alone; the others of the item go ahead.
"""

from __future__ import annotations

import hashlib
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


@dataclass(frozen=True, slots=True)
class Guard:
    member: Stamped
    keepers: tuple[Stamped, ...]
    ops: str = "native"  # which reads stamped it: the same are used to check
    recheck_content: bool = False


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


def _digest(path: str) -> bytes | None:
    h = hashlib.blake2b(digest_size=32)
    try:
        with open(path, "rb") as f:
            while chunk := f.read(1 << 20):
                h.update(chunk)
    except OSError:
        return None
    return h.digest()


def check(
    paths: Sequence[str],
    guards: Mapping[str, Guard],
    *,
    also_doomed: Callable[[str], bool] | None = None,
    lockers: Callable[[list[str]], Mapping[str, list[str]]] | None = None,
) -> dict[int, tuple[str, str]]:
    """Per index of ``paths`` that has a guard and fails it: (OpCode, message)."""
    from .reads import FallbackOps, default_ops

    idx = [i for i, p in enumerate(paths) if p in guards]
    if not idx:
        return {}
    doomed = {_norm(p) for p in paths}
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
            if _below_any(k.path, doomed) or (also_doomed is not None and also_doomed(k.path)):
                reasons.append("last_copy")
                continue
            if not k.same(now[(g.ops, k.path)]) or k.same_file(g.member):
                reasons.append("keeper_changed")
                continue
            if busy.get(k.path):
                reasons.append("keeper_busy")
                continue
            if g.recheck_content:
                a, b = _digest(g.member.path), _digest(k.path)
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
