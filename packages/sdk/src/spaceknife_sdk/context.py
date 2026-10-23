"""``ctx``: what a plugin's code gets with every call — the tree, its settings, the host's services.

Host-backed members are plain (blocking) calls, valid from async and sync handlers alike: the host
answers from its own thread, so waiting here never stalls it. Each one checks the plugin's
permissions before asking (the host checks again).
"""

from __future__ import annotations

import asyncio
import contextvars
import fnmatch
import hashlib
import os
import threading
import time
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, TYPE_CHECKING, Any, Literal, TypeVar
from urllib.parse import urlsplit

from pydantic import BaseModel

from spaceknife_protocol.plugins import NodeRef, Permission
from spaceknife_protocol.text import LANG, Text, localize

from .errors import Cancelled, HostError, PluginError
from .tree import Node, TreeView

if TYPE_CHECKING:
    from ._link import Link

M = TypeVar("M", bound=BaseModel)
R = TypeVar("R")
Tone = Literal["info", "ok", "warn", "danger"]
NodeLike = int | NodeRef | Node | str


@dataclass(slots=True)
class PluginSelf:
    """``ctx.plugin``: who the code is."""

    id: str
    version: str
    dir: Path
    storage: Path
    builtin: bool = False
    dev: bool = False


@dataclass(slots=True)
class RuntimeState:
    """What a running plugin knows about itself; the host updates it (language, settings, grants)."""

    plugin: PluginSelf
    granted: frozenset[str] = frozenset()
    settings: dict[str, Any] = field(default_factory=dict)
    lang: str = "zh-CN"
    roots: list[str] = field(default_factory=list)  # paths of the open scans' roots (files.read)
    picked: list[str] = field(default_factory=list)  # what the user chose for a scan source (files.read)
    on_paths: Any = None  # (readable paths) -> None: keeps the audit hook in step (a plugin process)
    catalog: dict[str, str] = field(default_factory=dict)  # the plugin's zh -> en texts
    net_hosts: list[str] = field(default_factory=list)
    exec: list[str] = field(default_factory=list)
    states: dict[str, dict[str, Any]] = field(default_factory=dict)
    state_models: dict[str, type[BaseModel]] = field(default_factory=dict)
    event_names: dict[type, str] = field(default_factory=dict)


@dataclass(slots=True)
class CallInfo:
    """One call, as the host describes it."""

    id: int = 0
    kind: str = "method"
    name: str = ""
    session: str | None = None
    scan_id: str | None = None
    view_id: str | None = None
    node: int | None = None
    timeout: float = 30.0
    request: int | str | None = None  # the frontend's request id (progress events)

    @staticmethod
    def from_dict(d: Mapping[str, Any]) -> CallInfo:
        return CallInfo(
            id=int(d.get("id", 0)),
            kind=str(d.get("kind", "method")),
            name=str(d.get("name", "")),
            session=d.get("session"),
            scan_id=d.get("scan_id"),
            view_id=d.get("view_id"),
            node=d.get("node"),
            timeout=float(d.get("timeout", 30.0)),
            request=d.get("request"),
        )


def require(state: RuntimeState, permission: Permission) -> None:
    if permission not in state.granted:
        raise PermissionError(
            f"this plugin was not granted {permission!r} (add it to permissions in plugin.toml)"
        )


class Settings:
    """``ctx.settings``: the plugin's own settings by local name."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def __getitem__(self, name: str) -> Any:
        return self._ctx._state.settings[name]

    def get(self, name: str, default: Any = None) -> Any:
        return self._ctx._state.settings.get(name, default)

    def __contains__(self, name: object) -> bool:
        return name in self._ctx._state.settings

    def items(self) -> Iterable[tuple[str, Any]]:
        return self._ctx._state.settings.items()

    def set(self, name: str, value: Any) -> None:
        """Change one of the plugin's own settings (validated and saved by the host)."""
        self._ctx._link.request("host.settings_set", {"name": name, "value": value})
        self._ctx._state.settings[name] = value


