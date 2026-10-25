"""The live side of plugins: a source's rescans and watcher, change annotators in comparisons, saved
snapshots opened by plugins, and what the memory watchdog counts for them."""

from __future__ import annotations

import asyncio
import time
import zipfile
from collections.abc import Callable
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife.plugins import sources
from spaceknife.plugins.listing import Collector, build, path_remap
from spaceknife.server import diag
from spaceknife.store.nodes import NodeStore
from spaceknife_client.views import ViewClient
from spaceknife_protocol import compare as pc
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import COMPARE_SUMMARY, SCAN_RESCAN, SCAN_START, VIEW_OPEN

from .plugin_helpers import connect, mock, plugin_server
from .test_compare_server import change_files, compared
from .test_scan_server import wait_done
from .tree_fixture import build_tree

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def _zip(path: Path, members: dict[str, bytes]) -> None:
    with zipfile.ZipFile(path, "w") as z:
        for name, data in members.items():
            z.writestr(name, data)


def _paths(store: NodeStore) -> list[str]:
    return sorted(store.path(i).replace("\\", "/") for i in range(2, len(store)))


async def _until(check: Callable[[], object], within: float = 20.0) -> None:
    deadline = time.monotonic() + within
    while not check():
        if time.monotonic() > deadline:
            raise AssertionError("timed out")
        await asyncio.sleep(0.05)


def _listed(entries: dict[str, int]) -> NodeStore:
    c = Collector()
    for path, size in entries.items():
        parts = path.split("/")
        d = c._folder(parts[:-1])
        if size < 0:
            c._folder(parts)
        else:
            d.files[parts[-1]] = (size, size, 0, 0, 0)
    return build("top", c.root, virtual=True)


def test_paths_map_onto_a_new_listing() -> None:
    old = _listed({"a/x.txt": 1, "a/y.txt": 2, "b/z.txt": 3, "c": -1})
    new = _listed({"a/y.txt": 2, "a/w.txt": 9, "b/z.txt": 3, "d/q.txt": 1})
    remap = path_remap(old, new)
    for node in range(len(old)):
        path = old.path(node) if node else ""
        target = int(remap[node])
        if node == 0:
            assert target == 0
        elif path.replace("\\", "/").endswith(("x.txt", "/c")):
            assert target == -1  # gone
        else:
            assert new.path(target) == path


async def test_rescan_lists_the_source_again_and_views_keep_their_place(tmp_path: Path) -> None:
    archive = tmp_path / "data.zip"
    _zip(archive, {"docs/readme.txt": b"x" * 100, "docs/deep/notes.md": b"y" * 50, "top.bin": b"z"})
    async with mock("t.full") as host:
        await host.scan_dir(tmp_path)  # files.read reaches the zip only inside a scanned folder
        scan_id = await host.source("zip", str(archive))
        handle = host.server.scans.scans[scan_id]
        store = handle.store
        await _until(lambda: handle.monitor is not None)  # the source has a watcher
        assert isinstance(handle.monitor, sources.SourceMonitor)
        assert handle.status().monitor is not None and handle.status().monitor.watcher == "t.full.watch_zip"
        # a view zoomed into docs/deep
        vh = host.server.views.open(
            host.session,  # type: ignore[arg-type]
            pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600)),
        )
        deep = store.find(str(Path(store.name(1)) / "docs" / "deep"))
        assert deep is not None and vh.view.zoom_to_node(deep)
        epoch = store.epoch
        # the archive changes: F5 lists it again at once
        _zip(archive, {"docs/readme.txt": b"x" * 100, "docs/deep/notes.md": b"y" * 50, "new.bin": b"n" * 7})
        await host.server.monitors.rescan(handle, 0)
        await _until(lambda: any(path.endswith("new.bin") for path in _paths(store)))
        assert _paths(store) == [
            "data.zip/docs",
            "data.zip/docs/deep",
            "data.zip/docs/deep/notes.md",
            "data.zip/docs/readme.txt",
            "data.zip/new.bin",
        ]
        assert store.epoch > epoch  # node ids plugins held are stale now
        assert store.path(vh.view.zoom.node).replace("\\", "/") == "data.zip/docs/deep"
        assert handle.status().state == "done" and handle.status().items == len(store) - 1


