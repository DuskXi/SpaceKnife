"""Glob matching over every name of a :class:`NodeStore` without a Python object per node.

The names are searched where the store keeps them: NUL-separated runs (``\\0name\\0name…``; NUL
never occurs in a file name) in its name heap, viewed without copying. A glob becomes one anchored
bytes regex that ``re`` runs over a whole segment in C; match offsets map back to node ids with
``searchsorted``. Segments are appended as the store
grows (scans in progress), so earlier work is reused. Case-insensitivity uses simple per-character
case variants (the same rule on every platform).

One index serves every thread of its store (the scan finishing, view refilters, the palette).
Per name it keeps 12 bytes (separator offset and character set); ASCII-lower-cased copies of the
runs are a cache shared by all indexes and bounded by ``LOWER_CACHE_BYTES`` (beyond it they are
made per query: a full substring search then costs about twice as much).
"""

from __future__ import annotations

import re
import threading
import weakref
from collections import OrderedDict
from typing import Any

import numpy as np

from spaceknife_protocol.filter_syntax import glob_parts

from ..store.nodes import NAME_ERRORS, NodeStore

SEGMENT_NODES = 1 << 18  # bounds the GIL hold of one regex scan
# Atomic: once a whole UTF-8 sequence matched, "??" cannot backtrack into the stray-byte fallback
# and split it. (A normalised glob never puts "?" right after "*", so "?" starts on a character.)
_ONE_CHAR = (
    rb"(?>[\x01-\x7f]|[\xc2-\xdf][\x80-\xbf]|[\xe0-\xef][\x80-\xbf]{2}|[\xf0-\xf4][\x80-\xbf]{3}|[\x80-\xff])"
)
_CACHE_PATTERNS = 16
_CACHE_BYTES = 256 << 20
_EMPTY = np.empty(0, dtype=np.int64)
Segment = tuple[int, np.ndarray, np.ndarray]  # (first id, \\0name run (uint8 view), NUL offsets)
_LOWER_TABLE = np.arange(256, dtype=np.uint8)
_LOWER_TABLE[65:91] += 32  # ASCII A-Z -> a-z, every other byte unchanged
LOWER_CACHE_BYTES = 256 << 20  # lower-cased runs kept across all indexes


class _LowerBudget:
    """Bytes of lower-cased runs cached by all indexes together."""

    def __init__(self) -> None:
        self.used = 0
        self._lock = threading.Lock()

    def take(self, nbytes: int) -> bool:
        with self._lock:
            if self.used + nbytes > LOWER_CACHE_BYTES:
                return False
            self.used += nbytes
            return True

    def give(self, holder: list[int]) -> None:
        with self._lock:
            self.used -= holder[0]
            holder[0] = 0


_LOWER_BUDGET = _LowerBudget()


def _signature_bits() -> np.ndarray:
    """Byte -> one bit of a 64-bit character set: a-z, 0-9 and . _ - space get their own bit, other
    ASCII bytes share 22 bits, every non-ASCII byte shares one. NUL contributes nothing."""
    bits = np.zeros(256, dtype=np.uint64)
    for b in range(1, 256):
        ch = chr(b)
        if "a" <= ch <= "z":
            bit = b - 97
        elif "0" <= ch <= "9":
            bit = 26 + b - 48
        elif ch in "._- ":
            bit = 36 + "._- ".index(ch)
        elif b < 128:
            bit = 40 + b % 22
        else:
            bit = 62
        bits[b] = np.uint64(1) << np.uint64(bit)
    return bits


_SIG_BITS = _signature_bits()
_SIG_BITS_CI = _SIG_BITS[_LOWER_TABLE]  # upper-case ASCII letters share their lower-case bit


def _char_regex(ch: str) -> bytes:
    variants = []
    for v in (ch, ch.lower(), ch.upper(), ch.title()):
        if len(v) == 1 and v not in variants:
            variants.append(v)
    encoded = [re.escape(v.encode("utf-8", NAME_ERRORS)) for v in variants]
    if len(encoded) == 1:
        return encoded[0]
    if all(len(e) == 1 for e in encoded):
        return b"[" + b"".join(encoded) + b"]"
    return b"(?:" + b"|".join(encoded) + b")"


