"""STRUCTURE (store/lock.py): a change excludes every reader, and a long reader holds up the
changes but not the other readers.

A cleanup plan built over a large scan read the tree for minutes; a file change on the monitored
disk then waited for it, and since a waiting writer used to stop every new reader, every picture,
summary and view of every scan waited too: a new scan of another disk started but its tab never
showed until the plan was built.
"""

from __future__ import annotations

import asyncio
import threading
import time
from pathlib import Path
from typing import Any

import pytest

from spaceknife.store.lock import STRUCTURE, StructureLock
from spaceknife_client.connection import Connection
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import SCAN_START, VIEW_OPEN

from .test_scan_server import wait_done


class Reader:
    """A reader on its own thread: started, held until released."""

    def __init__(self, lock: StructureLock, log: list[tuple[str, str]], name: str) -> None:
        self.started = threading.Event()
        self.release = threading.Event()

        def run() -> None:
            with lock.read():
                log.append(("start", name))
                self.started.set()
                self.release.wait(20)
                log.append(("end", name))

        self.thread = threading.Thread(target=run, daemon=True, name=f"reader-{name}")
        self.thread.start()

    async def wait_started(self, seconds: float) -> bool:
        return await asyncio.to_thread(self.started.wait, seconds)

    async def finish(self) -> None:
        self.release.set()
        await asyncio.to_thread(self.thread.join, 10)


async def _writer(lock: StructureLock, log: list[tuple[str, str]]) -> None:
    async with lock.write():
        assert lock.readers == 0
        log.append(("write", "w"))


async def test_a_long_reader_holds_up_the_change_not_the_other_readers() -> None:
    lock = StructureLock()
    log: list[tuple[str, str]] = []
    old = Reader(lock, log, "old")
    assert await old.wait_started(5)
    w = asyncio.create_task(_writer(lock, log))
    await asyncio.sleep(0.05)
    assert not w.done()  # the change waits for the reader that was there first
    new = Reader(lock, log, "new")
    assert await new.wait_started(2), (
        "a new reader waited for a change that itself waited for an older reader"
    )
    await new.finish()
    await asyncio.sleep(0.05)
    assert not w.done()
    await old.finish()
    await asyncio.wait_for(w, 5)
    assert log[-1] == ("write", "w")


async def test_once_the_older_readers_are_done_new_readers_wait_for_the_change() -> None:
    lock = StructureLock()
    log: list[tuple[str, str]] = []
    old = Reader(lock, log, "old")
    assert await old.wait_started(5)
    w = asyncio.create_task(_writer(lock, log))
    await asyncio.sleep(0.05)
    mid = Reader(lock, log, "mid")  # admitted: the change cannot start before "old" anyway
    assert await mid.wait_started(2)
    await old.finish()
    late = Reader(lock, log, "late")  # nothing older than the change runs: it waits now
    assert not await late.wait_started(0.3)
    await mid.finish()
    await asyncio.wait_for(w, 5)  # no stream of readers keeps the change out
    assert await late.wait_started(5)
    await late.finish()
    assert log.index(("write", "w")) < log.index(("start", "late"))


async def test_readers_held_back_go_on_when_the_waiting_change_is_given_up() -> None:
    lock = StructureLock()
    log: list[tuple[str, str]] = []
    old = Reader(lock, log, "old")
    assert await old.wait_started(5)
    w = asyncio.create_task(_writer(lock, log))
    await asyncio.sleep(0.05)
    mid = Reader(lock, log, "mid")
    assert await mid.wait_started(2)
    await old.finish()
    late = Reader(lock, log, "late")
    assert not await late.wait_started(0.3)
    w.cancel()
    with pytest.raises(asyncio.CancelledError):
        await w
    assert await late.wait_started(2), "a reader stayed blocked by a change that was given up"
    await mid.finish()
    await late.finish()
    assert ("write", "w") not in log


async def test_no_reader_ever_runs_during_a_change() -> None:
    lock = StructureLock()
    stop = threading.Event()
    seen: list[str] = []

    def reader() -> None:
        while not stop.is_set():
            with lock.read():
                if lock._writing:
                    seen.append("reader saw a change running")
                time.sleep(0.0005)

    threads = [threading.Thread(target=reader, daemon=True) for _ in range(6)]
    for t in threads:
        t.start()
    try:
        for _ in range(200):
            async with lock.write():
                if lock.readers:
                    seen.append("change ran beside a reader")
            await asyncio.sleep(0)
    finally:
        stop.set()
        for t in threads:
            await asyncio.to_thread(t.join, 5)
    assert not seen


async def test_a_view_opens_while_a_long_read_holds_up_a_change(tmp_path: Path, client: Connection) -> None:
    """Through the server: a reader holding the tree (a plan being built) and a change waiting for
    it no longer keep another scan's view from opening."""
    folder = tmp_path / "other"
    (folder / "a").mkdir(parents=True)
    (folder / "a" / "f.bin").write_bytes(b"x" * 5000)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(folder)], workers=0))
    await wait_done(client, started.scan_id)
    log: list[tuple[str, str]] = []
    long = Reader(STRUCTURE, log, "plan")
    assert await long.wait_started(5)
    change = asyncio.create_task(_writer(STRUCTURE, log))
    try:
        await asyncio.sleep(0.05)
        assert not change.done()
        t0 = time.monotonic()
        opened: Any = await asyncio.wait_for(
            client.call(
                VIEW_OPEN,
                pv.ViewOpenParams(scan_id=started.scan_id, viewport=pv.Viewport(width=800, height=600)),
            ),
            10,
        )
        took = time.monotonic() - t0
        assert opened.frame is not None
        assert took < 2.0, f"view.open waited {took:.1f} s behind a change waiting for a long reader"
        assert not change.done()
    finally:
        await long.finish()
        await asyncio.wait_for(change, 5)
