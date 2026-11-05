//! The merge of two trees into one union tree (port of `diff/engine.py`).
//!
//! Both trees are walked from their roots together, level by level. The children of a folder
//! that exists on both sides are matched by name (the Windows rule folds case like
//! `ntpath.normcase` of the decoded name; an exact spelling wins over a folded one); a file and
//! a folder of the same name are two different items (removed + added), a renamed item is
//! removed + added. Everything below a folder that exists on one side only is taken over whole.
//! `GONE` rows lie outside every child range and never take part.
//!
//! One folder pair is matched without allocating per node:
//!
//! 1. Aligned passes: position by position from the front and, when the counts differ, from the
//!    back. Two scans of the same disk list almost every folder in the same order.
//! 2. Leftovers by exact name (first come, first served in listing order).
//! 3. Windows rule only: the leftovers once more with case folded.
//!
//! The folder pairs of a level are matched in parallel (rayon); the union columns are gathered
//! in parallel too. The union store is laid out breadth first: every level's rows are grouped by
//! parent in parent order (a matched folder lists the target's children in the target's order,
//! then the removed ones), so child ranges are contiguous, every id is greater than its parent's
//! and the totals are what a reaggregation computes. Its size columns hold the target's values,
//! the base's for removed rows.
//!
//! [`compare`] only reads the two views: the caller holds a reader of each store.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use sk_store::names::normpath;
use sk_store::{Flags, Kind, NodeId, Nodes, Store, StoreBuilder, StoreError, StoreOptions};

use super::text::text;

/// [`DiffTree::changes`]: a folder is broken down when significant children hold this share ...
pub const DOMINANT: f64 = 0.5;
/// ... a child is significant from this share of its folder's change.
pub const MINOR: f64 = 0.05;

const DIR: u8 = Kind::Dir as u8;
const GONE: u32 = Flags::GONE.0;
const DUP: u32 = Flags::HARDLINK_DUP.0;
/// Folder pairs per parallel task.
const FOLDERS_PER_TASK: usize = 64;
/// Union rows per parallel task of the name copy.
const ROWS_PER_TASK: usize = 1 << 14;
/// One task's part of a level: union rows, rows per folder, children seen per side.
type LevelPart = (Vec<(i32, i32)>, Vec<u32>, usize, usize);
/// Leftover pairs (base × target) matched by a nested loop rather than a hash map.
const SMALL_LEFTOVERS: usize = 64;

/// What happened to a row between the base and the target.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Unchanged = 0,
    Added = 1,
    Removed = 2,
    Changed = 3,
}

impl Status {
    pub fn from_u8(v: u8) -> Status {
        match v {
            0 => Status::Unchanged,
            1 => Status::Added,
            2 => Status::Removed,
            _ => Status::Changed,
        }
    }
}

/// The label of a status for the user (current language).
pub fn status_label(status: Status) -> &'static str {
    text(match status {
        Status::Unchanged => "status.unchanged",
        Status::Added => "status.added",
        Status::Removed => "status.removed",
        Status::Changed => "status.changed",
    })
}

/// Growth or shrinkage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Grow,
    Shrink,
}

impl Direction {
    /// `"grow"` / `"shrink"`.
    pub fn parse(s: &str) -> Result<Direction, DiffError> {
        match s {
            "grow" => Ok(Direction::Grow),
            "shrink" => Ok(Direction::Shrink),
            _ => Err(DiffError::Direction),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    /// The cancel callback returned true.
    #[error("{}", text("cancelled"))]
    Cancelled,
    #[error("{}", text("direction"))]
    Direction,
    /// Overlapping child ranges (a corrupt tree): the walk would never end.
    #[error("{}", text("overlap"))]
    Overlap,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// One change unit of [`DiffTree::changes`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// row of [`DiffTree::store`]
    pub node: NodeId,
    pub path: String,
    pub kind: Kind,
    /// a rest unit: `Changed`
    pub status: Status,
    /// growth (or shrinkage) inside the unit: what the ranking uses, > 0
    pub amount: i64,
    /// net change, `target_alloc - base_alloc`
    pub delta: i64,
    pub base_alloc: i64,
    pub target_alloc: i64,
    pub base_size: i64,
    pub target_size: i64,
    /// the folder `node` without its significant children (listed on their own)
    pub rest: bool,
    /// rest unit: its changed entries (children of `node`)
    pub members: i64,
}

/// The union of two trees; one entry per row of [`DiffTree::store`] in every array.
pub struct DiffTree {
    /// union tree; size columns hold the target's values (removed rows: the base's)
    pub store: Store,
    /// row of the base store, -1 where the item is not in the base
    pub base_id: Vec<i32>,
    /// row of the target store, -1 where the item is not in the target
    pub target_id: Vec<i32>,
    /// allocation in the base (folders: subtree total; hard-link duplicates 0)
    pub base_alloc: Vec<i64>,
    pub target_alloc: Vec<i64>,
    /// logical size, same rules
    pub base_size: Vec<i64>,
    pub target_size: Vec<i64>,
    /// `target_alloc - base_alloc`
    pub delta: Vec<i64>,
    pub status: Vec<Status>,
    /// growth summed over the subtree (a file: its own), ≥ 0
    pub grow: Vec<i64>,
    /// shrinkage summed over the subtree, ≥ 0
    pub shrink: Vec<i64>,
    /// names were matched with the Windows rule
    pub fold_case: bool,
}

impl std::fmt::Debug for DiffTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffTree")
            .field("rows", &self.len())
            .field("fold_case", &self.fold_case)
            .finish()
    }
}

