"""Round 4, visualizations (docs/plugins.md 「第三轮之后的改进」): a plugin's own layout
(``@plugin.layout``, shapes.v1) in the plugin process and in process, what happens when it fails or
answers badly, its options, ``check``; a visualization's ``when``; ``colors.of`` for web panels."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.r4vis"
version = "0.1.0"
name = { zh = "第四轮可视化", en = "Round four visuals" }
entry = "plugin.py"
permissions = ["tree.read"]

[[settings]]
name = "fancy"
title = { zh = "花哨", en = "Fancy" }
default = false

[[settings]]
name = "gap"
title = { zh = "间距", en = "Gap" }
default = 2

[[color_modes]]
name = "texts"
title = { zh = "文字", en = "Texts" }
by = "ext"
palette = ["muted", "accent"]
map = { txt = 1 }
"""

CODE = """from __future__ import annotations

import numpy as np

from spaceknife_sdk import LayoutInput, Plugin, Shapes, T, shapes

plugin = Plugin()


@plugin.layout("pack", T("圆堆积", "Packing"), fill=0.5, options=["gap"])
def pack(v: LayoutInput) -> Shapes:
    return shapes.pack(v, padding=float(v.options.get("gap", 2)))


@plugin.layout("grid", T("网格", "Grid"), fill=1.0, nested=False, when=["setting:fancy"])
def grid(v: LayoutInput) -> Shapes:
    out = v.shapes()
    top = v.children()
    w = v.width / max(1, len(top))
    out.rects(top, np.arange(len(top)) * w, 0, w - 2, v.height / 2)
    # names come from the tree: a layout may read it with tree.read
    assert all(v.tree.node(int(n)).name for n in v.node[top] if n > 0)
    return out


@plugin.layout("broken", T("坏的", "Broken"))
def broken(v: LayoutInput) -> Shapes:
    raise ValueError("no room")


@plugin.layout("short", T("短的", "Short"))
def short(v: LayoutInput) -> Shapes:
    return Shapes(max(0, len(v) - 1))
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    folder = tmp_path / "t.r4vis"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


TREE = make_tree(
    {
        "a": {"x.bin": b"x" * 40_000, "y.bin": b"y" * 30_000},
        "b": {"z.bin": b"z" * 20_000},
        "c.txt": b"c" * 10_000,
    }
)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_layout_of_its_own(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin / "plugin.py", process=process) as h:
        h.scan(TREE)
        frame = await h.visual("pack", width=600, height=400)
        d = frame.draw
        assert d.layout == "shapes.v1" and len(d) >= 6
        assert (d.geometry["w"] == d.geometry["h"]).all()
        # its option changes the picture (a setting the plugin names in options)
        before = d.geometry["r"].copy()
        await h.set_setting("gap", 8)
        after = (await h.visual("pack", width=600, height=400)).draw.geometry["r"]
        assert after.sum() < before.sum()


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_failing_layout_says_why(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin / "plugin.py", process=process) as h:
        h.scan(TREE)
        # what it raised by name (the traceback is in the plugin's log), the SDK's own checks in full
        with pytest.raises(AssertionError, match=r"the layout drew nothing: .*ValueError"):
            await h.visual("broken")
        with pytest.raises(
            AssertionError, match=r"the layout drew nothing: .*Shapes for \d+ blocks, the layout got"
        ):
            await h.visual("short")


async def test_when_on_a_visualization(plugin: Path) -> None:
    async with MockHost.load(plugin / "plugin.py") as h:
        h.scan(TREE)
        assert await h.visual_available("pack") is None
        assert await h.visual_available("grid") == "需要先打开相应的设置"
        await h.set_setting("fancy", True)
        assert await h.visual_available("grid") is None
        d = (await h.visual("grid", width=600, height=400)).draw
        assert d.meta is not None and d.meta.nested is False  # type: ignore[union-attr]
        # visual.list tells the frontends the conditions
        from spaceknife.layout import registry

        assert registry.get("t.r4vis.grid").when == ("setting:fancy",)


async def test_check_finds_a_layout_without_its_function(tmp_path: Path) -> None:
    from spaceknife.plugins.check import check_plugin

    folder = tmp_path / "t.r4nofn"
    folder.mkdir()
    (folder / "plugin.toml").write_text(
        TOML.replace("t.r4vis", "t.r4nofn")
        + '\n[[visualizations]]\nname = "own"\ntitle = { zh = "自带", en = "Own" }\nbase = "shapes"\n',
        encoding="utf-8",
    )
    (folder / "plugin.py").write_text(
        "from spaceknife_sdk import Plugin\n\nplugin = Plugin()\n", encoding="utf-8"
    )
    result = check_plugin(folder)
    assert any("has no layout function" in i.text() for i in result.issues)


async def test_colors_of_for_web_panels(plugin: Path) -> None:
    from spaceknife.view.colors import node_colors
    from spaceknife_protocol import view as pv

    async with MockHost.load(plugin / "plugin.py") as h:
        h.scan(TREE)
        await h._ready()
        assert h.scan_id is not None
        vhost = h.server.views.open(
            h.session, pv.ViewOpenParams(scan_id=h.scan_id, viewport=pv.Viewport(width=600, height=400))
        )
        view = vhost.view
        view.render()  # the branch slots are the frames'
        a, x, txt = h.find("a"), h.find("a/x.bin"), h.find("c.txt")
        r = node_colors(view, [a, x, txt, 10_000_000], "class")
        assert r.kind == [2, 0, 0, -1]
        assert r.depth[:3] == [1, 2, 1]
        assert r.branch[0] == r.branch[1] != 0  # x.bin lies in a: the same branch
        assert r.cls[3] == 255 and r.pal is None
        # a plugin's colour mode: its palette index, asked as for drawn rows
        p = node_colors(view, [x, txt], "t.r4vis.texts")
        assert p.palette == "t.r4vis.texts" and p.pal == [0, 1]
        h.server.views.close(view.id)


def test_engine_answers_are_bounded() -> None:
    """A plugin can not send more than the blocks it got, nor huge coordinates."""
    from spaceknife.layout import Basis, LayoutParams, RootSpec, StoreSizes
    from spaceknife.layout.shapes import Hierarchy, Placed, ShapesEngine

    from .store_builder import build

    store = build({"a.bin": 1 << 20, "b.bin": 1 << 19})

    def place(h: Hierarchy) -> Placed:
        p = Placed.empty(len(h))
        p.shape[:] = 1
        p.x[:] = 1e30
        p.w[:] = 1e30
        p.h[:] = 10
        return p

    e = ShapesEngine(Basis(store), "t.v", place)
    d = e.run(LayoutParams(width=100, height=100), RootSpec(node=0), StoreSizes(e.basis))
    assert np.abs(d.geometry["x"].astype(np.int64)).max() <= 1 << 24
