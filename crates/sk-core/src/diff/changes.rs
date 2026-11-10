//! The change tree (port of the pure parts of `server/compares.py`): only what changed, as a
//! store whose sizes are the changes, plus short labels and the protocol's change items.

use std::path::MAIN_SEPARATOR;

use sk_protocol::compare::{ChangeItem, ChangeKind, ChangeStatus};
use sk_store::{Flags, Kind, NodeId, Nodes, Store, StoreBuilder, StoreError, StoreOptions};

use super::engine::{Change, DiffTree, Status};
use super::text::text;

const DIR: u8 = Kind::Dir as u8;
const CLEAR: u32 = Flags::PENDING.0 | Flags::HARDLINK_DUP.0 | Flags::GONE.0;

/// What changed between two trees; one entry per row of its store in every array.
///
/// The store's sizes are the changes: `alloc` / `size` of a file are `|Δ|` of its allocated /
/// logical bytes, a folder's totals the sums below it, so the treemap area is the change. The
/// rows are also a [`DiffTree`] ([`ChangeTree::diff`]: change units, both ends).
#[derive(Debug)]
pub struct ChangeTree {
    /// the kept rows (`diff.store` is the change store)
    pub diff: DiffTree,
    /// new files below (a file: 1 when it is new)
    pub new_files: Vec<i32>,
}

impl ChangeTree {
    /// Keep the rows that changed (and their folders, which then changed too) plus the roots.
    pub fn of(diff: &DiffTree, opts: StoreOptions) -> Result<ChangeTree, StoreError> {
        let u = diff.store.read();
        let n = u.len();
        let mut keep: Vec<bool> = (0..n).map(|i| diff.grow[i] + diff.shrink[i] > 0).collect();
        keep[0] = true;
        for r in u.roots() {
            keep[r as usize] = true;
        }
        let ids: Vec<u32> = (0..n as u32).filter(|&i| keep[i as usize]).collect();
        let m = ids.len();
        let mut new_of = vec![-1i32; n];
        for (k, &i) in ids.iter().enumerate() {
            new_of[i as usize] = k as i32;
        }
        let image_len: usize = ids.iter().map(|&i| u.name_len(i) as usize + 1).sum();
        let mut builder = StoreBuilder::new(m, image_len, opts);
        builder.set_ext_table(u.ext_table().iter().map(|e| e.to_string()).collect());
        let area = |i: u32| diff.grow[i as usize] + diff.shrink[i as usize];
        {
            let c = builder.columns();
            for (k, &i) in ids.iter().enumerate() {
                let p = u.parent(i);
                c.parent[k] = if p >= 0 { new_of[p as usize] } else { -1 };
                c.depth[k] = u.depth(i);
                c.kind[k] = u.kind_raw(i);
                c.flags[k] = u.flags_raw(i) & !CLEAR;
                c.name_len[k] = u.name_len(i);
                c.ext[k] = u.ext(i);
                c.mtime[k] = u.mtime(i);
                c.ctime[k] = u.ctime(i);
                c.atime[k] = u.atime(i);
                c.ino[k] = u.ino(i);
                c.nlink[k] = u.nlink(i);
            }
            // the union is breadth first and grouped by parent in parent order: each folder's
            // kept children are one run
            c.child_start.fill(0);
            c.child_count.fill(0);
            for k in 1..m {
                let p = c.parent[k] as usize;
                if c.child_count[p] == 0 {
                    c.child_start[p] = k as i32;
                }
                c.child_count[p] += 1;
            }
            // a folder's own blocks (POSIX directories hold some) changed by its area less the
            // children's areas
            let mut child_area = vec![0i64; m];
            for k in 1..m {
                child_area[c.parent[k] as usize] += area(ids[k]);
            }
            for (k, &i) in ids.iter().enumerate() {
                let iu = i as usize;
                if c.kind[k] == DIR {
                    c.alloc[k] = (area(i) - child_area[k]).max(0);
                    c.size[k] = 0;
                } else {
                    c.alloc[k] = diff.delta[iu].abs();
                    c.size[k] = (diff.target_size[iu] - diff.base_size[iu]).abs();
                }
            }
            c.tot_size.fill(0);
            c.tot_alloc.fill(0);
            c.tot_files.fill(0);
            c.tot_dirs.fill(0);
            for k in (1..m).rev() {
                let p = c.parent[k] as usize;
                c.tot_size[p] += c.tot_size[k] + c.size[k];
                c.tot_alloc[p] += c.tot_alloc[k] + c.alloc[k];
                c.tot_files[p] += c.tot_files[k] + (c.kind[k] == Kind::File as u8) as i32;
                c.tot_dirs[p] += c.tot_dirs[k] + (c.kind[k] == DIR) as i32;
            }
        }
        {
            let image = builder.image();
            let mut at = 0;
            for &i in &ids {
                let name = u.name_bytes(i);
                image[at] = 0;
                image[at + 1..at + 1 + name.len()].copy_from_slice(name);
                at += 1 + name.len();
            }
        }
        let store = builder.finish()?;
        let pick_i64 = |v: &[i64]| -> Vec<i64> { ids.iter().map(|&i| v[i as usize]).collect() };
        let pick_i32 = |v: &[i32]| -> Vec<i32> { ids.iter().map(|&i| v[i as usize]).collect() };
        let pruned = DiffTree {
            store: store.clone(),
            base_id: pick_i32(&diff.base_id),
            target_id: pick_i32(&diff.target_id),
            base_alloc: pick_i64(&diff.base_alloc),
            target_alloc: pick_i64(&diff.target_alloc),
            base_size: pick_i64(&diff.base_size),
            target_size: pick_i64(&diff.target_size),
            delta: pick_i64(&diff.delta),
            status: ids.iter().map(|&i| diff.status[i as usize]).collect(),
            grow: pick_i64(&diff.grow),
            shrink: pick_i64(&diff.shrink),
            fold_case: diff.fold_case,
        };
        drop(u);
        let nodes = store.read();
        let new_leaf: Vec<i64> = (0..m)
            .map(|k| (pruned.status[k] == Status::Added && !nodes.is_dir(k as NodeId)) as i64)
            .collect();
        let new_files = subtree_sums(&nodes, &new_leaf)
            .into_iter()
            .map(|v| v as i32)
            .collect();
        drop(nodes);
        Ok(ChangeTree {
            diff: pruned,
            new_files,
        })
    }

