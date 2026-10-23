"""Moving to the trash, restoring from it, deleting permanently (docs/manage.md §2.5).

**A request to move something to the trash never becomes a permanent deletion.** On Windows the
shell silently destroys what does not fit its recycle bin (too large, path too long, the volume set
to "delete immediately", no bin at all) when no confirmation may be shown. So, before an item is
touched: the volume must have a bin (local fixed volume with a GUID, ``NukeOnDelete`` off, no
``NoRecycleFiles`` policy), the item must fit the bin's capacity (with headroom) and its paths must
stay short enough inside the bin; otherwise the item gets ``no_trash`` and is left alone. During
the operation the shell's own announcement is the second guard: a deletion it starts without
"recycle" is refused (``ops._win``), which aborts the operation before anything is destroyed; the
rest is retried without the refused item.

Linux follows the freedesktop.org Trash specification: the home trash
(``$XDG_DATA_HOME/Trash``) for files on its file system, else ``$topdir/.Trash/$uid`` (an admin
trash with the sticky bit) or ``$topdir/.Trash-$uid``; an ``info/<name>.trashinfo`` is written
first (``O_EXCL`` picks a free name), then the item is renamed into ``files/`` — a rename, never a
copy. macOS uses ``NSFileManager trashItemAtURL`` (best effort: not tested on a Mac).

``trash_ref`` (for :func:`restore`) is the item's info file: ``$I…`` in ``$Recycle.Bin\\<SID>``
on Windows, the ``.trashinfo`` on Linux; on macOS a JSON pair of the trashed and original path.
Restoring never overwrites: an occupied original path fails with ``exists``.

Every function here is synchronous and may take long: run it on a worker thread. ``progress`` is
called (from a worker thread) once per item with its final result; ``cancel`` is polled between
items (inside a folder too, for permanent deletion).
"""

from __future__ import annotations

import contextlib
import errno
import json
import os
import shutil
import stat
import struct
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal
from urllib.parse import quote, unquote_to_bytes

from loguru import logger

from .. import fmt
from ..i18n import Texts
from .locks import find_lockers
from .paths import protected_reason

if TYPE_CHECKING:
    from ..dupes.guard import Elsewhere, Guard
    from . import _win

OpCode = Literal[
    "ok",
    "missing",
    "denied",
    "locked",
    "protected",
    "no_trash",
    "exists",
    "cancelled",
    "ambiguous_name",
    "changed",
    "keeper_changed",
    "last_copy",
    "keeper_busy",
    "error",
]
ProgressFn = Callable[[int, int, "OpResult"], None]  # (items done, items in total, the one just done)
CancelFn = Callable[[], bool]

# Every user-facing text of this module (M7.5 extracts them for translation).
TEXT = Texts(
    {
        "trashed": "已移到回收站",
        "restored": "已还原",
        "deleted": "已永久删除",
        "missing": "已不存在",
        "denied": "没有权限",
        "locked": "正在被 {names} 使用，需先退出",
        "locked_unknown": "正被其他程序使用，需先关闭",
        "cancelled": "已取消",
        "cancelled_partly": "已取消，其中一部分已删除",
        "error": "操作失败：{detail}",
        "partly": "部分内容未能删除：{detail}",
        "mount_inside": "其中有另一个卷的挂载点，未进入",
        "no_bin_remote": "网络位置没有回收站，只能永久删除",
        "no_bin_removable": "可移动磁盘没有回收站，只能永久删除",
        "no_bin_volume": "这个卷没有回收站，只能永久删除",
        "no_bin_nuke": "这个卷的回收站设置为「不移到回收站，直接删除」，只能永久删除",
        "too_big": "超出回收站容量（{size}，回收站最多 {limit}），只能永久删除",
        "too_long": "路径过长，回收站放不下，只能永久删除",
        "refused": "系统准备直接删除而不是移到回收站，已阻止，未做任何改动",
        "destroyed": "未在回收站中找到，可能已被系统直接删除",
        "no_trash_dir": "这个卷上没有可用的回收站目录，只能永久删除",
        "cross_mount": "回收站与它不在同一个挂载点，无法移入",
        "no_name": "回收站中找不到可用的名字",
        "not_moved": "未能移到回收站（系统没有说明原因），未做改动",
        "unsupported": "这个系统不支持移到回收站",
        "exists": "原位置已有同名项目，未覆盖",
        "ambiguous_name": "另一项与它只差大小写，按名字会打开另一项；没有改动任何东西",
        "trailing_name": "它或上级文件夹的名字以点或空格结尾，Windows 按路径找到的不是它；没有改动任何东西",
        "bad_ref": "不是可还原的回收站条目",
        "gone_from_trash": "回收站中已没有这一项（可能已被清空）",
        "list_sep": "、",
    }
)

WIN_MAX_PATH = 259  # longest path the shell's recycle engine handles (MAX_PATH less the NUL)
CAPACITY_SHARE = 0.9  # an item must stay below this share of the bin's capacity ...
BATCH_SHARE = 0.5  # ... and one shell operation below this share (in case the engine sums a batch)
TRUST_SHARE = 0.25  # a folder whose scanned size is below this share is not measured again
BATCH_ITEMS = 64  # items per shell operation (progress and cancel granularity)
TRASH_USAGE_LIMIT = 100_000  # entries walked to tell a Linux trash's size (-1 beyond)


@dataclass(slots=True)
class OpResult:
    path: str
    ok: bool
    code: OpCode
    message: str  # for the user, in the current language
    trash_ref: str = ""  # move_to_trash: what restore() takes to put it back


