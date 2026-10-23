"""dupes.*: the duplicate-file search through the real server (docs/dupes.md §11, §8.5).

Searches run on temporary trees; deleting goes through the delete basket in ``delete`` mode with
a shortened undo window (only files the test created are touched).
"""

from __future__ import annotations

import asyncio
import os
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
    EVENT_DUPES_CHANGED,
    EVENT_DUPES_PROGRESS,
    EVENT_OPS_PROGRESS,
    OPS_UNDO,
    SCAN_CLOSE,
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


async def test_the_plan_picks_up_a_search_and_the_status_names_its_scope(
    tmp_path: Path, client: Connection
) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    plan = await client.call(DUPES_PLAN, pd.DupesScope(scan_ids=[scan_id]))
    assert plan.current is None
    st = await searched(client, scan_id, min_size=MIB)
    assert st.mode == "all" and st.node is None and st.min_size == MIB
    # an interface that reconnects finds the search through the plan
    plan = await client.call(DUPES_PLAN, pd.DupesScope(scan_ids=[scan_id]))
    assert plan.current is not None and plan.current.job_id == st.job_id and plan.current.state == "done"
    assert plan.last is not None and plan.last.groups == 1
    folder = await node_of(client, scan_id, files["copy"].parent)
    st2 = await searched(client, scan_id, mode="against", node=folder)
    assert st2.mode == "against" and st2.node == folder and st2.job_id != st.job_id
    cov = await client.call(
        DUPES_FOLDER, pd.DupesFolderParams(job_id=st2.job_id, scan_id=scan_id, node=folder)
    )
    assert cov.elsewhere == 1


async def test_a_copy_the_monitor_saw_change_leaves_its_group(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    changed: list[pd.DupesChangedEvent] = []
    client.on_event(EVENT_DUPES_CHANGED, changed.append)
    n = await node_of(client, scan_id, files["copy"])
    files["copy"].write_bytes(blob(2 * MIB, 5))
    server.views.flash(scan_id, {n: 1})  # what the live monitor reports for a changed file
    for _ in range(50):
        if changed:
            break
        await asyncio.sleep(0.05)
    assert changed and changed[-1].reason == "invalidated"
    page = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=st.job_id))
    assert page.total == 0  # back to 「待核对」: never offered for deleting
    now = await client.call(DUPES_STATUS, pd.DupesJobParams(job_id=st.job_id))
    assert now.extra == 0 and now.reclaim == 0 and now.version > st.version and now.stale_groups == 1
    info = await client.call(TREE_NODE, p.TreeNodeParams(scan_id=scan_id, node=n))
    assert info.dupe is None or info.dupe.state not in ("extra", "keep")
    await client.call(BASKET_CLEAR, Empty())
    res = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    assert res.members == 0


async def test_two_searches_that_keep_each_others_copies_delete_nothing(
    tmp_path: Path, client: Connection
) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    # the same group with the other copy kept goes in as well: each item keeps what the other deletes
    keep = str(files["copy"])
    await client.call(
        DUPES_KEEP,
        pd.DupesKeepParams(job_id=st.job_id, overrides=[pd.DupesOverride(group=0, path=keep, role="keep")]),
    )
    res = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    # refused: the copy this choice keeps is what the basket already deletes; nothing counted
    assert res.members == 0 and res.groups == 0 and res.freed == 0 and [r.group for r in res.refused] == [0]
    basket = await client.call(BASKET_LIST, Empty())
    assert len([i for i in basket.items if i.kind == "dupes"]) == 1 and basket.items[0].members == 1
    # and the first item, executed now, still checks its keeper (which the user wants kept anyway)
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    done = await waiter.done(task.task_id)
    assert [r.ok for r in done.results] == [True]
    assert files["keep"].exists() and not files["copy"].exists()


