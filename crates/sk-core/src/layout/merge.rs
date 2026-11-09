//! Which blocks a container shows: the small-item and "+N items" merges (G5) and aggregate zoom
//! pages (Python `layout/merge.py`). Shared by every layout engine; each says, through the byte
//! threshold, what "too small" means in its own geometry.

use std::collections::HashMap;

use sk_protocol::filter_syntax::casefold;
use sk_store::{Kind, NodeId, Nodes};

use super::basis::Classes;
use super::rows::{KEY_FREE, KEY_MORE, KEY_NODE_MASK, KEY_SMALL, NO_CLASS, PAGE_REST, kind};

/// One block of a container, in drawing order, before it is placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    pub key: u32,
    pub node: i32,
    pub kind: u8,
    pub size: i64,
    pub cls: u8,
    pub count: u32,
    /// area basis when it is not `size` (a shrunk "+N items"); 0: `size`
    pub weight: f64,
}

impl Item {
    #[inline]
    pub fn basis(&self) -> f64 {
        if self.weight != 0.0 {
            self.weight
        } else {
            self.size as f64
        }
    }
}

/// One layout run's merge state: the member blocks found so far, and scratch buffers.
pub struct MergeCtx<'a> {
    pub nodes: &'a Nodes,
    pub classes: &'a Classes,
    pub sort_by_size: bool,
    /// SMALL/MORE key -> node ids (DrawList::members)
    pub members: HashMap<u32, Vec<NodeId>>,
    /// aggregate zoom: key of the block zoomed into
    pub avoid: Option<u32>,
    by_cls: Vec<f64>,
}

impl<'a> MergeCtx<'a> {
    pub fn new(
        nodes: &'a Nodes,
        classes: &'a Classes,
        sort_by_size: bool,
        members: HashMap<u32, Vec<NodeId>>,
    ) -> Self {
        MergeCtx {
            nodes,
            classes,
            sort_by_size,
            members,
            avoid: None,
            by_cls: Vec::new(),
        }
    }

    fn sort_by_name(&self, pairs: &mut [(NodeId, i64)]) {
        let mut keyed: Vec<(String, usize)> = pairs
            .iter()
            .enumerate()
            .map(|(i, &(id, _))| (casefold(&self.nodes.name(id)), i))
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        let sorted: Vec<(NodeId, i64)> = keyed.iter().map(|&(_, i)| pairs[i]).collect();
        pairs.copy_from_slice(&sorted);
    }

    /// Ordered items of one container after the small-item and "+N" merges, appended to `out`.
    ///
    /// `pairs` holds the container's nodes with their sizes (it is reordered and filtered);
    /// `extra` the free / unknown space blocks as (key, bytes); `thr_of(total)` the byte size below
    /// which an item is too small, for the container's total.
    pub fn merge_items(
        &mut self,
        container: i32,
        pairs: &mut Vec<(NodeId, i64)>,
        extra: &[(u32, i64)],
        thr_of: impl Fn(f64) -> f64,
        out: &mut Vec<Item>,
    ) {
        pairs.retain(|&(_, s)| s > 0);
        let s_sum: i64 = pairs.iter().map(|p| p.1).sum();
        let extra_sum: i64 = extra.iter().map(|e| e.1).sum();
        let total = s_sum as f64 + extra_sum as f64;
        if total <= 0.0 {
            return;
        }
        let thr = thr_of(total);
        let nodes = self.nodes;
        let is_file = |id: NodeId| nodes.kind_raw(id) != Kind::Dir as u8;
        let tag = container as u32 & KEY_NODE_MASK;
        let n_small = pairs
            .iter()
            .filter(|&&(id, s)| (s as f64) < thr && is_file(id))
            .count();
        let small_taken = n_small >= 2;
        let mut agg: Option<Item> = None;
        if small_taken {
            let n_cls = self.classes.len();
            self.by_cls.clear();
            self.by_cls.resize(n_cls.max(1), 0.0);
            let mut sm_ids = Vec::with_capacity(n_small);
            let mut sm_total = 0i64;
            for &(id, s) in pairs.iter() {
                if (s as f64) < thr && is_file(id) {
                    sm_ids.push(id);
                    let c = self.classes.cls_of(nodes, id) as usize;
                    if c >= self.by_cls.len() {
                        self.by_cls.resize(c + 1, 0.0);
                    }
                    self.by_cls[c] += s as f64;
                    sm_total += s;
                }
            }
            let mut top = 0;
            for (i, &v) in self.by_cls.iter().enumerate() {
                if v > self.by_cls[top] {
                    top = i;
                }
            }
            let cls = if self.by_cls[top] * 2.0 >= sm_total as f64 {
                top as u8
            } else {
                NO_CLASS
            };
            agg = Some(Item {
                key: KEY_SMALL | tag,
                node: container,
                kind: kind::SMALL,
                size: sm_total,
                cls,
                count: n_small as u32,
                weight: 0.0,
            });
            self.members.insert(KEY_SMALL | tag, sm_ids);
        }
        // "+N items": everything still below the threshold (folders, a lone small file, the aggregate)
        let tiny = |id: NodeId, s: i64| (s as f64) < thr && !(small_taken && is_file(id));
        let agg_tiny = agg.is_some_and(|a| (a.size as f64) < thr);
        let n_tiny = pairs.iter().filter(|&&(id, s)| tiny(id, s)).count() + agg_tiny as usize;
        let mut more: Option<Item> = None;
        if n_tiny >= 2 {
            let mut t_ids: Vec<NodeId> = Vec::with_capacity(n_tiny);
            let mut size = 0i64;
            for &(id, s) in pairs.iter() {
                if tiny(id, s) {
                    t_ids.push(id);
                    size += s;
                }
            }
            let mut count = t_ids.len() as u32;
            if let Some(a) = agg
                && agg_tiny
            {
                if let Some(m) = self.members.remove(&(KEY_SMALL | tag)) {
                    t_ids.extend(m);
                }
                size += a.size;
                count += a.count;
                agg = None;
            }
            self.members.insert(KEY_MORE | tag, t_ids);
            more = Some(Item {
                key: KEY_MORE | tag,
                node: container,
                kind: kind::MORE,
                size,
                cls: NO_CLASS,
                count,
                weight: 0.0,
            });
        }
        let drop_tiny = more.is_some();
        pairs.retain(|&(id, s)| {
            let below = (s as f64) < thr;
            let small = small_taken && below && is_file(id);
            !small && !(drop_tiny && below)
        });
        if self.sort_by_size {
            pairs.sort_by_key(|p| std::cmp::Reverse(p.1));
        } else {
            self.sort_by_name(pairs);
        }
        let start = out.len();
        for &(id, s) in pairs.iter() {
            out.push(Item {
                key: id,
                node: id as i32,
                kind: kind::FILE,
                size: s,
                cls: self.classes.cls_of(nodes, id),
                count: 1,
                weight: 0.0,
            });
        }
        for &(key, size) in extra {
            out.push(Item {
                key,
                node: -1,
                kind: if key == KEY_FREE {
                    kind::FREE
                } else {
                    kind::UNKNOWN
                },
                size,
                cls: NO_CLASS,
                count: 0,
                weight: 0.0,
            });
        }
        if let Some(a) = agg {
            out.push(a);
        }
        if self.sort_by_size && (!extra.is_empty() || agg.is_some()) {
            out[start..].sort_by_key(|it| std::cmp::Reverse(it.size)); // stable: equal sizes keep their order
        }
        if let Some(m) = more {
            out.push(m);
        }
    }

