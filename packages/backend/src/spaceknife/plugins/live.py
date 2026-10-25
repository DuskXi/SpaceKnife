"""What enabled plugins hear of while they run, besides their own calls (docs/plugins.md, 「插件进程听到
的变化」):

- the ``tree_changed`` hook: the tree of an open scan changed (file changes applied by its monitor,
  「重新扫描」, a scan source listed again). Per plugin and scan the changes of about a second come
  together in one call, and a plugin whose hook still runs gets the next one when it is done (never
  a queue of them); at most :data:`TREE_FOLDERS` folders are listed, ``more`` counts the rest;
- the ``snapshots`` hook (``snapshots.read`` only): snapshots were saved or pruned;
- notices (``ctx.notify``, ``notify`` only): checked, bounded, at most one per plugin every
  :data:`NOTICE_GAP_S` seconds, sent to the windows as ``plugin.notice`` (a system notification
  while the window is minimized is the frontend's part).
"""

from __future__ import annotations

import asyncio
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
SNAPSHOTS_EVENT_S = 0.5  # the snapshots hook: saves and prunes of this long come together
SNAPSHOT_PATHS = 64  # paths one snapshots call lists (of saved, of removed)
NOTICE_GAP_S = 5.0  # a plugin's notices: at most one in this many seconds (the SDK holds to it too)
NOTICE_ARGS_MAX = 4096  # a notice's command args, as JSON


@dataclass(slots=True)
class _TreeDue:
    """The changes of one scan a plugin has not heard of yet."""

    epoch: int
    folders: dict[int, None] = field(default_factory=dict)  # an ordered set, at most TREE_FOLDERS
    more: int = 0
    changes: int = 0

    def add(self, folders: Sequence[int], changes: int) -> None:
        self.changes += changes
        for f in folders:
            if f in self.folders:
                continue
            if len(self.folders) >= TREE_FOLDERS:
                self.more += 1
            else:
                self.folders[f] = None


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
        self._snaps.clear()

    def forget(self, plugin: str) -> None:
        """The plugin was turned off: what it had not heard of yet goes."""
        for key in [k for k in self._tree if k[0] == plugin]:
            self._tree.pop(key, None)
        for key in [k for k in self._tree_tasks if k[0] == plugin]:
            self._tree_tasks.pop(key).cancel()
        self._snaps.pop(plugin, None)
        self._notice_at.pop(plugin, None)

    def forget_scan(self, scan_id: str) -> None:
        for key in [k for k in self._tree if k[1] == scan_id]:
            self._tree.pop(key, None)

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
        for rec in recs:
            key = (rec.id, scan_id)
            due = self._tree.get(key)
            if due is None or due.epoch != store.epoch:  # (an epoch without a remap: from the top)
                stale = due is not None and bool(due.folders)
                due = self._tree[key] = _TreeDue(epoch=int(store.epoch))
                if stale:
                    due.add([int(r) for r in store.roots()], 0)
            due.add(folders, max(1, len(changes)))
            task = self._tree_tasks.get(key)
            if task is None or task.done():
                self._tree_tasks[key] = self._start(self._deliver_tree(rec.id, scan_id))

    def tree_remapped(self, scan_id: str, new_ids: Callable[[np.ndarray], np.ndarray]) -> None:
        """Node ids of ``scan_id`` changed (a relist moved rows, a compaction): the folders a plugin
        has not heard of yet follow."""
        handle = self.host.server.scans.scans.get(scan_id)
        if handle is None:
            return
        epoch = int(handle.store.epoch)
        for key, due in self._tree.items():
            if key[1] != scan_id:
                continue
            if due.folders:
                moved = new_ids(np.fromiter(due.folders, dtype=np.int64, count=len(due.folders)))
                due.folders = dict.fromkeys(int(m) for m in moved.tolist() if m >= 0)
            due.epoch = epoch

    async def _deliver_tree(self, plugin: str, scan_id: str) -> None:
        key = (plugin, scan_id)
        try:
            while key in self._tree:
                await asyncio.sleep(TREE_EVENT_S)  # what changes meanwhile comes along
                due = self._tree.pop(key, None)
                rec = self.host.records.get(plugin)
                handle = self.host.server.scans.scans.get(scan_id)
                if due is None or rec is None or rec.state != "enabled" or handle is None:
                    return
                store = handle.store
                folders = list(due.folders) if due.epoch == store.epoch else [int(r) for r in store.roots()]
                change = {
                    "scan_id": scan_id,
                    "epoch": int(store.epoch),
                    "version": int(store.version),
                    "folders": folders,
                    "more": due.more,
                    "changes": due.changes,
                }
                await self.host._hook(rec, "tree_changed", _scan=scan_id, change=change)
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