async def test_undo_brings_the_item_back_with_its_guards(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    files = tree(tmp_path / "t")
    scan_id = await scanned(client, tmp_path / "t")
    st = await searched(client, scan_id)
    await client.call(BASKET_CLEAR, Empty())
    await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="trash"))
    try:
        done = await waiter.done(task.task_id)
        if not all(r.ok for r in done.results):
            pytest.skip(f"no usable recycle bin here: {[r.message for r in done.results]}")
        assert not files["copy"].exists()
        await client.call(OPS_UNDO, po.OpTaskParams(task_id=task.task_id))
        for _ in range(100):
            if files["copy"].exists() and (await client.call(BASKET_LIST, Empty())).items:
                break
            await asyncio.sleep(0.05)
        assert files["copy"].exists()
        back = await client.call(BASKET_LIST, Empty())
        assert [i.kind for i in back.items] == ["dupes"] and back.items[0].members == 1
    finally:
        run = server.ops.runs.get(task.task_id)
        if run is not None and run.refs and not files["copy"].exists():
            from spaceknife.ops.trash import restore

            await asyncio.to_thread(restore, run.refs)
    # the guard came back with it: moving the copy back changed its change time, so it is not
    # deleted again without a new search (a guard-less item would delete it)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    done = await waiter.done(task.task_id)
    assert [r.code for r in done.results] == ["changed"]
    assert files["copy"].exists() and files["keep"].exists()


# --------------------------------------------------------------------------- review fixes


@pytest.mark.skipif(os.name == "nt", reason="a symlinked folder stands for SUBST / bind mounts")
async def test_two_searches_over_two_paths_of_one_folder_keep_a_copy(
    tmp_path: Path, client: Connection
) -> None:
    """The same folder scanned by two paths, searched twice with different keepers: the guard and
    the basket compare identities, so the content is never deleted twice."""
    data = tmp_path / "data"
    files = tree(data)
    alias = tmp_path / "alias"
    alias.symlink_to(data, target_is_directory=True)
    s1 = await scanned(client, data)
    s2 = await scanned(client, alias)
    await client.call(BASKET_CLEAR, Empty())
    st1 = await searched(client, s1)
    r1 = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st1.job_id))
    assert r1.members == 1
    st2 = await searched(client, s2)
    page = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=st2.job_id))
    other = next(m.path for m in page.items[0].members if m.role == "extra")
    await client.call(
        DUPES_KEEP,
        pd.DupesKeepParams(job_id=st2.job_id, overrides=[pd.DupesOverride(group=0, path=other, role="keep")]),
    )
    r2 = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st2.job_id))
    assert r2.members == 0 and r2.refused  # its keeper is, by identity, the extra copy already in
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    await waiter.done(task.task_id)
    assert files["keep"].exists() or files["copy"].exists()


@pytest.mark.skipif(os.name == "nt", reason="symlinks")
def test_the_guard_compares_identities_not_only_paths(tmp_path: Path) -> None:
    from spaceknife.dupes.guard import Elsewhere, Guard, Stamped, check
    from spaceknife.dupes.reads import default_ops

    def stamped(path: Path) -> Stamped:
        r = default_ops().identity([str(path)], True)
        return Stamped(
            path=str(path),
            dev=int(r["dev"][0]),
            fid_lo=int(r["fid_lo"][0]),
            fid_hi=int(r["fid_hi"][0]),
            size=int(r["size"][0]),
            mtime=int(r["mtime"][0]),
            change=int(r["change"][0]),
            usn=int(r["usn"][0]),
        )

    data = tmp_path / "data"
    (data / "sub").mkdir(parents=True)
    (data / "a.bin").write_bytes(b"x" * 4096)
    (data / "b.bin").write_bytes(b"x" * 4096)
    (data / "sub" / "c.bin").write_bytes(b"x" * 4096)
    alias = tmp_path / "alias"
    alias.symlink_to(data, target_is_directory=True)
    ops = default_ops().name
    # search 1 keeps a and deletes b; search 2 (by the other path) keeps b and deletes a
    g1 = Guard(member=stamped(data / "b.bin"), keepers=(stamped(data / "a.bin"),), ops=ops)
    g2 = Guard(member=stamped(alias / "a.bin"), keepers=(stamped(alias / "b.bin"),), ops=ops)
    doomed = [str(data / "b.bin"), str(alias / "a.bin")]
    failed = check(doomed, {doomed[0]: g1, doomed[1]: g2})
    assert {code for code, _ in failed.values()} == {"last_copy"}
    # a keeper below a folder the rest of the basket deletes by another path
    g3 = Guard(member=stamped(data / "a.bin"), keepers=(stamped(alias / "sub" / "c.bin"),), ops=ops)
    rest = Elsewhere(covers=lambda p: False, members=set, plain=lambda: [str(data / "sub")])
    failed = check([str(data / "a.bin")], {str(data / "a.bin"): g3}, also_doomed=rest)
    assert failed[0][0] == "last_copy"
    # the extra copies of another dupes item, by identity
    rest = Elsewhere(covers=lambda p: False, members=lambda: {g3.keepers[0].ident}, plain=list)
    assert check([str(data / "a.bin")], {str(data / "a.bin"): g3}, also_doomed=rest)[0][0] == "last_copy"
    # nothing else goes: it passes, the content read again (same reads as the job) still matches
    g4 = Guard(
        member=stamped(data / "a.bin"), keepers=(stamped(data / "b.bin"),), ops=ops, recheck_content=True
    )
    atime = [os.stat(data / n).st_atime_ns for n in ("a.bin", "b.bin")]
    assert check([str(data / "a.bin")], {str(data / "a.bin"): g4}) == {}
    if os.name != "nt":
        assert [os.stat(data / n).st_atime_ns for n in ("a.bin", "b.bin")] == atime


