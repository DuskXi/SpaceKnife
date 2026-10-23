"""dupes.*: the duplicate-file search through the real server (docs/dupes.md §11, §8.5).

Searches run on temporary trees; deleting goes through the delete basket in ``delete`` mode with
a shortened undo window (only files the test created are touched).
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import numpy as np
import pytest

from spaceknife.server import Server, ops_service
from spaceknife_client.connection import Connection
from spaceknife_protocol import dupes as pd
from spaceknife_protocol import ops as po
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import (
    BASKET_ADD,
    BASKET_CLEAR,
    BASKET_EXECUTE,
    BASKET_LIST,
    DUPES_CLEAR,
    DUPES_FOLDER,
    DUPES_FOLDERS,
    DUPES_GROUP,
    DUPES_GROUPS,
    DUPES_KEEP,
    DUPES_LOCATE,
    DUPES_PLAN,
    DUPES_START,
    DUPES_STATUS,
    DUPES_TO_BASKET,
    EVENT_DUPES_PROGRESS,
    EVENT_OPS_PROGRESS,
    SCAN_START,
    TREE_FIND,
    TREE_NODE,
    VIEW_OPEN,
    VIEW_SET,
)

from .test_scan_server import wait_done

MIB = 1 << 20


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(ops_service, "DELETE_DELAY_S", 0.2)
    import spaceknife.dupes.engine as E

    monkeypatch.setattr(E.JobOptions, "__init__", _racy_free(E.JobOptions.__init__))


def _racy_free(init):  # type: ignore[no-untyped-def]
    def wrapped(self, *a, **k):  # type: ignore[no-untyped-def]
        init(self, *a, **k)
        self.racy_s = 0.0  # files written by the test a moment ago are trusted from the cache

    return wrapped


def blob(n: int, seed: int) -> bytes:
    return np.random.default_rng(seed).integers(0, 256, n, dtype=np.uint8).tobytes()


def tree(root: Path) -> dict[str, Path]:
    a = blob(2 * MIB, 1)
    files = {
        "keep": root / "photos" / "a.jpg",
        "copy": root / "backups" / "a.jpg",  # born after the keeper, or (same tick) a longer path
        "other": root / "other.bin",
    }
    for k in ("keep", "copy"):
        files[k].parent.mkdir(parents=True, exist_ok=True)
        files[k].write_bytes(a)
    files["other"].write_bytes(blob(2 * MIB, 2))
    return files


async def scanned(client: Connection, root: Path) -> str:
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    await wait_done(client, started.scan_id)
    return started.scan_id


async def node_of(client: Connection, scan_id: str, path: Path) -> int:
    found = await client.call(TREE_FIND, p.TreeFindParams(scan_id=scan_id, path=str(path)))
    assert found.node is not None, path
    return found.node.id


async def searched(client: Connection, scan_id: str, **scope: object) -> pd.DupesStatus:
    loop = asyncio.get_running_loop()
    done: asyncio.Future[pd.DupesStatus] = loop.create_future()

    def on(ev: pd.DupesStatus) -> None:
        if ev.state in ("done", "cancelled", "failed") and not done.done():
            done.set_result(ev)

    client.on_event(EVENT_DUPES_PROGRESS, on)
    st = await client.call(DUPES_START, pd.DupesStartParams(scope=pd.DupesScope(scan_ids=[scan_id], **scope)))  # type: ignore[arg-type]
    if st.state in ("done", "failed"):
        return st
    ev = await asyncio.wait_for(done, 60)
    assert ev.job_id == st.job_id
    return await client.call(DUPES_STATUS, pd.DupesJobParams(job_id=st.job_id))


async def test_plan_search_groups_and_hover(tmp_path: Path, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    plan = await client.call(DUPES_PLAN, pd.DupesScope(scan_ids=[scan_id]))
    assert plan.candidates == 3 and plan.groups == 1 and plan.excluded["unique"] == 0
    st = await searched(client, scan_id)
    assert st.state == "done" and st.groups == 1 and st.extra == 1
    assert st.funnel.confirmed_groups == 1
    page = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=st.job_id))
    assert page.total == 1
    g = page.items[0]
    roles = {m.path: m.role for m in g.members}
    assert roles == {str(files["keep"]): "keep", str(files["copy"]): "extra"}
    assert g.keep_why == "rule"
    detail = await client.call(DUPES_GROUP, pd.DupesGroupParams(job_id=st.job_id, group=g.id))
    assert detail.total_members == 2
    # hover card data
    n = await node_of(client, scan_id, files["copy"])
    info = await client.call(TREE_NODE, p.TreeNodeParams(scan_id=scan_id, node=n))
    assert info.dupe is not None and info.dupe.state == "extra" and info.dupe.copies == 2
    assert info.dupe.others[0].path == str(files["keep"])
    folder = await node_of(client, scan_id, files["copy"].parent)
    finfo = await client.call(TREE_NODE, p.TreeNodeParams(scan_id=scan_id, node=folder))
    assert finfo.dupe is not None and finfo.dupe.redundant > 0
    # the folder page and the folder's coverage
    folders = await client.call(DUPES_FOLDERS, pd.DupesFoldersParams(job_id=st.job_id, scan_id=scan_id))
    assert folders.items and folders.items[0].path == str(files["copy"].parent)
    cov = await client.call(
        DUPES_FOLDER, pd.DupesFolderParams(job_id=st.job_id, scan_id=scan_id, node=folder)
    )
    assert cov.elsewhere == 1 and cov.only_here == 0
    # keep rules: prefer the backup folder
    st2 = await client.call(
        DUPES_KEEP, pd.DupesKeepParams(job_id=st.job_id, prefer=[str(files["copy"].parent)])
    )
    assert st2.extra == 1
    page = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=st.job_id))
    roles = {m.path: m.role for m in page.items[0].members}
    assert roles[str(files["copy"])] == "keep"


async def test_the_dupes_colouring_and_locate(tmp_path: Path, server: Server, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    opened = await client.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    assert opened.state.has_dupes
    upd = await client.call(
        VIEW_SET,
        pv.ViewSetParams(view_id=opened.state.view_id, settings=pv.ViewSettingsParams(color_mode="dupes")),
    )
    assert upd.state.color_mode == "dupes"
    host = server.views.views[opened.state.view_id]
    frame = host.view.last
    assert frame is not None and frame.dup is not None and frame.dupg is not None
    names = list(frame.draw.names)
    i = names.index("a.jpg")
    assert int(frame.dupg[i]) > 0
    loc = await client.call(
        DUPES_LOCATE, pd.DupesLocateParams(job_id=st.job_id, view_id=opened.state.view_id, group=0)
    )
    assert len(loc.rows) == 2 and all(r.exact for r in loc.rows)
    await client.call(DUPES_CLEAR, pd.DupesJobParams(job_id=st.job_id))
    await asyncio.sleep(0.2)
    assert host.view.color_mode == "class"
    del files


class OpsWaiter:
    def __init__(self, client: Connection) -> None:
        self.events: list[po.OpProgressEvent] = []
        self.changed = asyncio.Event()
        client.on_event(EVENT_OPS_PROGRESS, self._on)

    def _on(self, ev: po.OpProgressEvent) -> None:
        self.events.append(ev)
        self.changed.set()

    async def done(self, task_id: str) -> po.OpProgressEvent:
        async def wait() -> po.OpProgressEvent:
            while True:
                for ev in reversed(self.events):
                    if ev.task_id == task_id and ev.phase in ("done", "cancelled"):
                        return ev
                self.changed.clear()
                await self.changed.wait()

        return await asyncio.wait_for(wait(), 60)


async def test_basket_deletes_the_extra_copy_and_keeps_the_keeper(tmp_path: Path, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    res = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    assert res.groups == 1 and res.members == 1 and len(res.items) == 1
    basket = await client.call(BASKET_LIST, Empty())
    item = basket.items[0]
    assert item.kind == "dupes" and item.members == 1 and item.status == "ready"
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    done = await waiter.done(task.task_id)
    assert [r.ok for r in done.results] == [True]
    assert not files["copy"].exists() and files["keep"].exists()


async def test_a_changed_keeper_stops_the_delete(tmp_path: Path, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    with open(files["keep"], "r+b") as f:  # noqa: ASYNC230 - the keeper is edited after the search
        f.write(b"edited")
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    done = await waiter.done(task.task_id)
    assert [r.code for r in done.results] == ["keeper_changed"]
    assert files["copy"].exists()


async def test_a_changed_copy_is_not_deleted(tmp_path: Path, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    files["copy"].write_bytes(blob(2 * MIB, 9))
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    done = await waiter.done(task.task_id)
    assert [r.code for r in done.results] == ["changed"]
    assert files["copy"].exists()


async def test_the_keepers_folder_in_the_basket_blocks_the_group(tmp_path: Path, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    folder = await node_of(client, scan_id, files["keep"].parent)
    await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[folder]))
    basket = await client.call(BASKET_LIST, Empty())
    dup = next(i for i in basket.items if i.kind == "dupes")
    assert dup.status == "blocked" and dup.note
    # executing only the dupes item: the copy stays (its keeper is to be deleted too)
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete", ids=[dup.id]))
    done = await waiter.done(task.task_id)
    assert [r.code for r in done.results] == ["last_copy"]
    assert files["copy"].exists() and files["keep"].exists()


async def test_a_keeper_already_in_the_basket_is_refused(tmp_path: Path, client: Connection) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    keep = await node_of(client, scan_id, files["keep"])
    await client.call(BASKET_ADD, po.BasketAddParams(scan_id=scan_id, nodes=[keep]))
    res = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    assert res.members == 0 and res.refused


async def test_only_finished_live_scans_and_one_search_at_a_time(tmp_path: Path, client: Connection) -> None:
    tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    with pytest.raises(Exception, match="没有这个扫描"):
        await client.call(DUPES_PLAN, pd.DupesScope(scan_ids=["scan-nope"]))
    st = await searched(client, scan_id)
    assert st.state == "done"