@dataclass(frozen=True, slots=True)
class TrashStatus:
    """What moving something on a volume to the trash means there (UI-W5-39/40)."""

    available: bool
    reason: str = ""  # why not (for the user)
    location: str = ""  # where trashed items of this volume go
    used: int = -1  # bytes in it now (-1: unknown)
    capacity: int = 0  # Windows: the bin's capacity on the volume (0: none or unknown)
    same_volume: bool = True  # trashed items stay on the volume: the space returns only when emptied


def _result(path: str, code: OpCode, message: str = "", ref: str = "") -> OpResult:
    return OpResult(
        path=path, ok=code == "ok", code=code, message=message or TEXT.get(code, code), trash_ref=ref
    )


def _os_error(path: str, exc: OSError) -> OpResult:
    if isinstance(exc, FileNotFoundError):
        return _result(path, "missing")
    if getattr(exc, "winerror", None) in (32, 33):  # sharing / lock violation
        return _result(path, "locked", TEXT["locked_unknown"])
    if isinstance(exc, PermissionError) or exc.errno in (errno.EACCES, errno.EPERM, errno.EROFS):
        return _result(path, "denied")
    return _result(path, "error", TEXT["error"].format(detail=exc.strerror or exc))


# --------------------------------------------------------------------------- checks


def _final_name(path: str) -> str | None:
    """The stored name of what ``path`` opens (Windows; scan.worker.final_name), else None."""
    from ..scan.worker import final_name

    return final_name(path)


def reaches_another(path: str, root: str, *, root_names: dict[str, str | None] | None = None) -> bool:
    """Whether ``path`` may open an entry other than the one its names say. The file system's
    stored name of what it opens is compared with the scan ``root``'s own followed by the path's
    names below it (scan.worker.task_is); where they differ, a folder on the way that holds the
    path's name and another name equal to it but for case decides: an open by name can take the
    other one (on WinBtrfs the name of ``a`` opens its sibling ``A``, or follows ``A`` when that is
    a symlink; POSIX names on NTFS in a folder without the case-sensitive flag do the same).
    Without such a pair the path still names the item: renamed in case since the scan, another
    name of a hard link, a redirect. A folder that cannot be listed to tell counts as holding a
    pair; nothing counts when the stored name is unknown. ``root_names``: the roots' stored names,
    kept across the calls of one run."""
    from ..scan.worker import _long_path, names_below, task_is

    below = names_below(path, root)
    stored = _final_name(path) if below else None
    if not below or stored is None:
        return False
    cache = root_names if root_names is not None else {}
    if root not in cache:
        cache[root] = _final_name(root)
    if task_is(stored, cache[root], below):
        return False
    for i, name in enumerate(below):
        try:
            entries = os.listdir(_long_path(os.path.join(root, *below[:i])))
        except OSError:
            return True
        if name in entries and any(e != name and e.upper() == name.upper() for e in entries):
            return True
    return False


def win32_trims(path: str) -> bool:
    """Whether Win32 rewrites a name of ``path`` (Windows): one ending in a dot or a space loses
    it, so the path names another item (``odd.`` becomes ``odd``) or none. The scan lists such
    items as they are; deleting them by such a path would touch the other one."""
    if sys.platform != "win32":
        return False
    rest = os.path.splitdrive(path)[1]
    return any(
        n.endswith((".", " ")) for n in rest.replace("/", "\\").split("\\") if n not in ("", ".", "..")
    )


def _precheck(
    paths: Sequence[str],
    check_locks: bool,
    roots: Mapping[str, str],
    guards: Mapping[str, Guard] | None = None,
    also_doomed: Callable[[str], bool] | Elsewhere | None = None,
) -> dict[int, OpResult]:
    """Per index, the reason an item is skipped: reached by its path only as another entry, gone,
    protected, held open (§2.5 re-check); extra copies of duplicate files also when their guard
    fails (dupes.guard: changed, keeper_changed, last_copy, keeper_busy)."""
    skipped: dict[int, OpResult] = {}
    alive: list[int] = []
    root_names: dict[str, str | None] = {}
    for i, p in enumerate(paths):
        if win32_trims(p):  # before lexists: that would look at the other item
            skipped[i] = _result(p, "ambiguous_name", TEXT["trailing_name"])
            continue
        if not os.path.lexists(p):
            skipped[i] = _result(p, "missing")
            continue
        reason = protected_reason(p)
        if reason is not None:
            skipped[i] = _result(p, "protected", reason)
            continue
        root = roots.get(p)
        if root is not None and reaches_another(p, root, root_names=root_names):
            skipped[i] = _result(p, "ambiguous_name")
            continue
        alive.append(i)
    if check_locks and alive:
        lockers = find_lockers([paths[i] for i in alive])
        for i in alive:
            names = lockers.get(paths[i])
            if names:
                skipped[i] = _result(
                    paths[i], "locked", TEXT["locked"].format(names=TEXT["list_sep"].join(names))
                )
    if guards:
        from ..dupes.guard import check

        live = [i for i in alive if i not in skipped]
        failed = check(
            [paths[i] for i in live],
            guards,
            also_doomed=also_doomed,
            lockers=find_lockers if check_locks else None,
        )
        for j, (code, message) in failed.items():
            i = live[j]
            skipped[i] = _result(paths[i], code, message)  # type: ignore[arg-type]
    return skipped


class _Run:
    """Results in input order, progress as they become final."""

    def __init__(self, paths: Sequence[str], progress: ProgressFn | None, cancel: CancelFn | None) -> None:
        self.paths = list(paths)
        self.results: list[OpResult | None] = [None] * len(self.paths)
        self.progress = progress
        self.cancel = cancel
        self.done = 0

    def cancelled(self) -> bool:
        return self.cancel is not None and bool(self.cancel())

    def set(self, i: int, result: OpResult) -> None:
        if self.results[i] is not None:
            return
        self.results[i] = result
        self.done += 1
        if self.progress is not None:
            try:
                self.progress(self.done, len(self.paths), result)
            except Exception as exc:  # a broken observer must not stop the operation
                logger.warning("progress callback failed: {}", exc)

    def finish(self) -> list[OpResult]:
        for i, r in enumerate(self.results):
            if r is None:
                self.set(i, _result(self.paths[i], "cancelled"))
        return [r for r in self.results if r is not None]


