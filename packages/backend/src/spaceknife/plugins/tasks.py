"""Plugins' background tasks (``@plugin.background``): long work the host keeps track of.

A task is a call of kind ``task`` with no time limit of its own (unless declared), on the plugin's
long-call slots: it does not take one of the four call places, and a plain ``def`` runs on a thread
of its own in the plugin process. The host keeps a record per task — title, progress, how it ended —
that every window sees (``plugin.tasks``, the ``plugin.task`` event) and can cancel. A task declared
with ``restart`` is started again (same params) when the plugin's process crashed under it; one
declared ``start = "startup"`` starts whenever the plugin's process does.
"""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import functools
import itertools
import json
import time
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from loguru import logger

from spaceknife_protocol import plugins as pp
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_PLUGIN_TASK

from .. import i18n
from ..i18n import tr
from ..server.rpc import RpcError

if TYPE_CHECKING:
    from .host import PluginHost
    from .store import PluginRecord

MAX_RUNNING = 8  # tasks running per plugin
MAX_KEPT = 8  # ended tasks kept per plugin (the windows show how they ended)
MAX_PARAMS = 64 << 10  # bytes of JSON params
MAX_KEY = 120
MAX_ATTEMPTS = 4  # a task that keeps crashing its process stops being restarted (and the crash limit)
PUSH_EVERY_S = 0.25  # progress pushes per task at most this often

# the task a call runs for (set by the runner; _run_call reads it to map call ids to tasks)
CURRENT_TASK: contextvars.ContextVar[Task | None] = contextvars.ContextVar("plugin_task", default=None)


@dataclass(slots=True, eq=False)
class Task:
    id: str
    plugin: str
    name: str
    key: str
    params: dict[str, Any]
    scan_id: str | None = None  # the scan of the call that started it: the task's ctx.tree
    started: float = field(default_factory=time.time)
    state: pp.PluginTaskState = "running"
    done: float = 0.0
    total: float = 0.0
    text: str = ""
    texts: dict[str, str] | None = None  # the text in both languages, when the plugin gave a T(...)
    ended: float | None = None
    attempt: int = 0
    error: str = ""
    runner: asyncio.Task[None] | None = None
    call_id: int | None = None
    cancel_asked: bool = False
    stopping: bool = False  # the host stopped the plugin on purpose (disabled, restarted, reloaded)
    pushed: float = 0.0
    push_due: asyncio.TimerHandle | None = None

    @property
    def running(self) -> bool:
        return self.state in ("running", "restarting")


