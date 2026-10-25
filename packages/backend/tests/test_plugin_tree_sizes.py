"""``TreeChange.before`` / ``after`` are what each folder (with everything under it) took right
before and right after the changes of one ``tree_changed`` call, so one plugin's calls follow on
from each other: a folder's ``before`` in one call is its ``after`` in the one before, from the
size before the first change to the size the tree has in the end (docs/plugin-api.md, 「插件进程
听到的变化」; round 5's probes ``newfolder`` and ``rapidslow``).

Round 5 found two ways it broke: the host added up each change's own allocation, so a folder that
appeared with contents (a new folder with files, a tree moved or extracted in) and its parents grew
by the folder's own block only, and a removed folder lost its block from the count; and a hook
called again because the tree was reorganised meanwhile was told ``after`` as the tree was then
and ``before = after - its own changes``, so with another batch waiting both calls carried the
newer changes. Now the monitor reads each folder's size right before its first change in a batch
(``plugins.live.SizesBefore``), ``after`` is read right after the batch, and a hook called again
gets the waiting batch with its own (one call)."""

from __future__ import annotations

import asyncio
import itertools
import os
import shutil
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife.plugins import live as live_mod
from spaceknife.plugins.listing import from_spec
from spaceknife_sdk.testing import MockHost

pytestmark = pytest.mark.timeout(240)

PID = "t.treesizes"
TOML = f"""id = "{PID}"
version = "0.1.0"
name = {{ zh = "前后的占用", en = "Sizes before and after" }}
entry = "plugin.py"
permissions = ["tree.read"]
"""

CODE = """from __future__ import annotations

from typing import Any

from spaceknife_sdk import Context, Payload, Plugin, TreeChange

plugin = Plugin()
SEEN: list[dict[str, Any]] = []


class Seen(Payload):
    seen: list[dict[str, Any]]


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen(seen=SEEN)


@plugin.on("tree_changed")
def tree_changed(ctx: Context, change: TreeChange) -> None:
    read = [n.alloc for n in ctx.tree.nodes(change.folders)]
    SEEN.append(
        {"paths": change.paths, "before": change.before, "after": change.after, "read": read}
    )
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(live_mod, "TREE_EVENT_S", 0.2)
    folder = tmp_path / PID
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


async def _until(cond: Callable[[], Awaitable[object]], limit: float) -> None:
    end = time.monotonic() + limit
    while not await cond():
        if time.monotonic() > end:
            raise AssertionError("timed out")
        await asyncio.sleep(0.1)


def _tot(host: MockHost, rel: str) -> int:
    return int(host.store.cols["tot_alloc"][host.find(rel)])


def _by_path(seen: list[dict[str, Any]]) -> dict[str, list[tuple[int | None, int]]]:
    """Each folder's (before, after) over the calls, in order."""
    out: dict[str, list[tuple[int | None, int]]] = {}
    for call in seen:
        for p, b, a in zip(call["paths"], call["before"], call["after"], strict=True):
            out.setdefault(os.path.normpath(p), []).append((b, a))
    return out


def _follows_on(steps: list[tuple[int | None, int]], first: int, last: int) -> bool:
    """From ``first`` to ``last``, each call's before the previous one's after."""
    if not steps or steps[0][0] != first or steps[-1][1] != last:
        return False
    return all(b == prev_a for (_pb, prev_a), (b, _a) in itertools.pairwise(steps))


