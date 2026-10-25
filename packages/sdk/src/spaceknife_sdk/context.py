"""``ctx``: what a plugin's code gets with every call — the tree, its settings, the host's services.

Host-backed members are plain (blocking) calls, valid from async and sync handlers alike: the host
answers from its own thread, so waiting here never stalls it. Each one checks the plugin's
permissions before asking (the host checks again).
"""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import fnmatch
import hashlib
import io
import os
import threading
import time
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, TYPE_CHECKING, Any, Literal, TypeVar
from urllib.parse import urlsplit

import numpy as np
from pydantic import BaseModel

from spaceknife_protocol.plugins import NodeRef, Permission, PluginTaskInfo
from spaceknife_protocol.text import LANG, Text, fmt_ago, fmt_count, fmt_date, fmt_size, localize

from ._audit import trusted
from .errors import Cancelled, HostError, PluginError
from .tree import Flag, Kind, Node, TreeView

if TYPE_CHECKING:
    from ._link import Link
    from .models import ScanInfo, SnapshotInfo

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
    # worker threads (the manifest's limits.threads), the spares for cancelled calls, and how long
    # every thread may be held by cancelled calls before the host restarts the process
    threads: int = 2
    spare_threads: int = 8
    stuck_after: float = 30.0


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
    slot: str | None = None  # a node field: "inspector" or "hover"
    units: int = 1024  # the user's size unit base (1000 or 1024): ctx.fmt.size

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
            slot=d.get("slot"),
            units=1000 if d.get("units") == 1000 else 1024,
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

    def _entries(self, items: Iterable[Any], reason: Text | str) -> list[dict[str, Any]]:
        tree = self._ctx.tree
        out: list[dict[str, Any]] = []
        for item in items:
            why: Text | str = reason
            if isinstance(item, tuple):  # (node, its own reason)
                item, why = item
            text = (localize(why) if why else "") or ""
            if isinstance(item, Node):
                out.append({"node": item.id, "reason": text})
            elif isinstance(item, NodeRef):
                if item.epoch == tree.epoch or not item.path:
                    out.append({"node": item.node, "reason": text})
                else:
                    out.append({"path": item.path, "reason": text})
            elif isinstance(item, str):
                out.append({"path": item, "reason": text})
            else:
                out.append({"node": int(item), "reason": text})
        return out

    def propose(
        self, items: Iterable[NodeLike | tuple[NodeLike, Text | str]], reason: Text | str = ""
    ) -> int:
        """Add ``items`` to the delete basket; returns how many were added. Each item is a node
        (id, :class:`Node`, :class:`NodeRef` or absolute path) or ``(node, its own reason)``;
        ``reason`` is for the items without one. However many calls a command makes, the user gets
        one message for them at its end (none when the command shows its own toast). Needs
        ``basket.propose``."""
        require(self._ctx._state, "basket.propose")
        tree = self._ctx.tree
        value, _ = self._ctx._link.request(
            "host.basket",
            {
                "scan_id": tree.scan_id,
                "epoch": tree.epoch,
                "items": self._entries(items, reason),
                "call": self._ctx.call_info.id,
            },
        )
        return int(value.get("added", 0))

    def withdraw(self, items: Iterable[NodeLike]) -> int:
        """Take back items this plugin put in the basket (others' and the user's stay); returns how
        many went. Needs ``basket.propose``."""
        require(self._ctx._state, "basket.propose")
        tree = self._ctx.tree
        value, _ = self._ctx._link.request(
            "host.basket_withdraw",
            {"scan_id": tree.scan_id, "epoch": tree.epoch, "items": self._entries(items, "")},
        )
        return int(value.get("removed", 0))


