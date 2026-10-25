"""view.* over the real WebSocket: frames, navigation, filter, tags, pushes, ownership."""

from __future__ import annotations

import asyncio
import time
from collections.abc import Iterator
from pathlib import Path

import numpy as np
import pytest

from spaceknife.layout import registry
from spaceknife.layout.rows import KEY_FREE, KEY_UNKNOWN
from spaceknife.scan.volume import VolumeInfo
from spaceknife.server import Server, monitors
from spaceknife.view.state import F_DIM, F_MATCH, F_SCANNING, F_SELECTED
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_client.views import ViewClient
from spaceknife_protocol import CAP_BINARY_FRAMES, CAP_SUBSCRIPTIONS
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    SCAN_CLOSE,
    SCAN_START,
    TREE_FIND,
    VIEW_BACK,
    VIEW_CLOSE,
    VIEW_FILTER,
    VIEW_LIST,
    VIEW_OPEN,
    VIEW_RENDER,
    VIEW_RESULTS,
    VIEW_REVEAL,
    VIEW_SELECT,
    VIEW_SET,
    VIEW_TAG,
    VIEW_UP,
    VIEW_ZOOM,
)

from .test_scan_server import finished, wait_done
from .test_view_visuals import FAKES
from .tree_fixture import build_tree


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


async def scanned(tmp_path: Path, client: Connection) -> str:
    facts = build_tree(tmp_path / "root")
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
    await wait_done(client, started.scan_id)
    return started.scan_id


def row_named(frame: object, name: str) -> int:
    names = frame.names  # type: ignore[attr-defined]
    assert name in names, names
    return int(names.index(name))


async def test_open_navigate_and_back(tmp_path: Path, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, frame = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=1000, height=700, dpr=1.25))
    )
    assert frame is not None and update.frame is not None
    assert len(frame) == update.frame.rows > 0
    assert (update.frame.width, update.frame.height) == (1250, 875)
    assert frame.columns["x"].dtype == np.int32 and frame.columns["flags"].dtype == np.uint16
    view_id = update.state.view_id
    assert [c.key for c in update.frame.breadcrumb] == [update.state.zoom_node]
    # zoom into "a" by its key
    a_row = row_named(frame, "a")
    key = int(frame.columns["key"][a_row])
    update, frame = await vc.call(VIEW_ZOOM, pv.ViewZoomParams(view_id=view_id, key=key))
    assert frame is not None and update.state.zoom_node == key and update.state.can_back
    assert [c.label for c in update.frame.breadcrumb][-1] == "a"  # type: ignore[union-attr]
    assert set(frame.names) >= {"two.bin", "one.bin"}
    update, frame = await vc.call(VIEW_UP, pv.ViewIdParams(view_id=view_id))
    assert frame is not None and update.state.selected == key
    assert frame.columns["flags"][row_named(frame, "a")] & F_SELECTED
    update, _ = await vc.call(VIEW_BACK, pv.ViewIdParams(view_id=view_id))
    assert update.state.zoom_node == key and update.state.can_forward
    # nothing to do: no frame
    update, frame = await vc.call(VIEW_SELECT, pv.ViewSelectParams(view_id=view_id, key=None))
    assert update.changed is False and frame is None


