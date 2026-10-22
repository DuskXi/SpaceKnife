"""Plugin colour modes (colors.v1): the host works out a palette index per drawn row (the ``pal``
column); the frontends paint it with the mode's palette from ``extensions.get``.

Declarative modes are rules over the drawn rows only (at most ``MAX_ROWS`` of them), all vectorised;
a code mode asks its plugin for the same indices (20 ms, else this frame is all neutral).
"""

from __future__ import annotations

import fnmatch
import re
import time
from collections.abc import Collection
from typing import TYPE_CHECKING, Any, Literal

import numpy as np
from loguru import logger

from spaceknife_protocol import CAP_COLORS
from spaceknife_sdk.manifest import ColorModeSpec, band_value

from ..i18n import _
from ..layout.rows import RectKind
from ..store.nodes import Flag, Kind
from .registry import EXTENSIONS

if TYPE_CHECKING:
    from ..layout.rows import DrawList
    from ..view.state import View

BUILTIN = frozenset({"class", "flat", "time", "branch"})


def color_unavailable(mode: str, caps: Collection[str]) -> str | None:
    """Why a session that announced ``caps`` cannot use colour mode ``mode`` (None: it can)."""
    if mode in BUILTIN:
        return None
    if CAP_COLORS not in caps:
        return _("这种配色在当前界面不可用")
    if EXTENSIONS.get("color_mode", mode) is None:
        return _("没有这种配色（插件可能已停用）")
    return None


class RulePainter:
    """A declarative colour mode (``by = ext | class | age | size | depth | name | set | insight``)."""

    def __init__(self, spec: ColorModeSpec, set_of: Any = None) -> None:
        self.spec = spec
        self.n = len(spec.palette)
        self.map = {str(k).lower().lstrip("."): int(v) for k, v in spec.map.items()}
        kind: Literal["time", "size"] = "time" if spec.by == "age" else "size"
        self.bands = np.array([band_value(b, kind) or 0 for b in spec.bands], dtype=np.int64)
        self.rules = [(re.compile(fnmatch.translate(r.glob.lower())), r.index) for r in spec.rules]
        self.set_of = set_of  # (view, set id) -> bool mask per node (node sets live in plugins)

    def __call__(self, view: View, draw: DrawList) -> np.ndarray:
        rows = len(draw)
        out = np.zeros(rows, dtype=np.uint8)
        kind = draw.kind
        node = draw.node.astype(np.int64)
        real = (node >= 0) & (kind <= RectKind.PENDING)
        if self.spec.folders == "ring":
            pick = real & (kind == RectKind.FILE)
        else:
            pick = real & (kind != RectKind.FOLDER)
        if self.spec.by == "depth":
            out[real] = np.minimum(draw.depth[real], self.n - 1)
            return out
        rows_ix = np.flatnonzero(pick)
        if not len(rows_ix):
            return out
        ids = node[rows_ix]
        store = view.store
        c = store.cols
        by = self.spec.by
        idx: np.ndarray
        if by == "ext":
            table = np.array([self.map.get(e, 0) for e in store.ext_table], dtype=np.uint8)
            idx = table[c["ext"][ids]]
        elif by == "class":
            cls = view.basis.cls_of(ids)
            names = view.basis.class_ids
            lut = np.zeros(256, dtype=np.uint8)
            for i, cid in enumerate(names):
                lut[i] = self.map.get(cid, 0)
            idx = lut[cls]
        elif by == "age":
            field = {"mtime": "mtime", "ctime": "ctime", "atime": "atime"}[self.spec.field]
            t = c[field][ids].astype(np.int64)
            age = int(time.time()) - t
            idx = (1 + np.searchsorted(self.bands, age, side="right")).astype(np.uint8)
            idx[t == 0] = 0
        elif by == "size":
            is_dir = c["kind"][ids] == Kind.DIR
            dup = (c["flags"][ids] & np.uint32(Flag.HARDLINK_DUP)) != 0
            size = np.where(is_dir, c["tot_alloc"][ids], np.where(dup, 0, c["alloc"][ids])).astype(np.int64)
            idx = (1 + np.searchsorted(self.bands, size, side="right")).astype(np.uint8)
        elif by == "name":
            idx = np.zeros(len(ids), dtype=np.uint8)
            for i, nid in enumerate(ids.tolist()):
                name = store.name(nid).lower()
                for regex, index in self.rules:
                    if regex.fullmatch(name):
                        idx[i] = index
                        break
        elif by == "set":
            mask = self.set_of(view, self.spec.set) if self.set_of is not None else None
            idx = np.zeros(len(ids), dtype=np.uint8)
            if mask is not None:
                inside = ids < len(mask)
                idx[inside] = np.where(np.asarray(mask)[ids[inside]], 1, 0)
        elif by == "insight":
            from ..insights import match_node

            idx = np.zeros(len(ids), dtype=np.uint8)
            for i, nid in enumerate(ids.tolist()[:5000]):  # thin: insight colours on the first 5000 rows
                found = match_node(store, nid)
                if found is not None:
                    idx[i] = self.map.get(str(found.kind), 0)
        else:
            return out
        out[rows_ix] = np.minimum(idx, self.n - 1)
        return out


def paint(view: View, draw: DrawList) -> tuple[np.ndarray | None, str | None]:
    """(pal column, colour mode id) of a frame in a plugin colour mode; (None, None) when the mode
    went away (the frame is drawn in the built-in colours)."""
    mode = view.color_mode
    c = EXTENSIONS.get("color_mode", mode)
    if c is None or c.impl is None:
        return None, None
    try:
        pal = c.impl(view, draw)
    except Exception:
        logger.exception("colour mode {} failed", mode)
        pal = np.zeros(len(draw), dtype=np.uint8)
    arr = np.asarray(pal, dtype=np.uint8)
    if arr.shape != (len(draw),):
        logger.warning("colour mode {} gave {} values for {} rows", mode, arr.shape, len(draw))
        arr = np.zeros(len(draw), dtype=np.uint8)
    return arr, mode


__all__ = ["RulePainter", "color_unavailable", "paint"]
