//! Changes to a scanned tree after the scan (file change monitor): docs/monitor.md §4.
//!
//! The store stays append-only: ids are never reused and a row's name is never rewritten (name
//! indexes and level caches rely on it). A file whose size or times changed is updated in place
//! ([`update_stats`]). A folder whose entries changed is listed again ([`relist`]) and gets a
//! *new* child range at the end of the store: entries that are still there are copied over (a
//! folder with its own child range and totals; its children are re-parented), new entries get new
//! rows, and the old range — plus every subtree that disappeared — is marked `GONE`: kind
//! `Other`, depth 0, no parent, no size, outside every child range, so tree walks never reach it
//! and whole-store scans mask it out. [`compact`] renumbers the store once too much is `GONE`.
//!
//! Everything that changes the tree here takes the exclusive [`Writer`]: readers never see a
//! half-moved folder.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;

use crate::store::{Nodes, Store, StoreBuilder, StoreOptions, Writer};
use crate::types::*;

/// Compact once `GONE` rows are this share of the store ...
pub const COMPACT_RATIO: f64 = 0.25;
/// ... and at least this many.
pub const COMPACT_MIN: usize = 100_000;

/// State a row carries over when it is copied to a new id (the attributes come from the new
/// listing).
const KEEP: u32 =
    Flags::PENDING.0 | Flags::ACCESS_DENIED.0 | Flags::ERROR.0 | Flags::HARDLINK_DUP.0;

/// What [`relist`] did to one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relist {
    /// first id of the folder's new child range
    pub start: NodeId,
    /// old ids of entries that are still there ...
    pub moved_from: Vec<NodeId>,
    /// ... and their new ids
    pub moved_to: Vec<NodeId>,
    /// old ids of entries that disappeared (their subtrees are `GONE` as well)
    pub removed: Vec<NodeId>,
    /// what each of them occupied (subtree total for folders)
    pub removed_alloc: Vec<i64>,
    /// new ids of entries that are new
    pub added: Vec<NodeId>,
    /// the new folders among them: still to be scanned
    pub pending: Vec<NodeId>,
    /// new ids of kept files whose allocation changed ...
    pub grown: Vec<NodeId>,
    /// ... and by how much (bytes, signed; 0 for hard-link duplicates)
    pub grown_by: Vec<i64>,
    /// rows marked `GONE` by this change
    pub gone: usize,
}

#[inline]
pub fn is_gone(flags: u32) -> bool {
    flags & Flags::GONE.0 != 0
}

pub fn gone_count(nodes: &Nodes) -> usize {
    nodes
        .flags_col()
        .iter()
        .filter(|f| is_gone(f.load(Ordering::Relaxed)))
        .count()
}

pub fn needs_compaction(nodes: &Nodes) -> bool {
    let gone = gone_count(nodes);
    gone >= COMPACT_MIN && gone as f64 >= COMPACT_RATIO * nodes.len() as f64
}

#[inline]
fn counted(nodes: &Nodes, id: NodeId) -> bool {
    !nodes.flags(id).intersects(Flags::HARDLINK_DUP)
}

/// Sets new sizes (and modification times) of files in place; every ancestor's totals follow.
/// Returns the change of allocation per node (0 for hard-link duplicates, which never count).
pub fn update_stats(
    w: &mut Writer<'_>,
    nodes: &[NodeId],
    size: &[i64],
    alloc: &[i64],
    mtime: Option<&[i64]>,
) -> Vec<i64> {
    assert!(nodes.len() == size.len() && nodes.len() == alloc.len());
    let mut out = Vec::with_capacity(nodes.len());
    for (k, &node) in nodes.iter().enumerate() {
        let keep = counted(w, node);
        let d_size = if keep { size[k] - w.size(node) } else { 0 };
        let d_alloc = if keep { alloc[k] - w.alloc(node) } else { 0 };
        let parent = w.parent(node);
        w.set_raw::<i64>(Col::Size, node, size[k]);
        w.set_raw::<i64>(Col::Alloc, node, alloc[k]);
        if let Some(m) = mtime {
            w.set_raw::<u32>(Col::Mtime, node, m[k].clamp(0, u32::MAX as i64) as u32);
        }
        if d_size != 0 || d_alloc != 0 {
            w.propagate(parent, [d_size, d_alloc, 0, 0]);
        }
        out.push(d_alloc);
    }
    w.touch();
    out
}

