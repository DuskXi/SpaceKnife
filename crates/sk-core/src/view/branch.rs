//! Top-level branch colours (the `branch` column; Python `view/branch.py`): one palette slot per
//! child of the zoom root.
//!
//! A first-level block keeps the slot it got when it was first drawn, so a branch keeps its hue
//! while a running scan reorders the blocks; every block below it takes its hue. A new branch
//! takes the slot used least by the branches in view that differs from its drawn neighbours'
//! slots (ties: the lower slot). First-level aggregates and free / unknown space are neutral (0).
//! The slots belong to one zoom root and are forgotten when it changes; at most [`LIMIT`] are
//! remembered, the least recently drawn going first.

use std::collections::{HashMap, VecDeque};

use sk_store::NodeId;

use crate::layout::rows::{DrawList, KEY_NODE_MASK, kind};

/// visual.branch.1 … visual.branch.8
pub const SLOTS: u8 = 8;
/// Branches remembered per view.
pub const LIMIT: usize = 4096;

/// The branch slots of one view, stable across its frames.
#[derive(Debug, Default, Clone)]
pub struct BranchHues {
    /// zoom key the slots belong to
    root: Option<u32>,
    slots: HashMap<u32, u8>,
    /// first-level keys, least recently drawn first (may hold stale duplicates)
    order: VecDeque<(u32, u64)>,
    stamp: HashMap<u32, u64>,
    tick: u64,
}

impl BranchHues {
    pub fn new() -> BranchHues {
        BranchHues::default()
    }

    /// First-level key -> slot of the branches remembered.
    pub fn slots(&self) -> HashMap<u32, u8> {
        self.slots.clone()
    }

    pub fn clear(&mut self) {
        self.root = None;
        self.slots.clear();
        self.order.clear();
        self.stamp.clear();
    }

    fn touch(&mut self, key: u32) {
        self.tick += 1;
        self.stamp.insert(key, self.tick);
        self.order.push_back((key, self.tick));
    }

    fn trim(&mut self) {
        while self.slots.len() > LIMIT {
            let Some((k, t)) = self.order.pop_front() else {
                break;
            };
            if self.stamp.get(&k) == Some(&t) {
                self.stamp.remove(&k);
                self.slots.remove(&k);
            }
        }
        if self.order.len() > 4 * LIMIT {
            let stamp = &self.stamp;
            self.order.retain(|(k, t)| stamp.get(k) == Some(t));
        }
    }

    /// Slot per row of `draw` (0 neutral), laid out below the zoom root keyed `root`; written
    /// into `out` (cleared first).
    pub fn hues_into(&mut self, root: u32, draw: &DrawList, out: &mut Vec<u8>) {
        if self.root != Some(root) {
            self.clear();
            self.root = Some(root);
        }
        out.clear();
        out.resize(draw.len(), 0);
        if draw.is_empty() {
            return;
        }
        let top: Vec<usize> = (0..draw.len())
            .filter(|&i| draw.parent[i] < 0 && draw.kind[i] <= kind::PENDING)
            .collect();
        let keys: Vec<u32> = top.iter().map(|&i| draw.key[i]).collect();
        let mut slots: Vec<u8> = vec![0; keys.len()];
        for (i, &k) in keys.iter().enumerate() {
            if let Some(&s) = self.slots.get(&k) {
                self.touch(k);
                slots[i] = s;
            }
        }
        let mut used = [0usize; SLOTS as usize + 1];
        for &s in &slots {
            used[s as usize] += 1;
        }
        let n = keys.len();
        for i in 0..n {
            if slots[i] != 0 {
                continue;
            }
            // neighbours on both sides; the list is a ring (the polar view's first and last
            // sectors meet at 12 o'clock), so the ends are neighbours of each other too
            let before = if n > 2 || i > 0 {
                slots[(i + n - 1) % n]
            } else {
                0
            };
            let after = if n > 2 || i + 1 < n {
                slots[(i + 1) % n]
            } else {
                0
            };
            let s = (1..=SLOTS)
                .filter(|&s| s != before && s != after)
                .min_by_key(|&s| (used[s as usize], s))
                .unwrap_or(1);
            slots[i] = s;
            used[s as usize] += 1;
            self.slots.insert(keys[i], s);
            self.touch(keys[i]);
        }
        self.trim();
        for (&row, &s) in top.iter().zip(&slots) {
            out[row] = s;
        }
        // parents come first: every row below the top takes its parent's slot
        for i in 0..draw.len() {
            let p = draw.parent[i];
            if p >= 0 && draw.depth[i] >= 2 {
                out[i] = out[p as usize];
            }
        }
    }

    /// [`BranchHues::hues_into`] into a new vector.
    pub fn hues(&mut self, root: u32, draw: &DrawList) -> Vec<u8> {
        let mut out = Vec::new();
        self.hues_into(root, draw, &mut out);
        out
    }

    /// Node ids changed: the slots follow their branches, gone ones go.
    pub fn remap(&mut self, new_id: &dyn Fn(NodeId) -> Option<NodeId>) {
        let Some(root) = self.root else { return };
        let (tag, node) = (root & !KEY_NODE_MASK, root & KEY_NODE_MASK);
        let Some(moved) = new_id(node) else {
            self.clear();
            return;
        };
        self.root = Some(tag | moved);
        let mut by_stamp: Vec<(u64, u32, u8)> = self
            .slots
            .iter()
            .map(|(&k, &s)| (self.stamp.get(&k).copied().unwrap_or(0), k, s))
            .collect();
        by_stamp.sort_unstable();
        self.slots.clear();
        self.stamp.clear();
        self.order.clear();
        for (_, k, s) in by_stamp {
            if let Some(m) = new_id(k) {
                self.slots.insert(m, s);
                self.touch(m);
            }
        }
    }
}
