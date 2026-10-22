"""The visualization registry: registration rules, availability per session, ``visual.list``."""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import replace

import pytest

from spaceknife import i18n
from spaceknife.layout import registry
from spaceknife.layout.api import ENGINE_API, Visualization
from spaceknife.layout.engine import TreemapEngine
from spaceknife.server import Server
from spaceknife.view.state import showable
from spaceknife_client.connection import Connection
from spaceknife_protocol import (
    CAP_BINARY_FRAMES,
    CAP_CANCEL,
    CAP_LAYOUT_FLAME,
    CAP_LAYOUT_POLAR,
    CAP_LOG_STREAM,
    CAP_SUBSCRIPTIONS,
    CAP_VISUALS,
)
from spaceknife_protocol import view as pv
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import VISUAL_LIST

from .conftest import CLIENT, TOKEN

FAKE = Visualization(
    id="test.fake",
    api=ENGINE_API,
    layout=pv.DRAWLIST_LAYOUT,
    capability="",
    title_id="树图",
    icon="vizTreemap",
    order=90,
    engine=TreemapEngine,
)
GATED = replace(FAKE, id="test.gated", layout=pv.FLAME_LAYOUT, capability=CAP_LAYOUT_FLAME, order=91)


@pytest.fixture
def fakes() -> Iterator[None]:
    registry.register(FAKE)
    registry.register(GATED)
    try:
        yield
    finally:
        registry.unregister(FAKE.id)
        registry.unregister(GATED.id)


def test_the_treemap_is_built_in() -> None:
    tree = registry.get("treemap")
    assert (tree.layout, tree.capability, tree.icon, tree.default_color) == (
        pv.DRAWLIST_LAYOUT,
        "",
        "vizTreemap",
        "class",
    )
    assert registry.all_visuals()[0] is tree
    with pytest.raises(KeyError):
        registry.get("pie")


@pytest.mark.parametrize(
    ("bad", "why"),
    [
        (replace(FAKE, id="treemap"), "already registered"),
        (replace(FAKE, id="test.other", api=ENGINE_API + 1), "engine API"),
        (replace(FAKE, id="Test"), "lowercase"),
        (replace(FAKE, id="a" * 65), "lowercase"),  # too long
        (replace(FAKE, id="test.other", layout="drawlist"), "layout"),  # no version
    ],
)
def test_register_refuses(bad: Visualization, why: str) -> None:
    with pytest.raises(ValueError, match=why):
        registry.register(bad)
    assert registry.get("treemap").engine is TreemapEngine


def test_availability_follows_the_capabilities(fakes: None) -> None:
    plain = registry.available([CAP_BINARY_FRAMES])
    assert [v.id for v in plain][:1] == ["treemap"] and FAKE in plain and GATED not in plain
    assert all(not v.capability for v in plain)
    flame = registry.available([CAP_LAYOUT_FLAME])
    assert flame.index(FAKE) < flame.index(GATED)  # by order
    # a session without visuals.v1 never leaves the treemap
    assert showable("treemap", []) is registry.get("treemap")
    assert showable("test.fake", [CAP_LAYOUT_FLAME]) is None
    assert showable("test.fake", [CAP_VISUALS]) is FAKE
    assert showable("test.gated", [CAP_VISUALS]) is None
    assert showable("test.gated", [CAP_VISUALS, CAP_LAYOUT_FLAME]) is GATED
    assert showable("pie", [CAP_VISUALS, CAP_LAYOUT_FLAME]) is None


async def test_visual_list(fakes: None, server: Server, client: Connection) -> None:
    listed = (await client.call(VISUAL_LIST, Empty())).visuals
    by_id = {v.id: v for v in listed}
    assert [v.id for v in listed][:1] == ["treemap"] and {"test.fake", "test.gated"} <= set(by_id)
    tree = by_id["treemap"]
    assert tree.available and tree.reason is None and tree.name == "树图"
    assert (tree.layout, tree.icon, tree.default_color, tree.compare, tree.free_space) == (
        pv.DRAWLIST_LAYOUT,
        "vizTreemap",
        "class",
        True,
        True,
    )
    assert by_id["test.gated"].available  # the test client announces every capability
    try:
        i18n.set_language("en")
        tree = next(v for v in (await client.call(VISUAL_LIST, Empty())).visuals if v.id == "treemap")
        assert tree.name == "Treemap"
    finally:
        i18n.set_language("zh-CN")
    # an older frontend: only the treemap, the rest say why not
    old = await Connection.open(server.url, TOKEN, CLIENT, capabilities=[CAP_BINARY_FRAMES])
    try:
        by_id = {v.id: v for v in (await old.call(VISUAL_LIST, Empty())).visuals}
    finally:
        await old.close()
    assert by_id["treemap"].available
    assert not by_id["test.fake"].available and by_id["test.fake"].reason == "这种可视化在当前界面不可用"
    assert not by_id["test.gated"].available and by_id["test.gated"].reason


async def test_the_built_ins_for_the_electron_renderer(server: Server) -> None:
    """What the renderer announces (rpc.ts: the base four plus visual/registry.ts) gets all three."""
    caps = [CAP_BINARY_FRAMES, CAP_SUBSCRIPTIONS, CAP_CANCEL, CAP_LOG_STREAM, CAP_VISUALS]
    caps += [CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR]
    conn = await Connection.open(server.url, TOKEN, CLIENT, capabilities=caps)
    try:
        assert {CAP_VISUALS, CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR} <= set(conn.hello.capabilities)
        listed = (await conn.call(VISUAL_LIST, Empty())).visuals
    finally:
        await conn.close()
    assert [(v.id, v.layout, v.icon, v.default_color, v.available) for v in listed] == [
        ("treemap", pv.DRAWLIST_LAYOUT, "vizTreemap", "class", True),
        ("flame", pv.FLAME_LAYOUT, "vizFlame", "branch", True),
        ("flame_polar", pv.POLAR_LAYOUT, "vizFlamePolar", "branch", True),
    ]
    assert [v.name for v in listed] == ["树图", "火焰图", "火焰-极坐标图"]
    assert all(v.reason is None and v.compare and v.free_space for v in listed)
