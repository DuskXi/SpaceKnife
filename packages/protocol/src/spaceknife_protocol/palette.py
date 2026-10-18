"""Command palette (W4): commands, recents, direct items and path search, all ranked by the backend.

Frontends only render what comes back and run the chosen command by its ``id``; scoring, hit ranges,
availability and status texts are computed here so that every frontend shows the same list
(UI-W4-49). Hit ranges are UTF-16 ``[start, end)`` offsets into the text they belong to.
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload
from .filter import FilterDiagnostic, FilterToken

PaletteTab = Literal["scan", "start", "settings"]
RecentKind = Literal["jump", "filter"]
PathState = Literal["in_scan", "outside", "missing"]

_SCAN_ID = Field(default=None, max_length=64)


class Span(Payload):
    start: int
    end: int


class PaletteContext(Params):
    """Where the palette was opened: the active tab (or the focused pane of a split)."""

    tab: PaletteTab
    scan_id: str | None = _SCAN_ID
    view_id: str | None = Field(default=None, max_length=64)
    basket: int = Field(
        default=0, ge=0, description="Items in the delete basket (held by the frontend until M6)"
    )


class PaletteQueryParams(Params):
    text: str = Field(default="", max_length=4096)
    context: PaletteContext


class PaletteCommand(Payload):
    id: str = Field(description="Run by the frontend; stable across versions")
    name: str
    hits: list[Span] = Field(default_factory=list, description="Matched characters of the name")
    status: str = Field(default="", description="State info, or the reason when disabled")
    enabled: bool = True
    keys: list[str] = Field(
        default_factory=list,
        description="Bindings in neutral notation ('Ctrl+Shift+F5'); the first is shown, Ctrl = ⌘ on macOS",
    )
    icon: str = Field(description="Icon name from the design icon set")
    more: bool = Field(default=False, description="Ends with '…': running it asks for one more choice")


class PaletteRecent(Payload):
    kind: RecentKind
    text: str = Field(description="Absolute path (jump) or filter expression (filter)")
    hits: list[Span] = Field(default_factory=list)
    tokens: list[FilterToken] = Field(default_factory=list, description="Filter colouring (W3 rules)")
    exists: bool = Field(default=True, description="jump: the path is still in this scan")
    node: int | None = None
    is_dir: bool = True
    alloc: int | None = Field(default=None, description="jump: current size; filter: see palette.preview")


class PaletteDirect(Payload):
    """The input itself as a filter expression or a path (UI-W4-31…33)."""

    kind: Literal["filter", "path"]
    text: str
    # filter
    tokens: list[FilterToken] = Field(default_factory=list)
    diagnostics: list[FilterDiagnostic] = Field(default_factory=list)
    valid: bool = True
    # path
    state: PathState | None = None
    node: int | None = None
    is_dir: bool = True
    alloc: int | None = None
    complete: str | None = Field(default=None, description="Tab: the path completed by one segment")


class PaletteResult(Payload):
    text: str
    direct: list[PaletteDirect]
    recent: list[PaletteRecent]
    commands: list[PaletteCommand]
    paths: bool = Field(description="Ask palette.paths for the path group (scan tab only)")
    seed: str | None = Field(default=None, description="Empty input: the selection's name searched for")


class PalettePathsParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    text: str = Field(default="", max_length=4096)
    view_id: str | None = Field(
        default=None, max_length=64, description="Empty text: seed with its selection"
    )
    limit: int = Field(default=50, ge=1, le=500)


class PalettePath(Payload):
    node: int
    name: str
    parent: str = Field(description="Absolute path of the containing folder")
    is_dir: bool
    alloc: int
    hits: list[Span]
    tier: int = Field(
        description="0 exact name · 1 prefix · 2 inside the name · 3 scattered letters · 4 child listing"
    )
    complete: str = Field(description="Tab: absolute path, folders with a trailing separator")


class PalettePaths(Payload):
    text: str
    items: list[PalettePath]
    total: int = Field(description="All hits, of which the best `limit` are listed")
    partial: bool = Field(description="The scan is still running: results may be incomplete")
    seed: str | None = None
    folder: str | None = Field(default=None, description="Listing the children of this folder")


class PalettePreviewParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    text: str = Field(max_length=8192)


class PalettePreview(Payload):
    text: str
    valid: bool
    files: int = 0
    alloc: int = 0
    size: int = 0


class PaletteRememberParams(Params):
    kind: RecentKind
    text: str = Field(min_length=1, max_length=8192)
    scan_id: str | None = Field(default=None, max_length=64, description="jump: the scan the path belongs to")


class PaletteClearParams(Params):
    scan_id: str | None = Field(default=None, max_length=64, description="Clear this scan target's recents")


class PaletteCommandList(Payload):
    """Every command with its default keys (W7 快捷键 page; the keymap becomes editable in M6)."""

    commands: list[PaletteCommand]
