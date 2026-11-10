//! Colour tags (red / yellow / green / blue), persisted per set of scan roots (Python
//! `view/tags.py`).
//!
//! Tags are keyed by the path relative to its scan root, so they survive rescans and snapshots.
//! A folder's tag applies to everything below it (inheritance is evaluated by the filter engine);
//! the marker is only drawn on the element that carries the tag.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sk_protocol::filter::{TAG_COLORS, TagColor};
use sk_store::{NodeId, Nodes};

use crate::filter::tag_bit;

/// Own tag of a node as the draw lists carry it: 0 none, 1–4 red, yellow, green, blue.
pub fn tag_index(c: TagColor) -> u8 {
    TAG_COLORS
        .iter()
        .position(|&t| t == c)
        .map_or(0, |i| i as u8 + 1)
}

/// The colour of a draw-list tag index (1–4).
pub fn tag_of_index(i: u8) -> Option<TagColor> {
    TAG_COLORS.get((i as usize).checked_sub(1)?).copied()
}

#[derive(Serialize, Deserialize)]
struct FileItem {
    root: usize,
    path: String,
    color: String,
}

#[derive(Serialize, Deserialize)]
struct FileData {
    #[serde(default)]
    version: i64,
    #[serde(default)]
    tags: Vec<FileItem>,
}

/// (tag version, store version, node count) a cache was made for.
type Key = (u64, u64, usize);

/// The tags of one set of scan roots.
#[derive(Debug, Default)]
pub struct TagStore {
    pub file: Option<PathBuf>,
    /// (root index, relative path) -> colour
    pub tags: BTreeMap<(usize, String), TagColor>,
    /// bumped on every change
    pub version: u64,
    /// (version, store version, node count) -> node -> colour
    resolved: Option<(Key, Arc<HashMap<NodeId, TagColor>>)>,
    bits: Option<(Key, Arc<[u8]>)>,
}

impl TagStore {
    /// Tags kept in `file` (loaded when it exists; None: in memory only).
    pub fn new(file: Option<PathBuf>) -> TagStore {
        let mut t = TagStore {
            file,
            ..TagStore::default()
        };
        if let Some(f) = t.file.clone()
            && f.exists()
        {
            t.load(&f);
        }
        t
    }

    fn load(&mut self, file: &Path) {
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        let Ok(data) = serde_json::from_str::<FileData>(&text) else {
            self.tags.clear();
            return;
        };
        for item in data.tags {
            if let Some(c) = TagColor::from_str(&item.color) {
                self.tags.insert((item.root, item.path), c);
            }
        }
    }

    /// Write the tags to the file (atomically: a temporary file renamed over it).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let data = FileData {
            version: 1,
            tags: self
                .tags
                .iter()
                .map(|((r, p), c)| FileItem {
                    root: *r,
                    path: p.clone(),
                    color: c.as_str().to_owned(),
                })
                .collect(),
        };
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&data)?)?;
        std::fs::rename(&tmp, file)
    }

    /// Drop every tag in memory (the saved file is removed by the caller).
    pub fn forget(&mut self) {
        self.tags.clear();
        self.version += 1;
        self.resolved = None;
        self.bits = None;
    }

    /// (index of the scan root, path below it joined with `/`) of a node; None for node 0.
    pub fn address(nodes: &Nodes, node: NodeId) -> Option<(usize, String)> {
        let roots: Vec<NodeId> = nodes.roots().collect();
        let mut parts: Vec<String> = Vec::new();
        let mut cur = node as i64;
        let mut steps = 0;
        while cur > 0 && !roots.contains(&(cur as NodeId)) {
            parts.push(nodes.name(cur as NodeId).into_owned());
            cur = nodes.parent(cur as NodeId) as i64;
            steps += 1;
            if steps > nodes.len() {
                return None;
            }
        }
        if cur <= 0 {
            return None;
        }
        let r = roots.iter().position(|&x| x == cur as NodeId)?;
        parts.reverse();
        Some((r, parts.join("/")))
    }

    fn key(&self, nodes: &Nodes) -> Key {
        (self.version, nodes.version(), nodes.len())
    }

    /// Node -> colour of every tag that resolves in `nodes`.
    pub fn resolve(&mut self, nodes: &Nodes) -> Arc<HashMap<NodeId, TagColor>> {
        let key = self.key(nodes);
        if let Some((k, m)) = &self.resolved
            && *k == key
        {
            return m.clone();
        }
        let roots: Vec<NodeId> = nodes.roots().collect();
        let mut out = HashMap::new();
        for ((r, rel), &color) in &self.tags {
            let Some(&root) = roots.get(*r) else { continue };
            let mut node = Some(root);
            for part in rel.split('/').filter(|p| !p.is_empty()) {
                let Some(at) = node else { break };
                node = nodes
                    .children(at)
                    .find(|&ch| nodes.name_bytes(ch) == part.as_bytes());
            }
            if let Some(nd) = node {
                out.insert(nd, color);
            }
        }
        let m = Arc::new(out);
        self.resolved = Some((key, m.clone()));
        m
    }

    pub fn of(&mut self, nodes: &Nodes, node: NodeId) -> Option<TagColor> {
        self.resolve(nodes).get(&node).copied()
    }

    /// Own tag bits per node ([`tag_bit`]), for the filter engine (cached).
    pub fn bits(&mut self, nodes: &Nodes) -> Arc<[u8]> {
        let key = self.key(nodes);
        if let Some((k, b)) = &self.bits
            && *k == key
        {
            return b.clone();
        }
        let mut out = vec![0u8; nodes.len()];
        for (&node, &c) in self.resolve(nodes).iter() {
            if let Some(v) = out.get_mut(node as usize) {
                *v = tag_bit(c);
            }
        }
        let b: Arc<[u8]> = out.into();
        self.bits = Some((key, b.clone()));
        b
    }

    /// Set (or with None remove) the tag of a node. Returns whether anything changed.
    pub fn set(&mut self, nodes: &Nodes, node: NodeId, color: Option<TagColor>) -> bool {
        let Some(addr) = Self::address(nodes, node) else {
            return false;
        };
        let changed = match color {
            None => self.tags.remove(&addr).is_some(),
            Some(c) => self.tags.insert(addr, c) != Some(c),
        };
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Remove the tag of `node` and of everything below it (Ctrl+0). Returns how many.
    pub fn clear_subtree(&mut self, nodes: &Nodes, node: NodeId) -> usize {
        let Some((root, rel)) = Self::address(nodes, node) else {
            return 0;
        };
        let prefix = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };
        let drop: Vec<(usize, String)> = self
            .tags
            .keys()
            .filter(|(r, p)| *r == root && (*p == rel || p.starts_with(&prefix)))
            .cloned()
            .collect();
        for k in &drop {
            self.tags.remove(k);
        }
        if !drop.is_empty() {
            self.version += 1;
        }
        drop.len()
    }
}
