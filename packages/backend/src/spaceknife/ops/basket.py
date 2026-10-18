"""The delete basket (docs/manage.md §2.2–2.3): what the user wants gone, held by the backend only.

Items are kept by **path**, not node id: a file change (store.mutate) gives a relisted folder's
entries new ids, so :meth:`Basket.refresh` finds every item again by its path and recomputes it.

Containment: an item covers its path and everything below it (a small-item group covers exactly
its members). Adding a path that an item already covers merges it into that item; adding an
ancestor of items absorbs them. Covered paths are kept sorted, so "what lies below this folder"
is one binary search (a contiguous run of keys with the folder's prefix).

Freeable space (§2.3) is allocation, minus cloud placeholders (nothing stored locally), minus hard
links whose other names are not all in the basket (deleting one name frees nothing); a group of
links that is complete counts once, for the first item holding one of its names. Links are found
by ``(volume, ino)`` with the scan's ``ino`` / ``nlink`` columns (MFT record numbers on Windows;
the store keeps no ``st_dev``: one scan root is one file system).

Every method takes the basket's lock. Methods given a store read it, so they must run where that
is safe: on the event loop thread, or on a worker inside ``STRUCTURE.read()`` (store.lock).
"""

from __future__ import annotations

import bisect
import hashlib
import os
import threading
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from typing import Literal

import numpy as np

from ..store.lookup import PathIndex
from ..store.nodes import Flag, Kind, NodeStore
from ..store.subtree import subtree_ids
from .paths import protected_reason
from .trash import OpResult

ItemKind = Literal["file", "dir", "link", "other", "group"]
Status = Literal["ready", "locked", "missing", "protected"]

# Every user-facing text of this module (M7.5 extracts them for translation).
TEXT: dict[str, str] = {
    "group_name": "{folder} 中 {count} 个小项",
    "bad_node": "扫描结果中没有这一项",
    "virtual": "MFT 伪文件夹中的项不在磁盘上",
    "not_folder": "小项聚合块的所在位置不是文件夹",
    "not_member": "不是所在文件夹的直接子项",
    "missing": "已不存在",
    "members_missing": "其中 {count} 项已不存在",
    "locked": "正在被 {names} 使用，需先退出",
    "links_file": "另有 {count} 个硬链接不在删除篮中，删除后不释放空间",
    "links_dir": "其中 {count} 个文件在别处还有硬链接，删除后不释放这部分空间",
    "failed": "{message}",
    "members_failed": "{count} 项未完成：{message}",
    "list_sep": "、",
    "note_sep": "；",
}

_KIND_NAMES: dict[int, ItemKind] = {
    Kind.FILE: "file",
    Kind.DIR: "dir",
    Kind.LINK: "link",
    Kind.OTHER: "other",
}


@dataclass(slots=True)
class BasketItem:
    id: str  # stable: a hash of the normalised path (groups: of their folder)
    path: str  # absolute path (a small-item group: its folder; members non-empty)
    name: str
    kind: ItemKind
    scan_id: str
    members: list[str] = field(default_factory=list)  # group: member paths captured when added
    alloc: int = 0  # freeable allocation (§2.3); 0 while missing or protected
    size: int = 0
    files: int = 0
    dirs: int = 0
    volume: str = ""  # mount point of the volume holding it
    status: Status = "ready"
    note: str = ""  # Chinese: who holds it open, hard links, what failed …


@dataclass(frozen=True, slots=True)
class Merged:
    path: str
    into: str  # id of the item that already covers ``path``


@dataclass(frozen=True, slots=True)
class Refused:
    path: str
    reason: str  # Chinese


@dataclass(slots=True)
class AddResult:
    added: list[str] = field(default_factory=list)  # ids of new items (or of a group that grew)
    merged: list[Merged] = field(default_factory=list)  # already covered by an item
    absorbed: list[str] = field(default_factory=list)  # ids of items now inside a new one (gone)
    refused: list[Refused] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class Target:
    """One path to execute: an item's path, or one member of a group."""

    item: str  # item id
    path: str
    size: int  # bytes it holds as last scanned (max of logical and allocated): trash capacity checks


