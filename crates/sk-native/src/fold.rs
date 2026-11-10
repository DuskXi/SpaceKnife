//! The fold of directory listings into a node store's rows (docs/native.md 2.3; the native
//! `fold.rs` of the Python era): the per-entry work of the store's `ingest_slice`,
//! `ingest_listing` and relist, column by column in one pass per entry. The store keeps its memory,
//! its locks and what readers see; this module plans and writes rows nobody reads yet:
//!
//! 1. [`SliceFold::plan`] checks a slice against the store, read through [`Tree`] (the errors of
//!    `ingest_slice`, before anything is written), and works out which listings are kept — not
//!    those of folders that left the tree (`GONE`), nor anything inside them —, the folder each
//!    one lists, its depth, where its rows start, and how many rows and name bytes they add;
//! 2. the store grows its columns, makes room for the names and lends the new rows ([`RowsOut`]);
//!    [`SliceFold::write`] fills them: the entry columns, `PENDING` on the folders still to list
//!    (never on `MOUNT` ones), zero totals and ranges, `\0name` and `name_off`; it sums each
//!    listing's entries and folds the sums up the slice (deepest first), and the folders listed
//!    inside the slice get their child range and subtree totals;
//! 3. the store publishes, in its order: [`SliceFold::listed_nodes`] gives the range and subtree
//!    totals of every store node the slice lists (the store adds them up the node's chain),
//!    [`SliceFold::starts`] every listing's first row.
//!
//! [`write_rows`] is step 2 for the entries of one folder (`ingest_listing`, a relist's new rows).
//! Several slices fold as one (`hardscan::SliceResult::append`).

use std::fmt;

use crate::columns::{NameOffsets, NodeColumns, flag, kind};

/// Totals of entries as their folder counts them: logical bytes, allocated bytes, files, folders
/// (wrapping sums). A further link of a file (`HARDLINK_DUP`) adds no bytes; a folder's own
/// allocation (POSIX directory blocks) counts.
pub type Totals = [i64; 4];

/// The entry columns of one or more listings, borrowed: the store's entry batch, a slice's entries
/// (struct-of-arrays, one row per entry; names concatenated, `name_lens[i]` bytes each).
#[derive(Clone, Copy, Debug)]
pub struct EntryColumns<'a> {
    pub names: &'a [u8],
    pub name_lens: &'a [u16],
    /// Ids in the store's extension table.
    pub ext: &'a [u16],
    pub kind: &'a [u8],
    pub flags: &'a [u32],
    pub size: &'a [i64],
    pub alloc: &'a [i64],
    pub mtime: &'a [u32],
    pub ctime: &'a [u32],
    pub atime: &'a [u32],
    pub ino: &'a [u64],
    pub nlink: &'a [u16],
}

impl EntryColumns<'_> {
    /// Entries (the length of `name_lens`).
    pub fn len(&self) -> usize {
        self.name_lens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name_lens.is_empty()
    }

    /// Every column has [`EntryColumns::len`] rows and the name lengths add up to `names`.
    pub fn is_consistent(&self) -> bool {
        let n = self.len();
        let same = [
            self.ext.len(),
            self.kind.len(),
            self.flags.len(),
            self.size.len(),
            self.alloc.len(),
            self.mtime.len(),
            self.ctime.len(),
            self.atime.len(),
            self.ino.len(),
            self.nlink.len(),
        ]
        .iter()
        .all(|&l| l == n);
        same && self.name_lens.iter().map(|&l| l as usize).sum::<usize>() == self.names.len()
    }
}

/// New rows a store lends to a write: every column's rows, their `name_off`, and the bytes of
/// their names (`\0name` per row, back to back) at the store's name offset `names_at`. Nobody may
/// read them until the store publishes them.
#[derive(Debug, Default)]
pub struct RowsOut<'a> {
    pub cols: NodeColumns<'a>,
    pub offsets: NameOffsets<'a>,
    /// The names' bytes plus one NUL per row.
    pub names: &'a mut [u8],
    /// The store's name offset of `names[0]` (a row's `name_off` is just past its NUL).
    pub names_at: u64,
}

