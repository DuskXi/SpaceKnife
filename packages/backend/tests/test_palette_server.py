"""palette.* over the real WebSocket."""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

import pytest

from spaceknife.filter.engine import FilterContext
from spaceknife.server import Server
from spaceknife_client.connection import Connection
from spaceknife_client.views import ViewClient
from spaceknife_protocol import palette as pp
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import (
    PALETTE_CLEAR,
    PALETTE_PATHS,
    PALETTE_PREVIEW,
    PALETTE_QUERY,
    PALETTE_REMEMBER,
    TREE_FIND,
    VIEW_FILTER,
    VIEW_OPEN,
    VIEW_SELECT,
)
from spaceknife_protocol.scan import TreeFindParams

from .test_view_server import scanned


@pytest.fixture(autouse=True)
def isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


async def test_palette_round_trip(tmp_path: Path, client: Connection) -> None:
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=1000, height=700))
    )
    view_id = update.state.view_id
    ctx = pp.PaletteContext(tab="scan", scan_id=scan_id, view_id=view_id, basket=3)

    # empty input: the nine commands, no recents, no path group without a selection
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="", context=ctx))
    assert [c.id for c in res.commands][:3] == ["view.rescan", "scan.rescan_all", "view.cycle_color"]
    assert len(res.commands) == 9 and res.recent == [] and res.direct == [] and not res.paths
    basket = next(c for c in res.commands if c.id == "basket.open")
    assert basket.status.startswith("3 项")

    # a selection seeds the path group
    movie = (
        await client.call(
            TREE_FIND,
            TreeFindParams(scan_id=scan_id, path=str(tmp_path / "root" / "a" / "sub" / "movie.mp4")),
        )
    ).node
    assert movie is not None
    await vc.call(VIEW_SELECT, pv.ViewSelectParams(view_id=view_id, key=movie.id))
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="", context=ctx))
    assert res.paths and res.seed == "movie.mp4"
    found = await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=scan_id, view_id=view_id, limit=5))
    assert found.seed == "movie.mp4" and movie.id not in [i.node for i in found.items]

    # typed: commands by pinyin, paths by name
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="qhps", context=ctx))
    assert res.commands[0].id == "view.cycle_color" and res.commands[0].hits == [pp.Span(start=0, end=4)]
    found = await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=scan_id, text="movie"))
    assert found.items[0].name == "movie.mp4" and found.items[0].tier == 1 and not found.partial

    # filter direct item + preview
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="*.mp4;>100kb", context=ctx))
    assert res.direct[0].kind == "filter" and res.direct[0].valid
    bad = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text=">500xb", context=ctx))
    assert not bad.direct[0].valid and bad.direct[0].diagnostics[0].code == "unknown_unit"
    preview = await client.call(PALETTE_PREVIEW, pp.PalettePreviewParams(scan_id=scan_id, text="*.mp4"))
    update, _ = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text="*.mp4"))
    assert update.filter is not None
    assert (
        (preview.files, preview.alloc)
        == (update.filter.files, update.filter.alloc)
        == (1, update.filter.alloc)
    )

    # path direct item and folder listing
    root = str(tmp_path / "root")
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text=root, context=ctx))
    assert res.direct[0].kind == "path" and res.direct[0].state == "in_scan" and res.direct[0].is_dir
    folder = str(tmp_path / "root" / "a") + os.sep
    listing = await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=scan_id, text=folder))
    assert listing.folder is not None and {i.name for i in listing.items} == {"one.bin", "two.bin", "sub"}

    # recents: remembered, matched, sized; then cleared
    await client.call(PALETTE_REMEMBER, pp.PaletteRememberParams(kind="filter", text="*.mp4"))
    await client.call(
        PALETTE_REMEMBER,
        pp.PaletteRememberParams(kind="jump", text=str(tmp_path / "root" / "a"), scan_id=scan_id),
    )
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="", context=ctx))
    assert [r.kind for r in res.recent] == ["jump", "filter"]
    assert res.recent[0].exists and res.recent[0].alloc and res.recent[1].tokens
    await client.call(PALETTE_CLEAR, pp.PaletteClearParams(scan_id=scan_id))
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="", context=ctx))
    assert res.recent == []

    # outside a scan tab: no paths, scan commands disabled
    res = await client.call(
        PALETTE_QUERY, pp.PaletteQueryParams(text="", context=pp.PaletteContext(tab="settings"))
    )
    save = next(c for c in res.commands if c.id == "snapshot.save")
    assert not save.enabled and not res.paths