    pub fn store(&self) -> &Store {
        &self.diff.store
    }

    pub fn len(&self) -> usize {
        self.diff.len()
    }

    pub fn is_empty(&self) -> bool {
        self.diff.is_empty()
    }

    pub fn memory_bytes(&self) -> usize {
        self.diff.memory_bytes() + self.new_files.len() * 4
    }

    /// Area basis per row: growth, shrinkage, or both (`None`: the change store's own sizes);
    /// with `matched` (a filter's matches per row), only the matching files, summed up.
    pub fn amounts(&self, grow: bool, shrink: bool, matched: Option<&[bool]>) -> Vec<i64> {
        let d = &self.diff;
        let nodes = d.store.read();
        let n = nodes.len();
        let Some(matched) = matched else {
            return match (grow, shrink) {
                (true, false) => d.grow.clone(),
                (false, true) => d.shrink.clone(),
                _ => (0..n as NodeId)
                    .map(|i| nodes.effective_size(i).1)
                    .collect(),
            };
        };
        let leaf: Vec<i64> = (0..n)
            .map(|i| {
                let sel = matched.get(i).copied().unwrap_or(false) && !nodes.is_dir(i as NodeId);
                if !sel {
                    return 0;
                }
                (if grow { d.grow[i] } else { 0 }) + (if shrink { d.shrink[i] } else { 0 })
            })
            .collect();
        subtree_sums(&nodes, &leaf)
    }
}

/// Per-row values summed over every subtree, a row's own included. Relies on parents having
/// lower ids than their children (true of compare and change trees; a scanned tree after a
/// relist is not, compact it first).
pub fn subtree_sums(nodes: &Nodes, leaf: &[i64]) -> Vec<i64> {
    let n = nodes.len();
    let mut sub: Vec<i64> = (0..n).map(|i| leaf.get(i).copied().unwrap_or(0)).collect();
    let parent = nodes.parents();
    let ascending = (1..n).all(|i| (parent[i] as i64) < i as i64);
    if ascending {
        for i in (1..n).rev() {
            if parent[i] >= 0 {
                sub[parent[i] as usize] += sub[i];
            }
        }
        return sub;
    }
    // any order: by depth, deepest first
    let depth = nodes.depths();
    let mut order: Vec<usize> = (1..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(depth[i]));
    for i in order {
        if parent[i] >= 0 {
            sub[parent[i] as usize] += sub[i];
        }
    }
    sub
}

// --------------------------------------------------------------------------- short labels (W6-32)