def glob_regex(pattern: str) -> re.Pattern[bytes]:
    """Anchored regex matching ``\\0<name>`` for names that match the normalised glob."""
    out = [b"\x00"]
    for kind, text in glob_parts(pattern):
        if kind == "any":
            out.append(rb"[^\x00]*")
        elif kind == "one":
            out.append(_ONE_CHAR)
        else:
            out.extend(_char_regex(ch) for ch in text)
    out.append(rb"(?=\x00|\Z)")
    return re.compile(b"".join(out), re.DOTALL)


def glob_regex_lower(pattern: str) -> re.Pattern[bytes]:
    """:func:`glob_regex` for an ASCII glob, run over ASCII-lower-cased names (plain literals)."""
    out = [b"\x00"]
    for kind, text in glob_parts(pattern):
        if kind == "any":
            out.append(rb"[^\x00]*")
        elif kind == "one":
            out.append(_ONE_CHAR)
        else:
            out.append(re.escape(text.lower().encode("ascii")))
    out.append(rb"(?=\x00|\Z)")
    return re.compile(b"".join(out), re.DOTALL)


def glob_str_regex(pattern: str) -> re.Pattern[str]:
    """The same glob for one decoded name (used on small candidate sets)."""
    out = []
    for kind, text in glob_parts(pattern):
        out.append(".*" if kind == "any" else "." if kind == "one" else re.escape(text))
    return re.compile("".join(out), re.DOTALL | re.IGNORECASE)