async def test_a_watcher_lists_the_source_again(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sources, "QUIET_S", 0.05)
    archive = tmp_path / "data.zip"
    _zip(archive, {"a.txt": b"a"})
    async with mock("t.full") as host:
        await host.scan_dir(tmp_path)
        scan_id = await host.source("zip", str(archive))
        handle = host.server.scans.scans[scan_id]
        await _until(lambda: handle.monitor is not None)
        mon = handle.monitor
        await asyncio.sleep(0.3)  # the watcher has looked at the archive once
        _zip(archive, {"a.txt": b"a", "b.txt": b"bb"})
        await _until(lambda: mon.reruns >= 1)
        assert any(path.endswith("b.txt") for path in _paths(handle.store))
        st = handle.status().monitor
        assert st is not None and st.state == "running" and st.events >= 1 and st.changes >= 1
        # a tree too big to list again on every change: the status bar asks for a rescan instead
        monkeypatch.setattr(sources, "RERUN_MAX_ITEMS", 1)
        reruns = mon.reruns
        await asyncio.sleep(0.1)
        _zip(archive, {"a.txt": b"a", "b.txt": b"bb", "c.txt": b"ccc"})
        await _until(lambda: handle.status().monitor.state == "degraded")
        assert "重新扫描" in handle.status().monitor.detail and mon.reruns == reruns
        # live monitoring off: the watcher stops
        host.server.settings.update({"scan.monitor": False})
        await host.server.monitors.apply_settings()
        assert handle.monitor is None


async def test_rescan_over_the_protocol(tmp_path: Path) -> None:
    archive = tmp_path / "data.zip"
    _zip(archive, {"one.txt": b"1"})
    async with plugin_server("t.full") as srv, connect(srv) as client:
        await client.call(SCAN_START, p.ScanStartParams(roots=[str(tmp_path)], workers=0))
        started = await client.call(SCAN_START, p.ScanStartParams(source="t.full.zip", uri=str(archive)))
        handle = srv.scans.scans[started.scan_id]
        await _until(lambda: handle.status().state == "done")
        _zip(archive, {"one.txt": b"1", "two.txt": b"22"})
        await client.call(SCAN_RESCAN, p.ScanRescanParams(scan_id=started.scan_id, node=1))
        await _until(lambda: any(path.endswith("two.txt") for path in _paths(handle.store)))


async def test_annotators_explain_what_the_rules_do_not(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import spaceknife.insights as insights

    # temporary folders have a rule of their own (and tmp_path is one): no rule knows these items
    monkeypatch.setattr(insights, "match_node", lambda store, node: None)
    root = tmp_path / "root"
    build_tree(root)
    async with plugin_server("t.full") as srv, connect(srv) as client:
        first = await wait_done(
            client, (await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))).scan_id
        )
        change_files(root)
        await asyncio.sleep(1.1)  # snapshot names have a 1 s resolution
        now = await wait_done(
            client, (await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))).scan_id
        )
        assert first.snapshot is not None
        info = await compared(client, now.scan_id, first.snapshot)
        update, _frame = await ViewClient(client).call(
            VIEW_OPEN,
            pv.ViewOpenParams(scan_id=info.compare_id, viewport=pv.Viewport(width=900, height=600)),
        )
        view_id = update.state.view_id
        summary = await client.call(COMPARE_SUMMARY, pc.CompareSummaryParams(view_id=view_id))
        grown = [i for i in summary.grow_items if i.kind != "group"]
        assert grown and all(i.reason == "grew" for i in grown)  # t.full: "grew" for what grew
        assert summary.shrink_items and all(i.reason == "" for i in summary.shrink_items)
        # asked again: the answers are remembered, the plugin is not asked again
        calls = srv.plugins._reasons.copy()
        again = await client.call(COMPARE_SUMMARY, pc.CompareSummaryParams(view_id=view_id))
        assert [i.reason for i in again.grow_items] == [i.reason for i in summary.grow_items]
        assert srv.plugins._reasons == calls


async def test_annotator_through_the_mock_host() -> None:
    async with mock("t.full") as host:
        got = await host.annotate([{"path": "/x", "delta": 5}, {"path": "/y", "delta": -5}])
        assert got == ["grew", None]


