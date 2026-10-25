"""Views (one per tab / split pane) and their frames.

Every method that changes a view answers with a :class:`ViewUpdate` (JSON) and, when the picture
changed, sends one binary frame (stream ``"view:<view_id>"``, layout ``FrameInfo.layout``) whose ``seq``
equals ``ViewUpdate.frame.seq`` and whose ``request_id`` is the request's id. Frames pushed while a scan
is running carry no ``request_id`` and arrive with a ``view.frame`` event.

A view shows one visualization (``ViewState.visual``, listed by ``visual.list``); each visualization
emits one wire layout. Every layout has the same common columns (one row per drawn block; parents come
before their children) plus its own geometry columns:

=========  =====  =====================================================================
column     dtype  meaning
=========  =====  =====================================================================
key        <u4    stable id: node id, or SMALL/MORE/FREE/UNKNOWN tagged id (top 2 bits)
node       <i4    node id (container for SMALL/MORE, -1 for FREE/UNKNOWN)
parent     <i4    row of the enclosing expanded folder, -1 at the top
kind       <u1    0 file · 1 expanded folder (drawn as the layout says) · 2 collapsed folder
                  · 3 pending folder · 4 "N small items" · 5 "+N items" · 6 free space · 7 unknown space
depth      <u1    nesting level, 1 = child of the zoom root (level colours: min(depth, 4))
cls        <u1    class index (``filter.vocab`` order) of files and small-item blocks, and of folders a
                  folder class swallows; 255: in no one class (other folders, mixed blocks, "+N items")
label      <u1    bits: 1 name · 2 size line · 4 large type · 8 short (name only, centred)
                  · 16 radial (``polar.v1`` only: the text runs along the radius)
                  · 32 top (``shapes.v1`` only: the name sits at the top inside the shape)
size       <i8    bytes shown (area basis; matched bytes in hide mode)
count      <u4    files in "N small items", entries in "+N items"
flags      <u2    1 match · 2 dimmed · 4 folder-mask hit (bold) · 8 own tag · 16 selected
                  · 32 ancestor of the selection (halo) · 64 scan running and the folder's subtree is
                  not fully listed yet (folder blocks up to 3 levels below the zoom root)
                  · 128 in a recycle bin / trash, or the bin itself (aggregates: by their container)
                  · 256 the bin folder itself (it carries the badge of what emptying it frees)
tag        <u1    own tag: 0 none, 1 red, 2 yellow, 3 green, 4 blue
tone       <u1    modification-age band for time colouring: 0 < 1 d … 4 ≥ 1 y / unknown
branch     <u1    top-level branch colour slot: 0 neutral, 1–8 (shade: min(depth, 4))
name_len   <u4    byte length of this row's name in the ``names`` blob (0: no label)
delta      <i8    compare views only: net change of allocated bytes
dstat      <u1    compare views only: 0 unchanged · 1 new · 2 deleted · 3 changed
=========  =====  =====================================================================

Blob ``names``: the rows' UTF-8 names, concatenated in row order.

Geometry columns (``<i4``, whole physical pixels of a ``width`` × ``height`` canvas):

* ``drawlist.v1`` (treemap): ``x y w h`` rectangles; kind 1 is a frame with a header, its children
  inside. ``FrameInfo.geometry`` is None.
* ``flame.v1``: ``x y w h`` bars, one band per level; kind 1 is an expanded solid bar, its children in
  the next band. ``FrameInfo.geometry`` is a :class:`FlameGeometry`.
* ``polar.v1``: ``a0 a1`` angles (:data:`ANGLE_UNIT` per turn, 0 = 12 o'clock, increasing clockwise on
  screen) and ``r0 r1`` radii from the centre (the ring gap already taken off); kind 1 is an expanded
  solid sector, its children in the next ring. ``FrameInfo.geometry`` is a :class:`PolarGeometry`.
* ``shapes.v1`` (a plugin's own layout: circle packing, icicles, cells …): ``x y w h`` the row's
  bounding box, ``r`` its corner radius (0 a rectangle; ``r = w/2 = h/2`` a circle) and ``poly`` the
  number of points of a convex polygon drawn instead of the rounded rectangle (0: none; at most
  :data:`MAX_POLY_POINTS`). The points follow in blob ``points``: ``<i4`` x, y pairs of all rows with
  ``poly > 0``, in row order. Rows are painted in row order, so a later row lies over an earlier one;
  the pointer hits the last row whose shape holds it. Kind 1 is an expanded folder drawn as its shape
  (its children inside it when ``ShapesGeometry.nested``). ``FrameInfo.geometry`` is a
  :class:`ShapesGeometry`.

In ``flame.v1`` and ``polar.v1`` the zoom root itself is the root bar / centre disc described by
``FrameInfo.root``; it is not a row. In ``shapes.v1`` it is drawn the same way when
``ShapesGeometry.root`` gives its shape.
"""

