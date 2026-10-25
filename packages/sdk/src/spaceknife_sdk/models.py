"""What plugin handlers return or receive besides reports: proposals, insights, listings, changes."""

from __future__ import annotations

import os
from collections.abc import Callable, Iterable, Mapping
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
    priority: int = Field(default=0, ge=-1000, le=1000, description="Cards are shown highest first")
    replaces: list[str] = Field(
        default_factory=list,
        max_length=32,
        description="Ids of insight rules whose card on this node it replaces ('*': any rule's)",
    )
    regenerate: LocalStr = Field(
        default="", max_length=300, description="How it comes back once deleted: 「npm install 会装回来」"
    )
    node: int | NodeRef | None = Field(
        default=None,
        description="The folder the card is about when it is not the node asked (a file inside a "
        "project: the project): the card names it and 「加入删除篮」 puts it in; it must be the node "
        "or one of its folders",
    )


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
    source: str = Field(
        default="scan",
        description='"scan" (a disk scan), "snapshot" (opened from a snapshot file) or a scan '
        "source's full id",
    )
    uri: str = Field(default="", description="What the scan source listed (its URI);  for the others")
    state: Literal["scanning", "done", "cancelled", "failed"] | str = "done"
    started_at: float | None = None
    finished_at: float | None = None
    snapshot: str | None = Field(default=None, description="The snapshot file it was opened from")
    front: bool = Field(default=False, description="The scan in front (the one ctx.tree reads by default)")
    on_disk: bool = Field(default=True, description="Its nodes are files here (False: a scan source's)")
    monitoring: Literal["off", "starting", "running", "degraded", "failed"] | str = Field(
        default="off",
        description="File changes followed live (the tree_changed hook): off (live monitoring is "
        "off, a snapshot, a source without a watcher), starting, running, degraded (some changes "
        "found by polling or rescans), failed",
    )


class TreeChange(_Record):
    """What the ``tree_changed`` hook gets: the tree of an open scan changed (files changed on disk
    while it is monitored, 「重新扫描」, a scan source listed again). At most one a second per scan;
    the changes of that second come together. ``folders``, ``paths``, ``before`` and ``after`` go
    together (``zip`` them): the paths and sizes need no read of the tree."""

    scan_id: str
    epoch: int = Field(
        default=0, description="The node ids below are of this epoch (the hook's ctx.tree is this tree)"
    )
    version: int = Field(default=0, description="The tree's version after the changes (ctx.tree.version)")
    folders: list[int] = Field(
        default_factory=list,
        description="Folders whose contents changed (their sizes changed with them, up to the root), "
        "at most 256; a scan source listed again: its root",
    )
    paths: list[str] = Field(default_factory=list, description="The path of each folder in folders")
    before: list[int | None] = Field(
        default_factory=list,
        description="Each folder's size on disk (bytes, with what is inside it: Node.alloc) before "
        "these changes; None where it is not known (the tree was replaced: a scan source listed again)",
    )
    after: list[int] = Field(
        default_factory=list, description="Each folder's size on disk after them (as ctx.tree has it)"
    )
    more: int = Field(default=0, description="About how many more folders changed than are listed")
    changes: int = Field(default=0, description="Files and folders that changed, added or went")


class SnapshotsChange(_Record):
    """What the ``snapshots`` hook gets (needs ``snapshots.read``): snapshots were saved (a finished
    scan saves itself, 「保存快照」) or the oldest automatic ones were pruned. ``ctx.snapshots.list()``
    has the details."""

    saved: list[str] = Field(default_factory=list, description="Snapshot files written")
    removed: list[str] = Field(default_factory=list, description="Snapshot files deleted")


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
    """What a code visualization gets: ``(v: VisualInput) -> np.ndarray`` (a weight per node,
    ``len(v.tree)`` of them).

    ``options``: the current values of the plugin's settings its ``options`` names (by name);
    ``settings``: all of the plugin's settings; ``ctx``: the call's context (``ctx.check()`` for
    cancellation)."""

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


# The attributes an entry of a scan source may carry (``attrs=``), as the filter's ``:attr:`` names
# them, plus cloud (a placeholder not stored here), denied (a folder that could not be read) and
# error (an entry that could not be read). The bits are the store's (tree.Flag).
LISTING_ATTRS: dict[str, int] = {
    "denied": 1 << 0,
    "compressed": 1 << 3,
    "sparse": 1 << 4,
    "hidden": 1 << 5,
    "system": 1 << 6,
    "readonly": 1 << 7,
    "cloud": 1 << 9,
    "error": 1 << 10,
    "archive": 1 << 12,
    "encrypted": 1 << 13,
    "temporary": 1 << 14,
    "notindexed": 1 << 15,
    "offline": 1 << 16,
}
_LISTING_KINDS = {"file": 0, "link": 2, "other": 3}


