"""Memory watchdog: when the backend holds far more than its scans account for, say what holds it.

Every ``PERIOD`` seconds the private memory of the process is compared with what the scans need
(node stores and name indexes). Beyond ``2 × accounted + 1.5 GiB`` a warning goes to the log
panel and a breakdown to the log file: scans, views, compute pool, session outboxes and — with
``SPACEKNIFE_MEMTRACE=1`` (tracemalloc, one frame) — the source lines holding the most memory.
Reports repeat only after another 25 % of growth.
"""

from __future__ import annotations

import asyncio
import os
import threading
import tracemalloc
from typing import TYPE_CHECKING

from loguru import logger

from ..events import emit
from ..filter.names import existing_index
from ..i18n import tr
from ..memory import process_memory
from . import compute

if TYPE_CHECKING:
    from .app import Server

PERIOD = 15.0
SLACK = 3 << 29  # 1.5 GiB
GiB = float(1 << 30)


def accounted(server: Server) -> int:
    """Private bytes the open scans need: node stores in plain memory (with growth headroom; mapped
    columns are file-backed, not private) and name indexes."""
    total = 0
    for handle in list(server.scans.scans.values()):
        store = handle.store
        total += store.ram_bytes() + handle.transient
        index = existing_index(store)
        if index is not None:
            total += index.memory_bytes()
    return total


def report(server: Server) -> str:
    mem = process_memory()
    lines = [
        f"private {(mem.private or 0) / GiB:.2f} GiB, resident {(mem.resident or 0) / GiB:.2f} GiB, "
        f"accounted {accounted(server) / GiB:.2f} GiB, threads {threading.active_count()}"
    ]
    for handle in list(server.scans.scans.values()):
        store = handle.store
        cols = sum(a.nbytes for a in store.cols.values())
        index = existing_index(store)
        lines.append(
            f"  scan {handle.id}: {len(store):,} nodes, columns {cols / 2**20:.0f} MB"
            f"{' (mapped)' if store.mapped else ''}, "
            f"names {store.names_size() / 2**20:.0f} MB, "
            f"name index {(index.memory_bytes() if index else 0) / 2**20:.0f} MB"
        )
    lines.append(f"  views {len(server.views.views)}, compute {compute.stats()}")
    for session in list(server.sessions):
        count, size = session.outbox_stats()
        lines.append(f"  session {session.id}: outbox {count} messages, {size / 2**20:.1f} MB")
    if tracemalloc.is_tracing():
        snap = tracemalloc.take_snapshot()
        lines.append("  largest allocation sites:")
        for st in snap.statistics("lineno")[:12]:
            lines.append(f"    {st.size / 2**20:9.1f} MB {st.count:8d} blocks  {st.traceback[0]}")
    return "\n".join(lines)


async def watch(server: Server) -> None:
    last = 0
    while True:
        await asyncio.sleep(PERIOD)
        try:
            private = process_memory().private
            if private is None:
                continue
            limit = 2 * accounted(server) + SLACK
            if private <= limit or private <= last * 1.25:
                continue
            last = private
            text = await asyncio.to_thread(report, server) if tracemalloc.is_tracing() else report(server)
            logger.warning("memory above what the scans account for:\n{}", text)
            emit(
                "system",
                tr(
                    "后端内存占用异常：{used} GB（扫描数据约 {scans} GB），诊断信息已写入日志文件",
                    used=f"{private / GiB:.1f}",
                    scans=f"{accounted(server) / GiB:.1f}",
                ),
                level="WARNING",
            )
        except Exception:  # diagnostics never take the backend down
            logger.exception("memory watchdog failed")


def install(server: Server) -> None:
    if os.environ.get("SPACEKNIFE_MEMTRACE"):
        tracemalloc.start(1)

    def start() -> None:
        task = asyncio.get_running_loop().create_task(watch(server), name="memory-watchdog")
        server.background.append(task)

    server.on_start.append(start)


__all__ = ["accounted", "install", "report", "watch"]
