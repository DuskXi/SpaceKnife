"""The plugin system's acceptance harness: a real backend (``python -m spaceknife serve`` in a child
process with its own temporary SPACEKNIFE_HOME and a free port), plugins installed, described and
enabled over the protocol as the app does it (each in its own plugin process), and driven like the
frontend drives them. Every wait is for a condition (``until``), never a fixed sleep.

docs/plugins.md 「验收测试」 says what these tests hold and that changes to the plugin system keep
them green."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import sys
import time
from collections import defaultdict
from collections.abc import Awaitable, Callable, Iterable
from pathlib import Path
from typing import Any

from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol.session import ClientInfo

FIXTURES = Path(__file__).parent / "fixtures"
CLIENT = ClientInfo(name="acceptance", version="0.0.0", kind="test")
START_S = 90.0  # the backend's ready line (a loaded runner, a cold import)
WAIT_S = 45.0  # the longest any condition may take (they take well under a second when all is well)
EVENTS = ("plugin.changed", "plugin.task", "extensions.changed", "settings.changed", "plugin.toast")


def node_of(found: Any) -> int | None:
    """The node id in a ``tree.find`` answer."""
    if not isinstance(found, dict):
        return None
    n = found.get("node")
    if isinstance(n, dict):
        n = n.get("id", n.get("node"))
    return int(n) if isinstance(n, int) else None


class Backend:
    """One backend process and one connection to it."""

    def __init__(self, home: Path) -> None:
        self.home = home
        self.log_path = home / "backend.log"
        self.proc: asyncio.subprocess.Process | None = None
        self._conn: Connection | None = None
        self._log: Any = None
        self._drain: asyncio.Task[None] | None = None
        self.events: dict[str, list[dict[str, Any]]] = defaultdict(list)

    # ------------------------------------------------------------------ the process

    async def start(self) -> Backend:
        self.home.mkdir(parents=True, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if k != "SPACEKNIFE_LANGUAGE"}  # it would pin the language
        env.update(
            SPACEKNIFE_HOME=str(self.home),
            SPACEKNIFE_SYSTEM_LOCALE="zh-CN",  # general.language "system": Chinese until a test switches
            PYTHONDONTWRITEBYTECODE="1",
        )
        self._log = await asyncio.to_thread(self.log_path.open, "wb")  # closed in stop()
        self.proc = await asyncio.create_subprocess_exec(
            sys.executable,
            "-X",
            "utf8",
            "-m",
            "spaceknife",
            "serve",
            "--port",
            "0",
            "--log-format",
            "plain",
            "--log-level",
            "DEBUG",
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=self._log,
            env=env,
        )
        assert self.proc.stdout is not None
        try:
            line = await asyncio.wait_for(self.proc.stdout.readline(), START_S)
        except TimeoutError:
            raise AssertionError(
                f"the backend printed no ready line in {START_S:g} s\n{self.tail()}"
            ) from None
        if not line:
            await self.proc.wait()
            raise AssertionError(
                f"the backend exited ({self.proc.returncode}) before it was ready\n{self.tail()}"
            )
        ready = json.loads(line)
        self._drain = asyncio.create_task(self._read_rest(self.proc.stdout))
        self._conn = await Connection.open(ready["url"], ready["token"], CLIENT)
        for name in EVENTS:
            self._conn.on_raw(name, lambda p, n=name: self.events[n].append(p))
        return self

    @staticmethod
    async def _read_rest(stream: asyncio.StreamReader) -> None:
        while await stream.read(65536):
            pass

    async def stop(self) -> None:
        """Ask the backend to shut down (it stops its plugin processes), then wait for it; kill
        only this process, by its handle, when it does not end."""
        if self._conn is not None:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(self._conn.call_raw("app.shutdown", {}), 10)
            with contextlib.suppress(Exception):
                await asyncio.wait_for(self._conn.close(), 5)
        if self.proc is not None and self.proc.returncode is None:
            try:
                await asyncio.wait_for(self.proc.wait(), 30)
            except TimeoutError:
                self.proc.kill()
                await self.proc.wait()
        if self._drain is not None:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(self._drain, 5)
        if self._log is not None:
            self._log.close()

    def tail(self, lines: int = 60) -> str:
        """The end of the backend's log (for a failure's message)."""
        try:
            text = self.log_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return "(no backend log)"
        return "--- backend log (end) ---\n" + "\n".join(text.splitlines()[-lines:])

    def log(self) -> str:
        try:
            return self.log_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    # ------------------------------------------------------------------ calls

    @property
    def conn(self) -> Connection:
        assert self._conn is not None, "start() first"
        return self._conn

    async def call(self, method: str, params: dict[str, Any] | None = None, timeout: float = WAIT_S) -> Any:  # noqa: ASYNC109
        try:
            return await asyncio.wait_for(self.conn.call_raw(method, params or {}), timeout)
        except TimeoutError:
            raise AssertionError(f"{method} did not answer in {timeout:g} s\n{self.tail()}") from None

    async def error(self, method: str, params: dict[str, Any] | None = None) -> RemoteError:
        """The error a call ends with (it must end with one)."""
        try:
            value = await self.call(method, params)
        except RemoteError as exc:
            return exc
        raise AssertionError(f"{method} answered {value!r}, an error was expected")

    async def until[V](
        self,
        what: str,
        probe: Callable[[], Awaitable[V]],
        ok: Callable[[V], object] = bool,
        limit: float = WAIT_S,
        every: float = 0.05,
    ) -> V:
        """Ask ``probe`` until ``ok`` holds for its answer; the answer. A RemoteError counts as
        not yet. Fails after ``limit`` seconds with the last answer and the backend's log."""
        end = time.monotonic() + limit
        last: Any = None
        while True:
            try:
                last = await probe()
                if ok(last):
                    return last
            except RemoteError as exc:
                last = exc
            if time.monotonic() > end:
                raise AssertionError(f"timed out waiting for {what}; last: {last!r}\n{self.tail()}")
            await asyncio.sleep(every)

    # ------------------------------------------------------------------ plugins

    async def install(self, folder: Path) -> str:
        """Install from a folder, describe, enable with every permission it asks for: the app's
        flow (plugin.install → plugin.describe → plugin.enable)."""
        res = await self.call("plugin.install", {"path": str(folder), "replace": True})
        pid = str(res["id"])
        desc = await self.call("plugin.describe", {"id": pid})
        grant = [p["id"] for p in desc["plugin"]["permissions"]]
        info = await self.call("plugin.enable", {"id": pid, "hash": desc["plugin"]["hash"], "grant": grant})
        assert info["state"] == "enabled", info
        return pid

    async def plugin(self, pid: str) -> dict[str, Any]:
        """The plugin as plugin.list has it (state, process: pid, restarts …)."""
        listed = await self.call("plugin.list", {})
        return next(p for p in listed["plugins"] if p["id"] == pid)

    async def pcall(self, pid: str, name: str, timeout: float = WAIT_S, **params: Any) -> Any:  # noqa: ASYNC109
        return await self.call(f"plugin.{pid}.{name}", params, timeout)

    async def dump(self, pid: str) -> dict[str, Any]:
        return dict((await self.pcall(pid, "dump"))["seen"])

    async def calls(self, pid: str, kind: str) -> list[dict[str, Any]]:
        """The probe's records of one kind of call."""
        return [c for c in (await self.dump(pid))["calls"] if c["kind"] == kind]

    async def tasks(self, pid: str) -> list[dict[str, Any]]:
        return [t for t in (await self.call("plugin.tasks", {}))["tasks"] if t["plugin"] == pid]

    async def settings(self, values: dict[str, Any]) -> Any:
        return await self.call("settings.update", {"values": values})

    async def extensions(self) -> dict[str, Any]:
        return dict(await self.call("extensions.get", {}))

    # ------------------------------------------------------------------ scans and views

    async def scan(self, root: Path) -> str:
        """A hard scan of ``root``, finished."""
        st = await self.call("scan.start", {"roots": [str(root)], "auto_snapshot": False})
        sid = str(st["scan_id"])
        await self.until(
            f"the scan of {root} to finish",
            lambda: self.call("scan.status", {"scan_id": sid}),
            lambda s: s["state"] in ("done", "failed", "cancelled"),
        )
        st = await self.call("scan.status", {"scan_id": sid})
        assert st["state"] == "done", st
        return sid

    async def watching(self, sid: str) -> dict[str, Any]:
        """The scan's monitor runs and watches every folder (Linux places a watch per folder after
        the scan listed it; a recursive watcher says -1)."""
        dirs = int((await self.call("scan.status", {"scan_id": sid})).get("dirs") or 0)

        def ready(st: dict[str, Any]) -> bool:
            m = st.get("monitor") or {}
            w = m.get("watched_dirs")
            return m.get("state") == "running" and w is not None and (w < 0 or w >= dirs)

        st = await self.until(
            f"the monitor of {sid} to watch its {dirs} folders",
            lambda: self.call("scan.status", {"scan_id": sid}),
            ready,
        )
        return dict(st["monitor"])

    async def find(self, sid: str, path: Path | str) -> int | None:
        try:
            return node_of(await self.call("tree.find", {"scan_id": sid, "path": str(path)}))
        except RemoteError:
            return None

    async def node(self, sid: str, path: Path | str) -> dict[str, Any]:
        """A node of the tree as tree.find gives it (``alloc`` … of a folder: its whole subtree)."""
        found = await self.call("tree.find", {"scan_id": sid, "path": str(path)})
        assert isinstance(found.get("node"), dict), f"{path} is not in {sid}"
        return dict(found["node"])

    async def view(self, sid: str, **settings: Any) -> str:
        res = await self.call(
            "view.open",
            {
                "scan_id": sid,
                "viewport": {"width": 1280, "height": 800, "dpr": 1},
                "settings": settings or None,
            },
        )
        state = res["state"]
        return str(state.get("view_id") or state["id"])

    async def check_many(
        self, ids: Iterable[str], sid: str, vid: str | None, node: int | None
    ) -> dict[str, tuple[bool, str]]:
        """palette.check_many as a context menu asks it: each command's (enabled, status)."""
        res = await self.call(
            "palette.check_many",
            {
                "ids": list(ids),
                "context": {"tab": "scan", "scan_id": sid, "view_id": vid, "basket": 0, "node": node},
            },
        )
        return {c["id"]: (bool(c.get("enabled")), str(c.get("status") or "")) for c in res["commands"]}

    async def fields(self, pid: str, sid: str, node: int, slot: str = "inspector") -> list[dict[str, Any]]:
        res = await self.call("plugin.fields", {"scan_id": sid, "node": node, "slot": slot})
        return [f for f in res.get("fields", []) if f.get("plugin") == pid]


def by_path(changes: list[dict[str, Any]]) -> dict[str, list[tuple[int | None, int]]]:
    """Each folder's (before, after) over the tree_changed calls, in order."""
    out: dict[str, list[tuple[int | None, int]]] = {}
    for call in changes:
        for p, b, a in zip(call["paths"], call["before"], call["after"], strict=True):
            out.setdefault(os.path.normcase(os.path.normpath(p)), []).append((b, a))
    return out


def key(path: Path | str) -> str:
    return os.path.normcase(os.path.normpath(str(path)))


def follows_on(steps: list[tuple[int | None, int]], first: int, last: int) -> bool:
    """From ``first`` to ``last``, each call's before the previous one's after."""
    if not steps or steps[0][0] != first or steps[-1][1] != last:
        return False
    return all(steps[i][0] == steps[i - 1][1] for i in range(1, len(steps)))
