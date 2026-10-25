"""What enabled plugins hear of while they run, besides their own calls (docs/plugins.md, 「插件进程听到
的变化」):

- the ``tree_changed`` hook: the tree of an open scan changed (file changes applied by its monitor,
  「重新扫描」, a scan source listed again). Per plugin and scan the changes of about a second come
  together in one call, and a plugin whose hook still runs gets the next one when it is done (never
  a queue of them); at most :data:`TREE_FOLDERS` folders are listed, ``more`` counts the rest. Each
  folder comes with its path and its size on disk before and after the changes. The folders follow
  every remap, also while the hook runs: a hook that met a reorganised tree (``STALE_TREE``: live
  monitoring moved rows meanwhile) is called again with the new ids (host.py ``_hook``), and after
  :data:`~.host.HOOK_STALE_RETRIES` tries its changes go with the next ones (at most
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
from collections.abc import Callable, Mapping, Sequence
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
GROWN_CHANGES = 200_000  # changes of one batch whose sizes are followed up the tree (more: sizes unknown)
GROWN_FOLDERS = 65_536  # folders whose growth one plugin's pending changes follow (more: unknown)
SNAPSHOTS_EVENT_S = 0.5  # the snapshots hook: saves and prunes of this long come together
SNAPSHOT_PATHS = 64  # paths one snapshots call lists (of saved, of removed)
NOTICE_GAP_S = 5.0  # a plugin's notices: at most one in this many seconds (the SDK holds to it too)
NOTICE_ARGS_MAX = 4096  # a notice's command args, as JSON


@dataclass(slots=True)
class _TreeDue:
    """The changes of one scan a plugin has not heard of yet (or is hearing of: in flight)."""

    epoch: int
    folders: dict[int, None] = field(default_factory=dict)  # an ordered set, at most TREE_FOLDERS
    # every folder holding a change -> what its size on disk changed by in these changes (its
    # size before = after - this); None: not known (the tree was replaced, too many changes)
    grown: dict[int, int] | None = field(default_factory=dict)
    more: int = 0
    changes: int = 0

    def add(self, folders: Sequence[int], changes: int, grown: Mapping[int, int] | None = None) -> None:
        self.changes += changes
        for f in folders:
            if f in self.folders:
                continue
            if len(self.folders) >= TREE_FOLDERS:
                self.more += 1
            else:
                self.folders[f] = None
        if grown is None or self.grown is None or len(self.grown) + len(grown) > GROWN_FOLDERS:
            self.grown = None
            return
        for k, v in grown.items():
            self.grown[k] = self.grown.get(k, 0) + v

    def remap(self, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        """Node ids changed: the folders (and the growth they follow) take their new ids."""
        if self.folders:
            moved = new_ids(np.fromiter(self.folders, dtype=np.int64, count=len(self.folders)))
            self.folders = dict.fromkeys(int(m) for m in moved.tolist() if m >= 0)
        if self.grown:
            keys = np.fromiter(self.grown, dtype=np.int64, count=len(self.grown))
            moved = new_ids(keys)
            self.grown = {
                int(m): d for m, d in zip(moved.tolist(), self.grown.values(), strict=True) if m >= 0
            }

    def forget_ids(self, roots: Sequence[int]) -> None:
        """The tree was replaced without a remap: what changed is somewhere below the roots."""
        had = bool(self.folders)
        self.folders = {}
        self.grown = None
        if had:
            self.add([int(r) for r in roots], 0)

    def then(self, newer: _TreeDue) -> _TreeDue:
        """These changes (not heard of in the end) followed by ``newer``, of the same epoch."""
        out = _TreeDue(epoch=newer.epoch, more=self.more + newer.more)
        out.add(list(self.folders), self.changes, self.grown)
        out.add(list(newer.folders), newer.changes, newer.grown)
        return out


def grown_by(store: NodeStore, changes: Mapping[int, int]) -> dict[int, int] | None:
    """What each folder's size on disk changed by in ``changes`` (node -> change of its own
    allocation): a changed file counts in its folder and every folder above, a changed folder in
    itself and above. None when there are too many changes to follow (or none were named)."""
    if not changes or len(changes) > GROWN_CHANGES:
        return None
    n = len(store)
    ids = np.fromiter(changes.keys(), dtype=np.int64, count=len(changes))
    deltas = np.fromiter(changes.values(), dtype=np.int64, count=len(changes))
    ok = (ids > 0) & (ids < n)
    ids, deltas = ids[ok], deltas[ok]
    c = store.cols
    parent = c["parent"]
    cur = np.where(c["kind"][ids] == Kind.DIR, ids, parent[ids].astype(np.int64))
    out: dict[int, int] = {}
    for _level in range(n):  # one step up per round; a parent is always closer to the top
        keep = (cur > 0) & (cur < n)
        cur, deltas = cur[keep], deltas[keep]
        if not len(cur):
            break
        folders, at = np.unique(cur, return_inverse=True)
        summed = np.zeros(len(folders), dtype=np.int64)
        np.add.at(summed, at, deltas)
        for f, d in zip(folders.tolist(), summed.tolist(), strict=True):
            out[f] = out.get(f, 0) + d
        if len(out) > GROWN_FOLDERS:
            return None
        nxt = parent[folders].astype(np.int64)
        cur, deltas = np.where(nxt == folders, 0, nxt), summed  # a root's parent is the top (0)
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

    def tree_changed(self, scan_id: str, changes: Mapping[int, int]) -> None:
        """The tree of ``scan_id`` changed (``views.flash``: node -> change of its allocation)."""
        host = self.host
        if host._loop is None or host.closed:
            return
        recs = self._listeners("tree_changed")
        handle = host.server.scans.scans.get(scan_id) if recs else None
        if handle is None:
            return
        store = handle.store
        folders = changed_folders(store, changes)
        grown = grown_by(store, changes)
        for rec in recs:
            key = (rec.id, scan_id)
            due = self._tree.get(key)
            if due is None:
                due = self._tree[key] = _TreeDue(epoch=int(store.epoch))
            elif due.epoch != store.epoch:  # an epoch without a remap: from the top
                due.forget_ids(store.roots())
                due.epoch = int(store.epoch)
            due.add(folders, max(1, len(changes)), grown)
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

    def _change(self, scan_id: str, due: _TreeDue) -> tuple[dict[str, Any], int] | None:
        """The hook's args for ``due`` as the tree is now, and their epoch (None: the scan went)."""
        handle = self.host.server.scans.scans.get(scan_id)
        if handle is None:
            return None
        store = handle.store
        if due.epoch != store.epoch:  # (remaps keep it in step; anything else: from the top)
            due.forget_ids(store.roots())
            due.epoch = int(store.epoch)
        n = len(store)
        folders = [f for f in due.folders if 0 < f < n]
        tot = store.cols["tot_alloc"]
        after = [int(tot[f]) for f in folders]
        grown = due.grown
        before: list[int | None] = [
            None if grown is None or f not in grown else a - grown[f]
            for f, a in zip(folders, after, strict=True)
        ]
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
        if due.epoch != store.epoch:
            due.forget_ids(store.roots())
            due.epoch = int(store.epoch)
        newer = self._tree.get(key)
        if newer is not None and newer.epoch != due.epoch:
            newer.forget_ids(store.roots())
            newer.epoch = due.epoch
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
                        _scan=scan_id,
                        _epoch=first[1],
                        _again=functools.partial(self._change, scan_id, due),
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
                self._start(self.host._hook(rec, "snapshots", change={"saved": saved, "removed": removed}))

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


__all__ = ["NOTICE_GAP_S", "SNAPSHOTS_EVENT_S", "TREE_EVENT_S", "TREE_FOLDERS", "Live", "changed_folders"]