# --------------------------------------------------------------------------- public


def trash_available(path: str) -> bool:
    """Whether the volume of ``path`` has a trash to move things to (Windows: a recycle bin on a
    local fixed volume that is not set to delete immediately)."""
    return trash_status(path, usage=False).available


def trash_status(path: str, *, usage: bool = True) -> TrashStatus:
    """The trash of the volume holding ``path``; ``usage``: also tell how much it holds now."""
    try:
        if sys.platform == "win32":
            return _win_status(path, usage)
        if sys.platform == "darwin":
            return _mac_status(path)
        return _fd_status(path, usage)
    except OSError as exc:
        return TrashStatus(False, TEXT["error"].format(detail=exc.strerror or exc))


def move_to_trash(
    paths: Sequence[str],
    *,
    progress: ProgressFn | None = None,
    cancel: CancelFn | None = None,
    sizes: Mapping[str, int] | None = None,
    check_locks: bool = True,
    roots: Mapping[str, str] | None = None,
    guards: Mapping[str, Guard] | None = None,
    also_doomed: Callable[[str], bool] | Elsewhere | None = None,
) -> list[OpResult]:
    """Move each path to the trash; one result per path, in order. Never deletes anything.

    ``sizes``: bytes per path (keyed by the same strings) as last scanned, e.g. the basket's
    targets; folders well below the recycle bin's capacity are then not measured again.
    ``check_locks``: skip items held open. ``roots``: the scan root of each path (same keys); a
    path whose names below it open an entry differing only in case is left alone
    (``ambiguous_name``, ``reaches_another``). ``guards``: extra copies of duplicate files and what
    must still hold for them (dupes.guard); ``also_doomed``: paths deleted by something else.
    """
    run = _Run(paths, progress, cancel)
    for i, res in _precheck(run.paths, check_locks, roots or {}, guards, also_doomed).items():
        run.set(i, res)
    todo = [i for i, r in enumerate(run.results) if r is None]
    if sys.platform == "win32":
        _win_trash(run, todo, sizes or {})
    else:
        for i in todo:
            if run.cancelled():
                break
            if sys.platform == "darwin":
                run.set(i, _mac_trash(run.paths[i]))
            else:
                run.set(i, _fd_trash(run.paths[i]))
    return run.finish()


def restore(refs: Sequence[str]) -> list[OpResult]:
    """Put trashed items back where they were (never over an existing item: ``exists``)."""
    out: list[OpResult] = []
    for ref in refs:
        try:
            if sys.platform == "win32":
                out.append(_win_restore(ref))
            elif sys.platform == "darwin":
                out.append(_mac_restore(ref))
            else:
                out.append(_fd_restore(ref))
        except OSError as exc:
            out.append(_os_error(ref, exc))
    return out


def delete_permanently(
    paths: Sequence[str],
    *,
    progress: ProgressFn | None = None,
    cancel: CancelFn | None = None,
    check_locks: bool = True,
    roots: Mapping[str, str] | None = None,
    guards: Mapping[str, Guard] | None = None,
    also_doomed: Callable[[str], bool] | Elsewhere | None = None,
) -> list[OpResult]:
    """Delete each path for good (folders with everything inside); links are removed, never
    followed, and other file systems mounted inside a folder are not entered. ``roots`` as for
    :func:`move_to_trash`."""
    run = _Run(paths, progress, cancel)
    for i, res in _precheck(run.paths, check_locks, roots or {}, guards, also_doomed).items():
        run.set(i, res)
    for i, r in enumerate(run.results):
        if r is not None:
            continue
        if run.cancelled():
            break
        run.set(i, _delete_one(run.paths[i], run.cancel))
    return run.finish()


# --------------------------------------------------------------------------- permanent deletion


def _is_link_like(st: os.stat_result) -> bool:
    """A symlink, or a Windows reparse point that is a name surrogate (junction, mount point)."""
    if stat.S_ISLNK(st.st_mode):
        return True
    tag = getattr(st, "st_reparse_tag", 0)
    return bool(tag & 0x20000000)  # IO_REPARSE_TAG name surrogate bit


def _unlink(path: str) -> None:
    try:
        os.unlink(path)
    except PermissionError:
        if sys.platform != "win32":
            raise
        st = os.lstat(path)
        if st.st_file_attributes & stat.FILE_ATTRIBUTE_READONLY:  # what Explorer deletes after asking
            os.chmod(path, stat.S_IWRITE)
            os.unlink(path)
        elif stat.S_ISDIR(st.st_mode):
            os.rmdir(path)  # a directory link
        else:
            raise
    except IsADirectoryError:
        os.rmdir(path)


def _remove_link(path: str) -> None:
    try:
        _unlink(path)
    except OSError:
        os.rmdir(path)


