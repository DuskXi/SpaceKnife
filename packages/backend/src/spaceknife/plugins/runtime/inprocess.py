"""Plugin code inside the backend process: built-in plugins, and plugins in developer mode.

The code runs on its own thread with its own event loop (as in a plugin process): its blocking
host requests are answered by the backend's loop meanwhile. There is no isolation besides that —
a time limit only stops waiting for it.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import contextlib
import threading
from collections.abc import Sequence
from pathlib import Path
from typing import Any

import numpy as np

from spaceknife_protocol.errors import ErrorCode
from spaceknife_sdk._link import Image
from spaceknife_sdk._runtime import (
    CallError,
    Dispatcher,
    describe_safely,
    load_plugin,
    state_from_init,
    unload_plugin,
)
from spaceknife_sdk.errors import HostError, ManifestError
from spaceknife_sdk.manifest import Manifest
from spaceknife_sdk.plugin import Description

from .base import Callbacks, Chunk, PluginRuntime, RuntimeCallError, StartInfo

REQUEST_TIMEOUT = 120.0


class InProcessLink:
    """The SDK's link for code running in the host: requests hop onto the backend's loop."""

    def __init__(self, runtime: InProcessRuntime) -> None:
        self.rt = runtime

    def request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        frames: Sequence[bytes] = (),
        timeout: float | None = None,
    ) -> tuple[Any, list[bytes]]:
        rt = self.rt
        if threading.get_ident() == rt.host_thread:
            raise RuntimeError("an in-process plugin asked the host from the host's own loop")
        fut = asyncio.run_coroutine_threadsafe(
            rt.callbacks.request(method, dict(params), list(frames)), rt.host_loop
        )
        try:
            got: tuple[Any, list[bytes]] = fut.result(timeout or REQUEST_TIMEOUT)
            return got
        except concurrent.futures.TimeoutError:
            fut.cancel()
            raise HostError(ErrorCode.PLUGIN_TIMEOUT, f"the host did not answer {method}") from None
        except RuntimeCallError as exc:
            raise HostError(exc.code, exc.message, exc.data) from None

    def notify(self, type: str, payload: dict[str, Any]) -> None:
        rt = self.rt
        with contextlib.suppress(RuntimeError):  # the backend's loop is closing
            rt.host_loop.call_soon_threadsafe(rt.callbacks.notify, type, dict(payload))

    def image(self, scan_id: str, columns: Sequence[str]) -> Image:
        """The store's own columns, read-only (no copy)."""
        store = self.rt.store_lookup(scan_id)
        if store is None:
            raise HostError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {scan_id}")
        cols: dict[str, np.ndarray] = {}
        for name in columns:
            if name == "cls":
                cols[name] = self.rt.cls_lookup(scan_id)
            else:
                cols[name] = store.col(name)
        return Image(
            scan_id=scan_id,
            version=int(store.version),
            epoch=int(getattr(store, "epoch", 0)),
            n=len(store),
            columns=cols,
        )

    def store_of(self, scan_id: str) -> Any:
        return self.rt.store_lookup(scan_id) if self.rt.builtin else None