class Tasks:
    def __init__(self, host: PluginHost) -> None:
        self.host = host
        self._ids = itertools.count(1)
        self._tasks: dict[str, Task] = {}  # by id
        self._calls: dict[int, Task] = {}  # call id -> the task it runs

    # ------------------------------------------------------------- what is declared

    def _spec(self, rec: PluginRecord, name: str) -> dict[str, Any] | None:
        desc = rec.description
        if desc is None:
            return None
        for c in desc.contributions:
            if c.get("kind") == "background" and (c.get("spec") or {}).get("name") == name:
                return dict(c.get("spec") or {})
        return None

    def declared(self, rec: PluginRecord) -> list[dict[str, Any]]:
        desc = rec.description
        if desc is None:
            return []
        return [dict(c.get("spec") or {}) for c in desc.contributions if c.get("kind") == "background"]

    def wants_process(self, rec: PluginRecord) -> bool:
        """Whether the plugin declares a task to start with its process (so the process starts)."""
        return any(s.get("start") == "startup" for s in self.declared(rec))

    # ------------------------------------------------------------- starting and stopping

    def find(self, plugin: str, name: str, key: str) -> Task | None:
        return next(
            (
                t
                for t in self._tasks.values()
                if t.plugin == plugin and t.name == name and t.key == key and t.running
            ),
            None,
        )

    def start(
        self,
        rec: PluginRecord,
        name: str,
        params: Mapping[str, Any] | None = None,
        key: str = "",
        *,
        replace: bool = False,
        scan_id: str | None = None,
    ) -> Task:
        """Start ``name`` (one per name and key: a running one is returned as it is, or with
        ``replace`` cancelled and started again). ``scan_id``: the scan its ``ctx.tree`` reads (the
        one of the call that started it; none: the scan in front, as for any call)."""
        spec = self._spec(rec, name)
        if spec is None:
            raise RpcError(
                ErrorCode.METHOD_NOT_FOUND,
                f"{rec.id} declares no background task {name!r}",
                {"plugin": rec.id},
            )
        key = str(key)[:MAX_KEY]
        body = dict(params or {})
        if len(json.dumps(body, default=str)) > MAX_PARAMS:
            raise RpcError(ErrorCode.PLUGIN_LIMIT, f"task params are larger than {MAX_PARAMS} bytes")
        running = self.find(rec.id, name, key)
        if running is not None:
            if not replace:
                return running
            self.cancel(running)
        if sum(1 for t in self._tasks.values() if t.plugin == rec.id and t.running) >= MAX_RUNNING:
            raise RpcError(
                ErrorCode.PLUGIN_LIMIT,
                tr("插件 {id} 的后台任务太多", id=rec.id),
                {"plugin": rec.id, "running": MAX_RUNNING},
            )
        t = Task(
            id=f"task-{next(self._ids)}",
            plugin=rec.id,
            name=name,
            key=key,
            params=body,
            scan_id=str(scan_id)[:64] if scan_id else None,
        )
        self._tasks[t.id] = t
        t.runner = asyncio.ensure_future(self._run(rec, t, spec))
        t.runner.add_done_callback(functools.partial(self._ended, t, rec.id))
        self._trim(rec.id)
        return t

    def _ended(self, t: Task, plugin: str, _runner: object = None) -> None:
        """The runner is over; cancelled before it ever ran, its record ends here."""
        if t.running:
            t.state = "cancelled"
            t.ended = time.time()
            self.push(t, now=True)
            self._trim(plugin)

    def cancel(self, t: Task) -> Task:
        if t.running:
            t.cancel_asked = True
            if t.runner is not None and not t.runner.done():
                t.runner.cancel()
        return t

    def cancel_named(self, plugin: str, name: str, key: str | None) -> int:
        n = 0
        for t in list(self._tasks.values()):
            if t.plugin == plugin and t.name == name and (key is None or t.key == key) and t.running:
                self.cancel(t)
                n += 1
        return n

    def get(self, task_id: str) -> Task:
        t = self._tasks.get(task_id)
        if t is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, f"no task {task_id!r}")
        return t

    def process_started(self, rec: PluginRecord) -> None:
        """The plugin's process (re)started: its ``startup`` tasks start unless they run."""
        for spec in self.declared(rec):
            if spec.get("start") == "startup" and self.find(rec.id, str(spec["name"]), "") is None:
                try:
                    self.start(rec, str(spec["name"]))
                except RpcError as exc:
                    logger.bind(source=f"plugin:{rec.id}").warning(
                        "background task {} did not start: {}", spec.get("name"), exc.message
                    )

    def stopping(self, plugin: str) -> None:
        """The host stops the plugin on purpose: its tasks end as cancelled, none restarts."""
        for t in self._tasks.values():
            if t.plugin == plugin and t.running:
                t.stopping = True

    def forget(self, plugin: str) -> None:
        """The plugin is gone (disabled, uninstalled): its tasks are cancelled and forgotten."""
        for t in [t for t in self._tasks.values() if t.plugin == plugin]:
            t.stopping = True
            self.cancel(t)
            if not t.running:
                self._tasks.pop(t.id, None)

    def close(self) -> None:
        for t in self._tasks.values():
            t.stopping = True
            if t.runner is not None and not t.runner.done():
                t.runner.cancel()

    # ------------------------------------------------------------- running

    async def _run(self, rec: PluginRecord, t: Task, spec: dict[str, Any]) -> None:
        token = CURRENT_TASK.set(t)
        timeout = spec.get("timeout")
        try:
            while True:
                t.attempt += 1
                t.state = "running"
                t.error = ""
                self.push(t, now=True)
                crashes = rec.restarts
                try:
                    await self.host.call(
                        rec.id,
                        "task",
                        t.name,
                        {"params": t.params, "key": t.key},
                        where=self.host.places.task(t.scan_id),
                        timeout=float(timeout) if timeout else None,
                    )
                    t.state = "done"
                except RpcError as exc:
                    crashed = rec.restarts > crashes and not t.stopping and not t.cancel_asked
                    if (
                        crashed
                        and spec.get("restart")
                        and t.attempt < MAX_ATTEMPTS
                        and rec.state == "enabled"
                        and not self.host.closed
                    ):
                        t.state = "restarting"
                        t.error = exc.message
                        self.push(t, now=True)
                        logger.bind(source=f"plugin:{rec.id}").warning(
                            "background task {} is started again after a crash: {}", t.name, exc.message
                        )
                        continue
                    if t.cancel_asked or t.stopping or exc.code == ErrorCode.REQUEST_CANCELLED:
                        t.state = "cancelled"
                    else:
                        t.state = "failed"
                        t.error = (
                            tr("超过 {s} 秒的时限", s=f"{float(timeout):g}")
                            if exc.code == ErrorCode.PLUGIN_TIMEOUT and timeout
                            else exc.message
                        )[:512]
                break
        except asyncio.CancelledError:
            t.state = "cancelled"
        finally:
            CURRENT_TASK.reset(token)
            t.ended = time.time()
            t.call_id = None
            self.push(t, now=True)
            self._trim(rec.id)

    def call_started(self, call_id: int) -> None:
        """``_run_call`` of a task's call: progress messages of this call belong to the task."""
        t = CURRENT_TASK.get()
        if t is not None:
            t.call_id = call_id
            self._calls[call_id] = t

    def call_ended(self, call_id: int) -> None:
        self._calls.pop(call_id, None)

    def progress(
        self, call_id: int, done: float, total: float, text: str, texts: Mapping[str, Any] | None = None
    ) -> bool:
        """``ctx.progress`` of a task's call (True when it was one). ``texts``: the text in both
        languages (the plugin gave a ``T(...)``): shown in the language of the moment."""
        t = self._calls.get(call_id)
        if t is None:
            return False
        t.done, t.total, t.text = float(done), float(total), str(text)[:512]
        t.texts = (
            {str(k): str(v)[:512] for k, v in list(texts.items())[:2]} if isinstance(texts, Mapping) else None
        )
        self.push(t)
        return True

    def relocalize(self) -> None:
        """The interface language changed: every window gets the running tasks in the new one."""
        for t in list(self._tasks.values()):
            if t.running:
                self.push(t, now=True)

    # ------------------------------------------------------------- what the windows see

    def info(self, t: Task) -> pp.PluginTaskInfo:
        rec = self.host.records.get(t.plugin)
        spec = self._spec(rec, t.name) if rec is not None else None
        title = (spec or {}).get("title") or {}
        lang = i18n.language()
        zh = str(title.get("zh") or t.name)
        en = str(title.get("en") or "") or (
            rec.description.catalog.get(zh, "") if rec is not None and rec.description is not None else ""
        )
        shown = (en or zh) if lang == "en" else zh
        text = (t.texts or {}).get(lang) or t.text
        return pp.PluginTaskInfo(
            id=t.id,
            plugin=t.plugin,
            name=t.name,
            key=t.key,
            title=shown,
            state=t.state,
            done=t.done,
            total=t.total,
            text=text,
            started=t.started,
            ended=t.ended,
            attempt=max(1, t.attempt),
            error=t.error,
        )

    def list(self, plugin: str | None = None) -> list[pp.PluginTaskInfo]:
        return [self.info(t) for t in self._tasks.values() if plugin is None or t.plugin == plugin]

    def push(self, t: Task, *, now: bool = False) -> None:
        """Tell every window (progress: at most every PUSH_EVERY_S, the last one always)."""
        loop = self.host._loop
        wait = PUSH_EVERY_S - (time.monotonic() - t.pushed)
        if not now and wait > 0:
            if t.push_due is None and loop is not None:
                t.push_due = loop.call_later(wait, self._push_now, t)
            return
        self._push_now(t)

    def _push_now(self, t: Task) -> None:
        if t.push_due is not None:
            t.push_due.cancel()
            t.push_due = None
        t.pushed = time.monotonic()
        ev = self.info(t)
        for s in self.host._sessions():
            with contextlib.suppress(Exception):
                s.emit(EVENT_PLUGIN_TASK, ev)

    def _trim(self, plugin: str) -> None:
        ended = [t for t in self._tasks.values() if t.plugin == plugin and not t.running]
        for t in ended[: max(0, len(ended) - MAX_KEPT)]:
            self._tasks.pop(t.id, None)


__all__ = ["CURRENT_TASK", "MAX_RUNNING", "Task", "Tasks"]