async def _watching(host: MockHost, sid: str, folders: int) -> None:
    """The scan's monitor watches its ``folders`` folders (Linux places a watch per folder; a
    recursive watcher says -1)."""
    mon = host.server.scans.scans[sid].monitor
    assert mon is not None

    async def ready() -> bool:
        watched = mon.status().watched_dirs
        return bool(watched < 0 or watched >= folders)

    await _until(ready, 20.0)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_folders_that_appear_or_go_with_contents_count_all_of_it(
    plugin: Path, tmp_path: Path, process: bool
) -> None:
    """Real file changes, applied by the scan's monitor: a tree moved in (three nested folders,
    1 MB in each), a folder made and then given 2 MB, a folder with 1.5 MB removed. Each folder's
    sizes run from what it took before (0 for the new ones) to what the tree has at the end, over
    however many calls the monitor's batches make of it."""
    root = tmp_path / "sizes"
    (root / "dl").mkdir(parents=True)
    (root / "dl" / "seed.bin").write_bytes(b"s" * 10_000)
    (root / "old" / "inner").mkdir(parents=True)
    (root / "old" / "a.bin").write_bytes(b"o" * 1_000_000)
    (root / "old" / "inner" / "b.bin").write_bytes(b"o" * 500_000)
    outside = tmp_path / "outside"
    (outside / "x" / "deep" / "deeper").mkdir(parents=True)
    for rel in ("a.bin", "deep/b.bin", "deep/deeper/c.bin"):
        (outside / "x" / rel).write_bytes(b"m" * 1_000_000)
    (outside / "f.bin").write_bytes(b"c" * 2_000_000)
    folders = sum(1 for _ in os.walk(root))
    async with MockHost.load(plugin, process=process) as host:
        sid = await host.scan_dir(root)
        await _watching(host, sid, folders)
        await host.settled()
        start = {"": _tot(host, ""), "dl": _tot(host, "dl")}

        os.rename(outside / "x", root / "dl" / "x")  # a tree moved (extracted, cloned) in at once
        (root / "c").mkdir()
        # the file comes whole (one written bit by bit into a folder not watched yet is another
        # matter: the monitor's, not these sizes')
        os.replace(outside / "f.bin", root / "c" / "f.bin")
        shutil.rmtree(root / "old")

        def has(rel: str) -> bool:
            try:
                host.find(rel)
            except KeyError:
                return False
            return True

        async def applied() -> bool:
            return has("dl/x/deep/deeper/c.bin") and has("c/f.bin") and not has("old")

        await _until(applied, 30.0)
        end = {rel: _tot(host, rel) for rel in ("", "dl", "dl/x", "c")}
        path = {rel: os.path.normpath(host.path(rel)) for rel in end}
        seen: list[dict[str, Any]] = []

        async def heard_all() -> bool:
            await host.settled()
            seen[:] = (await host.call("seen"))["seen"]
            got = _by_path(seen)
            return all(got.get(path[rel]) and got[path[rel]][-1][1] == end[rel] for rel in end)

        await _until(heard_all, 30.0)
        got = _by_path(seen)
        assert _follows_on(got[path[""]], start[""], end[""]), got[path[""]]
        assert _follows_on(got[path["dl"]], start["dl"], end["dl"]), got[path["dl"]]
        assert _follows_on(got[path["dl/x"]], 0, end["dl/x"]), got[path["dl/x"]]  # new: from nothing
        assert _follows_on(got[path["c"]], 0, end["c"]), got[path["c"]]
        assert end["dl/x"] >= 3_000_000 and end["c"] >= 2_000_000


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_hook_called_again_hears_what_came_meanwhile_with_its_own(
    plugin: Path, tmp_path: Path, process: bool
) -> None:
    """A new file moves the folder's rows: the hook hearing of it meets a reorganised tree when a
    second file came while it ran (round 5's ``rapidslow``). It is called again with both, from
    the size before the first file to the size after the second; what it reads is what it is told."""
    from spaceknife.monitor.base import Change
    from spaceknife.server.monitors import ScanMonitor

    root = tmp_path / "rapid"
    (root / "dl").mkdir(parents=True)
    (root / "dl" / "seed.bin").write_bytes(b"s" * 12_000)
    async with MockHost.load(plugin, process=process) as host:
        sid = await host.scan_dir(root)
        hp = host.server.plugins
        handle = hp.server.scans.scans[sid]
        # the changes this test applies and no others: a monitor without watchers (as with live
        # monitoring off), so the moment of each is the test's
        await hp.server.monitors.stop(sid)
        mon = ScanMonitor(hp.server.monitors, handle, watch=False)
        await mon.start()
        handle.monitor = mon  # MockHost stops it
        await host.settled()
        first = _tot(host, "dl")
        api = hp.api
        real = api.handle
        armed: list[Path] = []

        async def handle_request(rec: Any, method: str, params: dict[str, Any], frames: list[bytes]) -> Any:
            if armed and method in ("host.tree", "host.node"):  # the hook reads: a second file now
                target = armed.pop()
                target.write_bytes(b"2" * 1_200_000)
                await mon._process([Change("created", str(target), is_dir=False)])
            return await real(rec, method, params, frames)

        api.handle = handle_request
        armed.append(root / "dl" / "two.bin")
        (root / "dl" / "one.bin").write_bytes(b"1" * 1_200_000)
        epoch = int(host.store.epoch)
        await mon._process([Change("created", str(root / "dl" / "one.bin"), is_dir=False)])
        assert int(host.store.epoch) > epoch  # rows moved

        async def quiet() -> bool:
            await host.settled()
            return not armed and not hp.live._tree and not hp.live._tree_tasks

        await _until(quiet, 30.0)
        api.handle = real
        seen = (await host.call("seen"))["seen"]
        steps = _by_path(seen)[os.path.normpath(host.path("dl"))]
        last = _tot(host, "dl")
        assert last >= first + 2_400_000
        assert _follows_on(steps, first, last), steps
        assert steps == [(first, last)]  # one call for both
        for call in seen:
            assert call["read"] == call["after"]


def test_sizes_before_are_taken_once_and_follow_remaps(tmp_path: Path) -> None:
    store, _contents = from_spec(
        {"a": {"b": {"x.bin": 5000}, "y.bin": 300}, "z.bin": 10}, str(tmp_path / "r"), 0
    )

    def find(rel: str) -> int:
        node = 1
        for part in rel.split("/"):
            node = next(ch for ch in store.children(node) if store.name(ch) == part)
        return int(node)

    tot = store.cols["tot_alloc"]
    a, b = find("a"), find("a/b")
    taken = live_mod.SizesBefore(store)
    taken.take([b])  # b and every folder above
    assert taken.sizes == {b: int(tot[b]), a: int(tot[a]), 1: int(tot[1])}
    was_a = int(tot[a])
    tot[a] += 4096  # changed: what was taken stays
    taken.take([a])
    taken.new([99])
    assert taken.sizes is not None and taken.sizes[a] == was_a and taken.sizes[99] == 0
    store.epoch += 1  # rows moved: the sizes follow
    taken.remap(lambda ids: np.where(np.asarray(ids) == b, 77, np.asarray(ids)))
    assert taken.sizes is not None and 77 in taken.sizes and b not in taken.sizes
    assert taken.of(store) is taken.sizes
    store.epoch += 1  # another tree's ids (no remap): not known
    assert taken.of(store) is None
    taken.take([a])
    assert taken.sizes is None

    # one plugin's pending changes: before of the first, after of the last
    due = live_mod._TreeDue(epoch=0)
    due.add([b], 1, {b: (10, 20), a: (100, 110)})
    due.add([a], 1, {a: (110, 150), 1: (1000, 1040)})
    assert due.sizes == {b: (10, 20), a: (100, 150), 1: (1000, 1040)}
    newer = live_mod._TreeDue(epoch=0)
    newer.add([b], 1, {b: (20, 25)})
    due.absorb(newer)
    assert due.sizes is not None and due.sizes[b] == (10, 25) and due.changes == 3
    due.add([b], 1, None)  # a tree replaced meanwhile: not known
    assert due.sizes is None