async def test_call_sync_refuses_the_event_loop() -> None:
    async with mock("t.full") as host:
        hp = await host._ready()
        from spaceknife.server.rpc import RpcError

        with pytest.raises(RpcError) as err:
            hp.call_sync("t.full", "method", "hello", {}, timeout=0.1)
        assert err.value.code == ErrorCode.PLUGIN_UNAVAILABLE
        # from a worker thread it works, and an annotator fills only blank reasons
        items = [
            pc.ChangeItem(node=1, path="/a", label="a", kind="file", delta=3, base=0, target=3, status="new"),
            pc.ChangeItem(
                node=2,
                path="/b",
                label="b",
                kind="file",
                delta=3,
                base=0,
                target=3,
                status="new",
                reason="rule",
            ),
        ]
        out = await asyncio.to_thread(hp.annotate_changes, "cmp-1", items)
        assert [i.reason for i in out] == ["grew", "rule"]


@pytest.mark.parametrize("process", [False, True])
async def test_snapshots_last_one_call_and_two_at_most(process: bool) -> None:
    async with mock("t.hist", process=process) as host:
        hp = await host._ready()
        start = datetime.now(UTC)
        for i in range(3):
            host.snapshot(when=start - timedelta(days=i))
        got = await host.call("open_some", count=1)
        assert got["listed"] == 3 and len(got["nodes"]) == 1 and got["nodes"][0] == len(host.store)
        assert hp.api.opened() == []  # released when the call ended
        got = await host.call("open_some", count=3)
        assert len(got["nodes"]) == 2 and got["refused"] == ErrorCode.PLUGIN_LIMIT
        got = await host.call("open_some", count=3, close=True)
        assert len(got["nodes"]) == 3 and got["refused"] == 0
        assert hp.api.opened() == [] and hp.api.opened_bytes() == 0
        rep = await host.report("history")
        assert "3" in host.text(rep)


async def test_the_memory_watchdog_counts_what_plugins_hold() -> None:
    async with mock("t.hist") as host:
        hp = await host._ready()
        server = host.server
        before = diag.accounted(server)
        store = host.store
        hp.api._opened["snap-t.hist-99"] = ("t.hist", store)
        try:
            assert diag.plugin_bytes(server) >= store.ram_bytes() > 0
            assert diag.accounted(server) == before + diag.plugin_bytes(server)
            server.sessions.discard(host.session)  # the mock session has no outbox to report
            assert "snap-t.hist-99" in diag.report(server)
        finally:
            hp.api.release_plugin("t.hist")
        assert hp.api.opened() == []


