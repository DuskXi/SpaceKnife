"""Plugin layouts (``shapes.v1``, layout/shapes.py): the hierarchy, the checks on the answer, labels,
the cache, the wire frame. The placers here are plain functions: the SDK's circle packing and a few
hand-made answers (the plugin-process path is example.shapes' tests and test_plugin_layouts.py)."""

from __future__ import annotations

from collections.abc import Callable

import numpy as np
import pytest

from spaceknife.layout import Basis, LayoutParams, RectKind, RootSpec, StoreSizes
from spaceknife.layout.contract import check_draw
from spaceknife.layout.rows import KEY_SMALL, L_NAME, L_SHORT, L_SIZE, L_TOP, DrawList
from spaceknife.layout.shapes import (
    BOX,
    CENTRE,
    POLYGON,
    TOP,
    Hierarchy,
    Placed,
    PlaceError,
    ShapesEngine,
    band_width,
    convex,
    label_bits,
)
from spaceknife_protocol import view as pv
from spaceknife_protocol.binary import decode_frame
from spaceknife_sdk import shapes as sk
from spaceknife_sdk.shapes import LayoutInput, Shapes

from .store_builder import build, find
from .test_layout_golden import tree

MB = 1 << 20
PARAMS = LayoutParams(width=800, height=600)


def from_shapes(fn: Callable[[LayoutInput], Shapes]) -> Callable[[Hierarchy], Placed]:
    """A placer from an SDK layout function, through the wire format as the runtime sends it."""

    def place(h: Hierarchy) -> Placed:
        v = LayoutInput.from_wire({**h.params(), "options": dict(h.options)}, h.columns(), None)
        value, cols, points = fn(v).to_wire()
        root = value["root"]
        return Placed(
            shape=cols["shape"],
            x=cols["x"],
            y=cols["y"],
            w=cols["w"],
            h=cols["h"],
            r=cols["r"],
            poly=cols["poly"],
            points=points,
            label=cols["label"],
            root=None if root is None else (*root[:5], tuple(root[5])),
            message=value["message"],
        )

    return place


SPEC = {
    "photos": {f"img{i}.jpg": (2 + i) * MB for i in range(10)},
    "music": {"a.mp3": 30 * MB, "b.mp3": 20 * MB, "deep": {"c.mp3": 8 * MB}},
    "notes.txt": 3 * MB,
}


def engine(store: object, place: Callable[[Hierarchy], Placed], **kw: object) -> ShapesEngine:
    return ShapesEngine(Basis(store), "test.shapes", place, **kw)  # type: ignore[arg-type]


def run(e: ShapesEngine, params: LayoutParams = PARAMS, root: RootSpec | None = None) -> DrawList:
    store = e.basis.store
    spec = root or RootSpec(node=next(iter(store.roots())))
    draw = e.run(params, spec, StoreSizes(e.basis))
    problems = check_draw(draw, store, spec, full=True)
    assert not problems, problems
    return draw


def test_circle_packing_keeps_the_row_contract() -> None:
    store = build(SPEC)
    d = run(engine(store, from_shapes(sk.pack), fill=0.5))
    assert d.layout == pv.SHAPES_LAYOUT
    g = d.geometry
    assert (2 * g["r"] == g["w"]).all() and (g["w"] == g["h"]).all()  # circles stay circles
    assert isinstance(d.meta, pv.ShapesGeometry) and d.meta.root is not None
    music = d.row(find(store, "music"))
    assert music is not None and d.kind[music] == RectKind.FOLDER
    kids = np.flatnonzero(d.parent == music)
    cx, cy, r = g["x"][music] + g["r"][music], g["y"][music] + g["r"][music], g["r"][music]
    for k in kids.tolist():  # inside the folder's circle
        kx, ky = g["x"][k] + g["r"][k], g["y"][k] + g["r"][k]
        assert np.hypot(kx - cx, ky - cy) + g["r"][k] <= r + 2


def test_a_block_without_a_shape_takes_its_contents_along() -> None:
    store = build(SPEC)
    music = find(store, "music")

    def place(v: LayoutInput) -> Shapes:
        out = sk.pack(v)
        out.clear(int(np.flatnonzero(v.node == music)[0]))
        return out

    d = run(engine(store, from_shapes(place), fill=0.5))
    assert d.row(music) is None
    assert d.row(find(store, "music/a.mp3")) is None and d.row(find(store, "music/deep")) is None
    assert d.row(find(store, "photos")) is not None


