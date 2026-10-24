//! The node store's columns from an `MftTable` (docs/native.md 2.1, docs/memory.md §6).
//!
//! `mft_build` is the native twin of `spaceknife.scan.mft._table_to_store_numpy`: every column
//! (node order, parent links, depths, child ranges, flags, sizes, times, extensions, the
//! `\0name` image, the four subtree totals) comes out exactly as the numpy build makes it; the
//! tests compare both column by column. The steps, each over all rows in parallel:
//!
//! 1. parents: the first row of every record (later rows are its other names, `HARDLINK_DUP`),
//!    folders by record, each row's parent row, metafiles under `$NTFS 元数据`, unknown parents,
//!    non-folders and folders that are their own parent under `$孤立文件`;
//! 2. breadth-first order from the root (children grouped in row order); parent cycles not
//!    reached from the root are broken under the orphans (the smallest row of each cycle, or every
//!    row on a cycle when one is longer than 4096, as the numpy label propagation gives up);
//! 3. extensions: the distinct raw extensions of all file names go to Python (`ext_ids`), which
//!    lower-cases them and numbers them in the order the numpy build does;
//! 4. the columns in node order, straight into fresh numpy arrays the store adopts as they are,
//!    the `\0name` image laid out once; with `consume` each table column is deleted from the
//!    table as soon as nothing needs it any more;
//! 5. subtree totals level by level from the deepest (what `NodeStore.reaggregate` computes).
//!
//! Nothing here runs Python per row; Python sees only the distinct extensions.

use std::sync::atomic::{AtomicU32, Ordering};

use numpy::{Element, PyArray1, PyArrayMethods};
use pyo3::buffer::{Element as BufElement, PyBuffer};
use pyo3::exceptions::{PyMemoryError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use rayon::prelude::*;

const ROOT_RECORD: u64 = 5;
const BADCLUS_RECORD: u64 = 8;
const FIRST_USER_RECORD: u64 = 16;
const KIND_FILE: u8 = 0;
const KIND_DIR: u8 = 1;
const F_HARDLINK_DUP: u32 = 1 << 2;
const F_VIRTUAL: u32 = 1 << 17;
const OFF_NARROW_LIMIT: usize = 1 << 32; // name offsets fit uint32 below this many bytes of names
const CYCLE_LABEL_ROUNDS: usize = 4096; // the numpy build's label propagation rounds
const NONE: u32 = u32::MAX;

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

    /// SAFETY: `i` in bounds, written before and by nobody now.
    #[inline]
    unsafe fn get(self, i: usize) -> T
    where
        T: Copy,
    {
        unsafe { self.0.add(i).read() }
    }
}

fn oom(what: &str) -> PyErr {
    PyMemoryError::new_err(format!("out of memory building the MFT tree ({what})"))
}

fn vec_filled<T: Clone>(len: usize, v: T, what: &str) -> PyResult<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| oom(what))?;
    out.resize(len, v);
    Ok(out)
}

/// One input column, borrowed through the buffer protocol (C-contiguous, of type `T`).
struct Col<T: BufElement> {
    buf: Option<PyBuffer<T>>,
    len: usize,
}

impl<T: BufElement> Col<T> {
    fn get(table: &Bound<'_, PyDict>, key: &str) -> PyResult<Col<T>> {
        let obj = table
            .get_item(key)?
            .ok_or_else(|| PyValueError::new_err(format!("table has no column {key:?}")))?;
        let buf = PyBuffer::<T>::get(&obj)
            .map_err(|e| PyValueError::new_err(format!("column {key:?}: {e}")))?;
        if !buf.is_c_contiguous() || buf.dimensions() > 1 {
            return Err(PyValueError::new_err(format!(
                "column {key:?} must be a contiguous 1-d array"
            )));
        }
        let len = buf.item_count();
        Ok(Col {
            buf: Some(buf),
            len,
        })
    }

    fn slice(&self) -> &[T] {
        match &self.buf {
            Some(b) if self.len > 0 => {
                // SAFETY: the exporter stays alive and unresized while the buffer is held; the
                // build only reads it.
                unsafe { std::slice::from_raw_parts(b.buf_ptr() as *const T, self.len) }
            }
            _ => &[],
        }
    }
}

