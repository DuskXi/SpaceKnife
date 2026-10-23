"""What plugin handlers return or receive besides reports: proposals, insights, listings, changes."""

from __future__ import annotations

import os
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from pydantic import BaseModel, ConfigDict, Field

from spaceknife_protocol.plugins import NodeRef
from spaceknife_protocol.text import LocalStr

from .tree import TreeView

if TYPE_CHECKING:
    from .context import Context


class Proposal(BaseModel):
    """A candidate of a cleanup: a node (id, reference or absolute path) and why."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    node: int | NodeRef | str
    reason: LocalStr = Field(default="", max_length=120)
    checked: bool = True


class Insight(BaseModel):
    """What a code insight says about a node (the insight card: what it is, can it go, how)."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    title: LocalStr
    reason: LocalStr
    kind: str = "other"
    cleanup: Literal["safe", "tool", "review", "keep"] = "review"
    app: LocalStr = ""
    note: LocalStr = ""
    command: str = Field(default="", description="Shown, never run")


class _Record(BaseModel):
    """A model that also reads like the dict it used to be (``info["scan_id"]``, ``info.get(…)``)."""

    model_config = ConfigDict(extra="ignore", frozen=True)

    def __getitem__(self, key: str) -> Any:
        if key not in type(self).model_fields:
            raise KeyError(key)
        return getattr(self, key)

    def get(self, key: str, default: Any = None) -> Any:
        return getattr(self, key) if key in type(self).model_fields else default


class ScanInfo(_Record):
    """An open scan: what ``ctx.scans()`` lists (newest first) and the ``scan_done`` /
    ``scan_closed`` hooks get. Times are unix seconds."""

    scan_id: str
    roots: list[str] = Field(default_factory=list, description="The roots' paths (a source: its name)")
    items: int = Field(default=0, description="Folders and files, the roots not counted")
    source: str = Field(default="scan", description='"scan", "snapshot" or a scan source\'s full id')
    state: Literal["scanning", "done", "cancelled", "failed"] | str = "done"
    started_at: float | None = None
    finished_at: float | None = None
    snapshot: str | None = Field(default=None, description="The snapshot file it was opened from")
    front: bool = Field(default=False, description="The scan in front (the one ctx.tree reads by default)")
    on_disk: bool = Field(default=True, description="Its nodes are files here (False: a scan source's)")


class SnapshotInfo(_Record):
    """A saved snapshot: what ``ctx.snapshots.list()`` gives (newest first); ``path`` opens it."""

    path: str
    created_at: datetime
    roots: list[str] = Field(default_factory=list)
    node_count: int = 0
    bytes_alloc: int = 0
    files: int = 0
    file_bytes: int = Field(default=0, description="The size of the snapshot file itself")
    method: str | None = Field(default=None, description='How it was read: "hard", "mft" or None')


class Change(BaseModel):
    """One file change a watcher reports (paths relative to the source's root, '/'-separated)."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    path: str
    kind: Literal["created", "modified", "deleted", "overflow"] = "modified"


class ChangeUnit(BaseModel):
    """One changed item of a comparison, for a change annotator."""

    model_config = ConfigDict(extra="ignore", frozen=True)

    path: str
    kind: str = "dir"
    status: str = "changed"
    delta: int = 0
    base: int = 0
    target: int = 0


@dataclass(frozen=True, slots=True)
class ExportInfo:
    """The optional fourth argument of an export format: ``(tree, root, match, info)`` (the older
    form; :class:`ExportInput` has everything). ``filter`` is the filter expression the export is
    limited to ("" when it is not filtered; ``match`` then is None)."""

    filter: str = ""
    options: dict[str, Any] = field(default_factory=dict)


class _Inputs:
    """What the inputs of code visualizations, colour modes and export formats share."""

    __slots__ = ()
    ctx: Context | None

    @property
    def settings(self) -> Mapping[str, Any]:
        """The plugin's own settings by local name (as ``ctx.settings``)."""
        if self.ctx is None:
            return {}
        return dict(self.ctx.settings.items())

    @property
    def cancelled(self) -> bool:
        """The host gave up on this call (a newer frame, the user went on): stop and return anything."""
        return self.ctx is not None and self.ctx.cancelled

    def check(self) -> None:
        """Raise :class:`~spaceknife_sdk.Cancelled` when :attr:`cancelled`."""
        if self.ctx is not None:
            self.ctx.check()