impl DiffTree {
    pub fn len(&self) -> usize {
        self.status.len()
    }

    pub fn is_empty(&self) -> bool {
        self.status.is_empty()
    }

    /// Treemap area basis in compare mode: `|change|` per file, a folder the sum below it
    /// (grow + shrink), so nested blocks add up like the normal sizes do.
    pub fn area(&self) -> Vec<i64> {
        self.grow
            .iter()
            .zip(&self.shrink)
            .map(|(g, s)| g + s)
            .collect()
    }

    /// Per-row amounts of a direction.
    pub fn amounts(&self, direction: Direction) -> &[i64] {
        match direction {
            Direction::Grow => &self.grow,
            Direction::Shrink => &self.shrink,
        }
    }

    pub fn memory_bytes(&self) -> usize {
        let rows = self.len();
        self.store.read().memory_bytes() + rows * (2 * 4 + 8 * 8 + 1)
    }

    /// The `limit` largest change units in `direction`, largest first.
    ///
    /// The units partition the change exactly: none lies inside another and together they add up
    /// to the growth (shrinkage) of the whole scope. Rule, from the scope down: a child holding at
    /// least `minor` of its folder's change is *significant*. A folder whose significant children
    /// together hold at least `dominant` of its change is broken down: each significant child
    /// competes on its own (and is broken down by the same rule), the rest of the folder becomes
    /// one `rest` unit. Any other folder is one unit, and so is every file. Ties: lower row first.
    ///
    /// `scope`: rank inside this row's subtree only (0: everything); a folder scope is always
    /// broken down. `amount`: per-row amounts to rank by instead of [`DiffTree::grow`] /
    /// [`DiffTree::shrink`] (a folder's amount must be the sum below it).
    pub fn changes(
        &self,
        direction: Direction,
        limit: usize,
        scope: NodeId,
        amount: Option<&[i64]>,
        dominant: f64,
        minor: f64,
    ) -> Vec<Change> {
        let amount = amount.unwrap_or(self.amounts(direction));
        let nodes = self.store.read();
        let mut out = Vec::new();
        if limit == 0 || scope as usize >= nodes.len() {
            return out;
        }
        let mut lists: Vec<Vec<NodeId>> = Vec::new();
        // (-amount, node, is rest, list, rank): a min-heap through Reverse
        type Entry = Reverse<(i64, NodeId, u8, usize, usize)>;
        let mut heap: BinaryHeap<Entry> = BinaryHeap::new();
        let mut rests: HashMap<NodeId, (i64, Vec<NodeId>)> = HashMap::new();
        let significant = |node: NodeId, everything: bool| -> (Vec<NodeId>, i64) {
            let (lo, hi) = nodes.child_range(node);
            let bound = minor * amount[node as usize] as f64;
            let mut ids: Vec<NodeId> = (lo..hi)
                .filter(|&c| {
                    let a = amount[c as usize];
                    a > 0 && (everything || a as f64 >= bound)
                })
                .collect();
            ids.sort_by_key(|&c| Reverse(amount[c as usize])); // stable
            let positive = (lo..hi).filter(|&c| amount[c as usize] > 0).count() as i64;
            let others = positive - ids.len() as i64;
            (ids, others)
        };
        let sum = |ids: &[NodeId]| -> i64 { ids.iter().map(|&c| amount[c as usize]).sum() };
        let expand = |node: NodeId,
                      ids: Vec<NodeId>,
                      others: i64,
                      lists: &mut Vec<Vec<NodeId>>,
                      heap: &mut BinaryHeap<Entry>,
                      rests: &mut HashMap<NodeId, (i64, Vec<NodeId>)>| {
            let rest = amount[node as usize] - sum(&ids);
            if let Some(&first) = ids.first() {
                heap.push(Reverse((-amount[first as usize], first, 0, lists.len(), 0)));
            }
            if rest > 0 && node != scope {
                heap.push(Reverse((-rest, node, 1, usize::MAX, 0)));
                rests.insert(node, (others, ids.clone()));
            }
            if !ids.is_empty() {
                lists.push(ids);
            }
        };
        if nodes.is_dir(scope) {
            let (ids, others) = significant(scope, true);
            expand(scope, ids, others, &mut lists, &mut heap, &mut rests);
        } else if amount[scope as usize] > 0 {
            return vec![self.change(&nodes, scope, amount[scope as usize], None, 0)];
        }
        while out.len() < limit {
            let Some(Reverse((neg, node, is_rest, li, rank))) = heap.pop() else {
                break;
            };
            if is_rest == 1 {
                let (others, sig) = rests.remove(&node).unwrap_or_default();
                out.push(self.change(&nodes, node, -neg, Some(&sig), others));
                continue;
            }
            let sibs = &lists[li];
            if rank + 1 < sibs.len() {
                let next = sibs[rank + 1];
                heap.push(Reverse((-amount[next as usize], next, 0, li, rank + 1)));
            }
            if nodes.is_dir(node) {
                let (ids, others) = significant(node, false);
                if !ids.is_empty() && sum(&ids) as f64 >= dominant * amount[node as usize] as f64 {
                    expand(node, ids, others, &mut lists, &mut heap, &mut rests);
                    continue;
                }
            }
            out.push(self.change(&nodes, node, -neg, None, 0));
        }
        out
    }

