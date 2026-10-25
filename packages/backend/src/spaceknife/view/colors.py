"""What colours nodes in a view (``colors.of``): the facts the view's colour modes paint from, for
any nodes of its scan, not only the drawn rows. A web panel that draws the scan its own way paints
with the same colours as the view next to it (``sk.colors``); the frontend turns these facts into
the colours of the theme on screen, as it does for the rows.

- ``kind``: 0 a file, 2 a folder (painted as a collapsed one), 3 a folder not listed yet, -1 no node;
- ``cls`` and ``tone``: the class index and the modification-age band, as the frame columns;
- ``depth``: levels below the zoom root, and ``branch``: the slot of the zoom root's child the node
  lies in, as the view's 「顶层分支」 gave it (0 when that child is not drawn yet);
- ``pal``: a plugin colour mode's palette index, its painter asked as for drawn rows.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import numpy as np

from spaceknife_protocol import view as pv

from ..filter import FilterContext
from ..layout.rows import COLLAPSED, FILE, PENDING_RECT, DrawList
from ..store.nodes import Flag, Kind
from .state import _bands

if TYPE_CHECKING:
    from .state import View


def node_colors(view: View, nodes: list[int], mode: str) -> pv.ViewColors:
    store = view.store
    c = store.cols
    n_store = len(store)
    ids = np.asarray(nodes, dtype=np.int64)
    ok = (ids >= 0) & (ids < n_store)
    safe = np.where(ok, ids, 0)
    is_dir = c["kind"][safe] == Kind.DIR
    pending = (c["flags"][safe] & np.uint32(Flag.PENDING)) != 0
    kind = np.where(is_dir, np.where(pending, PENDING_RECT, COLLAPSED), FILE).astype(np.int64)
    kind[~ok] = -1
    cls = np.full(len(ids), 255, dtype=np.int64)
    if ok.any():
        cls[ok] = view.basis.cls_of(ids[ok])
    tone = _bands(FilterContext.current().now, c["mtime"][safe].astype(np.int64)).astype(np.int64)
    # levels below the zoom root, and the zoom root's child each node lies in
    zoom = view.zoom.node
    top_depth = int(c["depth"][zoom]) + 1 if 0 <= zoom < n_store else 1
    depth = c["depth"][safe].astype(np.int64) - (top_depth - 1)
    depth[~ok | (depth < 1)] = 0
    anc = safe.copy()
    parent = c["parent"]
    for _ in range(int(depth.max()) if len(depth) else 0):
        deep = c["depth"][anc] > top_depth
        if not deep.any():
            break
        anc = np.where(deep, parent[anc], anc)
    # the first-level block's slot (its key is its node id), as the frames gave them
    branch = np.array([view.hues.slot_of(int(a)) for a in anc.tolist()], dtype=np.int64)
    branch[depth == 0] = 0
    pal = None
    palette = None
    if mode not in pv.BUILTIN_COLOR_MODES:
        from ..extensions.registry import EXTENSIONS

        cm = EXTENSIONS.get("color_mode", mode)
        if cm is not None and cm.impl is not None:
            n = len(ids)
            draw = DrawList(
                key=np.where(ok, ids, 0).astype(np.uint32),
                node=np.where(ok, ids, -1).astype(np.int32),
                parent=np.full(n, -1, dtype=np.int32),
                kind=np.where(kind < 0, FILE, kind).astype(np.uint8),
                depth=np.maximum(depth, 1).astype(np.uint8),
                cls=cls.astype(np.uint8),
                label=np.zeros(n, dtype=np.uint8),
                size=np.zeros(n, dtype=np.int64),
                count=np.zeros(n, dtype=np.uint32),
                names=[""] * n,
            )
            got = np.asarray(cm.impl(view, draw), dtype=np.int64).reshape(-1)
            pal = got.tolist() if len(got) == n else [0] * n
            palette = mode
    return pv.ViewColors(
        mode=mode,
        palette=palette,
        kind=kind.tolist(),
        cls=cls.tolist(),
        tone=tone.tolist(),
        branch=branch.tolist(),
        depth=depth.tolist(),
        pal=pal,
    )


__all__ = ["node_colors"]
