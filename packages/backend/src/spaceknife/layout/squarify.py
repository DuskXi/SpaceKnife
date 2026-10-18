"""Squarified treemap rows (Bruls, Huizing, van Wijk 2000).

Works in floating point; callers snap the returned cells to the pixel grid. Adjacent cells share
exactly the same boundary values (positions are accumulated, and the last cell of a row is
stretched to the row's end), so rounding never opens or overlaps a seam.
"""

from __future__ import annotations

from collections.abc import Sequence

Cell = tuple[float, float, float, float]  # x0, y0, x1, y1


def squarify(areas: Sequence[float], x0: float, y0: float, x1: float, y1: float) -> list[Cell]:
    """Cells for ``areas`` (best sorted descending; their sum should equal the rectangle's area)."""
    n = len(areas)
    out: list[Cell] = [(x0, y0, x0, y0)] * n
    total = float(sum(areas))
    if n == 0 or total <= 0 or x1 <= x0 or y1 <= y0:
        return out
    scale = (x1 - x0) * (y1 - y0) / total
    a = [v * scale for v in areas]
    i = 0
    while i < n:
        w, h = x1 - x0, y1 - y0
        if w <= 0 or h <= 0:
            for k in range(i, n):
                out[k] = (x0, y0, x0, y0)
            break
        side = w if w < h else h
        side2 = side * side
        # grow the row while the worst aspect ratio improves
        rsum = a[i]
        rmin = rmax = a[i]
        best = max(side2 * rmax / (rsum * rsum), (rsum * rsum) / (side2 * rmin)) if rsum > 0 else float("inf")
        j = i + 1
        while j < n:
            v = a[j]
            nsum = rsum + v
            nmin = v if v < rmin else rmin
            nmax = v if v > rmax else rmax
            s2 = nsum * nsum
            worst = max(side2 * nmax / s2, s2 / (side2 * nmin)) if nmin > 0 else float("inf")
            if worst > best:
                break
            rsum, rmin, rmax, best = nsum, nmin, nmax, worst
            j += 1
        last = j == n
        if w >= h:  # a column along the left edge
            cx = x1 if last else x0 + rsum / h
            y = y0
            for k in range(i, j):
                ny = y1 if k == j - 1 else y + a[k] / (cx - x0) if cx > x0 else y
                out[k] = (x0, y, cx, ny)
                y = ny
            x0 = cx
        else:  # a row along the top edge
            cy = y1 if last else y0 + rsum / w
            x = x0
            for k in range(i, j):
                nx = x1 if k == j - 1 else x + a[k] / (cy - y0) if cy > y0 else x
                out[k] = (x, y0, nx, cy)
                x = nx
            y0 = cy
        i = j
    return out