impl RowsOut<'_> {
    fn check(&self, rows: usize, name_bytes: usize) {
        assert!(
            self.cols.len() == rows
                && self.cols.is_uniform()
                && self.offsets.len() == rows
                && self.names.len() == name_bytes,
            "the rows lent ({} rows, {} name bytes) do not fit the write ({rows} rows, \
             {name_bytes} name bytes)",
            self.cols.len(),
            self.names.len()
        );
    }
}

/// Writes rows: [`Writer::copy`] the entry columns and names of the next rows (one listing, or a
/// whole slice at once when none is skipped), [`Writer::place`] each listing's parent, depth,
/// flags and totals.
struct Writer<'o, 'a> {
    out: &'o mut RowsOut<'a>,
    /// Rows copied, name bytes written.
    row: usize,
    byte: usize,
}

impl Writer<'_, '_> {
    /// Entries `lo..hi` (their names from byte `*src` of `e.names` on, which moves past them) as
    /// the next rows: every column but parent, depth and flags (totals and ranges zero), the
    /// names and their offsets.
    fn copy(&mut self, e: &EntryColumns<'_>, lo: usize, hi: usize, src: &mut usize) {
        let r = self.row..self.row + (hi - lo);
        let c = &mut self.out.cols;
        c.kind[r.clone()].copy_from_slice(&e.kind[lo..hi]);
        c.size[r.clone()].copy_from_slice(&e.size[lo..hi]);
        c.alloc[r.clone()].copy_from_slice(&e.alloc[lo..hi]);
        c.mtime[r.clone()].copy_from_slice(&e.mtime[lo..hi]);
        c.ctime[r.clone()].copy_from_slice(&e.ctime[lo..hi]);
        c.atime[r.clone()].copy_from_slice(&e.atime[lo..hi]);
        c.ino[r.clone()].copy_from_slice(&e.ino[lo..hi]);
        c.nlink[r.clone()].copy_from_slice(&e.nlink[lo..hi]);
        c.ext[r.clone()].copy_from_slice(&e.ext[lo..hi]);
        c.name_len[r.clone()].copy_from_slice(&e.name_lens[lo..hi]);
        c.child_start[r.clone()].fill(0);
        c.child_count[r.clone()].fill(0);
        c.tot_size[r.clone()].fill(0);
        c.tot_alloc[r.clone()].fill(0);
        c.tot_files[r.clone()].fill(0);
        c.tot_dirs[r.clone()].fill(0);
        let lens = &e.name_lens[lo..hi];
        let past_nul = self.out.names_at + 1;
        let names = &mut *self.out.names;
        let (byte, read) = match &mut self.out.offsets {
            NameOffsets::Narrow(o) => put_names(
                names,
                &mut o[r.clone()],
                lens,
                e.names,
                self.byte,
                *src,
                |w| (past_nul + w) as u32,
            ),
            NameOffsets::Wide(o) => put_names(
                names,
                &mut o[r.clone()],
                lens,
                e.names,
                self.byte,
                *src,
                |w| past_nul + w,
            ),
        };
        self.byte = byte;
        *src = read;
        self.row = r.end;
    }

    /// Rows `at..` are entries `lo..hi`: children of `parent` at `depth`, their flags (folders
    /// `PENDING`, unless `MOUNT`, when `pending`). Returns the entries' totals.
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        e: &EntryColumns<'_>,
        lo: usize,
        hi: usize,
        at: usize,
        parent: i32,
        depth: u16,
        pending: bool,
    ) -> Totals {
        let r = at..at + (hi - lo);
        let c = &mut self.out.cols;
        c.parent[r.clone()].fill(parent);
        c.depth[r.clone()].fill(depth);
        let (kinds, flags) = (&e.kind[lo..hi], &e.flags[lo..hi]);
        let (sizes, allocs) = (&e.size[lo..hi], &e.alloc[lo..hi]);
        let mut acc: Totals = [0; 4];
        for (i, out) in c.flags[r].iter_mut().enumerate() {
            let (k, f) = (kinds[i], flags[i]);
            let dir = k == kind::DIR;
            *out = if pending && dir && f & flag::MOUNT == 0 {
                f | flag::PENDING
            } else {
                f
            };
            if f & flag::HARDLINK_DUP == 0 {
                acc[0] = acc[0].wrapping_add(sizes[i]);
                acc[1] = acc[1].wrapping_add(allocs[i]);
            }
            acc[2] += (k == kind::FILE) as i64;
            acc[3] += dir as i64;
        }
        acc
    }
}
/// `\0name` for every name (lengths `lens`, read from `src[at..]`) into `dst` from byte `w` on,
/// and each one's offset into `offs` (`off(p)`, `p` the position of its NUL in `dst`). Returns
/// where writing and reading stopped.
#[inline]
fn put_names<O>(
    dst: &mut [u8],
    offs: &mut [O],
    lens: &[u16],
    src: &[u8],
    mut w: usize,
    mut at: usize,
    off: impl Fn(u64) -> O,
) -> (usize, usize) {
    for (o, &l) in offs.iter_mut().zip(lens) {
        let l = l as usize;
        dst[w] = 0;
        dst[w + 1..w + 1 + l].copy_from_slice(&src[at..at + l]);
        *o = off(w as u64);
        w += l + 1;
        at += l;
    }
    (w, at)
}

