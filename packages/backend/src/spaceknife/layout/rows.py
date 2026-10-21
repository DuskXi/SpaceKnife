"""Rows of a draw list: what every layout emits, whatever it draws.

One row per drawn block, parents before their children. The common columns (key, node, parent,
kind, depth, class, label bits, size, count) mean the same in every layout; each wire layout adds its
own geometry columns (``GEOMETRY_COLUMNS``). See docs/protocol.md §4 for the row contract.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import IntEnum, IntFlag

import numpy as np

from spaceknife_protocol import view as pv

# Stable keys: node ids for real nodes; tagged ids for synthetic blocks.
KEY_SMALL = 1 << 30  # | container id: "N small items" of that container
KEY_MORE = 2 << 30  # | container id: "+N items" of that container
KEY_FREE = (3 << 30) | 1
KEY_UNKNOWN = (3 << 30) | 2
KEY_NODE_MASK = (1 << 30) - 1
# In an aggregate zoom the leftover "+N items" is KEY_MORE | its largest member instead: members are
# children of the container, so it differs from the container's blocks, and the zoomed block's own
# largest member is drawn on its own (RootSpec.key guards the rest), so it differs from that too.

PAGE_REST = 0.125  # aggregate zoom: the share of the view a shrunk "+N items" block keeps


class RectKind(IntEnum):
    FILE = 0
    FOLDER = 1  # expanded: frame + header (treemap) or solid bar / sector, children inside or beyond
    COLLAPSED = 2  # folder drawn as a solid block (detail level reached or too small)
    PENDING = 3  # folder not scanned yet
    SMALL = 4  # "N small items"
    MORE = 5  # "+N items"
    FREE = 6
    UNKNOWN = 7


RowKind = RectKind  # the name for layouts that draw no rectangles

FILE, FOLDER, COLLAPSED, PENDING_RECT = 0, 1, 2, 3  # RectKind values as plain ints (hot loop)


class Label(IntFlag):
    NONE = 0
    NAME = 1
    SIZE = 2  # second line with the size
    BIG = 4  # area > 52,000 px²: larger type
    SHORT = 8  # height < 22 px: name only, smaller type, centred vertically
    RADIAL = 16  # polar.v1: the name runs along the radius


L_NAME, L_SIZE, L_BIG, L_SHORT = int(Label.NAME), int(Label.SIZE), int(Label.BIG), int(Label.SHORT)
L_RADIAL = int(Label.RADIAL)


def label_box(cw: float, ch: float, big: bool = True) -> int:
    """Label bits of a ``cw`` × ``ch`` CSS px text box (UI-TM-14..16). ``big``: the larger type is
    allowed (treemap blocks; bars and sectors keep the normal size)."""
    if cw < 30 or ch < 15:
        return 0
    if ch < 22:
        return L_NAME | L_SHORT
    return L_NAME | (L_SIZE if cw >= 42 and ch >= 31 else 0) | (L_BIG if big and cw * ch > 52_000 else 0)


ROW_SCANS = 8  # DrawList.row: lookups per frame answered by scanning the keys, then they are indexed

COMMON_COLUMNS: tuple[tuple[str, type[np.generic]], ...] = (
    ("key", np.uint32),
    ("node", np.int32),
    ("parent", np.int32),
    ("kind", np.uint8),
    ("depth", np.uint8),
    ("cls", np.uint8),
    ("label", np.uint8),
    ("size", np.int64),
    ("count", np.uint32),
)

# geometry columns per wire layout, all i4
GEOMETRY_COLUMNS: dict[str, tuple[str, ...]] = {
    pv.DRAWLIST_LAYOUT: ("x", "y", "w", "h"),
    pv.FLAME_LAYOUT: ("x", "y", "w", "h"),
    pv.POLAR_LAYOUT: ("a0", "a1", "r0", "r1"),
}


@dataclass(slots=True)
class DrawList:
    """Columns of the draw list: one row per drawn block, parents before their children."""

    key: np.ndarray  # u4 stable id
    node: np.ndarray  # i4 node id (container id for SMALL/MORE, -1 for FREE/UNKNOWN)
    parent: np.ndarray  # i4 row of the enclosing FOLDER, -1 at the top
    kind: np.ndarray  # u1 RectKind
    depth: np.ndarray  # u1 nesting level, 1 = child of the zoom root
    cls: np.ndarray  # u1 index into the class table; 255 (NO_CLASS): a folder or block in no one class
    label: np.ndarray  # u1 Label bits
    size: np.ndarray  # i8 bytes (area basis, except a shrunk "+N items" in an aggregate zoom)
    count: np.ndarray  # u4 files in SMALL, entries in MORE (SMALL counted by its files)
    names: list[str]  # per row; "" where no label is drawn (SMALL/MORE/FREE/UNKNOWN: frontend text)
    geometry: dict[str, np.ndarray] = field(default_factory=dict)  # GEOMETRY_COLUMNS[layout], i4
    members: dict[int, np.ndarray] = field(default_factory=dict)  # SMALL/MORE key -> node ids
    width: int = 0  # physical px
    height: int = 0
    elapsed_s: float = 0.0
    layout: str = pv.DRAWLIST_LAYOUT  # wire layout of the rows
    visual: str = "treemap"  # the visualization that laid them out
    meta: pv.FlameGeometry | pv.PolarGeometry | None = None  # FrameInfo.geometry (None: drawlist.v1)
    levels: int = 0  # levels below the zoom root the layout could draw
    _index: tuple[np.ndarray, np.ndarray] | None = field(default=None, init=False, repr=False, compare=False)
    _lookups: int = field(default=0, init=False, repr=False, compare=False)

    def __len__(self) -> int:
        return len(self.key)

    @property
    def x(self) -> np.ndarray:
        return self.geometry["x"]

    @property
    def y(self) -> np.ndarray:
        return self.geometry["y"]

    @property
    def w(self) -> np.ndarray:
        return self.geometry["w"]

    @property
    def h(self) -> np.ndarray:
        return self.geometry["h"]

    def row(self, key: int) -> int | None:
        """The first row with ``key``. The first few lookups of a frame scan the key column (a render
        looks up one or two); after that the keys are indexed (change flashes look up hundreds)."""
        index = self._index
        if index is None:
            self._lookups += 1
            if self._lookups <= ROW_SCANS:
                hit = np.flatnonzero(self.key == key)
                return int(hit[0]) if len(hit) else None
            index = self._index = np.unique(self.key, return_index=True)
        keys, first = index
        i = int(np.searchsorted(keys, key))
        return int(first[i]) if i < len(keys) and keys[i] == key else None
