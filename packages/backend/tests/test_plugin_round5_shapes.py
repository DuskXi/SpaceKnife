"""Round 5, drawing a plugin's own layout (docs/plugins.md 「试写第四轮之后的修正」, gap D): the room
the host's labels take (``LayoutInput.label``), the line inside every shape's edge (``Shapes.stroke``
→ ``ShapesGeometry.stroke``), top labels that would overlap the name of the shape around them, in the
plugin process and in process. The frontend's half (selection rings, the line, the root's name) is
visual/shapes/shapes.test.ts."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import replace
from pathlib import Path

import numpy as np
import pytest

from spaceknife.layout.rows import L_NAME, L_TOP
from spaceknife.layout.shapes import Hierarchy, Placed
from spaceknife_protocol import view as pv
from spaceknife_sdk import LabelSpace, LayoutInput, Shapes, shapes
from spaceknife_sdk.shapes import LabelMode
from spaceknife_sdk.testing import MockHost, make_tree

from .store_builder import build, find
from .test_layout_shapes import MB, PARAMS, engine, from_shapes, run

pytestmark = pytest.mark.timeout(240)


# --------------------------------------------------------------------------- the label's room


def test_a_layout_is_told_the_room_labels_take() -> None:
    cols = {"node": np.zeros(0, dtype=np.int64)}
    v = LayoutInput.from_wire({"dpr": 2.0, "font_scale": 1.25}, cols, None)
    lu = 2.5
    assert v.label == LabelSpace(font=11 * lu, line=18 * lu, lines=34 * lu, top=19 * lu)
    # the same numbers the host decides with (layout/shapes.py) and draws with (visual/shapes)
    assert v.label.top == (pv.SHAPE_LABEL_TOP_INSET + pv.SHAPE_LABEL_TOP_BAND) * lu
    assert v.label.lines == pv.SHAPE_LABEL_TWO_LINES * lu
    # made by hand (an author's own test) it is worked out all the same
    made = LayoutInput(*(np.zeros(0) for _ in range(8)), dpr=1.0)
    assert made.label.top == 19.0 and made.label.font == 11.0


def nested_rects(
    step: float, *, first: float = 40, root: bool = False, label: LabelMode = "auto"
) -> Callable[[LayoutInput], Shapes]:
    """Every block a rectangle inside its folder's, ``first`` px below the canvas top and ``step``
    px lower per level (an open folder names itself at the top with "auto"); with ``root`` the zoom
    root is the whole canvas."""

    def place(v: LayoutInput) -> Shapes:
        out = v.shapes()
        if root:
            out.rect(-1, 0, 0, 800, 600)
        d = (v.depth - 1).astype(np.float64)
        top = first + step * d
        out.rects(np.arange(len(v)), 10 + 20 * d, top, 700 - 40 * d, 500 - top, 4, label=label)
        return out

    return place


SPEC = {"d": {"e": {"a.bin": 10 * MB, "c.bin": 2 * MB}, "b.bin": 5 * MB}}


def test_a_top_label_too_close_to_the_one_around_it_is_left_out() -> None:
    store = build(SPEC)
    d_node, e_node = find(store, "d"), find(store, "d/e")
    # a folder 8 px below its folder's top: its name would sit on the other one
    close = run(engine(store, from_shapes(nested_rects(8)), fill=1.0))
    d, e = close.row(d_node), close.row(e_node)
    assert d is not None and e is not None
    assert close.label[d] & L_TOP and close.names[d] == "d"
    assert close.label[e] == 0 and close.names[e] == ""
    # a label's height lower (LayoutInput.label.top, 19 lu): both names
    far = run(engine(store, from_shapes(nested_rects(19)), fill=1.0))
    d, e = far.row(d_node), far.row(e_node)
    assert d is not None and e is not None
    assert far.label[d] & L_TOP and far.label[e] & L_TOP and far.names[e] == "e"
    # the files' labels in the middle are not top labels: untouched
    a = close.row(find(store, "d/e/a.bin"))
    assert a is not None and close.label[a] & L_NAME


def test_the_zoom_roots_name_counts_too() -> None:
    store = build(SPEC)
    d_node, e_node = find(store, "d"), find(store, "d/e")
    # the root's name and size run along its top (4 to 21 px): a folder whose name starts at 14 px
    near = run(engine(store, from_shapes(nested_rects(19, first=10, root=True)), fill=1.0))
    d, e = near.row(d_node), near.row(e_node)
    assert d is not None and e is not None
    assert near.label[d] == 0
    assert near.label[e] & L_TOP  # 29 px down: clear of the root's name (d's is not written)
    clear = run(engine(store, from_shapes(nested_rects(19, first=17, root=True)), fill=1.0))
    d = clear.row(d_node)
    assert d is not None and clear.label[d] & L_TOP
    # where a folder's blocks lie elsewhere (an icicle) nothing lies around a label: all are written
    flat = run(engine(store, from_shapes(nested_rects(8, label="top")), fill=1.0, nested=False))
    rows = [flat.row(find(store, n)) for n in ("d", "d/e")]
    assert all(r is not None and flat.label[r] & L_TOP for r in rows)


@pytest.mark.parametrize("font_scale", [1.0, 1.5])
def test_the_sdk_packing_keeps_the_roots_name_clear(font_scale: float) -> None:
    store = build({f"f{i}": {"a.bin": (5 + i) * MB, "b.bin": 3 * MB} for i in range(12)})
    params = replace(PARAMS, dpr=2.0, font_scale=font_scale)
    d = run(engine(store, from_shapes(shapes.pack), fill=0.5), params)
    assert isinstance(d.meta, pv.ShapesGeometry) and d.meta.root is not None
    room = 19 * 2.0 * font_scale  # v.label.top
    g = d.geometry
    top = np.flatnonzero(d.depth == 1)
    assert len(top) >= 6 and (g["y"][top] >= d.meta.root.y + room - 1).all()


# --------------------------------------------------------------------------- the line


def placed_with(stroke: float) -> Callable[[Hierarchy], Placed]:
    inner = from_shapes(shapes.pack)

    def place(h: Hierarchy) -> Placed:
        p = inner(h)
        p.stroke = stroke
        return p

    return place


@pytest.mark.parametrize(
    ("stroke", "dpr", "drawn"),
    [
        (1.0, 1.0, 1.0),
        (1.0, 2.0, 2.0),
        (0.0, 2.0, 0.0),
        (2.5, 1.5, 3.75),
        (99.0, 1.0, 4.0),
        (float("nan"), 1.0, 0.0),
    ],
)
def test_the_line_is_physical_px_and_bounded(stroke: float, dpr: float, drawn: float) -> None:
    store = build(SPEC)
    d = run(engine(store, placed_with(stroke), fill=0.5), replace(PARAMS, dpr=dpr))
    assert isinstance(d.meta, pv.ShapesGeometry)
    assert d.meta.stroke == pytest.approx(drawn)


@pytest.mark.parametrize("stroke", [-1, 4.5, float("nan"), "wide", None])
def test_a_line_out_of_bounds_is_said_to_the_author(stroke: object) -> None:
    out = Shapes(1)
    assert out.stroke == 1.0 and out.to_wire()[0]["stroke"] == 1.0
    out.stroke = stroke  # type: ignore[assignment]
    with pytest.raises(ValueError, match=r"Shapes\.stroke is 0 to 4 CSS px"):
        out.to_wire()


# --------------------------------------------------------------------------- a plugin


TOML = """id = "t.r5shapes"
version = "0.1.0"
name = { zh = "第五轮形状", en = "Round five shapes" }
entry = "plugin.py"
permissions = []
"""

CODE = """from __future__ import annotations