/// Writes `entries` as the rows of `out` (one each), children of `parent` at `depth`: the entry
/// columns, zero totals and ranges, the names and their offsets; folders among them `PENDING`
/// (unless `MOUNT`) when `pending`. Returns the entries' totals. Panics when `entries` is not
/// consistent or `out` does not fit it (its rows, the names' bytes plus one per row).
pub fn write_rows(
    mut out: RowsOut<'_>,
    entries: &EntryColumns<'_>,
    parent: i32,
    depth: u16,
    pending: bool,
) -> Totals {
    assert!(entries.is_consistent(), "the entry columns do not agree");
    out.check(entries.len(), entries.names.len() + entries.len());
    let mut w = Writer {
        out: &mut out,
        row: 0,
        byte: 0,
    };
    let n = entries.len();
    w.copy(entries, 0, n, &mut 0);
    w.place(entries, 0, n, 0, parent, depth, pending)
}

/// The folder a listing of a slice lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Folder {
    /// A node of the store.
    Node(u32),
    /// Entry `entry` of the earlier listing `listing` of the same slice.
    Entry { listing: u32, entry: u32 },
}

/// What a fold reads of the store: the nodes a slice names (the store's view implements it).
pub trait Tree {
    /// Rows the store holds: the fold's rows come after them.
    fn rows(&self) -> usize;
    /// [`flag`] bits of a node (`node < rows()`).
    fn flags(&self, node: u32) -> u32;
    fn is_dir(&self, node: u32) -> bool;
    /// Its children (not 0: listed already).
    fn child_count(&self, node: u32) -> i32;
    fn depth(&self, node: u32) -> u16;
}

/// Why a slice cannot be folded (nothing was written).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldError {
    /// The entry columns differ in length, or the names do not add up.
    BadBatch,
    /// The slice does not hold together.
    BadSlice(&'static str),
    /// A listing lists what is not a folder: a node, or the row an entry of the slice would get.
    NotADirectory(u32),
    /// A folder listed already, or twice in the slice.
    AlreadyListed(u32),
}

impl fmt::Display for FoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FoldError::BadBatch => f.write_str("entry columns differ in length"),
            FoldError::BadSlice(why) => write!(f, "bad slice: {why}"),
            FoldError::NotADirectory(n) => write!(f, "node {n} is not a directory"),
            FoldError::AlreadyListed(n) => write!(f, "directory {n} was already listed"),
        }
    }
}

impl std::error::Error for FoldError {}

/// One listing of the slice.
#[derive(Clone, Copy, Debug)]
struct Listing {
    /// Its entries.
    lo: u32,
    hi: u32,
    /// Not of a folder that left the tree, nor inside one.
    kept: bool,
    /// It lists an entry of the slice (`dir` is a new row), of listing `up`, row `entry` of the
    /// entries.
    inner: bool,
    up: u32,
    entry: u32,
    /// The folder it lists, its depth, the first row of its entries.
    dir: u32,
    depth: u16,
    start: usize,
    /// Its entries' totals, then (after the write) its subtree's.
    acc: Totals,
}

