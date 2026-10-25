"""The engine interface: what a visualization's layout engine gets and must return.

Built-in visualizations and (later) plugins implement :class:`Engine` and describe themselves with a
:class:`Visualization`, registered in :mod:`.registry`. Hosts (views, the server) reach engines only
through the registry and never import one directly. ``ENGINE_API`` changes when this interface
does.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Protocol

import numpy as np

from spaceknife_protocol.text import Text
from spaceknife_protocol.view import ColorModeName

from ..i18n import _
from .basis import Basis
from .rows import DrawList

ENGINE_API = 1
MAX_ROWS = 50_000  # rows per frame of a partition layout (flame.v1, polar.v1); the treemap has none


@dataclass(frozen=True, slots=True)
class LayoutParams:
    width: float  # CSS px
    height: float
    dpr: float = 1.0
    detail: int = 5  # levels drawn below the zoom root (1–15)
    min_element: float = 14.0  # CSS px: blocks below min_element² are merged
    sort_by_size: bool = True
    gap: float = 2.0
    border: float = 1.0
    padding: float = 2.0
    header: float = 18.0
    min_draw: float = 2.0
    min_content: float = 4.0
    expand_min_w: float = 48.0
    expand_min_h: float = 40.0
    font_scale: float = 1.0  # 「元素字号」: header height and label thresholds scale with it
    aspect: float = 1.0  # 「元素比例」: the width / height cells aim for
    options: tuple[tuple[str, object], ...] = ()  # the visualization's own settings, by name

    def opt[T](self, name: str, default: T) -> T:
        """Option ``name``; ``default`` when it is not set or not of the default's type."""
        for key, value in self.options:
            if key == name:
                return value if isinstance(value, type(default)) else default
        return default


@dataclass(frozen=True, slots=True)
class RootSpec:
    """What fills the view: a node's children, or the members of an aggregate block."""

    node: int
    members: np.ndarray | None = None  # zoomed into a SMALL/MORE block: just these nodes
    free: int | None = None  # whole-volume root: free and unknown space blocks
    unknown: int | None = None
    key: int | None = None  # the SMALL/MORE block zoomed into: no block inside reuses its key


class SizeSource(Protocol):
    """Area basis per node for one run (bytes, i8)."""

    @property
    def n(self) -> int:
        """Nodes covered: ids from 0 to ``n`` - 1."""
        ...

    def of(self, ids: np.ndarray) -> np.ndarray:
        """Sizes of some nodes."""
        ...

    def full(self) -> np.ndarray:
        """Sizes of all ``n`` nodes (one O(N) array: engines that can, use :meth:`of`)."""
        ...


class StoreSizes:
    """The store's own sizes: subtree allocation for folders, own allocation for files."""

    __slots__ = ("basis", "n")

    def __init__(self, basis: Basis) -> None:
        self.basis = basis
        self.n = len(basis.store)

    def of(self, ids: np.ndarray) -> np.ndarray:
        return self.basis.sizes_of(ids)

    def full(self) -> np.ndarray:
        return self.basis.sizes()


class ArraySizes:
    """Sizes from an array: hide mode's matching bytes, a compare view's amounts."""

    __slots__ = ("n", "sizes")

    def __init__(self, sizes: np.ndarray) -> None:
        self.sizes = sizes
        self.n = len(sizes)

    def of(self, ids: np.ndarray) -> np.ndarray:
        out: np.ndarray = self.sizes[ids]
        return out

    def full(self) -> np.ndarray:
        return self.sizes