async def test_filter_dim_errors_and_hide(tmp_path: Path, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, frame = await vc.call(
        VIEW_OPEN,
        pv.ViewOpenParams(
            scan_id=scan_id,
            viewport=pv.Viewport(width=1200, height=900),
            settings=pv.ViewSettingsParams(detail=8, min_element=3),
        ),
    )
    view_id = update.state.view_id
    update, frame = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text="*.mp4"))
    assert frame is not None and update.filter is not None
    assert update.filter.active and update.filter.files == 1 and update.filter.size == 123456
    flags = frame.columns["flags"]
    assert flags[row_named(frame, "movie.mp4")] & F_MATCH
    assert flags[row_named(frame, "c.txt")] & F_DIM if "c.txt" in frame.names else True
    # an error keeps the last valid filter and sends no frame
    update, frame = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text="*.mp4;>500xb"))
    assert frame is None and update.filter is not None
    assert update.filter.stale and update.filter.diagnostics[0].code == "unknown_unit"
    assert update.filter.files == 1
    # hide mode: only the match is laid out
    update, frame = await vc.call(
        VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=pv.ViewSettingsParams(filter_mode="hide"))
    )
    assert frame is not None
    update, frame = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text="*.mp4"))
    update, frame = await vc.call(VIEW_RENDER, pv.ViewIdParams(view_id=view_id))
    assert frame is not None
    leaves = [n for n, k in zip(frame.names, frame.columns["kind"].tolist(), strict=True) if k == 0 and n]
    assert leaves == ["movie.mp4"]


async def test_results_and_reveal(tmp_path: Path, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN,
        pv.ViewOpenParams(
            scan_id=scan_id,
            viewport=pv.Viewport(width=1200, height=900),
            settings=pv.ViewSettingsParams(detail=1),
        ),
    )
    view_id = update.state.view_id
    empty = await client.call(VIEW_RESULTS, pv.ViewResultsParams(view_id=view_id))
    assert not empty.active and empty.total == 0 and empty.rows == []
    update, _ = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text="*.mp4;*.txt"))
    assert update.filter is not None
    res = await client.call(VIEW_RESULTS, pv.ViewResultsParams(view_id=view_id, sort="name", limit=500))
    assert res.active and res.sort == "name" and res.seq == update.frame.seq  # type: ignore[union-attr]
    assert sum(r.count for r in res.rows) == res.files == update.filter.files
    assert sum(r.alloc for r in res.rows) == res.alloc == update.filter.alloc
    assert res.max_alloc == max(r.alloc for r in res.rows)
    page = await client.call(VIEW_RESULTS, pv.ViewResultsParams(view_id=view_id, offset=1, limit=1))
    assert page.sort == "name" and page.offset == 1 and page.rows == res.rows[1:2]  # order remembered
    state = (await client.call(VIEW_LIST, pv.ViewListParams())).views[0]
    assert state.results_sort == "name"
    # locate the movie: at detail 1 it is folded away, so the view navigates to show it
    movie = (
        await client.call(
            TREE_FIND,
            p.TreeFindParams(scan_id=scan_id, path=str(tmp_path / "root" / "a" / "sub" / "movie.mp4")),
        )
    ).node
    assert movie is not None
    update, frame = await vc.call(VIEW_REVEAL, pv.ViewRevealParams(view_id=view_id, key=movie.id))
    assert frame is not None and update.state.selected == movie.id and update.state.can_back
    assert frame.columns["flags"][row_named(frame, "movie.mp4")] & F_SELECTED


async def test_tag_the_selection(tmp_path: Path, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, frame = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=1000, height=700))
    )
    assert frame is not None
    view_id = update.state.view_id
    key = int(frame.columns["key"][row_named(frame, "a")])
    await vc.call(VIEW_SELECT, pv.ViewSelectParams(view_id=view_id, key=key))
    update, frame = await vc.call(VIEW_TAG, pv.ViewTagParams(view_id=view_id, color="red"))
    assert frame is not None and frame.columns["tag"][row_named(frame, "a")] == 1
    update, frame = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text=":red"))
    assert update.filter is not None and update.filter.files == 3  # everything in "a" inherits the tag
    update, frame = await vc.call(VIEW_TAG, pv.ViewTagParams(view_id=view_id, color=None))
    assert frame is not None and frame.columns["tag"][row_named(frame, "a")] == 0


