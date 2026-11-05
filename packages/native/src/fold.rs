//! The fold of scan slices into a node store (spaceknife.store.nodes.NodeStore.ingest_slices):
//! `NodeStore.ingest_slice` of the backend, column by column, in one pass per entry and without
//! the GIL for the entries (docs/native.md 2.3).
//!
//! The store keeps its columns and names in Python (numpy arrays, a heap of chunks); the backend
//! drives the three steps and owns every allocation:
//!
//! 1. `SliceFold(parts, n, kind, flags, child_count, depth)` checks the slices against the store
//!    (the errors of ingest_slice, before anything is written) and works out which listings are
//!    kept (those of folders that left the tree, GONE, and everything inside them are not), where
//!    each one starts, its depth, and how many rows and bytes of names they add;
//! 2. the backend grows its columns and takes room in its name heap, then `write(cols, chunk, pos,
//!    base)` writes the new rows and names with the GIL released: rows the store does not count
//!    yet, so no reader sees them;
//! 3. `publish(store)`, with the GIL held from start to end, does what readers see, in the order
//!    of ingest_slice: listed folders lose PENDING, the store's version and row count move, the
//!    ranges' starts then counts are set, then the totals (new folders' own, and every listed store
//!    node's chain up to node 0).
//!
//! Several slices fold as one (`parts`): exactly what worker.merge_slices followed by one
//! ingest_slice did, without the merged copy.

use std::collections::HashSet;
use std::sync::atomic::{Ordering, fence};

use numpy::{Element, PyArray1, PyArrayMethods};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

/// spaceknife.store.nodes.Kind and Flag values.
const DIR: u8 = 1;
const FILE: u8 = 0;
const MOUNT: u32 = 1 << 1;
const HARDLINK_DUP: u32 = 1 << 2;
const PENDING: u32 = 1 << 11;
const GONE: u32 = 1 << 19;

/// A contiguous array or bytes object the fold keeps a reference to, read without the GIL.
struct In<T> {
    _owner: Py<PyAny>,
    ptr: *const T,
    len: usize,
}

// SAFETY: the owner keeps the memory alive and nothing writes to it during the fold (these are
// the slices' own arrays, never the store's).
unsafe impl<T: Sync> Send for In<T> {}
unsafe impl<T: Sync> Sync for In<T> {}

impl<T: Element> In<T> {
    fn new(obj: &Bound<'_, PyAny>, what: &str) -> PyResult<Self> {
        let arr = obj
            .cast::<PyArray1<T>>()
            .map_err(|_| PyValueError::new_err(format!("{what}: not a 1-d array of the store's type")))?;
        let ro = arr
            .try_readonly()
            .map_err(|e| PyValueError::new_err(format!("{what}: {e}")))?;
        let s = ro
            .as_slice()
            .map_err(|e| PyValueError::new_err(format!("{what}: {e}")))?;
        Ok(Self {
            _owner: obj.clone().unbind(),
            ptr: s.as_ptr(),
            len: s.len(),
        })
    }
}