/// A store node a slice lists, for the store to publish: its child range and subtree totals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub node: u32,
    pub start: u32,
    pub count: u32,
    pub totals: Totals,
}

/// The fold of one slice (several merged: `SliceResult::append`), planned against a store: see
/// the module docs.
#[derive(Debug)]
pub struct SliceFold {
    n0: usize,
    listings: Vec<Listing>,
    kept: usize,
    rows: usize,
    name_bytes: usize,
    written: bool,
}

impl SliceFold {
    /// Checks a slice — listing `k` lists `folders[k]`, its entries are rows
    /// `bounds[k]..bounds[k + 1]` of `entries` — against `tree`, as the store's `ingest_slice`
    /// does: the bounds, then listing by listing (a node must be in the store, a folder, not
    /// listed yet; an entry must be of an earlier listing, in its range, a folder), then folders
    /// listed twice. Listings of folders that left the tree (`GONE`) and listings inside them are
    /// skipped, not checked.
    pub fn plan(
        tree: &(impl Tree + ?Sized),
        folders: &[Folder],
        bounds: &[u32],
        entries: &EntryColumns<'_>,
    ) -> Result<SliceFold, FoldError> {
        if !entries.is_consistent() {
            return Err(FoldError::BadBatch);
        }
        let count = folders.len();
        if bounds.len() != count + 1
            || bounds[0] != 0
            || bounds.windows(2).any(|w| w[0] > w[1])
            || bounds[count] as usize != entries.len()
        {
            return Err(FoldError::BadSlice("bounds do not match the entries"));
        }
        let n0 = tree.rows();
        let mut listings: Vec<Listing> = Vec::with_capacity(count);
        let mut next = n0;
        let mut kept = 0usize;
        let mut skipped = false;
        for (k, &folder) in folders.iter().enumerate() {
            let (lo, hi) = (bounds[k], bounds[k + 1]);
            let mut l = Listing {
                lo,
                hi,
                kept: true,
                inner: false,
                up: 0,
                entry: 0,
                dir: 0,
                depth: 0,
                start: 0,
                acc: [0; 4],
            };
            let gone = match folder {
                Folder::Node(d) => (d as usize) < n0 && tree.flags(d) & flag::GONE != 0,
                Folder::Entry { listing, .. } => {
                    (listing as usize) < k && !listings[listing as usize].kept
                }
            };
            if gone {
                l.kept = false;
                skipped = true;
                listings.push(l);
                continue;
            }
            match folder {
                Folder::Node(d) => {
                    if d as usize >= n0 {
                        return Err(FoldError::BadSlice(
                            "a listing names a node that is not in the store",
                        ));
                    }
                    if !tree.is_dir(d) {
                        return Err(FoldError::NotADirectory(d));
                    }
                    if tree.child_count(d) != 0 {
                        return Err(FoldError::AlreadyListed(d));
                    }
                    l.dir = d;
                    l.depth = tree.depth(d);
                }
                Folder::Entry { listing, entry } => {
                    if listing as usize >= k {
                        return Err(FoldError::BadSlice(
                            "a listing can only name an entry of an earlier listing",
                        ));
                    }
                    // kept: a listing inside a skipped one is skipped
                    let up = &listings[listing as usize];
                    if entry >= up.hi - up.lo {
                        return Err(FoldError::BadSlice(
                            "a listing names an entry its listing does not have",
                        ));
                    }
                    let row = up.lo + entry;
                    let id = (up.start + entry as usize) as u32;
                    if entries.kind[row as usize] != kind::DIR {
                        return Err(FoldError::NotADirectory(id));
                    }
                    l.dir = id;
                    l.depth = up.depth.wrapping_add(1);
                    l.inner = true;
                    l.up = listing;
                    l.entry = row;
                }
            }
            l.start = next;
            next += (hi - lo) as usize;
            kept += 1;
            listings.push(l);
        }
        {
            let mut seen: Vec<u32> = listings.iter().filter(|l| l.kept).map(|l| l.dir).collect();
            seen.sort_unstable();
            if let Some(w) = seen.windows(2).find(|w| w[0] == w[1]) {
                return Err(FoldError::AlreadyListed(w[0]));
            }
        }
        let rows = next - n0;
        let names = if skipped {
            listings
                .iter()
                .filter(|l| l.kept)
                .map(|l| {
                    entries.name_lens[l.lo as usize..l.hi as usize]
                        .iter()
                        .map(|&n| n as usize)
                        .sum::<usize>()
                })
                .sum()
        } else {
            entries.names.len()
        };
        Ok(SliceFold {
            n0,
            listings,
            kept,
            rows,
            name_bytes: names + rows,
            written: false,
        })
    }