async def test_closing_the_scan_drops_its_search(tmp_path: Path, server: Server, client: Connection) -> None:
    tree(tmp_path / "t")
    sid = await scanned(client, tmp_path / "t")
    st = await searched(client, sid)
    changed: list[pd.DupesChangedEvent] = []
    client.on_event(EVENT_DUPES_CHANGED, changed.append)
    await client.call(SCAN_CLOSE, p.ScanIdParams(scan_id=sid))
    assert st.job_id not in server.dupes.jobs  # its store is no longer held
    for _ in range(20):
        if changed:
            break
        await asyncio.sleep(0.05)
    assert changed and changed[-1].reason == "cleared"


async def test_an_evicted_scan_takes_its_running_search_along(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    root = tmp_path / "t"
    for i in range(4):
        a = blob(16 * MIB, 90 + i)
        (root / f"x{i}").mkdir(parents=True)
        (root / f"x{i}" / "a.bin").write_bytes(a)
        (root / f"x{i}" / "b.bin").write_bytes(a)
    sid = await scanned(client, root)
    st = await client.call(DUPES_START, pd.DupesStartParams(scope=pd.DupesScope(scan_ids=[sid]), load="max"))
    h = server.dupes.jobs[st.job_id]
    server.views._evicted(sid)  # what MAX_SCANS eviction calls
    for _ in range(100):
        if st.job_id not in server.dupes.jobs and not h.running:
            break
        await asyncio.sleep(0.05)
    assert st.job_id not in server.dupes.jobs and not h.running
    assert h.job.open_handles() == 0


async def test_manual_keeps_pile_up_and_outlive_rule_changes_and_reruns(
    tmp_path: Path, client: Connection
) -> None:
    root = tmp_path / "t"
    files = tree(root)
    data = bytes(reversed(files["keep"].read_bytes()))
    (root / "photos" / "b.jpg").write_bytes(data)
    (root / "backups" / "b.jpg").write_bytes(data)
    sid = await scanned(client, root)
    st = await searched(client, sid)
    page = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=st.job_id))
    assert page.total == 2
    picks = [(g.id, next(m.path for m in g.members if m.role == "extra")) for g in page.items]

    async def roles(job_id: str) -> dict[str, str]:
        pg = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=job_id))
        return {m.path: m.role for it in pg.items for m in it.members}

    for gid, path in picks:  # what the drawer sends for 「保留这一份」, once per group
        await client.call(
            DUPES_KEEP,
            pd.DupesKeepParams(
                job_id=st.job_id, overrides=[pd.DupesOverride(group=gid, path=path, role="keep")]
            ),
        )
    got = await roles(st.job_id)
    assert all(got[path] == "keep" for _, path in picks)
    # another rule (the drawer sends no overrides): the manual choices stay
    await client.call(DUPES_KEEP, pd.DupesKeepParams(job_id=st.job_id, rule="newest"))
    got = await roles(st.job_id)
    assert all(got[path] == "keep" for _, path in picks)
    # 「再查一次」: the new search of the same scan starts with them
    st2 = await searched(client, sid)
    got = await roles(st2.job_id)
    assert all(got[path] == "keep" for _, path in picks)
    # forgotten on request
    await client.call(DUPES_KEEP, pd.DupesKeepParams(job_id=st2.job_id, reset_overrides=True))
    got = await roles(st2.job_id)
    assert any(got[path] == "extra" for _, path in picks)