@dataclass(slots=True)
class _Measure:
    base: int = 0  # freeable bytes apart from hard links
    size: int = 0
    files: int = 0
    dirs: int = 0
    ino: np.ndarray = field(default_factory=lambda: np.empty(0, dtype=np.uint64))
    link_alloc: np.ndarray = field(default_factory=lambda: np.empty(0, dtype=np.int64))
    nlink: np.ndarray = field(default_factory=lambda: np.empty(0, dtype=np.int64))
    member_sizes: list[int] = field(default_factory=list)


@dataclass(slots=True)
class _Entry:
    item: BasketItem
    key: str  # normalised path (a group: its folder)
    member_keys: list[str] = field(default_factory=list)
    measure: _Measure = field(default_factory=_Measure)
    member_sizes: list[int] = field(default_factory=list)  # parallel to item.members
    fingerprint: bytes = b""  # node state the measure was computed from (b"": recompute)
    missing_members: int = 0
    lock_note: str = ""
    error_note: str = ""
    link_note: str = ""

    @property
    def group(self) -> bool:
        return self.item.kind == "group"

    def covered(self) -> list[str]:
        return self.member_keys if self.group else [self.key]


def _key(path: str) -> str:
    return os.path.normcase(os.path.normpath(path))


def _prefix(key: str) -> str:
    return key if key.endswith(os.sep) else key + os.sep


def _ancestors_or_self(key: str) -> Iterable[str]:
    while True:
        yield key
        up = os.path.dirname(key)
        if up == key or not up:
            return
        key = up


def _item_id(key: str, group: bool) -> str:
    return hashlib.blake2b(
        ("group:" if group else "path:").encode() + key.encode("utf-8", "surrogatepass"), digest_size=8
    ).hexdigest()


def _anchor(path: str) -> str:
    drive, _ = os.path.splitdrive(os.path.abspath(path))
    return drive + os.sep if drive else os.sep


def default_volume_of(root_path: str) -> str:
    """Mount point of the volume holding a scan root (falls back to the path's drive or ``/``)."""
    from ..scan.volume import volume_for

    try:
        return volume_for(root_path).mount_point
    except (OSError, ValueError, RuntimeError):
        return _anchor(root_path)