/// A listed, live folder none of whose children still waits to be scanned (their ids are in a
/// scan's queue; [`relist`] would move them).
pub fn can_relist(nodes: &Nodes, folder: NodeId) -> bool {
    if !nodes.is_dir(folder) || nodes.flags(folder).intersects(Flags::GONE | Flags::PENDING) {
        return false;
    }
    !nodes
        .children(folder)
        .any(|c| nodes.flags(c).intersects(Flags::PENDING))
}

/// Every node strictly below any of `roots` (folders' child ranges, level by level).
pub(crate) fn descendants(nodes: &Nodes, roots: &[NodeId]) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut frontier: Vec<NodeId> = roots.iter().copied().filter(|&r| nodes.is_dir(r)).collect();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for &d in &frontier {
            for c in nodes.children(d) {
                found.push(c);
                if nodes.is_dir(c) {
                    next.push(c);
                }
            }
        }
        if found.len() > nodes.len() {
            break; // inconsistent ranges: never run away
        }
        frontier = next;
    }
    found
}

/// A folder's four totals from its children (what reaggregation computes).
fn own_totals(nodes: &Nodes, children: std::ops::Range<NodeId>) -> [i64; 4] {
    let mut t = [0i64; 4];
    for c in children {
        let dir = nodes.is_dir(c);
        if counted(nodes, c) {
            t[0] += nodes.size(c);
            t[1] += nodes.alloc(c);
        }
        t[2] += (nodes.kind(c) == Kind::File) as i64;
        t[3] += dir as i64;
        if dir {
            t[0] += nodes.tot_size(c);
            t[1] += nodes.tot_alloc(c);
            t[2] += nodes.tot_files(c) as i64;
            t[3] += nodes.tot_dirs(c) as i64;
        }
    }
    t
}

fn mark_gone(w: &mut Writer<'_>, ids: &[NodeId]) {
    for &id in ids {
        w.set_raw::<u32>(Col::Flags, id, Flags::GONE.0);
        w.set_raw::<u8>(Col::Kind, id, Kind::Other as u8);
        w.set_raw::<i32>(Col::Parent, id, -1);
        w.set_raw::<u16>(Col::Depth, id, 0);
        w.set_raw::<i64>(Col::Size, id, 0);
        w.set_raw::<i64>(Col::Alloc, id, 0);
        w.set_raw::<i32>(Col::ChildStart, id, 0);
        w.set_raw::<i32>(Col::ChildCount, id, 0);
        w.set_raw::<i64>(Col::TotSize, id, 0);
        w.set_raw::<i64>(Col::TotAlloc, id, 0);
        w.set_raw::<i32>(Col::TotFiles, id, 0);
        w.set_raw::<i32>(Col::TotDirs, id, 0);
    }
}

