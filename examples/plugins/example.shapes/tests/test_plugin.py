"""example.shapes with the mock host: frames of both layouts, the hexagons, the root shape."""

from __future__ import annotations

from collections.abc import AsyncIterator

import numpy as np
import pytest

from spaceknife_sdk.testing import MockHost, make_tree

TREE = make_tree(
    {
        "photos": {f"img{i}.jpg": b"x" * (40_000 + 1000 * i) for i in range(12)},
        "music": {"a.mp3": b"x" * 300_000, "b.mp3": b"x" * 200_000},
        "notes.txt": b"x" * 20_000,
    }
)


@pytest.fixture
async def host() -> AsyncIterator[MockHost]:
    async with MockHost.load(__file__) as h:
        h.scan(TREE)
        yield h


async def test_pack_draws_circles_inside_their_folders(host: MockHost) -> None:
    frame = await host.visual("pack", width=800, height=600)  # also checks the row contract
    d = frame.draw
    assert d.layout == "shapes.v1"
    g = d.geometry
    assert len(d) >= 14  # two folders, their files, the text file
    assert (2 * g["r"] == np.minimum(g["w"], g["h"])).all()  # every block a circle
    assert d.meta.root is not None and d.meta.root.w == d.meta.root.h  # the zoom root's circle
    photos = d.row(host.find("photos"))
    assert photos is not None and d.kind[photos] == 1  # an open folder, its files inside
    assert int((d.parent == photos).sum()) == 12


async def test_hexagons_are_polygons(host: MockHost) -> None:
    await host.set_setting("shape", "hexagon")
    frame = await host.visual("pack", width=800, height=600)
    poly = frame.draw.geometry["poly"]
    assert (poly == 6).all()
    assert frame.draw.points is not None and len(frame.draw.points) == 2 * 6 * len(frame.draw)


async def test_icicle_bands(host: MockHost) -> None:
    frame = await host.visual("icicle", width=800, height=400)
    d = frame.draw
    assert d.meta.nested is False
    g = d.geometry
    music = d.row(host.find("music"))
    assert music is not None
    below = np.flatnonzero(d.parent == music)
    assert len(below) == 2 and (g["y"][below] > g["y"][music]).all()  # the next band down
    assert g["w"][below].sum() <= g["w"][music] + 4  # within the folder's width


async def test_in_the_plugin_process() -> None:
    async with MockHost.load(__file__, process=True) as h:
        h.scan(TREE)
        frame = await h.visual("pack", width=640, height=480)
        assert frame.draw.layout == "shapes.v1" and len(frame.draw) >= 14
        await h.check(strict=True)
