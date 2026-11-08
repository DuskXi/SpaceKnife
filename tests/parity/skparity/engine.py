"""Running a scenario on one backend: the process, the WebSocket client, the steps (README.md,
'Scenarios'). The Rust replayer (crates/sk-server/tests/parity_replay.rs) runs the same steps."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import websockets

from . import fixtures
from .frames import decode
from .norm import Normalizer, shape

TOKEN = "parity-token-0123456789abcdef"
SUBPROTOCOL = "spaceknife.v1"
CAPABILITIES = [
    "binary-frames.v1",
    "subscriptions",
    "cancel",
    "log-stream",
    "visuals.v1",
    "layout.flame.v1",
    "layout.polar.v1",
    "updates.v1",
]
HERE = Path(__file__).resolve().parent.parent
# a date-time inside a line of text (exports): replaced by <time>
INLINE_TIME = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?")


class StepError(Exception):
    pass


# -------------------------------------------------------------------- the update feed


FEED = json.loads((HERE / "fixtures" / "feed.json").read_text(encoding="utf-8"))


async def start_feed() -> tuple[asyncio.AbstractServer, str]:
    """A release list on 127.0.0.1 (both backends check for updates on their own)."""
    body = json.dumps(FEED["releases"]).encode("utf-8")

    async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            while True:
                line = await reader.readline()
                if not line or line in (b"\r\n", b"\n"):
                    break
            head = (
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                f"Content-Length: {len(body)}\r\nETag: \"parity-1\"\r\nConnection: close\r\n\r\n"
            )
            writer.write(head.encode("ascii") + body)
            await writer.drain()
        finally:
            writer.close()

    server = await asyncio.start_server(handle, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    return server, f"http://127.0.0.1:{port}/repos/DuskXi/SpaceKnife/releases"


# -------------------------------------------------------------------- backends


@dataclass
class BackendSpec:
    """How to start a backend: argv before `serve`, and its working folder."""

    label: str
    argv: list[str]
    cwd: str | None = None

    @staticmethod
    def reference(worktree: str) -> BackendSpec:
        wt = Path(worktree).resolve()
        py = wt / ".venv" / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
        if py.exists():
            argv = [str(py), "-X", "utf8", "-m", "spaceknife"]
        else:
            argv = ["uv", "run", "--frozen", "--project", str(wt), "python", "-X", "utf8", "-m", "spaceknife"]
        return BackendSpec("reference", argv, str(wt))

    @staticmethod
    def candidate(exe: str) -> BackendSpec:
        return BackendSpec("candidate", [str(Path(exe).resolve())], None)


class Backend:
    def __init__(self, spec: BackendSpec, home: Path, env: dict[str, str], args: list[str]) -> None:
        self.spec, self.home, self.env, self.args = spec, home, env, args
        self.proc: asyncio.subprocess.Process | None = None
        self.ready: dict[str, Any] = {}
        self.stderr = home.parent / f"{spec.label}.stderr.log"

    async def start(self) -> dict[str, Any]:
        env = {k: v for k, v in os.environ.items() if not k.startswith("SPACEKNIFE_")}
        env.update(self.env)
        env["SPACEKNIFE_HOME"] = str(self.home)
        argv = [*self.spec.argv, "serve", "--token-stdin", "--log-format", "plain", *self.args]
        err = open(self.stderr, "ab")  # noqa: SIM115 - handed to the child
        # the same working folder for both: relative paths resolve alike ($TMP)
        self.proc = await asyncio.create_subprocess_exec(
            *argv, cwd=str(self.home.parent), env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err
        )
        err.close()
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write((TOKEN + "\n").encode("ascii"))
        await self.proc.stdin.drain()
        line = await asyncio.wait_for(self.proc.stdout.readline(), 120)
        if not line:
            raise StepError(f"{self.spec.label}: no ready line (see {self.stderr})")
        self.ready = json.loads(line)
        return self.ready

    async def stop(self) -> int | None:
        if self.proc is None:
            return None
        with contextlib.suppress(Exception):
            assert self.proc.stdin
            self.proc.stdin.close()
        try:
            code = await asyncio.wait_for(self.proc.wait(), 30)
        except TimeoutError:
            self.proc.kill()
            code = await self.proc.wait()
        self.proc = None
        return code

    async def exited(self, secs: float) -> int | None:
        if self.proc is None:
            return None
        try:
            return await asyncio.wait_for(self.proc.wait(), secs)
        except TimeoutError:
            return None


# -------------------------------------------------------------------- the client


class Client:
    """One session: requests by id, events by name, binary frames by request id."""

    def __init__(self, ws: Any, hello: dict[str, Any]) -> None:
        self.ws = ws
        self.hello = hello
        self.next = 1
        self.pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self.events: dict[str, deque[dict[str, Any]]] = defaultdict(deque)
        self.frames: dict[Any, list[bytes]] = defaultdict(list)
        self.arrived = asyncio.Event()
        self.reader = asyncio.create_task(self._read())

    @staticmethod
    async def open(url: str, caps: list[str] | None = None) -> Client:
        ws = await websockets.connect(url, subprotocols=[SUBPROTOCOL], max_size=None, compression=None)
        await ws.send(json.dumps(hello_frame(TOKEN, "1.1", caps or CAPABILITIES)))
        first = json.loads(await asyncio.wait_for(ws.recv(), 30))
        if "error" in first:
            raise StepError(f"hello failed: {first['error']}")
        return Client(ws, first["result"])

    async def _read(self) -> None:
        try:
            async for msg in self.ws:
                if isinstance(msg, bytes):
                    n = int.from_bytes(msg[:4], "little")
                    header = json.loads(msg[4 : 4 + n].decode("utf-8"))
                    self.frames[header.get("request_id")].append(msg)
                else:
                    v = json.loads(msg)
                    if "method" in v:
                        self.events[v["method"]].append(v.get("params") or {})
                    elif v.get("id") in self.pending:
                        fut = self.pending.pop(v["id"])
                        if not fut.done():
                            fut.set_result(v)
                self.arrived.set()
        except websockets.ConnectionClosed:
            pass
        finally:
            for fut in self.pending.values():
                if not fut.done():
                    fut.set_exception(StepError("connection closed"))
            self.arrived.set()

    async def call(self, method: str, params: Any, timeout: float = 120) -> tuple[int, dict[str, Any]]:
        rid = self.next
        self.next += 1
        fut: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self.pending[rid] = fut
        await self.ws.send(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}))
        return rid, await asyncio.wait_for(fut, timeout)

    async def wait(self, cond: Any, timeout: float) -> bool:
        end = time.monotonic() + timeout
        while not cond():
            left = end - time.monotonic()
            if left <= 0:
                return False
            self.arrived.clear()
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self.arrived.wait(), left)
        return True

    async def close(self) -> None:
        with contextlib.suppress(Exception):
            await self.ws.close()
        self.reader.cancel()
        with contextlib.suppress(BaseException):
            await self.reader


def hello_frame(token: str, protocol: str, caps: list[str]) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "id": 0,
        "method": "session.hello",
        "params": {
            "token": token,
            "protocol": protocol,
            "client": {"name": "parity", "version": "0", "kind": "test"},
            "capabilities": caps,
        },
    }


# -------------------------------------------------------------------- one run


def make_writable(func: Any, path: str, _exc: Any) -> None:
    os.chmod(path, stat.S_IWRITE | stat.S_IREAD | stat.S_IEXEC)
    func(path)


@dataclass
class Places:
    """The folders of one run: $TMP holds $HOME (the backend's data) and $ROOT (fixtures)."""

    tmp: Path
    home: Path
    root: Path

    @staticmethod
    def make(label: str) -> Places:
        tmp = Path(tempfile.mkdtemp(prefix=f"sk-parity-{label}-"))
        if sys.platform == "win32":
            tmp = Path(os.path.realpath(tmp))  # the long name, as the backends report it
        home, root = tmp / "home", tmp / "root"
        home.mkdir()
        root.mkdir()
        return Places(tmp, home, root)

    def mapping(self) -> dict[str, str]:
        return {"$ROOT": str(self.root), "$HOME": str(self.home), "$TMP": str(self.tmp)}

    def path(self, s: str) -> str:
        for name, real in self.mapping().items():
            if s == name or s.startswith(name + "/"):
                rest = s[len(name) + 1 :]
                return str(Path(real, *rest.split("/"))) if rest else real
        return s

    def remove(self) -> None:
        shutil.rmtree(self.tmp, onerror=make_writable)  # noqa: PTH - readonly fixture files


@dataclass
class Run:
    """A scenario on one backend; `transcript` is what it recorded (normalized)."""

    spec: BackendSpec
    scenario: dict[str, Any]
    places: Places
    rules: dict[str, Any]
    feed: str
    norm: Normalizer = field(init=False)
    saved: dict[str, Any] = field(default_factory=dict)
    transcript: list[dict[str, Any]] = field(default_factory=list)
    backend: Backend | None = None
    clients: dict[str, Client] = field(default_factory=dict)
    view_scan: dict[str, str] = field(default_factory=dict)

    def __post_init__(self) -> None:
        self.norm = Normalizer(self.rules, self.places.mapping())

    # ------------------------------------------------------------ backend lifetime

    def env(self) -> dict[str, str]:
        env = {
            "SPACEKNIFE_LANGUAGE": "en",
            "SPACEKNIFE_UPDATE_FEED": self.feed,
            "SPACEKNIFE_APP_VERSION": "0.3.4",
            "SPACEKNIFE_INSTALL_KIND": "portable",
        }
        env.update(self.scenario.get("env", {}))
        return env

    async def boot(self) -> None:
        self.backend = Backend(self.spec, self.places.home, self.env(), list(self.scenario.get("args", [])))
        await self.backend.start()
        self.clients["main"] = await Client.open(self.backend.ready["url"])

    async def shutdown(self) -> None:
        for c in self.clients.values():
            await c.close()
        self.clients.clear()
        if self.backend is not None:
            await self.backend.stop()
            self.backend = None

    # ------------------------------------------------------------ values

    def resolve(self, v: Any) -> Any:
        """`$name.a.0.b` → a saved raw result; `$ROOT/…`, `$HOME/…`, `$TMP/…` → real paths."""
        if isinstance(v, dict):
            return {k: self.resolve(x) for k, x in v.items()}
        if isinstance(v, list):
            return [self.resolve(x) for x in v]
        if isinstance(v, str) and v.startswith("$"):
            head = v.split("/", 1)[0]
            if head in ("$ROOT", "$HOME", "$TMP"):
                return self.places.path(v)
            parts = v[1:].split(".")
            if parts[0] in self.saved:
                cur: Any = self.saved[parts[0]]
                try:
                    for p in parts[1:]:
                        cur = cur[int(p)] if isinstance(cur, list) else cur[p]
                except (KeyError, IndexError, TypeError, ValueError) as e:
                    raise StepError(f"{v}: not in the saved result") from e
                return cur
        return v

    async def resolve_nodes(self, client: Client, value: Any, scan: str) -> None:
        await self.resolve_ids(client, self.norm.node_ids(value), scan)

    async def resolve_ids(self, client: Client, ids: set[int], scan: str) -> None:
        """Node id → normalized path through `tree.node`, once per (scan, id)."""
        if not scan:
            return
        for nid in sorted(ids):
            if (scan, nid) in self.norm.nodes:
                continue
            _, r = await client.call("tree.node", {"scan_id": scan, "node": nid})
            path = r.get("result", {}).get("path") if "result" in r else None
            if path is not None:
                self.norm.nodes[(scan, nid)] = self.norm.path_text(path)

    def scan_of(self, params: Any, result: Any) -> str:
        if isinstance(params, dict):
            if isinstance(params.get("scan_id"), str):
                return str(params["scan_id"])
            if isinstance(params.get("view_id"), str) and params["view_id"] in self.view_scan:
                return self.view_scan[params["view_id"]]
        if isinstance(result, dict):
            for k in ("scan_id",):
                if isinstance(result.get(k), str):
                    return str(result[k])
            st = result.get("state")
            if isinstance(st, dict) and isinstance(st.get("scan_id"), str):
                return str(st["scan_id"])
        return self.saved.get("$last_scan", "")

    def note_ids(self, params: Any, result: Any) -> None:
        if isinstance(result, dict):
            if isinstance(result.get("scan_id"), str):
                self.saved["$last_scan"] = result["scan_id"]
            st = result.get("state")
            if isinstance(st, dict) and isinstance(st.get("view_id"), str) and isinstance(st.get("scan_id"), str):
                self.view_scan[st["view_id"]] = st["scan_id"]
        if isinstance(params, dict) and isinstance(params.get("scan_id"), str):
            self.saved["$last_scan"] = params["scan_id"]

    def pick(self, step: dict[str, Any], v: Any) -> Any:
        if isinstance(v, dict) and "pick" in step:
            v = {k: v[k] for k in step["pick"] if k in v}
        if isinstance(v, dict) and "omit" in step:
            v = {k: x for k, x in v.items() if k not in step["omit"]}
        return v

    # ------------------------------------------------------------ steps

    async def step(self, step: dict[str, Any]) -> dict[str, Any] | None:
        client = self.clients.get(step.get("on", "main"))
        if "call" in step:
            assert client is not None
            method = step["call"]
            params = self.resolve(step.get("params", {}))
            rid, r = await client.call(method, params, float(step.get("timeout", 120)))
            rec: dict[str, Any] = {"call": method}
            if "result" in r:
                result = r["result"]
                if "as" in step:
                    self.saved[step["as"]] = result
                self.note_ids(params, result)
                scan = self.scan_of(params, result)
                self.norm.scan = scan
                frames: list[bytes] = []
                if step.get("binary"):
                    # a frame follows when the picture changed; otherwise give a stray one a moment
                    wait = 15 if isinstance(result, dict) and result.get("changed") else 0.5
                    ok = await client.wait(lambda: bool(client.frames.get(rid)), wait)
                    frames = client.frames.pop(rid, []) if ok else []
                decoded = [decode(f) for f in frames]
                await self.resolve_nodes(client, result, scan)
                for _, cols, _ in decoded:
                    ids = {int(k) & ((1 << 30) - 1) for k in cols.get("key", ("", []))[1]}
                    ids |= {int(n) for n in cols.get("node", ("", []))[1] if int(n) >= 0}
                    await self.resolve_ids(client, ids, scan)
                self.norm.scan = scan
                shown = self.pick(step, result)
                rec["result"] = shape(shown) if step.get("shape") else self.norm.norm(shown)
                if step.get("binary"):
                    rec["frames"] = [self.norm.frame(h, c, b) for h, c, b in decoded]
            else:
                rec["error"] = self.norm.error(r["error"])
            return None if step.get("record") is False else rec
        if "wait_scan" in step:
            assert client is not None
            scan_id = self.resolve(step["wait_scan"])
            _, r = await client.call("$/subscribe", {"topic": "scan", "params": {"scan_id": scan_id}})
            sub = r["result"]["sub"]
            want_snapshot = bool(step.get("snapshot"))
            # a scan that ran before (rescan, continue): its old final status does not count
            before = self.resolve(step["newer_than"]) if "newer_than" in step else None
            seen: list[dict[str, Any]] = []

            def final() -> bool:
                q = client.events["scan.progress"]
                while q:
                    ev = q.popleft()
                    if ev.get("sub") == sub:
                        seen.append(ev["status"])
                return (
                    bool(seen)
                    and seen[-1]["state"] not in ("scanning", "queued")
                    and (not want_snapshot or seen[-1].get("snapshot") is not None)
                    and (before is None or seen[-1].get("finished_at") != before)
                )

            if not await client.wait(final, float(step.get("timeout", 120))):
                raise StepError("the scan did not finish")  # no ids: the replayer says the same
            await client.call("$/unsubscribe", {"sub": sub})
            status = seen[-1]
            if "as" in step:
                self.saved[step["as"]] = status
            items = [s.get("items", 0) for s in seen]
            self.norm.scan = scan_id
            self.saved["$last_scan"] = scan_id
            await self.resolve_nodes(client, status, scan_id)
            return {"wait_scan": self.norm.norm(self.pick(step, status)), "monotonic": items == sorted(items)}
        if "until" in step:
            # poll a call until a field of its result is (not) null or equals a value; records
            # whether that happened in time (the monitor and rescan scenarios)
            assert client is not None
            u = step["until"]
            end = time.monotonic() + float(step.get("timeout", 30))
            met = False
            while time.monotonic() < end and not met:
                _, r = await client.call(u["call"], self.resolve(u.get("params", {})))
                cur: Any = r.get("result")
                for part in u.get("field", "").split(".") if u.get("field") else []:
                    cur = cur.get(part) if isinstance(cur, dict) else None
                if "equals" in u:
                    met = cur == u["equals"]
                elif u.get("null", False):
                    met = cur is None
                else:
                    met = cur is not None
                if not met:
                    await asyncio.sleep(0.1)
            return {"until": u["call"], "met": met}
        if "subscribe" in step:
            assert client is not None
            params = {"topic": step["subscribe"], "params": self.resolve(step.get("params", {}))}
            if "rate_hz" in step:
                params["rate_hz"] = step["rate_hz"]
            _, r = await client.call("$/subscribe", params)
            if "result" in r and "as" in step:
                self.saved[step["as"]] = r["result"]
            rec = {"subscribe": step["subscribe"]}
            rec.update({"result": self.norm.norm(r["result"])} if "result" in r else {"error": self.norm.error(r["error"])})
            return rec
        if "events" in step:
            assert client is not None
            name, count = step["events"], int(step.get("count", 1))
            q = client.events[name]
            mode = step.get("mode", "all")
            until = step.get("stop_at")
            if until is not None:
                # up to the first event whose `field` is one of `in` (how many come before it
                # depends on timing: record it with mode "last")
                def hit(e: dict[str, Any]) -> bool:
                    return e.get(until["field"]) in until["in"]

                ok = await client.wait(lambda: any(hit(e) for e in q), float(step.get("timeout", 20)))
                got = []
                while q:
                    e = q.popleft()
                    got.append(e)
                    if hit(e):
                        break
            else:
                ok = await client.wait(lambda: len(q) >= count, float(step.get("timeout", 20)))
                if mode == "latest":  # everything queued by now; the newest one is recorded
                    got = list(q)[-1:]
                    q.clear()
                    mode = "all"
                else:
                    got = [q.popleft() for _ in range(min(count, len(q)))]
            if mode == "shape":
                body: Any = [shape(e) for e in got]
            elif mode == "count":
                body = len(got)
            else:
                body = [self.norm.norm(self.pick(step, e)) for e in got]
                if mode == "last":
                    body = body[-1:]
            return {"events": name, "got": body, **({} if ok else {"timeout": True})}
        if "drain" in step:
            assert client is not None
            if step.get("settle"):
                await asyncio.sleep(float(step["settle"]))
            client.events[step["drain"]].clear()
            return None
        if "handshake" in step:
            return {"handshake": await self.handshake(step["handshake"])}
        if "connect" in step:
            assert self.backend is not None
            self.clients[step["connect"]] = await Client.open(self.backend.ready["url"], step.get("capabilities"))
            return None
        if "fs" in step:
            self.fs(step["fs"])
            return None
        if "read" in step:
            # a file a backend wrote (exports): its lines, paths and date-times normalized
            p = Path(self.places.path(step["read"]))
            if not p.exists():
                return {"read": None}
            text = p.read_bytes().decode("utf-8", "replace").replace("\r\n", "\n")
            lines = [INLINE_TIME.sub("<time>", self.norm.path_text(line)) for line in text.split("\n")]
            return {"read": lines}
        if "sleep" in step:
            await asyncio.sleep(float(step["sleep"]))
            return None
        if "restart" in step:
            await self.shutdown()
            await self.boot()
            return None
        if "exit" in step:
            assert self.backend is not None
            code = await self.backend.exited(float(step.get("timeout", 20)))
            for c in self.clients.values():
                await c.close()
            self.clients.clear()
            self.backend.proc = None if code is not None else self.backend.proc
            return {"exit": code is not None}
        if "ready" in step:
            assert self.backend is not None
            r = dict(self.backend.ready)
            r.pop("url", None)
            r.pop("port", None)
            return {"ready": self.norm.norm(r)}
        if "hello" in step:
            assert client is not None
            return {"hello": self.norm.norm(client.hello)}
        if "note" in step:
            return None
        raise StepError(f"unknown step {step}")

    def fs(self, op: dict[str, Any]) -> None:
        p = Path(self.places.path(op["path"])) if "path" in op else None
        kind = op["op"]
        if kind == "write":
            assert p
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(b"x" * int(op.get("size", 0)))
            t = float(op.get("mtime", 1600000000))
            fixtures.set_times(p, t, t, t)
        elif kind == "touch":
            assert p
            t = float(op.get("mtime", 1600000000))
            fixtures.set_times(p, t, t, t)
        elif kind == "mkdir":
            assert p
            p.mkdir(parents=True, exist_ok=True)
        elif kind == "remove":
            assert p
            if p.is_dir():
                shutil.rmtree(p, onerror=make_writable)  # noqa: PTH
            else:
                p.unlink()
        elif kind == "rename":
            assert p
            p.rename(self.places.path(op["to"]))
        elif kind == "fixture":
            assert p
            fixtures.build(p, op["tree"])
        else:
            raise StepError(f"unknown fs op {kind}")

    async def handshake(self, h: dict[str, Any]) -> dict[str, Any]:
        """A raw connection: what the server answers and how it closes."""
        assert self.backend is not None
        url = self.backend.ready["url"]
        if "path" in h:
            url = url.rsplit("/", 1)[0] + h["path"]
        out: dict[str, Any] = {}
        try:
            kw: dict[str, Any] = {"max_size": None, "compression": None, "open_timeout": 10}
            if h.get("subprotocol", True):
                kw["subprotocols"] = [SUBPROTOCOL]
            if "origin" in h:
                kw["origin"] = h["origin"]
            ws = await websockets.connect(url, **kw)
        except websockets.InvalidStatus as e:
            return {"http_status": e.response.status_code}
        except Exception as e:  # noqa: BLE001 - recorded
            return {"refused": type(e).__name__}
        try:
            send = h.get("send", "hello")
            if send == "hello":
                frame = hello_frame(h.get("token", TOKEN), h.get("protocol", "1.1"), h.get("capabilities", CAPABILITIES))
                if "params" in h:
                    frame["params"] = h["params"]
                await ws.send(json.dumps(frame))
            elif send == "binary":
                await ws.send(b"\x00\x01\x02\x03")
            elif send == "request":
                await ws.send(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "system.ping", "params": {"payload": "x"}}))
            elif send == "garbage":
                await ws.send("{not json")
            # send == "nothing": wait for the hello timeout
            try:
                while True:
                    msg = await asyncio.wait_for(ws.recv(), float(h.get("timeout", 15)))
                    if isinstance(msg, str):
                        v = json.loads(msg)
                        if "error" in v:
                            out["error"] = self.norm.error(v["error"])
                        elif "result" in v:
                            res = v["result"]
                            out["result"] = {"protocol": res.get("protocol"), "capabilities": res.get("capabilities")}
                            break
            except websockets.ConnectionClosed as e:
                out["close_code"] = e.rcvd.code if e.rcvd else None
            except TimeoutError:
                out["timeout"] = True
        finally:
            await ws.close()
        return out


