"""The row contract (docs/protocol.md §4.1) for every registered visualization.

Each engine lays out the synthetic tree of the golden test in several situations — a whole volume
with free and unknown space, a folder deep down, an aggregate zoom and the "+N items" inside it, a
file as the zoom root, hide-mode sizes, a scan still running, a viewport too small for a level —
and :func:`check_draw` must find nothing wrong. A visualization registered later (a plugin) is
checked the same way.
"""

from __future__ import annotations

from collections.abc import Callable
from functools import cache

import numpy as np
import pytest

from spaceknife.classes import DEFAULT_CLASSES
from spaceknife.layout import registry
from spaceknife.layout.api import (
    ArraySizes,
    Engine,
    LayoutParams,
    RootSpec,
    SizeSource,
    StoreSizes,
    Visualization,
)
from spaceknife.layout.basis import Basis
from spaceknife.layout.contract import check_draw
from spaceknife.layout.rows import KEY_MORE, KEY_NODE_MASK, KEY_SMALL, DrawList, RectKind
from spaceknife.store.nodes import Kind

from .test_layout_golden import ROOT, hide_sizes, tree

VISUALS = registry.all_visuals()
FHD = LayoutParams(width=1920, height=1080)
UHD = LayoutParams(width=2560, height=1440, dpr=1.5, detail=15)
SMALL_VIEW = LayoutParams(width=640, height=400, dpr=1.25, detail=8, sort_by_size=False)
TINY = LayoutParams(width=30, height=30)

Run = Callable[[Engine, Basis], tuple[DrawList, RootSpec, SizeSource]]


@cache
def basis(pending: bool = False) -> Basis:
    return Basis(tree(pending_every=3 if pending else 0), DEFAULT_CLASSES)


@cache
def engine(visual: str, pending: bool = False) -> Engine:
    return registry.get(visual).engine(basis(pending))


def largest(b: Basis, kind: Kind) -> int:
    n = len(b.store)
    sizes = np.where(b.store.cols["kind"][:n] == kind, b.sizes(), -1)
    sizes[: ROOT + 1] = -1
    return int(np.argmax(sizes))


def aggregate(draw: DrawList, tag: int | None = None) -> RootSpec | None:
    """Zoom into the drawn aggregate block (of kind ``tag``, else any) with the most members, as a
    view does (``View.zoom_into``)."""
    rows = [
        r
        for r in np.flatnonzero((draw.kind == RectKind.SMALL) | (draw.kind == RectKind.MORE)).tolist()
        if tag is None or (int(draw.key[r]) & ~KEY_NODE_MASK) == tag
    ]
    if not rows:
        return None
    row = max(rows, key=lambda r: (int(draw.count[r]), -int(draw.key[r])))
    key = int(draw.key[row])
    return RootSpec(node=int(draw.node[row]), members=draw.members[key], key=key)


def run(
    e: Engine, b: Basis, params: LayoutParams, root: RootSpec, sizes: SizeSource | None = None
) -> tuple[DrawList, RootSpec, SizeSource]:
    s = sizes or StoreSizes(b)
    return e.run(params, root, s), root, s


def _volume(vis: Visualization) -> RootSpec:
    if not vis.free_space:
        return RootSpec(node=ROOT)
    return RootSpec(node=ROOT, free=2 << 30, unknown=500 << 20)


SCENARIOS: dict[str, Callable[[Visualization], tuple[DrawList, RootSpec, SizeSource]]] = {
    "fhd": lambda v: run(engine(v.id), basis(), FHD, RootSpec(node=ROOT)),
    "volume": lambda v: run(engine(v.id), basis(), FHD, _volume(v)),
    "volume_small_by_name": lambda v: run(engine(v.id), basis(), SMALL_VIEW, _volume(v)),
    "folder_uhd": lambda v: run(engine(v.id), basis(), UHD, RootSpec(node=largest(basis(), Kind.DIR))),
    "file_root": lambda v: run(engine(v.id), basis(), FHD, RootSpec(node=largest(basis(), Kind.FILE))),
    "hide": lambda v: run(
        engine(v.id), basis(), FHD, RootSpec(node=ROOT), ArraySizes(hide_sizes(basis().store))
    ),
    "pending": lambda v: run(engine(v.id, True), basis(True), UHD, RootSpec(node=ROOT)),
    # no band or ring fits: a partition draws the zoom root alone
    "tiny": lambda v: run(engine(v.id), basis(), TINY, _volume(v)),
}


@pytest.mark.parametrize("vis", VISUALS, ids=[v.id for v in VISUALS])
@pytest.mark.parametrize("scenario", list(SCENARIOS))
def test_the_row_contract_holds(vis: Visualization, scenario: str) -> None:
    draw, root, sizes = SCENARIOS[scenario](vis)
    assert draw.layout == vis.layout
    assert check_draw(draw, basis(scenario == "pending").store, root, sizes=sizes) == []
    if scenario == "pending":
        assert (draw.kind == RectKind.PENDING).any()


@pytest.mark.parametrize("vis", VISUALS, ids=[v.id for v in VISUALS])
@pytest.mark.parametrize("tag", [KEY_SMALL, KEY_MORE])
def test_the_row_contract_holds_in_aggregate_zooms(vis: Visualization, tag: int) -> None:
    e, b = engine(vis.id), basis()
    sizes = StoreSizes(b)
    draw = e.run(FHD, RootSpec(node=ROOT), sizes)
    zoom = aggregate(draw, tag)
    if zoom is None:
        pytest.skip(f"{vis.id} draws no such aggregate at 1080p")
    inner = e.run(FHD, zoom, sizes)
    assert check_draw(inner, b.store, zoom, sizes=sizes) == []
    assert ((inner.kind <= RectKind.PENDING) & (inner.depth == 1)).any()  # members of its own
    leftover = aggregate(inner)  # the "+N items" inside, zoomed into again
    if leftover is not None:
        again = e.run(FHD, leftover, sizes)
        assert check_draw(again, b.store, leftover, sizes=sizes) == []


@pytest.mark.parametrize("vis", VISUALS, ids=[v.id for v in VISUALS])
def test_levels_never_exceed_the_detail(vis: Visualization) -> None:
    e = engine(vis.id)
    for params in (FHD, UHD, SMALL_VIEW, LayoutParams(width=120, height=90, detail=15)):
        assert 0 <= e.levels(params) <= params.detail
        assert e.run(params, RootSpec(node=ROOT), StoreSizes(basis())).levels == e.levels(params)