    /// A unit: a whole row, or (`rest`: the significant children taken out) its rest.
    fn change(
        &self,
        nodes: &Nodes,
        node: NodeId,
        amount: i64,
        rest: Option<&[NodeId]>,
        members: i64,
    ) -> Change {
        let value = |arr: &[i64]| -> i64 {
            arr[node as usize] - rest.map_or(0, |r| r.iter().map(|&c| arr[c as usize]).sum())
        };
        Change {
            node,
            path: nodes.path_lossy(node),
            kind: nodes.kind(node),
            status: if rest.is_some() {
                Status::Changed
            } else {
                self.status[node as usize]
            },
            amount,
            delta: value(&self.delta),
            base_alloc: value(&self.base_alloc),
            target_alloc: value(&self.target_alloc),
            base_size: value(&self.base_size),
            target_size: value(&self.target_size),
            rest: rest.is_some(),
            members,
        }
    }
}

/// Options of [`compare_with`].
#[derive(Default)]
pub struct CompareOptions<'a> {
    /// Match names case-insensitively (the Windows rule); default: on Windows.
    pub fold_case: Option<bool>,
    /// Gets the fraction done (0..1) now and then, on the calling thread.
    pub progress: Option<&'a mut dyn FnMut(f64)>,
    /// Polled between steps; true makes the compare stop with [`DiffError::Cancelled`].
    pub cancel: Option<&'a dyn Fn() -> bool>,
    /// How the union store is set up (mapping policy); default: the app's.
    pub store: Option<StoreOptions>,
}

/// Merge `base` and `target` by path ([`compare_with`] without progress or cancel).
pub fn compare(
    base: &Nodes,
    target: &Nodes,
    fold_case: Option<bool>,
) -> Result<DiffTree, DiffError> {
    compare_with(
        base,
        target,
        CompareOptions {
            fold_case,
            ..CompareOptions::default()
        },
    )
}

/// Merge `base` and `target` by path into a [`DiffTree`].
pub fn compare_with(
    base: &Nodes,
    target: &Nodes,
    opts: CompareOptions<'_>,
) -> Result<DiffTree, DiffError> {
    let t0 = Instant::now();
    let fold = opts.fold_case.unwrap_or(cfg!(windows));
    let mut merge = Merge {
        b: Side::new(base),
        t: Side::new(target),
        fold,
        progress: opts.progress,
        cancel: opts.cancel,
        done: 0,
        total: (base.len() + target.len()).max(1),
    };
    let tree = merge.run(opts.store.unwrap_or_default())?;
    tracing::debug!(
        "compare: {} + {} rows -> {} in {:.2} s",
        base.len(),
        target.len(),
        tree.len(),
        t0.elapsed().as_secs_f64()
    );
    Ok(tree)
}

