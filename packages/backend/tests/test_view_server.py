"""view.* over the real WebSocket: frames, navigation, filter, tags, pushes, ownership."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife.server import Server
from spaceknife.view.state import F_DIM, F_MATCH, F_SELECTED
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_client.views import ViewClient
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

from .test_scan_server import wait_done
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
