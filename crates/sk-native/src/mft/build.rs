//! An [`MftTable`] → the node store's columns of a whole volume in one go ([`build_tree`];
//! docs/native.md 2.1, docs/memory.md §6; the native `mft/build.rs` of the Python era). The
//! store's memory comes from a [`TreeSink`] once the build knows how many nodes and name bytes
//! there are; the build fills the columns and the name image in place.
//!
//! Record 5 becomes the root (named [`PseudoNames::root`]); metafiles (records 0–15 and the
//! `$Extend` tree) go under the metafile folder; names whose parent is missing, not a folder or
//! part of a parent cycle go under the orphans folder (each pseudo folder only when non-empty).
//! Sizes follow the hard scan: folders and links occupy nothing, the second and later names of a
//! file are `HARDLINK_DUP`; alternate data streams count only with
//! [`TreeOptions::include_ads`], except in the metafile folder, where they always count toward
//! allocation (`$UsnJrnl:$J`, `$Secure:$SDS` …). `$BadClus:$Bad` never counts (it is as long as
//! the volume).
//!
//! The build, every step over all rows in parallel:
//!
//! 1. parents: the first row of every record (later rows are its other names, `HARDLINK_DUP`),
//!    folders by record, each row's parent row, metafiles under the metafile folder, unknown
//!    parents, non-folders and folders that are their own parent under the orphans;
//! 2. breadth-first order from the root (children grouped in row order); every parent cycle not
//!    reached from the root breaks its smallest row out under the orphans (every row of a cycle
//!    longer than [`CYCLE_LIMIT`], as the numpy build's label propagation did);
//! 3. extensions: the distinct raw extensions of the file names, sorted (up to 7 bytes as
//!    little-endian integers with the length on top, then longer ones as bytes), lower-cased and
//!    numbered in that order;
//! 4. the `\0name` image laid out once in node order, then the columns, written straight into the
//!    sink's memory; each column of the table is dropped as soon as nothing needs it any more
//!    (the table is consumed: the build needs about 57 B a row beyond it, not a second table);
//! 5. subtree totals level by level from the deepest, each level's folders in parallel.
//!
//! The parallel steps run on a rayon pool of the build's own, as wide as the CPUs this process may
//! use; it goes with its threads when the build returns (nothing stays in the process).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

use rayon::prelude::*;

use super::MftTable;
use crate::columns::{NodeColumns, flag, kind};
use crate::util::lower_ext;

pub const ROOT_RECORD: u64 = 5;
/// `$BadClus`: its `$Bad` stream spans the whole volume.
pub const BADCLUS_RECORD: u64 = 8;
/// Records 0–15 are metafiles / reserved.
pub const FIRST_USER_RECORD: u64 = 16;
/// A parent cycle longer than this breaks every member out (not only its smallest row).
pub const CYCLE_LIMIT: usize = 4096;

const KIND_FILE: u8 = kind::FILE;
const KIND_DIR: u8 = kind::DIR;
const NONE: u32 = u32::MAX;

/// How [`build_tree`] counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeOptions {
    /// Count alternate data streams.
    pub include_ads: bool,
    /// Every name of a hard-linked file counts.
    pub count_links_each: bool,
}

/// Names of the three nodes no row of the table stands for (bytes in the store's encoding).
#[derive(Clone, Copy, Debug)]
pub struct PseudoNames<'a> {
    /// The volume's root (record 5), e.g. `C:\`.
    pub root: &'a [u8],
    /// The folder of the metafiles.
    pub meta: &'a [u8],
    /// The folder of entries no folder of the tree reaches.
    pub orphans: &'a [u8],
}

/// Where the built columns go: the store's memory, asked for once.
pub trait TreeSink {
    type Error;