// --------------------------------------------------------------------------- names

/// A name folded by the Windows rule (`ntpath.normcase` of the decoded name: lower case).
/// Names that are not valid UTF-8 only fold their ASCII letters (two different invalid
/// sequences never become equal).
fn fold_name(raw: &[u8]) -> Cow<'_, [u8]> {
    if raw.is_ascii() {
        if raw.iter().any(u8::is_ascii_uppercase) {
            return Cow::Owned(raw.to_ascii_lowercase());
        }
        return Cow::Borrowed(raw);
    }
    match std::str::from_utf8(raw) {
        Ok(s) => Cow::Owned(s.to_lowercase().into_bytes()),
        Err(_) => Cow::Owned(raw.to_ascii_lowercase()),
    }
}

// --------------------------------------------------------------------------- one input

struct Side<'a> {
    nodes: &'a Nodes,
    n: usize,
    kind: &'a [u8],
    flags: &'a [AtomicU32],
}

impl<'a> Side<'a> {
    fn new(nodes: &'a Nodes) -> Side<'a> {
        Side {
            nodes,
            n: nodes.len(),
            kind: nodes.kinds_raw(),
            flags: nodes.flags_col(),
        }
    }

    #[inline]
    fn is_dir(&self, row: u32) -> bool {
        self.kind[row as usize] == DIR
    }

    #[inline]
    fn flags(&self, row: u32) -> u32 {
        self.flags[row as usize].load(Ordering::Relaxed)
    }

    #[inline]
    fn name(&self, row: u32) -> &'a [u8] {
        self.nodes.name_bytes(row)
    }

    /// Live children of `row` (-1 or not a folder: none).
    fn children(&self, row: i32, out: &mut Vec<u32>) {
        out.clear();
        if row < 0 || (row as usize) >= self.n || !self.is_dir(row as u32) {
            return;
        }
        let (lo, hi) = self.nodes.child_range(row as u32);
        out.extend((lo..hi).filter(|&c| self.flags(c) & GONE == 0));
    }

    /// Per union row (`ids`: this side's row, -1): allocation and size at this end (subtree totals
    /// for folders, 0 for hard-link duplicates) and the row's own allocation as it counts toward
    /// its folder (a folder's own blocks too).
    fn end(&self, row: i32) -> (i64, i64, i64) {
        if row < 0 {
            return (0, 0, 0);
        }
        let r = row as u32;
        let counted = self.flags(r) & DUP == 0;
        let (alloc, size) = if counted {
            (self.nodes.alloc(r), self.nodes.size(r))
        } else {
            (0, 0)
        };
        if self.is_dir(r) {
            (self.nodes.tot_alloc(r), self.nodes.tot_size(r), alloc)
        } else {
            (alloc, size, alloc)
        }
    }
}

// --------------------------------------------------------------------------- matching

/// Reused buffers of one task.
#[derive(Default)]
struct Scratch {
    kb: Vec<u32>,
    kt: Vec<u32>,
    mb: Vec<i32>,
    mt: Vec<i32>,
}

struct Pairs<'s, 'a> {
    b: &'s Side<'a>,
    t: &'s Side<'a>,
    fold: bool,
}

impl Pairs<'_, '_> {
    /// The children of the folder pair `(b, t)` as union rows `(base row, target row)`: the
    /// target's in order (with their match), then the base's unmatched ones. Returns how many
    /// children each side had.
    fn level(&self, b: i32, t: i32, s: &mut Scratch, out: &mut Vec<(i32, i32)>) -> (usize, usize) {
        self.b.children(b, &mut s.kb);
        self.t.children(t, &mut s.kt);
        let (nb, nt) = (s.kb.len(), s.kt.len());
        s.mb.clear();
        s.mb.resize(nb, -1);
        s.mt.clear();
        s.mt.resize(nt, -1);
        if nb > 0 && nt > 0 {
            self.match_children(s);
        }
        for (j, &row) in s.kt.iter().enumerate() {
            let m = s.mt[j];
            out.push((
                if m >= 0 { s.kb[m as usize] as i32 } else { -1 },
                row as i32,
            ));
        }
        for (i, &row) in s.kb.iter().enumerate() {
            if s.mb[i] < 0 {
                out.push((row as i32, -1));
            }
        }
        (nb, nt)
    }

    #[inline]
    fn try_pair(&self, s: &mut Scratch, i: usize, j: usize) -> bool {
        let (rb, rt) = (s.kb[i], s.kt[j]);
        if self.b.is_dir(rb) == self.t.is_dir(rt) && self.b.name(rb) == self.t.name(rt) {
            s.mb[i] = j as i32;
            s.mt[j] = i as i32;
            return true;
        }
        false
    }