async def run_scenario(
    spec: BackendSpec,
    scenario: dict[str, Any],
    rules: dict[str, Any],
    feed: str,
    places: Places | None = None,
    home_from: Path | None = None,
    keep: bool = False,
    replay: bool = False,
) -> tuple[list[dict[str, Any]], Places]:
    """Run `scenario` on one backend; (normalized transcript, places). `home_from`: start from a
    copy of that data folder (the upgrade scenario)."""
    places = places or Places.make(spec.label)
    for name, tree in scenario.get("fixtures", {}).items():
        target = places.root / name
        if not target.exists():
            fixtures.build(target, tree)
    if home_from is not None:
        shutil.rmtree(places.home)
        shutil.copytree(home_from, places.home)
    run = Run(spec, scenario, places, rules, feed)
    try:
        await run.boot()
        for i, step in enumerate(scenario["steps"]):
            if replay and step.get("live_only"):
                continue  # needs what only a live 0.3.4 writes (its format-2 snapshots)
            try:
                rec = await run.step(step)
            except StepError as e:
                rec = {"step_error": str(e)}
            except TimeoutError:
                rec = {"step_error": "TimeoutError"}
            except websockets.ConnectionClosed:
                rec = {"step_error": "connection closed"}
            if rec is not None:
                rec = {"i": i, **rec}
                run.transcript.append(rec)
    finally:
        await run.shutdown()
        if not keep:
            with contextlib.suppress(OSError):
                places.remove()
    return run.transcript, places
