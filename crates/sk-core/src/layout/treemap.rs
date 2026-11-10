//! Nested treemap layout (`drawlist.v1`; Python `layout/engine.py`): the zoom root's subtree as a
//! flat, pre-ordered list of pixel rectangles.
//!
//! Geometry follows docs/UI-SPEC.md (T section, tokens `treemap.geometry`): every block keeps a
//! 2 px gap to its neighbours; an expanded folder has a 1 px border, 2 px padding and an 18 px
//! header; blocks under 2 px are not drawn and content under 4 px is not subdivided. All lengths
//! are scaled by the device pixel ratio and snapped to whole physical pixels (Python's
//! round-half-to-even), so seams are crisp at 125 % / 150 % and identical in every frontend.
//!
//! Small items (G5): in each container, files whose share of the area is below the minimum
//! element size are merged into one "N small items" block; siblings that are still too small
//! afterwards are merged into "+N items", placed last (merge.rs). Aggregate zoom (UI-TM-13): the
//! members of such a block fill the view; the largest are drawn one by one and the rest form one
//! "+N items" block, shrunk when that lets more members through.

use std::time::Instant;

use sk_protocol::view::DRAWLIST_LAYOUT;
use sk_store::{Flags, Kind, NodeId, Nodes};

use super::api::{Engine, LayoutParams, RootSpec, SizeSource};
use super::basis::{Basis, children};
use super::merge::{Item, MergeCtx};
use super::rows::{DrawList, KEY_FREE, KEY_UNKNOWN, Row, kind, label, label_box};
use super::squarify::{Cell, squarify_into};

