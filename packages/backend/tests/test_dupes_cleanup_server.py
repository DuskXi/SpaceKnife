"""dupes.cleanup_*: the cleanup plan through the real server (docs/dupes.md §16).

Every test works on files it made in its own temporary folder, with the cleanup's sandbox set to
that folder (I13): nothing outside it can be touched. The search runs first, then the plan: edits,
risks and their acknowledgement, approval, the run, undo and purge.
"""

from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

import numpy as np
import pytest

from spaceknife_client.connection import Connection
from spaceknife_client.connection import RemoteError as RpcError
from spaceknife_protocol import dupes as pd
from spaceknife_protocol import dupes_cleanup as pc
from spaceknife_protocol import ops as po
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import (
    BASKET_ADD,
    BASKET_CLEAR,
    DUPES_CLEANUP_APPROVE,
    DUPES_CLEANUP_EDIT,
    DUPES_CLEANUP_EXECUTE,
    DUPES_CLEANUP_GROUP,
    DUPES_CLEANUP_GROUPS,
    DUPES_CLEANUP_OPEN,
    DUPES_CLEANUP_PREVIEW,
    DUPES_CLEANUP_PURGE,
    DUPES_CLEANUP_PURGE_PLAN,
    DUPES_CLEANUP_RUN,
    DUPES_CLEANUP_RUNS,
    DUPES_CLEANUP_STATE,
    DUPES_CLEANUP_UNDO,
    DUPES_CLEANUP_UNDO_EDIT,
    DUPES_GROUPS,
    DUPES_START,
    DUPES_STATUS,
    EVENT_DUPES_CLEANUP_PROGRESS,
    EVENT_DUPES_PROGRESS,
    SCAN_START,
    TREE_FIND,
)

from .test_scan_server import wait_done

MIB = 1 << 20
POSIX = os.name != "nt"


@pytest.fixture(autouse=True)
def sandboxed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("SPACEKNIFE_CLEANUP_SANDBOX", str(tmp_path))
    monkeypatch.delenv("SPACEKNIFE_CLEANUP_FAULT", raising=False)
    import spaceknife.dupes.engine as E
    import spaceknife.server.dupes_cleanup as M

    monkeypatch.setattr(M, "PURGE_DELAY_S", 0.2)
    init = E.JobOptions.__init__

    def racy_free(self, *a, **k):  # type: ignore[no-untyped-def]
        init(self, *a, **k)
        self.racy_s = 0.0

    monkeypatch.setattr(E.JobOptions, "__init__", racy_free)


def blob(n: int, seed: int) -> bytes:
    return np.random.default_rng(seed).integers(0, 256, n, dtype=np.uint8).tobytes()


def make(root: Path, files: dict[str, bytes]) -> dict[str, Path]:
    out = {}
    for rel, data in files.items():
        f = root / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(data)
        out[rel] = f
    return out


async def scanned(client: Connection, root: Path) -> str:
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    await wait_done(client, started.scan_id)
    return started.scan_id


async def searched(client: Connection, scan_id: str) -> pd.DupesStatus:
    loop = asyncio.get_running_loop()
    done: asyncio.Future[pd.DupesStatus] = loop.create_future()

    def on(ev: pd.DupesStatus) -> None:
        if ev.state in ("done", "cancelled", "failed") and not done.done():
            done.set_result(ev)

    client.on_event(EVENT_DUPES_PROGRESS, on)
    st = await client.call(DUPES_START, pd.DupesStartParams(scope=pd.DupesScope(scan_ids=[scan_id])))
    if st.state not in ("done", "failed"):
        await asyncio.wait_for(done, 60)
    return await client.call(DUPES_STATUS, pd.DupesJobParams(job_id=st.job_id))


async def opened(client: Connection, job_id: str, preset: str = "conservative") -> pc.CleanupState:
    st = await client.call(DUPES_CLEANUP_OPEN, pc.CleanupOpenParams(job_id=job_id, preset=preset))  # type: ignore[arg-type]
    for _ in range(200):
        if st.facts_done >= st.facts_total:
            return st
        await asyncio.sleep(0.05)
        st = await client.call(DUPES_CLEANUP_STATE, pc.CleanupPlanParams(plan_id=st.plan_id))
    raise AssertionError("the risk check did not finish")


async def edit(client: Connection, st: pc.CleanupState, **kw: object) -> pc.CleanupState:
    return await client.call(
        DUPES_CLEANUP_EDIT,
        pc.CleanupEditParams(plan_id=st.plan_id, base_version=st.version, edit=pc.CleanupEdit(**kw)),  # type: ignore[arg-type]
    )


