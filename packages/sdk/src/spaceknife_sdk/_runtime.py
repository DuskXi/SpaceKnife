"""Running a plugin's code: the dispatcher (shared by both runtimes) and the plugin process's loop.

The host calls handlers by kind and name (:class:`Dispatcher`); each call gets its own
:class:`Context` and runs with the request's language. Async handlers run on the plugin's event loop,
plain functions on a small thread pool, so a long computation does not block other calls.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import contextlib
import contextvars
import importlib.util
import itertools
import os
import queue
import sys
import threading
import time
import traceback
from collections.abc import Callable, Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any

import numpy as np
from pydantic import BaseModel, ValidationError

from spaceknife_protocol.binary import decode_frame, encode_frame
from spaceknife_protocol.errors import ErrorCode, RequestError
from spaceknife_protocol.plugins import NodeRef, ReportContext
from spaceknife_protocol.text import CATALOG, LANG, UNIT_BASE

from . import wire
from ._current import CURRENT
from ._link import Image
from .context import CallInfo, Context, PluginSelf, RuntimeState
from .errors import Cancelled, HostError, ManifestError, PluginError
from .manifest import Manifest
from .models import (
    ChangeUnit,
    ColorInput,
    ExportInfo,
    ExportInput,
    Listing,
    Proposal,
    ScanInfo,
    SnapshotsChange,
    TreeChange,
    VisualInput,
)
from .plugin import Handler, Plugin, loaded
from .shapes import LayoutInput, Shapes
from .tree import Columns

MAX_CHUNK = 1 << 20
REQUEST_TIMEOUT = 120.0
SPARE_THREADS = 8  # threads that take over from cancelled plain-def calls still running
STUCK_AFTER = 30.0  # s: every call thread held by a cancelled call this long: the host restarts us
CANCEL_GRACE = 5.0  # s: a cancelled call still running after this long is named in the log
MAX_TASK_THREADS = 8  # background tasks written as plain def run on threads of their own
Emit = Callable[[Any, list[bytes]], None]


class Workers:
    """The threads plain ``def`` calls run on: ``threads`` of them at once.

    A thread cannot be stopped: a cancelled call is answered at once and its result dropped
    (``ctx.check()`` inside it raises from then on). Its place goes to the next call right away and
    the function finishes on a spare thread (up to ``spare`` of them), so a slow function that was
    asked for a 20 ms colour or a 30 ms hover field does not block the others. Only when the spare
    threads are used up too, a cancelled call keeps its place; when every place has been held by
    cancelled calls for ``stuck_after`` seconds, ``on_stuck`` is called (the host restarts the process)."""

    def __init__(
        self,
        threads: int,
        *,
        spare: int = SPARE_THREADS,
        stuck_after: float = STUCK_AFTER,
        name: str = "sk-plugin",
        on_stuck: Callable[[int], None] | None = None,
        log: Callable[[str], None] | None = None,
    ) -> None:
        self.threads = max(1, threads)
        self.spare = max(0, spare)
        self.stuck_after = stuck_after
        self.pool = concurrent.futures.ThreadPoolExecutor(
            max_workers=self.threads + self.spare, thread_name_prefix=name
        )
        self._slots: asyncio.Semaphore | None = None
        self.abandoned: dict[asyncio.Future[Any], tuple[float, str]] = {}  # finishing on a spare thread
        self.held: dict[asyncio.Future[Any], tuple[float, str]] = {}  # cancelled, keeping their place
        self._on_stuck = on_stuck
        self._log = log
        self._stuck_told = False

    def _sem(self) -> asyncio.Semaphore:
        if self._slots is None:
            self._slots = asyncio.Semaphore(self.threads)
        return self._slots

    async def run(self, fn: Callable[[], Any], what: str = "") -> Any:
        """``fn()`` on a worker thread once a place is free (wrap it in a ``contextvars`` copy's
        ``run`` when the call's language must come along)."""
        sem = self._sem()
        await sem.acquire()
        loop = asyncio.get_running_loop()
        fut = loop.run_in_executor(self.pool, fn)
        try:
            result = await asyncio.shield(fut)
        except asyncio.CancelledError:
            if fut.done():
                sem.release()
                raise
            now = time.monotonic()
            fut.add_done_callback(_drop_result)
            if len(self.abandoned) < self.spare:
                self.abandoned[fut] = (now, what)
                sem.release()  # its place goes to the next call now; a spare thread finishes it

                def gone(f: asyncio.Future[Any]) -> None:
                    self.abandoned.pop(f, None)

                fut.add_done_callback(gone)
            else:
                self.held[fut] = (now, what)

                def freed(f: asyncio.Future[Any]) -> None:
                    self.held.pop(f, None)
                    self._stuck_told = False
                    sem.release()

                fut.add_done_callback(freed)
                if len(self.held) >= self.threads:
                    loop.call_later(self.stuck_after, self._check_stuck)
            if self._log is not None and what:
                loop.call_later(CANCEL_GRACE, self._still_running, fut, what)
            raise
        except BaseException:
            sem.release()
            raise
        sem.release()
        return result

    def _still_running(self, fut: asyncio.Future[Any], what: str) -> None:
        if not fut.done() and self._log is not None:
            self._log(
                f"the cancelled call {what} still runs after {CANCEL_GRACE:g} s and keeps a thread: "
                "call ctx.check() in its loops (a plain def cannot be stopped from outside)"
            )

    def _check_stuck(self) -> None:
        now = time.monotonic()
        old = [f for f, (since, _w) in self.held.items() if not f.done() and now - since >= self.stuck_after]
        if len(old) >= self.threads and not self._stuck_told and self._on_stuck is not None:
            self._stuck_told = True
            self._on_stuck(len(old) + len(self.abandoned))

    def busy(self) -> int:
        """Cancelled calls whose functions still run."""
        return len(self.abandoned) + len(self.held)

    def shutdown(self) -> None:
        self.pool.shutdown(wait=False, cancel_futures=True)


class CallError(RequestError):
    """A call failed: what goes back to the host (protocol error code, message, data)."""


def error_of(exc: BaseException, log: Callable[[str], None] | None = None) -> CallError:
    """Map what a handler raised to a protocol error (tracebacks stay in the plugin's log)."""
    if isinstance(exc, CallError):
        return exc
    if isinstance(exc, PluginError):
        return CallError(ErrorCode.PLUGIN_ERROR, exc.message(), exc.data)
    if isinstance(exc, Cancelled | asyncio.CancelledError):
        return CallError(ErrorCode.REQUEST_CANCELLED, "request cancelled")
    if isinstance(exc, HostError):
        return CallError(exc.code, exc.message, exc.data)
    if isinstance(exc, PermissionError):
        return CallError(ErrorCode.PERMISSION_DENIED, str(exc))
    if isinstance(exc, ValidationError):
        details = [
            {"loc": [str(p) for p in e["loc"]], "msg": e["msg"], "type": e["type"]}
            for e in exc.errors(include_url=False, include_context=False, include_input=False)
        ]
        return CallError(ErrorCode.INVALID_PARAMS, "invalid params", details)
    if log is not None:
        log("".join(traceback.format_exception(exc)))
    return CallError(
        ErrorCode.PLUGIN_ERROR, f"plugin error: {type(exc).__name__}", {"type": type(exc).__name__}
    )


def _frame(columns: Mapping[str, Any], stream: str = "plugin", layout: str = "columns.v1") -> bytes:
    cols: dict[str, np.ndarray] = {}
    blobs: dict[str, bytes] = {}
    for k, v in columns.items():
        if isinstance(v, bytes | bytearray | memoryview):
            blobs[k] = bytes(v)
        else:
            arr = np.asarray(v)
            if arr.dtype == np.bool_:
                arr = arr.astype(np.uint8)
            if arr.dtype == np.int64 or arr.dtype == np.uint64 or arr.dtype.kind in "iuf":
                cols[k] = arr
            else:
                raise TypeError(f"column {k!r}: numeric arrays only, got {arr.dtype}")
    return encode_frame(stream=stream, layout=layout, columns=cols, blobs=blobs)


def _dump(value: Any) -> Any:
    if isinstance(value, BaseModel):
        return value.model_dump(mode="json")
    if isinstance(value, list | tuple):
        return [_dump(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _dump(v) for k, v in value.items()}
    if isinstance(value, np.generic):
        return value.item()
    return value


def _proposal(item: Any) -> dict[str, Any]:
    if isinstance(item, Proposal):
        p = item
    elif isinstance(item, Mapping):
        p = Proposal.model_validate(dict(item))
    elif isinstance(item, NodeRef | str):
        p = Proposal(node=item)
    else:
        p = Proposal(node=int(item))
    d = p.model_dump(mode="json")
    if isinstance(p.node, NodeRef):
        d["node"] = p.node.model_dump(mode="json")
    return d


class Dispatcher:
    """Runs the calls of one plugin (on the event loop it is used from)."""

    def __init__(
        self,
        plugin: Plugin,
        state: RuntimeState,
        link: Any,
        *,
        log: Callable[[str], None] | None = None,
        threads: int = 2,
        spare: int = SPARE_THREADS,
        stuck_after: float = STUCK_AFTER,
    ) -> None:
        self.plugin = plugin
        self.state = state
        self.link = link
        self.log = log or (lambda text: link.notify("log", {"level": "ERROR", "message": text[-8000:]}))
        self.threads = threads

        def warn(text: str) -> None:
            link.notify("log", {"level": "WARNING", "message": text[:8000]})

        def stuck(n: int) -> None:
            link.notify("stuck", {"threads": n})

        self.workers = Workers(threads, spare=spare, stuck_after=stuck_after, on_stuck=stuck, log=warn)
        # background tasks written as plain def: threads of their own (they may run for hours)
        self.task_workers = Workers(MAX_TASK_THREADS, spare=MAX_TASK_THREADS, name="sk-task", log=warn)
        self.executor = self.workers.pool
        self.cancels: dict[int, threading.Event] = {}
        self.tasks: dict[int, asyncio.Task[Any]] = {}
        state.state_models.update(plugin.states)
        state.event_names.update({m: n for n, m in plugin.events.items()})
        for name, model in plugin.states.items():
            if name not in state.states:
                with contextlib.suppress(Exception):
                    state.states[name] = model().model_dump(mode="json")

    def close(self) -> None:
        for ev in self.cancels.values():
            ev.set()
        for task in self.tasks.values():
            task.cancel()
        self.workers.shutdown()
        self.task_workers.shutdown()

    def busy(self) -> list[int]:
        return sorted(self.cancels)

    def cancel(self, call_id: int) -> None:
        ev = self.cancels.get(call_id)
        if ev is not None:
            ev.set()
        task = self.tasks.get(call_id)
        if task is not None:
            task.cancel()

    def update(self, msg: Mapping[str, Any]) -> None:
        st = self.state
        if msg.get("lang"):
            st.lang = str(msg["lang"])
        if msg.get("settings") is not None:
            st.settings = dict(msg["settings"])
        if msg.get("granted") is not None:
            st.granted = frozenset(msg["granted"])
        if msg.get("roots") is not None:
            st.roots = [str(r) for r in msg["roots"]]
            self._paths_changed()

    def _paths_changed(self) -> None:
        st = self.state
        if st.on_paths is not None:
            st.on_paths([*st.roots, *st.picked])

    def picked(self, uri: Any) -> None:
        """A scan source's URI (the user chose it): readable from now on, as scanned folders are."""
        st = self.state
        if "files.read" not in st.granted or not isinstance(uri, str) or not os.path.isabs(uri):
            return
        real = os.path.realpath(uri)
        if real not in st.picked:
            st.picked = [*st.picked[-63:], real]
            self._paths_changed()

    # ------------------------------------------------------------- calls

    async def run(
        self,
        call_id: int,
        kind: str,
        name: str,
        params: Any,
        ctx_info: Mapping[str, Any],
        frames: Sequence[bytes] = (),
        emit: Emit | None = None,
    ) -> tuple[Any, list[bytes]]:
        """Run one call; returns (JSON value, binary frames) or raises :class:`CallError`."""
        handler = self.plugin.handlers.get((kind, name))
        if kind in ("source", "watch") and isinstance(params, Mapping):
            self.picked(params.get("uri"))
        if handler is None:
            raise CallError(ErrorCode.METHOD_NOT_FOUND, f"{self.plugin.id} has no {kind} {name!r}")
        info = CallInfo.from_dict({**ctx_info, "id": call_id, "kind": kind, "name": name})
        cancel = threading.Event()
        self.cancels[call_id] = cancel
        ctx = Context(self.state, self.link, info, cancel=cancel, executor=self.workers)
        if kind == "task":
            ctx._follow_lang = True  # it may outlive a language switch: T(...) follows at ctx.check()
        tokens = (
            LANG.set(str(ctx_info.get("lang") or self.state.lang)),
            CATALOG.set(self.state.catalog),
            CURRENT.set(ctx),
            UNIT_BASE.set(info.units),
        )
        task = asyncio.current_task()
        if task is not None:
            self.tasks[call_id] = task
        try:
            return await self._invoke(handler, ctx, params, list(frames), emit or (lambda v, f: None))
        except BaseException as exc:
            if isinstance(exc, asyncio.CancelledError) and not cancel.is_set():
                raise
            raise error_of(exc, self.log) from None
        finally:
            UNIT_BASE.reset(tokens[3])
            CURRENT.reset(tokens[2])
            CATALOG.reset(tokens[1])
            LANG.reset(tokens[0])
            self.cancels.pop(call_id, None)
            self.tasks.pop(call_id, None)

    async def _call(self, h: Handler, *args: Any, workers: Workers | None = None) -> Any:
        if h.is_async:
            return await h.fn(*args)
        cv = contextvars.copy_context()
        # cancelled: answered at once, the function finishes on a spare thread (Workers)
        return await (workers or self.workers).run(lambda: cv.run(h.fn, *args), f"{h.kind} {h.name!r}")

    async def _invoke(
        self, h: Handler, ctx: Context, params: Any, frames: list[bytes], emit: Emit
    ) -> tuple[Any, list[bytes]]:
        kind = h.kind
        p = params if isinstance(params, Mapping) else {}
        if kind in ("method", "check"):
            if h.params is not None and h.arity >= 2:
                arg = h.params.model_validate(dict(p))
                scan = getattr(arg, "scan_id", None)  # an ActionContext: ctx.tree is its scan
                if isinstance(scan, str) and scan and not ctx.call_info.scan_id:
                    ctx.call_info.scan_id = scan
                result = await self._call(h, ctx, arg)
            else:
                result = await self._call(h, ctx)
            return self._value(result)
        if kind in ("report", "cleanup"):
            c = ReportContext.model_validate(dict(p))
            if c.scan_id and not ctx.call_info.scan_id:
                ctx.call_info.scan_id = c.scan_id
            result = await self._call(h, ctx, c)
            if kind == "cleanup":
                return [_proposal(i) for i in (result or [])], []
            return self._value(result)
        if kind in ("field", "insight"):
            node = ctx.tree.node(int(p["node"]))
            result = await self._call(h, ctx, node)
            if isinstance(result, str):
                return {"type": "field", "value": result, "unit": "text"}, []
            return self._value(result)
        if kind == "set":
            result = await self._call(h, ctx, ctx.tree)
            arr = np.asarray(result)
            if arr.dtype == np.bool_:
                return {"mask": True, "n": int(arr.shape[0])}, [_frame({"v": arr.astype(np.uint8)})]
            return {"mask": False, "n": int(arr.shape[0])}, [_frame({"v": arr.astype(np.int64)})]
        if kind == "visual":
            v = VisualInput(tree=ctx.tree, options=dict(p.get("options") or {}), ctx=ctx)
            result = await self._call(h, v)
            arr = np.asarray(result)
            return {"n": int(arr.shape[0])}, [_frame({"v": arr.astype(np.float64)})]
        if kind == "layout":
            cols = decode_frame(frames[0])[1] if frames else {}
            li = LayoutInput.from_wire(dict(p), cols, ctx)
            shaped = await self._call(h, li)
            if shaped is None:
                shaped = li.shapes()
            # said to the author as they are (the frame shows them, MockHost.visual raises them)
            if not isinstance(shaped, Shapes):
                raise PluginError("a layout function returns Shapes (v.shapes(), or shapes.pack(v))")
            if len(shaped) != len(li):
                raise PluginError(f"Shapes for {len(shaped)} blocks, the layout got {len(li)}")
            value, placed, points = shaped.to_wire()
            return value, [_frame(placed), _frame({"p": points})]
        if kind == "color":
            ids = decode_frame(frames[0])[1]["ids"] if frames else np.zeros(0, dtype=np.int64)
            ids = np.asarray(ids, dtype=np.int64)
            if h.arity == 1:
                ci = ColorInput(tree=ctx.tree, ids=ids, palette=int(p.get("palette") or 1), ctx=ctx)
                result = await self._call(h, ci)
            else:
                result = await self._call(h, ctx.tree, ids)
            arr = np.asarray(result)
            return {"n": int(arr.shape[0])}, [_frame({"pal": arr.astype(np.uint8)})]
        if kind == "legend":
            result = await self._call(h, ctx)
            return [_legend_item(i) for i in (result or [])][:16], []
        if kind == "export":
            match = None
            if frames:
                match = decode_frame(frames[0])[1]["match"].astype(bool)
            root = int(p.get("root", 0))
            tree = ctx.tree
            filt = str(p.get("filter") or "")
            options = dict(p.get("options") or {})
            if h.arity == 1:
                x = ExportInput(
                    tree=tree,
                    root=root,
                    match=match,
                    filter=filt,
                    options=options,
                    selection=[int(n) for n in p.get("selection") or []],
                    ctx=ctx,
                )
                gen = h.fn(x)
            elif h.arity >= 4:
                gen = h.fn(tree, root, match, ExportInfo(filter=filt, options=options))
            else:
                gen = h.fn(tree, root, match)
            cv = contextvars.copy_context()
            count = await self.workers.run(lambda: cv.run(self._drain, gen, ctx, emit), f"export {h.name!r}")
            return {"chunks": count}, []
        if kind == "source":
            out = Listing(
                flush=lambda batch: emit({"count": len(batch["kind"])}, [_frame(_listing_cols(batch))])
            )
            await self._call(h, ctx, str(p.get("uri", "")), out)
            out.flush()
            return {"root": out.root, "entries": out.sent, "duplicates": out.duplicates}, []
        if kind in ("watch", "stream"):
            if kind == "stream" and h.params is not None and h.arity >= 2:
                agen = h.fn(ctx, h.params.model_validate(dict(p)))
            elif kind == "stream":
                agen = h.fn(ctx)
            else:
                agen = h.fn(ctx, str(p.get("uri", "")))
            n = 0
            async for item in agen:
                ctx.check()
                n += 1
                if isinstance(item, Columns):
                    emit({"layout": item.layout}, [_frame(item, layout=item.layout)])
                elif kind == "watch":
                    emit([_dump(c) for c in item], [])
                else:
                    emit(_dump(item), [])
            return {"items": n}, []
        if kind == "annotate":
            items = [ChangeUnit.model_validate(d) for d in p.get("items", [])]
            result = await self._call(h, ctx, items)
            return [None if r is None else str(r)[:80] for r in (result or [])], []
        if kind == "hook":
            event = str(p.get("event", ""))
            if event in ("scan_done", "scan_closed"):
                await self._call(h, ctx, ScanInfo.model_validate(p.get("scan") or {"scan_id": ""}))
            elif event == "tree_changed":
                await self._call(h, ctx, TreeChange.model_validate(p.get("change") or {"scan_id": ""}))
            elif event == "snapshots":
                await self._call(h, ctx, SnapshotsChange.model_validate(p.get("change") or {}))
            elif event == "settings":
                await self._call(h, ctx, list(p.get("changed") or []))
            else:
                await self._call(h, ctx)
            return None, []
        if kind == "task":
            raw = p.get("params")
            if h.params is not None and h.arity >= 2:
                await self._call(h, ctx, h.params.model_validate(dict(raw or {})), workers=self.task_workers)
            elif h.arity >= 2:
                await self._call(h, ctx, dict(raw or {}), workers=self.task_workers)
            else:
                await self._call(h, ctx, workers=self.task_workers)
            return None, []
        raise CallError(ErrorCode.METHOD_NOT_FOUND, f"unknown call kind {kind!r}")

    def _drain(self, gen: Iterable[Any], ctx: Context, emit: Emit) -> int:
        n = 0
        for chunk in gen:
            ctx.check()
            if isinstance(chunk, str):
                data = chunk.encode("utf-8", "surrogatepass" if os.name == "nt" else "surrogateescape")
            elif isinstance(chunk, bytes | bytearray):
                data = bytes(chunk)
            else:
                raise TypeError("an export format yields str (or bytes with binary = true)")
            for a in range(0, len(data), MAX_CHUNK):
                emit({"bytes": len(data[a : a + MAX_CHUNK])}, [_frame({"data": data[a : a + MAX_CHUNK]})])
                n += 1
        return n

    def _value(self, result: Any) -> tuple[Any, list[bytes]]:
        if isinstance(result, Columns):
            return {"columns": True, "layout": result.layout}, [_frame(result, layout=result.layout)]
        if isinstance(result, np.ndarray):
            return {"columns": True, "layout": "columns.v1"}, [_frame({"v": result})]
        return _dump(result), []


def _drop_result(fut: asyncio.Future[Any]) -> None:
    """A cancelled thread's result: retrieved (so nothing is logged) and dropped."""
    if not fut.cancelled():
        fut.exception()


def _legend_item(item: Any) -> Any:
    """A legend entry from code: a text, or ``(text, colour)``."""
    from spaceknife_protocol.text import localize

    if isinstance(item, tuple | list) and len(item) == 2:
        label, color = item
        return {"label": str(localize(label) or ""), "color": _dump(color)}
    return {"label": str(localize(item) or ""), "color": None}


def _listing_cols(batch: dict[str, Any]) -> dict[str, Any]:
    out = {k: v for k, v in batch.items() if k != "_paths"}
    out["paths"] = batch["_paths"]
    return out


# --------------------------------------------------------------------------- loading a plugin's code


def load_plugin(folder: Path, manifest: Manifest) -> Plugin:
    """Import the plugin's entry (its ``Plugin()`` picks this folder up), or wrap a declarative one."""
    if manifest.entry is None:
        return Plugin(manifest=manifest, dir=folder)
    entry = folder / manifest.entry
    lib = folder / "lib"
    for p in (str(lib), str(folder)):
        if Path(p).is_dir() and p not in sys.path:
            sys.path.insert(0, p)
    modname = "skplugin_" + manifest.id.replace(".", "_").replace("-", "_")
    target = entry / "__init__.py" if entry.is_dir() else entry
    if not target.is_file():
        raise ManifestError([_issue("no_entry", f"entry {manifest.entry!r} does not exist")])
    spec = importlib.util.spec_from_file_location(
        modname, target, submodule_search_locations=[str(entry)] if entry.is_dir() else None
    )
    if spec is None or spec.loader is None:
        raise ManifestError([_issue("no_entry", f"cannot import {manifest.entry!r}")])
    module = importlib.util.module_from_spec(spec)
    sys.modules[modname] = module
    with loaded(folder, manifest) as loading:
        try:
            spec.loader.exec_module(module)
        except BaseException:
            sys.modules.pop(modname, None)
            raise
        made = loading.instance()
    found = getattr(module, "plugin", None)
    plugin = found if isinstance(found, Plugin) else made
    if plugin is None:
        raise ManifestError([_issue("no_plugin", f"{manifest.entry} does not create plugin = Plugin()")])
    return plugin


def unload_plugin(manifest: Manifest) -> None:
    modname = "skplugin_" + manifest.id.replace(".", "_").replace("-", "_")
    for name in [m for m in sys.modules if m == modname or m.startswith(modname + ".")]:
        sys.modules.pop(name, None)


def _issue(code: str, message: str) -> Any:
    from .errors import Issue

    return Issue(code, message)


def state_from_init(init: Mapping[str, Any]) -> RuntimeState:
    return RuntimeState(
        plugin=PluginSelf(
            id=str(init["plugin"]),
            version=str(init.get("version", "")),
            dir=Path(str(init["dir"])),
            storage=Path(str(init["storage"])),
            builtin=bool(init.get("builtin", False)),
            dev=bool(init.get("dev", False)),
        ),
        granted=frozenset(init.get("granted", ())),
        settings=dict(init.get("settings") or {}),
        lang=str(init.get("lang", "zh-CN")),
        roots=[str(r) for r in init.get("roots", ())],
        net_hosts=[str(h) for h in init.get("net_hosts", ())],
        exec=[str(e) for e in init.get("exec", ())],
        states={k: dict(v) for k, v in (init.get("states") or {}).items()},
        threads=max(1, min(int(init.get("threads", 2)), 64)),
        spare_threads=max(0, min(int(init.get("spare_threads", SPARE_THREADS)), 64)),
        stuck_after=max(0.1, float(init.get("stuck_after", STUCK_AFTER))),
    )


def dispatcher_for(plugin: Plugin, state: RuntimeState, link: Any) -> Dispatcher:
    """The dispatcher of a plugin with the worker threads its init asked for."""
    return Dispatcher(
        plugin,
        state,
        link,
        threads=state.threads,
        spare=state.spare_threads,
        stuck_after=state.stuck_after,
    )


# --------------------------------------------------------------------------- the plugin process


class ChildLink:
    """The plugin process's link to the host: requests over the channel, answered by id."""

    def __init__(self, send: Callable[[dict[str, Any], Sequence[bytes]], None]) -> None:
        self._send = send
        self._ids = itertools.count(1)
        self._pending: dict[int, concurrent.futures.Future[tuple[Any, list[bytes]]]] = {}
        self._lock = threading.Lock()
        self._images: dict[str, Image] = {}

    def request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        frames: Sequence[bytes] = (),
        timeout: float | None = None,
    ) -> tuple[Any, list[bytes]]:
        fut: concurrent.futures.Future[tuple[Any, list[bytes]]] = concurrent.futures.Future()
        with self._lock:
            rid = next(self._ids)
            self._pending[rid] = fut
        self._send(
            {"type": "request", "id": rid, "method": method, "params": params, "frames": len(frames)}, frames
        )
        try:
            return fut.result(timeout or REQUEST_TIMEOUT)
        except concurrent.futures.TimeoutError:
            raise HostError(ErrorCode.PLUGIN_TIMEOUT, f"the host did not answer {method}") from None
        finally:
            with self._lock:
                self._pending.pop(rid, None)

    def on_reply(self, msg: Mapping[str, Any], frames: list[bytes]) -> None:
        with self._lock:
            fut = self._pending.get(int(msg["id"]))
        if fut is None or fut.done():
            return
        err = msg.get("error")
        if err:
            fut.set_exception(
                HostError(int(err.get("code", -32603)), str(err.get("message", "")), err.get("data"))
            )
        else:
            fut.set_result((msg.get("value"), frames))

    def fail_all(self, message: str) -> None:
        with self._lock:
            pending = list(self._pending.values())
        for fut in pending:
            if not fut.done():
                fut.set_exception(HostError(ErrorCode.PLUGIN_UNAVAILABLE, message))

    def notify(self, type: str, payload: dict[str, Any]) -> None:
        self._send({"type": type, **payload}, ())

    def image(self, scan_id: str, columns: Sequence[str]) -> Image:
        from .image import map_image

        for attempt in range(2):
            value, _ = self.request("host.image", {"scan_id": scan_id, "columns": list(columns)})
            key = str(value.get("key", ""))
            hit = self._images.get(scan_id)
            if hit is not None and hit.key == key and set(columns) <= set(hit.columns):
                return hit
            try:
                image = map_image(value)
                break
            except OSError as exc:  # released between the answer and the mapping: ask once more
                if attempt:
                    raise HostError(ErrorCode.UNKNOWN_SCAN, f"the tree image is gone: {exc}") from None
        if hit is not None:
            hit.close()
            with contextlib.suppress(HostError):
                self.notify("image_drop", {"key": hit.key})
        self._images[scan_id] = image
        return image

    def forget_image(self, scan_id: str) -> None:
        """A snapshot was closed: unmap its image (the host drops its side)."""
        hit = self._images.pop(scan_id, None)
        if hit is not None:
            hit.close()

    def store_of(self, scan_id: str) -> Any:
        return None


class ChildServer:
    """The plugin process: a reader and a writer thread around one asyncio loop."""

    def __init__(
        self,
        read_frame: Callable[[], tuple[int, bytes]],
        write: Callable[[list[bytes]], None],
    ) -> None:
        self._read_frame = read_frame
        self._write = write
        self._out: queue.Queue[list[bytes] | None] = queue.Queue()
        self.link = ChildLink(self.send)
        self.loop = asyncio.new_event_loop()
        self.dispatcher: Dispatcher | None = None
        self.guard: Any = None  # the audit hook: the channel's threads may make native calls
        self._stopping = False
        self._writer = threading.Thread(target=self._write_loop, name="sk-plugin-writer", daemon=True)
        self._writer.start()

    def _host_thread(self) -> None:
        g = self.guard
        if g is not None:
            g.allow_thread()

    def close_output(self) -> None:
        """Send what is queued, then stop the writer (before exiting)."""
        self._out.put(None)
        self._writer.join(5.0)

    # ------------------------------------------------------------- output

    def send(self, msg: dict[str, Any] | BaseModel, frames: Sequence[bytes] = ()) -> None:
        parts: list[bytes] = []
        for f in frames:
            parts += wire.frame_binary(f)
        parts += wire.frame_json(msg)
        self._out.put(parts)

    def _write_loop(self) -> None:
        while True:
            parts = self._out.get()
            if parts is None:
                return
            self._host_thread()
            try:
                self._write(parts)
            except (OSError, ValueError, EOFError):
                os._exit(3)  # the host is gone

    def read_message(self) -> tuple[dict[str, Any], list[bytes]]:
        """The next JSON message and the binary frames before it."""
        import json

        frames: list[bytes] = []
        while True:
            kind, body = self._read_frame()
            if kind == wire.KIND_BINARY:
                frames.append(bytes(body))
                continue
            return json.loads(body), frames

    # ------------------------------------------------------------- serving

    def start(self, plugin: Plugin, state: RuntimeState) -> None:
        self.dispatcher = dispatcher_for(plugin, state, self.link)

    def serve(self) -> int:
        reader = threading.Thread(target=self._read_loop, name="sk-plugin-reader", daemon=True)
        reader.start()
        beat = threading.Thread(target=self._beat_loop, name="sk-plugin-heartbeat", daemon=True)
        beat.start()
        try:
            self.loop.run_forever()
        finally:
            if self.dispatcher is not None:
                self.dispatcher.close()
            self._out.put(None)
            self._writer.join(2.0)
        return 0

    def _beat_loop(self) -> None:
        while not self._stopping:
            time.sleep(wire.HEARTBEAT_S)
            d = self.dispatcher
            if d is not None and d.busy():
                self.send({"type": "heartbeat", "busy": d.busy()[:64], "rss": _rss()})

    def _read_loop(self) -> None:
        self._host_thread()
        try:
            while True:
                msg, frames = self.read_message()
                t = msg.get("type")
                if t == "reply":
                    self.link.on_reply(msg, frames)
                elif t == "cancel":
                    d = self.dispatcher
                    if d is not None:
                        ev = d.cancels.get(int(msg["id"]))
                        if ev is not None:
                            ev.set()
                        self.loop.call_soon_threadsafe(d.cancel, int(msg["id"]))
                elif t == "call":
                    self.loop.call_soon_threadsafe(self._start_call, msg, frames)
                elif t == "update":
                    if self.dispatcher is not None:
                        self.loop.call_soon_threadsafe(self.dispatcher.update, msg)
                elif t == "shutdown":
                    self.loop.call_soon_threadsafe(self._shutdown)
                    return
        except (EOFError, OSError, ValueError):
            pass
        self.link.fail_all("the host closed the connection")
        self._stopping = True
        os._exit(0)  # no host: nothing left to do

    def _start_call(self, msg: dict[str, Any], frames: list[bytes]) -> None:
        self.loop.create_task(self._run_call(msg, frames))

    async def _run_call(self, msg: dict[str, Any], frames: list[bytes]) -> None:
        d = self.dispatcher
        call_id = int(msg["id"])
        assert d is not None

        def emit(value: Any, fr: list[bytes]) -> None:
            self.send({"type": "chunk", "id": call_id, "value": value, "frames": len(fr)}, fr)

        try:
            value, out = await d.run(
                call_id,
                str(msg["kind"]),
                str(msg["name"]),
                msg.get("params"),
                msg.get("ctx") or {},
                frames,
                emit,
            )
        except CallError as err:
            self.send(
                {
                    "type": "error",
                    "id": call_id,
                    "code": err.code,
                    "message": err.message[:8000],
                    "data": err.data,
                }
            )
            return
        except asyncio.CancelledError:
            self.send(
                {"type": "error", "id": call_id, "code": ErrorCode.REQUEST_CANCELLED, "message": "cancelled"}
            )
            return
        except Exception as exc:  # an encoding problem of the result
            failed = error_of(exc, d.log)
            self.send(
                {
                    "type": "error",
                    "id": call_id,
                    "code": failed.code,
                    "message": failed.message,
                    "data": failed.data,
                }
            )
            return
        try:
            self.send({"type": "result", "id": call_id, "value": value, "frames": len(out)}, out)
        except (TypeError, ValueError) as exc:
            failed = error_of(exc, d.log)
            self.send({"type": "error", "id": call_id, "code": failed.code, "message": failed.message})

    def _shutdown(self) -> None:
        async def stop() -> None:
            d = self.dispatcher
            if d is not None:
                for (kind, name), h in list(d.plugin.handlers.items()):
                    spec_event = _hook_event(d.plugin, name) if kind == "hook" else None
                    if spec_event == "stop":
                        with contextlib.suppress(Exception):
                            await asyncio.wait_for(
                                d.run(0x7FFFFFFF, "hook", h.name, {"event": "stop"}, {}), timeout=2.0
                            )
            self._stopping = True
            self.loop.stop()

        self.loop.create_task(stop())


def _hook_event(plugin: Plugin, name: str) -> str | None:
    for kind, spec in plugin.declared:
        if kind == "hook" and getattr(spec, "name", None) == name:
            return str(getattr(spec, "event", ""))
    return None


def _rss() -> int:
    try:
        if sys.platform.startswith("linux"):
            with open("/proc/self/statm") as fh:
                return int(fh.read().split()[1]) * os.sysconf("SC_PAGE_SIZE")
        if sys.platform == "win32":  # no resource module there (the host reads it from outside)
            return 0
        import resource

        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        return int(rss if sys.platform == "darwin" else rss * 1024)
    except (OSError, ValueError, ImportError):
        return 0


def describe_safely(plugin: Plugin) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
    """(description, error) for the ``ready`` message."""
    try:
        return plugin.describe().model_dump(mode="json"), None
    except ManifestError as exc:
        return None, {"code": "invalid", "message": str(exc), "issues": [i.as_dict() for i in exc.issues]}
    except Exception as exc:
        return None, {"code": "describe", "message": f"{type(exc).__name__}: {exc}"}


__all__ = [
    "CallError",
    "ChildLink",
    "ChildServer",
    "Dispatcher",
    "Workers",
    "describe_safely",
    "dispatcher_for",
    "error_of",
    "load_plugin",
    "state_from_init",
    "unload_plugin",
]