impl<T> In<T> {
    fn get(&self) -> &[T] {
        // SAFETY: see `In`; ptr/len came from a live slice of the owner.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

/// A store column written without the GIL. It keeps the array alive itself: another thread may
/// replace the store's column meanwhile (NodeStore.compact; the backend keeps that from happening
/// during a fold, see `SliceFold::write`), and the old array must outlive the writes into it.
struct Out<T> {
    _owner: Py<PyAny>,
    ptr: *mut T,
    len: usize,
}

// SAFETY: the owner keeps the memory alive; only the fold writes these rows (the store has one
// writer), see `SliceFold::write`.
unsafe impl<T: Send> Send for Out<T> {}
unsafe impl<T: Send> Sync for Out<T> {}

impl<T> Out<T> {
    /// SAFETY: no other `&mut` to the same rows exists while the result lives.
    #[allow(clippy::mut_from_ref)]
    unsafe fn get(&self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

/// A writable contiguous array of `T` with at least `rows` elements.
fn out_of<T: Element>(obj: &Bound<'_, PyAny>, what: &str, rows: usize) -> PyResult<Out<T>> {
    let arr = obj
        .cast::<PyArray1<T>>()
        .map_err(|_| PyValueError::new_err(format!("{what} has another type")))?;
    let mut rw = arr
        .try_readwrite()
        .map_err(|e| PyValueError::new_err(format!("{what}: {e}")))?;
    let s = rw
        .as_slice_mut()
        .map_err(|e| PyValueError::new_err(format!("{what}: {e}")))?;
    if s.len() < rows {
        return Err(PyValueError::new_err(format!("{what} is too short")));
    }
    Ok(Out {
        _owner: obj.clone().unbind(),
        ptr: s.as_mut_ptr(),
        len: s.len(),
    })
}

/// The store's column `key`, as `out_of`.
fn out<T: Element>(cols: &Bound<'_, PyDict>, key: &str, rows: usize) -> PyResult<Out<T>> {
    let obj = cols
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("the store has no column {key}")))?;
    out_of(&obj, &format!("column {key}"), rows)
}

/// name_off: uint32 until the names pass 4 GiB, then int64 (NodeStore._fit_offsets).
enum Offsets {
    Narrow(Out<u32>),
    Wide(Out<i64>),
}

/// One slice (a worker.SliceResult) as the fold reads it.
struct Part {
    refs: In<i64>, // (listings, 3) flattened
    bounds: In<i64>,
    names: In<u8>,
    lens: In<u16>,
    kind: In<u8>,
    flags: In<u32>,
    size: In<i64>,
    alloc: In<i64>,
    mtime: In<u32>,
    ctime: In<u32>,
    atime: In<u32>,
    ino: In<u64>,
    nlink: In<u16>,
    ext: Option<In<u16>>,
}

/// One listing of the folded slices, in order.
#[derive(Clone, Copy, Default)]
struct Listing {
    part: u32,
    /// Its reference as the slice gave it, and the index of its part's first listing.
    r: [i64; 3],
    first: usize,
    /// Its entries in its part, and where their names start in the part's name bytes.
    lo: usize,
    hi: usize,
    name_at: usize,
    /// Kept: not of a folder that left the tree, nor inside one.
    kept: bool,
    /// The folder it lists: a store node, or (inner) a row this fold adds.
    dir: i64,
    inner: bool,
    /// Inner: the listing (index into `listings`) whose entry it lists.
    up: usize,
    /// The listed folder's depth.
    depth: i64,
    /// Its first row in the store (`start` of ingest_slice).
    start: i64,
    /// Its subtree's totals: size, allocated, files, folders (filled by `write`).
    acc: [i64; 4],
}

#[pyclass(module = "spaceknife_native._native")]
pub struct SliceFold {
    parts: Vec<Part>,
    listings: Vec<Listing>,
    /// Listings per part (for `starts`).
    per_part: Vec<usize>,
    n0: i64,
    rows: usize,
    name_bytes: usize,
    written: bool,
    published: bool,
}

fn bad(msg: &str) -> PyErr {
    PyValueError::new_err(msg.to_owned())
}

fn column<'py>(entries: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    entries.get_item(key)
}

#[pymethods]
impl SliceFold {
    /// Checks `parts` — each `(refs, bounds, name_bytes, name_lens, entries, ext_ids)` with numpy
    /// arrays of the store's types (refs flattened int64, bounds int64, names uint8, name_lens
    /// uint16, the ENTRY_COLUMNS of `entries`, ext_ids uint16 or None) — against a store of `n`
    /// rows whose kind, flags, child_count and depth columns are given. Raises ValueError as
    /// ingest_slice does (nothing is written then).
    #[new]
    fn new(
        parts: &Bound<'_, PyList>,
        n: i64,
        kind: &Bound<'_, PyAny>,
        flags: &Bound<'_, PyAny>,
        child_count: &Bound<'_, PyAny>,
        depth: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        let s_kind = In::<u8>::new(kind, "kind")?;
        let s_flags = In::<u32>::new(flags, "flags")?;
        let s_count = In::<i32>::new(child_count, "child_count")?;
        let s_depth = In::<u16>::new(depth, "depth")?;
        if n < 0 || [s_kind.len, s_flags.len, s_count.len, s_depth.len].iter().any(|&l| (l as i64) < n) {
            return Err(bad("the store's columns are shorter than its rows"));
        }
        let (s_kind, s_flags, s_count, s_depth) = (s_kind.get(), s_flags.get(), s_count.get(), s_depth.get());
        let mut out = Self {
            parts: Vec::new(),
            listings: Vec::new(),
            per_part: Vec::new(),
            n0: n,
            rows: 0,
            name_bytes: 0,
            written: false,
            published: false,
        };
        for (p, item) in parts.iter().enumerate() {
            let t = item.cast::<PyTuple>()?;
            if t.len() != 6 {
                return Err(bad("a part is (refs, bounds, names, name_lens, entries, ext_ids)"));
            }
            let entries = t.get_item(4)?;
            let ext = t.get_item(5)?;
            let part = Part {
                refs: In::new(&t.get_item(0)?, "refs")?,
                bounds: In::new(&t.get_item(1)?, "bounds")?,
                names: In::new(&t.get_item(2)?, "names")?,
                lens: In::new(&t.get_item(3)?, "name_lens")?,
                kind: In::new(&column(&entries, "kind")?, "kind")?,
                flags: In::new(&column(&entries, "flags")?, "flags")?,
                size: In::new(&column(&entries, "size")?, "size")?,
                alloc: In::new(&column(&entries, "alloc")?, "alloc")?,
                mtime: In::new(&column(&entries, "mtime")?, "mtime")?,
                ctime: In::new(&column(&entries, "ctime")?, "ctime")?,
                atime: In::new(&column(&entries, "atime")?, "atime")?,
                ino: In::new(&column(&entries, "ino")?, "ino")?,
                nlink: In::new(&column(&entries, "nlink")?, "nlink")?,
                ext: if ext.is_none() { None } else { Some(In::new(&ext, "ext_ids")?) },
            };
            let refs = part.refs.get();
            let bounds = part.bounds.get();
            if refs.len() % 3 != 0 || bounds.len() != refs.len() / 3 + 1 {
                return Err(bad("refs and bounds do not match"));
            }
            let rows = part.lens.len;
            let same = [
                part.kind.len,
                part.flags.len,
                part.size.len,
                part.alloc.len,
                part.mtime.len,
                part.ctime.len,
                part.atime.len,
                part.ino.len,
                part.nlink.len,
            ];
            if same.iter().any(|&l| l != rows) || part.ext.as_ref().is_some_and(|e| e.len != rows) {
                return Err(bad("the entry columns of a slice differ in length"));
            }
            if bounds[0] != 0
                || bounds[bounds.len() - 1] != rows as i64
                || bounds.windows(2).any(|w| w[1] < w[0])
            {
                return Err(bad("bounds do not cover the entries in order"));
            }
            let lens = part.lens.get();
            let mut at = 0usize;
            let first = out.listings.len();
            for k in 0..refs.len() / 3 {
                let (lo, hi) = (bounds[k] as usize, bounds[k + 1] as usize);
                let bytes: usize = lens[lo..hi].iter().map(|&l| l as usize).sum();
                out.listings.push(Listing {
                    part: p as u32,
                    r: [refs[3 * k], refs[3 * k + 1], refs[3 * k + 2]],
                    first,
                    lo,
                    hi,
                    name_at: at,
                    ..Listing::default()
                });
                at += bytes;
            }
            if at != part.names.len {
                return Err(bad("the name bytes do not match the name lengths"));
            }
            // which listings are of folders that left the tree (or lie inside one): skipped
            for k in 0..refs.len() / 3 {
                let (t0, a) = (refs[3 * k], refs[3 * k + 1]);
                let gone = if t0 == 0 {
                    0 <= a && a < n && s_flags[a as usize] & GONE != 0
                } else {
                    0 <= a && (a as usize) < k && !out.listings[first + a as usize].kept
                };
                out.listings[first + k].kept = !gone;
            }
            out.per_part.push(refs.len() / 3);
            out.parts.push(part);
        }

        // ingest_slice's checks, in its order, over the kept listings
        let kept: Vec<usize> = (0..out.listings.len()).filter(|&k| out.listings[k].kept).collect();
        let refs_of = |l: &Listing| -> (i64, i64, i64, usize) { (l.r[0], l.r[1], l.r[2], l.first) };
        for &k in &kept {
            let (t0, a, _, first) = refs_of(&out.listings[k]);
            let local = (k - first) as i64;
            if t0 != 0 && !(0 <= a && a < local) {
                return Err(bad("a listing can only name an entry of an earlier listing"));
            }
        }
        for &k in &kept {
            let (t0, a, b, first) = refs_of(&out.listings[k]);
            if t0 != 0 {
                let j = &out.listings[first + a as usize];
                if !(0 <= b && b < (j.hi - j.lo) as i64) {
                    return Err(bad("a listing names an entry its listing does not have"));
                }
            }
        }
        for &k in &kept {
            let (t0, a, _, _) = refs_of(&out.listings[k]);
            if t0 == 0 && !(0 <= a && a < n) {
                return Err(bad("a listing names a node that is not in the store"));
            }
        }
        // where each kept listing starts, the folder it lists and its depth
        let mut start = n;
        for &k in &kept {
            let (t0, a, b, first) = refs_of(&out.listings[k]);
            let count = (out.listings[k].hi - out.listings[k].lo) as i64;
            let (dir, depth, up) = if t0 == 0 {
                (a, s_depth[a as usize] as i64, 0)
            } else {
                let j = first + a as usize;
                (out.listings[j].start + b, out.listings[j].depth + 1, j)
            };
            let l = &mut out.listings[k];
            l.dir = dir;
            l.depth = depth;
            l.inner = t0 != 0;
            l.up = up;
            l.start = start;
            start += count;
        }
        for &k in &kept {
            let l = out.listings[k];
            let kind = if l.inner {
                let j = &out.listings[l.up];
                out.parts[j.part as usize].kind.get()[j.lo + (l.dir - j.start) as usize]
            } else {
                s_kind[l.dir as usize]
            };
            if kind != DIR {
                return Err(bad(&format!("node {} is not a directory", l.dir)));
            }
        }
        let mut seen = HashSet::with_capacity(kept.len());
        for &k in &kept {
            let l = &out.listings[k];
            if (!l.inner && s_count[l.dir as usize] != 0) || !seen.insert(l.dir) {
                return Err(bad("a directory was already listed"));
            }
        }
        out.rows = (start - n) as usize;
        out.name_bytes = kept
            .iter()
            .map(|&k| {
                let l = &out.listings[k];
                let lens = out.parts[l.part as usize].lens.get();
                lens[l.lo..l.hi].iter().map(|&x| x as usize).sum::<usize>()
            })
            .sum::<usize>()
            + out.rows;
        Ok(out)
    }

