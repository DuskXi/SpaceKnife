"""The engine interface: what a visualization's layout engine gets and must return.

Built-in visualizations and (later) plugins implement :class:`Engine` and describe themselves with a
:class:`Visualization`, registered in :mod:`.registry`. Hosts (views, the server) reach engines only
through the registry and never import one directly. ``ENGINE_API`` changes when this interface
does.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Protocol

import numpy as np

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

    @property
    def title(self) -> str:
        return _(self.title_id)