class ColumnSizes:
    """Areas by another measure of the store (variant visualizations): ``files`` counts files (a
    folder: the files below it), ``items`` files and folders, ``size`` logical bytes."""

    __slots__ = ("basis", "metric", "n")

    def __init__(self, basis: Basis, metric: str) -> None:
        if metric not in ("size", "files", "items"):
            raise ValueError(f"unknown size metric {metric!r}")
        self.basis = basis
        self.metric = metric
        self.n = len(basis.store)

    def of(self, ids: np.ndarray) -> np.ndarray:
        c = self.basis.store.cols
        is_dir = c["kind"][ids] == 1
        if self.metric == "size":
            dup = (c["flags"][ids] & np.uint32(4)) != 0  # Flag.HARDLINK_DUP
            out: np.ndarray = np.where(is_dir, c["tot_size"][ids], np.where(dup, 0, c["size"][ids]))
            return out.astype(np.int64)
        files = c["tot_files"][ids].astype(np.int64)
        if self.metric == "items":
            files = files + c["tot_dirs"][ids].astype(np.int64)
        out = np.where(is_dir, files, 1).astype(np.int64)
        return out

    def full(self) -> np.ndarray:
        return self.of(np.arange(self.n, dtype=np.int64))


def subtree_sums(store_cols: dict[str, np.ndarray], n: int, leaf: np.ndarray) -> np.ndarray:
    """Folder totals from per-node weights (folders' own values ignored), one pass per depth level."""
    kind = store_cols["kind"][:n]
    depth = store_cols["depth"][:n]
    parent = store_cols["parent"][:n]
    out = np.where(kind == 1, 0, np.asarray(leaf[:n], dtype=np.int64)).astype(np.int64)
    out = np.maximum(out, 0)
    for d in range(int(depth.max()) if n else 0, 0, -1):
        idx = np.nonzero(depth == d)[0]
        np.add.at(out, parent[idx], out[idx])
    return out


class Engine(Protocol):
    """Lays out one store for one visualization; built once per store and reused across frames."""

    def run(self, params: LayoutParams, root: RootSpec, sizes: SizeSource) -> DrawList:
        """The rows of ``root`` in the viewport, sized by ``sizes``; satisfies the row contract
        (:func:`.contract.check_draw`)."""
        ...

    def levels(self, params: LayoutParams) -> int:
        """Levels below the zoom root this viewport can show (at most ``params.detail``)."""
        ...


EngineFactory = Callable[[Basis], Engine]


@dataclass(frozen=True, slots=True)
class Visualization:
    """A visualization the server offers: its engine and the wire layout frontends draw it with."""

    id: str  # "treemap", "flame", "flame_polar"; plugins: dotted / dashed lowercase words
    api: int  # ENGINE_API it was written against
    layout: str  # wire layout of its frames ("drawlist.v1", "flame.v1", "polar.v1")
    capability: str  # what a session must have announced to get it ("" for the baseline drawlist.v1)
    title_id: str  # message id of its name (Chinese source text)
    icon: str  # design icon key
    order: int  # place in pickers, then id
    engine: EngineFactory
    default_color: ColorModeName = "class"  # colour mode a view starts with on it
    compare: bool = True  # can show compare views
    free_space: bool = True  # shows free and unknown space at a whole-volume root
    # own options: (LayoutParams.options name, settings key) — the flame graph's origin, a plugin's
    options: tuple[tuple[str, str], ...] = ()
    aliases: tuple[str, ...] = ()  # extra search words of its 「可视化：…」 command
    size_metric: str = "alloc"  # alloc (bytes on disk) | size | files | items | code (a plugin's weights)
    size_unit: str = "bytes"  # what the areas count: bytes | count
    sizes: Callable[[Basis], SizeSource] | None = None  # areas other than the store's own bytes
    plugin: str | None = None  # the plugin that adds it (None: built in)
    text: Text | None = None  # a plugin's name in both languages (title_id is the built-ins')
    # a plugin's ``when``: palette needs of its 「可视化：…」 command (each gives why it does not
    # hold), and the conditions as written (visual.list: frontends grey it out with the reason)
    needs: tuple[Callable[[Any], str | None], ...] = ()
    when: tuple[str, ...] = ()

    @property
    def title(self) -> str:
        if self.text is not None:
            return self.text.get(_lang())
        return _(self.title_id)


def _lang() -> str:
    from ..i18n import language

    return language()