const GENERIC: &[&str] = &[
    "data",
    "cache",
    "caches",
    "download",
    "downloads",
    "video",
    "videos",
    "file",
    "files",
    "image",
    "images",
    "temp",
    "tmp",
    "bin",
    "lib",
    "lib64",
    "src",
    "local",
    "roaming",
    "appdata",
    "default",
    "user",
    "users",
    "storage",
    "filestorage",
    "share",
    "shared",
    "misc",
    "store",
    "backup",
    "backups",
    "content",
    "contents",
    "resources",
    "assets",
    "logs",
    "log",
    "db",
    "database",
    "packages",
    "package",
    "modules",
    "node_modules",
    "build",
    "dist",
    "out",
    "output",
    "target",
    "release",
    "debug",
    "x64",
    "x86",
    "win64",
    "amd64",
    "common",
    "program",
    "programs",
    "app",
    "apps",
];

/// An id-like name: letters and digits (both) of 8+ characters, or hex of 12+.
fn id_like(name: &str) -> bool {
    let n = name.chars().count();
    let word = |c: char| c.is_ascii_alphanumeric() || "_-.{}".contains(c);
    if n >= 8
        && name.chars().all(word)
        && name.chars().any(|c| c.is_ascii_digit())
        && name.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    n >= 12
        && name
            .chars()
            .all(|c| c.is_ascii_hexdigit() || "-{}".contains(c))
}

fn generic(name: &str) -> bool {
    GENERIC.contains(&name.to_lowercase().as_str()) || id_like(name)
}

/// A file: its name. A folder with a generic name: the nearest distinctive folder above it in
/// front — 「SoftwareDistribution\Download」, 「WeChat Files\…\Video」 (UI-W6-32).
pub fn short_label(nodes: &Nodes, node: NodeId, scope: NodeId) -> String {
    let name = nodes.name_printable(node).into_owned();
    if !nodes.is_dir(node) || !generic(&name) {
        return name;
    }
    let mut p = nodes.parent(node);
    let mut skipped = false;
    // never the root itself (a whole path)
    while p > 0 && p as NodeId != scope && nodes.parent(p as NodeId) > 0 {
        let pn = nodes.name_printable(p as NodeId);
        if !generic(&pn) {
            let mid = if skipped {
                format!("…{MAIN_SEPARATOR}")
            } else {
                String::new()
            };
            return format!("{pn}{MAIN_SEPARATOR}{mid}{name}");
        }
        skipped = true;
        p = nodes.parent(p as NodeId);
    }
    name
}

// --------------------------------------------------------------------------- change items

pub fn change_kind(kind: Kind) -> ChangeKind {
    match kind {
        Kind::File => ChangeKind::File,
        Kind::Dir => ChangeKind::Dir,
        Kind::Link => ChangeKind::Link,
        Kind::Other => ChangeKind::Other,
    }
}

pub fn change_status(status: Status) -> ChangeStatus {
    match status {
        Status::Unchanged => ChangeStatus::Same,
        Status::Added => ChangeStatus::New,
        Status::Removed => ChangeStatus::Deleted,
        Status::Changed => ChangeStatus::Changed,
    }
}

/// The change item of a row of a change tree. `reason`: the insight note (insights rules),
/// `on_disk`: the target is the live tree and the row was not deleted.
pub fn change_item(
    tree: &ChangeTree,
    node: NodeId,
    scope: NodeId,
    reason: String,
    on_disk: bool,
) -> ChangeItem {
    let nodes = tree.diff.store.read();
    let i = node as usize;
    ChangeItem {
        node,
        path: nodes.path_printable(node),
        label: short_label(&nodes, node, scope),
        kind: change_kind(nodes.kind(node)),
        delta: tree.diff.delta[i],
        base: tree.diff.base_alloc[i],
        target: tree.diff.target_alloc[i],
        status: change_status(tree.diff.status[i]),
        new_files: tree.new_files[i] as i64,
        members: 0,
        reason,
        on_disk,
    }
}

/// The item of a change unit: [`change_item`], or for a rest unit (the other changed entries of
/// a broken-down folder together) a `group` item 「{label} 中其余 {n} 项」.
pub fn rest_item(item: ChangeItem, unit: &Change) -> ChangeItem {
    if !unit.rest {
        return item;
    }
    let label = text("rest")
        .replace("{label}", &item.label)
        .replace("{n}", &unit.members.to_string());
    ChangeItem {
        kind: ChangeKind::Group,
        label,
        delta: unit.delta,
        base: unit.base_alloc,
        target: unit.target_alloc,
        status: ChangeStatus::Changed,
        members: unit.members,
        on_disk: false,
        ..item
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_names() {
        assert!(generic("Download"));
        assert!(generic("a1b2c3d4"));
        assert!(generic("0123456789abcdef"));
        assert!(!generic("WeChat Files"));
        assert!(!generic("abcdefgh"));
    }
}