async def select_seed(tmp_path: Path, client: Connection, path: Path) -> tuple[str, str, int]:
    """Scan, open a view and select ``path``: (scan id, view id, node id)."""
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=1000, height=700))
    )
    node = (await client.call(TREE_FIND, TreeFindParams(scan_id=scan_id, path=str(path)))).node
    assert node is not None
    await vc.call(VIEW_SELECT, pv.ViewSelectParams(view_id=update.state.view_id, key=node.id))
    return scan_id, update.state.view_id, node.id


async def test_selected_scan_root_seeds_with_its_last_segment(tmp_path: Path, client: Connection) -> None:
    scan_id, view_id, root = await select_seed(tmp_path, client, tmp_path / "root")  # stored as a full path
    ctx = pp.PaletteContext(tab="scan", scan_id=scan_id, view_id=view_id)
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="", context=ctx))
    assert res.paths and res.seed == "root"
    found = await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=scan_id, view_id=view_id))
    assert found.seed == "root" and root not in [i.node for i in found.items]


@pytest.mark.skipif(sys.platform == "win32", reason="'?' cannot be part of a Windows file name")
async def test_selected_name_is_searched_literally(tmp_path: Path, client: Connection) -> None:
    (tmp_path / "root").mkdir()
    for name in ("what?.txt", "whatX.txt", "what?.txt.bak"):
        (tmp_path / "root" / name).write_bytes(b"x")
    scan_id, view_id, _ = await select_seed(tmp_path, client, tmp_path / "root" / "what?.txt")
    found = await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=scan_id, view_id=view_id))
    assert found.seed == "what?.txt" and [i.name for i in found.items] == ["what?.txt.bak"]


async def test_nodes_found_before_they_are_sized(
    tmp_path: Path, server: Server, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    scan_id = await scanned(tmp_path, client)
    sub = str(tmp_path / "root" / "a" / "sub")
    await client.call(PALETTE_REMEMBER, pp.PaletteRememberParams(kind="jump", text=sub, scan_id=scan_id))
    node = (await client.call(TREE_FIND, TreeFindParams(scan_id=scan_id, path=sub))).node
    assert node is not None
    shared = server.views.shared(scan_id)
    assert shared is not None
    sizes = shared.layout.sizes()[: node.id]  # taken while the scan had not reached "sub" yet
    monkeypatch.setattr(shared.layout, "sizes", lambda: sizes)
    ctx = pp.PaletteContext(tab="scan", scan_id=scan_id)
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text="", context=ctx))
    jump = res.recent[0]
    assert (jump.kind, jump.exists, jump.node, jump.alloc) == ("jump", True, node.id, None)
    res = await client.call(PALETTE_QUERY, pp.PaletteQueryParams(text=sub, context=ctx))
    assert (res.direct[0].state, res.direct[0].node) == ("outside", None)  # not scanned yet
    listing = await client.call(PALETTE_PATHS, pp.PalettePathsParams(scan_id=scan_id, text=sub + os.sep))
    assert listing.items == [] and listing.total == 0


async def test_preview_of_an_age_filter_follows_the_clock(
    tmp_path: Path, client: Connection, monkeypatch: pytest.MonkeyPatch
) -> None:
    scan_id = await scanned(tmp_path, client)
    params = pp.PalettePreviewParams(scan_id=scan_id, text="*.mp4;<1d")  # modified within a day
    assert (await client.call(PALETTE_PREVIEW, params)).files == 1
    later = int(time.time()) + 10 * 86400
    monkeypatch.setattr(FilterContext, "current", classmethod(lambda cls, **kw: cls(now=later, **kw)))
    assert (await client.call(PALETTE_PREVIEW, params)).files == 0