import numpy as np

from spaceknife_sdk import LayoutInput, Plugin, Shapes, T, shapes

plugin = Plugin()


@plugin.layout("pack", T("圆堆积", "Packing"), fill=0.5)
def pack(v: LayoutInput) -> Shapes:
    out = shapes.pack(v)
    out.stroke = 0.5
    return out


@plugin.layout("plain", T("默认", "Default"), fill=0.5)
def plain(v: LayoutInput) -> Shapes:
    return shapes.pack(v)


@plugin.layout("rooms", T("标签", "Labels"), fill=1.0)
def rooms(v: LayoutInput) -> Shapes:
    # each open folder one label lower than the one around it: every name shows
    out = v.shapes()
    room = v.label.top
    d = (v.depth - 1).astype(np.float64)
    out.rect(-1, 0, 0, v.width, v.height)
    top = room + room * d
    out.rects(np.arange(len(v)), 10 + 20 * d, top, v.width - 20 - 40 * d, v.height - top - 10, 4)
    return out


@plugin.layout("thick", T("太粗", "Too thick"))
def thick(v: LayoutInput) -> Shapes:
    out = shapes.pack(v)
    out.stroke = 9
    return out
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    folder = tmp_path / "t.r5shapes"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


TREE = make_tree(
    {
        "photos": {"2021": {"a.jpg": b"a" * 300_000, "b.jpg": b"b" * 200_000}, "c.jpg": b"c" * 100_000},
        "notes.txt": b"n" * 50_000,
    }
)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_plugin_sets_its_line_and_names_every_folder(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin / "plugin.py", process=process) as h:
        h.scan(TREE)
        meta = (await h.visual("pack", width=600, height=400)).draw.meta
        assert isinstance(meta, pv.ShapesGeometry) and meta.stroke == 0.5
        meta = (await h.visual("plain", width=600, height=400)).draw.meta
        assert isinstance(meta, pv.ShapesGeometry) and meta.stroke == pv.SHAPE_STROKE
        # spaced by v.label.top, no folder's name is left out
        d = (await h.visual("rooms", width=800, height=600)).draw
        folders = [i for i in range(len(d)) if d.kind[i] == 1]
        assert len(folders) >= 2 and all(d.label[i] & L_TOP for i in folders)
        assert {d.names[i] for i in folders} >= {"photos", "2021"}
        # a line it may not have: said in full
        with pytest.raises(AssertionError, match=r"Shapes\.stroke is 0 to 4 CSS px \(got 9\)"):
            await h.visual("thick")
