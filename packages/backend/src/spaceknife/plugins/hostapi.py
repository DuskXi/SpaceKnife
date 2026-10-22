"""What a plugin may ask its host (``host.*`` requests), each checked against its permissions.

The same handlers answer a plugin process and in-process code. Node ids carry the tree's epoch:
a request with an old epoch is ``STALE_TREE`` (paths still resolve). Only the allow-list of
built-in methods is reachable through ``host.call`` (allow.py); deletes are never.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any
from weakref import WeakKeyDictionary

import numpy as np
from pydantic import BaseModel, ValidationError

from spaceknife_protocol import KNOWN_CAPABILITIES
from spaceknife_protocol import plugins as pp
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import EVENT_PLUGIN_TOAST
from spaceknife_protocol.ops import BasketAddParams
from spaceknife_protocol.text import LANG

from .. import i18n
from ..i18n import plural
from ..layout.basis import Basis
from ..server.compute import compute
from ..server.rpc import RpcError
from ..store.nodes import NodeStore
from . import query
from .allow import permission_for
from .image import export
from .runtime.base import RuntimeCallError

if TYPE_CHECKING:
    from ..server.scans import ScanHandle
    from .host import PluginHost
    from .store import PluginRecord

MAX_NODES = 100_000


class PluginCaller:
    """The 'session' a plugin's ``host.call`` runs a built-in handler with (no socket)."""

    def __init__(self, plugin: str) -> None:
        self.id = f"plugin:{plugin}"
        self.plugin = plugin
        self.capabilities = frozenset(KNOWN_CAPABILITIES)
        self.client = None

    def emit(self, *args: Any, **kw: Any) -> None:
        return None

    def emit_raw(self, *args: Any, **kw: Any) -> None:
        return None

    def send_binary(self, frame: bytes) -> None:
        return None

    def add_subscription(self, sub: Any) -> None:
        return None


def _deny(permission: str) -> RuntimeCallError:
    return RuntimeCallError(
        ErrorCode.PERMISSION_DENIED,
        f"this plugin was not granted {permission!r}",
        {"permission": permission},
    )