def _remove_tree(top: str, cancel: CancelFn | None) -> tuple[OSError | None, bool, bool]:
    """Delete a folder bottom-up; returns (first error, cancelled, anything deleted)."""
    first: OSError | None = None
    deleted = False
    dev = os.lstat(top).st_dev
    stack: list[tuple[str, bool]] = [(top, False)]
    while stack:
        folder, listed = stack.pop()
        if listed:
            try:
                os.rmdir(folder)
                deleted = True
            except OSError as exc:
                first = first or exc
            continue
        if cancel is not None and cancel():
            return first, True, deleted
        stack.append((folder, True))
        try:
            with os.scandir(folder) as it:
                entries = list(it)
        except OSError as exc:
            first = first or exc
            continue
        for entry in entries:
            try:
                st = entry.stat(follow_symlinks=False)
                if _is_link_like(st):  # symlink, junction: the link only, never what it points to
                    if sys.platform == "win32" and stat.S_ISDIR(st.st_mode) and os.path.ismount(entry.path):
                        first = first or OSError(errno.EXDEV, TEXT["mount_inside"], entry.path)
                        continue
                    _remove_link(entry.path)
                    deleted = True
                elif stat.S_ISDIR(st.st_mode):
                    if sys.platform != "win32" and st.st_dev != dev:
                        first = first or OSError(errno.EXDEV, TEXT["mount_inside"], entry.path)
                        continue
                    stack.append((entry.path, False))
                else:
                    _unlink(entry.path)
                    deleted = True
            except OSError as exc:
                first = first or exc
    return first, False, deleted


def _delete_one(path: str, cancel: CancelFn | None) -> OpResult:
    try:
        st = os.lstat(path)
        if _is_link_like(st):
            _remove_link(path)
        elif stat.S_ISDIR(st.st_mode):
            error, stopped, deleted = _remove_tree(path, cancel)
            if stopped:
                return _result(path, "cancelled", TEXT["cancelled_partly"] if deleted else "")
            if error is not None:
                if not deleted:
                    return _os_error(path, error)
                return _result(path, "error", TEXT["partly"].format(detail=error.strerror or error))
        else:
            _unlink(path)
    except OSError as exc:
        return _os_error(path, exc)
    return _result(path, "ok", TEXT["deleted"])


# --------------------------------------------------------------------------- Windows


@dataclass(frozen=True, slots=True)
class _Bin:
    root: str  # volume mount point
    folder: str  # $Recycle.Bin\<SID> on it
    capacity: int  # bytes


def _bin_settings(guid: str, total: int) -> _win.BinSettings:
    """The volume's recycle-bin settings (a seam: tests simulate a volume that destroys)."""
    from . import _win

    return _win.bin_settings(guid, total)


def _win_bin(path: str) -> _Bin | str:
    """The recycle bin for ``path``, or why there is none (Chinese)."""
    from . import _win

    root = _win.volume_root(path)
    kind = _win.drive_type(root)
    if kind == _win.DRIVE_REMOTE:
        return TEXT["no_bin_remote"]
    if kind == _win.DRIVE_REMOVABLE:
        return TEXT["no_bin_removable"]
    guid = _win.volume_guid(root) if kind == _win.DRIVE_FIXED else ""
    if not guid:
        return TEXT["no_bin_volume"]
    settings = _bin_settings(guid, shutil.disk_usage(root).total)
    if settings.nuke or settings.max_bytes <= 0:
        return TEXT["no_bin_nuke"]
    return _Bin(
        root=root, folder=os.path.join(root, "$Recycle.Bin", _win.user_sid()), capacity=settings.max_bytes
    )


def _win_status(path: str, usage: bool) -> TrashStatus:
    from . import _win

    found = _win_bin(path)
    if isinstance(found, str):
        return TrashStatus(False, found, same_volume=False)
    used = _win.bin_usage(found.root)[0] if usage else -1
    return TrashStatus(True, location=found.folder, used=used, capacity=found.capacity)


def _measure(path: str, limit: int, bin_folder: str) -> tuple[int, bool]:
    """(bytes, too long) of an item, walking a folder until it passes ``limit``.

    "Too long": some path inside would pass MAX_PATH where it is now, or once it is renamed to
    ``$R??????`` in the bin.
    """
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or _is_link_like(st):
        return st.st_size, len(path) > WIN_MAX_PATH
    total = 0
    base = len(path)
    in_bin = len(bin_folder) + 1 + 8 + len(os.path.splitext(path)[1])  # \$R + 6 characters + ext
    stack = [path]
    while stack:
        folder = stack.pop()
        try:
            with os.scandir(folder) as it:
                for entry in it:
                    full = len(entry.path)
                    if full > WIN_MAX_PATH or in_bin + full - base > WIN_MAX_PATH:
                        return total, True
                    est = entry.stat(follow_symlinks=False)
                    if stat.S_ISDIR(est.st_mode) and not _is_link_like(est):
                        stack.append(entry.path)
                    else:
                        total += est.st_size
                    if total > limit:
                        return total, False
        except OSError:
            continue  # what cannot be read here cannot be moved either: the engine reports it
    return total, False


def _hr_result(path: str, hr: int) -> OpResult:
    from . import _win

    value = hr & 0xFFFFFFFF
    code = _win.hresult_code(hr)
    win32 = value >> 16 == 0x8007
    if value in (0x80270027, 0x80270028) or (win32 and code in (32, 33)):
        return _result(path, "locked", TEXT["locked_unknown"])
    if value in (0x80270002, 0x80270021, 0x80270022, 0x8027003F) or (win32 and code == 5):
        return _result(path, "denied")
    if win32 and code in (2, 3):
        return _result(path, "missing")
    if value in (0x80270036, 0x80270037, 0x80270038, 0x8027003A):  # the engine: cannot recycle
        return _result(path, "no_trash", TEXT["no_bin_volume"])
    if value in (0x80270000, 0x80270001, 0x80004004):
        return _result(path, "cancelled")
    return _result(path, "error", TEXT["error"].format(detail=f"0x{value:08X}"))


def _win_key(path: str) -> str:
    return os.path.normcase(os.path.normpath(path))