def test_a_folder_left_with_nothing_inside_is_collapsed() -> None:
    store = build(SPEC)
    photos = find(store, "photos")

    def place(v: LayoutInput) -> Shapes:
        out = sk.pack(v)
        for i in v.children(int(np.flatnonzero(v.node == photos)[0])).tolist():
            out.clear(i)
        return out

    d = run(engine(store, from_shapes(place), fill=0.5))
    row = d.row(photos)
    assert row is not None and d.kind[row] == RectKind.COLLAPSED


def test_the_hierarchy_is_what_the_partition_engines_pick() -> None:
    store = build(SPEC)
    seen: list[Hierarchy] = []

    def place(h: Hierarchy) -> Placed:
        seen.append(h)
        return Placed.empty(len(h))

    run(engine(store, place, fill=1.0))
    h = seen[0]
    assert (h.parent < np.arange(len(h))).all()  # parents first
    assert h.root_kind == RectKind.FOLDER and h.root_size == int(Basis(store).sizes_of(np.array([0]))[0])
    top = h.parent == -1
    assert np.isclose(h.u1[top].max(), 1.0) and np.isclose(h.u0[top].min(), 0.0)
    assert (h.width, h.height) == (800, 600)


def test_small_items_merge_by_area() -> None:
    files = {f"f{i}.bin": 1000 for i in range(400)} | {"big.bin": 400 * MB}
    store = build({"d": files})
    seen: list[Hierarchy] = []

    def place(h: Hierarchy) -> Placed:
        seen.append(h)
        return from_shapes(sk.pack)(h)

    d = run(engine(store, place, fill=0.5))
    assert (seen[0].kind == RectKind.SMALL).sum() == 1  # the 400 tiny files are one block
    small = int(np.flatnonzero(d.kind == RectKind.SMALL)[0])
    assert d.count[small] == 400 and int(d.key[small]) & KEY_SMALL


def test_row_budget() -> None:
    store = tree(5000)
    seen: list[int] = []

    def place(h: Hierarchy) -> Placed:
        seen.append(len(h))
        return Placed.empty(len(h))

    run(engine(store, place, max_rows=200, fill=1.0), LayoutParams(width=3840, height=2160, detail=15))
    assert 0 < seen[0] <= 200


def test_answers_are_cached_by_what_they_depend_on() -> None:
    store = build(SPEC)
    calls: list[int] = []
    epoch = [0]

    def place(h: Hierarchy) -> Placed:
        calls.append(len(h))
        return from_shapes(sk.pack)(h)

    e = engine(store, place, fill=0.5, key=lambda: epoch[0])
    run(e)
    run(e)
    assert len(calls) == 1  # the same picture: asked once
    run(e, LayoutParams(width=640, height=480))
    assert len(calls) == 2  # another canvas
    epoch[0] += 1
    run(e, LayoutParams(width=640, height=480))
    assert len(calls) == 3  # the plugin's settings changed


def test_failures_draw_nothing_and_say_why() -> None:
    store = build(SPEC)

    def fails(h: Hierarchy) -> Placed:
        raise PlaceError("too slow")

    d = run(engine(store, fails))
    assert len(d) == 0 and isinstance(d.meta, pv.ShapesGeometry) and d.meta.message == "too slow"

    def short(h: Hierarchy) -> Placed:
        return Placed.empty(len(h) - 1)

    d = run(engine(store, short))
    assert len(d) == 0 and isinstance(d.meta, pv.ShapesGeometry) and "values for" in d.meta.message


def test_polygons_are_checked() -> None:
    store = build({"a.bin": 10 * MB, "b.bin": 10 * MB, "c.bin": 10 * MB})

    def place(v: LayoutInput) -> Shapes:
        out = v.shapes()
        out.polygon(0, [(10, 10), (200, 10), (200, 200), (10, 200)])
        out.polygon(1, [(300, 10), (500, 10), (320, 30), (500, 200), (300, 200)])  # not convex
        out.polygon(2, [(600, 300), (700, 400), (600, 500), (500, 400)])
        return out

    d = run(engine(store, from_shapes(place), fill=1.0))
    assert len(d) == 2 and (d.geometry["poly"] == 4).all()
    assert d.points is not None and d.points.tolist()[:8] == [10, 10, 200, 10, 200, 200, 10, 200]
    first = d.row(find(store, "a.bin"))
    assert first is not None and d.geometry["x"][first] == 10 and d.geometry["w"][first] == 190