@dataclass(slots=True)
class VisualInput(_Inputs):
    """What a code visualization gets: ``(v: VisualInput) -> np.ndarray`` (a weight per node).

    ``options``: its declared options (own settings by name, as the view has them); ``settings``:
    all of the plugin's settings; ``ctx``: the call's context (``ctx.check()`` for cancellation)."""

    tree: TreeView
    options: dict[str, Any] = field(default_factory=dict)
    ctx: Context | None = None


@dataclass(slots=True)
class ColorInput(_Inputs):
    """What a code colour mode gets: ``(c: ColorInput) -> np.ndarray[uint8]``, a palette index for
    each of ``ids`` (the drawn rows' nodes; 0 is the neutral colour). Runs within 20 ms per frame."""

    tree: TreeView
    ids: np.ndarray
    palette: int = 1
    ctx: Context | None = None


@dataclass(slots=True)
class ExportInput(_Inputs):
    """What an export format gets: ``(x: ExportInput) -> Iterator[str]`` (``bytes`` with
    ``binary = true``). ``root``: the node whose contents are exported (the view's place, the
    selection's node, or a root); ``match``: the filter's matches (None: not filtered);
    ``options``: the values of the format's ``options`` the user picked in the export dialog."""

    tree: TreeView
    root: int
    match: np.ndarray | None = None
    filter: str = ""
    options: dict[str, Any] = field(default_factory=dict)
    selection: list[int] = field(default_factory=list)
    ctx: Context | None = None


BATCH = 50_000


class Listing:
    """What a scan source fills: folders and files by path relative to the source's root ('/').

    Parent folders are added automatically. The SDK sends the entries to the host in batches.
    """

    def __init__(self, flush: Callable[[dict[str, Any]], None] | None = None) -> None:
        self._flush = flush
        self._clear()
        self.root = ""
        self.sent = 0

    def _clear(self) -> None:
        self.paths: list[str] = []
        self.kinds: list[int] = []
        self.sizes: list[int] = []
        self.allocs: list[int] = []
        self.mtimes: list[int] = []
        self.ctimes: list[int] = []
        self.atimes: list[int] = []

    def root_name(self, text: str) -> None:
        """What the source's root is called in the interface (default: the URI)."""
        self.root = text

    @staticmethod
    def _norm(path: str) -> str:
        p = path.replace("\\", "/").strip("/")
        parts = [x for x in p.split("/") if x not in ("", ".")]
        if any(x == ".." for x in parts):
            raise ValueError(f"{path!r}: '..' is not allowed")
        return "/".join(parts)

    def _add(self, path: str, kind: int, size: int, alloc: int, mtime: int, ctime: int, atime: int) -> None:
        p = self._norm(path)
        if not p:
            return
        self.paths.append(p)
        self.kinds.append(kind)
        self.sizes.append(max(0, int(size)))
        self.allocs.append(max(0, int(alloc)))
        self.mtimes.append(max(0, min(int(mtime), 2**32 - 1)))
        self.ctimes.append(max(0, min(int(ctime), 2**32 - 1)))
        self.atimes.append(max(0, min(int(atime), 2**32 - 1)))
        if len(self.paths) >= BATCH:
            self.flush()

    def dir(self, path: str, mtime: int = 0) -> None:
        self._add(path, 1, 0, 0, mtime, 0, 0)

    def file(
        self, path: str, size: int, alloc: int | None = None, mtime: int = 0, ctime: int = 0, atime: int = 0
    ) -> None:
        self._add(path, 0, size, size if alloc is None else alloc, mtime, ctime, atime)

    def batch(self) -> dict[str, Any]:
        """The pending entries as columns plus the UTF-8 paths blob; clears them."""
        enc = [
            p.encode("utf-8", "surrogatepass" if os.name == "nt" else "surrogateescape") for p in self.paths
        ]
        out = {
            "kind": np.asarray(self.kinds, dtype=np.uint8),
            "size": np.asarray(self.sizes, dtype=np.int64),
            "alloc": np.asarray(self.allocs, dtype=np.int64),
            "mtime": np.asarray(self.mtimes, dtype=np.uint32),
            "ctime": np.asarray(self.ctimes, dtype=np.uint32),
            "atime": np.asarray(self.atimes, dtype=np.uint32),
            "path_len": np.asarray([len(e) for e in enc], dtype=np.uint32),
            "_paths": b"".join(enc),
        }
        self.sent += len(self.paths)
        self._clear()
        return out

    def flush(self) -> None:
        if self._flush is not None and self.paths:
            self._flush(self.batch())


__all__ = [
    "Change",
    "ChangeUnit",
    "ColorInput",
    "ExportInfo",
    "ExportInput",
    "Insight",
    "Listing",
    "Proposal",
    "ScanInfo",
    "SnapshotInfo",
    "VisualInput",
]