    /// Every column of `nodes` nodes (node 0 the virtual top, node 1 the root, the rest
    /// breadth-first) and a name image of `image_len` bytes, all zero-filled, for the build to
    /// fill; `ext_table` is the store's extension table (id 0 = no extension). The image becomes
    /// `\0name` per node in id order; `name_off` follows from `name_len`.
    fn columns(
        &mut self,
        nodes: usize,
        image_len: usize,
        ext_table: Vec<String>,
    ) -> Result<(NodeColumns<'_>, &mut [u8]), Self::Error>;
}

/// Why a table could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildError<E> {
    /// The table does not hold together (its columns differ in rows, names out of range).
    Table(&'static str),
    /// More rows than the store holds.
    TooManyRows,
    /// A pseudo folder's name is longer than 65535 bytes.
    NameTooLong(usize),
    /// The sink refused, or gave columns of another shape.
    Sink(E),
}

impl<E: fmt::Display> fmt::Display for BuildError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Table(why) => write!(f, "inconsistent $MFT table: {why}"),
            BuildError::TooManyRows => f.write_str("the $MFT table has too many rows"),
            BuildError::NameTooLong(n) => write!(f, "a name of {n} bytes is too long"),
            BuildError::Sink(e) => e.fmt(f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for BuildError<E> {}

/// What [`build_tree`] made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildReport {
    /// Rows of the table.
    pub names: usize,
    /// Nodes built (node 0, the root, the pseudo folders in use, every row).
    pub nodes: usize,
    /// Entries under the orphans folder.
    pub orphans: usize,
    /// Parent cycles broken (corrupt directory entries).
    pub cycles: usize,
}

/// Builds the node columns of a whole volume from `$MFT` columns into `sink` (see the module
/// docs). The table is consumed: its columns go as soon as the build is done with them.
pub fn build_tree<S>(
    table: MftTable,
    names: &PseudoNames<'_>,
    opts: &TreeOptions,
    sink: &mut S,
) -> Result<BuildReport, BuildError<S::Error>>
where
    S: TreeSink + Send,
    S::Error: Send,
{
    // the build's own pool: its threads go with it (docs/memory.md §6); without one, the
    // current pool
    match rayon::ThreadPoolBuilder::new()
        .num_threads(crate::cpus::usable())
        .thread_name(|i| format!("sk-mft-build-{i}"))
        .build()
    {
        Ok(pool) => pool.install(|| build(table, names, opts, sink)),
        Err(_) => build(table, names, opts, sink),
    }
}

/// A raw pointer that parallel workers write through, each to its own elements.
#[derive(Clone, Copy)]
struct Out<T>(*mut T);
// SAFETY: used only where every worker writes a disjoint set of elements.
unsafe impl<T> Send for Out<T> {}
unsafe impl<T> Sync for Out<T> {}
impl<T> Out<T> {
    /// SAFETY: `i` in bounds and written by this worker only.
    #[inline]
    unsafe fn put(self, i: usize, v: T) {
        unsafe { self.0.add(i).write(v) }
    }

    /// SAFETY: `[i, i + src.len())` in bounds and written by this worker only.
    #[inline]
    unsafe fn copy_in(self, i: usize, src: &[T])
    where
        T: Copy,
    {
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), self.0.add(i), src.len()) }
    }
}