    fn match_children(&self, s: &mut Scratch) {
        let (nb, nt) = (s.kb.len(), s.kt.len());
        let n = nb.min(nt);
        // 1. aligned from the front
        let mut left = 0;
        for i in 0..n {
            if !self.try_pair(s, i, i) {
                left += 1;
            }
        }
        if left == 0 && nb == nt {
            return;
        }
        // 2. aligned from the back where the counts differ (an entry added or removed)
        if nb != nt {
            for k in 0..n {
                let (i, j) = (nb - 1 - k, nt - 1 - k);
                if s.mb[i] < 0 && s.mt[j] < 0 {
                    self.try_pair(s, i, j);
                }
            }
        }
        // 3. the rest by name, 4. by folded name (Windows)
        self.by_key(s, false);
        if self.fold {
            self.by_key(s, true);
        }
    }

    /// Leftovers with equal keys pair up, first come first served in listing order.
    fn by_key(&self, s: &mut Scratch, fold: bool) {
        let lb: Vec<usize> = (0..s.kb.len()).filter(|&i| s.mb[i] < 0).collect();
        if lb.is_empty() {
            return;
        }
        let lt: Vec<usize> = (0..s.kt.len()).filter(|&j| s.mt[j] < 0).collect();
        if lt.is_empty() {
            return;
        }
        fn key<'a>(side: &Side<'a>, row: u32, fold: bool) -> (Cow<'a, [u8]>, bool) {
            let name = side.name(row);
            (
                if fold {
                    fold_name(name)
                } else {
                    Cow::Borrowed(name)
                },
                side.is_dir(row),
            )
        }
        let keys_b: Vec<_> = lb.iter().map(|&i| key(self.b, s.kb[i], fold)).collect();
        if lb.len() * lt.len() <= SMALL_LEFTOVERS {
            let mut free = vec![true; lb.len()];
            for &j in &lt {
                let k = key(self.t, s.kt[j], fold);
                if let Some(p) = (0..lb.len()).find(|&p| free[p] && keys_b[p] == k) {
                    free[p] = false;
                    s.mb[lb[p]] = j as i32;
                    s.mt[j] = lb[p] as i32;
                }
            }
            return;
        }
        let mut waiting: HashMap<(Cow<'_, [u8]>, bool), VecDeque<usize>> =
            HashMap::with_capacity(lb.len());
        for (p, k) in keys_b.into_iter().enumerate() {
            waiting.entry(k).or_default().push_back(lb[p]);
        }
        for &j in &lt {
            let k = key(self.t, s.kt[j], fold);
            if let Some(i) = waiting.get_mut(&k).and_then(VecDeque::pop_front) {
                s.mb[i] = j as i32;
                s.mt[j] = i as i32;
            }
        }
    }

    /// Scan roots: equal normalised paths (folded with the Windows rule), first come first.
    fn roots(&self, out: &mut Vec<(i32, i32)>) -> (usize, usize) {
        let mut s = Scratch::default();
        self.b.children(0, &mut s.kb);
        self.t.children(0, &mut s.kt);
        let key = |side: &Side<'_>, row: u32| -> (Vec<u8>, bool) {
            let path = normpath(side.name(row));
            let path = if self.fold {
                fold_name(&path).into_owned()
            } else {
                path
            };
            (path, side.is_dir(row))
        };
        let mut waiting: HashMap<(Vec<u8>, bool), VecDeque<usize>> = HashMap::new();
        for (i, &row) in s.kb.iter().enumerate() {
            waiting.entry(key(self.b, row)).or_default().push_back(i);
        }
        let mut matched = vec![false; s.kb.len()];
        for &row in &s.kt {
            let hit = waiting
                .get_mut(&key(self.t, row))
                .and_then(VecDeque::pop_front);
            if let Some(i) = hit {
                matched[i] = true;
            }
            out.push((hit.map_or(-1, |i| s.kb[i] as i32), row as i32));
        }
        for (i, &row) in s.kb.iter().enumerate() {
            if !matched[i] {
                out.push((row as i32, -1));
            }
        }
        (s.kb.len(), s.kt.len())
    }
}

// --------------------------------------------------------------------------- the merge

struct Merge<'a, 'o> {
    b: Side<'a>,
    t: Side<'a>,
    fold: bool,
    progress: Option<&'o mut dyn FnMut(f64)>,
    cancel: Option<&'o dyn Fn() -> bool>,
    done: usize,
    total: usize,
}

