"""A :class:`~spaceknife_sdk.TreeView` over a store in this process, without a plugin host.

The code of the plugins shipped with the app (``plugins/builtin/``) runs where the store is — an
export on the export's own thread, the command line without a server — so its tree view answers
requests directly from the store: the same query, node and path code the host uses for plugin
processes (:mod:`.query`), the columns without a copy, and ``TreeView.store`` itself.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

import numpy as np

from spaceknife_protocol.errors import ErrorCode
from spaceknife_sdk._link import Image
from spaceknife_sdk.errors import HostError
from spaceknife_sdk.tree import TreeView

from ..layout.basis import Basis
from ..store.nodes import NodeStore
from . import query
from .runtime.base import RuntimeCallError

LOCAL_SCAN = "local"
MAX_NODES = 100_000


class StoreLink:
    """The SDK's link for a store in this process: requests are answered on the calling thread."""

    def __init__(self, store: NodeStore, scan_id: str = LOCAL_SCAN) -> None:
        self.store = store
        self.scan_id = scan_id
        self._basis: Basis | None = None

    @property
    def basis(self) -> Basis:
        if self._basis is None:
            self._basis = Basis(self.store)
        return self._basis

    def request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        frames: Sequence[bytes] = (),
        timeout: float | None = None,
    ) -> tuple[Any, list[bytes]]:
        store = self.store
        try:
            if method == "host.query":
                return query.run(store, self.basis, params)
            if method == "host.node":
                n = len(store)
                nodes = [int(x) for x in params.get("nodes", [])][:MAX_NODES]
                bad = [x for x in nodes if not 0 <= x < n]
                if bad:
                    raise HostError(ErrorCode.UNKNOWN_NODE, f"no node {bad[0]}")
                return {"nodes": [query.node_dict(store, self.basis, x) for x in nodes]}, []
            if method == "host.paths":
                n = len(store)
                what = params.get("what", "path")
                texts = [
                    "" if not 0 <= x < n else store.name(x) if what == "name" else store.path(x)
                    for x in (int(v) for v in params.get("nodes", [])[:MAX_NODES])
                ]
                return {"texts": texts}, []
            if method == "host.find":
                return {"node": query.find(store, str(params["path"])), "epoch": self.epoch}, []
        except RuntimeCallError as exc:
            raise HostError(exc.code, exc.message, exc.data) from None
        raise HostError(ErrorCode.METHOD_NOT_FOUND, f"{method} needs the plugin host")

    def notify(self, type: str, payload: dict[str, Any]) -> None:
        return None

    @property
    def epoch(self) -> int:
        return int(getattr(self.store, "epoch", 0))

    def image(self, scan_id: str, columns: Sequence[str]) -> Image:
        store = self.store
        cols: dict[str, np.ndarray] = {}
        for name in columns:
            if name == "cls":
                cols[name] = self.basis.cls_of(np.arange(len(store), dtype=np.int64))
            else:
                cols[name] = store.col(name)
        return Image(
            scan_id=scan_id, version=int(store.version), epoch=self.epoch, n=len(store), columns=cols
        )

    def store_of(self, scan_id: str) -> NodeStore | None:
        return self.store


def local_tree(store: NodeStore, scan_id: str = LOCAL_SCAN) -> TreeView:
    """A tree view of ``store`` for code running in this process (built-in plugins)."""
    link = StoreLink(store, scan_id)
    roots = list(store.roots())
    return TreeView(
        link,  # the Link protocol, plus store_of
        {
            "scan_id": scan_id,
            "epoch": link.epoch,
            "version": int(store.version),
            "n": len(store),
            "roots": roots,
            "root_paths": [store.name(r) for r in roots],
        },
    )


__all__ = ["LOCAL_SCAN", "StoreLink", "local_tree"]