fn build<S: TreeSink>(
    table: MftTable,
    extra: &PseudoNames<'_>,
    opts: &TreeOptions,
    sink: &mut S,
) -> Result<BuildReport, BuildError<S::Error>> {
    let MftTable {
        record,
        parent,
        name_off,
        name_len,
        kind,
        flags,
        size,
        alloc,
        ads_size,
        ads_alloc,
        mtime,
        ctime,
        atime,
        nlink,
        names,
        ..
    } = table;
    let n = record.len();
    let lens = [
        parent.len(),
        name_off.len(),
        name_len.len(),
        kind.len(),
        flags.len(),
        size.len(),
        alloc.len(),
        ads_size.len(),
        ads_alloc.len(),
        mtime.len(),
        ctime.len(),
        atime.len(),
        nlink.len(),
    ];
    if lens.iter().any(|&l| l != n) {
        return Err(BuildError::Table("the table's columns differ in rows"));
    }
    if n >= i32::MAX as usize - 4 {
        return Err(BuildError::TooManyRows);
    }
    for name in [extra.root, extra.meta, extra.orphans] {
        if name.len() > u16::MAX as usize {
            return Err(BuildError::NameTooLong(name.len()));
        }
    }
    let blob = names.len();
    if name_off
        .par_iter()
        .zip(&name_len)
        .any(|(&o, &l)| o as usize + l as usize > blob)
    {
        return Err(BuildError::Table("name offsets out of range"));
    }
    let (meta_row, orphan_row, root_row) = (n, n + 1, n + 2);
    let total = n + 3;

    // ------------------------------------------------------------------ 1. parents
    let nrec = record
        .par_iter()
        .copied()
        .max()
        .map_or(0, |m| m as usize + 1)
        .max(ROOT_RECORD as usize + 1);
    let first: Vec<AtomicU32> = (0..nrec)
        .into_par_iter()
        .map(|_| AtomicU32::new(NONE))
        .collect();
    record
        .par_iter()
        .enumerate()
        .with_min_len(1 << 14)
        .for_each(|(i, &r)| {
            first[r as usize].fetch_min(i as u32, Ordering::Relaxed);
        });
    let first: Vec<u32> = first.into_par_iter().map(AtomicU32::into_inner).collect();
    let dup: Vec<bool> = record
        .par_iter()
        .enumerate()
        .map(|(i, &r)| first[r as usize] != i as u32)
        .collect();
    let mut container: Vec<bool> = Vec::with_capacity(total);
    container.par_extend(
        kind.par_iter()
            .zip(&dup)
            .map(|(&k, &d)| k == KIND_DIR && !d),
    );
    container.extend([true, true, true]);
    // folder row of every record (the record's first row, when it is a folder)
    let mut dir_row: Vec<i32> = first
        .into_par_iter()
        .map(|f| {
            if f != NONE && container[f as usize] {
                f as i32
            } else {
                -1
            }
        })
        .collect();
    dir_row[ROOT_RECORD as usize] = root_row as i32;
    let mut pdir: Vec<i32> = Vec::with_capacity(total);
    pdir.par_extend((0..n).into_par_iter().map(|i| {
        let p = parent[i];
        let mut d = if p < nrec as u64 {
            dir_row[p as usize]
        } else {
            -1
        };
        if d < 0 || (container[i] && d == i as i32) {
            d = orphan_row as i32; // unknown parent, not a folder, or a folder in itself
        }
        let r = record[i];
        if r < FIRST_USER_RECORD && r != ROOT_RECORD {
            d = meta_row as i32;
        }
        if r == ROOT_RECORD {
            d = -2; // the root's own "." entry is the root node itself
        }
        d
    }));
    drop(dir_row);
    drop(parent);
    let used = |row: usize| pdir.par_iter().any(|&d| d == row as i32);
    let (meta_used, orphan_used) = (used(meta_row), used(orphan_row));
    pdir.push(if meta_used { root_row as i32 } else { -1 });
    pdir.push(if orphan_used { root_row as i32 } else { -1 });
    pdir.push(-1);

    // ------------------------------------------------------------------ 2. breadth-first order
    let mut topo = breadth_first(&pdir, &container, root_row);
    let expected = 1 + pdir.par_iter().filter(|&&d| d >= 0).count();
    let mut cycles = 0usize;
    if topo.rows.len() < expected {
        let mut seen = vec![false; total];
        for &r in &topo.rows {
            seen[r as usize] = true;
        }
        let stuck: Vec<u32> = (0..total as u32)
            .into_par_iter()
            .filter(|&i| !seen[i as usize] && pdir[i as usize] >= 0)
            .collect();
        drop(seen);
        let breakers = cycle_breakers(&pdir, &stuck);
        cycles = breakers.len();
        for b in breakers {
            pdir[b as usize] = orphan_row as i32;
        }
        pdir[orphan_row] = root_row as i32;
        drop(topo);
        topo = breadth_first(&pdir, &container, root_row);
    }
    let orphans = if pdir[orphan_row] >= 0 {
        topo.counts[orphan_row] as usize
    } else {
        0
    };
    drop(pdir);
    let rows = &topo.rows;
    let count = rows.len() + 1;
    let nlevels = topo.levels.len() - 1;

    // ------------------------------------------------------------------ 3. extensions
    let name_of = |r: usize| {
        let o = name_off[r] as usize;
        &names[o..o + name_len[r] as usize]
    };
    let mut keys: Vec<ExtKey> = (0..n)
        .into_par_iter()
        .with_min_len(1 << 12)
        .fold(HashSet::new, |mut set, r| {
            if kind[r] == KIND_FILE
                && let Some(e) = raw_ext(name_of(r))
            {
                set.insert(ext_key(e));
            }
            set
        })
        .reduce(HashSet::new, |mut a, b| {
            if a.len() < b.len() {
                return b.into_iter().chain(a).collect();
            }
            a.extend(b);
            a
        })
        .into_iter()
        .collect();
    keys.par_sort_unstable(); // Short (as integers) before Long (as bytes)
    let lowered: Vec<String> = keys
        .par_iter()
        .map(|k| String::from_utf8_lossy(&lower_ext(&ext_bytes(k))).into_owned())
        .collect();
    let mut ext_table: Vec<String> = vec![String::new()];
    let mut numbered: HashMap<String, u16> = HashMap::from([(String::new(), 0)]);
    let mut index: HashMap<ExtKey, u16> = HashMap::with_capacity(keys.len());
    for (key, ext) in keys.into_iter().zip(lowered) {
        let id = match numbered.get(&ext) {
            Some(&id) => id,
            None if ext_table.len() >= u16::MAX as usize => 0, // the table is full
            None => {
                let id = ext_table.len() as u16;
                ext_table.push(ext.clone());
                numbered.insert(ext, id);
                id
            }
        };
        index.insert(key, id);
    }
    drop(numbered);

    // ------------------------------------------------------------------ 4. names and columns
    let extra_name = |row: usize| -> &[u8] {
        if row == root_row {
            extra.root
        } else if row == meta_row {
            extra.meta
        } else {
            extra.orphans
        }
    };
    let node_len = |r: usize| {
        if r >= n {
            extra_name(r).len() as u16
        } else {
            name_len[r]
        }
    };
    // where each node's name goes in the image (after its NUL); node 0's empty name first
    let mut starts: Vec<usize> = vec![0; count];
    let mut acc = 1usize;
    for (s, &r) in starts[1..].iter_mut().zip(rows) {
        *s = acc + 1;
        acc += node_len(r as usize) as usize + 1;
    }
    let image_len = acc;
    let (c, image) = sink
        .columns(count, image_len, ext_table)
        .map_err(BuildError::Sink)?;
    // the parallel writes below go through raw pointers: the shape must be exactly this
    assert!(
        c.len() == count && c.is_uniform() && image.len() == image_len,
        "the sink's columns do not have the shape asked for"
    );
    {
        let img = Out(image.as_mut_ptr());
        rows.par_iter()
            .enumerate()
            .with_min_len(1 << 12)
            .for_each(|(j, &r)| {
                let r = r as usize;
                let src = if r >= n { extra_name(r) } else { name_of(r) };
                // SAFETY: the names' ranges of the image are disjoint and inside it (the image
                // is zero-filled: every NUL is in place)
                unsafe { img.copy_in(starts[j + 1], src) };
            });
    }
    drop(starts);
    gather(c.name_len, rows, node_len);
    gather(c.ext, rows, |r| {
        if r < n && kind[r] == KIND_FILE {
            raw_ext(name_of(r)).map_or(0, |e| index[&ext_key(e)])
        } else {
            0
        }
    });
    drop(index);
    drop(names);
    drop(name_off);
    drop(name_len);

    // parents, depths, child ranges: level by level, each folder's children the next ids
    let (levels, counts) = (&topo.levels, &topo.counts);
    c.parent[0] = -1;
    c.child_start[0] = 1;
    c.child_count[0] = 1;
    {
        let parent_out = Out(c.parent.as_mut_ptr());
        for k in 0..nlevels {
            let (lo, hi) = (levels[k], levels[k + 1]);
            let d = (k + 1) as u16; // wraps past 65535 levels, as the numpy build's uint16
            c.depth[lo + 1..hi + 1].par_iter_mut().for_each(|v| *v = d);
            // the level's folders own consecutive ranges of the next level, in order
            let starts = &mut c.child_start[lo + 1..hi + 1];
            let many = &mut c.child_count[lo + 1..hi + 1];
            let mut next = hi + 1; // id of the next level's first node
            for (j, (s, m)) in starts.iter_mut().zip(many.iter_mut()).enumerate() {
                let r = rows[lo + j] as usize;
                if container[r] {
                    let kids = counts[r] as usize;
                    *s = next as i32;
                    *m = kids as i32;
                    next += kids;
                }
            }
            let starts = &c.child_start[lo + 1..hi + 1];
            let many = &c.child_count[lo + 1..hi + 1];
            (0..hi - lo)
                .into_par_iter()
                .with_min_len(1 << 10)
                .for_each(|j| {
                    let (s, m) = (starts[j] as usize, many[j] as usize);
                    for child in s..s + m {
                        // SAFETY: the level's child ranges are disjoint (and inside the column:
                        // its length is `count`, asserted above)
                        unsafe { parent_out.put(child, (lo + j + 1) as i32) };
                    }
                });
        }
    }
    drop(container);

    // the entry columns; in the metafile folder (when it exists and streams are not counted
    // anyway) streams count
    let in_meta: Vec<bool> = match rows.iter().position(|&r| r as usize == meta_row) {
        Some(j) if !opts.include_ads => {
            let m = j + 1;
            let mut v = vec![false; count];
            v[m] = true;
            for k in m + 1..count {
                v[k] = v[c.parent[k] as usize];
            }
            v
        }
        _ => Vec::new(),
    };
    let root_first = record.par_iter().position_first(|&r| r == ROOT_RECORD);
    c.kind[0] = KIND_DIR;
    gather(c.kind, rows, |r| if r < n { kind[r] } else { KIND_DIR });
    c.flags[0] = flag::VIRTUAL;
    gather(c.flags, rows, |r| {
        if r == root_row {
            0
        } else if r >= n {
            flag::VIRTUAL
        } else if dup[r] && !opts.count_links_each {
            flags[r] | flag::HARDLINK_DUP
        } else {
            flags[r]
        }
    });
    drop(flags);
    drop(dup);
    gather(c.size, rows, |r| {
        if r >= n {
            return 0;
        }
        let s = if kind[r] == KIND_DIR { 0 } else { size[r] };
        if opts.include_ads && record[r] != BADCLUS_RECORD {
            s.wrapping_add(ads_size[r])
        } else {
            s
        }
    });
    drop(size);
    drop(ads_size);
    {
        let alloc_col = &mut *c.alloc;
        alloc_col[1..]
            .par_iter_mut()
            .zip(rows.par_iter())
            .enumerate()
            .with_min_len(1 << 12)
            .for_each(|(j, (out, &r))| {
                let r = r as usize;
                *out = if r >= n {
                    0
                } else {
                    let a = if kind[r] == KIND_FILE { alloc[r] } else { 0 };
                    let counted = opts.include_ads || in_meta.get(j + 1).copied().unwrap_or(false);
                    if counted && record[r] != BADCLUS_RECORD {
                        a.wrapping_add(ads_alloc[r])
                    } else {
                        a
                    }
                };
            });
    }
    drop(alloc);
    drop(ads_alloc);
    drop(in_meta);
    let clip = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
    for (col, src) in [
        (&mut *c.mtime, mtime),
        (&mut *c.ctime, ctime),
        (&mut *c.atime, atime),
    ] {
        gather(col, rows, |r| if r < n { clip(src[r]) } else { 0 });
        if let Some(r) = root_first {
            col[1] = clip(src[r]);
        }
    }
    c.nlink[0] = 1;
    gather(c.nlink, rows, |r| if r < n { nlink[r] } else { 1 });
    drop(nlink);
    gather(c.ino, rows, |r| if r < n { record[r] } else { 0 });
    c.ino[1] = ROOT_RECORD;
    drop(record);
    drop(kind);

    // ------------------------------------------------------------------ 5. subtree totals
    totals(&mut TotalCols::of(c), levels);
    Ok(BuildReport {
        names: n,
        nodes: count,
        orphans,
        cycles,
    })
}