def _match_unmatched(unmatched: list[tuple[str, str | None]], where: dict[str, int]) -> dict[int, str]:
    """Recycled items the shell reported under another spelling of their path: matched through
    the original path recorded in their ``$I`` file (index → ``$R`` path)."""
    found: dict[int, str] = {}
    for reported, recycled in unmatched:
        if not recycled:
            continue
        try:
            with open(_info_of(recycled), "rb") as fh:
                original = parse_recycle_info(fh.read(1 << 16))[2]
        except (OSError, ValueError):
            original = reported
        for candidate in (original, reported):
            at = where.get(_win_key(candidate))
            if at is None:
                at = where.get(_win_key(os.path.realpath(candidate)))
            if at is not None:
                found[at] = recycled
                break
        else:
            logger.error("recycle: {} landed in the bin as {} but matches no request", reported, recycled)
    return found


def _info_of(recycled: str) -> str:
    """``$R…`` path in the bin → its ``$I…`` record."""
    folder, name = os.path.split(recycled)
    return os.path.join(folder, "$I" + name[2:])


def _win_trash(run: _Run, todo: list[int], sizes: Mapping[str, int]) -> None:
    from . import _win

    bins: dict[str, _Bin | str] = {}
    ready: list[tuple[int, _Bin, int]] = []  # (index, bin, bytes)
    for i in todo:
        path = run.paths[i]
        try:
            root = _win.volume_root(path)
            found = bins.get(root)
            if found is None:
                found = bins[root] = _win_bin(path)
        except OSError as exc:
            run.set(i, _os_error(path, exc))
            continue
        if isinstance(found, str):
            run.set(i, _result(path, "no_trash", found))
            continue
        limit = int(found.capacity * CAPACITY_SHARE)
        if len(path) > WIN_MAX_PATH:
            run.set(i, _result(path, "no_trash", TEXT["too_long"]))
            continue
        hint = sizes.get(path)
        try:
            is_dir = os.path.isdir(path) and not os.path.islink(path)
            if is_dir and hint is not None and hint <= found.capacity * TRUST_SHARE:
                size, too_long = hint, False  # far from the limit; the engine guard covers the rest
            else:
                size, too_long = _measure(path, limit, found.folder)
        except OSError as exc:
            run.set(i, _os_error(path, exc))
            continue
        if too_long:
            run.set(i, _result(path, "no_trash", TEXT["too_long"]))
        elif size > limit:
            run.set(
                i,
                _result(
                    path,
                    "no_trash",
                    TEXT["too_big"].format(size=fmt.size(size), limit=fmt.size(found.capacity)),
                ),
            )
        else:
            ready.append((i, found, size))

    # consecutive items on one volume, bounded in count and bytes, per shell operation
    batches: list[list[int]] = []
    current: list[int] = []
    volume, used = "", 0
    for i, found, size in ready:
        if current and (
            found.root != volume or len(current) >= BATCH_ITEMS or used + size > found.capacity * BATCH_SHARE
        ):
            batches.append(current)
            current, used = [], 0
        current.append(i)
        volume, used = found.root, used + size
    if current:
        batches.append(current)

    for batch in batches:
        pending = batch
        while pending and not run.cancelled():
            where = {_win_key(run.paths[i]): i for i in pending}

            def done(entry: _win.Recycled, where: dict[str, int] = where) -> None:  # as each item lands
                at = where.get(_win_key(entry.path))
                if at is not None and entry.recycled:
                    run.set(at, _result(run.paths[at], "ok", TEXT["trashed"], _info_of(entry.recycled)))

            outcome = _win.recycle([run.paths[i] for i in pending], cancel=run.cancel, on_item=done)
            late = _match_unmatched(outcome.unmatched, where)
            retry: list[int] = []
            refused = [_win_key(p) for p in outcome.refused]
            for i, entry in zip(pending, outcome.items, strict=True):
                path = run.paths[i]
                key = _win_key(path)
                if not entry.recycled and i in late:
                    entry.recycled = late[i]
                if entry.recycled:
                    run.set(i, _result(path, "ok", TEXT["trashed"], _info_of(entry.recycled)))
                elif entry.refused or any(r == key or r.startswith(key + os.sep) for r in refused):
                    run.set(i, _result(path, "no_trash", TEXT["refused"]))
                elif outcome.cancelled and entry.hr < 0:
                    continue  # finish() reports it cancelled
                elif not entry.reached and refused:
                    retry.append(i)  # the operation stopped at a refused item before this one
                elif entry.hr >= 0 and not os.path.lexists(path):
                    run.set(i, _result(path, "error", TEXT["destroyed"]))
                    logger.error("recycle: {} vanished without a recycle-bin entry", path)
                elif entry.hr >= 0:
                    run.set(i, _result(path, "error", TEXT["not_moved"]))
                else:
                    run.set(i, _hr_result(path, entry.hr))
            if retry and len(retry) < len(pending):
                pending = retry  # something was settled (at least the refused item): go on with the rest
            else:
                for i in retry:
                    run.set(i, _result(run.paths[i], "error", TEXT["not_moved"]))
                pending = []
        if run.cancelled():
            return


def parse_recycle_info(data: bytes) -> tuple[int, float, str]:
    """``$I`` record → (size, deletion time as unix seconds, original path).

    Version 1 (Vista–8.1): 8-byte version, size, FILETIME, then the path in 260 UTF-16 units;
    version 2 (Windows 10+): the same header, a 4-byte length in units, then the path.
    """
    if len(data) < 24:
        raise ValueError("record too short")
    version, size, filetime = struct.unpack_from("<qqq", data)
    if version == 1:
        raw = data[24 : 24 + 520]
    elif version == 2:
        if len(data) < 28:
            raise ValueError("record too short")
        (units,) = struct.unpack_from("<i", data, 24)
        raw = data[28 : 28 + 2 * max(0, units)]
    else:
        raise ValueError(f"unknown record version {version}")
    path = raw.decode("utf-16-le", "surrogatepass").split("\0", 1)[0]
    if not path:
        raise ValueError("record has no path")
    return size, (filetime - 116444736000000000) / 1e7, path