class HostApi:
    def __init__(self, host: PluginHost) -> None:
        self.host = host
        self.server = host.server
        self._bases: WeakKeyDictionary[NodeStore, Basis] = WeakKeyDictionary()
        self._opened: dict[str, tuple[str, NodeStore]] = {}  # opened by plugins: id -> (plugin, store)

    # ------------------------------------------------------------- scans

    def handles(self) -> dict[str, ScanHandle]:
        return self.server.scans.scans

    def store_of(self, scan_id: str) -> NodeStore | None:
        h = self.handles().get(scan_id)
        if h is not None:
            return h.store
        snap = self._opened.get(scan_id)
        return snap[1] if snap is not None else None

    def scan_of_store(self, store: NodeStore) -> str | None:
        for sid, h in list(self.handles().items()):
            if h.store is store:
                return sid
        for sid, (_owner, s) in list(self._opened.items()):
            if s is store:
                return sid
        return None

    def running(self, scan_id: str) -> bool:
        h = self.handles().get(scan_id)
        return h is not None and h.running

    def basis(self, store: NodeStore) -> Basis:
        b = self._bases.get(store)
        if b is None:
            b = self._bases[store] = Basis(store)
        return b

    def cls_column(self, scan_id: str) -> np.ndarray:
        store = self.store_of(scan_id)
        if store is None:
            return np.zeros(0, dtype=np.uint8)
        b = self.basis(store)
        out: np.ndarray = b.cls_of(np.arange(len(store), dtype=np.int64))
        return out

    def scan_roots(self) -> list[str]:
        out: list[str] = []
        for h in list(self.handles().values()):
            if getattr(h, "plugin_source", None):
                continue
            out += [r.path for r in h.roots]
        return out

    def active_scan(self) -> str | None:
        """The scan in front: the newest one with a view, else the newest one."""
        handles = self.handles()
        viewed = [h.scan_id for h in self.server.views.views.values() if h.scan_id in handles]
        if viewed:
            return viewed[-1]
        return next(reversed(handles), None) if handles else None

    def _store(self, rec: PluginRecord, p: Mapping[str, Any], *, epoch: bool = True) -> tuple[str, NodeStore]:
        if "tree.read" not in rec.granted:
            raise _deny("tree.read")
        scan_id = p.get("scan_id") or self.active_scan()
        if scan_id is None:
            raise RuntimeCallError(ErrorCode.UNKNOWN_SCAN, "no scan")
        store = self.store_of(str(scan_id))
        if store is None:
            raise RuntimeCallError(ErrorCode.UNKNOWN_SCAN, f"unknown scan: {scan_id}")
        if epoch and p.get("epoch") is not None and int(p["epoch"]) != int(getattr(store, "epoch", 0)):
            raise RuntimeCallError(
                ErrorCode.STALE_TREE, "the tree was reorganised: node ids changed", {"epoch": store.epoch}
            )
        return str(scan_id), store

    def tree_info(self, scan_id: str, store: NodeStore) -> dict[str, Any]:
        h = self.handles().get(scan_id)
        b = self.basis(store)
        lang = i18n.language()
        from spaceknife_protocol.classes import localized

        return {
            "scan_id": scan_id,
            "epoch": int(getattr(store, "epoch", 0)),
            "version": int(store.version),
            "n": len(store),
            "roots": list(store.roots()),
            "root_paths": [store.name(r) for r in store.roots()],
            "virtual": bool(getattr(h, "plugin_source", None)) if h is not None else True,
            "running": bool(h is not None and h.running),
            "classes": [
                {"index": i, "id": c.id, "name": localized(c, lang).name}
                for i, c in enumerate(b.classes.classes)
            ],
        }

    # ------------------------------------------------------------- requests

    async def handle(
        self, rec: PluginRecord, method: str, params: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        fn = getattr(self, "_" + method.removeprefix("host.").replace(".", "_"), None)
        if fn is None or not method.startswith("host."):
            raise RuntimeCallError(ErrorCode.METHOD_NOT_FOUND, f"unknown host request {method}")
        token = LANG.set(i18n.language())
        try:
            result: tuple[Any, list[bytes]] = await fn(rec, params, frames)
            return result
        except RpcError as exc:
            raise RuntimeCallError(exc.code, exc.message, exc.data) from None
        except (KeyError, ValueError, TypeError) as exc:
            raise RuntimeCallError(ErrorCode.INVALID_PARAMS, f"{method}: {exc}") from None
        finally:
            LANG.reset(token)

    async def _tree(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        scan_id, store = self._store(rec, p, epoch=False)
        return self.tree_info(scan_id, store), []

    async def _scans(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        if "tree.read" not in rec.granted:
            raise _deny("tree.read")
        out = []
        for sid, h in list(self.handles().items()):
            st = h.status()
            out.append(
                {
                    "scan_id": sid,
                    "roots": [r.path for r in h.roots],
                    "state": st.state,
                    "items": st.items,
                    "source": getattr(h, "plugin_source", None) or h.source,
                }
            )
        return {"scans": out}, []

    async def _node(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        _scan_id, store = self._store(rec, p)
        nodes = [int(n) for n in p.get("nodes", [])][:MAX_NODES]
        n = len(store)
        bad = [x for x in nodes if not 0 <= x < n]
        if bad:
            raise RuntimeCallError(ErrorCode.UNKNOWN_NODE, f"no node {bad[0]}")
        b = self.basis(store)

        def work() -> list[dict[str, Any]]:
            return [query.node_dict(store, b, x) for x in nodes]

        return {"nodes": await compute(work)}, []

    async def _paths(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        _scan_id, store = self._store(rec, p)
        nodes = [int(n) for n in p.get("nodes", [])][:MAX_NODES]
        n = len(store)
        what = p.get("what", "path")

        def work() -> list[str]:
            return [
                ("" if not 0 <= x < n else store.name(x) if what == "name" else store.path(x)) for x in nodes
            ]

        return {"texts": await compute(work)}, []

    async def _find(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        _scan_id, store = self._store(rec, p, epoch=False)
        found = await compute(query.find, store, str(p["path"]))
        return {"node": found, "epoch": int(getattr(store, "epoch", 0))}, []

    async def _query(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        scan_id, store = self._store(rec, p)
        b = self.basis(store)
        shared = self.server.views.shared(scan_id) if scan_id in self.handles() else None
        tags = shared.tags.bits(store) if shared is not None else None
        sets = None
        where = p.get("where")
        if where and ":set:" in str(where):
            from ..extensions import EXTENSIONS

            ids = [c.id for c in EXTENSIONS.of("node_set")]
            sets = await asyncio.to_thread(self.host.set_masks, scan_id, store, ids)
        return await compute(query.run, store, b, p, tags=tags, sets=sets)

    async def _image(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        scan_id, store = self._store(rec, p, epoch=False)
        from spaceknife_sdk.tree import COLUMNS

        cols = {str(c) for c in p.get("columns", [])} | {"child_start", "child_count"}
        bad = cols - set(COLUMNS)
        if bad:
            raise RuntimeCallError(ErrorCode.INVALID_PARAMS, f"unknown columns: {sorted(bad)}")
        found = self.host.images.find(scan_id, int(store.version), cols, self.running(scan_id))
        if found is None:
            b = self.basis(store)
            found = await compute(
                export, scan_id, store, sorted(cols), lambda: b.cls_of(np.arange(len(store), dtype=np.int64))
            )
            self.host.images.add(found, rec.id)
        else:
            self.host.images.use(found, rec.id)
        return found.desc, []

    async def _call(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        method = str(p.get("method", ""))
        allowed, perm = permission_for(method)
        if not allowed:
            raise RuntimeCallError(
                ErrorCode.PERMISSION_DENIED, f"plugins may not call {method}", {"method": method}
            )
        if perm is not None and perm not in rec.granted:
            raise _deny(perm)
        route = self.server.router.get(method)
        if route is None:
            raise RuntimeCallError(ErrorCode.METHOD_NOT_FOUND, f"method not found: {method}")
        body = dict(p.get("params") or {})
        if method == "basket.add":
            body["origin"] = (
                rec.id if not str(body.get("origin") or "").startswith(rec.id + ".") else body["origin"]
            )
        try:
            params = route.spec.params.model_validate(body)
        except ValidationError as exc:
            raise RuntimeCallError(
                ErrorCode.INVALID_PARAMS, "invalid params", [e["msg"] for e in exc.errors(include_url=False)]
            ) from None
        result = await route.handler(PluginCaller(rec.id), params)  # type: ignore[arg-type]
        if method == "basket.add":
            self._told_basket(rec, int(getattr(result, "added", 0)))
        return (result.model_dump(mode="json") if isinstance(result, BaseModel) else result), []

    async def _basket(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        if "basket.propose" not in rec.granted:
            raise _deny("basket.propose")
        scan_id, store = self._store(rec, p, epoch=False)
        nodes = [int(n) for n in p.get("nodes", [])]
        if nodes and p.get("epoch") is not None and int(p["epoch"]) != int(getattr(store, "epoch", 0)):
            raise RuntimeCallError(ErrorCode.STALE_TREE, "the tree was reorganised: node ids changed")
        for path in p.get("paths", []):
            found = await compute(query.find, store, str(path))
            if found is not None:
                nodes.append(found)
        nodes = list(dict.fromkeys(nodes))[:MAX_NODES]
        reason = str(p.get("reason", ""))[:200]
        res = await self.server.ops.add(
            PluginCaller(rec.id),  # type: ignore[arg-type]
            BasketAddParams(scan_id=scan_id, nodes=nodes, origin=rec.id, reasons=[reason] * len(nodes)),
        )
        self._told_basket(rec, res.added)
        return {
            "added": res.added,
            "merged": res.merged,
            "refused": [r.model_dump() for r in res.refused],
        }, []

    def _told_basket(self, rec: PluginRecord, added: int) -> None:
        if added <= 0:
            return
        text = plural(
            added,
            "{plugin} 向删除篮加入了 {count} 项",
            "{plugin} added 1 item to the delete basket",
            plugin=rec.name,
            count=added,
        )
        ev = pp.PluginToastEvent(plugin=rec.id, text=text, tone="info")
        for s in self.host._sessions():
            s.emit(EVENT_PLUGIN_TOAST, ev)

    async def _settings_set(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        from ..settings import SettingsError

        name = str(p["name"])
        try:
            value = p.get("value")
            if not isinstance(value, bool | int | float | str):
                raise RuntimeCallError(ErrorCode.INVALID_PARAMS, "a setting is a bool, a number or a string")
            self.server.settings.update({f"plugin.{rec.id}.{name}": value})
        except SettingsError as exc:
            raise RuntimeCallError(ErrorCode.INVALID_PARAMS, str(exc)) from None
        return {"ok": True}, []

    async def _snapshots(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        if "snapshots.read" not in rec.granted:
            raise _deny("snapshots.read")
        from ..server.scans import snapshot_info
        from ..store import snapshot

        found = await asyncio.to_thread(snapshot.list_snapshots)
        root = p.get("root")
        out = []
        for path, header in found:
            info = snapshot_info(path, header).model_dump(mode="json")
            roots = [str(r) for r in info.get("roots", [])]
            if root and not any(os.path.normcase(r) == os.path.normcase(str(root)) for r in roots):
                continue
            out.append(info)
        return {"snapshots": out}, []

    async def _snapshot_open(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        if "snapshots.read" not in rec.granted or "tree.read" not in rec.granted:
            raise _deny("snapshots.read")
        from pathlib import Path

        from ..store import snapshot

        mine = [k for k, (owner, _s) in self._opened.items() if owner == rec.id]
        if len(mine) >= 2:
            self._opened.pop(mine[0], None)
        try:
            store, _header = await asyncio.to_thread(snapshot.load, Path(str(p["path"])))
        except (OSError, snapshot.SnapshotError) as exc:
            raise RuntimeCallError(ErrorCode.SNAPSHOT_ERROR, str(exc)) from None
        sid = f"snap-{rec.id}-{len(self._opened)}-{id(store) & 0xFFFF:x}"
        self._opened[sid] = (rec.id, store)
        return self.tree_info(sid, store), []

    async def _storage_usage(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        from . import paths as ppaths

        def size() -> int:
            total = 0
            for root, _dirs, files in os.walk(ppaths.data_dir(rec.id)):
                for f in files:
                    with contextlib.suppress(OSError):
                        total += os.path.getsize(os.path.join(root, f))
            return total

        return {"bytes": await asyncio.to_thread(size)}, []

    async def _source_batch(
        self, rec: PluginRecord, p: dict[str, Any], frames: list[bytes]
    ) -> tuple[Any, list[bytes]]:
        raise RuntimeCallError(ErrorCode.INVALID_REQUEST, "listings go with the source call's output")

    # ------------------------------------------------------------- results the host checks

    def checked_report(self, value: Any) -> Any:
        """A report within its limits (tables cut to 2000 rows, 200 blocks, 4 levels, 2 MiB)."""
        if not isinstance(value, dict):
            raise RpcError(ErrorCode.PLUGIN_ERROR, "a report handler must return a Report")
        try:
            rep = pp.Report.model_validate(value)
        except ValidationError as exc:
            raise RpcError(
                ErrorCode.PLUGIN_ERROR, f"invalid report: {exc.errors(include_url=False)[0]['msg']}"
            ) from None
        cut = False
        count = 0

        def trim(blocks: list[Any], depth: int) -> list[Any]:
            nonlocal cut, count
            out = []
            for b in blocks:
                count += 1
                if count > pp.MAX_BLOCKS:
                    cut = True
                    break
                if b.type == "table" and len(b.rows) > pp.MAX_TABLE_ROWS:
                    b = b.model_copy(update={"rows": b.rows[: pp.MAX_TABLE_ROWS]})
                    cut = True
                if b.type == "section":
                    if depth >= pp.MAX_DEPTH:
                        cut = True
                        continue
                    b = b.model_copy(update={"blocks": trim(list(b.blocks), depth + 1)})
                out.append(b)
            return out

        blocks = trim(list(rep.blocks), 1)
        if cut:
            blocks.append(pp.CalloutBlock(tone="warn", text=i18n._("报告太大，只显示了一部分")))
        out = rep.model_copy(update={"blocks": blocks}).model_dump(mode="json")
        import json

        if len(json.dumps(out, ensure_ascii=False)) > pp.MAX_REPORT_BYTES:
            raise RpcError(ErrorCode.PLUGIN_LIMIT, i18n._("报告超过 2 MiB"))
        return out

    async def cleanup_result(
        self, rec: PluginRecord, name: str, params: dict[str, Any], value: Any
    ) -> dict[str, Any]:
        """Proposals -> the review dialog's rows: resolved, sized, epochs checked."""
        if not isinstance(value, list):
            raise RpcError(ErrorCode.PLUGIN_ERROR, "a cleanup must return a list of proposals")
        scan_id = params.get("scan_id") or self.active_scan()
        store = self.store_of(str(scan_id)) if scan_id else None
        if store is None:
            raise RpcError(ErrorCode.UNKNOWN_SCAN, "no scan")
        epoch = int(getattr(store, "epoch", 0))
        items = value[:MAX_NODES]
        from ..extensions import EXTENSIONS

        c = EXTENSIONS.get("cleanup", f"{rec.id}.{name}")
        title = c.spec.title.get(i18n.language()) if c is not None else name

        def work() -> tuple[list[pp.CleanupItem], int]:
            out: list[pp.CleanupItem] = []
            dropped = 0
            n = len(store)
            seen: set[int] = set()
            for it in items:
                raw = it.get("node") if isinstance(it, dict) else it
                node: int | None = None
                if isinstance(raw, dict):
                    if int(raw.get("epoch", epoch)) == epoch and 0 < int(raw.get("node", -1)) < n:
                        node = int(raw["node"])
                    elif raw.get("path"):
                        node = query.find(store, str(raw["path"]))
                elif isinstance(raw, str):
                    node = query.find(store, raw)
                elif isinstance(raw, int) and 0 < raw < n:
                    node = raw
                if node is None or node in seen:
                    dropped += 1
                    continue
                seen.add(node)
                v = store.view(node)
                out.append(
                    pp.CleanupItem(
                        node=pp.NodeRef(scan_id=str(scan_id), node=node, epoch=epoch, path=store.path(node)),
                        name=v.name,
                        path=store.path(node),
                        alloc=v.tot_alloc,
                        size=v.tot_size,
                        reason=str(it.get("reason", "") if isinstance(it, dict) else "")[:120],
                        checked=bool(it.get("checked", True)) if isinstance(it, dict) else True,
                    )
                )
            return out, dropped

        rows, dropped = await compute(work)
        res = pp.CleanupResult(
            origin=f"{rec.id}.{name}",
            title=title,
            items=rows,
            freeable=sum(r.alloc for r in rows if r.checked),
            dropped=dropped,
        )
        return res.model_dump(mode="json")


__all__ = ["HostApi", "PluginCaller"]