/// Release a column and, with `consume`, delete it from the table (its memory goes now).
fn done<T: BufElement>(
    table: &Bound<'_, PyDict>,
    key: &str,
    col: &mut Col<T>,
    consume: bool,
) -> PyResult<()> {
    col.buf = None;
    col.len = 0;
    if consume && table.contains(key)? {
        table.del_item(key)?;
    }
    Ok(())
}

fn zeros<'py, T: Element>(py: Python<'py>, len: usize) -> Bound<'py, PyArray1<T>> {
    PyArray1::<T>::zeros(py, len, false)
}

fn out_of<T: Element>(a: &Bound<'_, PyArray1<T>>) -> Out<T> {
    Out(a.data())
}

/// The build's own rayon pool, as many threads as the global pool would have. It goes when the
/// build returns, threads and all: the backend otherwise runs nothing on rayon's global pool, whose
/// threads would stay in it for good after the first MFT scan (about 0.1 MB each, docs/memory.md
/// §6). When it cannot be made, the work runs on the global pool.
fn build_pool() -> Option<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .thread_name(|i| format!("sk-mft-build-{i}"))
        .build()
        .ok()
}

/// `f` without the GIL, on the build's pool.
fn par<R: Send>(
    py: Python<'_>,
    pool: &Option<rayon::ThreadPool>,
    f: impl FnOnce() -> R + Send,
) -> R {
    py.detach(|| match pool {
        Some(p) => p.install(f),
        None => f(),
    })
}

/// Rows of the breadth-first order and where each level starts.
struct Topo {
    rows: Vec<u32>,     // node id - 1 -> row
    levels: Vec<usize>, // level k is rows[levels[k]..levels[k + 1]]
    counts: Vec<u32>,   // children of every row
}

/// Children of every row in row order, then rows level by level from `root`.
fn breadth_first(pdir: &[i32], container: &[bool], root: usize) -> PyResult<Topo> {
    let total = pdir.len();
    let counts_a: Vec<AtomicU32> = {
        let mut v = Vec::new();
        v.try_reserve_exact(total)
            .map_err(|_| oom("child counts"))?;
        v.extend((0..total).map(|_| AtomicU32::new(0)));
        v
    };
    pdir.par_iter().with_min_len(1 << 14).for_each(|&p| {
        if p >= 0 {
            counts_a[p as usize].fetch_add(1, Ordering::Relaxed);
        }
    });
    let counts: Vec<u32> = counts_a.into_iter().map(AtomicU32::into_inner).collect();
    // ends[p] = one past p's group; filled backwards it becomes the group starts
    let mut starts = vec_filled(total, 0u32, "child groups")?;
    let mut acc = 0u32;
    for (s, &c) in starts.iter_mut().zip(&counts) {
        acc += c;
        *s = acc;
    }
    let children = acc as usize;
    let mut order = vec_filled(children, 0u32, "child order")?;
    for i in (0..total).rev() {
        let p = pdir[i];
        if p >= 0 {
            let e = &mut starts[p as usize];
            *e -= 1;
            order[*e as usize] = i as u32;
        }
    }
    let mut rows: Vec<u32> = Vec::new();
    rows.try_reserve_exact(children + 1)
        .map_err(|_| oom("node order"))?;
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
                for (k, &kid) in order[s..s + c].iter().enumerate() {
                    // SAFETY: the frontier's child ranges are disjoint and inside the new level
                    unsafe { dst.put(o + k, kid) };
                }
            });
        levels.push(rows.len());
        frontier = rows[base..]
            .par_iter()
            .copied()
            .filter(|&r| container[r as usize])
            .collect();
    }
    Ok(Topo {
        rows,
        levels,
        counts,
    })
}

/// One row per parent cycle among `stuck` (rows with a parent, not reached from the root): the
/// smallest row of each cycle, or every row on a cycle when one is longer than the numpy
/// build's label propagation runs (it then returns them all).
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
    let cyc: Vec<u32> = (0..total as u32).filter(|&i| alive[i as usize]).collect();
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
    if longest > CYCLE_LABEL_ROUNDS {
        return cyc;
    }
    mins.sort_unstable();
    mins
}

/// Raw extension of a file name with scan.worker's rule: the bytes after the last dot when the
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

/// The numpy build's key of a raw extension: up to 7 bytes little-endian with the length in the
/// top byte (sorted as integers); longer ones as the length byte and the bytes (sorted as bytes).
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