async def test_scan_progress_pushes_frames(tmp_path: Path, server: Server, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    server.views._on_scan(scan_id, "done")
    pushed = await vc.pushed.get()
    assert pushed.view_id == update.state.view_id and pushed.reason == "scan-final"
    frame = await vc.frame(pushed.view_id, pushed.seq)
    assert len(frame) == pushed.rows


async def test_free_and_unknown_space_follow_the_volume(
    tmp_path: Path, server: Server, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    """What is deleted while the scan is monitored becomes free space in the open view, not unknown
    space: views used to keep the volume's free and used space of the moment they opened, so 765 GB
    deleted showed as unknown space. A change within a second of the last read of the free space is
    read a moment later, with no further change needed."""
    facts = build_tree(tmp_path / "root")
    for name, size in (("big.bin", 300_000), ("second.bin", 120_000)):
        (facts.root / name).write_bytes(b"x" * size)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(facts.root)], workers=0))
    await wait_done(client, started.scan_id)
    handle = server.scans.scans[started.scan_id]
    mon = handle.monitor
    assert mon is not None
    for _ in range(250):  # the watcher reads on its own thread
        if mon.status().state in ("running", "degraded") and mon.fed:
            break
        await asyncio.sleep(0.02)
    await asyncio.sleep(0.05)

    # the scanned folder stands in for a whole volume, sized so that every block is drawn: its used
    # space is what the tree holds plus a fixed part outside it (unknown space), read when asked
    root, store = handle.roots[0], handle.store
    seen = int(store.cols["tot_alloc"][root.node])
    hidden = seen // 3
    total = 2 * seen + hidden

    def used() -> int:
        return int(store.cols["tot_alloc"][root.node]) + hidden

    def volume(_path: str) -> VolumeInfo:
        return VolumeInfo(root.path, "", "test", "", total, total - used(), 4096, False, False)

    monkeypatch.setattr(monitors, "volume_for", volume)
    root.whole_volume, root.volume = True, volume(root.path)

    vc = ViewClient(client)
    _, first = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=handle.id, viewport=pv.Viewport(width=800, height=600))
    )
    assert first is not None

    def space(frame: object) -> tuple[int, int]:
        sizes = frame.columns["size"]  # type: ignore[attr-defined]
        free, unknown = frame.row(KEY_FREE), frame.row(KEY_UNKNOWN)  # type: ignore[attr-defined]
        return (
            int(sizes[free]) if free is not None else 0,
            int(sizes[unknown]) if unknown is not None else 0,
        )

    assert space(first) == (total - used(), hidden)

    async def deleted(name: str) -> None:
        """Delete ``name``: a pushed frame draws the freed space as free space, unknown stays."""
        (facts.root / name).unlink()
        end = time.monotonic() + 10
        while True:
            pushed = await asyncio.wait_for(vc.pushed.get(), max(0.1, end - time.monotonic()))
            async with asyncio.timeout(10):
                frame = await vc.frame(pushed.view_id, pushed.seq)
            if store.find(str(facts.root / name)) is None and space(frame) == (total - used(), hidden):
                break

    await deleted("big.bin")
    # a read of the free space just happened: the next change is read a moment later
    monkeypatch.setattr(monitors, "FREE_REFRESH_S", 0.8)
    mon._free_at = time.monotonic()
    await deleted("second.bin")
    status = server.scans.scans[handle.id].status()
    assert (status.free, status.unknown) == (total - used(), hidden)  # the status bar agrees


