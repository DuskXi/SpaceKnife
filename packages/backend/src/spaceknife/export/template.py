"""SpaceSniffer-compatible export templates: parsing, the exported tree and rendering.

The language (docs/export-templates.md) has three parts:

* tags ``<%name%>`` — a property of the current element (``pathfile``, ``disksize``, ``nestinglevel`` …);
* brace groups ``{…}`` — plain grouping, or a function when the group starts with a keyword:
  ``{if{a}=={b}?{x}:{y}}``, ``{leftpad{text}{pad}{length}}``, ``{nest{text}}``, ``{&br}`` …;
* plain text, copied as it is (``{script}`` drops line breaks and tabs from a section first).

A template file has a header, a detail and a footer section plus sorting options (``@sort=size`` …).
The detail is rendered once per element, depth first; header and footer once, on the export root.

Rendering is streamed: :func:`render_chunks` yields pieces of about ``chunk_chars`` characters, so a
tree of millions of elements never needs one big string (:func:`render` joins them once). Parsing
never raises: problems become :class:`TemplateIssue` diagnostics and the affected part renders a
marker (``*BAD TAG*``, ``*BAD PAD*``, ``*BAD IF*``).
"""

from __future__ import annotations

import html
import json
import operator
import os
import re
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any, Literal, cast

import numpy as np

from .. import fmt
from ..i18n import Texts
from ..store.nodes import Flag, Kind, NodeStore

Blocks = Literal["files", "folders", "mixed"]
SortKey = Literal["size", "name", "none"]
Severity = Literal["error", "warning"]
Section = Literal["options", "header", "detail", "footer"]

# User-facing text of this module, translated when read (M7.5).
MESSAGES = Texts(
    {
        "unknown_tag": "未知的标签 <%{name}%>，输出 *BAD TAG*",
        "unclosed_tag": "标签缺少结尾的 %>",
        "unclosed_group": "缺少与之配对的 }}",
        "stray_close": "多余的 }}，按原样输出（要输出 }} 请写 {{&curlycl}}）",
        "unknown_entity": "未知的转义 {{{name}}}，按原样输出",
        "bad_if": "if 的写法应为 {usage}：{detail}",
        "bad_call": "{name} 的写法应为 {usage}",
        "bad_option": "选项 @{key} 的值 {value!r} 无效，可选：{allowed}",
        "unknown_option": "未知的选项 @{key}，已忽略",
        "bad_line": "这一行既不是 @选项=值，也不是段标记（@header / @detail / @footer），已忽略",
        "duplicate_section": "@{name} 段出现了不止一次，以最后一次为准",
        "if_operand": "缺少值或值没有用 {{}} 括起来",
        "if_operator": "缺少比较运算符（== != < <= > >=）",
        "if_question": "缺少 ?",
        "if_end": "结尾缺少 }}",
    }
)

BAD_TAG = "*BAD TAG*"
BAD_PAD = "*BAD PAD*"
BAD_IF = "*BAD IF*"
BAD_CALL = "*BAD CALL*"
MAX_PAD = 10_000  # longest leftpad / rightpad result (a typo must not allocate gigabytes per element)

ENTITIES = {
    "&br": "\n",
    "&tab": "\t",
    "&curlyop": "{",
    "&curlycl": "}",
    "&angleop": "<",
    "&anglecl": ">",
}
ARGS = Texts(  # names of the arguments in the usage hints (:func:`usage`)
    {
        "text": "文字",
        "pad": "填充",
        "length": "长度",
        "value1": "值1",
        "value2": "值2",
        "then": "结果",
        "else": "否则",
    }
)
USAGE: dict[str, tuple[str, ...]] = {  # function -> its arguments (keys of ARGS)
    "leftpad": ("text", "pad", "length"),
    "rightpad": ("text", "pad", "length"),
    "nest": ("text",),
    "html": ("text",),
    "csv": ("text",),
    "json": ("text",),
}
FUNCTIONS = frozenset({"if", *USAGE})
COMPARISONS = ("==", "!=", "<=", ">=", "<", ">")  # longest first
SECTIONS: tuple[Section, ...] = ("header", "detail", "footer")
OPTION_VALUES: dict[str, tuple[str, ...]] = {
    "blocks": ("files", "folders", "mixed"),
    "sort": ("size", "name", "none"),
    "order": ("desc", "asc"),
}

_WS = " \t\r\n"
_SPECIAL = re.compile(r"[{}]|<%")
_WORD = re.compile(r"&?[A-Za-z]+")
_SCRIPT = re.compile(r"\{[ \t\r\n]*script[ \t\r\n]*\}", re.IGNORECASE)
_NUMBER = re.compile(r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)")
_SECTION_LINE = re.compile(r"[ \t]*@(header|detail|footer)[ \t]*", re.IGNORECASE)
_OPTION_LINE = re.compile(r"[ \t]*@([A-Za-z]+)[ \t]*=(.*)")
_SEPARATORS = "\\/" if os.sep == "\\" else "/"
_SEPARATORS_T = tuple(_SEPARATORS)  # str.endswith takes a tuple
_DUP = int(Flag.HARDLINK_DUP)


def usage(word: str) -> str:
    """How a function is written, e.g. ``{leftpad{文字}{填充}{长度}}`` (in the interface language)."""
    if word == "if":
        a = ARGS
        return "{if{" + a["value1"] + "}=={" + a["value2"] + "}?{" + a["then"] + "}:{" + a["else"] + "}}"
    return "{" + word + "".join("{" + ARGS[arg] + "}" for arg in USAGE[word]) + "}"