def _win_restore(ref: str) -> OpResult:
    from . import _win

    info = os.path.abspath(ref)
    folder, name = os.path.split(info)
    bin_dir = os.path.dirname(folder)
    valid = (
        name[:2].upper() == "$I"
        and len(name) > 2
        and os.path.basename(folder).upper() == _win.user_sid().upper()
        and os.path.basename(bin_dir).lower() == "$recycle.bin"
        and os.path.normcase(os.path.dirname(bin_dir).rstrip("\\") + "\\")
        == os.path.normcase(_win.volume_root(info))
    )
    if not valid:
        return _result(ref, "error", TEXT["bad_ref"])
    try:
        with open(info, "rb") as fh:
            _, _, original = parse_recycle_info(fh.read(1 << 16))
    except FileNotFoundError:
        return _result(ref, "missing", TEXT["gone_from_trash"])
    except ValueError:
        return _result(ref, "error", TEXT["bad_ref"])
    payload = os.path.join(folder, "$R" + name[2:])
    if not os.path.lexists(payload):
        return _result(original, "missing", TEXT["gone_from_trash"])
    if os.path.lexists(original):
        return _result(original, "exists")
    os.makedirs(os.path.dirname(original), exist_ok=True)
    try:
        os.rename(payload, original)  # MoveFileEx without REPLACE_EXISTING: never overwrites
    except FileExistsError:
        return _result(original, "exists")
    except OSError as exc:
        return _os_error(original, exc)
    try:
        os.remove(info)
    except OSError as exc:  # Explorer drops a record whose $R is gone; nothing is lost
        logger.warning("restore: could not remove {}: {}", info, exc)
    with contextlib.suppress(OSError):
        _win.notify_changed(original, folder=os.path.isdir(original))
    return _result(original, "ok", TEXT["restored"], ref)


# --------------------------------------------------------------------------- Linux (freedesktop)


@dataclass(frozen=True, slots=True)
class _FdTrash:
    root: str  # holds files/ and info/
    topdir: str  # "" for the home trash (absolute Path=); else Path= is relative to it


def home_trash() -> str:
    """``$XDG_DATA_HOME/Trash`` (``~/.local/share/Trash`` when unset or not absolute)."""
    xdg = os.environ.get("XDG_DATA_HOME", "")
    base = xdg if os.path.isabs(xdg) else os.path.join(os.path.expanduser("~"), ".local", "share")
    return os.path.join(base, "Trash")


def _existing_dev(path: str) -> int:
    while True:
        try:
            return os.stat(path).st_dev
        except OSError:
            up = os.path.dirname(path)
            if up == path:
                raise
            path = up


def _mount_top(path: str, dev: int) -> str:
    """The highest folder above ``path`` on the same file system (its mount point)."""
    cur = os.path.realpath(os.path.dirname(os.path.abspath(path)))
    while True:
        up = os.path.dirname(cur)
        if up == cur:
            return cur
        try:
            if os.lstat(up).st_dev != dev:
                return cur
        except OSError:
            return cur
        cur = up


def _usable_dir(path: str, uid: int, create: bool) -> bool:
    """A trash folder that is a real folder owned by ``uid`` (made with mode 0700 if ``create``)."""
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        if not create:
            return os.access(os.path.dirname(path), os.W_OK | os.X_OK)
        try:
            os.mkdir(path, 0o700)
            st = os.lstat(path)
        except OSError:
            return False
    except OSError:
        return False
    return stat.S_ISDIR(st.st_mode) and st.st_uid == uid


def _fd_find(path: str, *, create: bool) -> _FdTrash | str:
    """The trash for ``path`` (freedesktop spec), or why there is none (Chinese)."""
    if sys.platform == "win32":
        return TEXT["unsupported"]
    dev = os.lstat(path).st_dev
    home = home_trash()
    if _existing_dev(home) == dev:
        if create:
            for sub in ("files", "info"):
                os.makedirs(os.path.join(home, sub), mode=0o700, exist_ok=True)
        return _FdTrash(home, "")
    uid = os.getuid()
    top = _mount_top(path, dev)
    admin = os.path.join(top, ".Trash")
    try:
        st = os.lstat(admin)
        if stat.S_ISDIR(st.st_mode) and st.st_mode & stat.S_ISVTX:  # not a link, sticky: usable
            mine = os.path.join(admin, str(uid))
            if _usable_dir(mine, uid, create):
                return _fd_ready(_FdTrash(mine, top), create)
    except OSError:
        pass
    mine = os.path.join(top, f".Trash-{uid}")
    if _usable_dir(mine, uid, create):
        return _fd_ready(_FdTrash(mine, top), create)
    return TEXT["no_trash_dir"]


def _fd_ready(trash: _FdTrash, create: bool) -> _FdTrash | str:
    if create:
        try:
            for sub in ("files", "info"):
                os.makedirs(os.path.join(trash.root, sub), mode=0o700, exist_ok=True)
        except OSError:
            return TEXT["no_trash_dir"]
    return trash


def _fd_status(path: str, usage: bool) -> TrashStatus:
    if sys.platform == "win32":
        return TrashStatus(False, TEXT["unsupported"])
    found = _fd_find(path, create=False)
    if isinstance(found, str):
        return TrashStatus(False, found, same_volume=False)
    return TrashStatus(
        True, location=found.root, used=_tree_bytes(os.path.join(found.root, "files")) if usage else -1
    )


