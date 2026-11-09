//! Partition layouts: the zoom root's subtree as nested intervals, one level per band or ring
//! (Python `layout/partition.py`).
//!
//! Shared by the flame graph (`flame.v1`) and the polar flame graph (`polar.v1`). Every row owns
//! an interval [u0, u1) of the zoom root's whole width or whole turn and a level (1 = the zoom
//! root's children); a [`Projection`] snaps intervals to pixels and says what is visible, what can
//! show its children on the next level and which label bits a row gets. The zoom root itself is
//! not a row: frontends draw it as the root bar or the centre disc (`FrameInfo.root`).
//!
//! A container shares its interval out by bytes against max(its children's bytes, its own size);
//! siblings share their boundaries; a child always lies within its parent. Small items merge as
//! in the treemap with a length instead of an area. Frames hold at most [`MAX_ROWS`] rows: a
//! folder whose children would not fit any more is drawn collapsed.

use std::time::Instant;

use sk_protocol::view::FrameGeometry;
use sk_store::{Flags, Kind, NodeId, Nodes};

use super::api::{Engine, LayoutParams, MAX_ROWS, RootSpec, SizeSource};
use super::basis::{Basis, children};
use super::merge::{Item, MergeCtx};
use super::rows::{DrawList, KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, KEY_UNKNOWN, Row, kind};

/// A row's geometry, in the order of the layout's geometry columns.
pub type Cell = [i32; 4];

/// Where the intervals of one viewport go: built per run from the layout parameters.
pub trait Projection {
    /// Levels below the zoom root that fit (at most `LayoutParams::detail`).
    fn levels(&self) -> u32;
    /// Pixels the whole interval [0, 1) spans along `level`: the length scale of the merges.
    fn unit_px(&self, level: u32) -> f64;
    /// Snapped geometry of [a, b) on `level`; None when it is too small to draw.
    fn cell(&self, a: f64, b: f64, level: u32) -> Option<Cell>;
    /// Whether a folder drawn as `cell` is large enough to show its children on the next level.
    fn expandable(&self, cell: &Cell, level: u32) -> bool;
    /// Label bits of a row of `kind` drawn as `cell`.
    fn label(&self, cell: &Cell, kind: u8) -> u8;
    /// `FrameInfo.geometry` of the frame.
    fn meta(&self) -> FrameGeometry;
}

/// Builds the projection of one run.
pub type MakeProjection = fn(&LayoutParams) -> Box<dyn Projection>;

/// Lays out a store as a partition for one projection (an [`Engine`]).
#[derive(Clone)]
pub struct PartitionEngine {
    pub make_projection: MakeProjection,
    /// wire layout of the rows ("flame.v1", "polar.v1")
    pub layout: &'static str,
    pub visual: &'static str,
    /// rows per frame (tests lower it)
    pub max_rows: usize,
}

impl PartitionEngine {
    pub fn new(
        make_projection: MakeProjection,
        layout: &'static str,
        visual: &'static str,
    ) -> Self {
        PartitionEngine {
            make_projection,
            layout,
            visual,
            max_rows: MAX_ROWS,
        }
    }
}

impl Engine for PartitionEngine {
    fn levels(&self, params: &LayoutParams) -> u32 {
        (self.make_projection)(params).levels()
    }

    fn run(
        &self,
        nodes: &Nodes,
        basis: &Basis,
        params: &LayoutParams,
        root: &RootSpec,
        sizes: &dyn SizeSource,
        out: &mut DrawList,
    ) {
        let t0 = Instant::now();
        out.clear();
        let proj = (self.make_projection)(params);
        let classes = basis.classes(nodes);
        let members = std::mem::take(&mut out.members);
        let n = sizes.n().min(nodes.len());
        let mut run = Run {
            nodes,
            proj: &*proj,
            sizes,
            n,
            min_px: params.min_element * params.dpr,
            max_rows: self.max_rows,
            reserved: 0,
            ctx: MergeCtx::new(nodes, &classes, params.sort_by_size, members),
            out,
            pairs: Vec::new(),
        };
        if proj.levels() > 0 {
            if let Some(m) = &root.members {
                run.ctx.avoid = root.key;
                run.pairs_of(m.iter().copied());
                run.top(root.node as i32, &[], true);
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
                run.pairs_of(children(nodes, root.node, n));
                run.top(root.node as i32, &extra, false);
            }
            // a file as the zoom root: no rows, the root bar / centre disc is the file
        }
        let members = std::mem::take(&mut run.ctx.members);
        let (w, h) = params.canvas();
        out.members = members;
        out.width = w;
        out.height = h;
        out.layout = self.layout;
        out.visual = self.visual.to_owned();
        out.meta = Some(proj.meta());
        out.levels = proj.levels();
        out.elapsed_s = t0.elapsed().as_secs_f64();
    }
}

