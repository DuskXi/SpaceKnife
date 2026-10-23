"""Helpers for code visualizations (``@plugin.visualization(..., base="flame")``).

A code visualization gives every node a weight (an array of length ``tree.n``); the host keeps the
files' weights, sums them into the folders and draws them with the built-in engine named by
``base`` — the layout formats stay the host's (docs/plugin-dev.md 「可视化」). Weights are
whole numbers ≥ 0; NaN and negative ones count as 0.

>>> @plugin.visualization("by_lines", T("按行数", "By lines"), base="treemap")
... def by_lines(v: VisualInput) -> np.ndarray:
...     return layout.files_only(v.tree, layout.by_column(v.tree, "size") // 40)
"""

from __future__ import annotations

import numpy as np

from .tree import Kind, TreeView


def files_only(tree: TreeView, weights: np.ndarray) -> np.ndarray:
    """``weights`` with every non-file node set to 0 (folders get the sums of their contents)."""
    w = np.asarray(weights, dtype=np.float64).copy()
    kind = tree.col("kind")
    w[: len(kind)][kind[: len(w)] != int(Kind.FILE)] = 0
    return w


def by_column(tree: TreeView, column: str = "size") -> np.ndarray:
    """A store column as weights (``size``: logical bytes, ``alloc``: on disk, ``mtime`` …)."""
    return files_only(tree, tree.col(column).astype(np.float64))


def count(tree: TreeView) -> np.ndarray:
    """One per file: areas by file count."""
    return files_only(tree, np.ones(tree.n, dtype=np.float64))


def check(tree: TreeView, weights: np.ndarray) -> list[str]:
    """What the host would do differently than you might expect (empty: nothing)."""
    w = np.asarray(weights)
    out: list[str] = []
    if w.ndim != 1:
        out.append(f"weights must be one-dimensional (got shape {w.shape})")
        return out
    if len(w) != tree.n:
        out.append(f"{len(w)} weights for {tree.n} nodes: missing ones count as 0, extra ones are ignored")
    if w.dtype.kind not in "iuf":
        out.append(f"weights must be numbers (got {w.dtype})")
        return out
    f = w.astype(np.float64)
    if np.isnan(f).any():
        out.append("NaN weights count as 0")
    if (f < 0).any():
        out.append("negative weights count as 0")
    if w.dtype.kind == "f" and np.any(np.abs(f[np.isfinite(f)] % 1) > 0):
        out.append("weights are rounded down to whole numbers: scale them up first")
    return out


__all__ = ["by_column", "check", "count", "files_only"]
