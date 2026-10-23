"""A plugin in its own process: the default runtime.

Started like the scan helper (helper/launch.py) — a private named pipe or a socket in a 0700 folder,
a one-time token file, the peer's pid from the OS — but never elevated, and with the plugin's limits
(limits.py). Both ends send requests and answer the other's concurrently (ids per direction): the
host calls handlers, the plugin asks for tree rows, images and services.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import subprocess
import sys
import threading
import time
from collections import deque
from collections.abc import Sequence
from typing import IO, Any

from loguru import logger
from pydantic import ValidationError

from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.plugins import PluginProcess
from spaceknife_sdk import wire
from spaceknife_sdk.plugin import Description

from ...helper.auth import new_token, peer_is_launched, tokens_match, write_token_file
from ...helper.transport import PLUGIN_PREFIX, AsyncChannel, Listener
from .. import limits
from .base import Callbacks, Chunk, PluginRuntime, RuntimeCallError, StartInfo

CONNECT_TIMEOUT = 10.0
READY_TIMEOUT = 30.0  # importing the plugin's code (numpy, pydantic …) on a cold disk
HUNG_AFTER = 30.0  # calls in flight and not a word for this long: it is stuck
CANCEL_GRACE = 5.0  # a cancelled call still running after this long: the process is stopped
WATCH_EVERY = 1.0
STDERR_LINES = 200
_ACCEPT_SLICE = 0.25


def child_command() -> list[str]:
    """Interpreter + module (isolated: no PYTHON* variables, no user site, no current folder), or the
    frozen app's own executable in plugin-host mode."""
    if getattr(sys, "frozen", False):
        return [sys.executable, "--plugin-host"]
    return [sys.executable, "-I", "-X", "utf8", "-m", "spaceknife.plugins.child"]


ENV_KEEP = (
    "PATH",
    "SYSTEMROOT",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "LANG",
    "LC_ALL",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "SPACEKNIFE_HOME",
    "SPACEKNIFE_LANGUAGE",
    "XDG_DATA_HOME",
)


def child_env() -> dict[str, str]:
    """Only what Python needs: no tokens, no SPACEKNIFE_* besides the data folder and language."""
    env = {k: v for k, v in os.environ.items() if k in ENV_KEEP}
    env.update(
        {"OPENBLAS_NUM_THREADS": "1", "OMP_NUM_THREADS": "1", "MKL_NUM_THREADS": "1", "PYTHONUTF8": "1"}
    )
    return env