class NameIndex:
    """NUL-joined name segments of one store plus a small per-pattern result cache.

    Thread-safe. Segments are append-only: a query works on a snapshot of the segment list and
    caches its result as complete up to the end of the last segment it scanned. ``_lock`` guards
    the list, ``n`` and the cache and is never held during a scan; ``_build`` lets one thread at a
    time add segments.
    """

    def __init__(self, store: NodeStore) -> None:
        # weak: the index is the value of a WeakKeyDictionary keyed by this store, and a strong
        # reference back to the key would keep a closed scan (store and index) alive for good
        self._store = weakref.ref(store)
        self.n = 0
        self.segments: list[Segment] = []
        self._lock = threading.Lock()
        self._build = threading.Lock()
        self._cache: OrderedDict[str, tuple[int, Any]] = OrderedDict()  # key -> (complete below, result)
        self._lower: dict[int, np.ndarray] = {}  # segment index -> ASCII-lower-cased run (budgeted cache)
        self._lower_bytes = [0]  # this index's share of the shared budget, returned when it dies
        weakref.finalize(self, _LOWER_BUDGET.give, self._lower_bytes)
        self._sig: dict[int, np.ndarray] = {}  # segment index -> per-name character sets (palette search)

    def update(self, n: int) -> None:
        """Index nodes up to ``n`` (a snapshot of ``len(store)``)."""
        if self.n >= n:
            return
        with self._build:
            while self.n < n:
                a, b = self.n, min(n, self.n + SEGMENT_NODES)
                stop, run, sep = self.store.names_run(a, b)  # may stop early: at a heap chunk's end
                with self._lock:
                    self.segments.append((a, run, sep))
                    self.n = stop

    def _snapshot(self, n: int) -> tuple[list[Segment], int]:
        """Index up to ``n``; the segments so far and the node count they cover (``>= n``)."""
        self.update(n)
        with self._lock:
            return self.segments[:], self.n

    def _cached(self, key: str, *, touch: bool = True) -> tuple[int, Any] | None:
        with self._lock:
            hit = self._cache.get(key)
            if hit is not None and touch:
                self._cache.move_to_end(key)
            return hit

    def _remember(self, key: str, done: int, result: Any) -> None:
        """Cache ``result`` (complete for ids below ``done``) unless a more complete one is there."""
        with self._lock:
            old = self._cache.get(key)
            if old is None or old[0] < done:
                self._cache[key] = (done, result)
            self._cache.move_to_end(key)
            # bounded by count and by bytes (one-letter searches over millions of names hit
            # millions of ids); the newest two stay, typing refines the previous query
            while len(self._cache) > 2 and (
                len(self._cache) > _CACHE_PATTERNS
                or sum(_nbytes(r) for _, r in self._cache.values()) > _CACHE_BYTES
            ):
                self._cache.popitem(last=False)

    def memory_bytes(self) -> int:
        """Bytes held by the index: separator offsets, cached lower-cased runs, signatures and results
        (the runs themselves are views of the store's names)."""
        with self._lock:
            total = sum(s[2].nbytes for s in self.segments)
            total += sum(b.nbytes for b in self._lower.values())
            total += sum(a.nbytes for a in self._sig.values())
            return total + sum(_nbytes(r) for _, r in self._cache.values())

    @property
    def store(self) -> NodeStore:
        store = self._store()
        if store is None:
            raise ReferenceError("the node store of this name index is gone")
        return store

    def _lowered(self, k: int) -> np.ndarray:
        """Run ``k`` with ASCII letters lower-cased: cached while the shared budget allows."""
        hit = self._lower.get(k)
        if hit is None:
            hit = _LOWER_TABLE[self.segments[k][1]]
            if _LOWER_BUDGET.take(hit.nbytes):
                with self._lock:
                    self._lower[k] = hit
                    self._lower_bytes[0] += hit.nbytes
        return hit

    def match(self, pattern: str, n: int) -> np.ndarray:
        """Sorted ids (< ``n``) of nodes whose name matches the normalised glob ``pattern``."""
        segments, covered = self._snapshot(n)
        cached = self._cached(pattern)
        done, found = cached if cached is not None else (0, _EMPTY)
        if done < n:
            parts = glob_parts(pattern)
            # "*text*": a plain substring search (no backtracking); a hit can never span two names
            contains = len(parts) == 3 and parts[0] == parts[2] == ("any", "") and parts[1][0] == "lit"
            # ASCII globs run as plain literals over an ASCII-lower-cased copy: same case rule
            # (an ASCII letter's only single-character case variants are its ASCII pair), much faster
            lower = pattern.isascii()
            if contains:
                lit = parts[1][1]
                if lower:
                    regex = re.compile(re.escape(lit.lower().encode("ascii")))
                else:
                    regex = re.compile(b"".join(_char_regex(ch) for ch in lit), re.DOTALL)
            else:
                regex = glob_regex_lower(pattern) if lower else glob_regex(pattern)
            new = [found]
            for k, (first, joined, sep) in enumerate(segments):
                seg_end = first + len(sep)
                if seg_end <= done:
                    continue
                text = self._lowered(k) if lower else joined
                pos = np.fromiter((m.start() for m in regex.finditer(text)), dtype=np.int64)
                if contains:
                    ids = np.unique(np.searchsorted(sep, pos, side="right") - 1) + first
                else:
                    ids = np.searchsorted(sep, pos) + first
                new.append(ids[ids >= done])
            found = np.concatenate(new)
            self._remember(pattern, covered, found)
        return found[found < n]

    def search(self, key: str, regex: re.Pattern[bytes], *, lower: bool, n: int) -> np.ndarray:
        """Sorted ids (< ``n``) of nodes whose name contains a match of ``regex``.

        ``regex`` must never match a NUL (names are NUL-separated); with ``lower`` it runs over the
        ASCII-lower-cased copy. Results are cached under ``key`` like :meth:`match`.
        """
        segments, covered = self._snapshot(n)
        ckey = "\x00re:" + key
        cached = self._cached(ckey)
        done, found = cached if cached is not None else (0, _EMPTY)
        if done < n:
            new = [found]
            for k, (first, joined, sep) in enumerate(segments):
                if first + len(sep) <= done:
                    continue
                text = self._lowered(k) if lower else joined
                pos = np.fromiter((m.start() for m in regex.finditer(text)), dtype=np.int64)
                ids = np.unique(np.searchsorted(sep, pos, side="right") - 1) + first
                new.append(ids[ids >= done])
            found = np.concatenate(new)
            self._remember(ckey, covered, found)
        return found[found < n]

    def ascii_tiers(self, query: str, n: int, *, scattered: bool = True) -> tuple[np.ndarray, ...]:
        """Ids (< ``n``) whose name, ASCII-case-insensitively, equals / starts with / contains
        ``query`` / holds its letters in order — four disjoint sorted tiers (palette, UI-W4-28).

        Pure numpy over the lower-cased segments: candidate positions of the first byte are
        narrowed byte by byte, so no Python object is made per hit. While the user types on, the
        hits of the previous, shorter query are a superset, so only those names are searched.
        ``query`` must be ASCII.
        """
        ql = query.lower()
        q = ql.encode("ascii")
        segments, covered = self._snapshot(n)
        ckey = _tier_key(ql, scattered)
        cached = self._cached(ckey)
        tiers: tuple[np.ndarray, ...]
        if cached is not None and cached[0] >= n:
            tiers = cached[1]
        else:
            tiers = self._refined(ql, q, scattered, segments, covered) or self._scan_tiers(
                q, scattered, cached, segments
            )
            self._remember(ckey, covered, tiers)
        return tuple(t[t < n] for t in tiers)

    def _signatures(self, k: int) -> np.ndarray:
        """Per name of segment ``k``: a 64-bit set of the (lower-cased) characters it contains."""
        hit = self._sig.get(k)
        if hit is None:
            _, arr, sep = self.segments[k]
            hit = self._sig[k] = (
                np.bitwise_or.reduceat(_SIG_BITS_CI[arr], sep) if len(sep) else np.zeros(0, np.uint64)
            )
        return hit

    def _scan_tiers(
        self, q: bytes, scattered: bool, cached: tuple[int, Any] | None, segments: list[Segment]
    ) -> tuple[np.ndarray, ...]:
        done = cached[0] if cached is not None else 0
        parts: list[list[np.ndarray]] = [[t] for t in cached[1]] if cached is not None else [[], [], [], []]
        need = np.bitwise_or.reduce(_SIG_BITS[np.frombuffer(q, dtype=np.uint8)])
        for k, (first, _, sep) in enumerate(segments):
            if first + len(sep) <= done:
                continue
            arr = self._lowered(k)
            # names lacking any character of the query cannot match in any tier
            local = np.flatnonzero((self._signatures(k) & need) == need)
            for tier, ids in enumerate(_tiers_among(arr, sep, local, q, scattered)):
                ids = ids + first
                parts[tier].append(ids[ids >= done])
        return tuple(np.concatenate(p) if p else np.empty(0, dtype=np.int64) for p in parts)

    def _refined(
        self, ql: str, q: bytes, scattered: bool, segments: list[Segment], covered: int
    ) -> tuple[np.ndarray, ...] | None:
        """Typing on: search only the hits of the longest shorter query already answered."""
        for cut in range(len(ql) - 1, 0, -1):
            prev = self._cached(_tier_key(ql[:cut], scattered), touch=False)
            if prev is None or prev[0] < covered:
                continue
            cand = np.sort(np.concatenate(prev[1]))
            cand = cand[cand < covered]  # another thread may have answered it over more segments
            firsts = np.array([seg[0] for seg in segments], dtype=np.int64)
            seg_of = np.searchsorted(firsts, cand, side="right") - 1
            parts: list[list[np.ndarray]] = [[], [], [], []]
            for k in np.unique(seg_of).tolist():
                first, _, sep = segments[k]
                arr = self._lowered(k)
                for tier, ids in enumerate(_tiers_among(arr, sep, cand[seg_of == k] - first, q, scattered)):
                    parts[tier].append(ids + first)
            return tuple(np.concatenate(p) if p else np.empty(0, dtype=np.int64) for p in parts)
        return None