class Clipboard:
    """``ctx.clipboard``: put text on the user's clipboard. Needs ``clipboard``."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def write_text(self, text: Text | str) -> None:
        """Copy ``text`` (≤ 1 MiB): the app of the user who started the call puts it on the
        clipboard and says so. From a call no user started (a hook, a watcher) nothing is copied."""
        require(self._ctx._state, "clipboard")
        value = localize(text) if not isinstance(text, str) else text
        self._ctx._link.notify(
            "clipboard", {"text": str(value)[: 1 << 20], "session": self._ctx.call_info.session}
        )


@dataclass(frozen=True, slots=True)
class Fmt:
    """``ctx.fmt``: how the app writes sizes, counts and times (an export, a toast, a cell of text)."""

    units: int = 1024

    def size(self, n: float, *, whole: bool = False) -> str:
        return fmt_size(n, self.units, whole=whole)

    def count(self, n: float) -> str:
        return fmt_count(n)

    def date(self, ts: float) -> str:
        return fmt_date(ts)

    def ago(self, ts: float) -> str:
        return fmt_ago(ts)


class FileNotAllowed(PermissionError):
    """``ctx.files``: the path is outside the scanned folders, the scan source's URI and the
    plugin's own folders (the plugin may not read it). A ``PermissionError`` that is not this one
    is the system's: the user's account may not read the file either."""


# the allowed folders, resolved once per set of roots (resolving a path walks its folders)
_BASES: dict[tuple[str, ...], tuple[str, ...]] = {}
# readers that ctx.files.hash_many runs at once (each waits on the disk most of the time)
HASH_THREADS = 4
# flags of files whose contents are not here: reading them makes the cloud client fetch them
NOT_LOCAL = Flag.CLOUD | Flag.OFFLINE
# reads go in bursts, each with the host's permit for that disk (its disk-load limit, which scans
# share): a burst is at most this many bytes or this long
IO_BURST_BYTES = 4 << 20
IO_BURST_S = 0.05
IO_FREE_S = 1.0  # the host said "no limit here": ask again after this long
PROGRESS_EVERY_S = 0.2
Progress = Callable[[int, int], None]  # (bytes read, bytes in all)


class _Lease:
    """One reader's reads on one disk, in bursts paced by the host's I/O governor."""

    __slots__ = ("_ctx", "base", "free_until", "permit", "since", "used")

    def __init__(self, ctx: Context, base: str) -> None:
        self._ctx = ctx
        self.base = base
        self.permit = 0
        self.since = 0.0
        self.used = 0
        self.free_until = 0.0

    def need(self, n: int) -> None:
        """Before reading ``n`` bytes: within the current burst, or wait for the next permit."""
        now = time.monotonic()
        if now < self.free_until or (
            self.permit and self.used < IO_BURST_BYTES and now - self.since < IO_BURST_S
        ):
            self.used += n
            return
        release = [self.permit] if self.permit else []
        self.permit = 0
        while True:  # each request waits at most ~2 s: cancelling the call is noticed meanwhile
            self._ctx.check()
            value, _ = self._ctx._link.request("host.io", {"base": self.base, "release": release})
            release = []
            got = int((value or {}).get("permit") or 0)
            if got or not (value or {}).get("wait"):
                break
        self.since = time.monotonic()
        self.used = n
        if got:
            self.permit = got
        else:  # no limit on this disk (nothing measures it, or its scan is paused)
            self.free_until = self.since + IO_FREE_S

    def close(self) -> None:
        if self.permit:
            with contextlib.suppress(Exception):
                self._ctx._link.notify("io_release", {"permits": [self.permit]})
            self.permit = 0


class _PacedFile(io.RawIOBase):
    """A file read through ``ctx.files``: every read waits for the disk's permit."""

    def __init__(self, raw: io.FileIO, lease: _Lease, owned: bool = True) -> None:
        super().__init__()
        self._raw = raw
        self._lease = lease
        self._owned = owned  # the lease ends with the file (else a reader of many files keeps it)

    @property
    def name(self) -> Any:
        return self._raw.name

    def readable(self) -> bool:
        return True

    def seekable(self) -> bool:
        return self._raw.seekable()

    def seek(self, pos: int, whence: int = 0) -> int:
        return self._raw.seek(pos, whence)

    def tell(self) -> int:
        return self._raw.tell()

    def fileno(self) -> int:
        return self._raw.fileno()

    def readinto(self, b: Any) -> int | None:
        self._lease.need(len(b))
        return self._raw.readinto(b)

    def close(self) -> None:
        if not self.closed:
            try:
                if self._owned:
                    self._lease.close()
                self._raw.close()
            finally:
                super().close()