impl Merge<'_, '_> {
    fn tick(&mut self, fraction: f64) -> Result<(), DiffError> {
        if let Some(cancel) = self.cancel
            && cancel()
        {
            return Err(DiffError::Cancelled);
        }
        if let Some(progress) = self.progress.as_mut() {
            progress(fraction.clamp(0.0, 1.0));
        }
        Ok(())
    }

    fn union_is_dir(&self, b: i32, t: i32) -> bool {
        if t >= 0 {
            self.t.is_dir(t as u32)
        } else {
            self.b.is_dir(b as u32)
        }
    }

    fn run(&mut self, store_opts: StoreOptions) -> Result<DiffTree, DiffError> {
        let mut rows_b: Vec<i32> = vec![0];
        let mut rows_t: Vec<i32> = vec![0];
        let mut parent: Vec<i32> = vec![-1];
        let mut bounds: Vec<usize> = vec![0, 1]; // level k: rows bounds[k] .. bounds[k + 1]
        let (mut seen_b, mut seen_t) = (0usize, 0usize);
        loop {
            self.tick(0.75 * self.done as f64 / self.total as f64)?;
            let lo = bounds[bounds.len() - 2];
            let hi = rows_b.len();
            let folders: Vec<usize> = (lo..hi)
                .filter(|&u| self.union_is_dir(rows_b[u], rows_t[u]))
                .collect();
            if folders.is_empty() {
                break;
            }
            let pairs = Pairs {
                b: &self.b,
                t: &self.t,
                fold: self.fold,
            };
            let results: Vec<LevelPart> = if lo == 0 {
                let mut out = Vec::new();
                let (vb, vt) = pairs.roots(&mut out);
                vec![(out.clone(), vec![out.len() as u32], vb, vt)]
            } else {
                folders
                    .par_chunks(FOLDERS_PER_TASK)
                    .map(|chunk| {
                        let mut s = Scratch::default();
                        let mut out = Vec::new();
                        let mut counts = Vec::with_capacity(chunk.len());
                        let (mut vb, mut vt) = (0, 0);
                        for &u in chunk {
                            let before = out.len();
                            let (cb, ct) = pairs.level(rows_b[u], rows_t[u], &mut s, &mut out);
                            vb += cb;
                            vt += ct;
                            counts.push((out.len() - before) as u32);
                        }
                        (out, counts, vb, vt)
                    })
                    .collect()
            };
            let mut at = 0;
            for (out, counts, vb, vt) in results {
                seen_b += vb;
                seen_t += vt;
                self.done += vb + vt;
                for &c in &counts {
                    parent.extend(std::iter::repeat_n(folders[at] as i32, c as usize));
                    at += 1;
                }
                rows_b.extend(out.iter().map(|p| p.0));
                rows_t.extend(out.iter().map(|p| p.1));
            }
            if seen_b > self.b.n || seen_t > self.t.n || rows_b.len() > u32::MAX as usize / 2 {
                return Err(DiffError::Overlap); // only overlapping child ranges can do this
            }
            if rows_b.len() == hi {
                break;
            }
            bounds.push(rows_b.len());
        }
        if *bounds.last().unwrap() != rows_b.len() {
            bounds.push(rows_b.len());
        }
        self.tick(0.75)?;
        self.build(rows_b, rows_t, parent, &bounds, store_opts)
    }

