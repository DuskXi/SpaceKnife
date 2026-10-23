"""The persistent hash cache (docs/dupes.md §7).

One file per volume and algorithm: ``cache_dir()/dupes/v1/<key>-a<alg>.skh``, a 64-byte header
and a numpy structured array of :data:`ENTRY_DTYPE` (128 bytes) sorted by file id. Every scan and
job of that volume shares it; it holds no file names. The file is read whole and closed (so that
「清除缓存」 can delete it on Windows) and written at the end of a job (and every 60 s of a long
one) to a temporary file, fsynced and renamed, after merging what is on disk by then (the CLI may
have written meanwhile: the newer hash wins).

**Asymmetric trust** (§7.3): an entry whose size, mtime and change time match may *separate* a
file from others (its quick digest, its prefix digest at a checkpoint); it may *join* files only
when it is a full digest, not racy, of the same algorithm, on a volume of strong trust, and (where
the volume has a USN journal) its USN matches too. A stale entry used to separate can at worst
hide a duplicate; joining needs the bytes to be the ones that were hashed.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import struct
import threading
import time
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from .. import paths as app_paths

MAGIC = b"SKDH"
VERSION = 1
HEADER = 64
MAX_PER_VOLUME = 256 * 1024
MAX_AGE_DAYS = 90

E_FULL = 1
E_RACY = 2
E_PREFIX = 4
E_QUICK = 8

ENTRY_DTYPE = np.dtype(
    {
        "names": [
            "fid_lo",
            "fid_hi",
            "size",
            "mtime",
            "change",
            "usn",
            "hashed_at",
            "quick",
            "full",
            "p_off",
            "p_dig",
            "params",
            "flags",
            "last_seen",
        ],
        "formats": [
            "<u8",
            "<u8",
            "<i8",
            "<i8",
            "<i8",
            "<i8",
            "<i8",
            "<u8",
            "V32",
            "<u8",
            "V16",
            "<u2",
            "u1",
            "<u2",
        ],
        "offsets": [0, 8, 16, 24, 32, 40, 48, 56, 64, 96, 104, 120, 122, 124],
        "itemsize": 128,
    }
)
_HEAD = struct.Struct("<4sHBBB7xQq16s16x")  # magic, version, alg, params, trust, count, written, key hash


def cache_root() -> Path:
    return app_paths.cache_dir() / "dupes" / "v1"


def key_hash(key: bytes) -> bytes:
    return hashlib.blake2b(key, digest_size=16).digest()


def _keys(hi: np.ndarray, lo: np.ndarray) -> np.ndarray:
    k = np.empty((len(hi), 2), dtype=">u8")
    k[:, 0] = hi
    k[:, 1] = lo
    return k.view("S16").ravel()


def today() -> int:
    return int(time.time() // 86400) & 0xFFFF


@dataclass(slots=True)
class Stamp:
    size: int
    mtime: int
    change: int
    usn: int = 0


class VolumeCache:
    """The entries of one volume for one algorithm; lookups by (fid_hi, fid_lo)."""

    def __init__(self, path: Path, key: bytes, alg: int, params: int, strong: bool) -> None:
        self.path = path
        self.key = key_hash(key)
        self.alg = alg
        self.params = params
        self.strong = strong
        self.table = np.zeros(0, ENTRY_DTYPE)
        self._keys = np.zeros(0, "S16")
        self._new = np.zeros(0, ENTRY_DTYPE)  # entries learnt since the last save (grows by doubling)
        self._n_new = 0
        self._seen: set[int] = set()  # rows whose last_seen moves to today
        self._disk: tuple[int, int] | None = None  # (mtime_ns, size) of the file as last read or written
        self._lock = threading.Lock()
        self.loaded_bytes = 0
        self.written = 0.0

    @classmethod
    def load(cls, key: bytes, alg: int, params: int, strong: bool, root: Path | None = None) -> VolumeCache:
        root = root or cache_root()
        path = root / f"{key_hash(key).hex()}-a{alg}.skh"
        vc = cls(path, key, alg, params, strong)
        vc.table = vc._read()
        vc._keys = _keys(vc.table["fid_hi"], vc.table["fid_lo"])
        return vc

    def _read(self) -> np.ndarray:
        try:
            with open(self.path, "rb") as f:
                head = f.read(HEADER)
                if len(head) < HEADER:
                    return np.zeros(0, ENTRY_DTYPE)
                magic, version, alg, _params, _trust, count, _written, kh = _HEAD.unpack_from(head)
                if magic != MAGIC or version != VERSION or alg != self.alg or kh != self.key:
                    return np.zeros(0, ENTRY_DTYPE)
                data = np.fromfile(f, dtype=ENTRY_DTYPE, count=count)
                self.loaded_bytes = HEADER + data.nbytes
                st = os.fstat(f.fileno())
                self._disk = (st.st_mtime_ns, st.st_size)
        except (OSError, ValueError, struct.error):
            return np.zeros(0, ENTRY_DTYPE)
        if len(data) != count:
            return np.zeros(0, ENTRY_DTYPE)  # torn: rebuilt
        data = data[data["params"] == self.params] if params_differ(data, self.params) else data
        return np.sort(data, order=["fid_hi", "fid_lo"])

    def __len__(self) -> int:
        return len(self.table)

    def lookup(self, fid_hi: np.ndarray, fid_lo: np.ndarray) -> np.ndarray:
        """Row of each id in :attr:`table`, or -1."""
        q = _keys(np.asarray(fid_hi, "<u8"), np.asarray(fid_lo, "<u8"))
        if not len(self._keys):
            return np.full(len(q), -1, dtype=np.int64)
        pos = np.searchsorted(self._keys, q)
        pos = np.minimum(pos, len(self._keys) - 1)
        hit = self._keys[pos] == q
        return np.where(hit, pos, -1).astype(np.int64)

    def entry(self, row: int) -> np.void:
        e: np.void = self.table[row]
        return e

    def touch(self, rows: Iterable[int]) -> None:
        """Entries seen again: their last day moves to today (only those not seen today already,
        so a second run on the same day writes nothing for them)."""
        day = today()
        want = [int(r) for r in rows if r >= 0]
        if not want:
            return
        fresh = [r for r, d in zip(want, self.table["last_seen"][want].tolist(), strict=True) if d != day]
        if fresh:
            with self._lock:
                self._seen.update(fresh)

    def put(
        self,
        fid_hi: int,
        fid_lo: int,
        stamp: Stamp,
        *,
        quick: int | None = None,
        full: bytes | None = None,
        prefix: tuple[int, bytes] | None = None,
        racy: bool = False,
    ) -> None:
        """Record what was learnt about one file (replacing what was known of it)."""
        e = np.zeros((), ENTRY_DTYPE)
        e["fid_lo"], e["fid_hi"] = fid_lo, fid_hi
        e["size"], e["mtime"], e["change"], e["usn"] = stamp.size, stamp.mtime, stamp.change, stamp.usn
        e["hashed_at"] = time.time_ns()
        e["params"] = self.params
        e["last_seen"] = today()
        flags = 0
        if quick is not None:
            e["quick"] = quick
            flags |= E_QUICK
        if full is not None:
            e["full"] = np.void(full)
            flags |= E_FULL
        if prefix is not None:
            e["p_off"] = prefix[0]
            e["p_dig"] = np.void(prefix[1][:16].ljust(16, b"\0"))
            flags |= E_PREFIX
        if racy:
            flags |= E_RACY
        e["flags"] = flags
        with self._lock:
            if self._n_new >= len(self._new):
                grown = np.zeros(max(64, 2 * len(self._new)), ENTRY_DTYPE)
                grown[: self._n_new] = self._new[: self._n_new]
                self._new = grown
            self._new[self._n_new] = e  # a later one of the same file wins at save (hashed_at)
            self._n_new += 1

    @property
    def dirty(self) -> bool:
        return bool(self._n_new) or bool(self._seen)

    def save(self) -> int:
        """Merge with the file on disk, evict, write atomically. Returns the bytes written."""
        with self._lock:
            new = self._new[: self._n_new].copy()
            seen = sorted(self._seen)
            self._new = np.zeros(0, ENTRY_DTYPE)
            self._n_new = 0
            self._seen.clear()
        base = self.table
        if seen:
            base["last_seen"][np.asarray(seen, dtype=np.int64)] = today()  # ours: in place
        # the file is read again only when another job wrote it since (else ``base`` is it)
        parts = [base]
        try:
            st = os.stat(self.path)
            if self._disk != (st.st_mtime_ns, st.st_size):
                parts.insert(0, self._read())
        except OSError:
            pass
        if len(new):
            parts.append(new)
        allrows = np.concatenate(parts, dtype=ENTRY_DTYPE) if len(parts) > 1 else parts[0].copy()
        if len(allrows):
            # the newest hash of each file wins
            order = np.lexsort((-allrows["hashed_at"], allrows["fid_lo"], allrows["fid_hi"]))
            allrows = allrows[order]
            k = _keys(allrows["fid_hi"], allrows["fid_lo"])
            first = np.r_[True, k[1:] != k[:-1]]
            allrows = allrows[first]
            age = (today() - allrows["last_seen"].astype(np.int64)) % 65536
            allrows = allrows[age <= MAX_AGE_DAYS]
            if len(allrows) > MAX_PER_VOLUME:
                keep = np.argsort(-allrows["last_seen"].astype(np.int64), kind="stable")[:MAX_PER_VOLUME]
                allrows = np.sort(allrows[keep], order=["fid_hi", "fid_lo"])
        self.table = allrows
        self._keys = _keys(allrows["fid_hi"], allrows["fid_lo"])
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(f".tmp{os.getpid()}-{threading.get_ident()}")
        head = _HEAD.pack(
            MAGIC, VERSION, self.alg, self.params, int(self.strong), len(allrows), time.time_ns(), self.key
        )
        with open(tmp, "wb") as f:
            f.write(head.ljust(HEADER, b"\0"))
            allrows.tofile(f)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, self.path)
        with contextlib.suppress(OSError):
            st = os.stat(self.path)
            self._disk = (st.st_mtime_ns, st.st_size)
        self.written = time.monotonic()
        return HEADER + allrows.nbytes


def params_differ(data: np.ndarray, params: int) -> bool:
    return bool(len(data)) and bool((data["params"] != params).any())


def split_ok(e: np.void, st: Stamp) -> bool:
    """The entry describes this file as it is now: may be used to tell it apart from others."""
    if int(e["size"]) != st.size or int(e["mtime"]) != st.mtime or int(e["change"]) != st.change:
        return False
    eu = int(e["usn"])
    return not (eu and st.usn and eu != st.usn)


def join_ok(vc: VolumeCache, e: np.void, st: Stamp, *, want_usn: bool) -> bool:
    """The entry's full digest may be used to declare this file identical to another."""
    if not vc.strong or not split_ok(e, st):
        return False
    flags = int(e["flags"])
    if not flags & E_FULL or flags & E_RACY:
        return False
    if st.change == 0:
        return False  # no change time: cannot rule out a rewrite with the old mtime
    return not (want_usn and (int(e["usn"]) == 0 or int(e["usn"]) != st.usn))


# --------------------------------------------------------------------------- last.json


def last_path(root: Path | None = None) -> Path:
    return (root or cache_root()) / "last.json"


def read_last(root: Path | None = None) -> dict[str, dict[str, object]]:
    try:
        data = json.loads(last_path(root).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def write_last(entries: dict[str, dict[str, object]], root: Path | None = None) -> None:
    path = last_path(root)
    with contextlib.suppress(OSError):
        path.parent.mkdir(parents=True, exist_ok=True)
        data = read_last(root)
        data.update(entries)
        if len(data) > 200:
            data = dict(sorted(data.items(), key=lambda kv: str(kv[1].get("at", "")))[-200:])
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
        os.replace(tmp, path)