class Basket:
    def __init__(self, *, volume_of: Callable[[str], str] | None = None) -> None:
        self._lock = threading.RLock()
        self._entries: dict[str, _Entry] = {}  # id -> entry, in the order they were added
        self._cover: dict[str, str] = {}  # covered key -> item id
        self._sorted: list[str] = []  # covered keys, sorted
        self._groups: dict[str, str] = {}  # folder key -> id of its group
        self._volume_of = volume_of or default_volume_of
        self._volumes: dict[str, str] = {}  # scan root path -> mount point
        self._absorbed_paths: dict[str, str] = {}  # during one add(): absorbed id -> its path
        self.version = 0  # bumped by every change (pushes compare it)

    # ------------------------------------------------------------------ queries

    def __len__(self) -> int:
        return len(self._entries)

    def items(self) -> list[BasketItem]:
        """Copies of the items, in the order they were added."""
        with self._lock:
            return [replace(e.item, members=list(e.item.members)) for e in self._entries.values()]

    def get(self, item_id: str) -> BasketItem | None:
        with self._lock:
            entry = self._entries.get(item_id)
            return replace(entry.item, members=list(entry.item.members)) if entry else None

    def contains(self, path: str) -> str | None:
        """Id of the item that covers ``path`` (it or an ancestor is in the basket), else None."""
        with self._lock:
            return self._covering(_key(path))

    def freeable(self) -> dict[str, int]:
        """Volume mount point → bytes the basket would free (ready and locked items)."""
        with self._lock:
            out: dict[str, int] = {}
            for e in self._entries.values():
                if e.item.status in ("ready", "locked"):
                    out[e.item.volume] = out.get(e.item.volume, 0) + e.item.alloc
            return out

    def targets(self, ids: Sequence[str] | None = None) -> list[Target]:
        """What executing ``ids`` (default: everything) touches: item paths and group members.

        Missing and protected items are left out (the pre-execution check would skip them anyway).
        """
        with self._lock:
            chosen = (
                self._entries.values()
                if ids is None
                else [self._entries[i] for i in ids if i in self._entries]
            )
            out: list[Target] = []
            for e in chosen:
                if e.item.status in ("missing", "protected"):
                    continue
                if e.group:
                    sizes = (
                        e.member_sizes
                        if len(e.member_sizes) == len(e.item.members)
                        else [0] * len(e.item.members)
                    )
                    out.extend(Target(e.item.id, p, s) for p, s in zip(e.item.members, sizes, strict=True))
                else:
                    out.append(Target(e.item.id, e.item.path, max(e.item.size, e.measure.base, e.item.alloc)))
            return out

    def lock_targets(self) -> list[str]:
        """Paths to hand to ``locks.find_lockers`` (item paths; members of groups)."""
        return [t.path for t in self.targets()]

    # ------------------------------------------------------------------ changes

    def add(
        self, store: NodeStore, scan_id: str, nodes: list[int], members: list[int] | None = None
    ) -> AddResult:
        """Add nodes of one scan; with ``members``, ``nodes`` is one folder and the members (its
        direct children) become one small-item group (UI-W5-29)."""
        with self._lock:
            result = AddResult()
            if members is not None:
                if len(nodes) != 1:
                    raise ValueError("a small-item group takes exactly one folder")
                self._add_group(store, scan_id, nodes[0], members, result)
            else:
                for node in nodes:
                    self._add_node(store, scan_id, node, result)
            # an item added and absorbed within this call is simply merged into its absorber
            gone = set(result.absorbed) & set(result.added)
            if gone:
                result.added = [i for i in result.added if i not in gone]
                result.absorbed = [i for i in result.absorbed if i not in gone]
                for item_id in gone:
                    path = self._absorbed_paths.get(item_id, "")
                    into = self._covering(_key(path)) if path else None
                    if into is not None:
                        result.merged.append(Merged(path, into))
            self._absorbed_paths.clear()
            self._settle()
            return result

    def remove(self, ids: list[str]) -> None:
        with self._lock:
            for item_id in ids:
                entry = self._entries.get(item_id)
                if entry is not None:
                    self._drop(entry)
            self._settle()

    def clear(self) -> None:
        with self._lock:
            self._entries.clear()
            self._cover.clear()
            self._sorted.clear()
            self._groups.clear()
            self.version += 1

    def refresh(self, store_of: Callable[[str], NodeStore | None]) -> list[str]:
        """Find every item again in its scan's tree; recompute what changed (after tree changes).

        Items whose scan is closed (``store_of`` returns None) keep what is known. Returns the ids
        of items that are newly missing (the service removes them and tells the user, UI-W5-48).
        """
        with self._lock:
            newly_missing: list[str] = []
            indexes: dict[int, PathIndex] = {}  # per store, for this call only (no strong cache)
            for entry in list(self._entries.values()):
                store = store_of(entry.item.scan_id)
                if store is None:
                    continue
                index = indexes.get(id(store))
                if index is None:
                    index = indexes[id(store)] = PathIndex(store)
                paths = entry.item.members if entry.group else [entry.item.path]
                nodes = [index.locate(p)[0] for p in paths]
                found = [n for n in nodes if n is not None]
                was = entry.item.status
                if not found:
                    entry.missing_members = len(paths) if entry.group else 0
                    entry.measure = _Measure()
                    entry.fingerprint = b""
                    entry.item.status = "missing"
                    if was != "missing":
                        newly_missing.append(entry.item.id)
                    continue
                entry.missing_members = len(paths) - len(found) if entry.group else 0
                if entry.group:
                    entry.item.name = self._group_name(entry.item.path, len(found))
                mark = _fingerprint(store, found)
                if mark != entry.fingerprint:
                    entry.measure = _measure(store, found, members=entry.group)
                    entry.fingerprint = mark
                    if entry.group:
                        sizes = dict(zip(found, entry.measure.member_sizes, strict=True))
                        entry.member_sizes = [sizes.get(n, 0) if n is not None else 0 for n in nodes]
                entry.item.status = self._status(entry, check=True)
            self._settle()
            return newly_missing

    def set_lockers(self, lockers: Mapping[str, list[str]]) -> None:
        """Apply ``locks.find_lockers`` results: items (or members) held open become ``locked``."""
        with self._lock:
            held = {_key(p): names for p, names in lockers.items() if names}
            for entry in self._entries.values():
                names: list[str] = []
                for k in entry.covered():
                    for n in held.get(k, ()):
                        if n not in names:
                            names.append(n)
                entry.lock_note = TEXT["locked"].format(names=TEXT["list_sep"].join(names)) if names else ""
                if entry.item.status in ("ready", "locked"):
                    entry.item.status = self._status(entry)
            self._settle()

    def apply_results(self, results: Iterable[OpResult]) -> list[BasketItem]:
        """Take executed paths out of the basket: done items leave it, groups lose done members,
        failures stay with their reason (UI-W5-43). Returns what left (for undo: :meth:`reinstate`)."""
        with self._lock:
            by_key = {_key(r.path): r for r in results}
            left: list[BasketItem] = []
            for entry in list(self._entries.values()):
                if any(k in by_key for k in entry.covered()):
                    entry.error_note = ""
                if entry.group:
                    self._apply_group(entry, by_key, left)
                    continue
                res = by_key.get(entry.key)
                if res is None:
                    continue
                if res.ok:
                    left.append(replace(entry.item, members=[]))
                    self._drop(entry)
                    continue
                entry.error_note = TEXT["failed"].format(message=res.message)
                if res.code == "missing":
                    entry.item.status = "missing"
                elif res.code == "locked":
                    entry.lock_note = res.message
                    entry.item.status = "locked"
            self._settle()
            return left

    def reinstate(self, items: Iterable[BasketItem]) -> AddResult:
        """Put items back (undo of a move to the trash); numbers are recomputed by :meth:`refresh`."""
        with self._lock:
            result = AddResult()
            for item in items:
                known = _Measure(base=item.alloc, size=item.size, files=item.files, dirs=item.dirs)
                if item.kind == "group":
                    keys = [_key(m) for m in item.members]
                    known.member_sizes = [0] * len(keys)
                    self._insert_group(
                        item.path, item.scan_id, item.volume, list(item.members), keys, known, None, result
                    )
                    continue
                entry = _Entry(
                    item=replace(item, status="ready", note=""), key=_key(item.path), measure=known
                )
                self._insert(entry, result)
            self._settle()
            return result

    # ------------------------------------------------------------------ adding

    def _check_node(self, store: NodeStore, node: int) -> str | None:
        c = store.cols
        if node <= 0 or node >= len(store) or c["flags"][node] & np.uint32(Flag.GONE):
            return TEXT["bad_node"]
        if any(c["flags"][n] & np.uint32(Flag.VIRTUAL) for n in (node, *store.ancestors(node))):
            return TEXT["virtual"]
        return None

    def _add_node(self, store: NodeStore, scan_id: str, node: int, result: AddResult) -> None:
        bad = self._check_node(store, node)
        path = store.path(node) if 0 < node < len(store) else str(node)
        if bad is not None:
            result.refused.append(Refused(path, bad))
            return
        reason = protected_reason(path)
        if reason is not None:
            result.refused.append(Refused(path, reason))
            return
        key = _key(path)
        into = self._covering(key)
        if into is not None:
            result.merged.append(Merged(path, into))
            return
        kind = _KIND_NAMES.get(int(store.cols["kind"][node]), "other")
        item = BasketItem(
            id=_item_id(key, False),
            path=path,
            name=store.name(node) if store.cols["depth"][node] > 1 else path,
            kind=kind,
            scan_id=scan_id,
            volume=self._volume(store, node),
        )
        entry = _Entry(
            item=item, key=key, measure=_measure(store, [node]), fingerprint=_fingerprint(store, [node])
        )
        self._insert(entry, result, check=False)

    def _add_group(
        self, store: NodeStore, scan_id: str, folder: int, members: list[int], result: AddResult
    ) -> None:
        bad = self._check_node(store, folder)
        folder_path = store.path(folder) if 0 < folder < len(store) else str(folder)
        if bad is None and store.cols["kind"][folder] != Kind.DIR:
            bad = TEXT["not_folder"]
        if bad is not None:
            result.refused.append(Refused(folder_path, bad))
            return
        folder_key = _key(folder_path)
        into = self._covering(folder_key)
        if into is not None:
            result.merged.append(Merged(folder_path, into))
            return
        paths: list[str] = []
        keys: list[str] = []
        nodes: list[int] = []
        seen: set[str] = set()
        for m in members:
            bad = self._check_node(store, m)
            path = store.path(m) if 0 < m < len(store) else str(m)
            if bad is None and int(store.cols["parent"][m]) != folder:
                bad = TEXT["not_member"]
            reason = bad or protected_reason(path)
            if reason is not None:
                result.refused.append(Refused(path, reason))
                continue
            key = _key(path)
            if key in seen:
                continue
            seen.add(key)
            covering = self._covering(key)
            if covering is not None:
                result.merged.append(Merged(path, covering))
                continue
            paths.append(path)
            keys.append(key)
            nodes.append(m)
        if not paths:
            return
        self._insert_group(
            folder_path,
            scan_id,
            self._volume(store, folder),
            paths,
            keys,
            _measure(store, nodes, members=True),
            _fingerprint(store, nodes),
            result,
        )

    def _insert_group(
        self,
        folder_path: str,
        scan_id: str,
        volume: str,
        paths: list[str],
        keys: list[str],
        measure: _Measure,
        fingerprint: bytes | None,
        result: AddResult,
    ) -> None:
        """Add members to the group of ``folder_path`` (made if needed). ``measure`` covers the
        given members; ``fingerprint`` is None when they are not known in a store right now."""
        sizes = measure.member_sizes if len(measure.member_sizes) == len(paths) else [0] * len(paths)
        folder_key = _key(folder_path)
        into = self._covering(folder_key)
        if into is not None:
            result.merged.extend(Merged(p, into) for p in paths)
            return
        fresh = [(p, k, s) for p, k, s in zip(paths, keys, sizes, strict=True) if self._covering(k) is None]
        for p, k, _ in zip(paths, keys, sizes, strict=True):
            if (hit := self._covering(k)) is not None:
                result.merged.append(Merged(p, hit))
        if not fresh:
            return
        for _, k, _ in fresh:  # whatever lies below a new member is now inside the group
            for below in self._below(k):
                if below in self._entries and self._entries[below].key != folder_key:
                    result.absorbed.append(below)
                    self._absorbed_paths[below] = self._entries[below].item.path
                    self._drop(self._entries[below])
        existing = self._groups.get(folder_key)
        entry = self._entries.get(existing) if existing else None
        if entry is None:
            item = BasketItem(
                id=_item_id(folder_key, True),
                path=folder_path,
                name="",
                kind="group",
                scan_id=scan_id,
                volume=volume,
            )
            entry = _Entry(item=item, key=folder_key)
            self._entries[item.id] = entry
            self._groups[folder_key] = item.id
        entry.item.members.extend(p for p, _, _ in fresh)
        entry.member_keys.extend(k for _, k, _ in fresh)
        if len(entry.member_sizes) != len(entry.item.members) - len(fresh):
            entry.member_sizes = [0] * (len(entry.item.members) - len(fresh))
        entry.member_sizes.extend(s for _, _, s in fresh)
        entry.item.name = self._group_name(folder_path, len(entry.item.members))
        self._index_add(entry.item.id, [k for _, k, _ in fresh])
        if existing is None and fingerprint is not None and len(fresh) == len(paths):
            entry.measure = measure
            entry.fingerprint = fingerprint
        else:  # grown, or partly known: the next refresh measures the whole group again
            entry.measure = _combine(entry.measure, measure) if existing is not None else measure
            entry.fingerprint = b""
        entry.item.status = self._status(entry)
        result.added.append(entry.item.id)
        self.version += 1

    def _insert(self, entry: _Entry, result: AddResult, *, check: bool = True) -> None:
        into = self._covering(entry.key)
        if into is not None:
            result.merged.append(Merged(entry.item.path, into))
            return
        for below in self._below(entry.key):
            if below in self._entries:
                result.absorbed.append(below)
                self._absorbed_paths[below] = self._entries[below].item.path
                self._drop(self._entries[below])
        self._entries[entry.item.id] = entry
        self._index_add(entry.item.id, [entry.key])
        entry.item.status = self._status(entry, check=check)
        result.added.append(entry.item.id)
        self.version += 1

    def _apply_group(self, entry: _Entry, by_key: Mapping[str, OpResult], left: list[BasketItem]) -> None:
        done: list[int] = []
        failed: list[OpResult] = []
        for i, k in enumerate(entry.member_keys):
            res = by_key.get(k)
            if res is None:
                continue
            if res.ok:
                done.append(i)
            else:
                failed.append(res)
        if done:
            moved = [entry.item.members[i] for i in done]
            left.append(
                replace(entry.item, members=moved, name=self._group_name(entry.item.path, len(moved)))
            )
            keep = sorted(set(range(len(entry.member_keys))) - set(done))
            gone_keys = [entry.member_keys[i] for i in done]
            sizes = (
                entry.member_sizes
                if len(entry.member_sizes) == len(entry.member_keys)
                else [0] * len(entry.member_keys)
            )
            freed = sum(sizes[i] for i in done)
            entry.item.members = [entry.item.members[i] for i in keep]
            entry.member_keys = [entry.member_keys[i] for i in keep]
            entry.member_sizes = [sizes[i] for i in keep]
            self._index_remove(gone_keys)
            if not entry.item.members:
                self._drop(entry)
                return
            entry.measure.base = max(0, entry.measure.base - freed)
            entry.measure.size = max(0, entry.measure.size - freed)
            entry.fingerprint = b""
            entry.item.name = self._group_name(entry.item.path, len(entry.item.members))
            self.version += 1
        if failed:
            entry.error_note = TEXT["members_failed"].format(count=len(failed), message=failed[0].message)
            self.version += 1

    # ------------------------------------------------------------------ bookkeeping

    def _covering(self, key: str) -> str | None:
        for k in _ancestors_or_self(key):
            hit = self._cover.get(k)
            if hit is not None:
                return hit
        return None

    def _below(self, key: str) -> list[str]:
        """Ids of items with a covered key strictly below ``key``."""
        prefix = _prefix(key)
        at = bisect.bisect_left(self._sorted, prefix)
        ids: list[str] = []
        while at < len(self._sorted) and self._sorted[at].startswith(prefix):
            item_id = self._cover[self._sorted[at]]
            if item_id not in ids:
                ids.append(item_id)
            at += 1
        return ids

    def _index_add(self, item_id: str, keys: list[str]) -> None:
        for k in keys:
            self._cover[k] = item_id
        if len(keys) < 32:
            for k in keys:
                bisect.insort(self._sorted, k)
        else:
            self._sorted.extend(keys)
            self._sorted.sort()

    def _index_remove(self, keys: list[str]) -> None:
        for k in keys:
            self._cover.pop(k, None)
        if len(keys) < 32:
            for k in keys:
                at = bisect.bisect_left(self._sorted, k)
                if at < len(self._sorted) and self._sorted[at] == k:
                    del self._sorted[at]
        else:
            gone = set(keys)
            self._sorted = [k for k in self._sorted if k not in gone]

    def _drop(self, entry: _Entry) -> None:
        self._entries.pop(entry.item.id, None)
        self._index_remove(entry.covered())
        if entry.group and self._groups.get(entry.key) == entry.item.id:
            del self._groups[entry.key]
        self.version += 1

    def _volume(self, store: NodeStore, node: int) -> str:
        root = node
        parent = store.cols["parent"]
        while int(parent[root]) > 0:
            root = int(parent[root])
        root_path = store.name(root)
        hit = self._volumes.get(root_path)
        if hit is None:
            hit = self._volumes[root_path] = self._volume_of(root_path)
        return hit

    @staticmethod
    def _group_name(folder_path: str, count: int) -> str:
        folder = os.path.basename(os.path.normpath(folder_path)) or folder_path
        return TEXT["group_name"].format(folder=folder, count=count)

    @staticmethod
    def _status(entry: _Entry, *, check: bool = False) -> Status:
        """Status of an item that exists; ``check``: ask :func:`protected_reason` again (disk)."""
        if check and not entry.group and protected_reason(entry.item.path) is not None:
            return "protected"
        if not check and entry.item.status == "protected":
            return "protected"
        return "locked" if entry.lock_note else "ready"

    def _settle(self) -> None:
        """Hard links across the basket, freeable bytes and notes of every item."""
        entries = list(self._entries.values())
        gain = np.zeros(len(entries), dtype=np.int64)
        short = np.zeros(len(entries), dtype=np.int64)  # names of incomplete link groups per item
        lacking = np.zeros(len(entries), dtype=np.int64)  # file items: other names not in the basket
        by_volume: dict[str, list[int]] = {}
        for i, e in enumerate(entries):
            if e.item.status in ("ready", "locked") and len(e.measure.ino):
                by_volume.setdefault(e.item.volume, []).append(i)
        for idx in by_volume.values():
            ino = np.concatenate([entries[i].measure.ino for i in idx])
            alloc = np.concatenate([entries[i].measure.link_alloc for i in idx])
            nlink = np.concatenate([entries[i].measure.nlink for i in idx])
            owner = np.concatenate([np.full(len(entries[i].measure.ino), i, dtype=np.int64) for i in idx])
            order = np.argsort(ino, kind="stable")  # within a link group: items in the order added
            ino_s = ino[order]
            starts = np.flatnonzero(np.r_[True, ino_s[1:] != ino_s[:-1]])
            counts = np.diff(np.r_[starts, len(ino_s)])
            grp_alloc = np.maximum.reduceat(alloc[order], starts)
            grp_nlink = np.maximum.reduceat(nlink[order], starts)
            complete = counts >= grp_nlink
            first_owner = owner[order][starts]
            np.add.at(gain, first_owner[complete], grp_alloc[complete])
            group_of = np.empty(len(ino), dtype=np.int64)
            group_of[order] = np.repeat(np.arange(len(starts)), counts)
            incomplete = ~complete[group_of]
            np.add.at(short, owner[incomplete], 1)
            np.maximum.at(lacking, owner[incomplete], (grp_nlink - counts)[group_of][incomplete])
        for i, e in enumerate(entries):
            item = e.item
            if item.status in ("ready", "locked"):
                item.alloc = int(e.measure.base + gain[i])
            else:
                item.alloc = 0
            item.size, item.files, item.dirs = e.measure.size, e.measure.files, e.measure.dirs
            if short[i] and item.status in ("ready", "locked"):
                e.link_note = (
                    TEXT["links_file"].format(count=int(lacking[i]))
                    if item.kind == "file"
                    else TEXT["links_dir"].format(count=int(short[i]))
                )
            else:
                e.link_note = ""
            notes = [
                TEXT["missing"] if item.status == "missing" else "",
                TEXT["members_missing"].format(count=e.missing_members)
                if e.missing_members and item.status != "missing"
                else "",
                e.lock_note,
                e.error_note,
                e.link_note,
            ]
            item.note = TEXT["note_sep"].join(n for n in notes if n)
        self.version += 1


