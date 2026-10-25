"""Read-only access to a scanned tree (``ctx.tree``).

Two ways to read:

* :meth:`TreeView.query` (preferred): the host filters the tree with a filter expression, a subtree,
  a kind and a minimum size, and sends back only the matching rows and the columns asked for.
* :meth:`TreeView.col`, :meth:`TreeView.children`, :meth:`TreeView.walk`: whole columns. The first use
  makes the host export them as a read-only image the plugin maps (no per-node remote calls).

Node ids are only valid for the tree they came from: ``epoch`` changes when the host reorganises the
tree (a compaction, a monitor relisting moved rows); results that carry ids carry the epoch too.
"""

from __future__ import annotations

import os
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass
from enum import IntEnum, IntFlag
from typing import TYPE_CHECKING, Any

import numpy as np

from spaceknife_protocol.binary import decode_frame
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.plugins import NodeRef

from .errors import HostError

if TYPE_CHECKING:
    from ._link import Image, Link


class Kind(IntEnum):
    """Node kinds (the same values as the host's store)."""

    FILE = 0
    DIR = 1
    LINK = 2
    OTHER = 3


class Flag(IntFlag):
    """Node flags (the same bits as the host's store)."""

    NONE = 0
    ACCESS_DENIED = 1 << 0
    MOUNT = 1 << 1
    HARDLINK_DUP = 1 << 2
    COMPRESSED = 1 << 3
    SPARSE = 1 << 4
    HIDDEN = 1 << 5
    SYSTEM = 1 << 6
    READONLY = 1 << 7
    REPARSE = 1 << 8
    CLOUD = 1 << 9
    ERROR = 1 << 10
    PENDING = 1 << 11
    ARCHIVE = 1 << 12
    ENCRYPTED = 1 << 13
    TEMPORARY = 1 << 14
    NOT_INDEXED = 1 << 15
    OFFLINE = 1 << 16
    VIRTUAL = 1 << 17
    ADS = 1 << 18
    GONE = 1 << 19


KIND_NAMES = {"file": Kind.FILE, "dir": Kind.DIR, "link": Kind.LINK, "other": Kind.OTHER}
# Store columns a plugin may read whole (``col``) and ask for in queries; plus "cls" (class index).
COLUMNS = (
    "parent",
    "depth",
    "kind",
    "flags",
    "ext",
    "size",
    "alloc",
    "mtime",
    "ctime",
    "atime",
    "ino",
    "nlink",
    "child_start",
    "child_count",
    "tot_size",
    "tot_alloc",
    "tot_files",
    "tot_dirs",
    "cls",
)
QUERY_LIMIT = 100_000  # rows a query gives when it does not say (Rows.truncated tells)
QUERY_MAX = 1_000_000  # the most a query may ask for
# query columns: the store's, plus the effective sizes ("bytes": allocated, subtree totals for
# folders; "logical": the same for logical sizes), "name" and "path"
QUERY_COLUMNS = (*COLUMNS, "bytes", "logical", "name", "path")


@dataclass(frozen=True, slots=True)
class ClassInfo:
    index: int
    id: str
    name: str


@dataclass(frozen=True, slots=True)
class Node:
    """One node, materialised. ``size`` / ``alloc`` are subtree totals for folders."""

    id: int
    name: str
    path: str
    kind: Kind
    size: int
    alloc: int
    files: int
    dirs: int
    mtime: int
    ctime: int
    atime: int
    ext: str
    parent: int
    depth: int
    child_count: int
    flags: Flag
    cls: str

    @property
    def is_dir(self) -> bool:
        return self.kind == Kind.DIR

    @property
    def is_file(self) -> bool:
        return self.kind == Kind.FILE

    @staticmethod
    def from_dict(d: Mapping[str, Any]) -> Node:
        return Node(
            id=int(d["id"]),
            name=str(d["name"]),
            path=str(d["path"]),
            kind=Kind(int(d["kind"])),
            size=int(d["size"]),
            alloc=int(d["alloc"]),
            files=int(d.get("files", 0)),
            dirs=int(d.get("dirs", 0)),
            mtime=int(d.get("mtime", 0)),
            ctime=int(d.get("ctime", 0)),
            atime=int(d.get("atime", 0)),
            ext=str(d.get("ext", "")),
            parent=int(d.get("parent", 0)),
            depth=int(d.get("depth", 0)),
            child_count=int(d.get("child_count", 0)),
            flags=Flag(int(d.get("flags", 0))),
            cls=str(d.get("cls", "")),
        )