/// Build the node columns of the store from an `MftTable` (see the module docs). Returns
/// `{"columns": {...}, "names": image, "count": nodes, "cycles": broken cycles, "orphans": n}`.
#[pyfunction]
#[pyo3(signature = (table, root_name, meta_name, orphan_name, ext_ids, include_ads = false, count_links_each = false, consume = false))]
#[allow(clippy::too_many_arguments)]
fn mft_build<'py>(
    py: Python<'py>,
    table: &Bound<'py, PyDict>,
    root_name: &[u8],
    meta_name: &[u8],
    orphan_name: &[u8],
    ext_ids: &Bound<'py, PyAny>,
    include_ads: bool,
    count_links_each: bool,
    consume: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let pool = build_pool();
    let mut rec_c = Col::<u64>::get(table, "record")?;
    let mut par_c = Col::<u64>::get(table, "parent")?;
    let mut kind_c = Col::<u8>::get(table, "kind")?;
    let n = rec_c.len;
    let mut names_c = Col::<u8>::get(table, "names")?;
    let mut off_c = Col::<u32>::get(table, "name_off")?;
    let mut len_c = Col::<u16>::get(table, "name_len")?;
    for (key, len) in [
        ("parent", par_c.len),
        ("kind", kind_c.len),
        ("name_off", off_c.len),
        ("name_len", len_c.len),
    ] {
        if len != n {
            return Err(PyValueError::new_err(format!(
                "column {key:?} has {len} rows, not {n}"
            )));
        }
    }
    if n >= (i32::MAX as usize) - 4 {
        return Err(PyValueError::new_err("too many rows for 32-bit node ids"));
    }
    for (name, what) in [
        (root_name, "root"),
        (meta_name, "metafile folder"),
        (orphan_name, "orphan folder"),
    ] {
        if name.len() > u16::MAX as usize {
            return Err(PyValueError::new_err(format!("{what} name too long")));
        }
    }
    let blob_len = names_c.len;
    {
        let (off, len) = (off_c.slice(), len_c.slice());
        let bad = par(py, &pool, || {
            off.par_iter()
                .zip(len)
                .any(|(&o, &l)| o as usize + l as usize > blob_len)
        });
        if bad {
            return Err(PyValueError::new_err("name offsets out of range"));
        }
    }
    let (meta_row, orphan_row, root_row) = (n, n + 1, n + 2);
    let total = n + 3;

    // ---------------------------------------------------------------- 1. parents
    let rec = rec_c.slice();
    let kind = kind_c.slice();
    let nrec = par(py, &pool, || {
        rec.par_iter().copied().max().map_or(0, |m| m as usize + 1)
    });
    let nrec = nrec.max(ROOT_RECORD as usize + 1);
    let (dup, container, mut pdir) = par(py, &pool, || -> PyResult<_> {
        let mut first: Vec<AtomicU32> = Vec::new();
        first.try_reserve_exact(nrec).map_err(|_| oom("records"))?;
        first.extend((0..nrec).map(|_| AtomicU32::new(NONE)));
        rec.par_iter()
            .enumerate()
            .with_min_len(1 << 14)
            .for_each(|(i, &r)| {
                first[r as usize].fetch_min(i as u32, Ordering::Relaxed);
            });
        let first: Vec<u32> = first.into_iter().map(AtomicU32::into_inner).collect();
        let dup: Vec<bool> = rec
            .par_iter()
            .enumerate()
            .map(|(i, &r)| first[r as usize] != i as u32)
            .collect();
        let mut container: Vec<bool> = Vec::new();
        container
            .try_reserve_exact(total)
            .map_err(|_| oom("folders"))?;
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
        let par = par_c.slice();
        let mut pdir: Vec<i32> = Vec::new();
        pdir.try_reserve_exact(total).map_err(|_| oom("parents"))?;
        pdir.par_extend((0..n).into_par_iter().map(|i| {
            let p = par[i];
            let mut d = if p < nrec as u64 {
                dir_row[p as usize]
            } else {
                -1
            };
            if d < 0 || (container[i] && d == i as i32) {
                d = orphan_row as i32; // unknown parent, not a folder, or a folder in itself
            }
            let r = rec[i];
            if r < FIRST_USER_RECORD && r != ROOT_RECORD {
                d = meta_row as i32;
            }
            if r == ROOT_RECORD {
                d = -2; // the root's own "." entry is the root node itself
            }
            d
        }));
        let used = |row: usize| pdir.par_iter().any(|&d| d == row as i32);
        let (meta_used, orphan_used) = (used(meta_row), used(orphan_row));
        pdir.push(if meta_used { root_row as i32 } else { -1 });
        pdir.push(if orphan_used { root_row as i32 } else { -1 });
        pdir.push(-1);
        Ok((dup, container, pdir))
    })?;
    done(table, "parent", &mut par_c, consume)?;

    // ---------------------------------------------------------------- 2. breadth-first order
    let (topo, cycles) = par(py, &pool, || -> PyResult<_> {
        let mut topo = breadth_first(&pdir, &container, root_row)?;
        let expected = 1 + pdir.par_iter().filter(|&&d| d >= 0).count();
        let mut cycles = 0usize;
        if topo.rows.len() < expected {
            let mut seen = vec![false; total];
            for &r in &topo.rows {
                seen[r as usize] = true;
            }
            let stuck: Vec<u32> = (0..total as u32)
                .filter(|&i| !seen[i as usize] && pdir[i as usize] >= 0)
                .collect();
            let breakers = cycle_breakers(&pdir, &stuck);
            cycles = breakers.len();
            for b in breakers {
                pdir[b as usize] = orphan_row as i32;
            }
            pdir[orphan_row] = root_row as i32;
            drop(topo);
            topo = breadth_first(&pdir, &container, root_row)?;
        }
        Ok((topo, cycles))
    })?;
    let orphans = if pdir[orphan_row] >= 0 {
        topo.counts[orphan_row] as usize
    } else {
        0
    };
    drop(pdir);
    let rows = &topo.rows;
    let count = rows.len() + 1;
    let nlevels = topo.levels.len() - 1;

    let cols = PyDict::new(py);
    let parent_a = zeros::<i32>(py, count);
    let depth_a = zeros::<u16>(py, count);
    let first_a = zeros::<i32>(py, count);
    let many_a = zeros::<i32>(py, count);
    {
        let (parent, depth, first, many) = (
            out_of(&parent_a),
            out_of(&depth_a),
            out_of(&first_a),
            out_of(&many_a),
        );
        let (levels, counts, container) = (&topo.levels, &topo.counts, &container);
        par(py, &pool, || {
            // SAFETY (all writes below): every node id is written by exactly one worker
            unsafe {
                parent.put(0, -1);
                first.put(0, 1);
                many.put(0, 1);
            }
            for k in 0..nlevels {
                let (lo, hi) = (levels[k], levels[k + 1]);
                let d = (k + 1) as u16; // wraps past 65535 levels, as the numpy build's uint16
                (lo..hi)
                    .into_par_iter()
                    .with_min_len(1 << 12)
                    .for_each(|j| unsafe {
                        depth.put(j + 1, d);
                    });
                // folders of this level, in order, own consecutive ranges of the next level
                let folders: Vec<usize> = (lo..hi)
                    .into_par_iter()
                    .filter(|&j| container[rows[j] as usize])
                    .collect();
                let mut at = Vec::with_capacity(folders.len());
                let mut next = hi + 1; // id of the next level's first node
                for &j in &folders {
                    at.push(next);
                    next += counts[rows[j] as usize] as usize;
                }
                folders
                    .par_iter()
                    .zip(&at)
                    .with_min_len(1 << 10)
                    .for_each(|(&j, &start)| {
                        let c = counts[rows[j] as usize] as usize;
                        unsafe {
                            first.put(j + 1, start as i32);
                            many.put(j + 1, c as i32);
                        }
                        for child in start..start + c {
                            unsafe { parent.put(child, (j + 1) as i32) };
                        }
                    });
            }
        });
    }
    drop(container);
    cols.set_item("parent", &parent_a)?;
    cols.set_item("depth", &depth_a)?;
    cols.set_item("child_start", &first_a)?;
    cols.set_item("child_count", &many_a)?;

    // ---------------------------------------------------------------- 3. extensions
    let (names, off, len) = (names_c.slice(), off_c.slice(), len_c.slice());
    let name_of = |r: usize| &names[off[r] as usize..off[r] as usize + len[r] as usize];
    let keys: Vec<ExtKey> = par(py, &pool, || {
        let mut keys: Vec<ExtKey> = (0..n)
            .into_par_iter()
            .with_min_len(1 << 12)
            .fold(std::collections::HashSet::new, |mut set, r| {
                if kind[r] == KIND_FILE
                    && let Some(e) = raw_ext(name_of(r))
                {
                    set.insert(ext_key(e));
                }
                set
            })
            .reduce(std::collections::HashSet::new, |mut a, b| {
                if a.len() < b.len() {
                    return b.into_iter().chain(a).collect();
                }
                a.extend(b);
                a
            })
            .into_iter()
            .collect();
        keys.par_sort_unstable(); // Short (as integers) before Long (as bytes)
        keys
    });
    let raws = PyList::empty(py);
    for k in &keys {
        raws.append(PyBytes::new(py, &ext_bytes(k)))?;
    }
    let ids: Vec<u16> = ext_ids.call1((raws,))?.extract()?;
    if ids.len() != keys.len() {
        return Err(PyValueError::new_err(
            "ext_ids returned the wrong number of ids",
        ));
    }
    let index: std::collections::HashMap<ExtKey, u16> = keys.into_iter().zip(ids).collect();

    // ---------------------------------------------------------------- 4. names and extensions
    let extra = |row: usize| -> &[u8] {
        if row == root_row {
            root_name
        } else if row == meta_row {
            meta_name
        } else {
            orphan_name
        }
    };
    let name_len_a = zeros::<u16>(py, count);
    let ext_a = zeros::<u16>(py, count);
    {
        let (nl, ex) = (out_of(&name_len_a), out_of(&ext_a));
        par(py, &pool, || {
            rows.par_iter()
                .enumerate()
                .with_min_len(1 << 12)
                .for_each(|(j, &r)| {
                    let r = r as usize;
                    let (l, e) = if r >= n {
                        (extra(r).len() as u16, 0)
                    } else {
                        let e = if kind[r] == KIND_FILE {
                            raw_ext(name_of(r)).map_or(0, |e| index[&ext_key(e)])
                        } else {
                            0
                        };
                        (len[r], e)
                    };
                    // SAFETY: node j + 1 is this worker's
                    unsafe {
                        nl.put(j + 1, l);
                        ex.put(j + 1, e);
                    }
                });
        });
    }
    // the \0name image in node order: each name after its NUL, node 0's empty name first
    let image_len: usize;
    let name_off_a: Bound<'py, PyAny>;
    let image_a = {
        // SAFETY: a fresh array nobody else sees
        let lens = unsafe { name_len_a.as_slice()? };
        let mut starts: Vec<usize> = vec_filled(count, 0usize, "name offsets")?;
        let mut acc = 0usize;
        for (s, &l) in starts.iter_mut().zip(lens) {
            *s = acc + 1;
            acc += l as usize + 1;
        }
        image_len = acc;
        let image = zeros::<u8>(py, image_len);
        let img = out_of(&image);
        par(py, &pool, || {
            rows.par_iter()
                .enumerate()
                .with_min_len(1 << 12)
                .for_each(|(j, &r)| {
                    let r = r as usize;
                    let src = if r >= n { extra(r) } else { name_of(r) };
                    let at = starts[j + 1];
                    // SAFETY: the names' ranges of the image are disjoint
                    unsafe { img.copy_in(at, src) };
                });
        });
        if image_len >= OFF_NARROW_LIMIT {
            let a = zeros::<i64>(py, count);
            let o = out_of(&a);
            par(py, &pool, || {
                starts
                    .par_iter()
                    .enumerate()
                    .for_each(|(k, &s)| unsafe { o.put(k, s as i64) })
            });
            name_off_a = a.into_any();
        } else {
            let a = zeros::<u32>(py, count);
            let o = out_of(&a);
            par(py, &pool, || {
                starts
                    .par_iter()
                    .enumerate()
                    .for_each(|(k, &s)| unsafe { o.put(k, s as u32) })
            });
            name_off_a = a.into_any();
        }
        image
    };
    drop(index);
    done(table, "names", &mut names_c, consume)?;
    done(table, "name_off", &mut off_c, consume)?;
    done(table, "name_len", &mut len_c, consume)?;
    cols.set_item("name_off", name_off_a)?;
    cols.set_item("name_len", &name_len_a)?;
    cols.set_item("ext", &ext_a)?;

    // ---------------------------------------------------------------- 4b. the entry columns
    // in the metafile folder (when it exists and streams are not counted anyway): streams count
    let meta_node = rows
        .iter()
        .position(|&r| r as usize == meta_row)
        .map(|j| j + 1);
    let in_meta: Vec<bool> = match meta_node {
        Some(m) if !include_ads => {
            // SAFETY: filled above, read only from here
            let parent = unsafe { parent_a.as_slice()? };
            let mut v = vec_filled(count, false, "metafile folder")?;
            v[m] = true;
            for k in m + 1..count {
                v[k] = v[parent[k] as usize];
            }
            v
        }
        _ => Vec::new(),
    };
    let root_first = par(py, &pool, || {
        rec.par_iter().position_first(|&r| r == ROOT_RECORD)
    });

    macro_rules! gather {
        ($ty:ty, $fill:expr, |$j:ident, $r:ident| $body:expr) => {{
            let a = zeros::<$ty>(py, count);
            let o = out_of(&a);
            par(py, &pool, || {
                // SAFETY: node j + 1 is this worker's
                unsafe { o.put(0, $fill) };
                rows.par_iter()
                    .enumerate()
                    .with_min_len(1 << 12)
                    .for_each(|($j, &row)| {
                        let $r = row as usize;
                        let v: $ty = if $r >= n { $fill } else { $body };
                        unsafe { o.put($j + 1, v) };
                    });
            });
            a
        }};
    }

    let kind_a = gather!(u8, KIND_DIR, |_j, r| kind[r]);
    let flags_a = {
        let mut fl_c = Col::<u32>::get(table, "flags")?;
        let fl = fl_c.slice();
        let flags_a = gather!(u32, 0, |_j, r| {
            let f = fl[r];
            if dup[r] && !count_links_each {
                f | F_HARDLINK_DUP
            } else {
                f
            }
        });
        // SAFETY: a fresh array
        let flags = unsafe { flags_a.as_slice_mut()? };
        for (j, &r) in rows.iter().enumerate() {
            if r as usize >= n {
                flags[j + 1] = if r as usize == root_row { 0 } else { F_VIRTUAL };
            }
        }
        flags[0] = F_VIRTUAL;
        done(table, "flags", &mut fl_c, consume)?;
        cols.set_item("flags", &flags_a)?;
        flags_a
    };
    drop(dup);
    let size_a = {
        let mut size_c = Col::<i64>::get(table, "size")?;
        let mut ads_c = if include_ads {
            Some(Col::<i64>::get(table, "ads_size")?)
        } else {
            None
        };
        let (sz, ads) = (size_c.slice(), ads_c.as_ref().map(Col::slice));
        let size_a = gather!(i64, 0, |_j, r| {
            let s = if kind[r] == KIND_DIR { 0 } else { sz[r] };
            match ads {
                Some(a) if rec[r] != BADCLUS_RECORD => s.wrapping_add(a[r]),
                _ => s,
            }
        });
        done(table, "size", &mut size_c, consume)?;
        if let Some(c) = ads_c.as_mut() {
            done(table, "ads_size", c, consume)?;
        }
        cols.set_item("size", &size_a)?;
        size_a
    };
    let alloc_a = {
        let mut alloc_c = Col::<i64>::get(table, "alloc")?;
        let need_ads = include_ads || !in_meta.is_empty();
        let mut ads_c = if need_ads {
            Some(Col::<i64>::get(table, "ads_alloc")?)
        } else {
            None
        };
        let (al, ads) = (alloc_c.slice(), ads_c.as_ref().map(Col::slice));
        let in_meta = &in_meta;
        let alloc_a = gather!(i64, 0, |j, r| {
            let a = if kind[r] != KIND_FILE { 0 } else { al[r] };
            let counted = include_ads || in_meta.get(j + 1).copied().unwrap_or(false);
            match ads {
                Some(s) if counted && rec[r] != BADCLUS_RECORD => a.wrapping_add(s[r]),
                _ => a,
            }
        });
        done(table, "alloc", &mut alloc_c, consume)?;
        if let Some(c) = ads_c.as_mut() {
            done(table, "ads_alloc", c, consume)?;
        }
        cols.set_item("alloc", &alloc_a)?;
        alloc_a
    };
    if consume {
        for key in ["ads_size", "ads_alloc"] {
            if table.contains(key)? {
                table.del_item(key)?;
            }
        }
    }
    drop(in_meta);
    for key in ["mtime", "ctime", "atime"] {
        let mut t_c = Col::<i64>::get(table, key)?;
        let t = t_c.slice();
        let a = gather!(u32, 0, |_j, r| t[r].clamp(0, u32::MAX as i64) as u32);
        if let Some(r) = root_first {
            // SAFETY: a fresh array
            let v = unsafe { a.as_slice_mut()? };
            v[1] = t[r].clamp(0, u32::MAX as i64) as u32;
        }
        done(table, key, &mut t_c, consume)?;
        cols.set_item(key, &a)?;
    }
    {
        let mut nl_c = Col::<u16>::get(table, "nlink")?;
        let nl = nl_c.slice();
        let a = gather!(u16, 1, |_j, r| nl[r]);
        done(table, "nlink", &mut nl_c, consume)?;
        cols.set_item("nlink", &a)?;
    }
    {
        let a = gather!(u64, 0, |_j, r| rec[r]);
        // SAFETY: a fresh array
        let v = unsafe { a.as_slice_mut()? };
        v[1] = ROOT_RECORD;
        cols.set_item("ino", &a)?;
    }
    done(table, "kind", &mut kind_c, consume)?;
    done(table, "record", &mut rec_c, consume)?;
    drop(topo);

    // ---------------------------------------------------------------- 5. subtree totals
    let tot_size_a = zeros::<i64>(py, count);
    let tot_alloc_a = zeros::<i64>(py, count);
    let tot_files_a = zeros::<i32>(py, count);
    let tot_dirs_a = zeros::<i32>(py, count);
    {
        // SAFETY: fresh arrays, read only (the four totals are written through `Out`)
        let (kind, flags, size, alloc, depth, parent, first, many) = unsafe {
            (
                kind_a.as_slice()?,
                flags_a.as_slice()?,
                size_a.as_slice()?,
                alloc_a.as_slice()?,
                depth_a.as_slice()?,
                parent_a.as_slice()?,
                first_a.as_slice()?,
                many_a.as_slice()?,
            )
        };
        let outs = (
            out_of(&tot_size_a),
            out_of(&tot_alloc_a),
            out_of(&tot_files_a),
            out_of(&tot_dirs_a),
        );
        par(py, &pool, || {
            totals(
                kind, flags, size, alloc, depth, parent, first, many, nlevels, outs,
            )
        });
    }
    cols.set_item("kind", &kind_a)?;
    cols.set_item("tot_size", &tot_size_a)?;
    cols.set_item("tot_alloc", &tot_alloc_a)?;
    cols.set_item("tot_files", &tot_files_a)?;
    cols.set_item("tot_dirs", &tot_dirs_a)?;

    let out = PyDict::new(py);
    out.set_item("columns", cols)?;
    out.set_item("names", image_a)?;
    out.set_item("count", count)?;
    out.set_item("cycles", cycles)?;
    out.set_item("orphans", orphans)?;
    Ok(out)
}