    /// Rows the kept listings add.
    #[getter]
    fn rows(&self) -> usize {
        self.rows
    }

    /// Bytes they take in the name heap (each name and its NUL).
    #[getter]
    fn name_bytes(&self) -> usize {
        self.name_bytes
    }

    /// Kept listings.
    #[getter]
    fn listings(&self) -> usize {
        self.listings.iter().filter(|l| l.kept).count()
    }

    /// Writes the new rows into the store's columns `cols` (grown to hold them) and their names
    /// into `chunk[pos:pos + name_bytes]`, a heap chunk whose first byte is the store's name offset
    /// `base` (`\0name` each, offsets just past the NUL). The GIL is released meanwhile.
    fn write(
        &mut self,
        py: Python<'_>,
        cols: &Bound<'_, PyDict>,
        chunk: &Bound<'_, PyAny>,
        pos: usize,
        base: i64,
    ) -> PyResult<()> {
        if self.written {
            return Err(PyRuntimeError::new_err("this fold was written already"));
        }
        let end = self.n0 as usize + self.rows;
        let parent = out::<i32>(cols, "parent", end)?;
        let depth = out::<u16>(cols, "depth", end)?;
        let kind = out::<u8>(cols, "kind", end)?;
        let flags = out::<u32>(cols, "flags", end)?;
        let name_len = out::<u16>(cols, "name_len", end)?;
        let ext = out::<u16>(cols, "ext", end)?;
        let size = out::<i64>(cols, "size", end)?;
        let alloc = out::<i64>(cols, "alloc", end)?;
        let mtime = out::<u32>(cols, "mtime", end)?;
        let ctime = out::<u32>(cols, "ctime", end)?;
        let atime = out::<u32>(cols, "atime", end)?;
        let ino = out::<u64>(cols, "ino", end)?;
        let nlink = out::<u16>(cols, "nlink", end)?;
        let zeros_i32 = [
            out::<i32>(cols, "child_start", end)?,
            out::<i32>(cols, "child_count", end)?,
            out::<i32>(cols, "tot_files", end)?,
            out::<i32>(cols, "tot_dirs", end)?,
        ];
        let zeros_i64 = [out::<i64>(cols, "tot_size", end)?, out::<i64>(cols, "tot_alloc", end)?];
        let offsets = match out::<u32>(cols, "name_off", end) {
            Ok(o) => Offsets::Narrow(o),
            Err(_) => Offsets::Wide(out::<i64>(cols, "name_off", end)?),
        };
        let heap = out_of::<u8>(chunk, "the name chunk", pos + self.name_bytes)?;
        let (parts, listings, n0) = (&self.parts, &mut self.listings, self.n0 as usize);
        let name_bytes = self.name_bytes;
        // the closure borrows the columns: their owners are dropped here, with the GIL
        py.detach(|| {
            // SAFETY: the rows n0.. are past the store's count, so no reader looks at them, and
            // the caller holds the store's single write; each column is a distinct array.
            unsafe {
                let (parent, depth, kind, flags) = (parent.get(), depth.get(), kind.get(), flags.get());
                let (name_len, ext, size, alloc) = (name_len.get(), ext.get(), size.get(), alloc.get());
                let (mtime, ctime, atime, ino, nlink) = (mtime.get(), ctime.get(), atime.get(), ino.get(), nlink.get());
                let heap = &mut heap.get()[pos..pos + name_bytes];
                let mut row = n0;
                let mut w = 0usize; // into the heap run
                for k in 0..listings.len() {
                    let l = listings[k];
                    if !l.kept {
                        continue;
                    }
                    let p = &parts[l.part as usize];
                    let (lo, hi) = (l.lo, l.hi);
                    let rng = row..row + (hi - lo);
                    parent[rng.clone()].fill(l.dir as i32);
                    depth[rng.clone()].fill((l.depth + 1) as u16);
                    kind[rng.clone()].copy_from_slice(&p.kind.get()[lo..hi]);
                    size[rng.clone()].copy_from_slice(&p.size.get()[lo..hi]);
                    alloc[rng.clone()].copy_from_slice(&p.alloc.get()[lo..hi]);
                    mtime[rng.clone()].copy_from_slice(&p.mtime.get()[lo..hi]);
                    ctime[rng.clone()].copy_from_slice(&p.ctime.get()[lo..hi]);
                    atime[rng.clone()].copy_from_slice(&p.atime.get()[lo..hi]);
                    ino[rng.clone()].copy_from_slice(&p.ino.get()[lo..hi]);
                    nlink[rng.clone()].copy_from_slice(&p.nlink.get()[lo..hi]);
                    match &p.ext {
                        Some(e) => ext[rng.clone()].copy_from_slice(&e.get()[lo..hi]),
                        None => ext[rng.clone()].fill(0),
                    }
                    for z in &zeros_i32 {
                        z.get()[rng.clone()].fill(0);
                    }
                    for z in &zeros_i64 {
                        z.get()[rng.clone()].fill(0);
                    }
                    let lens = &p.lens.get()[lo..hi];
                    name_len[rng.clone()].copy_from_slice(lens);
                    let names = p.names.get();
                    let mut at = l.name_at;
                    let mut acc = [0i64; 4];
                    let (src_kind, src_flags) = (&p.kind.get()[lo..hi], &p.flags.get()[lo..hi]);
                    let (src_size, src_alloc) = (&p.size.get()[lo..hi], &p.alloc.get()[lo..hi]);
                    for i in 0..hi - lo {
                        let r = row + i;
                        let len = lens[i] as usize;
                        heap[w] = 0;
                        heap[w + 1..w + 1 + len].copy_from_slice(&names[at..at + len]);
                        let off = base + (pos + w + 1) as i64;
                        match &offsets {
                            Offsets::Narrow(o) => o.get()[r] = off as u32,
                            Offsets::Wide(o) => o.get()[r] = off,
                        }
                        w += len + 1;
                        at += len;
                        let (k_, f) = (src_kind[i], src_flags[i]);
                        let is_dir = k_ == DIR;
                        flags[r] = if is_dir && f & MOUNT == 0 { f | PENDING } else { f };
                        if f & HARDLINK_DUP == 0 {
                            acc[0] = acc[0].wrapping_add(src_size[i]);
                            acc[1] = acc[1].wrapping_add(src_alloc[i]);
                        }
                        acc[2] += (k_ == FILE) as i64;
                        acc[3] += is_dir as i64;
                    }
                    listings[k].acc = acc;
                    row += hi - lo;
                }
                // folders listed in this fold are not pending (their rows are written: j < k)
                for l in listings.iter().filter(|l| l.kept && l.inner) {
                    flags[l.dir as usize] &= !PENDING;
                }
                // subtree totals inside the fold: deepest first is any order from the end
                for k in (0..listings.len()).rev() {
                    let l = listings[k];
                    if l.kept && l.inner {
                        let a = l.acc;
                        let up = &mut listings[l.up].acc;
                        for (u, x) in up.iter_mut().zip(a) {
                            *u = u.wrapping_add(x);
                        }
                    }
                }
            }
        });
        self.written = true;
        Ok(())
    }