@pytest.mark.skipif(os.name == "nt", reason="symlinks")
async def test_one_cross_search_over_two_paths_of_one_folder(tmp_path: Path, client: Connection) -> None:
    """Overlapping scans: each file is seen twice but is one single-named file; the real copy can
    still go (once), the content stays."""
    data = tmp_path / "data"
    files = tree(data)
    alias = tmp_path / "alias"
    alias.symlink_to(data, target_is_directory=True)
    s1 = await scanned(client, data)
    s2 = await scanned(client, alias)
    await client.call(BASKET_CLEAR, Empty())
    loop = asyncio.get_running_loop()
    done: asyncio.Future[pd.DupesStatus] = loop.create_future()
    client.on_event(
        EVENT_DUPES_PROGRESS, lambda ev: ev.state == "done" and not done.done() and done.set_result(ev)
    )
    st = await client.call(
        DUPES_START, pd.DupesStartParams(scope=pd.DupesScope(scan_ids=[s1, s2], cross_volume=True))
    )
    if st.state != "done":
        await asyncio.wait_for(done, 30)
    st = await client.call(DUPES_STATUS, pd.DupesJobParams(job_id=st.job_id))
    assert st.extra == 1 and st.groups == 1
    page = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=st.job_id))
    views = [m for m in page.items[0].members if m.view]
    assert len(views) == 2  # each file's second path
    r = await client.call(DUPES_TO_BASKET, pd.DupesBasketParams(job_id=st.job_id))
    assert r.members == 1
    waiter = OpsWaiter(client)
    task = await client.call(BASKET_EXECUTE, po.BasketExecuteParams(mode="delete"))
    await waiter.done(task.task_id)
    assert files["keep"].exists() != files["copy"].exists()  # one went, one stayed


async def test_the_folders_page_lists_deepest_folders_with_their_twin(
    tmp_path: Path, client: Connection
) -> None:
    root = tmp_path / "t"
    for i in range(3):
        a = blob(MIB + i, 120 + i)
        (root / "photos" / "2021").mkdir(parents=True, exist_ok=True)
        (root / "backup" / "phone" / "dcim").mkdir(parents=True, exist_ok=True)
        (root / "photos" / "2021" / f"{i}.jpg").write_bytes(a)
        (root / "backup" / "phone" / "dcim" / f"{i}.jpg").write_bytes(a)
    sid = await scanned(client, root)
    st = await searched(client, sid)
    page = await client.call(DUPES_FOLDERS, pd.DupesFoldersParams(job_id=st.job_id, scan_id=sid))
    assert page.total == 1  # not backup and backup/phone above it
    row = page.items[0]
    extra_side = "dcim" if row.path.endswith("dcim") else "2021"
    twin_side = "2021" if extra_side == "dcim" else "dcim"
    assert row.twin.endswith(twin_side) and row.twin_bytes == row.redundant and row.twin_node > 0


async def test_a_stopped_search_says_so(tmp_path: Path, client: Connection) -> None:
    root = tmp_path / "t"
    for i in range(4):
        a = blob(16 * MIB, 140 + i)
        (root / f"x{i}").mkdir(parents=True)
        (root / f"x{i}" / "a.bin").write_bytes(a)
        (root / f"x{i}" / "b.bin").write_bytes(a)
    sid = await scanned(client, root)
    plan = await client.call(DUPES_PLAN, pd.DupesScope(scan_ids=[sid]))
    assert plan.min_size == MIB and [s.scan_id for s in plan.scans] == [sid] and plan.disks
    loop = asyncio.get_running_loop()
    done: asyncio.Future[pd.DupesStatus] = loop.create_future()
    client.on_event(
        EVENT_DUPES_PROGRESS,
        lambda ev: ev.state in ("done", "cancelled") and not done.done() and done.set_result(ev),
    )
    await client.call(
        DUPES_START, pd.DupesStartParams(scope=pd.DupesScope(scan_ids=[sid]), budget_bytes=4 * MIB)
    )
    ev = await asyncio.wait_for(done, 30)
    assert ev.stop_reason == "budget" and ev.undecided > 0 and ev.error == ""