class InProcessRuntime(PluginRuntime):
    kind = "inprocess"

    def __init__(
        self,
        folder: Path,
        manifest: Manifest,
        start: StartInfo,
        callbacks: Callbacks,
        *,
        store_lookup: Any,
        cls_lookup: Any,
    ) -> None:
        self.folder = folder
        self.manifest = manifest
        self.start_info = start
        self.callbacks = callbacks
        self.builtin = start.builtin
        self.store_lookup = store_lookup
        self.cls_lookup = cls_lookup
        self.host_loop = asyncio.get_running_loop()
        self.host_thread = threading.get_ident()
        self.loop: asyncio.AbstractEventLoop | None = None
        self.thread: threading.Thread | None = None
        self.dispatcher: Dispatcher | None = None
        self._calls: dict[int, concurrent.futures.Future[tuple[Any, list[bytes]]]] = {}
        self.state = "stopped"

    async def start(self) -> Description:
        self.state = "starting"
        ready: concurrent.futures.Future[Description] = concurrent.futures.Future()

        def run() -> None:
            loop = asyncio.new_event_loop()
            self.loop = loop
            asyncio.set_event_loop(loop)
            try:
                plugin = load_plugin(self.folder, self.manifest)
                state = state_from_init(self.start_info.message())
                self.dispatcher = Dispatcher(plugin, state, InProcessLink(self))
                desc, err = describe_safely(plugin)
                if err is not None or desc is None:
                    ready.set_exception(
                        RuntimeCallError(ErrorCode.PLUGIN_ERROR, str((err or {}).get("message")), err)
                    )
                    return
                description = Description.model_validate(desc)
                state.catalog = dict(description.catalog)
                ready.set_result(description)
            except ManifestError as exc:
                ready.set_exception(
                    RuntimeCallError(
                        ErrorCode.PLUGIN_ERROR, str(exc), {"issues": [i.as_dict() for i in exc.issues]}
                    )
                )
                return
            except BaseException as exc:  # importing the plugin failed
                ready.set_exception(
                    RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, f"{type(exc).__name__}: {exc}")
                )
                return
            loop.run_forever()
            loop.close()

        self.thread = threading.Thread(target=run, name=f"sk-plugin-{self.manifest.id}", daemon=True)
        self.thread.start()
        try:
            desc = await asyncio.wrap_future(ready)
        except BaseException:
            self.state = "crashed"
            self._stop_loop()
            raise
        self.state = "running"
        return desc

    async def call(
        self,
        call_id: int,
        kind: str,
        name: str,
        params: Any,
        ctx: dict[str, Any],
        frames: Sequence[bytes] = (),
        on_chunk: Chunk | None = None,
    ) -> tuple[Any, list[bytes]]:
        d, loop = self.dispatcher, self.loop
        if d is None or loop is None or self.state != "running":
            raise RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, "the plugin is not running")
        host = self.host_loop

        def emit(value: Any, fr: list[bytes]) -> None:
            if on_chunk is not None:
                host.call_soon_threadsafe(on_chunk, value, fr)

        fut = asyncio.run_coroutine_threadsafe(d.run(call_id, kind, name, params, ctx, frames, emit), loop)
        self._calls[call_id] = fut
        # shielded: cancelling the wrapper would cancel the concurrent future from this thread while
        # the plugin's loop finishes the call ("Future in unexpected state"); the dispatcher is told
        # instead, and the call's late answer is dropped
        waiter = asyncio.wrap_future(fut)
        try:
            return await asyncio.shield(waiter)
        except CallError as err:
            raise RuntimeCallError(err.code, err.message, err.data) from None
        except asyncio.CancelledError:
            waiter.add_done_callback(_retrieved)
            loop.call_soon_threadsafe(d.cancel, call_id)
            raise
        finally:
            self._calls.pop(call_id, None)

    async def cancel(self, call_id: int) -> None:
        d, loop = self.dispatcher, self.loop
        if d is not None and loop is not None:
            ev = d.cancels.get(call_id)
            if ev is not None:
                ev.set()
            loop.call_soon_threadsafe(d.cancel, call_id)

    async def update(self, fields: dict[str, Any]) -> None:
        d, loop = self.dispatcher, self.loop
        if d is not None and loop is not None:
            loop.call_soon_threadsafe(d.update, fields)

    def busy(self) -> int:
        return len(self._calls)

    async def stop(self, grace: float = 2.0) -> None:
        d, loop = self.dispatcher, self.loop
        if d is not None and loop is not None and self.state == "running":
            hooks = [h for (kind, _name), h in d.plugin.handlers.items() if kind == "hook"]
            for h in hooks:
                spec = next(
                    (s for k, s in d.plugin.declared if k == "hook" and getattr(s, "name", "") == h.name),
                    None,
                )
                if spec is not None and getattr(spec, "event", "") == "stop":
                    fut = asyncio.run_coroutine_threadsafe(
                        d.run(2**31, "hook", h.name, {"event": "stop"}, {}), loop
                    )
                    try:
                        await asyncio.wait_for(asyncio.wrap_future(fut), grace)
                    except (TimeoutError, CallError, Exception):
                        fut.cancel()
        self._stop_loop()
        self.state = "stopped"

    def _stop_loop(self) -> None:
        d, loop = self.dispatcher, self.loop
        if d is not None:
            d.close()
        if loop is not None and loop.is_running():
            loop.call_soon_threadsafe(loop.stop)
        if self.thread is not None:
            self.thread.join(2.0)
        # calls still waiting (a stream, a watcher) end here, not when their futures are collected
        # later (by then the host's loop may be closed too)
        for fut in list(self._calls.values()):
            if not fut.done():
                with contextlib.suppress(concurrent.futures.InvalidStateError):
                    fut.set_exception(RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, "the plugin stopped"))
        unload_plugin(self.manifest)
        self.dispatcher = None

    def kill(self) -> None:
        self._stop_loop()
        self.state = "stopped"


__all__ = ["InProcessLink", "InProcessRuntime"]


def _retrieved(fut: asyncio.Future[Any]) -> None:
    if not fut.cancelled():
        fut.exception()