@dataclass(frozen=True, slots=True)
class TemplateIssue:
    severity: Severity
    code: str
    message: str  # for the user, in the interface language at parse time
    section: Section
    line: int  # 1-based, in the template source
    column: int  # 1-based


@dataclass(frozen=True, slots=True)
class Ordering:
    """Order of a folder's elements (SpaceSniffer's block sorting, fine sorting and direction)."""

    blocks: Blocks = "mixed"  # files first, folders first, or interleaved
    sort: SortKey = "size"  # by allocated size, by name (case-insensitive), or listing order
    descending: bool = True


DEFAULT_ORDERING = Ordering()


# ---------------------------------------------------------------------------------------- the tree


class ExportTree:
    """The elements below one export root, as SpaceSniffer exports them.

    Everything below the root is collected level by level (vectorised over the folders' child
    ranges), so each folder's children are one contiguous run of *positions*; per-position arrays
    hold the ids and child runs, and — when a filter mask is given or counts are asked for — the
    subtree counts and sizes of the visible elements, folded bottom-up one level at a time.

    With ``match`` (a bool per node: the files that pass the filter) only matching files are
    visible, and a folder is visible when its subtree holds a visible file; folder sizes and counts
    are those of the visible elements. The root is always visible.

    Memory: 13 bytes per element (ids, child runs, folder flag), plus 8 with counts and 17 more
    with a filter mask; nothing per element of the rest of the store.
    """

    def __init__(
        self,
        store: NodeStore,
        root: int,
        *,
        match: np.ndarray | None = None,
        ordering: Ordering = DEFAULT_ORDERING,
        counts: bool = True,
        now: float | None = None,
    ) -> None:
        self.store = store
        self.root = root
        self.ordering = ordering
        self.now = time.time() if now is None else now
        self.n = n = len(store)
        c = store.cols
        self._kind = c["kind"]
        self._flags = c["flags"]
        self._size = c["size"]
        self._alloc = c["alloc"]
        self._tot_size = c["tot_size"]
        self._tot_alloc = c["tot_alloc"]
        self._times = (c["mtime"], c["ctime"], c["atime"])
        if not 0 < root < n:
            raise ValueError(f"node {root} is not an element of this store")
        self._collect(c, n)
        self.is_dir: np.ndarray = self._kind[self.ids] == Kind.DIR
        self.vis: np.ndarray | None = None
        self.sub_files: np.ndarray | None = None
        self.sub_dirs: np.ndarray | None = None
        self.sub_size: np.ndarray | None = None
        self.sub_alloc: np.ndarray | None = None
        self.masked = match is not None
        if match is not None or counts:
            self._aggregate(match)
        root_path = store.path(root)
        self.root_path = root_path
        if int(c["parent"][root]) == 0:  # a scan root is stored under its full path
            self.root_name = os.path.basename(root_path.rstrip(_SEPARATORS)) or root_path
        else:
            self.root_name = store.name(root)
        parent = os.path.dirname(root_path)
        self.root_parent = "" if parent == root_path else parent

    def __len__(self) -> int:
        return len(self.ids)

    # ------------------------------------------------------------------ building

    def _collect(self, c: dict[str, np.ndarray], n: int) -> None:
        """Positions level by level: ``ids``, each folder's child run ``kstart``/``kcount``."""
        kind, flags = c["kind"], c["flags"]
        counts_col, starts_col = c["child_count"], c["child_start"]
        frontier = np.array([self.root], dtype=np.int64)
        ids: list[np.ndarray] = [frontier]
        kstart: list[np.ndarray] = []
        kcount: list[np.ndarray] = []
        offsets = [0, 1]
        pos = 1
        gone = np.uint32(Flag.GONE)
        while True:
            # the count before the start, as NodeStore.child_range reads them (scans publish the
            # other way round); a run past the store or at node 0 is a folder being listed: empty
            count = counts_col[frontier].astype(np.int64)
            start = starts_col[frontier].astype(np.int64)
            ok = (kind[frontier] == Kind.DIR) & (count > 0) & (start >= 1) & (start + count <= n)
            count = np.where(ok, count, 0)
            total = int(count.sum())
            first = np.cumsum(count) - count
            kids = np.repeat(start, count) + (np.arange(total, dtype=np.int64) - np.repeat(first, count))
            keep = (flags[kids] & gone) == 0
            if not bool(keep.all()):
                owner = np.repeat(np.arange(len(frontier)), count)[keep]
                kids = kids[keep]
                count = np.bincount(owner, minlength=len(frontier)).astype(np.int64)
                first = np.cumsum(count) - count
            kstart.append((pos + first).astype(np.int32))
            kcount.append(count.astype(np.int32))
            if not len(kids):
                break
            if pos + len(kids) > n or len(offsets) > 65536:  # a torn read of a store being changed
                kcount[-1][:] = 0  # never loop: the folders of this level read as not listed
                break
            ids.append(kids)
            pos += len(kids)
            offsets.append(pos)
            frontier = kids
        self.ids: np.ndarray = np.concatenate(ids).astype(np.int32)
        self.kstart: np.ndarray = np.concatenate(kstart)
        self.kcount: np.ndarray = np.concatenate(kcount)
        self.offsets = offsets[: len(ids) + 1]

    def _aggregate(self, match: np.ndarray | None) -> None:
        """Subtree counts (and, under a mask, sizes) of the visible elements, deepest level first."""
        m = len(self.ids)
        ids = self.ids
        leaf = ~self.is_dir
        if match is not None:
            hit = np.zeros(m, dtype=bool)
            inside = ids < len(match)  # a mask computed before the store grew: newer rows miss
            hit[inside] = np.asarray(match)[ids[inside]]
            own_leaf = leaf & hit
            counted = (self._flags[ids] & np.uint32(Flag.HARDLINK_DUP)) == 0
            own_size = np.where(own_leaf & counted, self._size[ids], 0)
            own_alloc = np.where(own_leaf & counted, self._alloc[ids], 0)
            sub_size = np.zeros(m, dtype=np.int64)
            sub_alloc = np.zeros(m, dtype=np.int64)
        else:
            own_leaf = leaf
        files = np.zeros(m, dtype=np.int32)
        dirs = np.zeros(m, dtype=np.int32)
        offs = self.offsets
        for level in range(len(offs) - 2, 0, -1):
            a, b = offs[level], offs[level + 1]
            pa, pb = offs[level - 1], offs[level]
            parents = np.flatnonzero(self.kcount[pa:pb] > 0) + pa
            if not len(parents):
                continue
            seg = self.kstart[parents].astype(np.int64) - a
            is_dir = self.is_dir[a:b]
            vis_dir = is_dir & (files[a:b] > 0) if match is not None else is_dir
            files[parents] += np.add.reduceat(own_leaf[a:b] + files[a:b], seg)
            dirs[parents] += np.add.reduceat(vis_dir + dirs[a:b], seg)
            if match is not None:
                sub_size[parents] += np.add.reduceat(own_size[a:b] + sub_size[a:b], seg)
                sub_alloc[parents] += np.add.reduceat(own_alloc[a:b] + sub_alloc[a:b], seg)
        self.sub_files, self.sub_dirs = files, dirs
        if match is not None:
            vis = own_leaf | (self.is_dir & (files > 0))
            vis[0] = True
            self.vis = vis
            self.sub_size, self.sub_alloc = sub_size, sub_alloc

    # ------------------------------------------------------------------ per position

    def sizes(self, pos: int) -> tuple[int, int]:
        """(logical, allocated) bytes: subtree totals of a folder (visible files only under a mask)."""
        return self._sizes(pos, int(self.ids[pos]), bool(self.is_dir[pos]))

    def _sizes(self, pos: int, node: int, is_dir: bool) -> tuple[int, int]:
        if is_dir:
            if self.sub_size is not None and self.sub_alloc is not None:
                return int(self.sub_size[pos]), int(self.sub_alloc[pos])
            return int(self._tot_size[node]), int(self._tot_alloc[node])
        if int(self._flags[node]) & _DUP:
            return 0, 0  # another link of a file counted elsewhere (as the tree counts it)
        return int(self._size[node]), int(self._alloc[node])

    def columns(self, positions: np.ndarray) -> dict[str, np.ndarray]:
        """Values of many elements at once (the data formats read them in batches): ``kind``,
        ``flags``, effective ``size`` / ``alloc``, subtree ``files`` / ``dirs`` and the times."""
        ids = self.ids[positions]
        is_dir = self.is_dir[positions]
        flags = self._flags[ids]
        dup = (flags & np.uint32(Flag.HARDLINK_DUP)) != 0
        if self.sub_size is not None and self.sub_alloc is not None:
            folder_size, folder_alloc = self.sub_size[positions], self.sub_alloc[positions]
        else:
            folder_size, folder_alloc = self._tot_size[ids], self._tot_alloc[ids]
        if self.sub_files is None or self.sub_dirs is None:
            self._aggregate(None)
        assert self.sub_files is not None
        assert self.sub_dirs is not None
        return {
            "kind": self._kind[ids],
            "flags": flags,
            "is_dir": is_dir,
            "size": np.where(is_dir, folder_size, np.where(dup, 0, self._size[ids])),
            "alloc": np.where(is_dir, folder_alloc, np.where(dup, 0, self._alloc[ids])),
            "files": self.sub_files[positions],
            "dirs": self.sub_dirs[positions],
            "mtime": self._times[0][ids],
            "ctime": self._times[1][ids],
            "atime": self._times[2][ids],
        }

    def counts(self, pos: int) -> tuple[int, int]:
        """(files, folders) in the subtree, the element itself excluded."""
        if self.sub_files is None or self.sub_dirs is None:
            self._aggregate(None)
        files, dirs = self.sub_files, self.sub_dirs
        assert files is not None
        assert dirs is not None
        return int(files[pos]), int(dirs[pos])

    def local_counts(self, pos: int) -> tuple[int, int]:
        """(files, folders) directly inside a folder."""
        ks, kc = int(self.kstart[pos]), int(self.kcount[pos])
        if not kc:
            return 0, 0
        is_dir = self.is_dir[ks : ks + kc]
        if self.vis is not None:
            vis = self.vis[ks : ks + kc]
            return int(np.count_nonzero(vis & ~is_dir)), int(np.count_nonzero(vis & is_dir))
        dirs = int(np.count_nonzero(is_dir))
        return kc - dirs, dirs

    def kind(self, pos: int) -> Kind:
        return Kind(int(self._kind[int(self.ids[pos])]))

    def flags(self, pos: int) -> Flag:
        return Flag(int(self._flags[int(self.ids[pos])]))

    def time(self, pos: int, which: int) -> int:
        """Unix seconds of modification (0), creation (1) or access (2); 0 = unknown."""
        return int(self._times[which][int(self.ids[pos])])

    def alloc_key(self, positions: np.ndarray) -> np.ndarray:
        """Allocated bytes per position, as sorting and the report rank elements."""
        ids = self.ids[positions]
        is_dir = self.is_dir[positions]
        folder = self.sub_alloc[positions] if self.sub_alloc is not None else self._tot_alloc[ids]
        dup = (self._flags[ids] & np.uint32(Flag.HARDLINK_DUP)) != 0
        own = np.where(dup, 0, self._alloc[ids])
        key: np.ndarray = np.where(is_dir, folder, own)
        return key

    def children(self, pos: int) -> np.ndarray:
        """Positions of a folder's visible children, in export order."""
        ks, kc = int(self.kstart[pos]), int(self.kcount[pos])
        r = np.arange(ks, ks + kc, dtype=np.int64)
        if self.vis is not None and kc:
            r = r[self.vis[ks : ks + kc]]
        if len(r) < 2:
            return r
        o = self.ordering
        block = None
        if o.blocks != "mixed":
            dirs = self.is_dir[r]
            block = dirs if o.blocks == "files" else ~dirs
        if o.sort == "name":
            names = [self.store.name(int(i)).casefold() for i in self.ids[r].tolist()]
            order = sorted(range(len(r)), key=names.__getitem__, reverse=o.descending)
            r = r[np.asarray(order, dtype=np.int64)]
            if block is not None:
                r = r[np.argsort(block[np.asarray(order)], kind="stable")]
            return r
        if o.sort == "size":
            key = self.alloc_key(r)
            fine = -key if o.descending else key
            idx = np.lexsort((fine, block)) if block is not None else np.argsort(fine, kind="stable")
            return r[idx]
        if block is not None:
            return r[np.argsort(block, kind="stable")]
        return r

    def root_element(self) -> Element:
        is_dir = bool(self.is_dir[0])
        return Element(self, 0, 0, self.root, is_dir, self.root_path, self.root_parent, self.root_name)

    def walk(self, max_level: int | None = None) -> Iterator[Element]:
        """Every visible element, depth first (the root first), each folder's children in order."""
        name_of = self.store.name
        ids, is_dir = self.ids, self.is_dir
        root = self.root_element()
        yield root
        if not root.is_dir or max_level == 0:
            return
        # one frame per open folder (its children in order, a cursor); paths[k]: the folder at level k
        frames = [_Frame(self.children(0), ids, is_dir, 1)]
        paths = [root.path]
        while frames:
            frame = frames[-1]
            item = frame.next()
            if item is None:
                frames.pop()
                continue
            pos, node, folder = item
            level = frame.level
            name = name_of(node)
            parent = paths[level - 1]
            path = parent + name if parent.endswith(_SEPARATORS_T) else parent + os.sep + name
            yield Element(self, pos, level, node, folder, path, parent, name)
            if folder and (max_level is None or level < max_level):
                kids = self.children(pos)
                if len(kids):
                    del paths[level:]
                    paths.append(path)
                    frames.append(_Frame(kids, ids, is_dir, level + 1))