struct Run<'a, 'b> {
    nodes: &'a Nodes,
    proj: &'a dyn Projection,
    sizes: &'a dyn SizeSource,
    n: usize,
    min_px: f64,
    max_rows: usize,
    /// items of the containers being emitted that are still to come
    reserved: usize,
    ctx: MergeCtx<'a>,
    out: &'b mut DrawList,
    pairs: Vec<(NodeId, i64)>,
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

    fn size_of(&self, id: i32) -> i64 {
        if id >= 0 && (id as usize) < self.n {
            self.sizes.size(self.nodes, id as NodeId)
        } else {
            0
        }
    }

    /// Blocks of a container of `own` bytes (its nodes in `self.pairs`) whose interval is `span`
    /// long, and the bytes the interval stands for.
    fn items(
        &mut self,
        container: i32,
        own: i64,
        extra: &[(u32, i64)],
        span: f64,
        level: u32,
        page: bool,
    ) -> (Vec<Item>, f64) {
        let length = self.proj.unit_px(level) * span;
        let mut items = Vec::new();
        if length <= 0.0 {
            return (items, 0.0);
        }
        let mut pairs = std::mem::take(&mut self.pairs);
        let total = if page {
            self.ctx
                .page_items(container, &mut pairs, self.min_px / length, &mut items);
            // Python's sum(): integers exactly until the first float
            let mut isum = 0i64;
            let mut fsum: Option<f64> = None;
            for it in &items {
                match fsum {
                    Some(f) => fsum = Some(f + it.basis()),
                    None if it.weight != 0.0 => fsum = Some(isum as f64 + it.weight),
                    None => isum += it.size,
                }
            }
            fsum.unwrap_or(isum as f64)
        } else {
            let positive: i64 = pairs.iter().filter(|p| p.1 > 0).map(|p| p.1).sum();
            let extra_sum: i64 = extra.iter().map(|e| e.1).sum();
            let total = f64::max(positive as f64 + extra_sum as f64, own as f64);
            let threshold = self.min_px * total / length;
            self.ctx
                .merge_items(container, &mut pairs, extra, |_| threshold, &mut items);
            total
        };
        self.pairs = pairs;
        (items, total)
    }

    fn top(&mut self, container: i32, extra: &[(u32, i64)], page: bool) {
        let own = if page { 0 } else { self.size_of(container) };
        let (items, total) = self.items(container, own, extra, 1.0, 1, page);
        self.reserved = items.len();
        self.container(&items, total, 0.0, 1.0, 1, -1);
    }

    /// Emit `items` over [u0, u1) of `level`, `total` bytes filling the interval.
    fn container(
        &mut self,
        items: &[Item],
        total: f64,
        u0: f64,
        u1: f64,
        level: u32,
        parent_row: i32,
    ) {
        if items.is_empty() || total <= 0.0 {
            return;
        }
        let span = u1 - u0;
        let deeper = level < self.proj.levels();
        let depth = level.min(255) as u8;
        let nodes = self.nodes;
        let pending = Flags::PENDING.bits();
        let mut done = 0.0f64; // bytes placed so far: boundaries from running totals
        let mut b = u0;
        for it in items {
            self.reserved -= 1;
            done += it.basis();
            let a = b;
            b = if done >= total {
                u1
            } else {
                f64::min(u1, u0 + span * (done / total))
            };
            let cell = self.proj.cell(a, b, level);
            let Some(cell) = cell else { continue };
            if self.out.len() >= self.max_rows {
                continue; // no room left: only when the first level alone overflows
            }
            let mut rk = it.kind;
            let node = it.node;
            let mut sub: Vec<Item> = Vec::new();
            let mut sub_total = 0.0;
            if rk == kind::FILE && nodes.kind_raw(node as NodeId) == Kind::Dir as u8 {
                let id = node as NodeId;
                if nodes.flags_raw(id) & pending != 0 {
                    rk = kind::PENDING;
                } else {
                    rk = kind::COLLAPSED;
                    if deeper && nodes.child_count(id) > 0 && self.proj.expandable(&cell, level) {
                        self.pairs_of(children(nodes, id, self.n));
                        (sub, sub_total) = self.items(node, it.size, &[], b - a, level + 1, false);
                        if self.out.len() + 1 + self.reserved + sub.len() <= self.max_rows {
                            rk = kind::FOLDER;
                        } else {
                            // its children would not fit in the frame any more
                            let tag = node as u32 & KEY_NODE_MASK;
                            self.ctx.members.remove(&(KEY_SMALL | tag));
                            self.ctx.members.remove(&(KEY_MORE | tag));
                            sub.clear();
                        }
                    }
                }
            }
            let lbl = self.proj.label(&cell, rk);
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
                    geo: cell,
                },
                name,
            );
            if !sub.is_empty() {
                self.reserved += sub.len();
                self.container(&sub, sub_total, a, b, level + 1, row);
            }
        }
    }
}