    /// Items of an aggregate zoom: the largest members one by one, the rest as one "+N items",
    /// appended to `out`.
    ///
    /// `need` is the share of the view one member needs to be drawn on its own (the minimum
    /// element size). Members are drawn on their own while each still gets it. When the
    /// leftover's share of the view is what keeps them merged, that block is shrunk to
    /// [`PAGE_REST`] of the view, so at least the largest member always gets through and zooming
    /// into the leftover again shows the next members.
    pub fn page_items(
        &mut self,
        container: i32,
        pairs: &mut Vec<(NodeId, i64)>,
        need: f64,
        out: &mut Vec<Item>,
    ) {
        pairs.retain(|&(_, s)| s > 0);
        let n = pairs.len();
        if n == 0 {
            return;
        }
        // largest first, ties by id: a stable representative
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let total: f64 = pairs.iter().map(|p| p.1 as f64).sum();
        let mut k_prop = n;
        let mut k_shrunk = n;
        let mut prefix = 0.0f64;
        let mut prefixes = Vec::with_capacity(n);
        for (i, &(_, s)) in pairs.iter().enumerate() {
            let sf = s as f64;
            prefix += sf;
            prefixes.push(prefix);
            if k_prop == n && sf < need * total {
                k_prop = i;
            }
            let basis = f64::min(total, prefix / (1.0 - PAGE_REST));
            if k_shrunk == n && sf < need * basis {
                k_shrunk = i;
            }
        }
        let mut k = k_prop.max(k_shrunk).max(1);
        if n - k == 1 {
            k = n; // merging needs at least two members
        }
        let (page, rest) = pairs.split_at_mut(k);
        if !self.sort_by_size {
            self.sort_by_name(page);
        }
        let nodes = self.nodes;
        for &(id, s) in page.iter() {
            out.push(Item {
                key: id,
                node: id as i32,
                kind: kind::FILE,
                size: s,
                cls: self.classes.cls_of(nodes, id),
                count: 1,
                weight: 0.0,
            });
        }
        if k < n {
            let mut key = KEY_MORE | (rest[0].0 & KEY_NODE_MASK);
            if Some(key) == self.avoid {
                // sizes changed since that zoom: the next member represents it
                key = KEY_MORE | (rest[1].0 & KEY_NODE_MASK);
            }
            let basis_k = f64::min(total, prefixes[k - 1] / (1.0 - PAGE_REST));
            let weight = (if k == k_prop { total } else { basis_k }) - prefixes[k - 1];
            let size: i64 = rest.iter().map(|p| p.1).sum();
            self.members.insert(key, rest.iter().map(|p| p.0).collect());
            out.push(Item {
                key,
                node: container,
                kind: kind::MORE,
                size,
                cls: NO_CLASS,
                count: rest.len() as u32,
                weight,
            });
        }
    }
}