class Columns(dict[str, np.ndarray]):
    """A table of equally long numeric columns: returned from a method or a stream, it travels as a
    binary frame instead of JSON. ``layout`` names the column set for the receiver."""

    def __init__(self, layout: str = "columns.v1", /, **columns: Any) -> None:
        super().__init__({k: np.asarray(v) for k, v in columns.items()})
        self.layout = layout

    def __len__(self) -> int:
        return len(next(iter(self.values()))) if dict.__len__(self) else 0


def _stale(epoch: int) -> HostError:
    return HostError(ErrorCode.STALE_TREE, "the tree was reorganised: node ids changed", {"epoch": epoch})


def _split_blob(blob: np.ndarray, lengths: np.ndarray) -> list[str]:
    raw = bytes(blob)
    out: list[str] = []
    pos = 0
    errors = "surrogatepass" if os.name == "nt" else "surrogateescape"
    for ln in lengths.tolist():
        out.append(raw[pos : pos + ln].decode("utf-8", errors))
        pos += ln
    return out


class Row:
    """One row of :class:`Rows`: ``row.id``, ``row.size`` …"""

    __slots__ = ("_i", "_rows")

    def __init__(self, rows: Rows, i: int) -> None:
        self._rows = rows
        self._i = i

    def __getattr__(self, name: str) -> Any:
        if name == "id":
            return int(self._rows.ids[self._i])
        try:
            return self._rows.value(name, self._i)
        except KeyError:
            raise AttributeError(name) from None

    def __getitem__(self, name: str) -> Any:
        return getattr(self, name)

    def __repr__(self) -> str:
        return f"Row(id={self.id})"


class Rows:
    """The result of :meth:`TreeView.query`: ``rows.ids``, ``rows["size"]``, ``rows.paths``, rows.

    ``total`` is how many matched before the limit; ``truncated`` says the rows stop short of it."""

    def __init__(
        self,
        tree: TreeView,
        ids: np.ndarray,
        columns: dict[str, np.ndarray],
        texts: dict[str, list[str]],
        total: int,
    ) -> None:
        self.tree = tree
        self.ids = ids
        self.columns = columns
        self.texts = texts
        self.total = total  # matches before the limit

    def __len__(self) -> int:
        return len(self.ids)

    @property
    def truncated(self) -> bool:
        """More rows matched than came back (the query's ``limit``)."""
        return self.total > len(self.ids)

    def __getitem__(self, name: str) -> np.ndarray:
        if name in ("name", "path"):
            return np.asarray(self.text(name), dtype=object)
        if name == "id":
            return self.ids
        return self.columns[name]

    def text(self, name: str) -> list[str]:
        if name not in self.texts:
            raise KeyError(f"query(columns=…) did not ask for {name!r}")
        return self.texts[name]

    @property
    def names(self) -> list[str]:
        return self.text("name")

    @property
    def paths(self) -> list[str]:
        return self.text("path")

    def value(self, name: str, i: int) -> Any:
        if name in self.texts:
            return self.texts[name][i]
        col = self.columns[name]
        return col[i].item()

    def __iter__(self) -> Iterator[Row]:
        for i in range(len(self.ids)):
            yield Row(self, i)

    def refs(self) -> list[NodeRef]:
        return [self.tree.ref(int(i)) for i in self.ids]

    def records(self) -> list[dict[str, Any]]:
        """Each row as a dict (table rows of a report: ``r.table(cols, rows=rows)`` does this)."""
        keys = [*self.columns, *self.texts]
        return [{"node": int(self.ids[i]), **{k: self.value(k, i) for k in keys}} for i in range(len(self))]