class _Frame:
    """A folder being walked: its children's positions and a cursor. Children become Python values
    a few hundred at a time, so a folder of a million entries costs 8 bytes per entry, not a tuple."""

    __slots__ = ("base", "dirs", "end", "i", "ids", "is_dir", "kids", "level", "nodes", "pos")
    CHUNK = 256

    def __init__(self, kids: np.ndarray, ids: np.ndarray, is_dir: np.ndarray, level: int) -> None:
        self.kids, self.ids, self.is_dir, self.level = kids, ids, is_dir, level
        self.i = self.end = self.base = 0
        self.pos: list[int] = []
        self.nodes: list[int] = []
        self.dirs: list[bool] = []

    def next(self) -> tuple[int, int, bool] | None:
        if self.i >= self.end:
            if self.end >= len(self.kids):
                return None
            self.base = self.end
            self.end = min(self.end + self.CHUNK, len(self.kids))
            run = self.kids[self.base : self.end]
            self.pos, self.nodes, self.dirs = run.tolist(), self.ids[run].tolist(), self.is_dir[run].tolist()
        k = self.i - self.base
        self.i += 1
        return self.pos[k], self.nodes[k], self.dirs[k]


class Element:
    """One exported element: what tags and the formats read."""

    __slots__ = ("_sizes", "is_dir", "level", "name", "node", "parent_path", "path", "pos", "tree")

    def __init__(
        self,
        tree: ExportTree,
        pos: int,
        level: int,
        node: int,
        is_dir: bool,
        path: str,
        parent: str,
        name: str,
    ) -> None:
        self.tree = tree
        self.pos = pos
        self.level = level  # nesting level: 0 for the export root
        self.node = node
        self.is_dir = is_dir
        self.path = path
        self.parent_path = parent
        self.name = name
        self._sizes: tuple[int, int] | None = None

    def sizes(self) -> tuple[int, int]:
        """(logical, allocated) bytes, as :meth:`ExportTree.sizes` gives them."""
        if self._sizes is None:
            self._sizes = self.tree._sizes(self.pos, self.node, self.is_dir)
        return self._sizes

    def stem_ext(self) -> tuple[str, str]:
        """(name without extension, extension without the dot); folders have no extension."""
        name = self.name
        dot = name.rfind(".")
        if self.is_dir or dot <= 0 or dot == len(name) - 1:
            return name, ""
        return name[:dot], name[dot + 1 :]