async def test_views_follow_whether_their_scan_runs(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    scan_id = await scanned(tmp_path, client)
    await finished(server, scan_id)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    view = server.views.views[update.state.view_id].view
    assert not view.scanning  # opened after the scan ended: no folder is marked as being scanned
    server.views._on_scan(scan_id, "scanning")
    assert view.scanning
    assert (await vc.pushed.get()).reason == "scan"
    server.views._on_scan(scan_id, "done")
    assert not view.scanning
    assert (await vc.pushed.get()).reason == "scan-final"


async def test_views_belong_to_their_session_and_scan(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    view_id = update.state.view_id
    assert [v.view_id for v in (await client.call(VIEW_LIST, pv.ViewListParams())).views] == [view_id]
    from .conftest import CLIENT, TOKEN

    other = await Connection.open(server.url, TOKEN, CLIENT)
    try:
        with pytest.raises(RemoteError) as err:
            await other.call(VIEW_RENDER, pv.ViewIdParams(view_id=view_id))
        assert err.value.code == ErrorCode.UNKNOWN_VIEW
    finally:
        await other.close()
    await client.call(VIEW_CLOSE, pv.ViewIdParams(view_id=view_id))
    with pytest.raises(RemoteError):
        await client.call(VIEW_RENDER, pv.ViewIdParams(view_id=view_id))
    # closing the scan closes its views
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    await client.call(SCAN_CLOSE, p.ScanIdParams(scan_id=scan_id))
    assert not server.views.views
    with pytest.raises(RemoteError) as err:
        await client.call(
            VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=8, height=8))
        )
    assert err.value.code == ErrorCode.UNKNOWN_SCAN


# ------------------------------------------------------------------ visualizations


@pytest.fixture
def fake_visuals() -> Iterator[None]:
    """Test-only visualizations on the treemap engine; ``test.rooted`` hands its rows out as flame.v1."""
    for v in FAKES:
        registry.register(v)
    try:
        yield
    finally:
        for v in FAKES:
            registry.unregister(v.id)


async def test_switching_the_visualization(tmp_path: Path, fake_visuals: None, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, first = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=1000, height=700))
    )
    assert first is not None and update.frame is not None
    assert (update.state.visual, update.frame.visual, update.frame.layout) == (
        "treemap",
        "treemap",
        "drawlist.v1",
    )
    assert update.frame.geometry is None and update.frame.root is None
    assert first.columns["branch"].dtype == np.uint8
    view_id = update.state.view_id
    key = int(first.columns["key"][row_named(first, "a")])
    await vc.call(VIEW_ZOOM, pv.ViewZoomParams(view_id=view_id, key=key))
    await vc.call(VIEW_SELECT, pv.ViewSelectParams(view_id=view_id, key=key + 1))
    set_visual = pv.ViewSetParams(view_id=view_id, settings=pv.ViewSettingsParams(visual="test.rooted"))
    update, frame = await vc.call(VIEW_SET, set_visual)
    assert update.changed and frame is not None and update.frame is not None
    info = update.frame
    assert (info.reason, info.visual, info.layout) == ("visual", "test.rooted", "flame.v1")
    assert isinstance(info.geometry, pv.FlameGeometry) and info.root is not None
    assert (info.root.key, info.root.kind) == (key, 1)  # the zoom root: folder "a"
    assert {"x", "y", "w", "h", "branch"} <= set(frame.columns)
    st = update.state
    assert (st.visual, st.zoom_node, st.selected, st.can_back, st.color_mode) == (
        "test.rooted",
        key,
        key + 1,
        True,
        "branch",
    )
    update, frame = await vc.call(VIEW_SET, set_visual)  # the same again: nothing to draw
    assert not update.changed and frame is None
    update, frame = await vc.call(
        VIEW_SET,
        pv.ViewSetParams(
            view_id=view_id, settings=pv.ViewSettingsParams(visual="treemap", color_mode="time")
        ),
    )
    assert update.frame is not None and update.frame.reason == "visual" and update.state.color_mode == "time"
    update, _ = await vc.call(
        VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=pv.ViewSettingsParams(detail=3))
    )
    assert update.frame is not None and update.frame.reason == "settings"
    with pytest.raises(RemoteError) as err:
        await vc.call(
            VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=pv.ViewSettingsParams(visual="pie"))
        )
    assert err.value.code == ErrorCode.INVALID_PARAMS
    assert (await client.call(VIEW_LIST, pv.ViewListParams())).views[0].visual == "treemap"