from __future__ import annotations

from typing import Annotated, Literal

from pydantic import Field

from .base import Params, Payload
from .compare import CompareDirection
from .filter import FilterDiagnostic, TagColor

DRAWLIST_LAYOUT = "drawlist.v1"
FLAME_LAYOUT = "flame.v1"
POLAR_LAYOUT = "polar.v1"
SHAPES_LAYOUT = "shapes.v1"
ANGLE_UNIT = 1 << 20  # polar.v1 angle units per turn: every value stays exact in an f32
MAX_POLY_POINTS = 12  # shapes.v1: points of one polygon at most (Voronoi cells, hexagons, triangles)
# shapes.v1 labels, in lu = dpr · font scale: where the host writes one (layout/shapes.py), how the
# frontend draws it (visual/shapes) and what a plugin's layout is told (the SDK's LayoutInput.label)
SHAPE_LABEL_FONT = 11.0  # a name's type size (13 in a big label, 10.5 on a single line)
SHAPE_LABEL_ONE_LINE = 18  # the band a one-line centred label needs (and 30 lu of width)
SHAPE_LABEL_TWO_LINES = 34  # the band a centred name-and-size label needs (and 42 lu of width)
SHAPE_LABEL_TOP_INSET = 4  # a top label's band starts this far below its shape's top edge
SHAPE_LABEL_TOP_BAND = 15  # and is this high (it needs 40 lu of width, the shape 60 lu of height)
SHAPE_STROKE = 1.0  # CSS px: the line around each shape unless the layout asks for another
SHAPE_STROKE_MAX = 4.0  # CSS px: the widest a layout may ask for
BUILTIN_VISUALS = ("treemap", "flame", "flame_polar")
VIEW_STREAM_PREFIX = "view:"


def view_stream(view_id: str) -> str:
    return VIEW_STREAM_PREFIX + view_id


ID_PATTERN = r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$"
VisualId = Annotated[str, Field(min_length=1, max_length=64, pattern=ID_PATTERN)]
BuiltinColorMode = Literal["class", "flat", "time", "branch"]
BUILTIN_COLOR_MODES: tuple[BuiltinColorMode, ...] = ("class", "flat", "time", "branch")
# An open id (colors.v1): the built-in modes, or a plugin's full id (``example.media.media``).
ColorModeId = Annotated[str, Field(min_length=1, max_length=64, pattern=ID_PATTERN)]
ColorModeName = ColorModeId  # the older name
SizeUnit = Literal["bytes", "count", "weight"]  # weight: a plugin's own measure (labels show shares)
FilterModeName = Literal["dim", "hide"]
ResultsSortName = Literal["size", "name", "count"]
ResultKind = Literal["file", "folder", "group", "rest"]

_VIEW_ID = Field(min_length=1, max_length=64)


class Viewport(Params):
    width: float = Field(gt=0, le=32768, description="CSS px")
    height: float = Field(gt=0, le=32768, description="CSS px")
    dpr: float = Field(default=1.0, ge=0.5, le=8)