# ---------------------------------------------------------------------------------------- tags


def _bool(value: bool) -> str:
    return "true" if value else "false"


def local_time(ts: int, pattern: str) -> str:
    """``ts`` in local time as ``pattern`` (strftime); "" when unknown (0) or out of range."""
    if ts <= 0:
        return ""
    try:
        return time.strftime(pattern, time.localtime(ts))
    except (OSError, OverflowError, ValueError):  # Windows: nothing before 1970 in local time
        return ""


def _date(ts: int) -> str:
    return local_time(ts, "%Y-%m-%d")


def _clock(ts: int) -> str:
    return local_time(ts, "%H:%M:%S")


def _age(el: Element, ts: int) -> str:
    if ts <= 0:
        return ""
    return str(max(0, int((el.tree.now - ts) // 86400)))  # whole days


def _counts(el: Element) -> tuple[int, int]:
    return el.tree.counts(el.pos) if el.is_dir else (0, 0)


def _local(el: Element) -> tuple[int, int]:
    return el.tree.local_counts(el.pos) if el.is_dir else (0, 0)


def _time_tags(prefix: str, which: int) -> dict[str, Callable[[Element], str]]:
    return {
        f"file{prefix}date": lambda e: _date(e.tree.time(e.pos, which)),
        f"file{prefix}time": lambda e: _clock(e.tree.time(e.pos, which)),
        f"file{prefix}age": lambda e: _age(e, e.tree.time(e.pos, which)),
    }


TAGS: dict[str, Callable[[Element], str]] = {
    "file": lambda e: e.name,
    "filename": lambda e: e.stem_ext()[0],
    "fileext": lambda e: e.stem_ext()[1],
    "path": lambda e: e.parent_path,
    "pathfile": lambda e: e.path,
    "size": lambda e: fmt.size(e.sizes()[0]),
    "sizebytes": lambda e: str(e.sizes()[0]),
    "disksize": lambda e: fmt.size(e.sizes()[1]),
    "disksizebytes": lambda e: str(e.sizes()[1]),
    **_time_tags("create", 1),
    **_time_tags("modify", 0),
    **_time_tags("access", 2),
    "isfile": lambda e: _bool(not e.is_dir),
    "isfolder": lambda e: _bool(e.is_dir),
    "iscontainer": lambda e: _bool(e.is_dir),
    "count": lambda e: str(sum(_counts(e))),
    "localcount": lambda e: str(sum(_local(e))),
    "filecount": lambda e: str(_counts(e)[0]),
    "localfilecount": lambda e: str(_local(e)[0]),
    "foldercount": lambda e: str(_counts(e)[1]),
    "localfoldercount": lambda e: str(_local(e)[1]),
    "containercount": lambda e: str(_counts(e)[1]),
    "localcontainercount": lambda e: str(_local(e)[1]),
    "containsfiles": lambda e: _bool(_local(e)[0] > 0),
    "containsfolders": lambda e: _bool(_local(e)[1] > 0),
    "containscontainers": lambda e: _bool(_local(e)[1] > 0),
    "nestinglevel": lambda e: str(e.level),
}
COUNT_TAGS = frozenset(
    {"count", "filecount", "foldercount", "containercount"}
)  # need the subtree counts (folded over the whole tree once)


# ---------------------------------------------------------------------------------------- nodes


class _Node:
    __slots__ = ()

    def put(self, el: Element, out: list[str]) -> None:
        raise NotImplementedError


class _Text(_Node):
    __slots__ = ("text",)

    def __init__(self, text: str) -> None:
        self.text = text

    def put(self, el: Element, out: list[str]) -> None:
        out.append(self.text)


class _Tag(_Node):
    __slots__ = ("fn",)

    def __init__(self, fn: Callable[[Element], str]) -> None:
        self.fn = fn

    def put(self, el: Element, out: list[str]) -> None:
        out.append(self.fn(el))


class _Seq(_Node):
    __slots__ = ("items",)
    items: tuple[_Node, ...]

    def __init__(self, items: list[_Node]) -> None:
        merged: list[_Node] = []
        for item in items:
            if isinstance(item, _Seq):
                merged.extend(item.items)
            elif isinstance(item, _Text) and merged and isinstance(merged[-1], _Text):
                merged[-1] = _Text(merged[-1].text + item.text)
            elif not (isinstance(item, _Text) and not item.text):
                merged.append(item)
        self.items = tuple(merged)

    def put(self, el: Element, out: list[str]) -> None:
        for item in self.items:
            item.put(el, out)


def _text(node: _Node, el: Element) -> str:
    if isinstance(node, _Text):
        return node.text
    out: list[str] = []
    node.put(el, out)
    return "".join(out)


def compare(a: str, op: str, b: str) -> bool:
    """``{if}``: numbers compare as numbers, everything else as case-insensitive text."""
    if a == b and op in ("==", "!=", "<=", ">="):  # the common case ({<%isfile%>} == {true}) first
        return op != "!="
    x, y = a.strip(), b.strip()
    if _NUMBER.fullmatch(x) and _NUMBER.fullmatch(y):
        try:
            dx, dy = Decimal(x), Decimal(y)
        except InvalidOperation:  # pragma: no cover - the pattern only admits valid decimals
            dx = dy = Decimal(0)
        return _cmp(dx, op, dy)
    return _cmp(x.casefold(), op, y.casefold())


_OPERATORS: dict[str, Callable[[Any, Any], Any]] = {
    "==": operator.eq,
    "!=": operator.ne,
    "<": operator.lt,
    "<=": operator.le,
    ">": operator.gt,
    ">=": operator.ge,
}


def _cmp(x: str | Decimal, op: str, y: str | Decimal) -> bool:
    return bool(_OPERATORS[op](x, y))


class _If(_Node):
    __slots__ = ("a", "b", "no", "op", "yes")

    def __init__(self, a: _Node, op: str, b: _Node, yes: _Node, no: _Node | None) -> None:
        self.a, self.op, self.b, self.yes, self.no = a, op, b, yes, no

    def put(self, el: Element, out: list[str]) -> None:
        if compare(_text(self.a, el), self.op, _text(self.b, el)):
            self.yes.put(el, out)
        elif self.no is not None:
            self.no.put(el, out)


class _Pad(_Node):
    __slots__ = ("left", "length", "pad", "text")

    def __init__(self, left: bool, text: _Node, pad: _Node, length: _Node) -> None:
        self.left, self.text, self.pad, self.length = left, text, pad, length

    def put(self, el: Element, out: list[str]) -> None:
        text, pad, length = _text(self.text, el), _text(self.pad, el), _text(self.length, el).strip()
        if not pad or not length.isdigit() or not length.isascii() or int(length) > MAX_PAD:
            out.append(BAD_PAD)
            return
        missing = int(length) - len(text)
        if missing <= 0:
            out.append(text)
            return
        fill = (pad * (missing // len(pad) + 1))[:missing]
        out.append(fill + text if self.left else text + fill)


class _Nest(_Node):
    __slots__ = ("body",)

    def __init__(self, body: _Node) -> None:
        self.body = body

    def put(self, el: Element, out: list[str]) -> None:
        if el.level:
            out.append(_text(self.body, el) * el.level)


def csv_field(text: str) -> str:
    """One CSV field (RFC 4180): quoted when it holds a comma, quote, line break or edge space."""
    if any(ch in text for ch in ',"\r\n') or text != text.strip(" "):
        return '"' + text.replace('"', '""') + '"'
    return text


_ESCAPES: dict[str, Callable[[str], str]] = {
    "html": lambda s: html.escape(s, quote=True),
    "csv": csv_field,
    "json": lambda s: json.dumps(s, ensure_ascii=False),
}


class _Escape(_Node):
    __slots__ = ("body", "fn")

    def __init__(self, fn: Callable[[str], str], body: _Node) -> None:
        self.fn, self.body = fn, body

    def put(self, el: Element, out: list[str]) -> None:
        out.append(self.fn(_text(self.body, el)))


# ---------------------------------------------------------------------------------------- parsing


class _Parser:
    """Recursive descent over one section; positions map back to the template source."""

    def __init__(
        self,
        text: str,
        section: Section,
        origin: list[int] | None,
        raw: str,
        first_line: int,
        issues: list[TemplateIssue],
    ) -> None:
        self.t = text
        self.n = len(text)
        self.section = section
        self.origin = origin  # offset in ``raw`` of each character (script mode dropped some)
        self.raw = raw
        self.first_line = first_line
        self.issues = issues
        self.tags: set[str] = set()

    def issue(self, at: int, code: str, severity: Severity = "error", **kw: object) -> None:
        if self.origin is None:
            o = min(at, len(self.raw))
        else:
            o = self.origin[at] if at < len(self.origin) else len(self.raw)
        line = self.first_line + self.raw.count("\n", 0, o)
        column = o - (self.raw.rfind("\n", 0, o) + 1) + 1
        self.issues.append(
            TemplateIssue(severity, code, MESSAGES[code].format(**kw), self.section, line, column)
        )

    def ws(self, i: int) -> int:
        t, n = self.t, self.n
        while i < n and t[i] in _WS:
            i += 1
        return i

    def run(self) -> _Node:
        items, _ = self.items(0, inner=False)
        return _Seq(items)

    def items(self, i: int, inner: bool) -> tuple[list[_Node], int]:
        t, n = self.t, self.n
        out: list[_Node] = []
        while i < n:
            m = _SPECIAL.search(t, i)
            j = m.start() if m else n
            if j > i:
                out.append(_Text(t[i:j]))
                i = j
                continue
            if t[i] == "{":
                node, i = self.group(i)
                out.append(node)
            elif t[i] == "}":
                if inner:
                    return out, i
                self.issue(i, "stray_close", "warning")
                out.append(_Text("}"))
                i += 1
            else:  # "<%"
                end = t.find("%>", i + 2)
                if end < 0:
                    self.issue(i, "unclosed_tag")
                    out.append(_Text(t[i:]))
                    i = n
                else:
                    out.append(self.tag(t[i + 2 : end], i))
                    i = end + 2
        return out, i

    def tag(self, raw_name: str, at: int) -> _Node:
        name = raw_name.strip().lower()
        fn = TAGS.get(name)
        if fn is None:
            self.issue(at, "unknown_tag", name=raw_name.strip())
            return _Text(BAD_TAG)
        self.tags.add(name)
        return _Tag(fn)

    def group(self, i: int) -> tuple[_Node, int]:
        """A ``{…}`` group at ``i``: an entity, a function call, or plain grouping."""
        t, n = self.t, self.n
        j = self.ws(i + 1)
        m = _WORD.match(t, j)
        word = m.group().lower() if m else ""
        end = m.end() if m else j
        after = t[end] if end < n else ""
        if word in ENTITIES or word == "script" or word.startswith("&"):
            k = self.ws(end)
            if k < n and t[k] == "}":
                if word in ENTITIES:
                    return _Text(ENTITIES[word]), k + 1
                if word == "script":
                    return _Text(""), k + 1  # the section was already prepared (see _compile)
                self.issue(i, "unknown_entity", "warning", name=t[j:end])
        elif word in FUNCTIONS and (not after or after in _WS or after in "{}"):
            return self.call(word, i, end)
        items, k = self.items(i + 1, inner=True)
        if k >= n:
            self.issue(i, "unclosed_group")
            return _Seq(items), n
        return _Seq(items), k + 1

    def arg(self, i: int) -> tuple[_Node | None, int]:
        j = self.ws(i)
        if j < self.n and self.t[j] == "{":
            return self.group(j)
        return None, j

    def skip_group(self, i: int) -> int:
        """Index after the ``}`` closing the group that opens at ``i`` (or the end)."""
        depth = 0
        for k in range(i, self.n):
            ch = self.t[k]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return k + 1
        return self.n

    def call(self, word: str, start: int, i: int) -> tuple[_Node, int]:
        if word == "if":
            return self.call_if(start, i)
        arity = 3 if word in ("leftpad", "rightpad") else 1
        args: list[_Node] = []
        for _ in range(arity):
            node, i = self.arg(i)
            if node is None:
                break
            args.append(node)
        k = self.ws(i)
        if len(args) == arity and k < self.n and self.t[k] == "}":
            if word in ("leftpad", "rightpad"):
                return _Pad(word == "leftpad", args[0], args[1], args[2]), k + 1
            if word == "nest":
                return _Nest(args[0]), k + 1
            return _Escape(_ESCAPES[word], args[0]), k + 1
        self.issue(start, "bad_call", name=word, usage=usage(word))
        marker = BAD_PAD if word in ("leftpad", "rightpad") else BAD_CALL
        return _Text(marker), self.skip_group(start)

    def call_if(self, start: int, i: int) -> tuple[_Node, int]:
        def fail(code: str) -> tuple[_Node, int]:
            self.issue(start, "bad_if", usage=usage("if"), detail=MESSAGES[code].format())
            return _Text(BAD_IF), self.skip_group(start)

        a, i = self.arg(i)
        if a is None:
            return fail("if_operand")
        j = self.ws(i)
        op = next((o for o in COMPARISONS if self.t.startswith(o, j)), None)
        if op is None:
            return fail("if_operator")
        b, i = self.arg(j + len(op))
        if b is None:
            return fail("if_operand")
        j = self.ws(i)
        if j >= self.n or self.t[j] != "?":
            return fail("if_question")
        yes, i = self.arg(j + 1)
        if yes is None:
            return fail("if_operand")
        j = self.ws(i)
        no: _Node | None = None
        if j < self.n and self.t[j] == ":":
            no, i = self.arg(j + 1)
            if no is None:
                return fail("if_operand")
            j = self.ws(i)
        if j >= self.n or self.t[j] != "}":
            return fail("if_end")
        return _If(a, op, b, yes, no), j + 1


@dataclass(frozen=True, slots=True)
class _Compiled:
    node: _Node
    tags: frozenset[str]
    static: str | None  # the output when the section has no tags and no functions


def _compile(raw: str, section: Section, first_line: int, issues: list[TemplateIssue]) -> _Compiled:
    text, origin = raw, None
    if _SCRIPT.search(raw):  # script mode: line breaks and tabs are layout, not output
        origin = [i for i, ch in enumerate(raw) if ch not in "\r\n\t"]
        text = "".join(raw[i] for i in origin)
    parser = _Parser(text, section, origin, raw, first_line, issues)
    node = parser.run()
    static = None
    if isinstance(node, _Seq) and all(isinstance(it, _Text) for it in node.items):
        static = "".join(it.text for it in node.items if isinstance(it, _Text))
    return _Compiled(node, frozenset(parser.tags), static)


@dataclass(frozen=True, slots=True)
class ExportTemplate:
    """A parsed template file: sections, ordering, display data and diagnostics."""

    name: str
    file_name: str  # suggested output file name ("" = none)
    ordering: Ordering
    header: str
    detail: str
    footer: str
    issues: tuple[TemplateIssue, ...]
    _compiled: tuple[_Compiled, _Compiled, _Compiled] = field(repr=False, compare=False)

    @property
    def ok(self) -> bool:
        """No errors (warnings allowed)."""
        return not any(i.severity == "error" for i in self.issues)

    @property
    def tags(self) -> frozenset[str]:
        return self._compiled[0].tags | self._compiled[1].tags | self._compiled[2].tags

    @property
    def needs_counts(self) -> bool:
        return bool(self.tags & COUNT_TAGS)


def parse_template(source: str, name: str = "") -> ExportTemplate:
    """Parse a template file (or a bare detail section); never raises.

    A source whose first line starts with ``@`` is a template file: ``@key=value`` option lines
    (``@name`` ``@file`` ``@blocks`` ``@sort`` ``@order``), blank lines and ``#`` comments, then the
    sections, each introduced by a line ``@header``, ``@detail`` or ``@footer``. Anything else is the
    detail section alone. Line breaks are normalised to ``\\n``; a section's last line break (the
    one before the next section line or the end of the file) is not part of it.
    """
    text = source.removeprefix("﻿").replace("\r\n", "\n").replace("\r", "\n")
    text = text.removesuffix("\n")
    issues: list[TemplateIssue] = []
    sections: dict[Section, tuple[str, int]] = {}
    options: dict[str, str] = {}
    if not text.startswith("@"):
        sections["detail"] = (text, 1)
    else:
        current: Section | None = None
        body: list[str] = []
        start = 0

        def close() -> None:
            if current is not None:
                if current in sections:
                    issues.append(
                        TemplateIssue(
                            "warning",
                            "duplicate_section",
                            MESSAGES["duplicate_section"].format(name=current),
                            current,
                            start,
                            1,
                        )
                    )
                sections[current] = ("\n".join(body), start + 1)

        for number, line in enumerate(text.split("\n"), start=1):
            marker = _SECTION_LINE.fullmatch(line)
            if marker is not None:
                close()
                current = cast(Section, marker.group(1).lower())
                body, start = [], number
                continue
            if current is not None:
                body.append(line)
                continue
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            opt = _OPTION_LINE.fullmatch(line)
            if opt is None:
                issues.append(
                    TemplateIssue("warning", "bad_line", MESSAGES["bad_line"], "options", number, 1)
                )
                continue
            key, value = opt.group(1).lower(), opt.group(2).strip()
            if key in ("name", "file"):
                options[key] = value
            elif key in OPTION_VALUES:
                if value.lower() in OPTION_VALUES[key] or (key == "blocks" and not value):
                    options[key] = value.lower() or "mixed"
                else:
                    message = MESSAGES["bad_option"].format(
                        key=key, value=value, allowed=" / ".join(OPTION_VALUES[key])
                    )
                    issues.append(TemplateIssue("warning", "bad_option", message, "options", number, 1))
            else:
                message = MESSAGES["unknown_option"].format(key=key)
                issues.append(TemplateIssue("warning", "unknown_option", message, "options", number, 1))
        close()
    compiled = []
    for section in SECTIONS:
        raw, first = sections.get(section, ("", 1))
        compiled.append(_compile(raw, section, first, issues))
    ordering = Ordering(
        blocks=cast(Blocks, options.get("blocks", "mixed")),
        sort=cast(SortKey, options.get("sort", "size")),
        descending=options.get("order", "desc") == "desc",
    )
    return ExportTemplate(
        name=options.get("name", name),
        file_name=options.get("file", ""),
        ordering=ordering,
        header=sections.get("header", ("", 1))[0],
        detail=sections.get("detail", ("", 1))[0],
        footer=sections.get("footer", ("", 1))[0],
        issues=tuple(issues),
        _compiled=(compiled[0], compiled[1], compiled[2]),
    )


def check(source: str) -> list[TemplateIssue]:
    """Diagnostics of a template source (for the editor and before an export)."""
    return list(parse_template(source).issues)


# ---------------------------------------------------------------------------------------- render


def export_roots(store: NodeStore, root: int) -> list[int]:
    """The roots an export covers: node 0 (the virtual top) stands for every scan root."""
    return list(store.roots()) if root == 0 else [root]


def render_chunks(
    store: NodeStore,
    root: int,
    template: str | ExportTemplate,
    *,
    match: np.ndarray | None = None,
    now: float | None = None,
    chunk_chars: int = 1 << 16,
) -> Iterator[str]:
    """Render ``template`` for the tree below ``root`` in pieces of about ``chunk_chars`` characters.

    ``root`` 0 renders every scan root in turn, each as its own export (header, detail, footer).
    ``match`` is a filter mask (a bool per node, :attr:`FilterResult.match`): only matching files
    and the folders that contain them are exported, with the sizes and counts of what is visible.
    """
    tpl = template if isinstance(template, ExportTemplate) else parse_template(template)
    header, detail, footer = tpl._compiled
    counts = tpl.needs_counts
    parts: list[str] = []
    size = 0
    for r in export_roots(store, root):
        tree = ExportTree(store, r, match=match, ordering=tpl.ordering, counts=counts, now=now)
        top = tree.root_element()
        header.node.put(top, parts)
        if detail.static != "":
            node = detail.node
            for el in tree.walk():
                before = len(parts)
                node.put(el, parts)
                for k in range(before, len(parts)):
                    size += len(parts[k])
                if size >= chunk_chars:
                    yield "".join(parts)
                    parts.clear()
                    size = 0
        footer.node.put(top, parts)
    if parts:
        out = "".join(parts)
        if out:
            yield out


def render(
    store: NodeStore,
    root: int,
    template: str | ExportTemplate,
    *,
    match: np.ndarray | None = None,
    now: float | None = None,
) -> str:
    """The whole output of :func:`render_chunks` as one string."""
    return "".join(render_chunks(store, root, template, match=match, now=now))