class ProcessRuntime(PluginRuntime):
    kind = "process"

    def __init__(self, start: StartInfo, callbacks: Callbacks, *, processes: int = 2) -> None:
        self.start_info = start
        self.callbacks = callbacks
        self.processes = processes
        self.state = "stopped"
        self.popen: subprocess.Popen[bytes] | None = None
        self.pid: int | None = None  # the interpreter at the other end (a venv launcher's child on Windows)
        self.channel: AsyncChannel | None = None
        self.job: limits.Job | None = None
        self.stderr: deque[str] = deque(maxlen=STDERR_LINES)
        self.last_exit: int | None = None
        self.rss = 0
        self._pending: dict[int, asyncio.Future[tuple[Any, list[bytes]]]] = {}
        self._chunks: dict[int, Chunk] = {}
        self._reader: asyncio.Task[None] | None = None
        self._watch: asyncio.Task[None] | None = None
        self._requests: set[asyncio.Task[None]] = set()
        self._send_lock = asyncio.Lock()
        self._last_seen = time.monotonic()
        self._stopping = False
        self._ready: asyncio.Future[dict[str, Any]] | None = None
        self._zombies: dict[int, float] = {}  # cancelled calls not answered yet -> when to give up

    # ------------------------------------------------------------- lifecycle

    async def start(self) -> Description:
        self.state = "starting"
        self._stopping = False
        loop = asyncio.get_running_loop()
        token = new_token()
        listener = Listener(PLUGIN_PREFIX)
        token_path = write_token_file(token)
        argv = [
            *child_command(),
            "--pipe",
            listener.address,
            "--token-file",
            str(token_path),
            "--server-pid",
            str(os.getpid()),
        ]
        os.makedirs(self.start_info.storage, exist_ok=True)
        try:
            # on the loop thread (fast): Linux's PR_SET_PDEATHSIG follows the thread that started it
            self.popen = subprocess.Popen(  # noqa: ASYNC220
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                cwd=self.start_info.storage,
                env=child_env(),
                close_fds=True,
                creationflags=limits.creation_flags(),
            )
        except OSError as exc:
            listener.close()
            token_path.unlink(missing_ok=True)
            self.state = "crashed"
            raise RuntimeCallError(
                ErrorCode.PLUGIN_UNAVAILABLE, f"cannot start the plugin process: {exc}"
            ) from exc
        self.job = limits.Job(self.start_info.memory_mb, self.processes)
        self.job.assign(self.popen.pid)
        threading.Thread(
            target=self._read_stderr, args=(self.popen.stderr,), daemon=True, name="sk-plugin-stderr"
        ).start()
        try:
            deadline = loop.time() + CONNECT_TIMEOUT
            channel: AsyncChannel | None = None
            while channel is None:
                if self.popen.poll() is not None:
                    raise RuntimeCallError(
                        ErrorCode.PLUGIN_UNAVAILABLE, self._exit_text(self.popen.returncode)
                    )
                if loop.time() > deadline:
                    raise RuntimeCallError(
                        ErrorCode.PLUGIN_UNAVAILABLE, "the plugin process did not connect in time"
                    )
                with contextlib.suppress(TimeoutError):
                    channel = await listener.accept(_ACCEPT_SLICE)
            self.channel = channel
            if not peer_is_launched(channel.peer_pid, self.popen.pid):
                raise RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, "a different process connected")
            self.pid = channel.peer_pid
            if self.pid != self.popen.pid:
                self.job.assign(self.pid)  # the real interpreter behind a venv launcher
            msg, _frames = await self._read_message(CONNECT_TIMEOUT)
            hello = wire.Hello.model_validate(msg)
            if not tokens_match(hello.token, token):
                raise RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, "the plugin process sent a wrong token")
            await self._send(self.start_info.message())
            msg, _frames = await self._read_message(READY_TIMEOUT)
            ready = wire.Ready.model_validate(msg)
        except (TimeoutError, EOFError, ConnectionError, OSError, ValidationError, ValueError) as exc:
            self.kill()
            self.state = "crashed"
            raise RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, self._why(exc)) from exc
        except RuntimeCallError:
            self.kill()
            self.state = "crashed"
            raise
        finally:
            listener.close()
            token_path.unlink(missing_ok=True)
        if ready.error is not None or ready.description is None:
            self.kill()
            self.state = "crashed"
            err = ready.error or {}
            raise RuntimeCallError(ErrorCode.PLUGIN_ERROR, str(err.get("message", "no description")), err)
        try:
            description = Description.model_validate(ready.description)
        except ValidationError as exc:
            self.kill()
            self.state = "crashed"
            raise RuntimeCallError(ErrorCode.PLUGIN_ERROR, f"invalid description: {exc}") from exc
        self.state = "running"
        self._last_seen = time.monotonic()
        self._reader = asyncio.create_task(self._read_loop(), name=f"plugin-{self.start_info.plugin}-reader")
        self._watch = asyncio.create_task(self._watchdog(), name=f"plugin-{self.start_info.plugin}-watch")
        return description

    def _why(self, exc: BaseException) -> str:
        code = self.popen.poll() if self.popen is not None else None
        if code is not None:
            return self._exit_text(code)
        return f"the plugin process failed to start: {type(exc).__name__}: {exc}"

    def _exit_text(self, code: int | None) -> str:
        tail = " | ".join(list(self.stderr)[-3:])
        return f"the plugin process exited ({code})" + (f": {tail}" if tail else "")

    def _read_stderr(self, stream: IO[bytes] | None) -> None:
        if stream is None:
            return
        source = f"plugin:{self.start_info.plugin}"
        with contextlib.suppress(OSError, ValueError):
            for raw in stream:
                line = raw.decode("utf-8", "replace").rstrip()
                if line:
                    self.stderr.append(line[:2000])
                    logger.bind(source=source).debug("{}: {}", source, line[:2000])

    async def stop(self, grace: float = 2.0) -> None:
        self._stopping = True
        if self.channel is not None and not self.channel.closed and self.state == "running":
            with contextlib.suppress(Exception):
                await self._send({"type": "shutdown"})
            p = self.popen
            if p is not None:
                end = time.monotonic() + grace + 0.5
                while p.poll() is None and time.monotonic() < end:  # noqa: ASYNC110 — a process, not an event
                    await asyncio.sleep(0.05)
        self.kill()
        self.state = "stopped"

    def kill(self) -> None:
        self._stopping = True
        for task in (self._reader, self._watch):
            if task is not None and not task.done() and task is not asyncio.current_task():
                task.cancel()
        if self.channel is not None:
            self.channel.close()
        p = self.popen
        if p is not None and p.poll() is None:
            with contextlib.suppress(OSError):
                p.kill()
            with contextlib.suppress(subprocess.TimeoutExpired, OSError):
                p.wait(2.0)
        if p is not None:
            self.last_exit = p.returncode
        if self.job is not None:
            self.job.close()
            self.job = None
        self._fail_all(ErrorCode.PLUGIN_UNAVAILABLE, "the plugin process stopped")

    def _fail_all(self, code: int, message: str) -> None:
        for fut in list(self._pending.values()):
            if not fut.done():
                fut.set_exception(RuntimeCallError(code, message))
        self._pending.clear()
        self._chunks.clear()
        self._zombies.clear()

    # ------------------------------------------------------------- the channel

    async def _send(self, msg: dict[str, Any], frames: Sequence[bytes] = ()) -> None:
        ch = self.channel
        if ch is None or ch.closed:
            raise RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, "the plugin process is not connected")
        parts: list[bytes] = []
        for f in frames:
            parts += wire.frame_binary(f)
        parts += wire.frame_json(msg)
        async with self._send_lock:
            try:
                await ch.send(parts, 30.0)
            except (OSError, ConnectionError, TimeoutError) as exc:
                raise RuntimeCallError(
                    ErrorCode.PLUGIN_UNAVAILABLE, f"sending to the plugin failed: {exc}"
                ) from exc

    async def _read_message(self, timeout: float | None) -> tuple[dict[str, Any], list[bytes]]:  # noqa: ASYNC109
        ch = self.channel
        assert ch is not None
        frames: list[bytes] = []
        while True:
            kind, body = await ch.read_frame(timeout, max_binary=wire.MAX_BINARY, max_json=wire.MAX_JSON)
            if kind == wire.KIND_BINARY:
                frames.append(bytes(body))
                if len(frames) > 64:
                    raise ValueError("too many binary frames before a message")
                continue
            data = json.loads(body)
            if not isinstance(data, dict):
                raise ValueError("a message must be an object")
            return data, frames

    async def _read_loop(self) -> None:
        why = ""
        try:
            while True:
                raw, frames = await self._read_message(None)
                self._last_seen = time.monotonic()
                try:
                    msg = wire.FROM_PLUGIN.validate_python(raw)
                except ValidationError as exc:
                    why = f"protocol violation: {exc.errors(include_url=False)[0]['msg']}"
                    logger.warning("plugin {} sent an invalid message: {}", self.start_info.plugin, exc)
                    break
                self._dispatch(msg, frames)
        except (EOFError, ConnectionError, OSError, ValueError) as exc:
            why = "" if self._stopping else f"{type(exc).__name__}: {exc}"
        except asyncio.CancelledError:
            return
        if self._stopping:
            return
        code = None
        p = self.popen
        if p is not None:
            with contextlib.suppress(subprocess.TimeoutExpired):
                code = p.wait(1.0)
        self.kill()
        self.state = "crashed"
        self.callbacks.exited(code, why or self._exit_text(code))

    def _dispatch(self, msg: Any, frames: list[bytes]) -> None:
        t = msg.type
        if t in ("result", "error"):
            self._zombies.pop(msg.id, None)
        if t == "result":
            fut = self._pending.pop(msg.id, None)
            self._chunks.pop(msg.id, None)
            if fut is not None and not fut.done():
                fut.set_result((msg.value, frames[: msg.frames]))
        elif t == "error":
            fut = self._pending.pop(msg.id, None)
            self._chunks.pop(msg.id, None)
            if fut is not None and not fut.done():
                fut.set_exception(RuntimeCallError(msg.code, msg.message, msg.data))
        elif t == "chunk":
            cb = self._chunks.get(msg.id)
            if cb is not None:
                try:
                    cb(msg.value, frames[: msg.frames])
                except Exception:
                    logger.exception("plugin {}: handling an output chunk failed", self.start_info.plugin)
        elif t == "request":
            task = asyncio.create_task(self._answer(msg.id, msg.method, msg.params, frames[: msg.frames]))
            self._requests.add(task)
            task.add_done_callback(self._requests.discard)
        elif t == "heartbeat":
            self.rss = msg.rss
        elif t == "ready":
            pass
        else:  # event, state, toast, log, progress
            self.callbacks.notify(t, msg.model_dump(exclude={"type"}))

    async def _answer(self, rid: int, method: str, params: dict[str, Any], frames: list[bytes]) -> None:
        try:
            value, out = await self.callbacks.request(method, params, frames)
            await self._send(wire.reply_message(rid, value, None, len(out)), out)
        except RuntimeCallError as err:
            with contextlib.suppress(RuntimeCallError):
                await self._send(
                    wire.reply_message(
                        rid, None, {"code": err.code, "message": err.message, "data": err.data}
                    )
                )
        except Exception as exc:
            logger.exception("plugin {}: answering {} failed", self.start_info.plugin, method)
            with contextlib.suppress(RuntimeCallError):
                await self._send(
                    wire.reply_message(
                        rid, None, {"code": int(ErrorCode.INTERNAL_ERROR), "message": type(exc).__name__}
                    )
                )

    def give_up(self, why: str) -> None:
        """Stop the process now (it cannot answer any more); the host starts it again when needed."""
        if self.popen is None or self._stopping:
            return
        self.kill()
        self.state = "crashed"
        self.callbacks.exited(None, why)

    async def _watchdog(self) -> None:
        while True:
            await asyncio.sleep(WATCH_EVERY)
            p = self.popen
            if p is None or self._stopping:
                return
            now = time.monotonic()
            if any(now > until for until in self._zombies.values()):
                logger.warning("plugin {} did not stop a cancelled call; stopping it", self.start_info.plugin)
                self.kill()
                self.state = "crashed"
                self.callbacks.exited(None, "the plugin did not stop a cancelled call")
                return
            if self._pending and time.monotonic() - self._last_seen > HUNG_AFTER:
                logger.warning("plugin {} is not responding; stopping it", self.start_info.plugin)
                self.kill()
                self.state = "crashed"
                self.callbacks.exited(None, "the plugin stopped responding")
                return
            if sys.platform == "darwin" and self.rss > (self.start_info.memory_mb << 20):
                logger.warning(
                    "plugin {} uses {} bytes, past its limit; stopping it", self.start_info.plugin, self.rss
                )
                self.kill()
                self.state = "crashed"
                self.callbacks.exited(None, "the plugin used more memory than allowed")
                return

    # ------------------------------------------------------------- calls

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
        if self.state != "running":
            raise RuntimeCallError(ErrorCode.PLUGIN_UNAVAILABLE, "the plugin is not running")
        fut: asyncio.Future[tuple[Any, list[bytes]]] = asyncio.get_running_loop().create_future()
        self._pending[call_id] = fut
        if on_chunk is not None:
            self._chunks[call_id] = on_chunk
        try:
            await self._send(wire.call_message(call_id, kind, name, params, ctx, len(frames)), frames)
            self._last_seen = max(self._last_seen, time.monotonic())
            return await fut
        except asyncio.CancelledError:
            self._zombies[call_id] = time.monotonic() + CANCEL_GRACE
            with contextlib.suppress(RuntimeCallError):
                await self._send({"type": "cancel", "id": call_id})
            raise
        finally:
            self._pending.pop(call_id, None)
            self._chunks.pop(call_id, None)

    async def cancel(self, call_id: int) -> None:
        with contextlib.suppress(RuntimeCallError):
            await self._send({"type": "cancel", "id": call_id})

    async def update(self, fields: dict[str, Any]) -> None:
        if self.state == "running":
            with contextlib.suppress(RuntimeCallError):
                await self._send({"type": "update", **fields})

    def busy(self) -> int:
        return len(self._pending)

    def info(self) -> PluginProcess:
        mem = self.rss or (limits.rss_of(self.pid) if self.pid else 0)
        return PluginProcess(
            state=self.state,
            pid=self.pid,
            memory=mem,
            last_exit=self.last_exit,
            stderr=list(self.stderr)[-20:],
        )


__all__ = ["ProcessRuntime", "child_command", "child_env"]