def test_convex() -> None:
    assert convex(np.array([[0, 0], [10, 0], [10, 10], [0, 10]], dtype=float))
    assert convex(np.array([[0, 0], [0, 10], [10, 10], [10, 0]], dtype=float))  # either orientation
    assert not convex(np.array([[0, 0], [10, 0], [2, 2], [10, 10], [0, 10]], dtype=float))
    star = [(np.cos(a), np.sin(a)) for a in np.arange(5) * 4 * np.pi / 5]  # turns twice
    assert not convex(np.array(star) * 50)
    assert not convex(np.array([[0, 0], [5, 0], [10, 0]], dtype=float))  # no area


@pytest.mark.parametrize(
    ("kind", "w", "h", "r", "mode", "bits"),
    [
        ("box", 200, 80, 4, CENTRE, L_NAME | L_SIZE),
        ("box", 200, 80, 40, CENTRE, L_NAME | L_SIZE),  # a pill: the band in the middle is wide
        ("box", 80, 80, 40, CENTRE, L_NAME | L_SIZE),  # a circle of 80: 2 lines at its widest
        ("box", 50, 50, 25, CENTRE, L_NAME | L_SHORT),  # one line through it
        ("box", 30, 30, 15, CENTRE, 0),  # too small for one line
        ("box", 300, 300, 4, TOP, L_NAME | L_TOP),
        ("box", 300, 40, 4, TOP, 0),  # too low for a name over contents
        ("poly", 200, 200, 0, CENTRE, L_NAME | L_SIZE),
    ],
)
def test_label_bits(kind: str, w: float, h: float, r: float, mode: int, bits: int) -> None:
    assert label_bits(kind, w, h, r, mode, 1.0) & ~4 == bits  # (the large type aside)


def test_band_width() -> None:
    assert band_width("box", 100, 50, 0, 10, 20) == 100
    assert band_width("box", 100, 100, 50, 50, 50.5) == pytest.approx(100, abs=0.5)  # a circle's middle
    assert band_width("box", 100, 100, 50, 10, 20) == pytest.approx(60, abs=0.5)  # near its top
    assert band_width("box", 100, 100, 50, 0, 10) == 0  # the band reaches its very top
    assert band_width("box", 100, 100, 50, -1, 10) == 0  # out of the shape


def test_labels_and_names() -> None:
    store = build({"big.bin": 500 * MB, "tiny.bin": 1 * MB})
    d = run(engine(store, from_shapes(sk.pack), fill=0.5))
    big, tiny = d.row(find(store, "big.bin")), d.row(find(store, "tiny.bin"))
    assert big is not None and d.label[big] & L_NAME and d.names[big] == "big.bin"
    assert tiny is not None and d.label[tiny] == 0 and d.names[tiny] == ""


def test_open_rect_folders_name_themselves_at_the_top() -> None:
    store = build({"d": {"a.bin": 10 * MB, "b.bin": 5 * MB}})

    def place(v: LayoutInput) -> Shapes:
        out = v.shapes()
        inset = 20 * (v.depth - 1)
        out.rects(np.arange(len(v)), 10 + inset, 10 + 2 * inset, 600 - 2 * inset, 400 - 3 * inset, 4)
        return out

    d = run(engine(store, from_shapes(place), fill=1.0))
    folder = d.row(find(store, "d"))
    assert folder is not None and d.label[folder] & L_TOP


def test_wire_frame_carries_the_points() -> None:
    from spaceknife.layout.rows import COMMON_COLUMNS
    from spaceknife.server.views import encode_draw
    from spaceknife.view.state import Frame

    store = build({"a.bin": 10 * MB, "b.bin": 4 * MB})

    def place(v: LayoutInput) -> Shapes:
        out = v.shapes()
        out.polygon(0, [(10, 10), (300, 10), (150, 300)])
        out.rect(1, 400, 10, 100, 100, radius=50)
        return out

    d = run(engine(store, from_shapes(place), fill=1.0))
    n = len(d)
    frame = Frame(
        draw=d,
        flags=np.zeros(n, np.uint16),
        tag=np.zeros(n, np.uint8),
        tone=np.zeros(n, np.uint8),
        branch=np.zeros(n, np.uint8),
        root=None,  # type: ignore[arg-type]
        selected_row=None,
        breadcrumb=[],
        filter_totals=None,
        scanned_total=0,
    )
    header, cols = decode_frame(encode_draw(frame, "v1", 1, None))
    assert header.layout == pv.SHAPES_LAYOUT
    assert {name for name, _ in COMMON_COLUMNS} | {"x", "y", "w", "h", "r", "poly"} <= set(cols)
    pts = np.frombuffer(cols["points"].tobytes(), dtype="<i4")
    assert pts.tolist() == [10, 10, 300, 10, 150, 300]
    assert BOX == 1 and POLYGON == 2