class StateHandle[M: BaseModel]:
    """``ctx.state(Model)``: a shared state the host keeps and pushes to subscribers."""

    def __init__(self, ctx: Context, model: type[M], name: str) -> None:
        self._ctx = ctx
        self._model = model
        self.name = name

    def get(self) -> M:
        raw = self._ctx._state.states.get(self.name)
        return self._model() if raw is None else self._model.model_validate(raw)

    def set(self, value: M) -> M:
        data = value.model_dump(mode="json")
        self._ctx._state.states[self.name] = data
        self._ctx._link.notify("state", {"name": self.name, "value": data})
        return value

    def update(self, **fields: Any) -> M:
        return self.set(self.get().model_copy(update=fields))


class Basket:
    """``ctx.basket``: propose files for the delete basket (the user still confirms deleting)."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def propose(self, items: Iterable[NodeLike], reason: Text | str = "") -> int:
        """Add ``items`` with ``reason``; returns how many were added. Needs ``basket.propose``."""
        require(self._ctx._state, "basket.propose")
        tree = self._ctx.tree
        nodes: list[int] = []
        paths: list[str] = []
        for item in items:
            if isinstance(item, Node):
                nodes.append(item.id)
            elif isinstance(item, NodeRef):
                nodes.append(item.node) if item.epoch == tree.epoch or not item.path else paths.append(
                    item.path
                )
            elif isinstance(item, str):
                paths.append(item)
            else:
                nodes.append(int(item))
        value, _ = self._ctx._link.request(
            "host.basket",
            {
                "scan_id": tree.scan_id,
                "epoch": tree.epoch,
                "nodes": nodes,
                "paths": paths,
                "reason": localize(reason) if reason else "",
            },
        )
        return int(value.get("added", 0))


class Files:
    """``ctx.files``: read files inside the scanned folders. Needs ``files.read``."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def _path(self, item: NodeLike) -> str:
        if isinstance(item, str):
            return item
        if isinstance(item, Node):
            return item.path
        if isinstance(item, NodeRef):
            return item.path or self._ctx.tree.path(item.node)
        return self._ctx.tree.path(int(item))

    def open_path(self, path: str | os.PathLike[str]) -> str:
        """Check that ``path`` lies inside an open scan, a scan source's URI the user chose, or the
        plugin's own folders; returns it."""
        require(self._ctx._state, "files.read")
        real = os.path.realpath(os.fspath(path))
        st = self._ctx._state
        allowed = [*st.roots, *st.picked, str(self._ctx.plugin.dir), str(self._ctx.plugin.storage)]
        for root in allowed:
            base = os.path.realpath(root)
            if os.path.normcase(real) == os.path.normcase(base) or os.path.normcase(real).startswith(
                os.path.normcase(base.rstrip("\\/") + os.sep)
            ):
                return real
        raise PermissionError(f"{path}: not inside a scanned folder")

    def open(self, item: NodeLike) -> IO[bytes]:
        return open(self.open_path(self._path(item)), "rb")

    def read(self, item: NodeLike, limit: int = 1 << 20) -> bytes:
        with self.open(item) as fh:
            return fh.read(limit)

    def hash(self, item: NodeLike, algorithm: str = "sha256", *, limit: int | None = None) -> str:
        """Hex digest of the file (or of its first ``limit`` bytes); checks for cancellation."""
        h = hashlib.new(algorithm)
        left = limit
        with self.open(item) as fh:
            while True:
                self._ctx.check()
                size = 1 << 20 if left is None else min(1 << 20, left)
                if size <= 0:
                    break
                chunk = fh.read(size)
                if not chunk:
                    break
                h.update(chunk)
                if left is not None:
                    left -= len(chunk)
        return h.hexdigest()


@dataclass(frozen=True, slots=True)
class Response:
    status: int
    headers: dict[str, str]
    body: bytes

    def json(self) -> Any:
        import json

        return json.loads(self.body)

    def text(self) -> str:
        return self.body.decode("utf-8", "replace")