/// Python's `round()` of a float: half to even.
#[inline]
pub(crate) fn pround(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Lengths in physical pixels.
#[derive(Clone, Debug)]
pub struct Geom {
    pub gap: i64,
    pub border: i64,
    pub pad: i64,
    pub header: i64,
    /// px per CSS px of label thresholds
    pub label_unit: f64,
    pub aspect: f64,
    pub min_draw: f64,
    pub min_content: f64,
    pub expand_w: f64,
    pub expand_h: f64,
    pub threshold: f64,
}

impl Geom {
    pub fn new(p: &LayoutParams) -> Geom {
        let d = p.dpr;
        Geom {
            gap: pround(p.gap * d).max(1),
            border: pround(p.border * d).max(1),
            pad: pround(p.padding * d),
            header: pround(p.header * p.font_scale * d),
            label_unit: d * p.font_scale,
            aspect: p.aspect,
            min_draw: p.min_draw * d,
            min_content: p.min_content * d,
            expand_w: p.expand_min_w * d,
            expand_h: p.expand_min_h * d,
            threshold: (p.min_element * d).powi(2),
        }
    }

    pub fn label(&self, w: i64, h: i64) -> u8 {
        label_box(w as f64 / self.label_unit, h as f64 / self.label_unit, true)
    }
}

/// The treemap behind the `treemap` visualization.
#[derive(Clone, Copy, Debug, Default)]
pub struct TreemapEngine;

impl Engine for TreemapEngine {
    fn run(
        &self,
        nodes: &Nodes,
        basis: &Basis,
        params: &LayoutParams,
        root: &RootSpec,
        sizes: &dyn SizeSource,
        out: &mut DrawList,
    ) {
        treemap(nodes, basis, params, root, sizes, out);
    }

    fn levels(&self, params: &LayoutParams) -> u32 {
        params.detail
    }
}

/// Lay out `root` as a treemap into `out`.
pub fn treemap(
    nodes: &Nodes,
    basis: &Basis,
    params: &LayoutParams,
    root: &RootSpec,
    sizes: &dyn SizeSource,
    out: &mut DrawList,
) {
    let t0 = Instant::now();
    out.clear();
    let classes = basis.classes(nodes);
    let members = std::mem::take(&mut out.members);
    let mut run = Run {
        nodes,
        p: params,
        g: Geom::new(params),
        sizes,
        n: sizes.n().min(nodes.len()),
        ctx: MergeCtx::new(nodes, &classes, params.sort_by_size, members),
        out,
        pairs: Vec::new(),
        weights: Vec::new(),
        cells: Vec::new(),
    };
    let (w, h) = params.canvas();
    let full = (0, 0, w as i64, h as i64);
    if let Some(m) = &root.members {
        run.ctx.avoid = root.key;
        run.pairs_of(m.iter().copied());
        run.container(root.node as i32, &[], full, 1, -1, true);
    } else if (root.node as usize) < nodes.len() && nodes.kind(root.node) == Kind::Dir {
        let mut extra: Vec<(u32, i64)> = Vec::new();
        if let Some(free) = root.free
            && free != 0
        {
            extra.push((KEY_FREE, free));
        }
        if let Some(u) = root.unknown
            && u > 0
        {
            extra.push((KEY_UNKNOWN, u));
        }
        let kids = children(nodes, root.node, run.n);
        run.pairs_of(kids);
        run.container(root.node as i32, &extra, full, 1, -1, false);
    } else if (root.node as usize) < run.n {
        // a file as the zoom root: its block fills the view
        run.pairs_of(std::iter::once(root.node));
        run.container(-1, &[], full, 1, -1, false);
    }
    let members = std::mem::take(&mut run.ctx.members);
    out.members = members;
    out.width = w;
    out.height = h;
    out.layout = DRAWLIST_LAYOUT;
    out.visual = "treemap".to_owned();
    out.meta = None;
    out.levels = params.detail;
    out.elapsed_s = t0.elapsed().as_secs_f64();
}

struct Run<'a, 'b> {
    nodes: &'a Nodes,
    p: &'a LayoutParams,
    g: Geom,
    sizes: &'a dyn SizeSource,
    n: usize,
    ctx: MergeCtx<'a>,
    out: &'b mut DrawList,
    /// scratch: (node, size) of the container being laid out
    pairs: Vec<(NodeId, i64)>,
    weights: Vec<f64>,
    cells: Vec<Cell>,
}

impl Run<'_, '_> {
    fn pairs_of(&mut self, ids: impl Iterator<Item = NodeId>) {
        self.pairs.clear();
        let n = self.n;
        for id in ids {
            let s = if (id as usize) < n {
                self.sizes.size(self.nodes, id)
            } else {
                0
            };
            self.pairs.push((id, s));
        }
    }

    /// Lay out the container whose nodes are in `self.pairs` in `rect`.
    fn container(
        &mut self,
        container: i32,
        extra: &[(u32, i64)],
        rect: (i64, i64, i64, i64),
        level: u32,
        parent_row: i32,
        page: bool,
    ) {
        let g = &self.g;
        let (x0, y0, x1, y1) = rect;
        // cells cover the content rectangle extended by one gap; each block keeps its gap right/bottom
        let (ex1, ey1) = (x1 + g.gap, y1 + g.gap);
        let area = ((ex1 - x0) * (ey1 - y0)) as f64;
        let mut items: Vec<Item> = Vec::new();
        let mut pairs = std::mem::take(&mut self.pairs);
        if page {
            let need = if area > 0.0 {
                g.threshold / area
            } else {
                f64::INFINITY
            };
            self.ctx.page_items(container, &mut pairs, need, &mut items);
        } else {
            let threshold = g.threshold;
            self.ctx.merge_items(
                container,
                &mut pairs,
                extra,
                |total| {
                    if area > 0.0 {
                        threshold * total / area
                    } else {
                        f64::INFINITY
                    }
                },
                &mut items,
            );
        }
        self.pairs = pairs;
        if items.is_empty() {
            return;
        }
        let aspect = g.aspect;
        let (fx0, fy0, fx1, fy1) = (x0 as f64, y0 as f64, ex1 as f64, ey1 as f64);
        let mut weights = std::mem::take(&mut self.weights);
        weights.clear();
        weights.extend(items.iter().map(Item::basis));
        let mut cells = std::mem::take(&mut self.cells);
        if page && items.len() > 1 && items.last().is_some_and(|it| it.kind == kind::MORE) {
            // the leftover gets a strip of its own: no member shares a squarify row with a far
            // larger block
            let k = weights.len() - 1;
            let body_sum = weights[..k].iter().fold(0.0, |a, &v| a + v);
            let mut two = Vec::with_capacity(2);
            squarify_into(
                &[body_sum, weights[k]],
                fx0,
                fy0,
                fx1,
                fy1,
                aspect,
                &mut two,
            );
            let (b, rest) = (two[0], two[1]);
            squarify_into(&weights[..k], b.0, b.1, b.2, b.3, aspect, &mut cells);
            cells.push(rest);
        } else {
            squarify_into(&weights, fx0, fy0, fx1, fy1, aspect, &mut cells);
        }
        self.weights = weights;
        let expandable = level < self.p.detail;
        let depth = level.min(255) as u8;
        let nodes = self.nodes;
        let pending = Flags::PENDING.bits();
        for (i, it) in items.iter().enumerate() {
            let (cx0, cy0, cx1, cy1) = cells[i];
            let g = &self.g;
            let (bx0, by0) = (pround(cx0), pround(cy0));
            let bw = pround(cx1) - g.gap - bx0;
            let bh = pround(cy1) - g.gap - by0;
            if (bw as f64) < g.min_draw || (bh as f64) < g.min_draw {
                continue;
            }
            let mut rk = it.kind;
            let node = it.node;
            if rk == kind::FILE && nodes.kind_raw(node as NodeId) == Kind::Dir as u8 {
                let id = node as NodeId;
                rk = if nodes.flags_raw(id) & pending != 0 {
                    kind::PENDING
                } else if expandable
                    && bw as f64 >= g.expand_w
                    && bh as f64 >= g.expand_h
                    && nodes.child_count(id) > 0
                {
                    kind::FOLDER
                } else {
                    kind::COLLAPSED
                };
            }
            let lbl = if rk == kind::FOLDER {
                label::NAME | label::SIZE
            } else {
                g.label(bw, bh)
            };
            let row = self.out.len() as i32;
            let name: &[u8] = if lbl != 0 && rk <= kind::PENDING {
                nodes.name_bytes(node as NodeId)
            } else {
                &[]
            };
            self.out.push(
                Row {
                    key: it.key,
                    node,
                    parent: parent_row,
                    kind: rk,
                    depth,
                    cls: it.cls,
                    label: lbl,
                    size: it.size,
                    count: it.count,
                    geo: [bx0 as i32, by0 as i32, bw as i32, bh as i32],
                },
                name,
            );
            if rk == kind::FOLDER {
                let (cx0, cy0) = (bx0 + g.border + g.pad, by0 + g.border + g.header);
                let (cx1, cy1) = (bx0 + bw - g.border - g.pad, by0 + bh - g.border - g.pad);
                if (cx1 - cx0) as f64 >= g.min_content && (cy1 - cy0) as f64 >= g.min_content {
                    let kids = children(nodes, node as NodeId, self.n);
                    self.pairs_of(kids);
                    // the nested call reuses the scratch buffers: keep this level's cells
                    self.cells = Vec::new();
                    self.container(node, &[], (cx0, cy0, cx1, cy1), level + 1, row, false);
                }
            }
        }
        self.cells = cells;
    }
}