/// Replaces a listed folder's children by a fresh listing of it (same batch as a scan).
///
/// Entries are matched by name (a name held twice pairs in listing order); a name that changed
/// between file and folder counts as removed and added. New folders are `PENDING` (to be
/// scanned) unless they are mount points. New files take `HARDLINK_DUP` from the batch, kept
/// ones keep theirs.
///
/// Kept entries change ids. While a scan runs, its queue names `PENDING` folders by id: do not
/// relist a folder with a `PENDING` child before the scan is done (see [`can_relist`]).
pub fn relist(
    w: &mut Writer<'_>,
    folder: NodeId,
    batch: &EntryBatch,
) -> Result<Relist, StoreError> {
    batch.check()?;
    if folder as usize >= w.len() || !w.is_dir(folder) || w.flags(folder).intersects(Flags::GONE) {
        return Err(StoreError::NotAFolder(folder));
    }
    let (lo, hi) = w.child_range(folder);
    let count = batch.len();
    let offs = batch.name_offsets();

    // each old entry matches one new entry: a name a folder holds twice (WinBtrfs shows invalid
    // UTF-8 as U+FFFD) pairs in listing order, never two new rows with one old row's children
    let mut by_name: HashMap<Vec<u8>, VecDeque<NodeId>> = HashMap::new();
    for i in lo..hi {
        by_name
            .entry(w.name_bytes(i).to_vec())
            .or_default()
            .push_back(i);
    }
    let mut kept_old: Vec<Option<NodeId>> = vec![None; count];
    for (k, slot) in kept_old.iter_mut().enumerate() {
        let name = &batch.names[offs[k]..offs[k + 1]];
        if let Some(o) = by_name.get_mut(name).and_then(|q| q.pop_front()) {
            let new_dir = batch.kind[k] == Kind::Dir as u8;
            if w.is_dir(o) == new_dir {
                *slot = Some(o);
            }
        }
    }
    let mut is_kept = vec![false; (hi - lo) as usize];
    for o in kept_old.iter().flatten() {
        is_kept[(o - lo) as usize] = true;
    }
    let removed: Vec<NodeId> = (lo..hi).filter(|&i| !is_kept[(i - lo) as usize]).collect();
    let removed_alloc: Vec<i64> = removed
        .iter()
        .map(|&r| {
            if w.is_dir(r) {
                w.tot_alloc(r)
            } else if counted(w, r) {
                w.alloc(r)
            } else {
                0
            }
        })
        .collect();
    let dead_below = descendants(w, &removed); // before anything moves

    let n0 = w.append_rows_unlinked(folder, batch)?;
    let mut out = Relist {
        start: n0 as NodeId,
        removed,
        removed_alloc,
        ..Relist::default()
    };
    for (k, o) in kept_old.iter().enumerate() {
        let new = (n0 + k) as NodeId;
        let Some(old) = *o else {
            out.added.push(new);
            if batch.kind[k] == Kind::Dir as u8 && batch.flags[k] & Flags::MOUNT.0 == 0 {
                out.pending.push(new);
                let f = w.flags_raw(new) | Flags::PENDING.0;
                w.set_raw::<u32>(Col::Flags, new, f);
            }
            continue;
        };
        out.moved_from.push(old);
        out.moved_to.push(new);
        let f = w.flags_raw(new) | (w.flags_raw(old) & KEEP);
        w.set_raw::<u32>(Col::Flags, new, f);
        if w.is_dir(old) {
            let (s, c) = (w.child_start(old), w.child_count(old));
            w.set_raw::<i32>(Col::ChildStart, new, s);
            w.set_raw::<i32>(Col::ChildCount, new, c);
            let tots = (
                w.tot_size(old),
                w.tot_alloc(old),
                w.tot_files(old),
                w.tot_dirs(old),
            );
            w.set_raw::<i64>(Col::TotSize, new, tots.0);
            w.set_raw::<i64>(Col::TotAlloc, new, tots.1);
            w.set_raw::<i32>(Col::TotFiles, new, tots.2);
            w.set_raw::<i32>(Col::TotDirs, new, tots.3);
            if c > 0 {
                for g in s as NodeId..(s + c) as NodeId {
                    w.set_raw::<i32>(Col::Parent, g, new as i32); // grandchildren
                }
            }
        } else if w.alloc(new) != w.alloc(old) {
            out.grown.push(new);
            out.grown_by.push(if counted(w, old) {
                w.alloc(new) - w.alloc(old)
            } else {
                0
            });
        }
    }

    let before = [
        w.tot_size(folder),
        w.tot_alloc(folder),
        w.tot_files(folder) as i64,
        w.tot_dirs(folder) as i64,
    ];
    w.set_raw::<i32>(Col::ChildStart, folder, n0 as i32);
    w.set_raw::<i32>(Col::ChildCount, folder, count as i32);
    let mut gone: Vec<NodeId> = (lo..hi).collect();
    gone.extend(dead_below);
    mark_gone(w, &gone);
    out.gone = gone.len();
    let after = own_totals(w, n0 as NodeId..(n0 + count) as NodeId);
    w.set_raw::<i64>(Col::TotSize, folder, after[0]);
    w.set_raw::<i64>(Col::TotAlloc, folder, after[1]);
    w.set_raw::<i32>(Col::TotFiles, folder, after[2] as i32);
    w.set_raw::<i32>(Col::TotDirs, folder, after[3] as i32);
    let parent = w.parent(folder);
    let delta = [
        after[0] - before[0],
        after[1] - before[1],
        after[2] - before[2],
        after[3] - before[3],
    ];
    if parent >= 0 && delta != [0; 4] {
        w.propagate(parent, delta);
    }
    w.touch();
    Ok(out)
}