class ViewSettingsParams(Params):
    """Fields left out keep their value."""

    detail: int | None = Field(default=None, ge=1, le=15)
    min_element: float | None = Field(default=None, ge=3, le=20, description="CSS px")
    sort_by_size: bool | None = None
    show_free: bool | None = None
    show_unknown: bool | None = None
    show_trash: bool | None = Field(
        default=None, description="The recycle bins: drawn, or taken out of the picture"
    )
    visual: VisualId | None = Field(default=None, description="A visual.list id (needs visuals.v1)")
    color_mode: ColorModeId | None = Field(
        default=None,
        description="Of the view's current visualization; 'branch' needs visuals.v1, a plugin's colors.v1",
    )
    colors: dict[str, ColorModeId] | None = Field(
        default=None,
        description="Colour mode per visualization (a split pane or a reopened view keeps them); "
        "color_mode, when also given, wins for the current one",
    )
    filter_mode: FilterModeName | None = None
    viewport: Viewport | None = None
    direction: CompareDirection | None = Field(default=None, description="Compare views: changes shown")


class ViewOpenParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    viewport: Viewport
    settings: ViewSettingsParams | None = None


class ViewIdParams(Params):
    view_id: str = _VIEW_ID


class ViewZoomParams(Params):
    view_id: str = _VIEW_ID
    key: int = Field(ge=0, lt=2**32, description="A block's key from the last frame, or a node id")


class ViewSelectParams(Params):
    view_id: str = _VIEW_ID
    key: int | None = Field(default=None, ge=0, lt=2**32)


class ViewSetParams(Params):
    view_id: str = _VIEW_ID
    settings: ViewSettingsParams


class ViewFilterParams(Params):
    view_id: str = _VIEW_ID
    text: str = Field(max_length=8192)
    cursor: int | None = Field(default=None, ge=0)


class ViewTagParams(Params):
    view_id: str = _VIEW_ID
    color: Literal["red", "yellow", "green", "blue"] | None = Field(
        default=None, description="None clears the selection's tag and every tag below it (Ctrl+0)"
    )


class ViewListParams(Params):
    scan_id: str | None = None


class ViewRevealParams(Params):
    view_id: str = _VIEW_ID
    key: int = Field(ge=0, lt=2**32, description="A result row's key, or any node id")


class ViewColorsParams(Params):
    view_id: str = _VIEW_ID
    nodes: list[int] = Field(max_length=20_000, description="Node ids of the view's scan")
    mode: ColorModeId | None = Field(default=None, description="A colour mode (None: the view's own)")


class ViewColors(Payload):
    """What colours nodes in a view (``colors.of``), one entry per node asked for; the frontend
    turns it into colours of the theme on screen as the view paints its blocks (a web panel gets
    them from ``sk.colors``)."""

    mode: ColorModeId = Field(description="The colour mode these are for")
    palette: str | None = Field(default=None, description="A plugin's colour mode: 'pal' indexes its palette")
    kind: list[int] = Field(
        description="0 file · 2 folder · 3 folder not listed yet · -1 not a node of the scan"
    )
    cls: list[int] = Field(description="Class index (filter.vocab order), 255: in no one class")
    tone: list[int] = Field(description="Modification-age band, 0 < 1 d … 4 ≥ 1 y / unknown")
    branch: list[int] = Field(description="Top-level branch slot below the view's zoom root (0 neutral, 1–8)")
    depth: list[int] = Field(
        description="Levels below the zoom root (1: its child; 0: the root or outside it)"
    )
    pal: list[int] | None = Field(default=None, description="Palette index (a plugin's colour mode)")


class ViewResultsParams(Params):
    view_id: str = _VIEW_ID
    sort: ResultsSortName | None = Field(
        default=None, description="New order (remembered per view); None keeps it"
    )
    offset: int = Field(default=0, ge=0)
    limit: int = Field(default=100, ge=0, le=500)


class Crumb(Payload):
    key: int
    label: str = Field(description="Folder name; 'small:N' / 'more:N' for an aggregate block")


class FilterInfo(Payload):
    text: str
    active: bool
    stale: bool = Field(description="The text has errors; the previous valid filter is still shown")
    mode: FilterModeName
    files: int = 0
    alloc: int = 0
    size: int = 0
    diagnostics: list[FilterDiagnostic] = Field(default_factory=list)


class FlashItem(Payload):
    row: int = Field(
        description="Row of this frame's draw list: the changed block or its drawn ancestor; "
        "-1 = the zoom root (flame.v1 / polar.v1 only)"
    )
    delta: int = Field(description="Change of allocated bytes (the static badge under reduced motion)")