/// `col[j + 1] = value(rows[j])` for every node but node 0, in parallel.
fn gather<T: Send>(col: &mut [T], rows: &[u32], value: impl Fn(usize) -> T + Sync) {
    col[1..]
        .par_iter_mut()
        .zip(rows.par_iter())
        .with_min_len(1 << 12)
        .for_each(|(out, &r)| *out = value(r as usize));
}

/// The columns the totals read and write.
struct TotalCols<'a> {
    kind: &'a [u8],
    flags: &'a [u32],
    size: &'a [i64],
    alloc: &'a [i64],
    child_start: &'a [i32],
    child_count: &'a [i32],
    tot_size: &'a mut [i64],
    tot_alloc: &'a mut [i64],
    tot_files: &'a mut [i32],
    tot_dirs: &'a mut [i32],
}

impl<'a> TotalCols<'a> {
    fn of(c: NodeColumns<'a>) -> TotalCols<'a> {
        TotalCols {
            kind: c.kind,
            flags: c.flags,
            size: c.size,
            alloc: c.alloc,
            child_start: c.child_start,
            child_count: c.child_count,
            tot_size: c.tot_size,
            tot_alloc: c.tot_alloc,
            tot_files: c.tot_files,
            tot_dirs: c.tot_dirs,
        }
    }
}

/// `reaggregate` over the finished columns: every node adds its own size and allocation (unless
/// it is a further hard link) and its subtree's to its parent; files and folders are counted.
/// Levels are contiguous id ranges (level k: ids `levels[k] + 1 .. levels[k + 1] + 1`, node 0
/// above them all), so each level's folders sum their children — all in the next level, whose
/// totals are complete — in parallel, deepest first.
fn totals(c: &mut TotalCols<'_>, levels: &[usize]) {
    let count = c.kind.len();
    let own = |x: usize| {
        let counted = c.flags[x] & flag::HARDLINK_DUP == 0;
        (
            if counted { c.size[x] } else { 0 },
            if counted { c.alloc[x] } else { 0 },
            (c.kind[x] == KIND_FILE) as i32,
            (c.kind[x] == KIND_DIR) as i32,
        )
    };
    // level bounds in ids, node 0 as the level above the root
    let mut bounds: Vec<usize> = vec![0];
    bounds.extend(levels.iter().map(|&l| l + 1));
    debug_assert_eq!(*bounds.last().unwrap(), count);
    for k in (0..bounds.len() - 1).rev() {
        let (lo, hi) = (bounds[k], bounds[k + 1]);
        // this level's totals are written; the next level's (ids >= hi) only read
        let (ts_w, ts_r) = c.tot_size.split_at_mut(hi);
        let (ta_w, ta_r) = c.tot_alloc.split_at_mut(hi);
        let (tf_w, tf_r) = c.tot_files.split_at_mut(hi);
        let (td_w, td_r) = c.tot_dirs.split_at_mut(hi);
        let (starts, many) = (c.child_start, c.child_count);
        ts_w[lo..]
            .par_iter_mut()
            .zip(&mut ta_w[lo..])
            .zip(&mut tf_w[lo..])
            .zip(&mut td_w[lo..])
            .enumerate()
            .with_min_len(1 << 10)
            .for_each(|(j, (((ts, ta), tf), td))| {
                let p = lo + j;
                let m = many[p].max(0) as usize;
                let mut acc = (0i64, 0i64, 0i32, 0i32);
                if m > 0 {
                    let s = starts[p] as usize;
                    for ch in s..s + m {
                        let (os, oa, of, od) = own(ch);
                        let at = ch - hi;
                        acc.0 = acc.0.wrapping_add(ts_r[at].wrapping_add(os));
                        acc.1 = acc.1.wrapping_add(ta_r[at].wrapping_add(oa));
                        acc.2 = acc.2.wrapping_add(tf_r[at].wrapping_add(of));
                        acc.3 = acc.3.wrapping_add(td_r[at].wrapping_add(od));
                    }
                }
                (*ts, *ta, *tf, *td) = acc;
            });
    }
}

/// Rows of the breadth-first order and where each level starts.
struct Topo {
    rows: Vec<u32>,     // node id - 1 -> row
    levels: Vec<usize>, // level k is rows[levels[k]..levels[k + 1]]
    counts: Vec<u32>,   // children of every row
}

/// Children of every row in row order, then rows level by level from `root`.
fn breadth_first(pdir: &[i32], container: &[bool], root: usize) -> Topo {
    let total = pdir.len();
    let counts_a: Vec<AtomicU32> = (0..total)
        .into_par_iter()
        .map(|_| AtomicU32::new(0))
        .collect();
    pdir.par_iter().with_min_len(1 << 14).for_each(|&p| {
        if p >= 0 {
            counts_a[p as usize].fetch_add(1, Ordering::Relaxed);
        }
    });
    let counts: Vec<u32> = counts_a
        .into_par_iter()
        .map(AtomicU32::into_inner)
        .collect();
    // ends of every group; filled backwards they become the group starts
    let mut starts: Vec<u32> = Vec::with_capacity(total);
    let mut acc = 0u32;
    for &c in &counts {
        acc += c;
        starts.push(acc);
    }
    let children = acc as usize;
    let mut order = vec![0u32; children];
    for i in (0..total).rev() {
        let p = pdir[i];
        if p >= 0 {
            let e = &mut starts[p as usize];
            *e -= 1;
            order[*e as usize] = i as u32;
        }
    }
    let mut rows: Vec<u32> = Vec::with_capacity(children + 1);
    rows.push(root as u32);
    let mut levels = vec![0, 1];
    let mut frontier: Vec<u32> = vec![root as u32];
    loop {
        // offsets of every frontier folder's children in the next level
        let mut offs = Vec::with_capacity(frontier.len());
        let mut sum = 0usize;
        for &f in &frontier {
            offs.push(sum);
            sum += counts[f as usize] as usize;
        }
        if sum == 0 {
            break;
        }
        let base = rows.len();
        rows.resize(base + sum, 0);
        let dst = Out(rows[base..].as_mut_ptr());
        frontier
            .par_iter()
            .zip(&offs)
            .with_min_len(1 << 10)
            .for_each(|(&f, &o)| {
                let s = starts[f as usize] as usize;
                let c = counts[f as usize] as usize;
                // SAFETY: the frontier's child ranges are disjoint and inside the new level
                unsafe { dst.copy_in(o, &order[s..s + c]) };
            });
        levels.push(rows.len());
        frontier = rows[base..]
            .par_iter()
            .copied()
            .filter(|&r| container[r as usize])
            .collect();
    }
    Topo {
        rows,
        levels,
        counts,
    }
}

/// One row per parent cycle among `stuck` (rows with a parent, not reached from the root): the
/// smallest row of each cycle — every row on the cycles when one is longer than [`CYCLE_LIMIT`].
fn cycle_breakers(pdir: &[i32], stuck: &[u32]) -> Vec<u32> {
    let total = pdir.len();
    let mut alive = vec![false; total];
    let mut indeg = vec![0u32; total];
    for &s in stuck {
        alive[s as usize] = true;
        indeg[pdir[s as usize] as usize] += 1;
    }
    let mut queue: Vec<u32> = stuck
        .iter()
        .copied()
        .filter(|&s| indeg[s as usize] == 0)
        .collect();
    while let Some(x) = queue.pop() {
        alive[x as usize] = false; // a tree hanging off a cycle: peel it
        let p = pdir[x as usize] as usize;
        indeg[p] -= 1;
        if alive[p] && indeg[p] == 0 {
            queue.push(p as u32);
        }
    }
    let cyc: Vec<u32> = stuck
        .iter()
        .copied()
        .filter(|&i| alive[i as usize])
        .collect();
    let mut seen = vec![false; total];
    let mut mins = Vec::new();
    let mut longest = 0usize;
    for &start in &cyc {
        if seen[start as usize] {
            continue;
        }
        let (mut x, mut len, mut min) = (start, 0usize, start);
        loop {
            seen[x as usize] = true;
            len += 1;
            min = min.min(x);
            x = pdir[x as usize] as u32;
            if x == start {
                break;
            }
        }
        longest = longest.max(len);
        mins.push(min);
    }
    if longest > CYCLE_LIMIT {
        let mut all = cyc;
        all.sort_unstable();
        return all;
    }
    mins.sort_unstable();
    mins
}

/// Raw extension of a file name with the scanner's rule: the bytes after the last dot when the
/// dot is not the first byte and 1–16 characters (at most 64 bytes) follow it.
#[inline]
fn raw_ext(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    let ext = &name[dot + 1..];
    if dot == 0 || ext.is_empty() || ext.len() > 64 {
        return None;
    }
    if ext.len() > 16 && ext.iter().filter(|&&b| b & 0xC0 != 0x80).count() > 16 {
        return None;
    }
    Some(ext)
}

/// A raw extension's sort key: up to 7 bytes little-endian with the length in the top byte
/// (sorted as integers); longer ones as the length byte and the bytes (sorted as bytes).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum ExtKey {
    Short(u64),
    Long(Vec<u8>),
}