    /// Listings kept (folded); the others are skipped.
    pub fn kept(&self) -> usize {
        self.kept
    }

    /// The store's rows before the fold: the first new row.
    pub fn first_row(&self) -> usize {
        self.n0
    }

    /// Rows the kept listings add.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Bytes their names take in the store (each name and its NUL).
    pub fn name_bytes(&self) -> usize {
        self.name_bytes
    }

    /// Writes the new rows (the store's rows `first_row()..first_row() + rows()`) into `out`,
    /// from the `entries` the plan was made of: see the module docs. Panics when `out` does not
    /// fit ([`SliceFold::rows`], [`SliceFold::name_bytes`]), on other entries, or a second time.
    pub fn write(&mut self, mut out: RowsOut<'_>, entries: &EntryColumns<'_>) {
        assert!(!self.written, "a fold is written once");
        assert!(
            entries.len() == self.listings.last().map_or(0, |l| l.hi as usize),
            "not the entries of the plan"
        );
        out.check(self.rows, self.name_bytes);
        {
            let mut w = Writer {
                out: &mut out,
                row: 0,
                byte: 0,
            };
            // every listing kept (the rule): the entries are the new rows, copied in one go;
            // otherwise listing by listing, the names of skipped ones passed over
            let whole = self.kept == self.listings.len();
            if whole {
                w.copy(entries, 0, entries.len(), &mut 0);
            }
            let mut src = 0usize;
            for l in &mut self.listings {
                let (lo, hi) = (l.lo as usize, l.hi as usize);
                if !l.kept {
                    src += entries.name_lens[lo..hi]
                        .iter()
                        .map(|&n| n as usize)
                        .sum::<usize>();
                    continue;
                }
                if !whole {
                    w.copy(entries, lo, hi, &mut src);
                }
                let at = l.start - self.n0;
                l.acc = w.place(
                    entries,
                    lo,
                    hi,
                    at,
                    l.dir as i32,
                    l.depth.wrapping_add(1),
                    true,
                );
            }
        } // subtree totals inside the slice: a listing names only earlier ones, so from the end
        // is deepest first
        for k in (0..self.listings.len()).rev() {
            let l = self.listings[k];
            if l.kept && l.inner {
                let up = &mut self.listings[l.up as usize].acc;
                for (u, v) in up.iter_mut().zip(l.acc) {
                    *u = u.wrapping_add(v);
                }
            }
        }
        // folders listed in the slice: listed (their flags as given, never PENDING), their range
        // and their subtree's totals
        let c = &mut out.cols;
        for l in self.listings.iter().filter(|l| l.kept && l.inner) {
            let r = l.dir as usize - self.n0;
            c.flags[r] = entries.flags[l.entry as usize];
            c.tot_size[r] = l.acc[0];
            c.tot_alloc[r] = l.acc[1];
            c.tot_files[r] = l.acc[2] as i32;
            c.tot_dirs[r] = l.acc[3] as i32;
            c.child_start[r] = l.start as i32;
            c.child_count[r] = (l.hi - l.lo) as i32;
        }
        self.written = true;
    }

    /// The store nodes the kept listings list, in order, with their child range and subtree
    /// totals (after [`SliceFold::write`]; before it the totals are 0).
    pub fn listed_nodes(&self) -> impl Iterator<Item = Listed> + '_ {
        self.listings
            .iter()
            .filter(|l| l.kept && !l.inner)
            .map(|l| Listed {
                node: l.dir,
                start: l.start as u32,
                count: l.hi - l.lo,
                totals: l.acc,
            })
    }

    /// Every listing's first row; None for a skipped listing.
    pub fn starts(&self) -> Vec<Option<u32>> {
        self.listings
            .iter()
            .map(|l| l.kept.then_some(l.start as u32))
            .collect()
    }
}