def _tree_bytes(path: str) -> int:
    total, seen = 0, 0
    stack = [path]
    while stack:
        folder = stack.pop()
        try:
            with os.scandir(folder) as it:
                for entry in it:
                    seen += 1
                    if seen > TRASH_USAGE_LIMIT:
                        return -1
                    st = entry.stat(follow_symlinks=False)
                    if stat.S_ISDIR(st.st_mode):
                        stack.append(entry.path)
                    total += getattr(st, "st_blocks", 0) * 512 or st.st_size
        except OSError:
            continue
    return total


def _fit_name(name: str, extra: int) -> str:
    """``name`` shortened (keeping its extension) so that it plus ``extra`` bytes fits NAME_MAX."""
    limit = 255 - extra
    raw = os.fsencode(name)
    if len(raw) <= limit:
        return name
    stem, ext = os.path.splitext(name)
    keep = limit - len(os.fsencode(ext))
    cut = os.fsencode(stem)[: max(1, keep)]
    return os.fsdecode(cut).rstrip("\udcff") + ext if keep > 0 else os.fsdecode(raw[:limit])


def _fd_trash(path: str) -> OpResult:
    assert sys.platform != "win32"
    try:
        found = _fd_find(path, create=True)
    except OSError as exc:
        return _os_error(path, exc)
    if isinstance(found, str):
        return _result(path, "no_trash", found)
    original = os.path.abspath(path)
    if found.topdir:
        real = os.path.join(os.path.realpath(os.path.dirname(original)), os.path.basename(original))
        recorded = os.path.relpath(real, found.topdir)
    else:
        recorded = original
    body = (
        "[Trash Info]\n"
        f"Path={quote(os.fsencode(recorded), safe='/')}\n"
        f"DeletionDate={time.strftime('%Y-%m-%dT%H:%M:%S')}\n"
    ).encode()
    base = os.path.basename(original.rstrip("/")) or "item"
    stem, ext = os.path.splitext(base)
    for n in range(1, 10_000):
        name = _fit_name(base if n == 1 else f"{stem}.{n}{ext}", len(".trashinfo"))
        info = os.path.join(found.root, "info", name + ".trashinfo")
        target = os.path.join(found.root, "files", name)
        try:
            fd = os.open(info, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            continue
        except OSError as exc:
            return _os_error(path, exc)
        try:
            os.write(fd, body)
        finally:
            os.close(fd)
        if os.path.lexists(target):  # a left-over without its info file: pick another name
            os.unlink(info)
            continue
        try:
            rename_noreplace(path, target)
        except OSError as exc:
            os.unlink(info)
            if exc.errno == errno.EEXIST:
                continue
            if exc.errno == errno.EXDEV:
                return _result(path, "no_trash", TEXT["cross_mount"])
            return _os_error(path, exc)
        return _result(path, "ok", TEXT["trashed"], info)
    return _result(path, "error", TEXT["no_name"])


def parse_trashinfo(text: str) -> str:
    """The ``Path=`` of a ``.trashinfo`` (decoded; may be relative to the trash's topdir)."""
    section = ""
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("[") and line.endswith("]"):
            section = line
        elif section == "[Trash Info]" and line.startswith("Path="):
            return os.fsdecode(unquote_to_bytes(line[5:]))
    raise ValueError("no Path in [Trash Info]")


def _fd_restore(ref: str) -> OpResult:
    if sys.platform == "win32":
        return _result(ref, "error", TEXT["unsupported"])
    info = os.path.abspath(ref)
    info_dir = os.path.dirname(info)
    root = os.path.dirname(info_dir)
    uid = os.getuid()
    if not info.endswith(".trashinfo") or os.path.basename(info_dir) != "info":
        return _result(ref, "error", TEXT["bad_ref"])
    if os.path.normpath(root) == os.path.normpath(home_trash()):
        topdir = ""
    elif os.path.basename(root) == f".Trash-{uid}":
        topdir = os.path.dirname(root)
    elif os.path.basename(root) == str(uid) and os.path.basename(os.path.dirname(root)) == ".Trash":
        topdir = os.path.dirname(os.path.dirname(root))
    else:
        return _result(ref, "error", TEXT["bad_ref"])
    try:
        with open(info, encoding="utf-8", errors="surrogateescape") as fh:
            recorded = parse_trashinfo(fh.read(1 << 16))
    except FileNotFoundError:
        return _result(ref, "missing", TEXT["gone_from_trash"])
    except ValueError:
        return _result(ref, "error", TEXT["bad_ref"])
    original = recorded if os.path.isabs(recorded) else os.path.join(topdir or "/", recorded)
    original = os.path.normpath(original)
    payload = os.path.join(root, "files", os.path.basename(info)[: -len(".trashinfo")])
    if not os.path.lexists(payload):
        return _result(original, "missing", TEXT["gone_from_trash"])
    if os.path.lexists(original):
        return _result(original, "exists")
    os.makedirs(os.path.dirname(original), exist_ok=True)
    try:
        rename_noreplace(payload, original)
    except FileExistsError:
        return _result(original, "exists")
    except OSError as exc:
        return _os_error(original, exc)
    try:
        os.unlink(info)
    except OSError as exc:
        logger.warning("restore: could not remove {}: {}", info, exc)
    return _result(original, "ok", TEXT["restored"], ref)


def rename_noreplace(src: str, dst: str) -> None:
    """Rename that fails with ``FileExistsError`` instead of replacing ``dst``.

    Windows: ``os.rename`` never replaces. Linux: ``renameat2(RENAME_NOREPLACE)``; macOS:
    ``renamex_np(RENAME_EXCL)``; where neither is available, a check right before ``rename``.
    """
    if sys.platform == "win32":
        os.rename(src, dst)
        return
    fn = _native_noreplace()
    if fn is not None and fn(src, dst):
        return
    if os.path.lexists(dst):
        raise FileExistsError(errno.EEXIST, os.strerror(errno.EEXIST), dst)
    os.rename(src, dst)


_NOREPLACE: list[Callable[[str, str], bool] | None] = []


def _native_noreplace() -> Callable[[str, str], bool] | None:
    """A no-replace rename returning True when done, False when unsupported here (raises else)."""
    if _NOREPLACE:
        return _NOREPLACE[0]
    import ctypes

    fn: Callable[[str, str], bool] | None = None
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        if sys.platform == "darwin":
            native = libc.renamex_np
            native.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]

            def call(src: bytes, dst: bytes) -> int:
                return int(native(src, dst, 0x4))  # RENAME_EXCL
        else:
            native = libc.renameat2
            native.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]

            def call(src: bytes, dst: bytes) -> int:
                return int(native(-100, src, -100, dst, 1))  # AT_FDCWD, RENAME_NOREPLACE

        def rename(src: str, dst: str) -> bool:
            if call(os.fsencode(src), os.fsencode(dst)) == 0:
                return True
            err = ctypes.get_errno()
            if err in (errno.EINVAL, errno.ENOSYS, errno.ENOTSUP):  # this file system cannot
                return False
            raise OSError(err, os.strerror(err), dst)

        fn = rename
    except (AttributeError, OSError):
        fn = None
    _NOREPLACE.append(fn)
    return fn


