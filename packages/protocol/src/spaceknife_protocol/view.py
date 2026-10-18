"""Views (one per tab / split pane) and their draw-list frames.

Every method that changes a view answers with a :class:`ViewUpdate` (JSON) and, when the picture
changed, sends one binary frame (stream ``"view:<view_id>"``, layout ``drawlist.v1``) whose ``seq``
equals ``ViewUpdate.frame.seq`` and whose ``request_id`` is the request's id. Frames pushed while a scan
is running carry no ``request_id`` and arrive with a ``view.frame`` event.

``drawlist.v1`` columns (one row per rectangle; parents come before their children; coordinates
are whole physical pixels in a ``width`` × ``height`` canvas):

=========  =====  =====================================================================
column     dtype  meaning
=========  =====  =====================================================================
key        <u4    stable id: node id, or SMALL/MORE/FREE/UNKNOWN tagged id (top 2 bits)
node       <i4    node id (container for SMALL/MORE, -1 for FREE/UNKNOWN)
parent     <i4    row of the enclosing expanded folder, -1 at the top
kind       <u1    0 file · 1 folder (frame + header) · 2 collapsed folder · 3 pending folder
                  · 4 "N small items" · 5 "+N items" · 6 free space · 7 unknown space
depth      <u1    nesting level, 1 = child of the zoom root (level colours: min(depth, 4))
x y w h    <i4    rectangle
cls        <u1    file class index (``filter.vocab`` order) for files and small-item blocks
label      <u1    bits: 1 name · 2 size line · 4 large type · 8 short (name only, centred)
size       <i8    bytes shown (area basis; matched bytes in hide mode)
count      <u4    files in "N small items", entries in "+N items"
flags      <u2    1 match · 2 dimmed · 4 folder-mask hit (bold) · 8 own tag · 16 selected
                  · 32 ancestor of the selection (halo header)
tag        <u1    own tag: 0 none, 1 red, 2 yellow, 3 green, 4 blue
tone       <u1    modification-age band for time colouring: 0 < 1 d … 4 ≥ 1 y / unknown
name_len   <u4    byte length of this row's name in the ``names`` blob (0: no label)
=========  =====  =====================================================================

Blob ``names``: the rows' UTF-8 names, concatenated in row order.
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload
from .filter import FilterDiagnostic, TagColor

DRAWLIST_LAYOUT = "drawlist.v1"
VIEW_STREAM_PREFIX = "view:"


def view_stream(view_id: str) -> str:
    return VIEW_STREAM_PREFIX + view_id


ColorModeName = Literal["class", "flat", "time"]
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
    color_mode: ColorModeName | None = None
    filter_mode: FilterModeName | None = None
    viewport: Viewport | None = None


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
    color_mode: ColorModeName
    filter_mode: FilterModeName
    viewport: Viewport
    selected: int | None = None
    results_sort: ResultsSortName = "size"
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


class ViewFrameEvent(Payload):
    """``view.frame``: a frame pushed without a request (scan progress, time-based filters)."""

    frame: FrameInfo