class TreeView:
    """A read-only view of one scan, valid for one call (the host may change the tree between calls).

    Every read is of the tree's ``epoch``: once the host reorganised the tree (moved rows: node ids
    changed), a read raises ``HostError`` with code ``STALE_TREE`` instead of answering with other
    nodes. ``pinned`` (``ctx.tree`` of a call that came with node ids): whole columns too."""

    def __init__(self, link: Link, info: Mapping[str, Any], *, pinned: bool = False) -> None:
        self._link = link
        self._pinned = pinned
        self.scan_id: str = str(info["scan_id"])
        self.epoch: int = int(info.get("epoch", 0))
        self.version: int = int(info.get("version", 0))
        self.n: int = int(info["n"])
        self.roots: list[int] = [int(r) for r in info.get("roots", ())]
        self.root_paths: list[str] = [str(p) for p in info.get("root_paths", ())]
        self.virtual: bool = bool(info.get("virtual", False))
        self.classes: dict[int, ClassInfo] = {
            int(c["index"]): ClassInfo(int(c["index"]), str(c["id"]), str(c["name"]))
            for c in info.get("classes", ())
        }
        self._image: Image | None = None
        self._image_cols: set[str] = set()
        self._nodes: dict[int, Node] = {}

    def __repr__(self) -> str:
        return f"TreeView({self.scan_id}, n={self.n}, epoch={self.epoch})"

    def __len__(self) -> int:
        """The number of nodes: ids are 0 … len(tree) - 1 (node 0 is the virtual top)."""
        return self.n

    def close(self) -> None:
        """Let go of a snapshot opened with ``ctx.snapshots.open`` before the call ends (at most two
        may be open at once). Nothing happens for a scan's own tree."""
        if not self.scan_id.startswith("snap-"):
            return
        self._image = None
        self._image_cols = set()
        forget = getattr(self._link, "forget_image", None)
        if forget is not None:
            forget(self.scan_id)
        self._link.request("host.snapshot_close", {"scan_id": self.scan_id})

    def __enter__(self) -> TreeView:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    # ------------------------------------------------------------- nodes

    def _params(self, **kw: Any) -> dict[str, Any]:
        return {"scan_id": self.scan_id, "epoch": self.epoch, **kw}

    def nodes(self, ids: Sequence[int] | np.ndarray) -> list[Node]:
        """Several nodes at once (one request)."""
        want = [int(i) for i in ids]
        missing = [i for i in dict.fromkeys(want) if i not in self._nodes]
        if missing:
            value, _ = self._link.request("host.node", self._params(nodes=missing))
            for d in value["nodes"]:
                node = Node.from_dict(d)
                self._nodes[node.id] = node
        return [self._nodes[i] for i in want]

    def node(self, id: int) -> Node:
        return self.nodes([id])[0]

    def parent(self, id: int) -> int:
        if "parent" in self._image_cols and self._image is not None:
            return int(self._image.columns["parent"][id])
        return self.node(id).parent

    def name(self, id: int) -> str:
        return self.names([id])[0]

    def names(self, ids: Sequence[int] | np.ndarray) -> list[str]:
        value, _ = self._link.request("host.paths", self._params(nodes=[int(i) for i in ids], what="name"))
        return [str(x) for x in value["texts"]]

    def path(self, id: int) -> str:
        return self.paths([id])[0]

    def paths(self, ids: Sequence[int] | np.ndarray) -> list[str]:
        value, _ = self._link.request("host.paths", self._params(nodes=[int(i) for i in ids], what="path"))
        return [str(x) for x in value["texts"]]

    def rel_path(self, id: int, under: int | None = None) -> str:
        """The path of ``id`` relative to ``under`` (default: its root), always with ``/`` — the
        same on every system (``path`` is the system's own: ``\\`` on Windows). "" for ``under``
        itself."""
        return self.rel_paths([id], under)[0]

    def rel_paths(self, ids: Sequence[int] | np.ndarray, under: int | None = None) -> list[str]:
        """:meth:`rel_path` of several nodes (one request)."""
        want = [int(i) for i in ids]
        base: list[str]
        if under is not None:
            top = self.path(int(under)).replace("\\", "/").rstrip("/")
            base = [top] * len(want)
        else:
            roots = {
                r: p.replace("\\", "/").rstrip("/") for r, p in zip(self.roots, self.root_paths, strict=False)
            }
            base = [roots.get(self.root_of(i), "") for i in want]
        out = []
        for full, top in zip((p.replace("\\", "/") for p in self.paths(want)), base, strict=True):
            if top and (full == top or full.startswith(top + "/")):
                out.append(full[len(top) :].lstrip("/"))
            else:
                out.append(full.lstrip("/"))
        return out

    def root_of(self, id: int) -> int:
        """The root (a scanned folder) that ``id`` is under (0 for the top node)."""
        node = int(id)
        roots = set(self.roots)
        guard = 0
        while node > 0 and node not in roots and guard < 4096:
            node = self.parent(node)
            guard += 1
        return node

    def file_ids(self, ids: Sequence[int] | np.ndarray) -> list[tuple[int, int, int] | None]:
        """(device, file id, link count) of files, as the system reports them now — for hard links
        where the scan did not record them (``Flag.HARDLINK_DUP`` marks the extra links it saw).
        Needs ``files.read``; None where a file cannot be read."""
        import os

        out: list[tuple[int, int, int] | None] = []
        for path in self.paths(ids):
            try:
                st = os.stat(path, follow_symlinks=False)
                out.append((int(st.st_dev), int(st.st_ino), int(st.st_nlink)))
            except OSError:
                out.append(None)
        return out

    def find(self, path: str) -> int | None:
        """The node at an absolute path (or a path relative to the first root), or None."""
        value, _ = self._link.request("host.find", self._params(path=path))
        found = value.get("node")
        if found is not None and value.get("epoch") is not None and int(value["epoch"]) != self.epoch:
            raise _stale(int(value["epoch"]))  # an id of the reorganised tree, not of this one
        return None if found is None else int(found)

    def ref(self, id: int, *, path: bool = True) -> NodeRef:
        """A reference to ``id`` that survives a reorganised tree when it carries the path."""
        return NodeRef(
            scan_id=self.scan_id, node=int(id), epoch=self.epoch, path=self.path(id) if path else ""
        )

    # ------------------------------------------------------------- whole columns (the image)

    def _ensure(self, names: Sequence[str]) -> Image:
        need = set(names) - self._image_cols
        if self._image is None or need:
            cols = sorted(self._image_cols | set(names))
            image = self._link.image(self.scan_id, cols)
            if image.epoch != self.epoch and self._pinned:
                raise _stale(image.epoch)  # the call's node ids are not of these columns
            self._image = image
            self._image_cols = set(cols)
            self.n = image.n
            if image.epoch != self.epoch:
                self.epoch = image.epoch
                self._nodes.clear()
        return self._image

    def col(self, name: str) -> np.ndarray:
        """A whole column, read-only, one value per node (ids 0 … n-1)."""
        if name not in COLUMNS:
            raise KeyError(f"unknown column {name!r}; one of {', '.join(COLUMNS)}")
        return self._ensure([name]).columns[name]

    def cols(self, *names: str) -> dict[str, np.ndarray]:
        image = self._ensure(names)
        return {n: image.columns[n] for n in names}

    def children(self, id: int) -> np.ndarray:
        """Ids of a folder's children (int32), empty for files."""
        image = self._ensure(["child_start", "child_count"])
        count = int(image.columns["child_count"][id])
        start = int(image.columns["child_start"][id])
        if count <= 0 or start < 1 or start + count > image.n:
            return np.zeros(0, dtype=np.int32)
        return np.arange(start, start + count, dtype=np.int32)

    def walk(self, id: int | None = None, *, chunk: int = 262_144) -> Iterator[np.ndarray]:
        """Every node from ``id`` down (default: from the roots), **level by level** (breadth first:
        ``id`` itself, then its children, then theirs …; within a level in id order), in chunks of
        ids (int64). Not depth first: a folder's descendants are not contiguous in what it yields."""
        image = self._ensure(["child_start", "child_count"])
        start_col = image.columns["child_start"].astype(np.int64)
        count_col = image.columns["child_count"].astype(np.int64)
        frontier = np.asarray(self.roots if id is None else [id], dtype=np.int64)
        while len(frontier):
            for a in range(0, len(frontier), chunk):
                yield frontier[a : a + chunk]
            counts = count_col[frontier]
            starts = start_col[frontier]
            keep = (counts > 0) & (starts >= 1)
            counts, starts = counts[keep], starts[keep]
            if not len(counts):
                break
            total = int(counts.sum())
            offsets = np.repeat(starts - np.concatenate(([0], np.cumsum(counts)[:-1])), counts)
            frontier = offsets + np.arange(total, dtype=np.int64)

    # ------------------------------------------------------------- queries (filtered by the host)

    def query(
        self,
        where: str | None = None,
        *,
        under: int | None = None,
        kind: str | None = None,
        min_size: int | None = None,
        columns: Sequence[str] = ("bytes",),
        sort: str | None = None,
        limit: int | None = None,
    ) -> Rows:
        """Rows the host selects: ``where`` is a filter expression (it matches files, as in the
        filter box), ``under`` a subtree, ``kind`` ``file`` / ``dir`` / ``link``, ``min_size`` bytes
        on disk (subtree totals for folders). ``columns`` from :data:`QUERY_COLUMNS`; ``sort`` a
        column, ``-column`` for descending.

        At most ``limit`` rows (default :data:`QUERY_LIMIT`, at most :data:`QUERY_MAX`), the first
        ones in ``sort`` order (in id order without one): ``rows.total`` says how many matched and
        ``rows.truncated`` whether some were left out. Leaving ``limit`` out and getting a truncated
        result also writes a warning to the plugin's log."""
        bad = [c for c in columns if c not in QUERY_COLUMNS]
        if bad:
            raise KeyError(f"unknown columns {bad}; use {', '.join(QUERY_COLUMNS)}")
        params = self._params(
            where=where,
            under=under,
            kind=kind,
            min_size=min_size,
            columns=list(columns),
            sort=sort,
            limit=QUERY_LIMIT if limit is None else max(1, min(int(limit), QUERY_MAX)),
        )
        value, frames = self._link.request("host.query", params)
        if not frames:
            return Rows(self, np.zeros(0, dtype=np.int64), {}, {}, int(value.get("total", 0)))
        _, cols = decode_frame(frames[0])
        ids = cols.pop("id").astype(np.int64)
        texts: dict[str, list[str]] = {}
        for name in ("name", "path"):
            if name in cols:
                blob = cols.pop(name)
                texts[name] = _split_blob(blob, cols.pop(f"{name}_len"))
        rows = Rows(self, ids, dict(cols), texts, int(value.get("total", len(ids))))
        if limit is None and rows.truncated:
            self._link.notify(
                "log",
                {
                    "level": "WARNING",
                    "message": f"tree.query({where!r}) matched {rows.total} rows; only the first "
                    f"{len(rows)} came back (limit={QUERY_LIMIT}): pass limit= (up to {QUERY_MAX}), "
                    "narrow it with under= / min_size=, or page through it",
                },
            )
        return rows

    def rule_folders(self, rule: str) -> np.ndarray:
        """The folders a folder rule decided the class of (``[[folder_rule]]``: its name, this
        plugin's; or another plugin's full id), in id order: the ones it matched itself, not the
        folders inside them, and not where the user chose a class instead."""
        value, _ = self._link.request("host.rule_folders", self._params(rule=str(rule)))
        return np.asarray(value.get("folders", []), dtype=np.int64)

    # ------------------------------------------------------------- built-in plugins only

    @property
    def store(self) -> Any:
        """The host's own store (built-in plugins running in the host process only)."""
        store = self._link.store_of(self.scan_id)
        if store is None:
            raise AttributeError("store: only built-in plugins running in the host have it")
        return store


__all__ = [
    "COLUMNS",
    "QUERY_COLUMNS",
    "QUERY_LIMIT",
    "QUERY_MAX",
    "ClassInfo",
    "Columns",
    "Flag",
    "Kind",
    "Node",
    "Row",
    "Rows",
    "TreeView",
]