# --------------------------------------------------------------------------- macOS (untested)


def _mac_status(path: str) -> TrashStatus:
    from ..scan.volume import volume_for

    vol = volume_for(path)
    if vol.is_remote:
        return TrashStatus(False, TEXT["no_bin_remote"], same_volume=False)
    return TrashStatus(True, location=os.path.join(os.path.expanduser("~"), ".Trash"))


def _mac_trash(path: str) -> OpResult:
    """``NSFileManager trashItemAtURL:resultingItemURL:error:`` through the Objective-C runtime."""
    import ctypes
    import ctypes.util

    try:
        objc = ctypes.cdll.LoadLibrary(ctypes.util.find_library("objc") or "libobjc.dylib")
        ctypes.cdll.LoadLibrary("/System/Library/Frameworks/Foundation.framework/Foundation")
    except OSError:
        return _result(path, "no_trash", TEXT["unsupported"])
    objc.objc_getClass.restype = ctypes.c_void_p
    objc.objc_getClass.argtypes = [ctypes.c_char_p]
    objc.sel_registerName.restype = ctypes.c_void_p
    objc.sel_registerName.argtypes = [ctypes.c_char_p]
    send = objc.objc_msgSend

    def msg(
        obj: Any, sel: bytes, *args: Any, restype: Any = ctypes.c_void_p, argtypes: tuple[Any, ...] = ()
    ) -> Any:
        send.restype = restype
        send.argtypes = [ctypes.c_void_p, ctypes.c_void_p, *argtypes]
        return send(obj, objc.sel_registerName(sel), *args)

    def cls(name: bytes) -> Any:
        return objc.objc_getClass(name)

    pool = msg(msg(cls(b"NSAutoreleasePool"), b"alloc"), b"init")
    try:
        text = msg(cls(b"NSString"), b"stringWithUTF8String:", os.fsencode(path), argtypes=(ctypes.c_char_p,))
        url = msg(cls(b"NSURL"), b"fileURLWithPath:", text, argtypes=(ctypes.c_void_p,))
        manager = msg(cls(b"NSFileManager"), b"defaultManager")
        out, err = ctypes.c_void_p(), ctypes.c_void_p()
        ok = msg(
            manager,
            b"trashItemAtURL:resultingItemURL:error:",
            url,
            ctypes.byref(out),
            ctypes.byref(err),
            restype=ctypes.c_bool,
            argtypes=(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p),
        )
        if not ok or not out.value:
            detail = "NSFileManager"
            if err.value:
                desc = msg(err.value, b"localizedDescription")
                raw = msg(desc, b"UTF8String", restype=ctypes.c_char_p)
                detail = raw.decode("utf-8", "replace") if isinstance(raw, bytes) else detail
            return _result(path, "error", TEXT["error"].format(detail=detail))
        landed = msg(msg(out.value, b"path"), b"UTF8String", restype=ctypes.c_char_p)
        where = os.fsdecode(landed) if isinstance(landed, bytes) else ""
        return _result(
            path, "ok", TEXT["trashed"], json.dumps({"trashed": where, "original": os.path.abspath(path)})
        )
    finally:
        msg(pool, b"drain")


def _mac_restore(ref: str) -> OpResult:
    try:
        data = json.loads(ref)
        trashed, original = str(data["trashed"]), str(data["original"])
    except (ValueError, KeyError, TypeError):
        return _result(ref, "error", TEXT["bad_ref"])
    parts = trashed.split("/")
    if ".Trash" not in parts and ".Trashes" not in parts:
        return _result(ref, "error", TEXT["bad_ref"])
    if not os.path.lexists(trashed):
        return _result(original, "missing", TEXT["gone_from_trash"])
    if os.path.lexists(original):
        return _result(original, "exists")
    os.makedirs(os.path.dirname(original), exist_ok=True)
    try:
        rename_noreplace(trashed, original)
    except FileExistsError:
        return _result(original, "exists")
    except OSError as exc:
        return _os_error(original, exc)
    return _result(original, "ok", TEXT["restored"], ref)


__all__ = [
    "TEXT",
    "CancelFn",
    "OpCode",
    "OpResult",
    "ProgressFn",
    "TrashStatus",
    "delete_permanently",
    "home_trash",
    "move_to_trash",
    "parse_recycle_info",
    "parse_trashinfo",
    "reaches_another",
    "rename_noreplace",
    "restore",
    "trash_available",
    "trash_status",
    "win32_trims",
]