class Net:
    """``ctx.net``: HTTP(S) to the hosts in ``net_hosts``. Needs ``net``."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def allowed(self, host: str) -> bool:
        hosts = self._ctx._state.net_hosts
        return "*" in hosts or any(fnmatch.fnmatch(host.lower(), h.lower()) for h in hosts)

    def fetch(
        self,
        url: str,
        *,
        method: str = "GET",
        headers: Mapping[str, str] | None = None,
        body: bytes | None = None,
        timeout: float = 30.0,
        limit: int = 64 << 20,
    ) -> Response:
        require(self._ctx._state, "net")
        parts = urlsplit(url)
        if parts.scheme not in ("http", "https") or not parts.hostname:
            raise PermissionError(f"{url}: only http(s) URLs")
        if not self.allowed(parts.hostname):
            raise PermissionError(f"{parts.hostname}: not in net_hosts")
        import urllib.error
        import urllib.request

        req = urllib.request.Request(url, data=body, method=method, headers=dict(headers or {}))
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                data = resp.read(limit + 1)
                if len(data) > limit:
                    raise PluginError(f"{url}: the answer is larger than {limit} bytes")
                return Response(resp.status, dict(resp.headers.items()), data)
        except urllib.error.HTTPError as exc:
            return Response(exc.code, dict(exc.headers.items()), exc.read(limit))


@dataclass(frozen=True, slots=True)
class Completed:
    returncode: int
    stdout: bytes
    stderr: bytes


class Snapshots:
    """``ctx.snapshots``: saved snapshots. Needs ``snapshots.read``."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def list(self, root: str | None = None) -> list[dict[str, Any]]:
        """Cheap: what the snapshot files' headers say (time, totals, roots)."""
        require(self._ctx._state, "snapshots.read")
        value, _ = self._ctx._link.request("host.snapshots", {"root": root})
        return list(value["snapshots"])

    def open(self, path: str) -> TreeView:
        """Expensive: load a snapshot as a tree (released when the call ends)."""
        require(self._ctx._state, "snapshots.read")
        value, _ = self._ctx._link.request(
            "host.snapshot_open", {"path": path, "call": self._ctx.call_info.id}
        )
        return TreeView(self._ctx._link, value)


class Log:
    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def _log(self, level: str, message: str) -> None:
        self._ctx._link.notify("log", {"level": level, "message": str(message)[:4000]})

    def debug(self, message: str) -> None:
        self._log("DEBUG", message)

    def info(self, message: str) -> None:
        self._log("INFO", message)

    def warning(self, message: str) -> None:
        self._log("WARNING", message)

    def error(self, message: str) -> None:
        self._log("ERROR", message)


