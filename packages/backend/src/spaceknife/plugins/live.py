"""What enabled plugins hear of while they run, besides their own calls (docs/plugins.md, 「插件进程听到
的变化」):

- the ``tree_changed`` hook: the tree of an open scan changed (file changes applied by its monitor,
  「重新扫描」, a scan source listed again). Per plugin and scan the changes of about a second come
  together in one call, and a plugin whose hook still runs gets the next one when it is done (never
  a queue of them); at most :data:`TREE_FOLDERS` folders are listed, ``more`` counts the rest. Each
  folder comes with its path and its size on disk (``tot_alloc``) before and after the changes:
  ``before`` as the monitor read it right before the first of them touched the folder (a folder
  they added: 0; :class:`SizesBefore`), ``after`` as it was right after the last of them was
  applied, so the calls of one plugin follow on from each other (one call's ``after`` is the next
  one's ``before``). The folders follow every remap, also while the hook runs: a hook that met a
  reorganised tree (``STALE_TREE``: live monitoring moved rows meanwhile) is called again with the
  new ids (host.py ``_hook``) and with the changes that came meanwhile (one call for both), and
  after :data:`~.host.HOOK_STALE_RETRIES` tries its changes go with the next ones (at most
  :data:`TREE_REQUEUES` times in a row);
- the ``snapshots`` hook (``snapshots.read`` only): snapshots were saved or pruned;
- notices (``ctx.notify``, ``notify`` only): checked, bounded, at most one per plugin every
  :data:`NOTICE_GAP_S` seconds, sent to the windows as ``plugin.notice`` (a system notification
  while the window is minimized is the frontend's part).
"""

from __future__ import annotations

import asyncio
import functools
import itertools
import json
import time
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import numpy as np
from loguru import logger

from spaceknife_protocol import plugins as pp
from spaceknife_protocol.methods import EVENT_PLUGIN_NOTICE

from ..extensions import BUILTIN, EXTENSIONS
from ..store.nodes import Kind

if TYPE_CHECKING:
    from ..server.session import Session
    from ..store.nodes import NodeStore
    from .host import PluginHost
    from .store import PluginRecord

TREE_EVENT_S = 1.0  # tree_changed: the changes of this long come together (per plugin and scan)
TREE_FOLDERS = 256  # folders one tree_changed lists (``more`` counts the others)
TREE_REQUEUES = 5  # changes a hook kept meeting a reorganised tree with: sent again this often, then dropped
# folders whose sizes before and after one batch (one plugin's pending changes) are kept, the folders
# above the changes included (more: before is not known); ~200 B each while they wait (about a second)
SIZES_FOLDERS = 32_768
SNAPSHOTS_EVENT_S = 0.5  # the snapshots hook: saves and prunes of this long come together
SNAPSHOT_PATHS = 64  # paths one snapshots call lists (of saved, of removed)
NOTICE_GAP_S = 5.0  # a plugin's notices: at most one in this many seconds (the SDK holds to it too)
NOTICE_ARGS_MAX = 4096  # a notice's command args, as JSON