/// A copy of the live tree renumbered breadth first (children right after each other, every
/// range after its folder again, no `GONE` rows) and `remap`: new id per old id, -1 for rows
/// that are not in the tree any more.
///
/// A pure read: run it under a [`crate::Reader`]; then adopt the copy with
/// [`Writer::adopt`] once nothing changed in between (compare [`Store::version`]). Memory besides
/// the copy itself: the order and `remap` (4 B a row each).
pub fn compact(nodes: &Nodes) -> Result<(Store, Vec<i32>), StoreError> {
    compact_with(nodes, StoreOptions::default())
}

/// [`compact`] with the new store's options (policy, chunk sizes).
pub fn compact_with(nodes: &Nodes, opts: StoreOptions) -> Result<(Store, Vec<i32>), StoreError> {
    let n = nodes.len();
    let order = crate::store::Appender::bfs_order(nodes);
    if order.len() > n {
        return Err(StoreError::RunawayWalk(0));
    }
    let m = order.len();
    let mut remap = vec![-1i32; n];
    for (new, &old) in order.iter().enumerate() {
        remap[old as usize] = new as i32;
    }
    let image_len: usize = order.iter().map(|&o| nodes.name_len(o) as usize + 1).sum();
    let mut b = StoreBuilder::new(m, image_len, opts);
    b.set_ext_table(nodes.ext_table().iter().map(|e| e.to_string()).collect());
    {
        let c = b.columns();
        for (new, &o) in order.iter().enumerate() {
            let p = nodes.parent(o);
            c.parent[new] = if p >= 0 { remap[p as usize] } else { -1 };
            c.depth[new] = nodes.depth(o);
            c.kind[new] = nodes.kind_raw(o);
            c.flags[new] = nodes.flags_raw(o);
            c.name_len[new] = nodes.name_len(o);
            c.ext[new] = nodes.ext(o);
            c.size[new] = nodes.size(o);
            c.alloc[new] = nodes.alloc(o);
            c.mtime[new] = nodes.mtime(o);
            c.ctime[new] = nodes.ctime(o);
            c.atime[new] = nodes.atime(o);
            c.ino[new] = nodes.ino(o);
            c.nlink[new] = nodes.nlink(o);
            let (s, e) = nodes.child_range(o);
            let listed = e > s;
            c.child_start[new] = if listed { remap[s as usize] } else { 0 };
            c.child_count[new] = (e - s) as i32;
            c.tot_size[new] = nodes.tot_size(o);
            c.tot_alloc[new] = nodes.tot_alloc(o);
            c.tot_files[new] = nodes.tot_files(o);
            c.tot_dirs[new] = nodes.tot_dirs(o);
        }
    }
    {
        // names: a fresh image, `\0name` per node in the new order
        let image = b.image();
        let mut pos = 0;
        for &o in &order {
            let name = nodes.name_bytes(o);
            image[pos] = 0;
            image[pos + 1..pos + 1 + name.len()].copy_from_slice(name);
            pos += name.len() + 1;
        }
    }
    let store = b.finish()?;
    Ok((store, remap))
}
