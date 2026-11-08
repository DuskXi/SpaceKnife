"""Fixture trees from fixtures/trees.json (the Rust replayer builds the same trees)."""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import time
from collections.abc import Iterable
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent.parent
DAY = 86400.0
# where the runs' folders go instead of the system's temp folder (README.md, "Directory order")
PARITY_TMP = "SPACEKNIFE_PARITY_TMP"


def specs() -> dict[str, Any]:
    return json.loads((HERE / "fixtures" / "trees.json").read_text(encoding="utf-8"))


def expand(spec: dict[str, Any]) -> list[dict[str, Any]]:
    """File entries with `range` expanded: `{i}` is the index, `{i3}` the index padded to 3."""
    out: list[dict[str, Any]] = []
    for f in spec.get("files", []):
        if "range" in f:
            for i in range(int(f["range"])):
                path = f["path"].replace("{i3}", f"{i:03d}").replace("{i}", str(i))
                out.append({**{k: v for k, v in f.items() if k not in ("range", "step")}, "path": path,
                            "size": int(f["size"]) + int(f.get("step", 0)) * i})
        else:
            out.append(f)
    return out


def _set_times_windows(path: Path, created: float | None, accessed: float, modified: float) -> None:
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined]
    k32.CreateFileW.restype = wintypes.HANDLE
    # FILE_WRITE_ATTRIBUTES, share all, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS (folders too)
    handle = k32.CreateFileW(str(path), 0x100, 0x7, None, 3, 0x02000000, None)
    if handle in (None, wintypes.HANDLE(-1).value):
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined]

    def ft(ts: float) -> wintypes.FILETIME:
        v = int((ts + 11644473600) * 10_000_000)
        return wintypes.FILETIME(v & 0xFFFFFFFF, v >> 32)

    c, a, m = ft(created if created is not None else modified), ft(accessed), ft(modified)
    ok = k32.SetFileTime(handle, ctypes.byref(c) if created is not None else None, ctypes.byref(a), ctypes.byref(m))
    k32.CloseHandle(handle)
    if not ok:
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined]


def _set_attrs_windows(path: Path, f: dict[str, Any]) -> None:
    import ctypes

    attrs = 0x20  # ARCHIVE
    if f.get("hidden"):
        attrs |= 0x2
    if f.get("readonly"):
        attrs |= 0x1
    if f.get("system"):
        attrs |= 0x4
    ctypes.WinDLL("kernel32").SetFileAttributesW(str(path), attrs)  # type: ignore[attr-defined]


def set_times(path: Path, created: float | None, accessed: float, modified: float) -> None:
    if sys.platform == "win32":
        _set_times_windows(path, created, accessed, modified)
    else:
        os.utime(path, (accessed, modified), follow_symlinks=False)


def build(root: Path, name: str, now: float | None = None) -> None:
    """Create fixture `name` at `root`. Times: `mtime` (absolute, every entry) or per file
    `created_days` / `modified_days` / `accessed_days` before now; Windows attributes from
    `hidden` / `readonly` / `system` (Windows only); contents: `fill` "x" (written) or "zero"
    (the size set, nothing written)."""
    spec = specs()[name]
    now = time.time() if now is None else now
    fixed = spec.get("mtime")
    root.mkdir(parents=True, exist_ok=True)
    files = expand(spec)
    for f in files:
        p = root / f["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        with open(p, "wb") as fh:
            if spec.get("fill", "x") == "zero":
                fh.truncate(int(f["size"]))
            else:
                fh.write(b"x" * int(f["size"]))
    for d in spec.get("dirs", []):
        (root / d).mkdir(parents=True, exist_ok=True)
    for f in files:
        p = root / f["path"]
        if "modified_days" in f:
            m = now - float(f["modified_days"]) * DAY
            a = now - float(f.get("accessed_days", f["modified_days"])) * DAY
            c = now - float(f["created_days"]) * DAY if "created_days" in f else None
            set_times(p, c, a, m)
        elif fixed is not None:
            set_times(p, float(fixed), float(fixed), float(fixed))
        if sys.platform == "win32" and any(f.get(k) for k in ("hidden", "readonly", "system")):
            _set_attrs_windows(p, f)
    if fixed is not None:
        # folders last, deepest first: making their children changed their times
        dirs = sorted({q for q in root.rglob("*") if q.is_dir()} | {root}, key=lambda q: -len(q.parts))
        for d in dirs:
            set_times(d, float(fixed), float(fixed), float(fixed))


# -------------------------------------------------------------------- directory order


def temp_parent() -> str | None:
    """SPACEKNIFE_PARITY_TMP: a folder on a file system that lists folders as the goldens' did,
    where `check` replays what differs where the system's temp folder lists them otherwise."""
    return os.environ.get(PARITY_TMP) or None


def listing(root: Path) -> dict[str, list[str]]:
    """How this file system lists a built tree: each folder ("" for the top, `a/b` below) with its
    entries in the order the system gives them (os.listdir: readdir, FindNextFileW), which is the
    order 0.3.4 keeps for entries of equal size (README.md, "Directory order")."""
    out: dict[str, list[str]] = {}
    stack = [(root, "")]
    while stack:
        d, rel = stack.pop()
        names = os.listdir(d)
        out[rel] = names
        for n in names:
            p = d / n
            if p.is_dir() and not p.is_symlink():
                stack.append((p, f"{rel}/{n}" if rel else n))
    return dict(sorted(out.items()))


def listings(names: Iterable[str], parent: str | None) -> dict[str, dict[str, list[str]]]:
    """`listing` of each tree, built in a temporary folder in `parent` (None: the system's temp
    folder) and removed."""
    out: dict[str, dict[str, list[str]]] = {}
    tmp = Path(tempfile.mkdtemp(prefix="sk-parity-listing-", dir=parent))
    try:
        for name in names:
            build(tmp / name, name)
            out[name] = listing(tmp / name)
    finally:
        def writable(func: Any, path: str, _: Any) -> None:
            os.chmod(path, 0o700)
            func(path)

        shutil.rmtree(tmp, onerror=writable)
    return out


def listing_difference(recorded: dict[str, list[str]] | None, actual: dict[str, list[str]]) -> str | None:
    """The first folder `actual` lists otherwise than `recorded` (a tree of golden/<platform>/
    listing.json), "." for the top; None when every folder is listed alike."""
    recorded = recorded or {}
    for folder in sorted(set(recorded) | set(actual)):
        if recorded.get(folder) != actual.get(folder):
            return folder or "."
    return None