async def test_a_view_opened_like_another_keeps_every_colour_mode(
    tmp_path: Path, fake_visuals: None, client: Connection
) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    viewport = pv.Viewport(width=800, height=600)
    update, _ = await vc.call(VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=viewport))
    view_id = update.state.view_id
    for settings in (
        pv.ViewSettingsParams(color_mode="time"),
        pv.ViewSettingsParams(visual="test.rooted", color_mode="flat"),
    ):
        update, _ = await vc.call(VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=settings))
    st = update.state
    assert {"treemap": "time", "test.rooted": "flat"}.items() <= st.colors.items()
    # a split pane / a reconnect opens with what the old view carried (actions.ts carried())
    like = pv.ViewSettingsParams(visual=st.visual, colors=st.colors, color_mode=st.color_mode)
    update, _ = await vc.call(VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=viewport, settings=like))
    assert (update.state.visual, update.state.color_mode) == ("test.rooted", "flat")
    update, _ = await vc.call(
        VIEW_SET,
        pv.ViewSetParams(view_id=update.state.view_id, settings=pv.ViewSettingsParams(visual="treemap")),
    )
    assert update.state.color_mode == "time"
    # unknown visualizations in the map are left out
    update, _ = await vc.call(
        VIEW_OPEN,
        pv.ViewOpenParams(
            scan_id=scan_id, viewport=viewport, settings=pv.ViewSettingsParams(colors={"pie": "flat"})
        ),
    )
    assert "pie" not in update.state.colors


async def test_switching_while_frames_are_pushed(
    tmp_path: Path, server: Server, fake_visuals: None, client: Connection
) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    view_id = update.state.view_id
    server.views._on_scan(scan_id, "scanning")
    pushed = await vc.pushed.get()
    assert (pushed.reason, pushed.layout) == ("scan", "drawlist.v1")
    update, frame = await vc.call(
        VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=pv.ViewSettingsParams(visual="test.rooted"))
    )
    assert frame is not None and update.frame is not None and update.frame.reason == "visual"
    assert update.frame.root is not None
    server.views._on_scan(scan_id, "done")
    pushed = await vc.pushed.get()
    assert (pushed.reason, pushed.visual, pushed.layout) == ("scan-final", "test.rooted", "flame.v1")
    assert pushed.root is not None and not pushed.root.flags & F_SCANNING
    frame = await vc.frame(pushed.view_id, pushed.seq)  # the client decodes the other layout too
    assert len(frame) == pushed.rows


async def test_older_frontends_stay_on_the_treemap(
    tmp_path: Path, server: Server, fake_visuals: None
) -> None:
    from .conftest import CLIENT, TOKEN

    old = await Connection.open(
        server.url, TOKEN, CLIENT, capabilities=[CAP_BINARY_FRAMES, CAP_SUBSCRIPTIONS]
    )
    try:
        scan_id = await scanned(tmp_path, old)
        vc = ViewClient(old)
        update, frame = await vc.call(
            VIEW_OPEN,
            pv.ViewOpenParams(
                scan_id=scan_id,
                viewport=pv.Viewport(width=800, height=600),
                settings=pv.ViewSettingsParams(visual="test.rooted", color_mode="branch"),
            ),
        )
        assert frame is not None and update.frame is not None
        assert (update.state.visual, update.state.color_mode, update.frame.layout) == (
            "treemap",
            "class",
            "drawlist.v1",
        )
        view_id = update.state.view_id
        for settings in (
            pv.ViewSettingsParams(visual="test.fake"),
            pv.ViewSettingsParams(color_mode="branch"),
        ):
            with pytest.raises(RemoteError) as err:
                await vc.call(VIEW_SET, pv.ViewSetParams(view_id=view_id, settings=settings))
            assert err.value.code == ErrorCode.INVALID_PARAMS
        view = server.views.views[view_id].view
        assert (view.visual, view.color_mode) == ("treemap", "class")  # nothing changed
    finally:
        await old.close()
