//! Subtree queries: every node below a node (whole or in bounded pieces), largest files,
//! per-class totals.
//!
//! A folder's children are one contiguous id range, so a walk only keeps ranges. Every walk is
//! safe while a scan adds listings (see [`Nodes::child_range`]) and never visits more than
//! `len` nodes: it fails with [`StoreError::RunawayWalk`] instead of running away.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::store::Nodes;
use crate::types::*;

/// Ids per piece of a chunked subtree walk ([`SubtreeChunks`]): a summary of a whole volume
/// holds a few MB of temporaries instead of an array of every node (docs/memory.md §5).
pub const SUMMARY_CHUNK: usize = 1 << 18;

/// Concatenates `start..start + count` for every pair.
pub fn ranges_to_ids(starts: &[NodeId], counts: &[u32]) -> Vec<NodeId> {
    let total: usize = counts.iter().map(|&c| c as usize).sum();
    let mut out = Vec::with_capacity(total);
    for (&s, &c) in starts.iter().zip(counts) {
        out.extend(s..s + c);
    }
    out
}

/// Ids of every node strictly below `node`, breadth first.
pub fn subtree_ids(nodes: &Nodes, node: NodeId) -> Result<Vec<NodeId>, StoreError> {
    let mut out = Vec::new();
    for piece in SubtreeChunks::new(nodes, node, usize::MAX) {
        out.extend_from_slice(&piece?);
    }
    Ok(out)
}

/// [`subtree_ids`] in pieces of at most `chunk` ids (every id once, unordered), so a walk over a
/// whole volume never holds an array of every node. The folder ranges still to visit are the
/// only thing that grows (8 B a folder).
pub struct SubtreeChunks<'a> {
    nodes: &'a Nodes,
    root: NodeId,
    /// child ranges still to visit: (next id, end)
    todo: Vec<(NodeId, NodeId)>,
    chunk: usize,
    total: usize,
    failed: bool,
}

impl<'a> SubtreeChunks<'a> {
    pub fn new(nodes: &'a Nodes, node: NodeId, chunk: usize) -> Self {
        let mut todo = Vec::new();
        if node == 0 || nodes.is_dir(node) {
            let (a, b) = nodes.child_range(node);
            if b > a {
                todo.push((a, b));
            }
        }
        SubtreeChunks {
            nodes,
            root: node,
            todo,
            chunk: chunk.max(1),
            total: 0,
            failed: false,
        }
    }
}

impl Iterator for SubtreeChunks<'_> {
    type Item = Result<Vec<NodeId>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let mut ids = Vec::new();
        while ids.len() < self.chunk {
            let Some(&(a, b)) = self.todo.last() else {
                break;
            };
            let take = ((b - a) as usize).min(self.chunk - ids.len()) as NodeId;
            self.todo.pop();
            if a + take < b {
                self.todo.push((a + take, b));
            }
            let mut found = Vec::new();
            for id in a..a + take {
                ids.push(id);
                if self.nodes.is_dir(id) {
                    let (s, e) = self.nodes.child_range(id);
                    if e > s {
                        found.push((s, e));
                    }
                }
            }
            self.todo.extend(found);
        }
        if ids.is_empty() {
            return None;
        }
        self.total += ids.len();
        if self.total > self.nodes.len() {
            // cannot happen with consistent ranges; never let a walk run away
            self.failed = true;
            return Some(Err(StoreError::RunawayWalk(self.root)));
        }
        Some(Ok(ids))
    }
}

/// [`SubtreeChunks`] with [`SUMMARY_CHUNK`].
pub fn subtree_chunks(nodes: &Nodes, node: NodeId) -> SubtreeChunks<'_> {
    SubtreeChunks::new(nodes, node, SUMMARY_CHUNK)
}

/// What [`summarize`] found below a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// node ids, largest first; of equal sizes the smaller id first
    pub top_files: Vec<NodeId>,
    /// allocated bytes per colour class
    pub class_bytes: Vec<i64>,
    pub class_files: Vec<i64>,
    pub latest_mtime: u32,
    /// bytes per (class, class by extension alone) where a folder class took files over,
    /// row-major `n_classes × n_classes` (None: nothing moved, or no `base_cls_of`)
    pub class_parts: Option<Vec<i64>>,
}

/// Largest files, per-class totals and newest modification time below `node`.
///
/// `cls_of` gives the colour class of a file node (`< n_classes`). With `base_cls_of` (the class
/// by extension alone), `class_parts` says what the folder classes took over. A file node
/// summarises itself. Works through the subtree `chunk` ids at a time; the result does not
/// depend on it.
pub fn summarize(
    nodes: &Nodes,
    node: NodeId,
    mut cls_of: impl FnMut(NodeId) -> usize,
    n_classes: usize,
    top: usize,
    mut base_cls_of: Option<&mut dyn FnMut(NodeId) -> usize>,
    chunk: usize,
) -> Result<Summary, StoreError> {
    let mut best: BinaryHeap<Reverse<(i64, Reverse<NodeId>)>> = BinaryHeap::new();
    let mut class_bytes = vec![0i64; n_classes];
    let mut class_files = vec![0i64; n_classes];
    let mut parts: Option<Vec<i64>> = None;
    let mut latest = 0u32;
    let mut visit = |ids: &[NodeId]| {
        for &id in ids {
            latest = latest.max(nodes.mtime(id));
            if nodes.is_dir(id) || nodes.flags(id).intersects(Flags::HARDLINK_DUP) {
                continue;
            }
            let alloc = nodes.alloc(id);
            if top > 0 {
                best.push(Reverse((alloc, Reverse(id))));
                if best.len() > top {
                    best.pop();
                }
            }
            let cls = cls_of(id);
            class_bytes[cls] += alloc;
            class_files[cls] += 1;
            if let Some(base_of) = base_cls_of.as_deref_mut() {
                let base = base_of(id);
                if base != cls {
                    let p = parts.get_or_insert_with(|| vec![0; n_classes * n_classes]);
                    p[cls * n_classes + base] += alloc;
                }
            }
        }
    };
    if nodes.is_dir(node) || node == 0 {
        for piece in SubtreeChunks::new(nodes, node, chunk) {
            visit(&piece?);
        }
    } else {
        visit(&[node]);
    }
    let mut top_files: Vec<(i64, NodeId)> = best
        .into_iter()
        .map(|Reverse((a, Reverse(id)))| (a, id))
        .collect();
    top_files.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
    Ok(Summary {
        top_files: top_files.into_iter().map(|(_, id)| id).collect(),
        class_bytes,
        class_files,
        latest_mtime: latest,
        class_parts: parts,
    })
}