# --------------------------------------------------------------------------- measuring


def _fingerprint(store: NodeStore, nodes: Sequence[int]) -> bytes:
    """Digest of the node state a measure depends on (ids, ranges, totals, own size and flags)."""
    c = store.cols
    ids = np.asarray(nodes, dtype=np.int64)
    h = hashlib.blake2b(digest_size=16)
    h.update(id(store).to_bytes(8, "little", signed=False))
    h.update(ids.tobytes())
    for col in (
        "child_start",
        "child_count",
        "tot_size",
        "tot_alloc",
        "tot_files",
        "tot_dirs",
        "size",
        "alloc",
        "flags",
        "nlink",
    ):
        h.update(np.ascontiguousarray(c[col][ids]).tobytes())
    return h.digest()


def _measure(store: NodeStore, nodes: Sequence[int], *, members: bool = False) -> _Measure:
    """Size, counts and freeable bytes of ``nodes`` (each with its subtree), §2.3.

    ``members``: the nodes are a group's members, so a member folder counts as a folder itself.
    """
    c = store.cols
    out = _Measure()
    covered: list[np.ndarray] = []
    for node in nodes:
        kind = int(c["kind"][node])
        dup = bool(c["flags"][node] & np.uint32(Flag.HARDLINK_DUP))
        if kind == Kind.DIR:
            below = subtree_ids(store, node)
            covered.append(below)
            size, alloc = int(c["tot_size"][node]), int(c["tot_alloc"][node])
            out.files += int(c["tot_files"][node])
            out.dirs += int(c["tot_dirs"][node]) + members
            out.member_sizes.append(max(size, alloc))
            out.size += size
            out.base += alloc
            continue
        covered.append(np.array([node], dtype=np.int64))
        size = int(c["size"][node])
        alloc = 0 if dup else int(c["alloc"][node])
        out.size += size
        out.base += alloc
        out.files += kind == Kind.FILE
        out.member_sizes.append(max(size, int(c["alloc"][node])))
    ids = np.concatenate(covered) if covered else np.empty(0, dtype=np.int64)
    if not len(ids):
        return out
    kinds = c["kind"][ids]
    flags = c["flags"][ids]
    counted = (flags & np.uint32(Flag.HARDLINK_DUP)) == 0
    cloud = ((flags & np.uint32(Flag.CLOUD)) != 0) & (kinds != Kind.DIR)
    nlinks = c["nlink"][ids].astype(np.int64)
    inos = c["ino"][ids]
    linked = (kinds == Kind.FILE) & (nlinks > 1) & (inos != 0) & ~cloud
    allocs = c["alloc"][ids].astype(np.int64)
    out.base -= int(allocs[counted & cloud].sum())  # a placeholder frees nothing locally
    out.base -= int(allocs[counted & linked].sum())  # counted again by the basket-wide link pass
    out.base = max(0, out.base)
    out.ino = inos[linked].astype(np.uint64)
    out.link_alloc = allocs[linked]
    out.nlink = nlinks[linked]
    return out


def _combine(a: _Measure, b: _Measure) -> _Measure:
    return _Measure(
        base=a.base + b.base,
        size=a.size + b.size,
        files=a.files + b.files,
        dirs=a.dirs + b.dirs,
        ino=np.concatenate([a.ino, b.ino]),
        link_alloc=np.concatenate([a.link_alloc, b.link_alloc]),
        nlink=np.concatenate([a.nlink, b.nlink]),
        member_sizes=a.member_sizes + b.member_sizes,
    )


__all__ = [
    "TEXT",
    "AddResult",
    "Basket",
    "BasketItem",
    "ItemKind",
    "Merged",
    "Refused",
    "Status",
    "Target",
    "default_volume_of",
]
