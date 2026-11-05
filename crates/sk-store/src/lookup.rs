//! Path → node for change events: per-folder name maps, built when a folder is first asked
//! about.
//!
//! [`Nodes::find`] compares names one by one at every level — fine for a single lookup, slow for
//! a folder of 100,000 entries that sees a change per second. Here each folder visited gets a
//! name map, valid while the folder keeps the same child range (a relist gives it a new one). At
//! most [`MAX_FOLDERS`] maps are kept (least recently used out first). Call
//! [`PathIndex::forget`] when the store is renumbered (compaction, adoption).
//!
//! A name is looked up in its exact case first, and only then case-insensitively
//! ([`normcase`]), and then only when one entry has it: a change in `A` must not land in `a`.

use std::collections::{HashMap, HashSet};

use crate::names::{is_sep, normcase, split_below};
use crate::store::Nodes;
use crate::types::*;

pub const MAX_FOLDERS: usize = 512;

struct FolderNames {
    span: (NodeId, NodeId),
    exact: HashMap<Vec<u8>, NodeId>,
    /// normcase(name) -> child, or None when several have it
    folded: HashMap<Vec<u8>, Option<NodeId>>,
    /// names more than one child has, exactly
    twice: HashSet<Vec<u8>>,
    used: u64,
}

/// Per-store path lookup cache. Not shared between threads; the caller passes the view.
#[derive(Default)]
pub struct PathIndex {
    maps: HashMap<NodeId, FolderNames>,
    clock: u64,
}

impl PathIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// `(node, deepest)`: the node at `path` (None if the tree has no such entry) and the
    /// deepest node on the way there that does exist (None when `path` is outside every root).
    pub fn locate(&mut self, nodes: &Nodes, path: &[u8]) -> (Option<NodeId>, Option<NodeId>) {
        for root in nodes.roots() {
            if nodes.flags(root).intersects(Flags::GONE) {
                continue;
            }
            let Some(parts) = split_below(path, nodes.name_bytes(root)) else {
                continue;
            };
            let mut node = root;
            for part in &parts {
                if !nodes.is_dir(node) {
                    return (None, Some(node));
                }
                match self.child(nodes, node, part) {
                    Some(c) => node = c,
                    None => return (None, Some(node)),
                }
            }
            return (Some(node), Some(node));
        }
        (None, None)
    }

    /// Whether no path names `node` alone: it, or a folder above it below its scan root, has a
    /// name its folder holds more than once, exactly, or a name with a path separator. Such a
    /// path opens one of them, or something else.
    pub fn unnamed(&mut self, nodes: &Nodes, node: NodeId) -> bool {
        let mut node = node;
        while nodes.depth(node) > 1 {
            let folder = nodes.parent(node) as NodeId;
            let name = nodes.name_bytes(node);
            if name.iter().any(|&b| is_sep(b)) || self.names(nodes, folder).twice.contains(name) {
                return true;
            }
            node = folder;
        }
        false
    }

    fn child(&mut self, nodes: &Nodes, folder: NodeId, name: &[u8]) -> Option<NodeId> {
        let names = self.names(nodes, folder);
        if let Some(&c) = names.exact.get(name) {
            return Some(c);
        }
        names.folded.get(normcase(name).as_ref()).copied().flatten()
    }

    fn names(&mut self, nodes: &Nodes, folder: NodeId) -> &FolderNames {
        let span = nodes.child_range(folder);
        self.clock += 1;
        let clock = self.clock;
        let fresh = self.maps.get(&folder).is_none_or(|m| m.span != span);
        if fresh {
            let mut exact = HashMap::new();
            let mut folded: HashMap<Vec<u8>, Option<NodeId>> = HashMap::new();
            let mut twice = HashSet::new();
            for i in span.0..span.1 {
                let own = nodes.name_bytes(i);
                if exact.contains_key(own) {
                    twice.insert(own.to_vec());
                } else {
                    exact.insert(own.to_vec(), i);
                }
                folded
                    .entry(normcase(own).into_owned())
                    .and_modify(|v| *v = None)
                    .or_insert(Some(i));
            }
            self.maps.insert(
                folder,
                FolderNames {
                    span,
                    exact,
                    folded,
                    twice,
                    used: clock,
                },
            );
            if self.maps.len() > MAX_FOLDERS
                && let Some((&old, _)) = self.maps.iter().min_by_key(|(_, m)| m.used)
            {
                self.maps.remove(&old);
            }
        }
        let m = self.maps.get_mut(&folder).expect("just made");
        m.used = clock;
        m
    }

    /// Drops every map (the store was renumbered).
    pub fn forget(&mut self) {
        self.maps.clear();
    }

    /// Number of folder maps held.
    pub fn len(&self) -> usize {
        self.maps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }
}