async def ack_all(client: Connection, st: pc.CleanupState) -> pc.CleanupState:
    for b in st.summary.buckets:
        if b.severity == "high" and b.unacked:
            st = await edit(client, st, op="ack", risk=b.id)
    return st


class Tasks:
    def __init__(self, client: Connection) -> None:
        self.events: list[pc.CleanupProgressEvent] = []
        client.on_event(EVENT_DUPES_CLEANUP_PROGRESS, self.events.append)

    async def final(self, task_id: str, limit_s: float = 30) -> pc.CleanupProgressEvent:
        end = ("done", "partial", "stopped", "failed", "undone")
        for _ in range(int(limit_s / 0.05)):
            mine = [e for e in self.events if e.task_id == task_id]
            if mine and mine[-1].state in end:
                return mine[-1]
            await asyncio.sleep(0.05)
        raise AssertionError(f"task {task_id} did not end: {[e.state for e in self.events]}")


async def run_plan(
    client: Connection, st: pc.CleanupState, tasks: Tasks, **kw: object
) -> pc.CleanupProgressEvent:
    st = await ack_all(client, st)
    appr = await client.call(
        DUPES_CLEANUP_APPROVE, pc.CleanupApproveParams(plan_id=st.plan_id, version=st.version)
    )
    task = await client.call(
        DUPES_CLEANUP_EXECUTE,
        pc.CleanupExecuteParams(plan_id=st.plan_id, token=appr.token, **kw),  # type: ignore[arg-type]
    )
    return await tasks.final(task.task_id)


def ino(path: Path) -> tuple[int, int]:
    s = os.lstat(path)
    return (s.st_dev, s.st_ino)


