"""What plugin handlers return or receive besides reports: proposals, insights, listings, changes."""

from __future__ import annotations

import os
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Literal

import numpy as np
from pydantic import BaseModel, ConfigDict, Field

from spaceknife_protocol.plugins import NodeRef
from spaceknife_protocol.text import LocalStr

from .tree import TreeView


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


class ScanInfo(BaseModel):
    """Argument of the ``scan_done`` / ``scan_closed`` hooks."""

    model_config = ConfigDict(extra="ignore", frozen=True)

    scan_id: str
    roots: list[str] = Field(default_factory=list)
    items: int = 0
    source: str = "scan"


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


@dataclass(slots=True)
class VisualInput:
    """What a code visualization gets: the tree and its options (own settings by name)."""

    tree: TreeView
    options: dict[str, Any] = field(default_factory=dict)


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


__all__ = ["Change", "ChangeUnit", "Insight", "Listing", "Proposal", "ScanInfo", "VisualInput"]