async def test_idle_processes_stop_and_come_back(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.plugins import host as host_module

    async with mock("t.full", process=True) as host:
        hp = await host._ready()
        assert await host.call("add", a=1) == {"total": 1}
        rec = hp.records["t.full"]
        assert rec.runtime is not None and rec.runtime.kind == "process"
        await hp.idle_sweep()
        assert rec.runtime is not None  # used a moment ago
        monkeypatch.setattr(host_module, "IDLE_STOP_S", -1.0)
        await hp.idle_sweep()
        assert rec.runtime is None and rec.state == "enabled"  # stopped, still on
        assert await host.call("add", a=2) == {"total": 2}  # started again on the next call
        assert rec.runtime is not None


async def test_the_idle_sweep_leaves_a_starting_process_alone(monkeypatch: pytest.MonkeyPatch) -> None:
    # a record never used (relinked in plugin dev) whose process takes long to start (Windows):
    # the minute's sweep must not kill it half-way (it then reported "exited (1)")
    from spaceknife.plugins import host as host_module
    from spaceknife.plugins.runtime.process import ProcessRuntime

    async with mock("t.full", process=True) as host:
        hp = await host._ready()
        rec = hp.records["t.full"]
        await hp._stop_runtime(rec)
        rec.last_used = 0.0
        monkeypatch.setattr(host_module, "IDLE_STOP_S", -1.0)
        start = ProcessRuntime.start

        async def slow_start(self: ProcessRuntime) -> object:
            self.state = "starting"
            await hp.idle_sweep()  # the sweep runs while this one starts
            return await start(self)

        monkeypatch.setattr(ProcessRuntime, "start", slow_start)
        assert await host.call("add", a=1) == {"total": 1}
        assert rec.runtime is not None and rec.runtime.state == "running"


async def test_a_caller_giving_up_does_not_stop_the_start(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.plugins.runtime.process import ProcessRuntime
    from spaceknife.server.rpc import RpcError

    async with mock("t.full", process=True) as host:
        hp = await host._ready()
        rec = hp.records["t.full"]
        await hp._stop_runtime(rec)
        assert rec.runtime is None
        start = ProcessRuntime.start

        async def slow_start(self: ProcessRuntime) -> object:
            await asyncio.sleep(1.0)  # a slow machine
            return await start(self)

        monkeypatch.setattr(ProcessRuntime, "start", slow_start)
        # a frame's colours wait 20 ms: far less than a process takes to start
        with pytest.raises(RpcError):
            await asyncio.to_thread(hp.call_sync, "t.full", "method", "add", {"a": 1}, timeout=0.02)
        await _until(lambda: rec.runtime is not None and rec.runtime.state == "running")
        assert await host.call("add", a=4) == {"total": 4}


async def test_long_lived_calls_have_no_time_limit(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.plugins import host as host_module

    async with mock("t.full") as host:
        hp = await host._ready()
        seen: list[dict[str, object]] = []
        rt_call = type(hp.records["t.full"].runtime).call

        async def spy(
            self: object, call_id: int, kind: str, name: str, params: object, ctx: dict, *a: object
        ) -> object:  # type: ignore[type-arg]
            seen.append({"kind": kind, "timeout": ctx["timeout"]})
            return await rt_call(self, call_id, kind, name, params, ctx, *a)  # type: ignore[arg-type]

        monkeypatch.setattr(type(hp.records["t.full"].runtime), "call", spy)
        await hp.call("t.full", "stream", "ticks", {}, on_chunk=lambda v, f: None)
        await hp.call("t.full", "method", "add", {"a": 1})
        assert seen[0] == {"kind": "stream", "timeout": host_module.LONG_CALL_S}
        assert seen[1]["kind"] == "method" and float(seen[1]["timeout"]) <= 600.0  # type: ignore[arg-type]


def test_the_watcher_registry(tmp_path: Path) -> None:
    import sys

    from spaceknife.monitor import base
    from spaceknife.monitor.poll import PollWatcher

    names = {k.name for k in base.watcher_kinds()}
    assert {"rdcw", "inotify", "fsevents", "poll"} <= names
    assert base.watcher_kinds("win32")[0].name == "rdcw"
    assert base.watcher_kinds("linux")[0].name == "inotify"
    assert base.watcher_kinds("darwin")[0].name == "fsevents"
    assert [k.name for k in base.watcher_kinds("freebsd14")] == ["poll"]
    made: list[str] = []

    def special(root: str, poll_interval: float, poll_rate: int) -> PollWatcher:
        made.append(root)
        return PollWatcher(root, poll_interval=poll_interval, poll_rate=poll_rate)

    def broken(root: str, poll_interval: float, poll_rate: int) -> PollWatcher:
        raise OSError("not here")

    base.register_watcher(base.WatcherKind("special", special, filesystems=("zfs",), priority=50))
    base.register_watcher(base.WatcherKind("broken", broken, priority=60))
    try:
        assert base.watcher_kinds(sys.platform, "zfs")[0].name == "broken"
        w = base.create_watcher(str(tmp_path), filesystem="ZFS")  # broken passes it on
        assert made == [str(tmp_path)] and isinstance(w, PollWatcher)
        base.create_watcher(str(tmp_path), filesystem="ext4")  # not for special: the platform's own
        assert made == [str(tmp_path)]
    finally:
        base.unregister_watcher("special")
        base.unregister_watcher("broken")
    assert {k.name for k in base.watcher_kinds()} == names


def test_ids_outside_a_remap_are_gone() -> None:
    remap = np.array([0, 5, -1], dtype=np.int64)
    assert sources._apply(remap, np.array([1, 2, 7, -3])).tolist() == [5, -1, -1, -1]


async def test_a_watcher_comes_back_after_its_plugin_restarts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """plugin dev reloads a plugin on save (restart), the user turns it off and on: the source
    scan's watcher stops meanwhile and watches again once the plugin is back."""
    monkeypatch.setattr(sources, "QUIET_S", 0.05)
    monkeypatch.setattr(sources, "WAIT_BACK_S", 0.05)
    archive = tmp_path / "data.zip"
    _zip(archive, {"a.txt": b"a"})
    async with mock("t.full") as host:
        await host.scan_dir(tmp_path)
        scan_id = await host.source("zip", str(archive))
        handle = host.server.scans.scans[scan_id]
        await _until(lambda: handle.monitor is not None)
        mon = handle.monitor
        assert isinstance(mon, sources.SourceMonitor)
        hp = host.server.plugins
        await hp.restart("t.full")  # the process goes, the watcher's call with it
        rec = hp.records["t.full"]
        await hp.disable("t.full")
        await _until(
            lambda: handle.status().monitor is not None and handle.status().monitor.state == "failed"
        )
        await hp.enable("t.full", rec.hash, sorted(rec.granted))
        await _until(lambda: handle.status().monitor.state == "running")
        reruns = mon.reruns
        _zip(archive, {"a.txt": b"a", "b.txt": b"bb"})
        await _until(lambda: mon.reruns > reruns and any(x.endswith("b.txt") for x in _paths(handle.store)))


@pytest.mark.parametrize(
    "code", [ErrorCode.REQUEST_CANCELLED, ErrorCode.PLUGIN_UNAVAILABLE], ids=["cancelled", "unavailable"]
)
async def test_a_watcher_ended_by_a_restart_watches_again_at_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, code: ErrorCode
) -> None:
    """A restart (plugin dev reloading the plugin) ends the watcher's call: PLUGIN_UNAVAILABLE when
    the process went first, REQUEST_CANCELLED when the plugin answered the host's cancel first (the
    full test run under load: test_a_watcher_comes_back_after_its_plugin_restarts timed out). Either
    way the watcher calls again after WAIT_BACK_S and the monitor says running again. It used to wait
    RETRY_S[1] (30 s) after a cancel, to stay degraded after either while it watched again, and to
    count the next reload as a second failure in a row (RETRY_S[2], 120 s)."""
    from spaceknife.server.rpc import RpcError

    monkeypatch.setattr(sources, "QUIET_S", 0.05)
    monkeypatch.setattr(sources, "WAIT_BACK_S", 0.05)
    monkeypatch.setattr(sources, "RETRY_S", (0.3, 30.0, 120.0))  # watched this long: not in a row
    archive = tmp_path / "data.zip"
    _zip(archive, {"a.txt": b"a"})
    async with mock("t.full") as host:
        await host.scan_dir(tmp_path)
        hp = host.server.plugins
        real = hp.call
        watches: list[float] = []

        async def call(plugin: str, kind: str, *args: Any, **kw: Any) -> Any:
            if kind == "watch":
                watches.append(time.monotonic())
                if len(watches) <= 2:  # two reloads end the first two: at once, then a while later
                    await asyncio.sleep(0.05 if len(watches) == 1 else 0.6)
                    raise RpcError(code, "request cancelled")
            return await real(plugin, kind, *args, **kw)

        monkeypatch.setattr(hp, "call", call)
        scan_id = await host.source("zip", str(archive))
        handle = host.server.scans.scans[scan_id]
        await _until(lambda: len(watches) >= 3, within=5.0)
        assert watches[1] - watches[0] < 3.0 and watches[2] - watches[1] < 3.0
        await _until(lambda: handle.status().monitor.state == "running", within=3.0)
        mon = handle.monitor
        assert isinstance(mon, sources.SourceMonitor)
        reruns = mon.reruns
        _zip(archive, {"a.txt": b"a", "b.txt": b"bb"})  # and it watches
        await _until(lambda: mon.reruns > reruns and any(x.endswith("b.txt") for x in _paths(handle.store)))


async def test_watchers_do_not_take_the_call_slots(tmp_path: Path) -> None:
    """Every open source scan has a watcher running for as long as it is open; they run beside the
    plugin's calls: with more of them than call slots, new source scans still list and fields
    still answer within their budget."""
    from spaceknife.plugins import host as host_mod
    from spaceknife.plugins.handlers import node_fields
    from spaceknife_protocol.plugins import PluginFieldsParams

    archives = []
    for i in range(host_mod.MAX_CONCURRENT + 2):
        archive = tmp_path / f"data{i}.zip"
        _zip(archive, {f"f{i}.txt": b"x" * (i + 1), "docs/readme.md": b"y"})
        archives.append(archive)
    async with mock("t.full") as host:
        await host.scan_dir(tmp_path)
        handles = []
        for archive in archives:
            scan_id = await host.source("zip", str(archive))
            handle = host.server.scans.scans[scan_id]
            await _until(lambda handle=handle: handle.monitor is not None)
            handles.append(handle)
        assert all(h.status().state == "done" for h in handles)
        last = handles[-1]
        hp = host.server.plugins
        t0 = time.monotonic()
        res = await node_fields(
            hp, None, PluginFieldsParams(scan_id=last.id, node=len(last.store) - 1, slot="inspector")
        )
        assert time.monotonic() - t0 < 1.5
        assert "t.full.name_len" in [f.id for f in res.fields]


async def test_a_busy_plugin_does_not_hold_a_field_past_its_budget(tmp_path: Path) -> None:
    """Waiting for a call slot counts against the budget: the details panel gets 'pending' instead
    of waiting for the plugin's long calls."""
    from spaceknife.plugins import host as host_mod
    from spaceknife.plugins.handlers import node_fields
    from spaceknife_protocol.plugins import PluginFieldsParams

    (tmp_path / "a.txt").write_bytes(b"a")
    async with mock("t.full") as host:
        scan_id = await host.scan_dir(tmp_path)
        hp = host.server.plugins
        sem = hp._slots.setdefault("t.full", asyncio.Semaphore(host_mod.MAX_CONCURRENT))
        for _ in range(host_mod.MAX_CONCURRENT):
            await sem.acquire()  # every slot busy
        try:
            t0 = time.monotonic()
            res = await node_fields(hp, None, PluginFieldsParams(scan_id=scan_id, node=2, slot="inspector"))
            assert time.monotonic() - t0 < 1.6 and res.pending and res.fields == []
        finally:
            for _ in range(host_mod.MAX_CONCURRENT):
                sem.release()
        await _until(lambda: any(k[0] == "t.full.name_len" for k in hp._field_cache))  # answered later
        res = await node_fields(hp, None, PluginFieldsParams(scan_id=scan_id, node=2, slot="inspector"))
        assert "t.full.name_len" in [f.id for f in res.fields]


async def test_a_change_of_settings_drops_the_cached_answers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.plugins.handlers import node_fields
    from spaceknife_protocol.plugins import PluginFieldsParams

    (tmp_path / "a.log").write_bytes(b"a")
    async with mock("t.full") as host:
        scan_id = await host.scan_dir(tmp_path)
        hp = host.server.plugins
        await node_fields(hp, None, PluginFieldsParams(scan_id=scan_id, node=2, slot="inspector"))
        await asyncio.to_thread(hp.set_masks, scan_id, host.store, ["t.full.logs"])
        assert hp._field_cache and hp._set_cache
        hooks: list[tuple[str, dict[str, object]]] = []

        async def hook(rec: object, event: str, **args: object) -> None:
            hooks.append((event, args))

        monkeypatch.setattr(hp, "_hook", hook)
        # a hook only for plugins that have one: give t.full one for the test
        monkeypatch.setattr(
            sources.EXTENSIONS,
            "owned",
            lambda owner, real=sources.EXTENSIONS.owned: [
                *real(owner),
                type("C", (), {"kind": "hook", "spec": type("S", (), {"event": "settings"})()})(),
            ],
        )
        version = sources.EXTENSIONS.version
        epoch = hp.epochs["t.full"]
        host.server.settings.update({"plugin.t.full.min_size": 5})
        await _until(lambda: hooks)
        assert hooks == [("settings", {"changed": ["min_size"]})]
        assert hp.epochs["t.full"] == epoch + 1
        assert not [k for k in hp._field_cache if k[0].startswith("t.full.")] and not hp._set_cache
        assert sources.EXTENSIONS.version > version  # frontends ask for fields again
        # a change of another setting is not this plugin's
        hooks.clear()
        host.server.settings.update({"treemap.flash_changes": False})
        await asyncio.sleep(0.1)
        assert hooks == [] and hp.epochs["t.full"] == epoch + 1