@dataclass(slots=True)
class _TreeDue:
    """The changes of one scan a plugin has not heard of yet (or is hearing of: in flight)."""

    epoch: int
    folders: dict[int, None] = field(default_factory=dict)  # an ordered set, at most TREE_FOLDERS
    # every folder the changes involve, the folders above them included -> (its size on disk right
    # before the first of them, right after the last); None: not known (the tree was replaced, a
    # scan source listed again, too many folders)
    sizes: dict[int, tuple[int, int]] | None = field(default_factory=dict)
    more: int = 0
    changes: int = 0

    def add(
        self, folders: Sequence[int], changes: int, sizes: Mapping[int, tuple[int, int]] | None = None
    ) -> None:
        """Changes that came after these: their folders, and the sizes (before, after) of the
        folders they involve (None: not known, and then not known for the whole lot)."""
        self.changes += changes
        for f in folders:
            if f in self.folders:
                continue
            if len(self.folders) >= TREE_FOLDERS:
                self.more += 1
            else:
                self.folders[f] = None
        mine = self.sizes
        if sizes is None or mine is None:
            self.sizes = None
            return
        for f, (b, a) in sizes.items():
            had = mine.get(f)
            mine[f] = (b if had is None else had[0], a)  # before: the first one's; after: the last
        if len(mine) > SIZES_FOLDERS:
            self.sizes = None

    def remap(self, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        """Node ids changed: the folders (and the sizes they come with) take their new ids."""
        if self.folders:
            moved = new_ids(np.fromiter(self.folders, dtype=np.int64, count=len(self.folders)))
            self.folders = dict.fromkeys(int(m) for m in moved.tolist() if m >= 0)
        if self.sizes:
            moved = new_ids(np.fromiter(self.sizes, dtype=np.int64, count=len(self.sizes)))
            self.sizes = {
                int(m): s for m, s in zip(moved.tolist(), self.sizes.values(), strict=True) if m >= 0
            }

    def forget_ids(self, roots: Sequence[int]) -> None:
        """The tree was replaced without a remap: what changed is somewhere below the roots."""
        had = bool(self.folders)
        self.folders = {}
        self.sizes = None
        if had:
            self.add([int(r) for r in roots], 0)

    def then(self, newer: _TreeDue) -> _TreeDue:
        """These changes followed by ``newer``, of the same epoch: one lot (each folder's size
        before is the one before these, after the one after ``newer``)."""
        out = _TreeDue(epoch=newer.epoch, more=self.more + newer.more)
        out.add(list(self.folders), self.changes, self.sizes)
        out.add(list(newer.folders), newer.changes, newer.sizes)
        return out

    def absorb(self, newer: _TreeDue) -> None:
        """:meth:`then`, in place (the hook hearing of these is called again with ``newer`` too)."""
        both = self.then(newer)
        self.epoch, self.folders, self.sizes = both.epoch, both.folders, both.sizes
        self.more, self.changes = both.more, both.changes


def add_sizes(store: NodeStore, folders: Iterable[int], into: dict[int, int]) -> None:
    """Each folder of ``folders`` and every folder above it that ``into`` does not have yet: its
    size on disk as the tree has it now (``tot_alloc``, with everything below it)."""
    c = store.cols
    tot, parent = c["tot_alloc"], c["parent"]
    n = len(store)
    for folder in folders:
        f = int(folder)
        while 0 < f < n and f not in into:  # up to a root (its parent is the top, 0)
            into[f] = int(tot[f])
            up = int(parent[f])
            if up == f:
                break
            f = up


class SizesBefore:
    """What each folder took on disk (``tot_alloc``) right before a batch of changes first touched
    it, for ``tree_changed`` (``TreeChange.before``). The scan's monitor calls :meth:`take` before
    it applies each change, with the folder whose totals the change starts from (a changed file's
    folder, a folder it lists again): that folder and every one above it are read, each the first
    time only; :meth:`new` for the folders the batch adds (they took nothing before it); and
    :meth:`remap` when the batch moves rows. What a folder added by the batch holds (its subtree,
    scanned before the batch is announced) is then in its ``after`` and its parents' too. ``sizes``
    is None once more than :data:`SIZES_FOLDERS` folders are involved; :meth:`of` gives None when
    the tree's ids changed some other way meanwhile."""

    __slots__ = ("epoch", "sizes", "store")

    def __init__(self, store: NodeStore) -> None:
        self.store = store
        self.epoch = int(store.epoch)
        self.sizes: dict[int, int] | None = {}

    def take(self, folders: Iterable[int]) -> None:
        sizes = self.sizes
        if sizes is None:
            return
        if int(self.store.epoch) != self.epoch:  # ids of another tree now: what was taken is lost
            self.sizes = None
            return
        add_sizes(self.store, folders, sizes)
        if len(sizes) > SIZES_FOLDERS:
            self.sizes = None

    def new(self, folders: Iterable[int]) -> None:
        if self.sizes is not None:
            for f in folders:
                self.sizes.setdefault(int(f), 0)

    def remap(self, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        """The batch moved rows (the epoch moved on with them)."""
        self.epoch = int(self.store.epoch)
        if self.sizes:
            moved = new_ids(np.fromiter(self.sizes, dtype=np.int64, count=len(self.sizes)))
            self.sizes = {
                int(m): s for m, s in zip(moved.tolist(), self.sizes.values(), strict=True) if m >= 0
            }

    def of(self, store: NodeStore) -> dict[int, int] | None:
        """The sizes, when ``store`` (the scan's tree now) is the tree whose ids they use."""
        return self.sizes if store is self.store and int(store.epoch) == self.epoch else None


def sizes_now(store: NodeStore, folders: Iterable[int]) -> dict[int, int]:
    """``before`` for an announcement of changes already applied that changed no size (or one that
    only says where to look): the folders and every one above them, as they are now."""
    out: dict[int, int] = {}
    add_sizes(store, folders, out)
    return out


def changed_folders(store: NodeStore, changes: Mapping[int, int]) -> list[int]:
    """The folders where something changed: the folder of each changed file, a changed folder and
    the folder holding it. No changes named (a scan source listed again): its roots."""
    n = len(store)
    if not changes:
        return [int(r) for r in store.roots()]
    ids = np.fromiter(changes.keys(), dtype=np.int64, count=len(changes))
    ids = ids[(ids >= 0) & (ids < n)]
    c = store.cols
    parents = c["parent"][ids].astype(np.int64)
    dirs = ids[c["kind"][ids] == Kind.DIR]
    out = np.unique(np.concatenate([parents, dirs]))
    out = out[(out >= 0) & (out < n)]
    return [int(x) for x in out.tolist()]


class Live:
    def __init__(self, host: PluginHost) -> None:
        self.host = host
        self._tree: dict[tuple[str, str], _TreeDue] = {}  # (plugin, scan) -> what it has not heard
        self._inflight: dict[tuple[str, str], _TreeDue] = {}  # … what its hook is hearing of now
        self._requeued: dict[tuple[str, str], int] = {}  # … sent again in a row (TREE_REQUEUES)
        self._tree_tasks: dict[tuple[str, str], asyncio.Task[None]] = {}
        self._snaps: dict[str, tuple[list[str], list[str]]] = {}  # plugin -> (saved, removed)
        self._snap_task: asyncio.Task[None] | None = None
        self._notice_at: dict[str, float] = {}
        self._notice_ids = itertools.count(1)

    def close(self) -> None:
        for task in [*self._tree_tasks.values(), self._snap_task]:
            if task is not None:
                task.cancel()
        self._tree.clear()
        self._inflight.clear()
        self._requeued.clear()
        self._snaps.clear()

    def forget(self, plugin: str) -> None:
        """The plugin was turned off: what it had not heard of yet goes."""
        for key in [k for k in self._tree if k[0] == plugin]:
            self._tree.pop(key, None)
        for key in [k for k in self._requeued if k[0] == plugin]:
            self._requeued.pop(key, None)
        for key in [k for k in self._tree_tasks if k[0] == plugin]:
            self._tree_tasks.pop(key).cancel()
        self._snaps.pop(plugin, None)
        self._notice_at.pop(plugin, None)

    def forget_scan(self, scan_id: str) -> None:
        for key in [k for k in self._tree if k[1] == scan_id]:
            self._tree.pop(key, None)
        for key in [k for k in self._requeued if k[1] == scan_id]:
            self._requeued.pop(key, None)

    def _listeners(self, event: str) -> list[PluginRecord]:
        owners = {c.owner for c in EXTENSIONS.of("hook") if c.spec.event == event and c.owner != BUILTIN}
        out = []
        for pid in sorted(owners):
            rec = self.host.records.get(pid)
            if rec is not None and rec.state == "enabled":
                out.append(rec)
        return out

    def _start(self, coro: Any) -> asyncio.Task[None]:
        task: asyncio.Task[None] = asyncio.get_running_loop().create_task(coro)
        background = self.host._background  # MockHost.settled() waits for these too
        background.add(task)
        task.add_done_callback(background.discard)
        return task

    # ------------------------------------------------------------- tree_changed

    def tree_changed(
        self, scan_id: str, changes: Mapping[int, int], before: Mapping[int, int] | None = None
    ) -> None:
        """The tree of ``scan_id`` changed (``views.flash``: node -> change of its allocation),
        just now: the changes are applied. ``before``: each folder they involve (and every one
        above) -> its size on disk before them (:class:`SizesBefore`); None: not known."""
        host = self.host
        if host._loop is None or host.closed:
            return
        recs = self._listeners("tree_changed")
        handle = host.server.scans.scans.get(scan_id) if recs else None
        if handle is None:
            return
        store = handle.store
        folders = changed_folders(store, changes)
        sizes: dict[int, tuple[int, int]] | None = None
        if before is not None:  # after: as they are now, right after the changes
            n, tot = len(store), store.cols["tot_alloc"]
            sizes = {int(f): (int(b), int(tot[f])) for f, b in before.items() if 0 < f < n}
        for rec in recs:
            key = (rec.id, scan_id)
            due = self._tree.get(key)
            if due is None:
                due = self._tree[key] = _TreeDue(epoch=int(store.epoch))
            elif due.epoch != store.epoch:  # an epoch without a remap: from the top
                due.forget_ids(store.roots())
                due.epoch = int(store.epoch)
            due.add(folders, max(1, len(changes)), sizes)
            task = self._tree_tasks.get(key)
            if task is None or task.done():
                self._tree_tasks[key] = self._start(self._deliver_tree(rec.id, scan_id))

    def tree_remapped(self, scan_id: str, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        """Node ids of ``scan_id`` changed (a relist moved rows, a compaction): the folders a plugin
        has not heard of yet follow, and so do those its hook is hearing of right now (it gets them
        again with these ids when it met the new tree). Each remap moves the epoch on by one."""
        handle = self.host.server.scans.scans.get(scan_id)
        if handle is None:
            return
        store = handle.store
        epoch = int(store.epoch)
        for dues in (self._tree, self._inflight):
            for key, due in dues.items():
                if key[1] != scan_id:
                    continue
                if due.epoch == epoch - 1:
                    due.remap(new_ids)
                elif due.epoch != epoch:  # an epoch went by without a remap: its ids mean nothing now
                    due.forget_ids(store.roots())
                due.epoch = epoch

    @staticmethod
    def _in_step(due: _TreeDue, store: NodeStore) -> None:
        """Remaps keep ``due`` in step with the tree; any other new epoch: from the top."""
        if due.epoch != store.epoch:
            due.forget_ids(store.roots())
            due.epoch = int(store.epoch)

    def _change(self, scan_id: str, due: _TreeDue) -> tuple[dict[str, Any], int] | None:
        """The hook's args for ``due`` in the tree's ids now, and their epoch (None: the scan
        went). Sizes as they were before and after these changes (the tree may have moved on
        since: later changes come with the next call); not known: before None, after as now."""
        handle = self.host.server.scans.scans.get(scan_id)
        if handle is None:
            return None
        store = handle.store
        self._in_step(due, store)
        n = len(store)
        folders = [f for f in due.folders if 0 < f < n]
        tot = store.cols["tot_alloc"]
        sizes = due.sizes
        pairs = [None if sizes is None else sizes.get(f) for f in folders]
        after = [int(tot[f]) if p is None else p[1] for f, p in zip(folders, pairs, strict=True)]
        before: list[int | None] = [None if p is None else p[0] for p in pairs]
        change = {
            "scan_id": scan_id,
            "epoch": int(store.epoch),
            "version": int(store.version),
            "folders": folders,
            "paths": [store.path(f) for f in folders],
            "before": before,
            "after": after,
            "more": due.more,
            "changes": due.changes,
        }
        return {"change": change}, int(store.epoch)

    def _again(self, key: tuple[str, str], due: _TreeDue) -> tuple[dict[str, Any], int] | None:
        """The hook hearing of ``due`` met a reorganised tree and is called again: in the tree's
        ids now, and with the changes that came meanwhile (they made the new tree), so that what
        it is told is the tree it reads: one call for both, sizes before the first and after the
        last."""
        handle = self.host.server.scans.scans.get(key[1])
        newer = self._tree.pop(key, None) if handle is not None else None
        if handle is not None and newer is not None:
            self._in_step(due, handle.store)
            self._in_step(newer, handle.store)
            due.absorb(newer)
        return self._change(key[1], due)

    def _requeue(self, key: tuple[str, str], due: _TreeDue) -> None:
        """The hook kept meeting a reorganised tree: these changes go with the next ones (they
        follow remaps like those), at most TREE_REQUEUES times in a row."""
        n = self._requeued.get(key, 0) + 1
        handle = self.host.server.scans.scans.get(key[1])
        if n > TREE_REQUEUES or handle is None:
            self._requeued.pop(key, None)
            logger.bind(source=f"plugin:{key[0]}").warning(
                "hook tree_changed of {}: the tree of {} was reorganised every time it ran ({} deliveries "
                "in a row); these changes are dropped (does it read a tree kept from another call?)",
                key[0],
                key[1],
                n,
            )
            return
        self._requeued[key] = n
        store = handle.store
        self._in_step(due, store)
        newer = self._tree.get(key)
        if newer is not None:
            self._in_step(newer, store)
        self._tree[key] = due if newer is None else due.then(newer)

    async def _deliver_tree(self, plugin: str, scan_id: str) -> None:
        key = (plugin, scan_id)
        try:
            while key in self._tree:
                await asyncio.sleep(TREE_EVENT_S)  # what changes meanwhile comes along
                due = self._tree.pop(key, None)
                rec = self.host.records.get(plugin)
                if due is None or rec is None or rec.state != "enabled":
                    return
                first = self._change(scan_id, due)
                if first is None:
                    return
                self._inflight[key] = due  # remaps reach it while the hook runs
                try:
                    ok = await self.host._hook(
                        rec,
                        "tree_changed",
                        self.host.places.tree_change(scan_id, first[1]),
                        _again=functools.partial(self._again, key, due),
                        **first[0],
                    )
                finally:
                    self._inflight.pop(key, None)
                if ok:
                    self._requeued.pop(key, None)
                else:
                    self._requeue(key, due)
        finally:
            if self._tree_tasks.get(key) is asyncio.current_task():
                self._tree_tasks.pop(key, None)

    # ------------------------------------------------------------- snapshots

    def snapshots_changed(self, saved: Sequence[str], removed: Sequence[str]) -> None:
        if self.host._loop is None or self.host.closed or not (saved or removed):
            return
        recs = [r for r in self._listeners("snapshots") if "snapshots.read" in r.granted]
        for rec in recs:
            s, r = self._snaps.setdefault(rec.id, ([], []))
            s.extend(p for p in saved if p not in s)
            r.extend(p for p in removed if p not in r)
            del s[:-SNAPSHOT_PATHS], r[:-SNAPSHOT_PATHS]
        if recs and (self._snap_task is None or self._snap_task.done()):
            self._snap_task = self._start(self._deliver_snapshots())

    async def _deliver_snapshots(self) -> None:
        await asyncio.sleep(SNAPSHOTS_EVENT_S)
        due, self._snaps = self._snaps, {}
        for plugin, (saved, removed) in due.items():
            rec = self.host.records.get(plugin)
            if rec is not None and rec.state == "enabled":  # each on its own: a slow one holds no other
                self._start(
                    self.host._hook(
                        rec,
                        "snapshots",
                        self.host.places.nowhere(),
                        change={"saved": saved, "removed": removed},
                    )
                )

    # ------------------------------------------------------------- notices

    def notice(self, rec: PluginRecord, payload: Mapping[str, Any], session: Session | None) -> None:
        """``ctx.notify``: checked (permission, its own command, sizes, one per NOTICE_GAP_S), then
        sent to the window the call came from, or to every window."""
        log = logger.bind(source=f"plugin:{rec.id}")
        if "notify" not in rec.granted:
            log.warning("notice of {} dropped: it was not granted notify", rec.id)
            return
        now = time.monotonic()
        if now - self._notice_at.get(rec.id, -1e9) < NOTICE_GAP_S:
            log.debug("notice of {} dropped: one every {:g} s", rec.id, NOTICE_GAP_S)
            return
        command = payload.get("command")
        if command is not None:
            c = EXTENSIONS.get("command", str(command))
            if c is None or c.owner != rec.id:
                log.warning("notice of {}: {!r} is not one of its commands; no button", rec.id, command)
                command = None
        args = payload.get("args") if isinstance(payload.get("args"), dict) else {}
        try:
            too_big = len(json.dumps(args, ensure_ascii=False)) > NOTICE_ARGS_MAX
        except (TypeError, ValueError):
            too_big = True
        if too_big:
            log.warning(
                "notice of {}: its args are not JSON of at most {} bytes; left out", rec.id, NOTICE_ARGS_MAX
            )
            args = {}
        tone = payload.get("tone")
        title = str(payload.get("title") or "")[:120]
        self._notice_at[rec.id] = now
        ev = pp.PluginNoticeEvent(
            plugin=rec.id,
            id=f"{rec.id}#{next(self._notice_ids)}"[-64:],
            title=title,
            body=str(payload.get("body") or "")[:1000],
            tone=tone if tone in ("info", "ok", "warn", "danger") else "info",
            command=str(command) if command else None,
            button=str(payload.get("button") or "")[:40],
            args=dict(args or {}),
        )
        log.info("notice: {}", title)  # the log panel keeps what a notice said
        for s in [session] if session is not None else self.host._sessions():
            s.emit(EVENT_PLUGIN_NOTICE, ev)


__all__ = [
    "NOTICE_GAP_S",
    "SIZES_FOLDERS",
    "SNAPSHOTS_EVENT_S",
    "TREE_EVENT_S",
    "TREE_FOLDERS",
    "Live",
    "SizesBefore",
    "changed_folders",
    "sizes_now",
]