# ------------------------------------------------------------------ the plan
async def test_there_is_no_one_click_deletion(tmp_path: Path, client: Connection) -> None:
    from spaceknife_protocol.methods import METHODS

    assert "dupes.to_basket" not in METHODS and "dupes.basket_members" not in METHODS
    # executing needs an approval's token: without one nothing runs
    a = blob(2 * MIB, 1)
    make(tmp_path / "t", {"keep/a.bin": a, "copy/a.bin": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    with pytest.raises(RpcError):
        await client.call(
            DUPES_CLEANUP_EXECUTE, pc.CleanupExecuteParams(plan_id=st.plan_id, token="0123456789abcdef")
        )
    assert (tmp_path / "t" / "copy" / "a.bin").exists()


async def test_default_keepers_match_the_drawer_and_every_decision_is_explained(
    tmp_path: Path, client: Connection
) -> None:
    a, b = blob(2 * MIB, 1), blob(2 * MIB, 2)
    make(tmp_path / "t", {"k/a.jpg": a, "backup/a.jpg": a, "x/b.bin": b, "y/b.bin": b, "z/b.bin": b})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    drawer = await client.call(DUPES_GROUPS, pd.DupesGroupsParams(job_id=job.job_id))
    st = await opened(client, job.job_id)
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    assert page.total == 2
    kept_drawer = {m.path for g in drawer.items for m in g.members if m.role == "keep"}
    kept_plan = {c.path for g in page.items for c in g.members if c.keep != "none"}
    assert kept_plan == kept_drawer
    for g in page.items:
        assert g.keep_line
        for c in g.members:
            assert c.source  # every decision says why
            if c.keep == "none":
                assert c.action == "trash"  # 「保守」: the others go to the recycle bin
                assert c.available["trash"] == ""
    detail = await client.call(
        DUPES_CLEANUP_GROUP, pc.CleanupGroupParams(plan_id=st.plan_id, group=page.items[0].id)
    )
    assert any(d.keep_chain for d in detail.copies)
    assert any(d.facts for d in detail.copies if d.item.keep == "none")


async def test_the_last_keeper_cannot_be_taken_away(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    files = make(tmp_path / "t", {"a/x.bin": a, "b/x.bin": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    g = page.items[0]
    keeper = next(c for c in g.members if c.keep != "none")
    other = next(c for c in g.members if c.keep == "none")
    # 「不保留」 on the keeper: the other one takes its place
    st = await edit(
        client,
        st,
        op="keep_remove",
        scope=pc.CleanupScope(copies=[pc.CleanupCopyRef(group=g.id, path=keeper.path)]),
    )
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    now = {c.path: c.keep for c in page.items[0].members}
    assert now[other.path] != "none" and now[keeper.path] == "none"
    # and then on the other one too: refused, nothing changed
    with pytest.raises(RpcError) as exc:
        await edit(
            client,
            st,
            op="keep_remove",
            scope=pc.CleanupScope(copies=[pc.CleanupCopyRef(group=g.id, path=other.path)]),
        )
    assert "至少留一份" in exc.value.message
    again = await client.call(DUPES_CLEANUP_STATE, pc.CleanupPlanParams(plan_id=st.plan_id))
    assert again.version == st.version
    # undo goes back to the drawer's choice
    st = await client.call(
        DUPES_CLEANUP_UNDO_EDIT, pc.CleanupVersionParams(plan_id=st.plan_id, base_version=st.version)
    )
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    assert {c.path: c.keep for c in page.items[0].members}[keeper.path] != "none"
    assert files["a/x.bin"].exists() and files["b/x.bin"].exists()


async def test_a_stale_version_is_refused_and_an_edit_voids_the_approval(
    tmp_path: Path, client: Connection
) -> None:
    a = blob(2 * MIB, 1)
    make(tmp_path / "t", {"a/x.bin": a, "b/x.bin": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    st = await ack_all(client, st)
    appr = await client.call(
        DUPES_CLEANUP_APPROVE, pc.CleanupApproveParams(plan_id=st.plan_id, version=st.version)
    )
    st = await client.call(DUPES_CLEANUP_STATE, pc.CleanupPlanParams(plan_id=st.plan_id))
    assert st.approval is not None and st.approval.token == appr.token and st.approval.steps == 1
    with pytest.raises(RpcError):
        await client.call(
            DUPES_CLEANUP_EDIT,
            pc.CleanupEditParams(
                plan_id=st.plan_id, base_version=st.version - 1, edit=pc.CleanupEdit(op="options")
            ),
        )
    st = await edit(client, st, op="options", options=pc.CleanupOptions(on_failure="stop"))
    assert st.approval is None
    with pytest.raises(RpcError):
        await client.call(
            DUPES_CLEANUP_EXECUTE, pc.CleanupExecuteParams(plan_id=st.plan_id, token=appr.token)
        )


async def test_a_preview_changes_nothing(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    make(tmp_path / "t", {"a/x.bin": a, "b/x.bin": a, "c/x.bin": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    diff = await client.call(
        DUPES_CLEANUP_PREVIEW,
        pc.CleanupEditParams(
            plan_id=st.plan_id,
            base_version=st.version,
            edit=pc.CleanupEdit(op="action_set", action="leave", scope=pc.CleanupScope(filter="")),
        ),
    )
    assert diff.before["trash"] == 2 and diff.after["trash"] == 0 and diff.after["leave"] == 2
    st2 = await client.call(DUPES_CLEANUP_STATE, pc.CleanupPlanParams(plan_id=st.plan_id))
    assert st2.version == st.version
    assert next(a for a in st2.summary.by_action if a.action == "trash").copies == 2


# ------------------------------------------------------------------ running
@pytest.mark.skipif(
    not POSIX, reason="the POSIX executor; Windows runs the same flow on the Windows checkout"
)
async def test_a_hard_link_run_undo_and_purge(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    files = make(tmp_path / "t", {"k/a.jpg": a, "copy/a.jpg": a})
    keep_id = ino(files["k/a.jpg"])
    copy_id = ino(files["copy/a.jpg"])
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    g = page.items[0]
    other = next(c for c in g.members if c.keep == "none")
    assert other.path == str(files["copy/a.jpg"])
    assert other.available["hardlink"] == "", other.available
    st = await edit(
        client,
        st,
        op="action_set",
        action="hardlink",
        scope=pc.CleanupScope(copies=[pc.CleanupCopyRef(group=g.id, path=other.path)]),
    )
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    c = next(c for c in page.items[0].members if c.keep == "none")
    assert c.action == "hardlink" and c.target == str(files["k/a.jpg"])
    assert {r.code for r in c.risks} >= {"edits_shared"}
    tasks = Tasks(client)
    ev = await run_plan(client, st, tasks)
    assert ev.state == "done", ev
    assert ev.counts.get("done") == 1
    assert ino(files["copy/a.jpg"]) == keep_id  # the copy's name is now the keeper's
    assert files["copy/a.jpg"].read_bytes() == a
    runs = await client.call(DUPES_CLEANUP_RUNS, pc.CleanupRunsParams())
    assert runs.total == 1 and runs.items[0].can_undo == 1 and runs.items[0].held > 0
    run_id = runs.items[0].run_id
    detail = await client.call(DUPES_CLEANUP_RUN, pc.CleanupRunParams(run_id=run_id))
    rec = Path(detail.items[0].recovery)
    assert rec.exists() and ino(rec) == copy_id  # the original waits in the recovery area
    assert str(rec).startswith(str(tmp_path))  # inside the sandbox
    # the results follow: the group is one file under two names now, nothing to free
    st2 = await client.call(DUPES_CLEANUP_STATE, pc.CleanupPlanParams(plan_id=st.plan_id))
    assert st2.summary.copies == 0
    # undo: the original is back under its name, the link gone
    t = await client.call(DUPES_CLEANUP_UNDO, pc.CleanupRunSeqsParams(run_id=run_id))
    ev = await tasks.final(t.task_id)
    assert ev.state == "undone", ev
    assert ino(files["copy/a.jpg"]) == copy_id and not rec.exists()
    assert ino(files["k/a.jpg"]) == keep_id and os.lstat(files["k/a.jpg"]).st_nlink == 1


@pytest.mark.skipif(not POSIX, reason="the POSIX executor")
async def test_purge_checks_the_original_and_the_keeper(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    files = make(
        tmp_path / "t", {"k/a.jpg": a, "copy/a.jpg": a, "k/b.jpg": blob(MIB * 2, 3), "copy/b.jpg": b""}
    )
    files["copy/b.jpg"].write_bytes(files["k/b.jpg"].read_bytes())
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    st = await edit(client, st, op="action_set", action="hardlink", scope=pc.CleanupScope(filter=""))
    tasks = Tasks(client)
    ev = await run_plan(client, st, tasks)
    assert ev.counts.get("done") == 2, ev
    runs = await client.call(DUPES_CLEANUP_RUNS, pc.CleanupRunsParams())
    run_id = runs.items[0].run_id
    detail = await client.call(DUPES_CLEANUP_RUN, pc.CleanupRunParams(run_id=run_id))
    recs = {Path(i.path).name: Path(i.recovery) for i in detail.items}
    # the keeper of b changes after the run: its original may be the only copy left
    files["k/b.jpg"].write_bytes(blob(2 * MIB, 9))
    plan = await client.call(DUPES_CLEANUP_PURGE_PLAN, pc.CleanupPurgeParams(run_id=run_id))
    can = {Path(x.path).name.split("-", 1)[1]: x.can for x in plan.items}
    assert can == {"a.jpg": True, "b.jpg": False}
    t = await client.call(DUPES_CLEANUP_PURGE, pc.CleanupPurgeParams(run_id=run_id))
    assert t.delay_s > 0
    ev = await tasks.final(t.task_id)
    assert ev.state == "done"
    assert not recs["a.jpg"].exists() and recs["b.jpg"].exists()  # only what was proven elsewhere


@pytest.mark.skipif(not POSIX, reason="the POSIX executor")
async def test_a_symbolic_link_and_the_recycle_bin(tmp_path: Path, client: Connection) -> None:
    a, b = blob(2 * MIB, 1), blob(2 * MIB, 2)
    make(tmp_path / "t", {"k/a.png": a, "other/a.png": a, "k/b.png": b, "other/b.png": b})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    page = await client.call(DUPES_CLEANUP_GROUPS, pc.CleanupGroupsParams(plan_id=st.plan_id))
    by_name = {Path(g.members[0].path).name: g for g in page.items}
    ga = by_name["a.png"]
    ca = next(c for c in ga.members if c.keep == "none")
    st = await edit(
        client,
        st,
        op="action_set",
        action="symlink",
        scope=pc.CleanupScope(copies=[pc.CleanupCopyRef(group=ga.id, path=ca.path)]),
    )
    tasks = Tasks(client)
    ev = await run_plan(client, st, tasks)
    assert ev.state == "done", ev
    link = Path(ca.path)
    assert link.is_symlink() and link.read_bytes() == a
    assert not os.path.isabs(os.readlink(link))  # same volume, no link on the way: relative
    other_b = next(c.path for c in by_name["b.png"].members if c.keep == "none")
    if Path(other_b).exists():
        # no usable recycle bin here (a CI runner's home trash on another file system): left alone
        assert ev.counts.get("skipped_unavailable", 0) + ev.counts.get("skipped_trash_full", 0) >= 1
    else:
        runs = await client.call(DUPES_CLEANUP_RUNS, pc.CleanupRunsParams())
        t = await client.call(DUPES_CLEANUP_UNDO, pc.CleanupRunSeqsParams(run_id=runs.items[0].run_id))
        await tasks.final(t.task_id)
        assert Path(other_b).exists() and not link.is_symlink() and link.read_bytes() == a


@pytest.mark.skipif(not POSIX, reason="the POSIX executor")
async def test_a_copy_changed_after_approval_is_skipped(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    files = make(tmp_path / "t", {"k/a.jpg": a, "copy/a.jpg": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    st = await edit(client, st, op="action_set", action="hardlink", scope=pc.CleanupScope(filter=""))
    st = await ack_all(client, st)
    appr = await client.call(
        DUPES_CLEANUP_APPROVE, pc.CleanupApproveParams(plan_id=st.plan_id, version=st.version)
    )
    files["copy/a.jpg"].write_bytes(blob(2 * MIB, 5))  # changed: the step must not run
    tasks = Tasks(client)
    task = await client.call(
        DUPES_CLEANUP_EXECUTE,
        pc.CleanupExecuteParams(plan_id=st.plan_id, token=appr.token, proceed_after_drops=True),
    )
    ev = await tasks.final(task.task_id)
    assert ev.counts.get("skipped_changed") == 1, ev
    assert ino(files["copy/a.jpg"]) != ino(files["k/a.jpg"])
    assert files["copy/a.jpg"].read_bytes() != a  # theirs, untouched


@pytest.mark.skipif(not POSIX, reason="the POSIX executor")
async def test_the_preflight_asks_again_when_it_drops_steps(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    files = make(tmp_path / "t", {"k/a.jpg": a, "copy/a.jpg": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    st = await ack_all(client, st)
    appr = await client.call(
        DUPES_CLEANUP_APPROVE, pc.CleanupApproveParams(plan_id=st.plan_id, version=st.version)
    )
    files["k/a.jpg"].write_bytes(blob(2 * MIB, 5))  # the keeper changed
    tasks = Tasks(client)
    task = await client.call(
        DUPES_CLEANUP_EXECUTE, pc.CleanupExecuteParams(plan_id=st.plan_id, token=appr.token)
    )
    ev = await tasks.final(task.task_id)
    assert ev.needs_confirm and ev.skipped and ev.skipped[0].code == "skipped_keeper_changed"
    assert files["copy/a.jpg"].read_bytes() == a


async def test_a_keeper_in_the_basket_skips_its_group(tmp_path: Path, client: Connection) -> None:
    a = blob(2 * MIB, 1)
    files = make(tmp_path / "t", {"k/a.jpg": a, "copy/a.jpg": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    found = await client.call(TREE_FIND, p.TreeFindParams(scan_id=sid, path=str(tmp_path / "t" / "k")))
    assert found.node is not None
    await client.call(BASKET_CLEAR, Empty())
    await client.call(BASKET_ADD, po.BasketAddParams(scan_id=sid, nodes=[found.node.id]))
    st = await ack_all(client, st)
    appr = await client.call(
        DUPES_CLEANUP_APPROVE, pc.CleanupApproveParams(plan_id=st.plan_id, version=st.version)
    )
    tasks = Tasks(client)
    task = await client.call(
        DUPES_CLEANUP_EXECUTE, pc.CleanupExecuteParams(plan_id=st.plan_id, token=appr.token)
    )
    ev = await tasks.final(task.task_id)
    assert ev.skipped and ev.skipped[0].code == "skipped_keeper_lost", ev
    assert files["copy/a.jpg"].exists()
    await client.call(BASKET_CLEAR, Empty())


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX permissions")
async def test_nothing_outside_the_sandbox(
    tmp_path: Path, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    a = blob(2 * MIB, 1)
    files = make(tmp_path / "t", {"k/a.jpg": a, "copy/a.jpg": a})
    sid = await scanned(client, tmp_path / "t")
    job = await searched(client, sid)
    st = await opened(client, job.job_id)
    st = await edit(client, st, op="action_set", action="hardlink", scope=pc.CleanupScope(filter=""))
    monkeypatch.setenv("SPACEKNIFE_CLEANUP_SANDBOX", str(tmp_path / "elsewhere"))
    tasks = Tasks(client)
    ev = await run_plan(client, st, tasks)
    assert ev.counts.get("done", 0) == 0
    assert ino(files["copy/a.jpg"]) != ino(files["k/a.jpg"])