fn ext_key(ext: &[u8]) -> ExtKey {
    if ext.len() <= 7 {
        let mut v = [0u8; 8];
        v[..ext.len()].copy_from_slice(ext);
        ExtKey::Short(u64::from_le_bytes(v) | (ext.len() as u64) << 56)
    } else {
        let mut v = Vec::with_capacity(ext.len() + 1);
        v.push(ext.len() as u8);
        v.extend_from_slice(ext);
        ExtKey::Long(v)
    }
}

fn ext_bytes(key: &ExtKey) -> Vec<u8> {
    match key {
        ExtKey::Short(k) => k.to_le_bytes()[..(k >> 56) as usize].to_vec(),
        ExtKey::Long(v) => v[1..].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(raw_ext(b"a.txt"), Some(&b"txt"[..]));
        assert_eq!(raw_ext(b".hidden"), None);
        assert_eq!(raw_ext(b"trailing."), None);
        assert_eq!(raw_ext(b"noext"), None);
        assert_eq!(raw_ext(b"x.tar.gz"), Some(&b"gz"[..]));
        assert_eq!(
            raw_ext(b"y.SixteenCharsExtX"),
            Some(&b"SixteenCharsExtX"[..])
        ); // 16
        assert_eq!(raw_ext(b"y.SeventeenCharsExt"), None); // 17 characters
        let wide = "\u{10000}".repeat(5); // 5 characters in 20 bytes
        let name = format!("b.{wide}");
        assert_eq!(raw_ext(name.as_bytes()), Some(wide.as_bytes()));
        let many = format!("c.{}", "é".repeat(17));
        assert_eq!(raw_ext(many.as_bytes()), None);
        assert!(ext_key(b"zip") < ext_key(b"txt")); // integers of the little-endian bytes
        assert!(ext_key(b"zz") < ext_key(b"aaa")); // the length is the top byte
        assert!(ext_key(b"abcdefg") < ext_key(b"abcdefgh"));
        assert_eq!(ext_bytes(&ext_key(b"abc")), b"abc");
        assert_eq!(ext_bytes(&ext_key(b"abcdefghij")), b"abcdefghij");
    }

    #[test]
    fn breakers_of_cycles() {
        // 0 -> 1 -> 2 -> 0 (a cycle), 3 -> 1 (hangs off it), 4 <-> 5, 6 -> 6 is not stuck
        let pdir = [1, 2, 0, 1, 5, 4, 6];
        let stuck = [0, 1, 2, 3, 4, 5];
        assert_eq!(cycle_breakers(&pdir, &stuck), vec![0, 4]);
        // one cycle longer than the limit: every row on it
        let n = CYCLE_LIMIT + 1;
        let pdir: Vec<i32> = (0..n as i32).map(|i| (i + 1) % n as i32).collect();
        let stuck: Vec<u32> = (0..n as u32).collect();
        assert_eq!(cycle_breakers(&pdir, &stuck).len(), n);
        let n = CYCLE_LIMIT;
        let pdir: Vec<i32> = (0..n as i32).map(|i| (i + 1) % n as i32).collect();
        let stuck: Vec<u32> = (0..n as u32).collect();
        assert_eq!(cycle_breakers(&pdir, &stuck), vec![0]);
    }

    #[test]
    fn levels_in_row_order() {
        // root 4; 0 and 2 under the root, 1 and 3 under 0; 3 is not a folder
        let pdir = [4, 0, 4, 0, -1];
        let container = [true, true, false, false, true];
        let t = breadth_first(&pdir, &container, 4);
        assert_eq!(t.rows, vec![4, 0, 2, 1, 3]);
        assert_eq!(t.levels, vec![0, 1, 3, 5]);
        assert_eq!(t.counts, vec![2, 0, 0, 0, 2]);
    }
}