    /// Makes the fold visible, as ingest_slice does and in its order, with the GIL held: the
    /// listed store nodes lose PENDING, `store.version` and `store._n` move, every listed folder's
    /// child range gets its start and then its count, then the new folders' totals are set and
    /// each listed store node passes its subtree's totals up its chain to node 0. Returns each
    /// part's listing starts (-1 for a listing that was skipped).
    fn publish<'py>(&mut self, py: Python<'py>, store: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        if self.published || (self.rows > 0 && !self.written) {
            return Err(PyRuntimeError::new_err("write the rows first, and publish once"));
        }
        let kept = self.listings.iter().any(|l| l.kept);
        if kept {
            let cols = store.getattr("cols")?.cast_into::<PyDict>()?;
            let end = self.n0 as usize + self.rows;
            let flags = out::<u32>(&cols, "flags", end)?;
            let starts = out::<i32>(&cols, "child_start", end)?;
            let counts = out::<i32>(&cols, "child_count", end)?;
            // SAFETY: with the GIL held no Python code runs meanwhile; distinct arrays.
            unsafe {
                for l in self.listings.iter().filter(|l| l.kept && !l.inner) {
                    flags.get()[l.dir as usize] &= !PENDING;
                }
            }
            let version: i64 = store.getattr("version")?.extract()?;
            store.setattr("version", version + 1)?;
            store.setattr("_n", end)?;
            unsafe {
                for l in self.listings.iter().filter(|l| l.kept) {
                    starts.get()[l.dir as usize] = l.start as i32;
                }
                fence(Ordering::Release); // the starts before the counts (child_range reads count first)
                for l in self.listings.iter().filter(|l| l.kept) {
                    counts.get()[l.dir as usize] = (l.hi - l.lo) as i32;
                }
            }
            if self.rows > 0 {
                let tot_size = out::<i64>(&cols, "tot_size", end)?;
                let tot_alloc = out::<i64>(&cols, "tot_alloc", end)?;
                let tot_files = out::<i32>(&cols, "tot_files", end)?;
                let tot_dirs = out::<i32>(&cols, "tot_dirs", end)?;
                let parent = out::<i32>(&cols, "parent", end)?;
                unsafe {
                    let (ts, ta, tf, td, par) =
                        (tot_size.get(), tot_alloc.get(), tot_files.get(), tot_dirs.get(), parent.get());
                    for l in self.listings.iter().filter(|l| l.kept && l.inner) {
                        let d = l.dir as usize;
                        (ts[d], ta[d], tf[d], td[d]) = (l.acc[0], l.acc[1], l.acc[2] as i32, l.acc[3] as i32);
                    }
                    for l in self.listings.iter().filter(|l| l.kept && !l.inner) {
                        let mut node = l.dir;
                        while node >= 0 {
                            let d = node as usize;
                            ts[d] = ts[d].wrapping_add(l.acc[0]);
                            ta[d] = ta[d].wrapping_add(l.acc[1]);
                            tf[d] = tf[d].wrapping_add(l.acc[2] as i32);
                            td[d] = td[d].wrapping_add(l.acc[3] as i32);
                            node = par[d] as i64;
                        }
                    }
                }
            }
        }
        self.published = true;
        let out = PyList::empty(py);
        let mut k = 0;
        for &n in &self.per_part {
            let v: Vec<i64> = self.listings[k..k + n]
                .iter()
                .map(|l| if l.kept { l.start } else { -1 })
                .collect();
            out.append(PyArray1::from_vec(py, v))?;
            k += n;
        }
        Ok(out)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<SliceFold>()?;
    Ok(())
}