class Context:
    """The context of one call. Create none yourself: the host passes one to every handler."""

    def __init__(
        self,
        state: RuntimeState,
        link: Link,
        call: CallInfo | None = None,
        *,
        cancel: threading.Event | None = None,
        executor: Any = None,
    ) -> None:
        self._state = state
        self._link = link
        self.call_info = call or CallInfo()
        self._cancel = cancel or threading.Event()
        self.deadline = time.monotonic() + self.call_info.timeout
        self._tree: TreeView | None = None
        self._node: Node | bool | None = False
        self._executor = executor
        self._last_toast = 0.0
        self.settings = Settings(self)
        self.basket = Basket(self)
        self.files = Files(self)
        self.net = Net(self)
        self.snapshots = Snapshots(self)
        self.log = Log(self)

    # ------------------------------------------------------------- who and where

    @property
    def plugin(self) -> PluginSelf:
        return self._state.plugin

    @property
    def storage(self) -> Path:
        """A folder the plugin may write to (kept across runs, removed with the plugin's data)."""
        self._state.plugin.storage.mkdir(parents=True, exist_ok=True)
        return self._state.plugin.storage

    @property
    def lang(self) -> str:
        return LANG.get()

    def t(self, zh: str, en: str = "", /, **fields: Any) -> str:
        """The text in the interface language, placeholders filled."""
        text = Text(zh=zh, en=en)
        return text.format(**fields).get() if fields else text.get()

    @property
    def granted(self) -> frozenset[str]:
        return self._state.granted

    # ------------------------------------------------------------- the tree

    def _tree_info(self, scan_id: str | None) -> dict[str, Any]:
        require(self._state, "tree.read")
        try:
            value, _ = self._link.request("host.tree", {"scan_id": scan_id})
        except HostError as exc:
            raise PluginError(Text(zh="需要先完成扫描", en="Scan first")) from exc
        if value is None:
            raise PluginError(Text(zh="需要先完成扫描", en="Scan first"))
        return dict(value)

    @property
    def tree(self) -> TreeView:
        """The scan this call is about (else the scan in front). Needs ``tree.read``."""
        if self._tree is None:
            self._tree = TreeView(self._link, self._tree_info(self.call_info.scan_id))
        return self._tree

    def scan(self, scan_id: str) -> TreeView:
        return TreeView(self._link, self._tree_info(scan_id))

    def scans(self) -> list[dict[str, Any]]:
        """The open scans: id, roots, state."""
        require(self._state, "tree.read")
        value, _ = self._link.request("host.scans", {})
        return list(value["scans"])

    @property
    def node(self) -> Node | None:
        """The node the call is about (a details panel field, a selection), or None."""
        if self._node is False:
            self._node = None if self.call_info.node is None else self.tree.node(int(self.call_info.node))
        return self._node if isinstance(self._node, Node) else None

    # ------------------------------------------------------------- events, state, messages

    def emit(self, event: BaseModel) -> None:
        """Send an event declared with ``@plugin.event`` (to the caller, or to everyone)."""
        name = self._state.event_names.get(type(event))
        if name is None:
            raise TypeError(f"{type(event).__name__} is not declared with @plugin.event")
        self._link.notify(
            "event",
            {"name": name, "payload": event.model_dump(mode="json"), "session": self.call_info.session},
        )

    def progress(self, done: float, total: float, text: Text | str = "") -> None:
        self._link.notify(
            "progress",
            {
                "id": self.call_info.id,
                "done": float(done),
                "total": float(total),
                "text": localize(text) or "",
            },
        )

    def state[S: BaseModel](self, model: type[S]) -> StateHandle[S]:
        name = next((n for n, m in self._state.state_models.items() if m is model), None)
        if name is None:
            raise TypeError(f"{model.__name__} is not declared with @plugin.state")
        return StateHandle(self, model, name)

    def toast(self, text: Text | str, tone: Tone = "info") -> None:
        """A short message for the user (at most one a second)."""
        now = time.monotonic()
        if now - self._last_toast < 1.0:
            return
        self._last_toast = now
        self._link.notify("toast", {"text": localize(text), "tone": tone, "session": self.call_info.session})

    # ------------------------------------------------------------- host services

    def call(self, method: str, params: Mapping[str, Any] | None = None) -> dict[str, Any]:
        """A built-in method from the allow-list (``tree.summary``, ``basket.list`` …), as a dict."""
        value, _ = self._link.request("host.call", {"method": method, "params": dict(params or {})})
        return dict(value) if isinstance(value, Mapping) else {"value": value}

    def exec(
        self,
        argv: Sequence[str],
        *,
        timeout: float = 60.0,
        cwd: str | None = None,
        input: bytes | None = None,
    ) -> Completed:
        """Run one of the programs listed in ``exec``. Needs ``exec``."""
        require(self._state, "exec")
        if not argv:
            raise ValueError("argv is empty")
        prog = os.path.basename(argv[0])
        stem = prog[:-4] if prog.lower().endswith(".exe") else prog
        if stem not in self._state.exec and prog not in self._state.exec:
            raise PermissionError(f"{prog}: not in the manifest's exec list")
        import subprocess

        res = subprocess.run(
            list(argv),
            cwd=cwd or str(self.storage),
            input=input,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        return Completed(res.returncode, res.stdout, res.stderr)

    async def compute(self, fn: Callable[..., R], /, *args: Any) -> R:
        """Run CPU-heavy code on a worker thread; the plugin's event loop stays responsive."""
        loop = asyncio.get_running_loop()
        cv = contextvars.copy_context()
        return await loop.run_in_executor(self._executor, lambda: cv.run(fn, *args))

    # ------------------------------------------------------------- cancellation

    @property
    def cancelled(self) -> bool:
        return self._cancel.is_set() or time.monotonic() > self.deadline

    def check(self) -> None:
        """Raise :class:`Cancelled` when the call was cancelled or ran out of time."""
        if self.cancelled:
            raise Cancelled()


__all__ = ["CallInfo", "Context", "PluginSelf", "RuntimeState", "Settings", "StateHandle"]