/// `NodeStore.reaggregate` over the finished columns: every node adds its own size and
/// allocation (unless it is a further hard link) and its subtree's to its parent; files and
/// folders are counted. Levels are contiguous id ranges, so each level's folders sum their
/// children in parallel, deepest first. Past 65535 levels the depth column has wrapped; then the
/// totals follow the wrapped depths exactly as `reaggregate` does (one depth value at a time).
#[allow(clippy::too_many_arguments)]
fn totals(
    kind: &[u8],
    flags: &[u32],
    size: &[i64],
    alloc: &[i64],
    depth: &[u16],
    parent: &[i32],
    first: &[i32],
    many: &[i32],
    nlevels: usize,
    (ts, ta, tf, td): (Out<i64>, Out<i64>, Out<i32>, Out<i32>),
) {
    let count = kind.len();
    let own = |c: usize| {
        let counted = flags[c] & F_HARDLINK_DUP == 0;
        (
            if counted { size[c] } else { 0 },
            if counted { alloc[c] } else { 0 },
            (kind[c] == KIND_FILE) as i32,
            (kind[c] == KIND_DIR) as i32,
        )
    };
    // SAFETY (reads of the totals): a node's totals are complete before its parent reads them
    let get = |c: usize| unsafe { (ts.get(c), ta.get(c), tf.get(c), td.get(c)) };
    if nlevels < u16::MAX as usize {
        // node ids of each depth: depth d (1-based) holds ids [start[d], start[d + 1])
        let mut start = vec![count; nlevels + 2];
        start[0] = 0;
        for (id, &d) in depth.iter().enumerate().rev() {
            start[d as usize] = id;
        }
        for d in (0..=nlevels).rev() {
            let (lo, hi) = (start[d], if d == nlevels { count } else { start[d + 1] });
            (lo..hi)
                .into_par_iter()
                .with_min_len(1 << 10)
                .for_each(|p| {
                    let c = many[p];
                    if c == 0 {
                        return;
                    }
                    let lo_c = first[p] as usize;
                    let mut acc = (0i64, 0i64, 0i32, 0i32);
                    for ch in lo_c..lo_c + c as usize {
                        let (s, a, f, dd) = get(ch);
                        let (os, oa, of, od) = own(ch);
                        acc.0 = acc.0.wrapping_add(s.wrapping_add(os));
                        acc.1 = acc.1.wrapping_add(a.wrapping_add(oa));
                        acc.2 = acc.2.wrapping_add(f.wrapping_add(of));
                        acc.3 = acc.3.wrapping_add(dd.wrapping_add(od));
                    }
                    // SAFETY: each parent is written once, by its own worker
                    unsafe {
                        ts.put(p, acc.0);
                        ta.put(p, acc.1);
                        tf.put(p, acc.2);
                        td.put(p, acc.3);
                    }
                });
        }
        return;
    }
    // wrapped depths: reaggregate's loop, literally
    let maxd = depth.iter().copied().max().unwrap_or(0);
    let mut sub = vec![(0i64, 0i64, 0i32, 0i32); count];
    for d in (1..=maxd).rev() {
        for c in 0..count {
            if depth[c] != d {
                continue;
            }
            let p = parent[c] as usize;
            let (s, a, f, dd) = sub[c];
            let (os, oa, of, od) = own(c);
            let t = &mut sub[p];
            t.0 = t.0.wrapping_add(s.wrapping_add(os));
            t.1 = t.1.wrapping_add(a.wrapping_add(oa));
            t.2 = t.2.wrapping_add(f.wrapping_add(of));
            t.3 = t.3.wrapping_add(dd.wrapping_add(od));
        }
    }
    for (c, t) in sub.into_iter().enumerate() {
        // SAFETY: single-threaded here
        unsafe {
            ts.put(c, t.0);
            ta.put(c, t.1);
            tf.put(c, t.2);
            td.put(c, t.3);
        }
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(mft_build, m)?)?;
    Ok(())
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
        assert!(ext_key(b"zip") < ext_key(b"txt")); // integers of the little-endian bytes: "p" < "t"
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
        // one cycle longer than the label rounds: every row on it
        let n = CYCLE_LABEL_ROUNDS + 1;
        let pdir: Vec<i32> = (0..n as i32).map(|i| (i + 1) % n as i32).collect();
        let stuck: Vec<u32> = (0..n as u32).collect();
        assert_eq!(cycle_breakers(&pdir, &stuck).len(), n);
        let n = CYCLE_LABEL_ROUNDS;
        let pdir: Vec<i32> = (0..n as i32).map(|i| (i + 1) % n as i32).collect();
        let stuck: Vec<u32> = (0..n as u32).collect();
        assert_eq!(cycle_breakers(&pdir, &stuck), vec![0]);
    }

    #[test]
    fn levels_in_row_order() {
        // root 4; 0 and 2 under the root, 1 and 3 under 0; 3 is not a folder
        let pdir = [4, 0, 4, 0, -1];
        let container = [true, true, false, false, true];
        let t = breadth_first(&pdir, &container, 4).unwrap();
        assert_eq!(t.rows, vec![4, 0, 2, 1, 3]);
        assert_eq!(t.levels, vec![0, 1, 3, 5]);
        assert_eq!(t.counts, vec![2, 0, 0, 0, 2]);
    }
}