    fn build(
        &mut self,
        base_id: Vec<i32>,
        target_id: Vec<i32>,
        parent: Vec<i32>,
        bounds: &[usize],
        store_opts: StoreOptions,
    ) -> Result<DiffTree, DiffError> {
        let n = base_id.len();
        let (b, t) = (&self.b, &self.t);
        let lens: Vec<u16> = (0..n)
            .into_par_iter()
            .with_min_len(4096)
            .map(|u| {
                if target_id[u] >= 0 {
                    t.nodes.name_len(target_id[u] as u32)
                } else {
                    b.nodes.name_len(base_id[u] as u32)
                }
            })
            .collect();
        let image_len: usize = lens.iter().map(|&l| l as usize + 1).sum();
        let mut builder = StoreBuilder::new(n, image_len, store_opts);

        // the union's extension table: the target's, then the base's others
        let mut table: Vec<String> = t.nodes.ext_table().iter().map(|e| e.to_string()).collect();
        if table.is_empty() {
            table.push(String::new());
        }
        let mut index: HashMap<String, u16> = table
            .iter()
            .enumerate()
            .map(|(i, e)| (e.clone(), i as u16))
            .collect();
        let ext_map: Vec<u16> = b
            .nodes
            .ext_table()
            .iter()
            .map(|e| {
                if let Some(&j) = index.get(&**e) {
                    return j;
                }
                if table.len() >= 65535 {
                    return 0;
                }
                let j = table.len() as u16;
                table.push(e.to_string());
                index.insert(e.to_string(), j);
                j
            })
            .collect();
        builder.set_ext_table(table);

        // the target's row, else the base's
        {
            let c = builder.columns();
            let src = |u: usize| -> (bool, u32) {
                let tr = target_id[u];
                if tr >= 0 {
                    (true, tr as u32)
                } else {
                    (false, base_id[u] as u32)
                }
            };
            fn gather<T: Copy + Send + Sync>(
                out: &mut [T],
                src: &(dyn Fn(usize) -> (bool, u32) + Sync),
                bcol: &[T],
                tcol: &[T],
            ) {
                out.par_iter_mut()
                    .with_min_len(4096)
                    .enumerate()
                    .for_each(|(u, v)| {
                        let (from_t, r) = src(u);
                        *v = if from_t {
                            tcol[r as usize]
                        } else {
                            bcol[r as usize]
                        };
                    });
            }
            gather(c.kind, &src, b.nodes.kinds_raw(), t.nodes.kinds_raw());
            gather(c.size, &src, b.nodes.sizes(), t.nodes.sizes());
            gather(c.alloc, &src, b.nodes.allocs(), t.nodes.allocs());
            gather(c.mtime, &src, b.nodes.mtimes(), t.nodes.mtimes());
            gather(c.ctime, &src, b.nodes.ctimes(), t.nodes.ctimes());
            gather(c.atime, &src, b.nodes.atimes(), t.nodes.atimes());
            gather(c.ino, &src, b.nodes.inos(), t.nodes.inos());
            gather(c.nlink, &src, b.nodes.nlinks(), t.nodes.nlinks());
            c.name_len.copy_from_slice(&lens);
            c.flags
                .par_iter_mut()
                .with_min_len(4096)
                .enumerate()
                .for_each(|(u, v)| {
                    let (from_t, r) = src(u);
                    *v = if from_t { t.flags(r) } else { b.flags(r) };
                });
            c.ext
                .par_iter_mut()
                .with_min_len(4096)
                .enumerate()
                .for_each(|(u, v)| {
                    let (from_t, r) = src(u);
                    *v = if from_t {
                        t.nodes.ext(r)
                    } else {
                        ext_map.get(b.nodes.ext(r) as usize).copied().unwrap_or(0)
                    };
                });
            c.kind[0] = DIR;
            c.flags[0] = Flags::VIRTUAL.0;
            c.parent.copy_from_slice(&parent);
            for (level, w) in bounds.windows(2).enumerate() {
                c.depth[w[0]..w[1]].fill(level as u16);
            }
            // child ranges: rows are grouped by parent in parent order
            c.child_start.fill(0);
            c.child_count.fill(0);
            for (u, &p) in parent.iter().enumerate().skip(1) {
                let p = p as usize;
                if c.child_count[p] == 0 {
                    c.child_start[p] = u as i32;
                }
                c.child_count[p] += 1;
            }
        }
        self.tick(0.8)?;
        let (b, t) = (&self.b, &self.t);
        copy_names(builder.image(), &lens, &base_id, &target_id, b, t);
        self.tick(0.85)?;
        let (b, t) = (&self.b, &self.t);

        // both ends of every row
        let ends = |side: &Side<'_>, ids: &[i32]| -> Vec<(i64, i64, i64)> {
            ids.par_iter()
                .with_min_len(4096)
                .map(|&r| side.end(r))
                .collect()
        };
        let eb = ends(b, &base_id);
        let et = ends(t, &target_id);
        let mut status: Vec<Status> = (0..n)
            .into_par_iter()
            .with_min_len(4096)
            .map(|u| {
                let (rb, rt) = (base_id[u], target_id[u]);
                if rb < 0 {
                    return Status::Added;
                }
                if rt < 0 {
                    return Status::Removed;
                }
                let (bb, tt) = (eb[u], et[u]);
                let (rb, rt) = (rb as u32, rt as u32);
                // also changed: a file's own size (a hard-link duplicate counts 0 at both ends),
                // a folder's own blocks, the duplicate flag, file <-> link
                let changed = bb.0 != tt.0
                    || bb.1 != tt.1
                    || b.nodes.size(rb) != t.nodes.size(rt)
                    || b.nodes.alloc(rb) != t.nodes.alloc(rt)
                    || b.kind[rb as usize] != t.kind[rt as usize]
                    || (b.flags(rb) ^ t.flags(rt)) & DUP != 0;
                if changed {
                    Status::Changed
                } else {
                    Status::Unchanged
                }
            })
            .collect();
        self.tick(0.9)?;