class _Meter:
    """Progress of reading: calls ``fn(done, total)`` at most every 0.2 s, and at the end."""

    def __init__(self, fn: Progress | None, total: int) -> None:
        self.fn = fn
        self.total = max(0, total)
        self.done = 0
        self._at = 0.0
        self._lock = threading.Lock()

    def add(self, n: int) -> None:
        if self.fn is None:
            return
        with self._lock:
            self.done += n
            now = time.monotonic()
            if now - self._at < PROGRESS_EVERY_S:
                return
            self._at = now
            done = self.done
        self.fn(done, max(done, self.total))

    def end(self) -> None:
        if self.fn is not None:
            self.fn(self.done, max(self.done, self.total))


class Files:
    """``ctx.files``: read files inside the scanned folders. Needs ``files.read``.

    Reads go through the host's disk-load limit (the level the user chose, shared with scans of the
    same disk; while one runs, plugins read one burst at a time). Skip nodes flagged ``Flag.CLOUD`` or
    ``Flag.OFFLINE`` (placeholders: reading one downloads it) and folders; :meth:`hash_many` does.
    ``FileNotAllowed`` (a ``PermissionError``): outside what the plugin may read; another
    ``PermissionError``: the system refused (the user may not either).
    """

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

    def _bases(self) -> tuple[str, ...]:
        st = self._ctx._state
        key = (*st.roots, "", *st.picked, "", str(self._ctx.plugin.dir), str(self._ctx.plugin.storage))
        hit = _BASES.get(key)
        if hit is None:
            hit = tuple(os.path.normcase(os.path.realpath(r)).rstrip("\\/") for r in key if r)
            if len(_BASES) > 64:
                _BASES.clear()
            _BASES[key] = hit
        return hit

    def _resolve(self, path: str | os.PathLike[str]) -> tuple[str, str]:
        """(the resolved path, the allowed folder it lies in); ``FileNotAllowed`` outside them."""
        require(self._ctx._state, "files.read")
        real = os.path.realpath(os.fspath(path))
        low = os.path.normcase(real)
        for base in self._bases():
            if low == base or low.startswith(base + os.sep):
                return real, base
        raise FileNotAllowed(f"{path}: not inside a scanned folder")

    def open_path(self, path: str | os.PathLike[str]) -> str:
        """Check that ``path`` lies inside an open scan, a scan source's URI the user chose, or the
        plugin's own folders; returns it resolved (``FileNotAllowed`` when it does not). What reads
        a path itself (``zipfile.ZipFile(path)``, ``ctx.exec``) is not paced by the disk-load limit:
        hand such libraries ``ctx.files.open(n)`` (a file object) where they take one."""
        real, _base = self._resolve(path)
        trusted.path = real  # the sandbox's own check of this open need not resolve it again
        return real

    def _open(self, real: str, base: str, lease: _Lease | None = None) -> IO[bytes]:
        """A resolved path (``_resolve``) for paced reading; with ``lease``, on that reader's permits."""
        trusted.path = real
        raw = open(real, "rb", buffering=0)  # noqa: SIM115 — closed with the paced file
        if lease is None or lease.base != base:
            paced = _PacedFile(raw, _Lease(self._ctx, base), owned=True)
        else:
            paced = _PacedFile(raw, lease, owned=False)
        return io.BufferedReader(paced, buffer_size=1 << 20)

    def open(self, item: NodeLike) -> IO[bytes]:
        """The file for reading (binary, seekable): its reads keep to the disk-load limit."""
        return self._open(*self._resolve(self._path(item)))

    def read(self, item: NodeLike, limit: int = 1 << 20) -> bytes:
        with self.open(item) as fh:
            return fh.read(limit)

    def _digest(
        self,
        where: tuple[str, str],
        algorithm: str,
        limit: int | None,
        meter: _Meter | None,
        lease: _Lease | None,
    ) -> str:
        h = hashlib.new(algorithm)
        left = limit
        with self._open(*where, lease) as fh:
            while True:
                self._ctx.check()
                size = 1 << 20 if left is None else min(1 << 20, left)
                if size <= 0:
                    break
                chunk = fh.read(size)
                if not chunk:
                    break
                h.update(chunk)
                if meter is not None:
                    meter.add(len(chunk))
                if left is not None:
                    left -= len(chunk)
        return h.hexdigest()

    def _meter(self, progress: bool | Progress | None, total: int) -> _Meter | None:
        if not progress:
            return None
        if callable(progress):
            return _Meter(progress, total)
        ctx = self._ctx
        text = Text(zh="正在读取文件内容", en="Reading file contents")
        return _Meter(lambda done, all_: ctx.progress(done, all_, text), total)

    def hash(
        self,
        item: NodeLike,
        algorithm: str = "sha256",
        *,
        limit: int | None = None,
        progress: bool | Progress | None = None,
    ) -> str:
        """Hex digest of the file (or of its first ``limit`` bytes); checks for cancellation every
        MiB. ``progress=True`` reports ``ctx.progress`` (bytes read of the file's size), or give a
        function ``(done, total)`` (bytes), called at most every 0.2 s and at the end."""
        where = self._resolve(self._path(item))
        meter = None
        if progress:
            try:
                size = os.path.getsize(where[0])
            except OSError:
                size = 0
            meter = self._meter(progress, size if limit is None else min(size, limit))
        out = self._digest(where, algorithm, limit, meter, None)
        if meter is not None:
            meter.end()
        return out

    def hash_many(
        self,
        items: Iterable[NodeLike],
        algorithm: str = "sha256",
        *,
        limit: int | None = None,
        threads: int | None = None,
        progress: bool | Progress | None = None,
    ) -> list[str | None]:
        """Hex digests of many files (of their first ``limit`` bytes), in order: a few readers at
        once (``threads``, default :data:`HASH_THREADS`), so the disk stays busy within its load
        limit. ``None`` for an item that is a folder, a cloud or offline placeholder (not read: that
        would download it), gone, or unreadable. Checks for cancellation every MiB; ``progress`` as
        in :meth:`hash`, over the bytes of all the files."""
        from concurrent.futures import ThreadPoolExecutor

        require(self._ctx._state, "files.read")
        todo = list(items)
        tree = self._ctx.tree
        ids = [int(i) for i in todo if isinstance(i, int | np.integer)]
        skip: set[int] = set()
        sizes: Any = None
        if ids:
            kinds = tree.col("kind")
            flags = tree.col("flags")
            skip = {i for i in ids if int(kinds[i]) != Kind.FILE or int(flags[i]) & int(NOT_LOCAL)}
            if progress:
                sizes = tree.col("size")
        paths: list[str | None] = []
        total = 0
        for i in todo:
            if (isinstance(i, int | np.integer) and int(i) in skip) or (
                isinstance(i, Node) and (not i.is_file or i.flags & int(NOT_LOCAL))
            ):
                paths.append(None)
                continue
            paths.append(self._path(i))
            if progress:
                if isinstance(i, int | np.integer) and sizes is not None:
                    size = int(sizes[int(i)])
                elif isinstance(i, Node):
                    size = int(i.size)
                else:
                    try:
                        size = os.path.getsize(paths[-1] or "")
                    except OSError:
                        size = 0
                total += size if limit is None else min(size, limit)
        meter = self._meter(progress, total)
        leases: list[_Lease] = []
        local = threading.local()  # each reader keeps its permit across small files of one disk

        def one(path: str | None) -> str | None:
            if path is None or self._ctx.cancelled:
                return None
            lease: _Lease | None = getattr(local, "lease", None)
            try:
                where = self._resolve(path)
                if lease is None or lease.base != where[1]:
                    if lease is not None:
                        lease.close()
                    lease = local.lease = _Lease(self._ctx, where[1])
                    leases.append(lease)
                return self._digest(where, algorithm, limit, meter, lease)
            except (OSError, ValueError):
                return None

        n = max(1, min(threads or HASH_THREADS, 16))
        try:
            if n == 1 or len(paths) < 4:
                out = [one(p) for p in paths]
            else:
                base = contextvars.copy_context()  # the call's (language, cancellation): one copy a task
                with ThreadPoolExecutor(n, thread_name_prefix="sk-hash") as pool:
                    out = list(pool.map(lambda p: base.copy().run(one, p), paths))
        finally:
            for lease in leases:
                lease.close()
        if meter is not None:
            meter.end()
        self._ctx.check()
        return out


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

    def list(self, root: str | None = None) -> list[SnapshotInfo]:
        """Cheap: what the snapshot files' headers say (time, totals, roots), newest first; only
        those of ``root`` (a scanned folder's path) when given."""
        from .models import SnapshotInfo

        require(self._ctx._state, "snapshots.read")
        value, _ = self._ctx._link.request("host.snapshots", {"root": root})
        return [SnapshotInfo.model_validate(d) for d in value["snapshots"]]

    def open(self, path: str) -> TreeView:
        """Expensive: load a snapshot as a tree (released when the call ends). Only a path from
        :meth:`list` or a snapshot the user opened (PermissionError otherwise)."""
        require(self._ctx._state, "snapshots.read")
        value, _ = self._ctx._link.request(
            "host.snapshot_open", {"path": path, "call": self._ctx.call_info.id}
        )
        return TreeView(self._ctx._link, value)