class Listing:
    """What a scan source fills: folders and files by path relative to the source's root ('/').

    Parent folders are added automatically. The SDK sends the entries to the host in batches; the
    host shows what came so far while the source is still listing (a long listing grows on screen).

    Besides path and sizes an entry can carry what a disk scan records: ``kind`` (``"link"``: a
    symbolic link, shown but never entered; ``"other"``: a device, pipe or socket), ``attrs``
    (:data:`LISTING_ATTRS`: ``hidden``, ``readonly`` … as the ``:attr:`` filter knows them),
    ``ino`` / ``nlink`` / ``dev`` (a file id with its link count: the host counts a hard-linked file
    once, the later names get ``Flag.HARDLINK_DUP`` as in a disk scan) and, for a folder, its own
    ``size`` / ``alloc`` (what the folder entry itself takes, counted toward its parent like a disk
    scan's directory blocks). ``size`` and ``alloc`` are kept as listed; the source's ``size = "size"``
    makes the views measure by ``size`` instead of ``alloc``.
    """

    def __init__(self, flush: Callable[[dict[str, Any]], None] | None = None) -> None:
        self._flush = flush
        self._clear()
        self.root = ""
        self.sent = 0
        self.duplicates = 0  # entries whose path was listed before (the last one stays)
        self._seen: set[int] = set()

    def _clear(self) -> None:
        self.paths: list[str] = []
        self.kinds: list[int] = []
        self.sizes: list[int] = []
        self.allocs: list[int] = []
        self.mtimes: list[int] = []
        self.ctimes: list[int] = []
        self.atimes: list[int] = []
        self.flags: list[int] = []
        self.inos: list[int] = []
        self.nlinks: list[int] = []
        self.devs: list[int] = []

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

    @staticmethod
    def _attrs(attrs: Iterable[str]) -> int:
        bits = 0
        for a in attrs:
            bit = LISTING_ATTRS.get(str(a).lower())
            if bit is None:
                raise ValueError(f"unknown attribute {a!r}; one of {', '.join(LISTING_ATTRS)}")
            bits |= bit
        return bits

    def _add(
        self,
        path: str,
        kind: int,
        size: int,
        alloc: int,
        mtime: int,
        ctime: int,
        atime: int,
        flags: int = 0,
        ino: int = 0,
        nlink: int = 1,
        dev: int = 0,
    ) -> None:
        p = self._norm(path)
        if not p:
            return
        key = hash(p)
        if key in self._seen:
            self.duplicates += 1  # the host warns: the earlier one's bytes would be gone silently
        else:
            self._seen.add(key)
        self.paths.append(p)
        self.kinds.append(kind)
        self.sizes.append(max(0, int(size)))
        self.allocs.append(max(0, int(alloc)))
        self.mtimes.append(max(0, min(int(mtime), 2**32 - 1)))
        self.ctimes.append(max(0, min(int(ctime), 2**32 - 1)))
        self.atimes.append(max(0, min(int(atime), 2**32 - 1)))
        self.flags.append(flags)
        self.inos.append(max(0, min(int(ino), 2**64 - 1)))
        self.nlinks.append(max(0, min(int(nlink), 65535)))
        self.devs.append(max(0, min(int(dev), 2**64 - 1)))
        if len(self.paths) >= BATCH:
            self.flush()

    def dir(
        self, path: str, mtime: int = 0, *, size: int = 0, alloc: int = 0, attrs: Iterable[str] = ()
    ) -> None:
        """A folder; ``size`` / ``alloc``: what the folder entry itself takes (not its contents)."""
        self._add(path, 1, size, alloc, mtime, 0, 0, self._attrs(attrs))

    def file(
        self,
        path: str,
        size: int,
        alloc: int | None = None,
        mtime: int = 0,
        ctime: int = 0,
        atime: int = 0,
        *,
        kind: Literal["file", "link", "other"] = "file",
        attrs: Iterable[str] = (),
        ino: int = 0,
        nlink: int = 1,
        dev: int = 0,
    ) -> None:
        """A file (``kind``: ``"link"`` a symbolic link, ``"other"`` a device, pipe or socket).
        ``ino`` with ``nlink`` > 1: a hard-linked file, counted once per (``dev``, ``ino``)."""
        k = _LISTING_KINDS.get(kind)
        if k is None:
            raise ValueError(f"kind {kind!r}: one of file, link, other")
        alloc_ = size if alloc is None else alloc
        self._add(path, k, size, alloc_, mtime, ctime, atime, self._attrs(attrs), ino, nlink, dev)

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
            "flags": np.asarray(self.flags, dtype=np.uint32),
            "ino": np.asarray(self.inos, dtype=np.uint64),
            "nlink": np.asarray(self.nlinks, dtype=np.uint16),
            "dev": np.asarray(self.devs, dtype=np.uint64),
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
    "LISTING_ATTRS",
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
    "SnapshotsChange",
    "TreeChange",
    "VisualInput",
]
