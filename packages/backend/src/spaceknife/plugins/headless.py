"""The enabled plugins without a window: ``spaceknife run … export <plugin format>`` and ``run help``.

A plugin host on a loop of its own (a thread): it reads the trust file of the data folder as the app
does, registers what the enabled plugins add, and starts their code when a format is used. The
stores of ``run`` are given to it as scans, so a plugin's format reads them like any other.
"""

from __future__ import annotations

import asyncio
import secrets
import threading
from typing import Any

from loguru import logger

START_TIMEOUT_S = 60.0


class Headless:
    def __init__(self) -> None:
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self.loop.run_forever, name="sk-run-plugins", daemon=True)
        self.server: Any = None
        self._scans = 0

    def _run(self, coro: Any, timeout: float = START_TIMEOUT_S) -> Any:
        return asyncio.run_coroutine_threadsafe(coro, self.loop).result(timeout)

    def start(self) -> Headless:
        self.thread.start()
        self._run(self._start())
        return self

    async def _start(self) -> None:
        from ..server.app import Server, ServerConfig

        self.server = Server(ServerConfig(token=secrets.token_hex(16), hello_timeout=1.0, ping_interval=None))
        await self.server.plugins.start()

    def add_store(self, store: Any) -> str:
        """``store`` as an open scan (a plugin's ``ctx.tree`` reads it); returns its scan id."""
        return str(self._run(self._add(store)))

    async def _add(self, store: Any) -> str:
        from ..scan.scanner import RootInfo, ScanProgress, ScanState
        from ..server.scans import ScanHandle

        for sid, h in self.server.scans.scans.items():
            if h.store is store:
                return str(sid)
        self._scans += 1
        sid = f"scan-run{self._scans}"
        self.server.scans.scans[sid] = ScanHandle(
            id=sid,
            store=store,
            roots=[
                RootInfo(path=store.name(r), node=r, volume=None, whole_volume=False) for r in store.roots()
            ],
            source="snapshot",
            saved_progress=ScanProgress(
                state=ScanState.DONE,
                items=len(store) - 1,
                files=int(store.cols["tot_files"][0]),
                dirs=int(store.cols["tot_dirs"][0]),
                bytes_alloc=int(store.cols["tot_alloc"][0]),
                bytes_size=int(store.cols["tot_size"][0]),
                errors=0,
                pending_dirs=0,
                current_path="",
                elapsed_s=0.0,
                rate=0.0,
                progress=1.0,
                started_at=None,
                finished_at=None,
            ),
        )
        await self.server.plugins.push_update({"roots": self.server.plugins.api.scan_roots()})
        return sid

    def close(self) -> None:
        if self.server is not None:
            try:
                self._run(self._close(), 30.0)
            except Exception:
                logger.exception("stopping the plugins failed")
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(5.0)

    async def _close(self) -> None:
        await self.server.plugins.close()
        await self.server.scans.close()


def has_enabled_plugins() -> bool:
    """Any plugin enabled in the data folder (besides the ones shipped with the app)?"""
    from .store import TrustStore

    try:
        trust = TrustStore()
        return any(e.enabled for e in trust.doc.plugins.values()) or bool(trust.doc.dev)
    except Exception:
        return False


__all__ = ["Headless", "has_enabled_plugins"]