        // subtree totals, growth, shrinkage and the folders' status, deepest rows first
        let mut grow = vec![0i64; n];
        let mut shrink = vec![0i64; n];
        let mut changed_below = vec![false; n];
        {
            let c = builder.columns();
            c.tot_size.fill(0);
            c.tot_alloc.fill(0);
            c.tot_files.fill(0);
            c.tot_dirs.fill(0);
            for u in (1..n).rev() {
                let is_dir = c.kind[u] == DIR;
                let both = base_id[u] >= 0 && target_id[u] >= 0;
                if is_dir && both && changed_below[u] {
                    status[u] = Status::Changed;
                }
                let own = et[u].2 - eb[u].2;
                let (g, s) = (own.max(0), (-own).max(0));
                if !is_dir {
                    grow[u] = g;
                    shrink[u] = s;
                }
                let p = parent[u] as usize;
                let counted = c.flags[u] & DUP == 0;
                c.tot_size[p] += c.tot_size[u] + if counted { c.size[u] } else { 0 };
                c.tot_alloc[p] += c.tot_alloc[u] + if counted { c.alloc[u] } else { 0 };
                c.tot_files[p] += c.tot_files[u] + (c.kind[u] == Kind::File as u8) as i32;
                c.tot_dirs[p] += c.tot_dirs[u] + is_dir as i32;
                if is_dir {
                    grow[p] += grow[u] + g;
                    shrink[p] += shrink[u] + s;
                } else {
                    grow[p] += g;
                    shrink[p] += s;
                }
                if status[u] != Status::Unchanged {
                    changed_below[p] = true;
                }
            }
        }
        status[0] = if changed_below[0] {
            Status::Changed
        } else {
            Status::Unchanged
        };
        let store = builder.finish()?;
        let base_alloc: Vec<i64> = eb.iter().map(|e| e.0).collect();
        let base_size: Vec<i64> = eb.iter().map(|e| e.1).collect();
        let target_alloc: Vec<i64> = et.iter().map(|e| e.0).collect();
        let target_size: Vec<i64> = et.iter().map(|e| e.1).collect();
        let delta = target_alloc
            .iter()
            .zip(&base_alloc)
            .map(|(t, b)| t - b)
            .collect();
        self.tick(1.0)?;
        Ok(DiffTree {
            store,
            base_id,
            target_id,
            base_alloc,
            target_alloc,
            base_size,
            target_size,
            delta,
            status,
            grow,
            shrink,
            fold_case: self.fold,
        })
    }
}

/// The union's `\0name` image in id order, copied in parallel blocks of rows.
fn copy_names(
    image: &mut [u8],
    lens: &[u16],
    base_id: &[i32],
    target_id: &[i32],
    b: &Side<'_>,
    t: &Side<'_>,
) {
    let n = lens.len();
    let mut blocks: Vec<(usize, &mut [u8])> = Vec::with_capacity(n / ROWS_PER_TASK + 1);
    let mut rest = image;
    let mut start = 0;
    while start < n {
        let stop = (start + ROWS_PER_TASK).min(n);
        let bytes: usize = lens[start..stop].iter().map(|&l| l as usize + 1).sum();
        let (head, tail) = rest.split_at_mut(bytes);
        blocks.push((start, head));
        rest = tail;
        start = stop;
    }
    blocks.into_par_iter().for_each(|(start, buf)| {
        let mut at = 0;
        let stop = (start + ROWS_PER_TASK).min(n);
        for u in start..stop {
            let name = if target_id[u] >= 0 {
                t.name(target_id[u] as u32)
            } else {
                b.name(base_id[u] as u32)
            };
            buf[at] = 0;
            buf[at + 1..at + 1 + name.len()].copy_from_slice(name);
            at += 1 + name.len();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_names() {
        assert_eq!(&*fold_name(b"Hello-World.TXT"), b"hello-world.txt");
        assert_eq!(&*fold_name("ÄRGER".as_bytes()), "ärger".as_bytes());
        assert_eq!(&*fold_name(b"ab\xffC"), b"ab\xffc");
        assert!(matches!(fold_name(b"lower"), Cow::Borrowed(_)));
    }
}