class TaskInfo(PluginTaskInfo):
    """A background task as the host keeps track of it (``ctx.tasks``, ``MockHost.task``)."""


class Tasks:
    """``ctx.tasks``: the plugin's background tasks (``@plugin.background``), run by the host."""

    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def start(
        self,
        name: str,
        params: BaseModel | Mapping[str, Any] | None = None,
        *,
        key: str = "",
        replace: bool = False,
    ) -> TaskInfo:
        """Start the task ``name`` with ``params`` (its second argument). One runs per name and
        ``key`` (a scan id, a folder …): starting it again returns the running one, or with
        ``replace`` cancels it and starts anew. Returns at once; the task runs on its own, its
        ``ctx.tree`` the scan of this call (else the scan in front)."""
        body = params.model_dump(mode="json") if isinstance(params, BaseModel) else dict(params or {})
        value, _ = self._ctx._link.request(
            "host.task_start",
            {
                "name": name,
                "key": str(key),
                "params": body,
                "replace": bool(replace),
                "scan_id": self._ctx.call_info.scan_id,
            },
        )
        return TaskInfo.model_validate(value)

    def cancel(self, name: str, *, key: str | None = None) -> int:
        """Cancel the task ``name`` (only the one of ``key`` when given); returns how many."""
        value, _ = self._ctx._link.request("host.task_cancel", {"name": name, "key": key})
        return int((value or {}).get("cancelled", 0))

    def list(self) -> list[TaskInfo]:
        """This plugin's tasks: the running ones and the last few that ended."""
        value, _ = self._ctx._link.request("host.tasks", {})
        return [TaskInfo.model_validate(d) for d in (value or {}).get("tasks", [])]


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
        self._follow_lang = False  # a background task: the interface language may change meanwhile
        self._last_toast = 0.0
        self.settings = Settings(self)
        self.basket = Basket(self)
        self.clipboard = Clipboard(self)
        self.files = Files(self)
        self.net = Net(self)
        self.snapshots = Snapshots(self)
        self.tasks = Tasks(self)
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
        """The text in the interface language, placeholders filled (``{n:size}`` … as T.format)."""
        text = Text(zh=zh, en=en)
        return text.format(**fields).get() if fields else text.get()

    @property
    def fmt(self) -> Fmt:
        """Numbers as the interface writes them (the user's size unit, the interface language):
        ``ctx.fmt.size(n)`` 1.2 GB, ``count(n)`` 1,234, ``date(t)``, ``ago(t)`` 3 天前."""
        return Fmt(self.call_info.units)

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

    def scans(self) -> list[ScanInfo]:
        """The open scans, newest first (``front`` marks the one in front). Needs ``tree.read``."""
        from .models import ScanInfo

        require(self._state, "tree.read")
        value, _ = self._link.request("host.scans", {})
        return [ScanInfo.model_validate(d) for d in value["scans"]]

    def invalidate(self, *kinds: str) -> None:
        """Forget what the host cached of this plugin's answers — fields, node sets, a code
        visualization's weights, its colour modes (legends, the views painting them), change
        reasons (``"node_field"``, ``"node_set"``, ``"visualization"``, ``"color_mode"``,
        ``"change_annotator"``; none: all of them). The host does this by itself when the plugin's
        settings change; call it when something else changed the answers (a file the plugin
        reads, a cache it built, a result computed in the background)."""
        self._link.request("host.invalidate", {"kinds": list(kinds)})

    @property
    def slot(self) -> str | None:
        """Where a node field is shown: ``"inspector"`` (1 s) or ``"hover"`` (30 ms); else None."""
        return self.call_info.slot

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
        """How far the call is (``total = 0``: it cannot say): a report's or a cleanup's progress
        bar, a background task's line in the status bar."""
        msg: dict[str, Any] = {
            "id": self.call_info.id,
            "done": float(done),
            "total": float(total),
            "text": (localize(text) or "")[:512],
        }
        if self._follow_lang and text and not isinstance(text, str):
            both: dict[str, str] = {}
            for lang in ("zh-CN", "en"):  # the host shows the one of the language at the time
                token = LANG.set(lang)
                try:
                    both[lang] = (localize(text) or "")[:512]
                finally:
                    LANG.reset(token)
            msg["texts"] = both
        self._link.notify("progress", msg)

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
        self._link.notify(
            "toast",
            {
                "text": localize(text),
                "tone": tone,
                "session": self.call_info.session,
                "call": self.call_info.id,
            },
        )

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
        cv = contextvars.copy_context()
        workers = self._executor
        if hasattr(workers, "run"):  # the plugin's worker threads (a cancelled one finishes aside)
            out: R = await workers.run(lambda: cv.run(fn, *args), f"compute {getattr(fn, '__name__', '')}")
            return out
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(workers, lambda: cv.run(fn, *args))

    # ------------------------------------------------------------- cancellation

    @property
    def cancelled(self) -> bool:
        return self._cancel.is_set() or time.monotonic() > self.deadline

    def check(self) -> None:
        """Raise :class:`Cancelled` when the call was cancelled or ran out of time. In a background
        task it also takes up a language switch (``ctx.lang`` and ``T(...)`` follow from here on)."""
        if self._follow_lang and LANG.get() != self._state.lang:
            LANG.set(self._state.lang)
        if self.cancelled:
            raise Cancelled()


__all__ = [
    "CallInfo",
    "Context",
    "FileNotAllowed",
    "PluginSelf",
    "RuntimeState",
    "Settings",
    "StateHandle",
    "TaskInfo",
    "Tasks",
]
