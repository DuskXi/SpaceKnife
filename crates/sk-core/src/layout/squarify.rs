//! Squarified treemap rows (Bruls, Huizing, van Wijk 2000; Python `layout/squarify.py`).
//!
//! Works in floating point; callers snap the returned cells to the pixel grid. Adjacent cells share
//! exactly the same boundary values (positions are accumulated, and the last cell of a row is
//! stretched to the row's end), so rounding never opens or overlaps a seam.

/// `(x0, y0, x1, y1)`
pub type Cell = (f64, f64, f64, f64);

/// Cells for `areas` (best sorted descending; their sum should equal the rectangle's area),
/// written into `out` (cleared first).
///
/// `aspect`: the width / height the cells aim for (1: squares). The rectangle is laid out squarely
/// with x divided by it, then stretched back: seams stay exact.
#[allow(clippy::needless_range_loop)] // k indexes both the areas and the cells
pub fn squarify_into(
    areas: &[f64],
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    aspect: f64,
    out: &mut Vec<Cell>,
) {
    out.clear();
    if aspect != 1.0 && aspect > 0.0 {
        squarify_into(areas, x0 / aspect, y0, x1 / aspect, y1, 1.0, out);
        for c in out.iter_mut() {
            *c = (c.0 * aspect, c.1, c.2 * aspect, c.3);
        }
        return;
    }
    let n = areas.len();
    out.resize(n, (x0, y0, x0, y0));
    let total: f64 = areas.iter().fold(0.0, |a, &v| a + v);
    if n == 0 || total <= 0.0 || x1 <= x0 || y1 <= y0 {
        return;
    }
    let scale = (x1 - x0) * (y1 - y0) / total;
    let (mut x0, mut y0) = (x0, y0);
    let a = |k: usize| areas[k] * scale;
    let mut i = 0;
    while i < n {
        let (w, h) = (x1 - x0, y1 - y0);
        if w <= 0.0 || h <= 0.0 {
            for c in &mut out[i..] {
                *c = (x0, y0, x0, y0);
            }
            break;
        }
        let side = if w < h { w } else { h };
        let side2 = side * side;
        // grow the row while the worst aspect ratio improves
        let mut rsum = a(i);
        let (mut rmin, mut rmax) = (rsum, rsum);
        let mut best = if rsum > 0.0 {
            f64::max(side2 * rmax / (rsum * rsum), (rsum * rsum) / (side2 * rmin))
        } else {
            f64::INFINITY
        };
        let mut j = i + 1;
        while j < n {
            let v = a(j);
            let nsum = rsum + v;
            let nmin = if v < rmin { v } else { rmin };
            let nmax = if v > rmax { v } else { rmax };
            let s2 = nsum * nsum;
            let worst = if nmin > 0.0 {
                f64::max(side2 * nmax / s2, s2 / (side2 * nmin))
            } else {
                f64::INFINITY
            };
            if worst > best {
                break;
            }
            (rsum, rmin, rmax, best) = (nsum, nmin, nmax, worst);
            j += 1;
        }
        let last = j == n;
        if w >= h {
            // a column along the left edge
            let cx = if last { x1 } else { x0 + rsum / h };
            let mut y = y0;
            for k in i..j {
                let ny = if k == j - 1 {
                    y1
                } else if cx > x0 {
                    y + a(k) / (cx - x0)
                } else {
                    y
                };
                out[k] = (x0, y, cx, ny);
                y = ny;
            }
            x0 = cx;
        } else {
            // a row along the top edge
            let cy = if last { y1 } else { y0 + rsum / w };
            let mut x = x0;
            for k in i..j {
                let nx = if k == j - 1 {
                    x1
                } else if cy > y0 {
                    x + a(k) / (cy - y0)
                } else {
                    x
                };
                out[k] = (x, y0, nx, cy);
                x = nx;
            }
            y0 = cy;
        }
        i = j;
    }
}

/// [`squarify_into`] into a new vector.
pub fn squarify(areas: &[f64], x0: f64, y0: f64, x1: f64, y1: f64, aspect: f64) -> Vec<Cell> {
    let mut out = Vec::with_capacity(areas.len());
    squarify_into(areas, x0, y0, x1, y1, aspect, &mut out);
    out
}