class PixelRect(Payload):
    x: int
    y: int
    w: int
    h: int


class FlameGeometry(Payload):
    """``flame.v1``: one band per level, the zoom root's bar in band 0."""

    kind: Literal["flame"] = "flame"
    origin: Literal["top", "bottom"] = Field(description="Side of the canvas holding the root bar")
    row_h: int = Field(description="Band pitch in physical px (bar height + gap)")
    rows: int = Field(description="Levels drawn beyond the root bar")
    gap: int = Field(description="Gap between bands and between neighbouring bars, physical px")
    root: PixelRect = Field(description="The zoom root's bar")


class PolarGeometry(Payload):
    """``polar.v1``: the zoom root is the centre disc, level k the k-th ring around it."""

    kind: Literal["polar"] = "polar"
    cx: int = Field(description="Centre, physical px")
    cy: int
    radius: int = Field(description="Outer edge of the last ring")
    hole: int = Field(description="Radius of the centre disc")
    ring_w: int = Field(description="Ring pitch: ring k spans [hole + (k-1)·ring_w, hole + k·ring_w)")
    rings: int = Field(description="Levels drawn around the centre disc")
    gap: int = Field(description="Gap between rings and between neighbouring sectors, physical px")
    unit: int = Field(default=ANGLE_UNIT, description="Angle units per turn")


class PixelShape(Payload):
    """A shape of ``shapes.v1``: a rounded rectangle, or a convex polygon (``points``) in its box."""

    x: int = Field(description="Bounding box, physical px")
    y: int
    w: int
    h: int
    r: int = Field(default=0, description="Corner radius (r = w/2 = h/2: a circle)")
    points: list[int] = Field(
        default_factory=list,
        max_length=2 * MAX_POLY_POINTS,
        description="x0 y0 x1 y1 … of a convex polygon drawn instead of the rounded rectangle",
    )


class ShapesGeometry(Payload):
    """``shapes.v1``: a plugin's own layout, drawn shape by shape."""

    kind: Literal["shapes"] = "shapes"
    root: PixelShape | None = Field(
        default=None,
        description="The zoom root's own shape (drawn and hit as FrameInfo.root); None: not drawn",
    )
    nested: bool = Field(
        default=True,
        description="A folder's rows lie inside its shape (circle packing); False: elsewhere (an icicle)",
    )
    message: str = Field(
        default="", max_length=300, description="Why nothing is drawn (the plugin failed or timed out)"
    )
    stroke: float = Field(
        default=0.0,
        ge=0,
        le=64,
        description="A line just inside the edge of each row's shape, in the canvas colour (the gaps "
        "between blocks), physical px; 0: none. It keeps a shape of its parent's colour visible",
    )


FrameGeometry = Annotated[FlameGeometry | PolarGeometry | ShapesGeometry, Field(discriminator="kind")]


class FrameRoot(Payload):
    """The zoom root of a flame.v1 / polar.v1 / shapes.v1 frame (root bar, centre disc, its shape):
    the same facts as a row."""

    key: int
    node: int
    kind: int
    size: int
    count: int = 0
    flags: int = 0
    tag: int = 0
    tone: int = 4
    branch: int = 0
    delta: int | None = Field(default=None, description="Compare views only")
    dstat: int | None = Field(default=None, description="Compare views only")


class FrameInfo(Payload):
    view_id: str
    seq: int
    width: int = Field(description="Canvas size in physical px")
    height: int
    dpr: float
    rows: int
    selected_row: int | None = None
    breadcrumb: list[Crumb]
    scanned: int = Field(description="Allocated bytes below the zoom root")
    filter: FilterInfo | None = None
    layout_ms: float
    reason: str = ""
    flash: list[FlashItem] = Field(
        default_factory=list, description="Rows changed by a file change since the last frame (UI-TM-43)"
    )
    visual: str = Field(default="treemap", description="Visualization that drew this frame")
    layout: str = Field(default=DRAWLIST_LAYOUT, description="Layout of the binary frame that follows")
    geometry: FrameGeometry | None = Field(default=None, description="None for drawlist.v1")
    root: FrameRoot | None = Field(default=None, description="The zoom root (flame.v1 / polar.v1)")
    palette: str | None = Field(
        default=None,
        description="colors.v1: a plugin colour mode drew this frame; its u1 'pal' column follows",
    )
    size_unit: SizeUnit = Field(default="bytes", description="What 'size' counts: bytes, or files / items")
    total: int = Field(
        default=0,
        description="The zoom root's own size in size_unit (what a row's share is of: 「占权重的 x%」)",
    )