def _tiers_among(
    arr: np.ndarray, sep: np.ndarray, local: np.ndarray, q: bytes, scattered: bool
) -> list[np.ndarray]:
    """:func:`_segment_tiers` restricted to the names ``local`` (sorted indices into ``sep``)."""
    if local.size * 3 > len(sep):  # most of the segment: scanning it whole is cheaper
        tiers = _segment_tiers(arr, sep, q, scattered)
        keep = np.zeros(len(sep), dtype=bool)
        keep[local] = True
        return [t[keep[t]] for t in tiers]
    if not local.size:
        return [local for _ in range(4)]
    starts = sep[local]  # each name's leading NUL
    ln = np.append(sep, len(arr))[local + 1] - starts
    offsets = np.cumsum(ln) - ln
    compact = arr[np.repeat(starts - offsets, ln) + np.arange(int(ln.sum()))]
    return [local[t] for t in _segment_tiers(compact, offsets, q, scattered)]


def _tier_key(ql: str, scattered: bool) -> str:
    return f"\x00tiers:{int(scattered)}:{ql}"


def _runs(ids: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Distinct values of a sorted array and the index of each one's first occurrence."""
    if not ids.size:
        return ids, ids
    first = np.flatnonzero(np.concatenate(([True], ids[1:] != ids[:-1])))
    return ids[first], first


def _segment_tiers(arr: np.ndarray, sep: np.ndarray, q: bytes, scattered: bool) -> list[np.ndarray]:
    size = len(arr)
    starts = sep + 1
    ends = np.append(sep[1:], size)
    k = len(q)
    # contiguous occurrences: positions of q[0], narrowed by the following bytes (NUL never matches)
    pos = np.flatnonzero(arr == q[0])
    for j in range(1, k):
        pos = pos[pos + j < size]
        pos = pos[arr[pos + j] == q[j]]
    ids = np.searchsorted(sep, pos, side="right") - 1
    sub_ids, first = _runs(ids)
    lead = pos[first] == starts[sub_ids]
    exact = lead & (ends[sub_ids] - starts[sub_ids] == k)
    tiers = [sub_ids[exact], sub_ids[lead & ~exact], sub_ids[~lead]]
    if not scattered or k < 2:
        tiers.append(np.empty(0, dtype=np.int64))
        return tiers
    # scattered: earliest position of each next byte after the previous one, inside the same name
    p0 = np.flatnonzero(arr == q[0])
    cand, at = _runs(np.searchsorted(sep, p0, side="right") - 1)
    cur = p0[at]
    for j in range(1, k):
        pj = np.flatnonzero(arr == q[j])
        if not pj.size:
            cand = cand[:0]
            break
        idx = np.searchsorted(pj, cur, side="right")
        ok = idx < pj.size
        nxt = pj[np.minimum(idx, pj.size - 1)]
        ok &= nxt < ends[cand]
        cand, cur = cand[ok], nxt[ok]
    tiers.append(np.setdiff1d(cand, sub_ids, assume_unique=True))
    return tiers


def char_regex(ch: str) -> bytes:
    """Bytes regex for one character in any of its single-character case variants."""
    return _char_regex(ch)


_INDEXES: weakref.WeakKeyDictionary[NodeStore, NameIndex] = weakref.WeakKeyDictionary()
_INDEXES_LOCK = threading.Lock()


def name_index(store: NodeStore) -> NameIndex:
    with _INDEXES_LOCK:  # one index per store, whichever thread asks first
        index = _INDEXES.get(store)
        if index is None:
            index = _INDEXES[store] = NameIndex(store)
        return index


def forget(store: NodeStore) -> None:
    """Drop the store's index (its content was replaced): the next search builds a new one."""
    with _INDEXES_LOCK:
        _INDEXES.pop(store, None)


def existing_index(store: NodeStore) -> NameIndex | None:
    """The store's index if one was built (diagnostics: never builds one)."""
    with _INDEXES_LOCK:
        return _INDEXES.get(store)


def _nbytes(result: Any) -> int:
    if isinstance(result, np.ndarray):
        return int(result.nbytes)
    if isinstance(result, tuple | list):
        return sum(_nbytes(r) for r in result)
    return 0
