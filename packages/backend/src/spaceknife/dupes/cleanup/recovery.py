"""The recovery area (docs/dupes.md §16.9.6): where originals of replaced copies wait.

It must be on the same volume as the original (a rename, never a copy; on btrfs the same
subvolume). Tried in order:

1. ``data_dir()/dupes/cleanup/recovery/<run>/`` when it is on that volume;
2. with a sandbox: ``<sandbox root>/.SpaceKnife-recovery/<run>/`` on that volume;
3. the top of the volume (Windows: the volume root, hidden; POSIX: up to where ``st_dev`` changes,
   ``.SpaceKnife-recovery-<uid>``, mode 0700) unless the setting turns it off or a sandbox is set;
4. the original's own folder, under a hidden name ``.~sk-<run6>-<seq>.orig`` (risk X02).

Names inside are ``<seq>-<original name>`` (people can put one back by hand); a ``README.txt``
says what this is, ``manifest.json`` (end of the run) maps numbers to the original paths.
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ...i18n import Texts
from .fsops import FsError, FsOps, Sandbox

AREA = ".SpaceKnife-recovery"

TEXT = Texts(
    {
        "readme": (
            "这个文件夹是 SpaceKnife 重复文件清理的恢复区。\n\n"
            "清理把一些副本换成了链接或块克隆；被换下来的原件暂时放在这里，名字是「序号-原来的名字」，"
            "manifest.json 里写着每一份原来在哪里。\n\n"
            "在 SpaceKnife 的「重复文件清理记录」里可以撤销这次清理（把原件放回原处），或者「清除恢复区」"
            "（核对之后永久删除这些原件，腾出空间）。也可以手动把某个文件改名放回原来的位置。\n\n"
            "请不要在这里放别的文件。\n"
        ),
    }
)


@dataclass(slots=True)
class Place:
    """Where one volume's originals go in a run."""

    kind: str  # data | sandbox | root | in_place
    path: str  # the folder ("" for in_place: each original's own folder)
    dev: int
    made: list[str] = field(default_factory=list)  # folders this run created (removed when empty)

    def name_for(self, seq: int, name: str, run_id: str) -> tuple[str, str]:
        """(folder, name) of an original in the recovery area."""
        if self.kind == "in_place":
            return "", f".~sk-{run_id[-6:]}-{seq}.orig"
        return self.path, f"{seq}-{name}"


def _dev_of(fs: FsOps, path: str) -> int | None:
    try:
        h = fs.open_dir(path)
    except (FsError, OSError):
        return None
    try:
        return fs.fstat(h).dev
    except (FsError, OSError):
        return None
    finally:
        fs.close(h)


def _top_of(path: str, dev: int) -> str:
    """The highest folder above ``path`` on the same device (a mount point or subvolume root)."""
    cur = os.path.abspath(path)
    while True:
        up = os.path.dirname(cur)
        if up == cur:
            return cur
        try:
            if os.stat(up).st_dev != dev:
                return cur
        except OSError:
            return cur
        cur = up


def _mkdirs(path: str, made: list[str], *, private: bool) -> bool:
    parts: list[str] = []
    cur = path
    while not os.path.isdir(cur):
        parts.append(cur)
        up = os.path.dirname(cur)
        if up == cur:
            break
        cur = up
    for p in reversed(parts):
        try:
            os.mkdir(p, 0o700 if private else 0o777)
            made.append(p)
        except FileExistsError:
            continue
        except OSError:
            return False
    return os.path.isdir(path)


def _hide(path: str) -> None:
    if sys.platform == "win32":  # pragma: no cover - Windows
        import ctypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
        attrs = k32.GetFileAttributesW(ctypes.c_wchar_p(path))
        if attrs != 0xFFFFFFFF:
            k32.SetFileAttributesW(ctypes.c_wchar_p(path), attrs | 0x2)


def place_for(
    fs: FsOps,
    run_id: str,
    folder: str,
    *,
    sandbox: Sandbox | None,
    at_root: bool,
    data_dir: Path,
) -> Place:
    """The recovery area for originals in ``folder`` (created as needed)."""
    dev = _dev_of(fs, folder)
    if dev is None:
        return Place("in_place", "", 0)
    made: list[str] = []
    cand = data_dir / "dupes" / "cleanup" / "recovery" / run_id
    base = data_dir
    while not base.exists() and base != base.parent:
        base = base.parent
    ddev = _dev_of(fs, str(base))
    if (
        ddev == dev
        and (sandbox is None or sandbox.allows(str(cand)))
        and _mkdirs(str(cand), made, private=True)
    ):
        _readme(str(cand))
        return Place("data", str(cand), dev, made)
    if sandbox is not None:
        for root in sandbox.roots:
            if _dev_of(fs, root) == dev:
                p = os.path.join(root, AREA, run_id)
                if _mkdirs(p, made, private=True):
                    _hide(os.path.join(root, AREA))
                    _readme(p)
                    return Place("sandbox", p, dev, made)
    elif at_root:
        top = (
            _top_of(folder, dev)
            if sys.platform != "win32"
            else os.path.splitdrive(os.path.abspath(folder))[0] + "\\"
        )
        uid = getattr(os, "getuid", lambda: -1)()
        name = AREA if sys.platform == "win32" else f"{AREA}-{uid}"
        p = os.path.join(top, name, run_id)
        if _dev_of(fs, top) == dev and _mkdirs(p, made, private=True):
            _hide(os.path.join(top, name))
            _readme(p)
            return Place("root", p, dev, made)
    return Place("in_place", "", dev, made)


def _readme(folder: str) -> None:
    p = os.path.join(folder, "README.txt")
    if not os.path.exists(p):
        with contextlib.suppress(OSError), open(p, "w", encoding="utf-8") as f:
            f.write(TEXT["readme"])


def write_manifest(folder: str, items: list[dict[str, Any]]) -> None:
    if not folder:
        return
    with contextlib.suppress(OSError):
        tmp = os.path.join(folder, "manifest.json.tmp")
        with open(tmp, "w", encoding="utf-8", errors="surrogatepass") as f:
            json.dump(items, f, ensure_ascii=False, indent=1)
        os.replace(tmp, os.path.join(folder, "manifest.json"))


def remove_if_empty(folder: str) -> None:
    """Remove a recovery folder once only our README and manifest are left (and its empty parents
    that are recovery folders too)."""
    if not folder or not os.path.isdir(folder):
        return
    try:
        names = set(os.listdir(folder))
    except OSError:
        return
    if names - {"README.txt", "manifest.json"}:
        return
    for n in names:
        with contextlib.suppress(OSError):
            os.unlink(os.path.join(folder, n))
    with contextlib.suppress(OSError):
        os.rmdir(folder)
    parent = os.path.dirname(folder)
    if os.path.basename(parent).startswith(AREA) or os.path.basename(parent) == "recovery":
        with contextlib.suppress(OSError):
            os.rmdir(parent)  # only when empty


def is_recovery_path(path: str) -> bool:
    """A path inside a recovery area, or an original kept beside itself."""
    parts = path.replace("\\", "/").split("/")
    if any(p.startswith(AREA) for p in parts):
        return True
    name = parts[-1] if parts else ""
    return name.startswith(".~sk-") and name.endswith(".orig")