class ViewState(Payload):
    view_id: str
    scan_id: str
    zoom_node: int
    zoom_key: int
    detail: int
    min_element: float
    sort_by_size: bool
    show_free: bool
    show_unknown: bool
    show_trash: bool = Field(
        default=True, description="The recycle bins are drawn (not taken out of the picture)"
    )
    has_trash: bool = Field(
        default=False, description="The scan holds a recycle bin / trash folder (the toggle is offered)"
    )
    visual: str = Field(default="treemap", description="A visual.list id")
    color_mode: ColorModeId = Field(description="Of the current visualization (each remembers its own)")
    colors: dict[str, ColorModeId] = Field(
        default_factory=dict, description="Colour mode each visualization remembers"
    )
    filter_mode: FilterModeName
    viewport: Viewport
    selected: int | None = None
    results_sort: ResultsSortName = "size"
    compare: bool = Field(default=False, description="A compare view: areas are changes (W6)")
    direction: CompareDirection = "all"
    can_back: bool
    can_forward: bool
    can_up: bool


class ViewUpdate(Payload):
    state: ViewState
    changed: bool = True
    frame: FrameInfo | None = Field(default=None, description="Present when a new frame was sent")
    filter: FilterInfo | None = Field(default=None, description="Current filter state (also without a frame)")


class ViewListResult(Payload):
    views: list[ViewState]


class ResultRow(Payload):
    """One line of the W3 result list: a drawn matching file, or the matches inside one block."""

    key: int = Field(description="Block to locate with view.reveal (the aggregate's key for 'group')")
    node: int = Field(description="The file or folder; the containing folder for 'group' and 'rest'")
    kind: ResultKind = Field(
        description="file: drawn on its own · folder: matches inside a folder block · "
        "group: matches inside a small-items / +N block · rest: matches too small to draw"
    )
    name: str = Field(description="'' for a group of several files and for 'rest'")
    count: int = Field(description="Matching files in this row")
    alloc: int
    size: int
    path: str = Field(description="Folder holding the block, relative to the scan root, platform separators")
    tag: TagColor | None = None
    tag_inherited: bool = Field(default=False, description="The tag comes from an enclosing folder")


class ViewResults(Payload):
    view_id: str
    seq: int = Field(description="Frame the rows were grouped by")
    active: bool = Field(description="A filter is in effect (otherwise there are no rows)")
    sort: ResultsSortName
    total: int = Field(description="Number of rows")
    files: int = Field(description="Matching files below the zoom root (sum of the rows' counts)")
    alloc: int
    size: int
    max_alloc: int = Field(description="Largest row, for the relative size bars")
    offset: int
    rows: list[ResultRow]


class VisualInfo(Payload):
    """One visualization this server offers (``visual.list``)."""

    id: str
    layout: str = Field(description="Layout of its frames")
    name: str = Field(description="Display name in the session's language")
    icon: str
    order: int
    compare: bool = Field(description="Can show compare views")
    free_space: bool = Field(description="Shows free / unknown space at a volume root")
    default_color: ColorModeId = Field(description="Colour mode a view starts with on this visualization")
    available: bool = Field(description="This session negotiated what it needs")
    reason: str | None = Field(default=None, description="Why it is not available (display text)")
    plugin: str | None = Field(default=None, description="The plugin that adds it (None: built in)")
    size_unit: SizeUnit = Field(default="bytes", description="What its areas count")
    when: list[str] = Field(
        default_factory=list,
        max_length=16,
        description="A plugin's conditions (docs/plugins.md 「when 条件」): where they fail it is shown "
        "disabled with the reason",
    )


class VisualListResult(Payload):
    visuals: list[VisualInfo]


class ViewFrameEvent(Payload):
    """``view.frame``: a frame pushed without a request (scan progress, time-based filters)."""

    frame: FrameInfo
