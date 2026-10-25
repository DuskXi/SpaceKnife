"""The plugin host: one per server. Finds plugins, enables them (the user's confirmation, hash
pinning), registers what they add, starts their code when it is first needed, routes
``plugin.<id>.<name>`` calls, forwards their events and shared states, and answers their requests
(hostapi.py). Every call is time-boxed, latest-wins where asked, and limited per plugin.
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
import os
import time
from collections import defaultdict, deque
from collections.abc import Awaitable, Callable, Mapping, Sequence
from pathlib import Path
from typing import TYPE_CHECKING, Any

import numpy as np
from loguru import logger
from pydantic import ConfigDict

from spaceknife_protocol import CAP_PLUGINS
from spaceknife_protocol import plugins as pp
from spaceknife_protocol.base import Params
from spaceknife_protocol.binary import BinaryFrameHeader, decode_frame, encode_frame
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_EXTENSIONS,
    EVENT_PLUGIN,
    EVENT_PLUGIN_CLIPBOARD,
    EVENT_PLUGIN_PROGRESS,
    EVENT_PLUGIN_TOAST,
    MethodSpec,
)
from spaceknife_sdk.errors import Issue
from spaceknife_sdk.manifest import HOOK_PERMISSIONS, PLUGIN_API, ExportFormatSpec, SettingSpec
from spaceknife_sdk.plugin import Description, describe_declarative

from .. import i18n
from ..extensions import BUILTIN, EXTENSIONS
from ..extensions import points as ext_points
from ..i18n import _, tr
from ..server.rpc import CURRENT_REQUEST, RawResult, Route, RpcError
from . import builtins
from . import install as inst
from . import paths as ppaths
from .adapters import AdapterError, build
from .hostapi import HostApi
from .image import Images
from .live import Live
from .runtime.base import Callbacks, PluginRuntime, RuntimeCallError, StartInfo
from .store import PluginRecord, TrustStore, discover, read_record
from .tasks import Tasks

if TYPE_CHECKING:
    from ..export import ExportFormat
    from ..server.app import Server
    from ..server.session import Session
    from ..server.topics import Subscription

MAX_CONCURRENT = 4  # calls in flight per plugin (watchers and streams do not count); more with threads
MAX_LONG = 64  # watchers and streams running per plugin (they wait for changes, not compute)
SPARE_THREADS = 8  # threads a plugin's cancelled plain-def calls finish on (their places go on)
STUCK_AFTER_S = 30.0  # every call thread held by cancelled calls this long: the process restarts
MAX_QUEUED = 32  # calls waiting per plugin
MAX_PROCESSES = 8  # plugin processes at once (the idlest goes first)
IDLE_STOP_S = 600.0
CRASH_WINDOW_S = 300.0
CRASH_LIMIT = 3
BACKOFF_S = (1.0, 5.0, 30.0)
EVENTS_PER_S = 50
STATES_PER_S = 20
MAX_RESULT_JSON = 8 << 20
EXTENSIONS_DEBOUNCE_S = 0.05
FIELD_TIMEOUT = {"inspector": 1.0, "hover": 0.03}
SET_TIMEOUT = 5.0
SET_RUNNING_EVERY_S = 5.0
COLOR_TIMEOUT = 0.02
# A code colour mode that missed its 20 ms (the first frame of a tree maps the tree's columns into the
# plugin's process, which takes longer) is asked again a moment later, a few times per tree version:
# the frame that showed neutral colours is drawn again once the columns are there.
COLOR_RETRY_S = 0.25
COLOR_RETRIES = 3
FIELD_LOG_S = 30.0  # a field that keeps failing is logged at most this often
VISUAL_TIMEOUT = 0.12
LAYOUT_TIMEOUT = 5.0  # s: a layout visualization places one picture (the first call also maps the tree)
ANNOTATE_TIMEOUT = 0.2
ANNOTATE_BATCH = 500
MAX_REASONS = 20_000  # remembered change reasons (all comparisons); forgotten all at once beyond
HOOK_TIMEOUT = 30.0
CHECK_TIMEOUT = 0.05
ROUTED_KINDS = ("method", "report", "cleanup")
CACHED_KINDS = frozenset({"node_field", "node_set", "visualization", "color_mode", "change_annotator"})
LONG_KINDS = ("stream", "watch", "task")  # no time limit unless the caller gives one
LONG_CALL_S = 10.0 * 365 * 86400  # what the plugin is told for those (its ctx.check() never trips)


class RawParams(Params):
    """A plugin method's params: checked by the plugin's own model (INVALID_PARAMS with details)."""

    model_config = ConfigDict(extra="allow", frozen=True)


class _Rate:
    """At most ``per_s`` a second (a sliding second)."""

    def __init__(self, per_s: int) -> None:
        self.per_s = per_s
        self.times: deque[float] = deque()

    def ok(self) -> bool:
        now = time.monotonic()
        while self.times and now - self.times[0] > 1.0:
            self.times.popleft()
        if len(self.times) >= self.per_s:
            return False
        self.times.append(now)
        return True


class PluginHost:
    def __init__(self, server: Server) -> None:
        self.server = server
        self.trust = TrustStore()
        self.records: dict[str, PluginRecord] = {}
        self.images = Images()
        self.api = HostApi(self)
        self.tasks = Tasks(self)
        self.live = Live(self)  # tree changes, snapshots and notices: what plugins hear of (live.py)
        self._ids = itertools.count(1)
        self._slots: dict[str, asyncio.Semaphore] = {}
        self._long_slots: dict[str, asyncio.Semaphore] = {}
        self.epochs: dict[str, int] = defaultdict(int)  # per plugin: cached answers older than this go
        self._seen_settings: dict[str, dict[str, Any]] = {}
        self._queued: dict[str, int] = defaultdict(int)
        self._latest: dict[tuple[str, str, str], asyncio.Task[Any]] = {}
        self._sites: dict[
            int, tuple[str, Session | None, Any]
        ] = {}  # call id -> (plugin, session, request id)
        # call id -> who hears its ctx.progress instead of the frontends (a scan source: its scan)
        self._progress_to: dict[int, Callable[[float, float, str], None]] = {}
        self._start_locks: dict[str, asyncio.Lock] = {}
        self._starting: dict[str, asyncio.Task[PluginRuntime]] = {}
        self._event_rates: dict[str, _Rate] = {}
        self._state_rates: dict[str, _Rate] = {}
        self._state_subs: dict[tuple[str, str], dict[str, tuple[Session, Subscription]]] = defaultdict(dict)
        self._stream_tasks: dict[str, asyncio.Task[None]] = {}
        self._set_cache: dict[tuple[str, str], tuple[tuple[int, int], float, np.ndarray]] = {}
        self._field_cache: dict[tuple[str, str, int, int, int, str], pp.PluginField | None] = {}
        self._field_inflight: set[tuple[str, str, int, int, int, str]] = set()
        self._basket_hook_due = False
        self._reasons: dict[tuple[str, str, str, str, int], str] = {}  # change annotators' answers
        self._legends: dict[tuple[str, str, str], tuple[int, list[tuple[str, Any]]]] = {}
        self._legend_asked: set[tuple[str, str, str]] = set()
        self._weights: dict[tuple[str, str, int, int, int, str], np.ndarray] = {}
        self._color_retries: dict[tuple[str, str, int, int], int] = {}  # COLOR_RETRIES per tree version
        self.field_errors: dict[tuple[str, int], str] = {}  # (field id, node) -> why it failed, the latest 64
        self._field_logged: dict[str, float] = {}  # field id -> when its last failure was logged
        self._visual_failures: dict[tuple[str, str], int] = defaultdict(int)
        self._changed: set[str] = set()
        self._flush: asyncio.TimerHandle | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._background: set[asyncio.Task[Any]] = set()
        self._closed = False
        self.dev_folders: list[Path] = []  # tests and ``plugin dev``: linked without the trust file
        self.force_runtime: pp.PluginRuntimeKind | None = None  # MockHost: where linked plugins run
        ext_points.install()

    # ------------------------------------------------------------- lifecycle

    @property
    def dev_mode(self) -> bool:
        if os.environ.get("SPACEKNIFE_PLUGIN_DEV") == "1":
            return True
        st = self.server.settings
        try:
            return bool(st.get("advanced.plugin_dev_mode"))
        except KeyError:
            return False

    async def start(self) -> None:
        """Find the plugins and register what the enabled ones add (code starts when needed)."""
        self._loop = asyncio.get_running_loop()
        EXTENSIONS.listeners[f"host-{id(self)}"] = self._on_extensions
        with contextlib.suppress(OSError):
            await asyncio.to_thread(inst.cleanup_old)
        records = await asyncio.to_thread(discover, self.trust, dev_mode=self.dev_mode)
        for folder in self.dev_folders:
            rec = await asyncio.to_thread(read_record, folder, dev=True)
            records = [r for r in records if r.id != rec.id] + [rec]
        for rec in records:
            self.records[rec.id] = rec
        for rec in records:
            await self._settle(rec)
        for rec in records:
            if rec.state == "enabled" and rec.manifest is not None and rec.manifest.activation == "startup":
                self._spawn(self._ensure_started(rec))

    @property
    def closed(self) -> bool:
        return self._closed

    async def close(self) -> None:
        self._closed = True
        EXTENSIONS.listeners.pop(f"host-{id(self)}", None)
        self.tasks.close()
        self.live.close()
        for task in list(self._stream_tasks.values()) + list(self._background):
            task.cancel()
        for rec in list(self.records.values()):
            await self._stop_runtime(rec)
            if not rec.builtin:  # the shipped plugins stay: they belong to the process (builtins.py)
                EXTENSIONS.unregister_owner(rec.id)
        self.images.close()
        if self._flush is not None:
            self._flush.cancel()

    def _spawn(self, coro: Awaitable[Any]) -> None:
        task = asyncio.ensure_future(coro)
        self._background.add(task)
        task.add_done_callback(self._background.discard)

    # ------------------------------------------------------------- state of a plugin

    def _state_of(self, rec: PluginRecord) -> pp.PluginState:
        if rec.state in ("error", "incompatible") and rec.issues:
            return rec.state
        if rec.manifest is None:
            return "error"
        if rec.builtin:
            return "enabled"
        entry = self.trust.get(rec.id)
        if entry is None:
            return "needs_confirm"
        if not entry.enabled:
            return "disabled"
        if not rec.dev and entry.hash != rec.hash:
            return "changed"
        if not set(rec.requested) <= set(entry.granted):
            return "changed"
        if (rec.manifest.net_hosts and sorted(rec.manifest.net_hosts) != sorted(entry.net_hosts)) or (
            rec.manifest.exec and sorted(rec.manifest.exec) != sorted(entry.exec)
        ):
            return "changed"
        return "enabled"

    async def _settle(self, rec: PluginRecord) -> None:
        """Bring the registry in line with the record's state (register an enabled plugin)."""
        state = self._state_of(rec)
        rec.state = state
        if rec.builtin:
            rec.granted = frozenset(rec.requested)
        else:
            entry = self.trust.get(rec.id)
            rec.granted = (
                frozenset(entry.granted) if entry is not None and state == "enabled" else frozenset()
            )
        if state != "enabled":
            return
        try:
            await self._activate(rec)
        except AdapterError as exc:
            rec.issues = list(exc.issues)
            rec.state = "error"
            logger.warning("plugin {} cannot be enabled: {}", rec.id, exc)
            return
        except RpcError as exc:
            rec.issues = [Issue("start", exc.message)]
            rec.state = "error"
            logger.warning("plugin {} cannot be enabled: {}", rec.id, exc.message)
            return
        if not rec.builtin and self.tasks.wants_process(rec) and not self._closed:
            # a task that runs whenever the plugin does: its process starts now (not when first used)
            rt = rec.runtime
            if rt is None or rt.state != "running":
                self._spawn(self._warm(rec))

    def _runtime_kind(self, rec: PluginRecord) -> pp.PluginRuntimeKind:
        if not rec.has_code:
            return "declarative"
        assert rec.manifest is not None
        if rec.builtin:
            return "inprocess"
        if self.force_runtime is not None and rec.dev:
            return self.force_runtime
        if rec.manifest.runtime == "inprocess" and self.dev_mode:
            return "inprocess"
        return "process"

    async def _describe(self, rec: PluginRecord) -> Description:
        """The plugin's description: declarative ones without code; code plugins from the cache
        (keyed by the hash) or by starting them."""
        assert rec.manifest is not None
        if not rec.has_code:
            return await asyncio.to_thread(describe_declarative, rec.folder, rec.manifest)
        cache = ppaths.cache_dir(rec.id) / f"describe-{rec.hash.removeprefix('sha256:')[:32]}.json"
        if rec.hash and not rec.dev and cache.is_file():
            try:
                return Description.model_validate_json(cache.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                pass
        rt = await self._ensure_started(rec)
        desc = rec.description
        assert desc is not None
        if rec.hash and not rec.dev:
            with contextlib.suppress(OSError):
                cache.parent.mkdir(parents=True, exist_ok=True)
                cache.write_text(desc.model_dump_json(), encoding="utf-8")
        _ = rt
        return desc

    async def _activate(self, rec: PluginRecord) -> None:
        if rec.builtin:  # registered once per process, with or without a host (builtins.py)
            loaded = await asyncio.to_thread(builtins.get, rec.id)
            if loaded is None:
                issues = builtins.failures().get(rec.folder.name) or [Issue("load", "not loaded")]
                raise AdapterError(issues)
            rec.description = loaded.description
            return
        desc = rec.description or await self._describe(rec)
        if desc.id != rec.id or desc.api != PLUGIN_API:
            raise AdapterError([Issue("describe", f"the code describes {desc.id!r} (API {desc.api})")])
        rec.description = desc
        contributions = build(self, rec, desc)
        EXTENSIONS.unregister_owner(rec.id)
        try:
            EXTENSIONS.register_many(contributions)
        except Exception as exc:
            raise AdapterError([Issue("register", str(exc))]) from exc
        for name, d in ((s.name, s.default) for s in desc.states):
            rec.states.setdefault(name, dict(d))
        rec.keys = [
            k for c in EXTENSIONS.owned(rec.id) if c.kind == "command" for k in getattr(c.impl, "keys", ())
        ]
        self.server.settings.plugins_changed()
        self._seen_settings[rec.id] = dict(self.server.settings.plugin_settings(rec.id))
        if rec.runtime is not None:  # started to describe itself: its settings exist only now
            await rec.runtime.update({"settings": self.server.settings.plugin_settings(rec.id)})
        if rec.start_due:  # its process started before this (first enable, a dev reload): now its
            rec.start_due = False  # hooks are registered and it has its settings
            rt = rec.runtime
            if rt is not None and rt.state == "running":
                self._spawn(self._hook(rec, "start"))

    async def _deactivate(self, rec: PluginRecord) -> None:
        if rec.builtin:
            return
        rec.start_due = False
        self.tasks.forget(rec.id)
        self.live.forget(rec.id)
        await self._stop_runtime(rec)
        EXTENSIONS.unregister_owner(rec.id)
        self.images.release_user(rec.id)
        self.api.release_plugin(rec.id)
        for key in [k for k in self._state_subs if k[0] == rec.id]:
            for session, sub in list(self._state_subs.pop(key).values()):
                session.remove_subscription(sub.id)
        self._forget(rec.id)
        self.server.settings.plugins_changed()

    def enabled_ids(self) -> frozenset[str]:
        return frozenset(r.id for r in self.records.values() if r.state == "enabled")

    def enabled(self, plugin: str) -> PluginRecord:
        rec = self.records.get(plugin)
        if rec is None or rec.state != "enabled":
            raise RpcError(
                ErrorCode.PLUGIN_DISABLED,
                tr("插件 {id} 没有启用", id=plugin),
                {"plugin": plugin, "state": rec.state if rec else None},
            )
        return rec

    # ------------------------------------------------------------- the code

    def _start_info(self, rec: PluginRecord) -> StartInfo:
        assert rec.manifest is not None
        m = rec.manifest
        return StartInfo(
            plugin=rec.id,
            version=m.version,
            folder=str(rec.folder),
            storage=str(ppaths.data_dir(rec.id)),
            lang=i18n.language(),
            settings=self.server.settings.plugin_settings(rec.id),
            granted=sorted(rec.granted),
            roots=self.api.scan_roots(),
            net_hosts=list(m.net_hosts),
            exec=list(m.exec),
            builtin=rec.builtin,
            dev=rec.dev,
            memory_mb=m.limits.memory_mb,
            states=dict(rec.states),
            threads=m.limits.threads,
            spare_threads=SPARE_THREADS,
            stuck_after=STUCK_AFTER_S,
        )

    def _callbacks(self, rec: PluginRecord) -> Callbacks:
        async def request(
            method: str, params: dict[str, Any], frames: list[bytes]
        ) -> tuple[Any, list[bytes]]:
            return await self.api.handle(rec, method, params, frames)

        def notify(type: str, payload: dict[str, Any]) -> None:
            self._on_notify(rec, type, payload)

        def exited(code: int | None, why: str) -> None:
            self._on_exit(rec, code, why)

        return Callbacks(request, notify, exited)

    async def _ensure_started(self, rec: PluginRecord) -> PluginRuntime:
        rt: PluginRuntime | None = rec.runtime
        if rt is not None and rt.state == "running":
            rec.last_used = time.monotonic()
            return rt
        # One start per plugin, shielded: a caller that gives up (a frame's colours wait 20 ms)
        # does not cancel the start the next caller will use.
        task = self._starting.get(rec.id)
        if task is None or task.done():
            task = asyncio.ensure_future(self._start(rec))
            self._starting[rec.id] = task
            self._background.add(task)
            task.add_done_callback(self._started)
        return await asyncio.shield(task)

    def _started(self, task: asyncio.Task[PluginRuntime]) -> None:
        self._background.discard(task)
        if not task.cancelled():
            task.exception()  # retrieved here too: every caller may have stopped waiting

    async def _start(self, rec: PluginRecord) -> PluginRuntime:
        lock = self._start_locks.setdefault(rec.id, asyncio.Lock())
        async with lock:
            rt: PluginRuntime | None = rec.runtime
            if rt is not None and rt.state == "running":
                return rt
            if rec.state == "crashed":
                raise RpcError(
                    ErrorCode.PLUGIN_UNAVAILABLE,
                    tr("插件 {id} 多次崩溃，已停用，需要重启", id=rec.id),
                    {"plugin": rec.id},
                )
            recent = [t for t in rec.crashes if time.monotonic() - t < CRASH_WINDOW_S]
            if recent:
                wait = BACKOFF_S[min(len(recent), len(BACKOFF_S)) - 1] - (time.monotonic() - recent[-1])
                if wait > 0:
                    await asyncio.sleep(wait)
            await self._make_room()
            kind = self._runtime_kind(rec)
            info = self._start_info(rec)
            cbs = self._callbacks(rec)
            if kind == "inprocess":
                from .runtime.inprocess import InProcessRuntime

                assert rec.manifest is not None
                rt = InProcessRuntime(
                    rec.folder,
                    rec.manifest,
                    info,
                    cbs,
                    store_lookup=self.api.store_of,
                    cls_lookup=self.api.cls_column,
                )
            else:
                from .runtime.process import ProcessRuntime

                processes = (
                    9 if "exec" in rec.granted else (1 if getattr(__import__("sys"), "frozen", False) else 2)
                )
                rt = ProcessRuntime(info, cbs, processes=processes)
            rec.runtime = rt
            try:
                desc = await rt.start()
            except RuntimeCallError as exc:
                rec.crashes.append(time.monotonic())
                self._note_crash(rec)
                raise RpcError(
                    ErrorCode.PLUGIN_UNAVAILABLE,
                    exc.message,
                    {"plugin": rec.id, **(exc.data if isinstance(exc.data, dict) else {})},
                ) from None
            if rec.description is None:
                rec.description = desc
            rec.last_used = time.monotonic()
            self._broadcast_plugin(rec)
            if rec.builtin or EXTENSIONS.owned(rec.id):
                self._spawn(self._hook(rec, "start"))
            else:  # started to describe itself (first enable, a dev reload): its hooks are not
                rec.start_due = True  # registered yet; _activate runs start once they are
            self.tasks.process_started(rec)
            return rt

    async def _make_room(self) -> None:
        running = [
            r
            for r in self.records.values()
            if r.runtime is not None and r.runtime.state == "running" and r.runtime.kind == "process"
        ]
        if len(running) < MAX_PROCESSES:
            return
        idle = sorted((r for r in running if not r.runtime.busy()), key=lambda r: r.last_used)
        if idle:
            await self._stop_runtime(idle[0])

    async def _stop_runtime(self, rec: PluginRecord) -> None:
        rt: PluginRuntime | None = rec.runtime
        if rt is None:
            return
        rec.runtime = None
        self.tasks.stopping(rec.id)  # on purpose: its background tasks end as cancelled, none restarts
        try:
            await rt.stop()
        except Exception:
            logger.exception("stopping plugin {} failed", rec.id)
            rt.kill()
        self.images.release_user(rec.id)
        self.api.release_plugin(rec.id)

    def _note_crash(self, rec: PluginRecord) -> None:
        now = time.monotonic()
        rec.crashes = [t for t in rec.crashes if now - t < CRASH_WINDOW_S]
        if len(rec.crashes) >= CRASH_LIMIT and rec.state == "enabled":
            rec.state = "crashed"
            logger.warning(
                "plugin {} crashed {} times in {} s; it stays off until restarted",
                rec.id,
                len(rec.crashes),
                CRASH_WINDOW_S,
            )
        self._broadcast_plugin(rec)

    def _on_exit(self, rec: PluginRecord, code: int | None, why: str) -> None:
        logger.bind(source=f"plugin:{rec.id}").warning("plugin {} ended: {}", rec.id, why)
        rec.crashes.append(time.monotonic())
        rec.restarts += 1
        # shared states stay subscribed with their last values; streams end with the process
        for sid, task in list(self._stream_tasks.items()):
            if sid.startswith(rec.id + "|"):
                task.cancel()
        self._note_crash(rec)

    async def idle_sweep(self) -> None:
        """Stop processes that were not used for a while (no subscriptions, not startup ones).

        Only running ones: a process still starting is not idle (its record may never have been
        used: a slow machine's start would be killed by the sweep that happens to run then)."""
        now = time.monotonic()
        for rec in list(self.records.values()):
            rt = rec.runtime
            if rt is None or rt.kind != "process" or rt.state != "running" or rt.busy():
                continue
            if rec.manifest is not None and rec.manifest.activation == "startup":
                continue
            if any(k[0] == rec.id for k in self._state_subs) or any(
                s.startswith(rec.id + "|") for s in self._stream_tasks
            ):
                continue
            if now - rec.last_used > IDLE_STOP_S:
                await self._stop_runtime(rec)
                self._broadcast_plugin(rec)

    # ------------------------------------------------------------- calls

    async def call(
        self,
        plugin: str,
        kind: str,
        name: str,
        params: Any = None,
        *,
        session: Session | None = None,
        ctx: Mapping[str, Any] | None = None,
        frames: Sequence[bytes] = (),
        on_chunk: Callable[[Any, list[bytes]], None] | None = None,
        timeout: float | None = None,  # noqa: ASYNC109 — the plugin's limit, enforced inside
        latest: bool = False,
        on_wait: Callable[[], None] | None = None,
        on_progress: Callable[[float, float, str], None] | None = None,
    ) -> tuple[Any, list[bytes]]:
        """Call a plugin's handler; RpcError with a plugin error code when it fails.

        The time limit counts from here: waiting for one of the plugin's ``MAX_CONCURRENT`` call
        slots is part of it (a field with a 1 s budget never waits longer than that), starting the
        plugin's process is not. Watchers and streams run beside the calls, on slots of their own.
        ``on_wait`` is called once when the call has to wait for a slot (a scan says so);
        ``on_progress(done, total, text)`` hears the call's ``ctx.progress`` (else the frontend of
        the request that made the call does)."""
        if self._closed:  # shutting down: nothing starts a plugin process again
            raise RpcError(ErrorCode.PLUGIN_UNAVAILABLE, "plugins are not running", {"plugin": plugin})
        rec = self.enabled(plugin)
        if not rec.has_code:
            raise RpcError(ErrorCode.METHOD_NOT_FOUND, f"{plugin} has no code")
        assert rec.manifest is not None
        limit: float | None = timeout
        long = kind in LONG_KINDS
        if long and timeout is None:
            limit = None  # streams, watchers and background tasks run until they are cancelled
        elif kind == "task":
            limit = float(timeout or 0) or None  # its declared limit (up to a day)
        else:
            if limit is None:
                decl = next(
                    (m for m in (rec.description.methods if rec.description else []) if m.name == name), None
                )
                limit = decl.timeout if decl is not None else 30.0
            limit = min(
                float(limit),
                float(rec.manifest.limits.timeout_s) if kind in ("method", "report") else 600.0,
                600.0,
            )
        if not long and self._queued[plugin] >= MAX_QUEUED:
            raise RpcError(ErrorCode.PLUGIN_LIMIT, tr("插件 {id} 的请求太多", id=plugin), {"plugin": plugin})
        key = (plugin, name, session.id if session is not None else "")
        if latest:
            prev = self._latest.get(key)
            if prev is not None and not prev.done():
                prev.cancel()
            task = asyncio.current_task()
            if task is not None:
                self._latest[key] = task
        loop = asyncio.get_running_loop()
        deadline = None if limit is None else loop.time() + limit
        if long:
            sem = self._long_slots.setdefault(plugin, asyncio.Semaphore(MAX_LONG))
        else:
            slots = max(MAX_CONCURRENT, rec.manifest.limits.threads)
            sem = self._slots.setdefault(plugin, asyncio.Semaphore(slots))
            self._queued[plugin] += 1
        queued = not long
        try:
            if sem.locked() and on_wait is not None:
                on_wait()
            try:
                async with asyncio.timeout_at(deadline):
                    await sem.acquire()
            except TimeoutError:
                raise RpcError(
                    ErrorCode.PLUGIN_TIMEOUT,
                    tr("插件 {id} 正忙，没有及时回答", id=plugin),
                    {"plugin": plugin, "queued": True},
                ) from None
            if queued:
                self._queued[plugin] -= 1
                queued = False
            try:
                return await self._run_call(
                    rec, kind, name, params, session, ctx, frames, on_chunk, deadline, on_progress
                )
            finally:
                sem.release()
        finally:
            if queued:
                self._queued[plugin] -= 1
            if latest and self._latest.get(key) is asyncio.current_task():
                self._latest.pop(key, None)

    async def _run_call(
        self,
        rec: PluginRecord,
        kind: str,
        name: str,
        params: Any,
        session: Session | None,
        ctx: Mapping[str, Any] | None,
        frames: Sequence[bytes],
        on_chunk: Callable[[Any, list[bytes]], None] | None,
        deadline: float | None,
        on_progress: Callable[[float, float, str], None] | None = None,
    ) -> tuple[Any, list[bytes]]:
        plugin = rec.id
        loop = asyncio.get_running_loop()
        t0 = loop.time()
        cold = rec.runtime is None or rec.runtime.state != "running"
        rt = await self._ensure_started(rec)
        if deadline is not None and cold:
            deadline += loop.time() - t0  # starting the process is not the call's time
        call_id = next(self._ids)
        info = {
            "lang": i18n.language(),
            "units": self.server.settings.unit_base(),  # ctx.fmt.size writes sizes as the app does
            "session": session.id if session is not None else None,
            "timeout": max(0.0, deadline - loop.time()) if deadline is not None else LONG_CALL_S,
        }
        info.update(ctx or {})
        self._sites[call_id] = (plugin, session, CURRENT_REQUEST.get())
        if on_progress is not None:
            self._progress_to[call_id] = on_progress
        if kind == "task":
            self.tasks.call_started(call_id)
        try:
            async with asyncio.timeout_at(deadline):
                value, out = await rt.call(call_id, kind, name, params, info, frames, on_chunk)
        except TimeoutError:
            raise RpcError(
                ErrorCode.PLUGIN_TIMEOUT, tr("插件 {id} 没有及时回答", id=plugin), {"plugin": plugin}
            ) from None
        except RuntimeCallError as exc:
            data: dict[str, Any] = {"plugin": plugin}
            if exc.data is not None:
                data["detail"] = exc.data
            raise RpcError(exc.code, exc.message, data) from None
        finally:
            self._sites.pop(call_id, None)
            self._progress_to.pop(call_id, None)
            self.tasks.call_ended(call_id)
            self.api.call_ended(call_id)
        rec.last_used = time.monotonic()
        return value, out

    def call_sync(
        self, plugin: str, kind: str, name: str, params: Any = None, *, timeout: float, **kw: Any
    ) -> tuple[Any, list[bytes]]:
        """:meth:`call` from a compute thread (frames, colours, sets): waits at most ``timeout``."""
        loop = self._loop
        if loop is None or self._closed:
            raise RpcError(ErrorCode.PLUGIN_UNAVAILABLE, "plugins are not running")
        try:
            here: asyncio.AbstractEventLoop | None = asyncio.get_running_loop()
        except RuntimeError:
            here = None
        if here is loop:  # waiting here would hold up the very loop that runs the call
            raise RpcError(ErrorCode.PLUGIN_UNAVAILABLE, "call_sync on the event loop", {"plugin": plugin})
        fut = asyncio.run_coroutine_threadsafe(
            self.call(plugin, kind, name, params, timeout=timeout, **kw), loop
        )
        try:
            return fut.result(timeout + 0.5)
        except TimeoutError:
            fut.cancel()
            raise RpcError(ErrorCode.PLUGIN_TIMEOUT, "timeout", {"plugin": plugin}) from None

    # ------------------------------------------------------------- what plugins send

    def _sessions(self) -> list[Session]:
        return [s for s in self.server.sessions if CAP_PLUGINS in s.capabilities]

    def _session(self, sid: str | None) -> Session | None:
        if sid is None:
            return None
        return next((s for s in self.server.sessions if s.id == sid), None)

    def _on_notify(self, rec: PluginRecord, type: str, payload: dict[str, Any]) -> None:
        if type == "log":
            level = str(payload.get("level", "INFO"))
            logger.bind(source=f"plugin:{rec.id}").log(
                level if level in ("DEBUG", "INFO", "WARNING", "ERROR") else "INFO",
                "{}",
                str(payload.get("message", ""))[:8000],
            )
        elif type == "event":
            name = str(payload.get("name", ""))
            if rec.description is None or name not in {e.name for e in rec.description.events}:
                return
            if not self._event_rates.setdefault(rec.id, _Rate(EVENTS_PER_S)).ok():
                return
            wire_name = pp.wire_name(rec.id, name)
            body = payload.get("payload") or {}
            target = self._session(payload.get("session"))
            for s in [target] if target is not None else self._sessions():
                s.emit_raw(wire_name, body)
        elif type == "state":
            name = str(payload.get("name", ""))
            if rec.description is None or name not in {s.name for s in rec.description.states}:
                return
            rec.states[name] = dict(payload.get("value") or {})
            if self._state_rates.setdefault(f"{rec.id}.{name}", _Rate(STATES_PER_S)).ok():
                self._push_state(rec, name)
            else:
                assert self._loop is not None
                self._loop.call_later(1.0 / STATES_PER_S, self._push_state, rec, name)
        elif type == "toast":
            if isinstance(payload.get("call"), int):
                self.api.toasted(int(payload["call"]))
            text = str(payload.get("text", ""))[:512]
            tone = payload.get("tone", "info")
            ev = pp.PluginToastEvent(
                plugin=rec.id, text=text, tone=tone if tone in ("info", "ok", "warn", "danger") else "info"
            )
            target = self._session(payload.get("session"))
            for s in [target] if target is not None else self._sessions():
                s.emit(EVENT_PLUGIN_TOAST, ev)
        elif type == "notice":
            self.live.notice(rec, payload, self._session(payload.get("session")))
        elif type == "clipboard":
            # ctx.clipboard.write_text: only with the permission, only for the session that asked
            # (a background call with no session writes nobody's clipboard)
            target = self._session(payload.get("session"))
            if "clipboard" not in rec.granted or target is None:
                logger.bind(source=f"plugin:{rec.id}").warning(
                    "clipboard text dropped: {}",
                    "no clipboard permission" if "clipboard" not in rec.granted else "no session asked",
                )
                return
            text = str(payload.get("text", ""))[: 1 << 20]
            target.emit(EVENT_PLUGIN_CLIPBOARD, pp.PluginClipboardEvent(plugin=rec.id, text=text))
        elif type == "progress":
            if self.tasks.progress(
                int(payload.get("id", 0)),
                float(payload.get("done", 0)),
                float(payload.get("total", 0)),
                str(payload.get("text", "")),
                payload.get("texts"),
            ):
                return  # a background task's: its record moves on (every window sees it)
            hear = self._progress_to.get(int(payload.get("id", 0)))
            if hear is not None:  # a scan source's: its scan's progress
                with contextlib.suppress(Exception):
                    text = str(payload.get("text", ""))[:512]
                    hear(float(payload.get("done", 0)), float(payload.get("total", 0)), text)
                return
            site = self._sites.get(int(payload.get("id", 0)))
            if site is None or site[1] is None:
                return
            site[1].emit(
                EVENT_PLUGIN_PROGRESS,
                pp.PluginProgressEvent(
                    plugin=rec.id,
                    request=site[2],
                    done=float(payload.get("done", 0)),
                    total=float(payload.get("total", 0)),
                    text=str(payload.get("text", ""))[:512],
                ),
            )
        elif type == "image_drop":
            self.images.drop(str(payload.get("key", "")), rec.id)
        elif type == "io_release":  # ctx.files is done with these disk-read permits
            self.api.io_release(rec, payload.get("permits"))
        elif type == "stuck":  # every call thread held by cancelled calls that do not return, for long
            rt = rec.runtime
            if rt is not None and rt.kind == "process":
                logger.bind(source=f"plugin:{rec.id}").warning(
                    "plugin {}: every worker thread has been busy with a cancelled call for {:.0f} s; "
                    "restarting it",
                    rec.id,
                    STUCK_AFTER_S,
                )
                rt.give_up("every worker thread is busy with a cancelled call")

    def _push_state(self, rec: PluginRecord, name: str) -> None:
        value = rec.states.get(name, {})
        for session, sub in list(self._state_subs.get((rec.id, name), {}).values()):
            if not sub.closed:
                session.emit_raw(pp.wire_name(rec.id, name), {"sub": sub.id, "value": value})

    # ------------------------------------------------------------- routes and topics

    def route(self, name: str) -> Route | None:
        """``plugin.<id>.<name>``: a public method, report or cleanup of an enabled plugin."""
        parts = pp.split_wire_name(name)
        if parts is None:
            return None
        plugin, local = parts
        rec = self.records.get(plugin)
        if rec is None or rec.description is None:
            return None
        decl = next((m for m in rec.description.methods if m.name == local), None)
        static = next(
            (
                c
                for c in EXTENSIONS.of("report")
                if c.owner == plugin and c.local == local and c.spec is not None and c.spec.blocks is not None
            ),
            None,
        )
        if decl is None and static is None:
            return None
        spec: MethodSpec[Any, Any] = MethodSpec(name, RawParams, RawParams, f"{plugin}: {local}")

        async def handler(session: Session, params: RawParams) -> Any:
            if CAP_PLUGINS not in session.capabilities:
                raise RpcError(ErrorCode.INVALID_REQUEST, "plugins.v1 capability was not negotiated")
            body = params.model_dump()
            if static is not None:
                return RawResult(self._static_report(static))
            assert decl is not None
            if not decl.public:
                raise RpcError(ErrorCode.PERMISSION_DENIED, f"{name} is not public", {"plugin": plugin})
            ctx = {k: body.get(k) for k in ("scan_id", "view_id", "node") if body.get(k) is not None}
            value, frames = await self.call(
                plugin, decl.kind, local, body, session=session, ctx=ctx, latest=decl.latest
            )
            if decl.kind == "cleanup":
                value = await self.api.cleanup_result(self.enabled(plugin), local, body, value)
            elif decl.kind == "report":
                value = self.api.checked_report(value)
            request = CURRENT_REQUEST.get()
            for f in frames:
                session.send_binary(_restamp(f, name, request))
            if value is not None and _too_big(value):
                raise RpcError(
                    ErrorCode.PLUGIN_LIMIT, tr("插件 {id} 的结果太大", id=plugin), {"plugin": plugin}
                )
            return RawResult(value if value is not None else {})

        return Route(spec, handler)

    def _static_report(self, c: Any) -> dict[str, Any]:
        from spaceknife_protocol.text import LANG

        token = LANG.set(i18n.language())
        try:
            return pp.Report.model_validate(
                {"title": c.spec.title.model_dump(), "blocks": c.spec.blocks}
            ).model_dump(mode="json")
        finally:
            LANG.reset(token)

    async def open_topic(
        self, session: Session, name: str, params: dict[str, Any] | None, rate_hz: float | None
    ) -> Subscription | None:
        """A plugin's shared state or stream (``$/subscribe {topic: plugin.<id>.<name>}``)."""
        from ..server.topics import Subscription

        parts = pp.split_wire_name(name)
        if parts is None:
            return None
        plugin, local = parts
        rec = self.records.get(plugin)
        if rec is None or rec.description is None:
            return None
        if CAP_PLUGINS not in session.capabilities:
            raise RpcError(ErrorCode.INVALID_REQUEST, "plugins.v1 capability was not negotiated")
        self.enabled(plugin)
        if local in {s.name for s in rec.description.states}:
            sub = Subscription(topic=name)
            subs = self._state_subs[(plugin, local)]
            subs[sub.id] = (session, sub)

            def unsubscribe(subs: dict[str, tuple[Session, Subscription]] = subs, sid: str = sub.id) -> None:
                subs.pop(sid, None)

            sub.on_close = unsubscribe
            session.emit_raw(name, {"sub": sub.id, "value": rec.states.get(local, {})})
            return sub
        if local in {s.name for s in rec.description.streams}:
            sub = Subscription(topic=name)
            key = f"{plugin}|{sub.id}"

            def chunk(value: Any, frames: list[bytes]) -> None:
                if sub.closed:
                    return
                for f in frames:
                    session.send_binary(_restamp(f, name, None))
                session.emit_raw(name, {"sub": sub.id, "value": value if not frames else None})

            async def run() -> None:
                try:
                    await self.call(plugin, "stream", local, params or {}, session=session, on_chunk=chunk)
                except (RpcError, asyncio.CancelledError):
                    pass
                finally:
                    self._stream_tasks.pop(key, None)

            task = asyncio.create_task(run(), name=f"plugin-stream-{name}")
            self._stream_tasks[key] = task
            sub.on_close = task.cancel  # type: ignore[assignment]
            return sub
        return None

    def close_session(self, session: Session) -> None:
        for key, task in list(self._latest.items()):
            if key[2] == session.id and not task.done():
                task.cancel()
        for subs in self._state_subs.values():
            for sid in [k for k, (s, _sub) in subs.items() if s is session]:
                subs.pop(sid, None)

    # ------------------------------------------------------------- extensions.changed

    def _on_extensions(self, kinds: frozenset[str]) -> None:
        self._changed |= set(kinds)
        loop = self._loop
        if loop is None or self._flush is not None:
            return
        try:
            self._flush = loop.call_later(EXTENSIONS_DEBOUNCE_S, self._send_changed)
        except RuntimeError:  # another thread, or the loop is closing
            with contextlib.suppress(RuntimeError):
                loop.call_soon_threadsafe(self._on_extensions, frozenset())

    def _send_changed(self) -> None:
        self._flush = None
        kinds = sorted(self._changed)
        self._changed.clear()
        if not kinds:
            return
        ev = pp.ExtensionsChangedEvent(version=EXTENSIONS.version, kinds=kinds)
        for s in self._sessions():
            s.emit(EVENT_EXTENSIONS, ev)

    def _broadcast_plugin(self, rec: PluginRecord) -> None:
        rt = rec.runtime
        ev = pp.PluginChangedEvent(id=rec.id, state=rec.state, process=rt.state if rt is not None else None)
        for s in self._sessions():
            s.emit(EVENT_PLUGIN, ev)

    # ------------------------------------------------------------- management

    def info(self, rec: PluginRecord) -> pp.PluginInfo:
        from .texts import permission_infos

        m = rec.manifest
        lang = i18n.language()
        counts: dict[str, int] = defaultdict(int)
        for c in EXTENSIONS.owned(rec.id):
            counts[c.kind] += 1
        rt = rec.runtime
        process = rt.info() if rt is not None else None
        if process is not None:
            process = process.model_copy(update={"restarts": rec.restarts})
        return pp.PluginInfo(
            id=rec.id,
            version=m.version if m else "",
            name=rec.name,
            description=m.description.get(lang) if m and m.description else "",
            author=m.author if m else "",
            icon=m.icon if m else "",
            homepage=m.homepage if m else "",
            license=m.license if m else "",
            state=rec.state,
            builtin=rec.builtin,
            dev=rec.dev,
            folder=str(rec.folder),
            hash=rec.hash,
            runtime=self._runtime_kind(rec) if m else "declarative",
            permissions=permission_infos(rec.requested, m.net_hosts if m else [], m.exec if m else []),
            net_hosts=list(m.net_hosts) if m else [],
            exec=list(m.exec) if m else [],
            granted=[p for p in pp.PERMISSIONS if p in rec.granted],
            contributions=dict(counts),
            keys=list(rec.keys),
            issues=[
                pp.PluginIssue(
                    code=i.code,
                    message=i.message,
                    file=i.file,
                    line=i.line,
                    severity="warning" if i.severity == "warning" else "error",
                )
                for i in rec.issues
            ],
            process=process,
        )

    def list_plugins(self) -> pp.PluginListResult:
        return pp.PluginListResult(
            plugins=[
                self.info(r) for r in sorted(self.records.values(), key=lambda r: (not r.builtin, r.id))
            ],
            folder=str(ppaths.plugins_dir()),
            dev_mode=self.dev_mode,
        )

    def get(self, plugin: str) -> PluginRecord:
        rec = self.records.get(plugin)
        if rec is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, tr("没有插件 {id}", id=plugin))
        return rec

    async def refresh(self, plugin: str) -> PluginRecord:
        """Read a plugin's folder again (its files may have changed)."""
        rec = self.get(plugin)
        fresh = await asyncio.to_thread(read_record, rec.folder, builtin=rec.builtin, dev=rec.dev)
        fresh.runtime, fresh.states, fresh.crashes, fresh.restarts = (
            rec.runtime,
            rec.states,
            rec.crashes,
            rec.restarts,
        )
        if fresh.hash != rec.hash or fresh.manifest != rec.manifest:
            fresh.description = None
        else:
            fresh.description = rec.description
        self.records[fresh.id] = fresh
        return fresh

    async def _preview(self, rec: PluginRecord) -> Description | None:
        """What a plugin adds, before it is enabled: a declarative one's manifest; a code plugin's
        description from the cache (by its files' hash) or by importing it once in a plugin
        process without permissions (``plugin check`` does the same)."""
        if rec.description is not None:
            return rec.description
        if rec.manifest is None:
            return None
        if not rec.has_code:
            try:
                return await asyncio.to_thread(describe_declarative, rec.folder, rec.manifest)
            except Exception:
                return None
        cache = ppaths.cache_dir(rec.id) / f"describe-{rec.hash.removeprefix('sha256:')[:32]}.json"
        if rec.hash and cache.is_file():
            with contextlib.suppress(OSError, ValueError):
                return Description.model_validate_json(cache.read_text(encoding="utf-8"))
        from .check import describe_in_subprocess

        desc, _issues = await asyncio.to_thread(describe_in_subprocess, rec.folder)
        if desc is not None and rec.hash and not rec.dev:
            with contextlib.suppress(OSError):
                cache.parent.mkdir(parents=True, exist_ok=True)
                cache.write_text(desc.model_dump_json(), encoding="utf-8")
        return desc

    def key_infos(self, rec: PluginRecord, desc: Description | None) -> list[pp.KeyInfo]:
        """The keys a plugin binds (its commands' keys and its keybindings), and what has them now."""
        from ..palette.commands import registry
        from ..palette.keymap import normalize

        lang = i18n.language()
        taken: dict[str, tuple[str, str]] = {}
        for c in registry():
            if c.owner == rec.id:
                continue
            for k in c.keys:
                taken.setdefault(normalize(k), (c.id, c.name))
        for kb in EXTENSIONS.of("keybinding"):
            if kb.owner != rec.id and kb.spec is not None:
                taken.setdefault(normalize(kb.spec.key), (kb.spec.command, kb.spec.command))
        out: list[pp.KeyInfo] = []
        items = desc.contributions if desc is not None else []
        titles: dict[str, str] = {}
        for item in items:
            if item.get("kind") == "command":
                spec = item.get("spec") or {}
                t = spec.get("title") or {}
                title = str((t.get("en") if lang == "en" else None) or t.get("zh", ""))
                full = f"{rec.id}.{spec.get('name', '')}"
                titles[full] = title
                for k in spec.get("keys") or []:
                    hit = taken.get(normalize(k))
                    out.append(pp.KeyInfo(key=k, command=full, title=title, conflict=hit[0] if hit else ""))
        for item in items:
            if item.get("kind") == "keybinding":
                spec = item.get("spec") or {}
                cmd = str(spec.get("command", ""))
                full = cmd if "." in cmd else f"{rec.id}.{cmd}"
                hit = taken.get(normalize(str(spec.get("key", ""))))
                conflict = hit[0] if hit and not spec.get("override") else ""
                out.append(
                    pp.KeyInfo(
                        key=str(spec.get("key", "")),
                        command=full,
                        title=titles.get(full, full),
                        conflict=conflict,
                    )
                )
        return out

    async def describe(self, plugin: str) -> pp.PluginDescribeResult:
        from .texts import contribution_groups, summary_lines

        rec = await self.refresh(plugin)
        entry = self.trust.get(rec.id)
        added = [p for p in rec.requested if entry is not None and p not in entry.granted]
        desc = await self._preview(rec)
        lang = i18n.language()
        groups = contribution_groups(desc, lang, rec.folder) if desc is not None else []
        keys = self.key_infos(rec, desc)
        conflicts = [tr("{key} 已用于 {command}", key=k.key, command=k.conflict) for k in keys if k.conflict]
        return pp.PluginDescribeResult(
            plugin=self.info(rec),
            added=added,  # type: ignore[arg-type]  # the manifest's permissions are Permission values
            conflicts=conflicts,
            summary=summary_lines(groups),
            groups=groups,
            keys=keys,
            unsigned=not rec.builtin,
        )

    async def enable(self, plugin: str, hash: str, grant: Sequence[str]) -> pp.PluginInfo:
        rec = await self.refresh(plugin)
        if rec.builtin:
            return self.info(rec)
        if rec.manifest is None or rec.state in ("error", "incompatible"):
            raise RpcError(
                ErrorCode.INVALID_PARAMS,
                tr(
                    "插件 {id} 无法启用：{why}",
                    id=plugin,
                    why="; ".join(i.message for i in rec.issues) or rec.state,
                ),
            )
        if not rec.dev and hash != rec.hash:
            raise RpcError(
                ErrorCode.INVALID_PARAMS, _("插件的文件在确认之后改动过，请重新查看"), {"hash": rec.hash}
            )
        if sorted(set(grant)) != sorted(set(rec.requested)):
            raise RpcError(
                ErrorCode.INVALID_PARAMS, _("必须同意插件要求的全部权限"), {"requested": rec.requested}
            )
        await self._stop_runtime(rec)
        EXTENSIONS.unregister_owner(rec.id)
        self.trust.confirm(rec.id, rec.manifest, rec.hash, list(rec.requested))
        rec.state = "needs_confirm"
        rec.crashes.clear()
        await self._settle(rec)
        self._broadcast_plugin(rec)
        if self.records[rec.id].state == "error":
            raise RpcError(
                ErrorCode.PLUGIN_ERROR,
                tr("插件 {id} 无法启用：{why}", id=plugin, why="; ".join(i.message for i in rec.issues)),
                {"issues": [i.as_dict() for i in rec.issues]},
            )
        return self.info(rec)

    async def disable(self, plugin: str) -> pp.PluginInfo:
        rec = self.get(plugin)
        if rec.builtin:
            raise RpcError(ErrorCode.INVALID_PARAMS, _("内置插件不能停用"))
        await self._deactivate(rec)
        self.trust.set_enabled(rec.id, False)
        rec.state = self._state_of(rec)
        self._broadcast_plugin(rec)
        return self.info(rec)

    async def restart(self, plugin: str) -> pp.PluginInfo:
        rec = self.get(plugin)
        if rec.builtin:  # nothing runs on its own: its code is called where it is used
            return self.info(rec)
        await self._stop_runtime(rec)
        rec.crashes.clear()
        if rec.state == "crashed":
            rec.state = self._state_of(rec)
        if rec.dev:
            rec = await self.refresh(plugin)
            if rec.state == "enabled" or self._state_of(rec) == "enabled":
                rec.description = None
                EXTENSIONS.unregister_owner(rec.id)
                await self._settle(rec)
        if rec.state == "enabled" and rec.has_code:
            await self._ensure_started(rec)
        self._broadcast_plugin(rec)
        return self.info(rec)

    async def install(self, path: str, replace: bool) -> pp.PluginInstallResult:
        def existing(pid: str) -> str | None:
            rec = self.records.get(pid)
            return (
                rec.manifest.version if rec is not None and rec.manifest is not None and not rec.dev else None
            )

        try:
            res = await asyncio.to_thread(inst.install, path, replace=replace, existing=existing)
        except inst.InstallError as exc:
            raise RpcError(ErrorCode.INVALID_PARAMS, str(exc)) from exc
        if res.installed and res.folder is not None:
            old = self.records.get(res.id)
            if old is not None:
                await self._deactivate(old)
            rec = await asyncio.to_thread(read_record, res.folder)
            self.records[rec.id] = rec
            await self._settle(rec)
            self._broadcast_plugin(rec)
        return pp.PluginInstallResult(
            id=res.id,
            version=res.version,
            installed=res.installed,
            conflict=res.conflict,
            existing_version=res.existing_version,
        )

    async def uninstall(self, plugin: str, remove_data: bool) -> None:
        rec = self.get(plugin)
        if rec.builtin:
            raise RpcError(ErrorCode.INVALID_PARAMS, _("内置插件不能卸载"))
        await self._deactivate(rec)
        if rec.dev:
            self.trust.unlink(rec.folder)
        else:
            try:
                await asyncio.to_thread(inst.uninstall, rec.folder, rec.id, remove_data=remove_data)
            except inst.InstallError as exc:
                raise RpcError(ErrorCode.INVALID_REQUEST, str(exc)) from exc
        self.trust.forget(rec.id)
        self.records.pop(rec.id, None)
        rec.state = "disabled"
        self._broadcast_plugin(rec)

    async def link(self, path: str) -> pp.PluginInfo:
        if not self.dev_mode:
            raise RpcError(ErrorCode.INVALID_REQUEST, _("链接开发中的插件需要先打开开发者模式"))
        folder = await asyncio.to_thread(lambda: Path(path).expanduser().resolve())
        rec = await asyncio.to_thread(read_record, folder, dev=True)
        if rec.manifest is None:
            raise RpcError(ErrorCode.INVALID_PARAMS, "; ".join(i.text() for i in rec.issues))
        self.trust.link(folder)
        old = self.records.get(rec.id)
        if old is not None:
            await self._deactivate(old)
        self.records[rec.id] = rec
        await self._settle(rec)
        self._broadcast_plugin(rec)
        return self.info(rec)

    async def link_dev(self, folder: Path, grant: bool = True) -> PluginRecord:
        """``plugin dev`` and tests: link a folder and enable it with what it asks for."""
        rec = await asyncio.to_thread(read_record, folder, dev=True)
        if rec.manifest is not None and grant:
            self.trust.confirm(rec.id, rec.manifest, "", list(rec.requested))
        old = self.records.get(rec.id)
        if old is not None:
            await self._deactivate(old)
        self.records[rec.id] = rec
        await self._settle(rec)
        return rec

    # ------------------------------------------------------------- hooks

    async def _hook(self, rec: PluginRecord, event: str, *, _scan: str | None = None, **args: Any) -> None:
        """Run the plugin's ``event`` hooks (one after the other); ``_scan``: the scan the hook is
        about (its ``ctx.tree``)."""
        need = HOOK_PERMISSIONS.get(event)
        if need is not None and need not in rec.granted:
            return  # it may not hear of this (plugin check says so)
        found = [c for c in EXTENSIONS.owned(rec.id) if c.kind == "hook" and c.spec.event == event]
        if not found:
            declared = rec.description is not None and any(
                d.get("kind") == "hook" and (d.get("spec") or {}).get("event") == event
                for d in rec.description.contributions
            )
            if declared:  # its code has one, the registry not (yet): say so instead of nothing
                logger.bind(source=f"plugin:{rec.id}").warning(
                    "hook {} of {} not run: its contributions are not registered (state {})",
                    event,
                    rec.id,
                    rec.state,
                )
            return
        ctx = {"scan_id": _scan} if _scan else None
        for c in found:
            limit = float(getattr(c.spec, "timeout", HOOK_TIMEOUT))
            try:
                await self.call(rec.id, "hook", c.local, {"event": event, **args}, ctx=ctx, timeout=limit)
            except (RpcError, asyncio.CancelledError) as exc:
                logger.bind(source=f"plugin:{rec.id}").warning(
                    "hook {} of {} failed: {}", event, rec.id, getattr(exc, "message", exc)
                )

    # ------------------------------------------------------------- cached answers

    def _forget(self, plugin: str) -> None:
        prefix = plugin + "."
        for key in [k for k in self._set_cache if k[0].startswith(prefix)]:
            self._set_cache.pop(key, None)
        for fkey in [k for k in self._field_cache if k[0].startswith(prefix)]:
            self._field_cache.pop(fkey, None)
        for wkey in [k for k in self._weights if k[0] == plugin]:
            self._weights.pop(wkey, None)
        for akey in [k for k in self._reasons if k[0].startswith(prefix)]:
            self._reasons.pop(akey, None)

    def basket_changed(self) -> None:
        """The delete basket changed: the plugins with a ``basket`` hook hear of it (once for a
        burst of changes)."""
        if self._loop is None or self._closed:
            return
        wanted = [
            rec
            for rec in self.records.values()
            if rec.state == "enabled"
            and any(c.kind == "hook" and c.spec.event == "basket" for c in EXTENSIONS.owned(rec.id))
        ]
        if not wanted or self._basket_hook_due:
            return
        self._basket_hook_due = True

        async def later() -> None:
            await asyncio.sleep(0.3)
            self._basket_hook_due = False
            for rec in wanted:
                if rec.state == "enabled":
                    await self._hook(rec, "basket")

        self._spawn(later())

    def invalidate(self, plugin: str, kinds: Sequence[str] = ()) -> None:
        """Drop what the host keeps of a plugin's answers (fields, node sets, a code visualization's
        weights, change reasons): its settings changed, or it called ``ctx.invalidate()``. The
        frontends ask for fields again; views that use its node sets filter again, views that
        show its visualization or colours draw again."""
        self.epochs[plugin] += 1
        self._forget(plugin)
        owned = {c.kind for c in EXTENSIONS.owned(plugin)}
        touched = (set(kinds) & owned) if kinds else owned & CACHED_KINDS
        if touched:
            EXTENSIONS.touch(frozenset(touched))
        views = getattr(self.server, "views", None)
        if views is not None and self._loop is not None:
            with contextlib.suppress(RuntimeError):
                self._spawn(views.plugin_changed(plugin))
        if self._loop is not None:  # its code legends: asked again now, ready when the menu opens
            lang = i18n.language()
            for c in EXTENSIONS.owned(plugin):
                if c.kind == "color_mode":
                    with contextlib.suppress(RuntimeError):
                        self.legend_of(c, lang)

    def settings_changed(self) -> None:
        """The settings changed: each plugin whose own values changed gets its ``settings`` hook with
        their names, and its cached answers go (they may depend on them)."""
        st = self.server.settings
        for rec in list(self.records.values()):
            if rec.state != "enabled":
                continue
            now = st.plugin_settings(rec.id)
            before = self._seen_settings.get(rec.id)
            self._seen_settings[rec.id] = dict(now)
            if before is None:
                continue
            changed = sorted(k for k in set(now) | set(before) if now.get(k) != before.get(k))
            if not changed:
                continue
            try:
                asyncio.get_running_loop()
            except RuntimeError:  # no loop (tests changing settings directly): only the caches go
                self.invalidate(rec.id)
                continue
            self._spawn(self._new_settings(rec, changed, dict(now)))

    async def _new_settings(self, rec: PluginRecord, changed: list[str], values: dict[str, Any]) -> None:
        """A plugin's settings changed: its process gets them first, then its cached answers go
        (legends, fields and weights are asked again, of a process that has the new values) and its
        ``settings`` hook runs (``ctx.settings`` already new)."""
        rt = rec.runtime
        if rt is not None and rt.state == "running":
            with contextlib.suppress(Exception):
                await rt.update({"settings": values})  # sent before every call that follows
        if rec.state != "enabled":
            return
        self.invalidate(rec.id)
        if any(c.kind == "hook" and c.spec.event == "settings" for c in EXTENSIONS.owned(rec.id)):
            await self._hook(rec, "settings", changed=changed)

    def legend_of(self, c: Any, lang: str) -> list[tuple[str, Any]]:
        """A colour mode's legend now, (text, colour or None) per palette entry: from the plugin's
        ``@plugin.color_legend`` (asked in the background, cached until its settings change), from
        the bands it follows, or as declared."""
        rec = self.records.get(c.owner)
        dynamic = (
            rec is not None
            and rec.description is not None
            and ["legend", c.local] in [list(h) for h in rec.description.handlers]
        )
        if dynamic and rec is not None and rec.state == "enabled":
            key = (c.owner, c.local, lang)
            hit = self._legends.get(key)
            if hit is not None and hit[0] == self.epochs[c.owner]:
                return hit[1]
            if key not in self._legend_asked:
                self._legend_asked.add(key)
                self._spawn(self._ask_legend(c, lang, key))
            if hit is not None:
                return hit[1]  # the one from before its settings changed, until the new one is here
        impl = c.impl
        if hasattr(impl, "legend"):
            return [(t, None) for t in impl.legend(lang)]
        return [(t.get(lang), None) for t in c.spec.legend]

    async def _ask_legend(self, c: Any, lang: str, key: tuple[str, str, str]) -> None:
        from spaceknife_protocol.text import LANG

        epoch = self.epochs[c.owner]
        try:
            token = LANG.set(lang)
            try:
                value, _ = await self.call(c.owner, "legend", c.local, {}, ctx={"lang": lang}, timeout=5.0)
            finally:
                LANG.reset(token)
            items = [
                (str(d.get("label", ""))[:80], d.get("color")) for d in (value or []) if isinstance(d, dict)
            ]
            self._legends[key] = (epoch, items)
            EXTENSIONS.touch(frozenset({"color_mode"}))
        except RpcError as exc:
            logger.bind(source=f"plugin:{c.owner}").warning("legend of {} failed: {}", c.id, exc.message)
        finally:
            self._legend_asked.discard(key)

    def fire(self, event: str, **args: Any) -> None:
        """Run every enabled plugin's ``event`` hooks (not waited for). A finished scan also starts
        the processes of the plugins whose fields or insights the details panel asks right away,
        so the first question does not wait for a process to start (a cold start is not counted in
        a field's time anyway, but the user waits for it)."""
        if event == "lang":
            self.tasks.relocalize()  # titles and texts of the running background tasks
        owners = {c.owner for c in EXTENSIONS.of("hook") if c.spec.event == event and c.owner != BUILTIN}
        scan = args.get("scan")
        about = str(scan.get("scan_id") or "") if event == "scan_done" and isinstance(scan, dict) else None
        for pid in owners:
            rec = self.records.get(pid)
            if rec is not None and rec.state == "enabled":
                self._spawn(self._hook(rec, event, _scan=about, **args))
        if event == "scan_done":
            asked = {c.owner for kind in ("node_field", "insight") for c in EXTENSIONS.of(kind)}
            for pid in asked - {BUILTIN}:
                rec = self.records.get(pid)
                if (
                    rec is not None
                    and rec.state == "enabled"
                    and (rec.runtime is None or rec.runtime.state != "running")
                ):
                    self._spawn(self._warm(rec))

    async def _warm(self, rec: PluginRecord) -> None:
        with contextlib.suppress(Exception):  # a plugin that cannot start says so when it is asked
            await self._ensure_started(rec)

    async def push_update(self, fields: dict[str, Any]) -> None:
        for rec in self.records.values():
            if rec.runtime is not None and rec.state == "enabled":
                body = dict(fields)
                if "settings" in body:
                    body["settings"] = self.server.settings.plugin_settings(rec.id)
                with contextlib.suppress(Exception):
                    await rec.runtime.update(body)

    # ------------------------------------------------------------- code on the render path

    def check_command(self, plugin: str, method: str, c: Any) -> str | None:
        """A command's own availability check (``check = "<method>"``): called with the
        :class:`ActionContext` the command would get (scan, view, selected node, zoom root, filter);
        50 ms, else available. It returns why the command is unavailable, or nothing."""
        try:
            value, _ = self.call_sync(plugin, "method", method, self.action_context(c), timeout=CHECK_TIMEOUT)
        except RpcError:
            return None
        return str(value) if isinstance(value, str) and value else None

    def action_context(self, c: Any) -> dict[str, Any]:
        """The palette's context as a command's :class:`ActionContext` (a check, a command run by
        the backend): where the user is."""
        view = getattr(c, "view", None)
        node = getattr(c, "selected_node", None)
        if view is None:
            return {"scan_id": None}
        filt = view.filter
        return {
            "scan_id": self.api.scan_of_store(view.store),
            "view_id": view.id,
            "node": node,
            "selection": [node] if node is not None else [],
            "zoom_node": int(view.zoom.node),
            "filter": filt.text if filt.active else "",
        }

    def annotate_changes(self, compare_id: str, items: list[Any]) -> list[Any]:
        """Reasons from the change annotators for items the insight rules left blank (compute
        threads): at most ``ANNOTATE_BATCH`` items per call, 200 ms each, remembered per comparison."""
        annotators = [c for c in EXTENSIONS.of("change_annotator") if c.owner in self.enabled_ids()]
        if not annotators:
            return items
        out = list(items)
        for c in annotators:
            todo: list[int] = []
            for i, it in enumerate(out):
                if it.reason or it.kind == "group":
                    continue
                key = (c.id, compare_id, it.path, it.status, int(it.delta))
                if key in self._reasons:
                    hit = self._reasons[key]
                    if hit:
                        out[i] = it.model_copy(update={"reason": hit})
                else:
                    todo.append(i)
            for at in range(0, len(todo), ANNOTATE_BATCH):
                chunk = todo[at : at + ANNOTATE_BATCH]
                payload = {
                    "items": [
                        {
                            "path": out[i].path,
                            "kind": out[i].kind,
                            "status": out[i].status,
                            "delta": int(out[i].delta),
                            "base": int(out[i].base),
                            "target": int(out[i].target),
                        }
                        for i in chunk
                    ]
                }
                try:
                    value, _ = self.call_sync(c.owner, "annotate", c.local, payload, timeout=ANNOTATE_TIMEOUT)
                except RpcError as exc:
                    logger.bind(source=f"plugin:{c.owner}").debug(
                        "change annotator {}: {}", c.id, exc.message
                    )
                    break  # this annotator is slow or failing now: no reasons from it this time
                reasons = value if isinstance(value, list) else []
                if len(self._reasons) > MAX_REASONS:
                    self._reasons.clear()
                for j, i in enumerate(chunk):
                    r = reasons[j] if j < len(reasons) else None
                    text = str(r)[:80] if isinstance(r, str) and r else ""
                    it = out[i]
                    self._reasons[(c.id, compare_id, it.path, it.status, int(it.delta))] = text
                    if text:
                        out[i] = it.model_copy(update={"reason": text})
        return out

    def set_masks(self, scan_id: str, store: Any, set_ids: Sequence[str]) -> dict[str, np.ndarray]:
        """Node sets of a scan (called from compute threads), cached by store version; 5 s each."""
        out: dict[str, np.ndarray] = {}
        running = self.api.running(scan_id)
        for sid in set_ids:
            c = EXTENSIONS.get("node_set", sid)
            if c is None:
                continue
            key = (sid, scan_id)
            hit = self._set_cache.get(key)
            epoch = self.epochs[c.owner]
            if hit is not None and (
                hit[0] == (store.version, epoch)
                or (running and hit[0][1] == epoch and time.monotonic() - hit[1] < SET_RUNNING_EVERY_S)
            ):
                out[sid] = hit[2]
                continue
            try:
                value, frames = self.call_sync(
                    c.owner,
                    "set",
                    c.local,
                    {"scan_id": scan_id},
                    ctx={"scan_id": scan_id},
                    timeout=SET_TIMEOUT,
                )
            except RpcError as exc:
                logger.bind(source=f"plugin:{c.owner}").warning(
                    "node set {} unavailable: {}", sid, exc.message
                )
                continue
            arr = decode_frame(frames[0])[1]["v"] if frames else np.zeros(0, dtype=np.uint8)
            n = len(store)
            mask = np.zeros(n, dtype=bool)
            if value.get("mask"):
                m = min(n, len(arr))
                mask[:m] = arr[:m].astype(bool)
            else:
                ids = arr.astype(np.int64)
                mask[ids[(ids >= 0) & (ids < n)]] = True
            self._set_cache[key] = ((store.version, epoch), time.monotonic(), mask)
            out[sid] = mask
        return out

    def set_mask_for_view(self, view: Any, set_id: str | None) -> np.ndarray | None:
        if not set_id:
            return None
        scan_id = self.api.scan_of_store(view.store)
        if scan_id is None:
            return None
        full = set_id if "." in set_id else None
        if full is None:
            return None
        return self.set_masks(scan_id, view.store, [full]).get(full)

    def remote_painter(self, plugin: str, name: str, palette: int) -> Callable[[Any, Any], np.ndarray]:
        def paint(view: Any, draw: Any) -> np.ndarray:
            rows = len(draw)
            out = np.zeros(rows, dtype=np.uint8)
            scan_id = self.api.scan_of_store(view.store)
            node = np.asarray(draw.node, dtype=np.int64)
            real = node >= 0
            if scan_id is None or not real.any():
                return out
            ids = node[real]
            frame = encode_frame(stream="plugin.color", layout="ids.v1", columns={"ids": ids})
            try:
                _value, frames = self.call_sync(
                    plugin,
                    "color",
                    name,
                    {"scan_id": scan_id, "palette": palette},
                    ctx={"scan_id": scan_id},
                    frames=[frame],
                    timeout=COLOR_TIMEOUT,
                )
            except RpcError as exc:
                if exc.code == ErrorCode.PLUGIN_TIMEOUT:
                    self._color_again(plugin, name, view.store)
                return out
            if frames:
                pal = decode_frame(frames[0])[1]["pal"]
                got = np.minimum(np.asarray(pal[: len(ids)], dtype=np.uint8), palette - 1)
                fill = np.zeros(len(ids), dtype=np.uint8)
                fill[: len(got)] = got
                out[real] = fill
            return out

        return paint

    def field_failed(self, field_id: str, node: int, why: str) -> None:
        """A node field raised: it is left out of the details panel and said in the plugin's log."""
        if len(self.field_errors) >= 64:
            self.field_errors.pop(next(iter(self.field_errors)))
        self.field_errors[(field_id, node)] = why
        now = time.monotonic()
        if now - self._field_logged.get(field_id, -FIELD_LOG_S) < FIELD_LOG_S:
            return  # a hover card asks often: the log says it once in a while
        self._field_logged[field_id] = now
        owner = field_id.rsplit(".", 1)[0]
        logger.bind(source=f"plugin:{owner}").warning("field {} on node {} failed: {}", field_id, node, why)

    def _color_again(self, plugin: str, name: str, store: Any) -> None:
        """A code colour mode's call timed out (from a compute thread): its views are drawn again a
        moment later, at most COLOR_RETRIES times per tree version."""
        key = (plugin, name, id(store), int(store.version))
        n = self._color_retries.get(key, 0)
        if n >= COLOR_RETRIES or self._loop is None or self._closed:
            return
        if len(self._color_retries) > 256:
            self._color_retries.clear()
        self._color_retries[key] = n + 1
        views = getattr(self.server, "views", None)
        if views is None:
            return

        async def later() -> None:
            await asyncio.sleep(COLOR_RETRY_S * (n + 1))
            await views.repaint_color(f"{plugin}.{name}")

        with contextlib.suppress(RuntimeError):
            self._loop.call_soon_threadsafe(lambda: self._spawn(later()))

    def remote_sizes(self, plugin: str, name: str, options: list[tuple[str, str]]) -> Callable[[Any], Any]:
        """A code visualization's areas: the plugin's weights per file, summed into folders."""
        from ..layout.api import ArraySizes, subtree_sums

        def sizes(basis: Any) -> Any:
            import json

            store = basis.store
            scan_id = self.api.scan_of_store(store)
            opts: dict[str, Any] = {}
            for opt, skey in options:
                with contextlib.suppress(KeyError):
                    opts[opt] = self.server.settings.get_value(skey)
            # the options are part of the key: two views with other options, or a change of them
            # (also through the view's own options), are other weights
            for opt, val in (getattr(basis, "options", None) or {}).items():
                if opt in opts:
                    opts[opt] = val
            okey = json.dumps(opts, sort_keys=True, default=str)
            key = (plugin, name, id(store), int(store.version), self.epochs[plugin], okey)
            hit = self._weights.get(key)
            if hit is None and scan_id is not None:
                try:
                    _value, frames = self.call_sync(
                        plugin,
                        "visual",
                        name,
                        {"scan_id": scan_id, "options": opts},
                        ctx={"scan_id": scan_id},
                        timeout=max(VISUAL_TIMEOUT, 5.0),
                    )
                    w = decode_frame(frames[0])[1]["v"] if frames else np.zeros(0)
                    leaf = np.zeros(len(store), dtype=np.int64)
                    m = min(len(leaf), len(w))
                    leaf[:m] = np.maximum(np.nan_to_num(np.asarray(w[:m], dtype=np.float64)), 0).astype(
                        np.int64
                    )
                    hit = subtree_sums(store.cols, len(store), leaf)
                except RpcError as exc:
                    logger.bind(source=f"plugin:{plugin}").warning(
                        "visualization {} failed: {}", name, exc.message
                    )
                    hit = None
                if hit is not None:
                    stale = [k for k in self._weights if k[:3] == key[:3] and k[3:5] != key[3:5]]
                    for k in stale:
                        self._weights.pop(k, None)
                    if len(self._weights) > 32:
                        self._weights.pop(next(iter(self._weights)))
                    self._weights[key] = hit
            if hit is None:
                from ..layout.api import StoreSizes

                return StoreSizes(basis)
            return ArraySizes(hit)

        return sizes

    def remote_layout(
        self, plugin: str, name: str, options: list[tuple[str, str]], store: Any
    ) -> tuple[Callable[[Any], Any], Callable[[], object]]:
        """A layout visualization's placer (layout/shapes.py): the hierarchy goes to the plugin's
        ``@plugin.layout`` function, its shapes come back; and what else its answer depends on
        (the plugin's cache epoch and the values of the settings ``options`` names)."""
        import json

        from spaceknife_protocol import view as pv

        from ..layout.shapes import Hierarchy, Placed, PlaceError

        def opts() -> dict[str, Any]:
            out: dict[str, Any] = {}
            for opt, skey in options:
                with contextlib.suppress(KeyError):
                    out[opt] = self.server.settings.get_value(skey)
            return out

        def key() -> object:
            return (self.epochs[plugin], json.dumps(opts(), sort_keys=True, default=str))

        def place(h: Hierarchy) -> Placed:
            scan_id = self.api.scan_of_store(store)
            o = opts()
            for opt, val in h.options:  # the view's own options win (a split pane, a reopened view)
                if opt in o:
                    o[opt] = val
            frame = encode_frame(stream="plugin.layout", layout="hierarchy.v1", columns=h.columns())
            try:
                value, frames = self.call_sync(
                    plugin,
                    "layout",
                    name,
                    {**h.params(), "scan_id": scan_id, "options": o},
                    ctx={"scan_id": scan_id} if scan_id else {},
                    frames=[frame],
                    timeout=LAYOUT_TIMEOUT,
                )
            except RpcError as exc:
                logger.bind(source=f"plugin:{plugin}").warning("layout {} failed: {}", name, exc.message)
                if exc.code == ErrorCode.PLUGIN_TIMEOUT:
                    raise PlaceError(tr("插件没有在 {s} 秒内排好布局", s=f"{LAYOUT_TIMEOUT:g}")) from None
                raise PlaceError(tr("插件排布局时出错：{why}", why=exc.message[:200])) from None
            cols = decode_frame(frames[0])[1] if frames else {}
            pts = decode_frame(frames[1])[1].get("p") if len(frames) > 1 else None
            n = len(h)
            if pts is not None and len(pts) > 2 * pv.MAX_POLY_POINTS * n:  # never from the SDK's Shapes
                raise PlaceError(tr("插件排布局时出错：{why}", why=f"{len(pts)} point coordinates"))

            def col(k: str, dt: Any) -> np.ndarray:
                v = cols.get(k)
                return np.zeros(n, dtype=dt) if v is None else np.asarray(v, dtype=dt)

            v = value if isinstance(value, dict) else {}
            root = v.get("root")
            root_t = None
            if isinstance(root, list) and len(root) == 6 and isinstance(root[5], list):
                with contextlib.suppress(TypeError, ValueError):
                    root_t = (*(float(a) for a in root[:5]), tuple(float(a) for a in root[5][: 2 * 12]))
            return Placed(
                shape=col("shape", np.uint8),
                x=col("x", np.float64),
                y=col("y", np.float64),
                w=col("w", np.float64),
                h=col("h", np.float64),
                r=col("r", np.float64),
                poly=col("poly", np.uint8),
                points=np.zeros(0) if pts is None else np.asarray(pts, dtype=np.float64),
                label=col("label", np.uint8),
                root=root_t,  # type: ignore[arg-type]
                message=str(v.get("message") or "")[:300],
            )

        return place, key

    def option_default(self, plugin: str, o: SettingSpec) -> Any:
        """An export option's default: with ``default_from``, that setting's current value."""
        if o.default_from is None:
            return o.default
        try:
            value = self.server.settings.get_value(f"plugin.{plugin}.{o.default_from}")
        except KeyError:
            return o.default
        if not isinstance(value, type(o.default)):
            return o.default
        if o.options and o.control != "multi" and value not in [v for v, _ in o.options]:
            return o.default
        return value

    def remote_format(self, rec: PluginRecord, spec: ExportFormatSpec) -> ExportFormat:
        """An export format whose chunks come from the plugin (on the compute thread of export.run)."""
        import queue

        from ..export import ExportFormat

        plugin, name = rec.id, spec.name
        host = self

        def chunks(
            store: Any,
            root: int,
            *,
            match: np.ndarray | None = None,
            filter_text: str = "",
            options: dict[str, Any] | None = None,
            selection: list[int] | None = None,
        ) -> Any:
            scan_id = host.api.scan_of_store(store)
            opts = {o.name: host.option_default(plugin, o) for o in spec.options}
            opts.update({k: v for k, v in (options or {}).items() if k in opts})
            q: queue.Queue[tuple[str, Any]] = queue.Queue(maxsize=64)

            def on_chunk(value: Any, frames: list[bytes]) -> None:
                for f in frames:
                    data = decode_frame(f)[1].get("data")
                    if data is not None:
                        q.put(("data", bytes(data)))

            frames = []
            if match is not None:
                frames.append(
                    encode_frame(
                        stream="plugin.export",
                        layout="match.v1",
                        columns={"match": np.asarray(match, dtype=np.uint8)},
                    )
                )
            loop = host._loop
            assert loop is not None
            fut = asyncio.run_coroutine_threadsafe(
                host.call(
                    plugin,
                    "export",
                    name,
                    {
                        "root": int(root),
                        "scan_id": scan_id,
                        "filter": filter_text,
                        "options": opts,
                        "selection": [int(n) for n in (selection or [])][:100_000],
                    },
                    ctx={"scan_id": scan_id},
                    frames=frames,
                    on_chunk=on_chunk,
                    timeout=600.0,
                ),
                loop,
            )
            fut.add_done_callback(lambda f: q.put(("end", f)))
            while True:
                try:
                    what, item = q.get(timeout=30.0)
                except Exception as exc:
                    fut.cancel()
                    raise OSError(_("插件导出 30 秒没有输出，已中止")) from exc
                if what == "data":
                    yield item if spec.binary else item.decode("utf-8", "surrogateescape")
                    continue
                failed = item.exception()
                if failed is not None:
                    raise OSError(getattr(failed, "message", str(failed)))
                return

        return ExportFormat(
            key=f"{plugin}.{name}",
            title_id=spec.title.zh,
            extension="." + spec.extension.lstrip("."),
            media_type=spec.media_type,
            bom=spec.bom,
            chunks=chunks,
            text=spec.title,
            description=spec.description,
            plugin=plugin,
            options=tuple(spec.options),
        )


def _restamp(frame: bytes, stream: str, request_id: Any) -> bytes:
    """A plugin's binary frame with the host's stream name and request id."""
    header, cols = decode_frame(frame)
    blobs = {k: bytes(v) for k, v in cols.items() if k in header.blobs}
    columns = {k: v for k, v in cols.items() if k not in header.blobs}
    return encode_frame(
        stream=stream[:64],
        layout=header.layout,
        columns=columns,
        blobs=blobs,
        seq=header.seq,
        final=header.final,
        request_id=request_id,
    )


def _too_big(value: Any) -> bool:
    import json

    try:
        return len(json.dumps(value, ensure_ascii=False)) > MAX_RESULT_JSON
    except (TypeError, ValueError):
        return True


__all__ = ["BinaryFrameHeader", "PluginHost", "RawParams", "RawResult"]
