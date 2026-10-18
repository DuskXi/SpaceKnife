"""path.forms, basket.* and ops.* over the real WebSocket (W5).

Only files of this test's temporary folder are touched. Whatever goes to the real recycle bin is
put back by the test (undo, or the finally block when an assertion failed first); the recycle bin
is never emptied.
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path

import pytest

from spaceknife.server import Server, ops_service
from spaceknife_client.connection import Connection
from spaceknife_protocol import ops as po
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import (
    BASKET_ADD,
    BASKET_CLEAR,
    BASKET_EXECUTE,
    BASKET_LIST,
    EVENT_BASKET,
    EVENT_OPS_PROGRESS,
    OPS_UNDO,
    PATH_FORMS,
    SCAN_START,
    TREE_FIND,
)

from .test_scan_server import wait_done


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(ops_service, "DELETE_DELAY_S", 0.3)  # the undo window, shortened


async def scanned(client: Connection, root: Path) -> str:
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    await wait_done(client, started.scan_id)
    return started.scan_id


async def node_of(client: Connection, scan_id: str, path: Path) -> int:
    found = await client.call(TREE_FIND, p.TreeFindParams(scan_id=scan_id, path=str(path)))
    assert found.node is not None, path
    return found.node.id


class Ops:
    """Collects ops.progress events and waits for a phase."""

    def __init__(self, client: Connection) -> None:
        self.events: list[po.OpProgressEvent] = []
        self.baskets: list[po.BasketState] = []
        self.changed = asyncio.Event()
        client.on_event(EVENT_OPS_PROGRESS, self._on)
        client.on_event(EVENT_BASKET, lambda ev: self.baskets.append(ev.state))

    def _on(self, ev: po.OpProgressEvent) -> None:
        self.events.append(ev)
        self.changed.set()

    async def phase(self, task_id: str, *phases: str) -> po.OpProgressEvent:
        async def wait() -> po.OpProgressEvent:
            while True:
                for ev in reversed(self.events):
                    if ev.task_id == task_id and ev.phase in phases:
                        return ev
                self.changed.clear()
                await self.changed.wait()

        return await asyncio.wait_for(wait(), timeout=60)


async def test_path_forms(tmp_path: Path, client: Connection) -> None:
    root = tmp_path / "root"
    (root / "a b").mkdir(parents=True)
    (root / "a b" / "文件 1.txt").write_bytes(b"x")
    scan_id = await scanned(client, root)
    node = await node_of(client, scan_id, root / "a b" / "文件 1.txt")
    forms = await client.call(PATH_FORMS, po.PathFormsParams(scan_id=scan_id, node=node))
    assert forms.raw == str(root / "a b" / "文件 1.txt") and forms.on_disk
    assert forms.uri.startswith("file://") and "%20" in forms.uri and "%E6%96%87" in forms.uri
    assert " " in forms.quoted and forms.quoted[0] in "\"'"


async def test_trash_and_undo(tmp_path: Path, server: Server, client: Connection) -> None:
    root = tmp_path / "root"
    (root / "keep").mkdir(parents=True)
    victim = root / "victim.bin"
    victim.write_bytes(b"v" * 40_000)
    folder = root / "old"
    (folder / "sub").mkdir(parents=True)
    (folder / "sub" / "x.dat").write_bytes(b"x" * 10_000)
    scan_id = await scanned(client, root)
    ops = Ops(client)
    await client.call(BASKET_CLEAR, Empty())
    v = await node_of(client, scan_id, victim)
    f = await node_of(client, scan_id, folder)
    sub = await node_of(client, scan_id, folder / "sub")
    added = await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[sub]))
    assert added.added == 1
    added = await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[v, f]))
    assert added.added == 2 and added.absorbed == 1  # the folder took in its sub folder
    state = await client.call(BASKET_LIST, Empty())
    assert {i.name for i in state.items} == {"victim.bin", "old"}
    assert state.freeable >= 50_000 and state.volumes
    refused = await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[0]))
    assert refused.added == 0 and refused.refused

    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="trash"))
    try:
        done = await ops.phase(task.task_id, "done", "cancelled")
        if not all(r.ok for r in done.results):
            pytest.skip(f"no usable recycle bin here: {[r.message for r in done.results]}")
        assert not victim.exists() and not folder.exists()
        assert done.can_undo and done.freed >= 50_000 and done.mode == "trash"
        assert not (await client.call(BASKET_LIST, Empty())).items  # executed items left the basket

        await client.call(OPS_UNDO, po.OpTaskParams(task_id=task.task_id))
        await ops.phase(task.task_id, "undone")
        assert victim.read_bytes() == b"v" * 40_000
        assert (folder / "sub" / "x.dat").exists()
        back = await client.call(BASKET_LIST, Empty())
        assert {i.name for i in back.items} == {"victim.bin", "old"}  # undo puts them back in the basket
    finally:
        run = server.ops.runs.get(task.task_id)
        if run is not None and run.refs:  # an assertion failed before undo: put them back anyway
            from spaceknife.ops.trash import restore

            await asyncio.to_thread(restore, run.refs)
    await client.call(BASKET_CLEAR, Empty())


async def test_delete_waits_and_can_be_called_off(tmp_path: Path, client: Connection) -> None:
    root = tmp_path / "root"
    root.mkdir()
    doomed = root / "doomed.tmp"
    doomed.write_bytes(b"d" * 1000)
    scan_id = await scanned(client, root)
    ops = Ops(client)
    await client.call(BASKET_CLEAR, Empty())
    node = await node_of(client, scan_id, doomed)
    await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[node]))

    # called off inside the undo window: nothing touched, still in the basket
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    assert task.delay_s > 0
    waiting = await ops.phase(task.task_id, "waiting")
    assert waiting.can_undo
    await client.call(OPS_UNDO, po.OpTaskParams(task_id=task.task_id))
    await ops.phase(task.task_id, "undone")
    assert doomed.exists()
    assert len((await client.call(BASKET_LIST, Empty())).items) == 1

    # and for real
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    done = await ops.phase(task.task_id, "done")
    assert [r.code for r in done.results] == ["ok"] and not doomed.exists()
    assert not done.can_undo and done.mode == "delete"
    assert not (await client.call(BASKET_LIST, Empty())).items


async def test_tree_follows_without_monitoring(tmp_path: Path, server: Server, client: Connection) -> None:
    root = tmp_path / "root"
    root.mkdir()
    gone = root / "gone.tmp"
    gone.write_bytes(b"g" * 5000)
    (root / "stay.tmp").write_bytes(b"s")
    scan_id = await scanned(client, root)
    handle = server.scans.scans[scan_id]
    if handle.monitor is not None:
        await server.monitors.stop(scan_id)
    ops = Ops(client)
    await client.call(BASKET_CLEAR, Empty())
    node = await node_of(client, scan_id, gone)
    await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[node]))
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    await ops.phase(task.task_id, "done")
    for _ in range(100):  # the parent folder is read again
        if handle.store.find(str(gone)) is None:
            break
        await asyncio.sleep(0.05)
    assert handle.store.find(str(gone)) is None
    assert handle.store.find(str(root / "stay.tmp")) is not None
    assert os.path.exists(root / "stay.tmp")
