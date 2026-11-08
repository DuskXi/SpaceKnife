//! The fold of listings into a node store's rows (`sk_native::fold`) on a store of plain vectors:
//! a slice folds as the listings one at a time would (the store's `ingest_listing` rules: rows in
//! order, folders `PENDING` unless `MOUNT`, totals up the chain), whatever the slice's shape;
//! listings of folders that left the tree are skipped with everything inside them; bad slices are
//! refused before anything is written; the lent rows are filled completely (names and their
//! offsets, zero totals and ranges) and nothing past them is touched.

use sk_native::columns::{NameOffsets, NodeColumns, flag, kind};
use sk_native::fold::{
    EntryColumns, FoldError, Folder, Listed, RowsOut, SliceFold, Totals, Tree, write_rows,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() % 1_000_000) as f64 / 1e6 < p
    }
}

/// Entries as a batch of owned columns.
#[derive(Clone, Debug, Default)]
struct Batch {
    names: Vec<u8>,
    name_lens: Vec<u16>,
    ext: Vec<u16>,
    kind: Vec<u8>,
    flags: Vec<u32>,
    size: Vec<i64>,
    alloc: Vec<i64>,
    mtime: Vec<u32>,
    ctime: Vec<u32>,
    atime: Vec<u32>,
    ino: Vec<u64>,
    nlink: Vec<u16>,
}

impl Batch {
    fn push(&mut self, name: &[u8], k: u8, f: u32, size: i64, alloc: i64, seed: u64) {
        self.names.extend_from_slice(name);
        self.name_lens.push(name.len() as u16);
        self.ext.push((seed % 5) as u16);
        self.kind.push(k);
        self.flags.push(f);
        self.size.push(size);
        self.alloc.push(alloc);
        self.mtime.push(seed as u32);
        self.ctime.push((seed >> 8) as u32);
        self.atime.push((seed >> 16) as u32);
        self.ino.push(seed >> 1);
        self.nlink.push((seed % 4) as u16 + 1);
    }

    fn cols(&self) -> EntryColumns<'_> {
        EntryColumns {
            names: &self.names,
            name_lens: &self.name_lens,
            ext: &self.ext,
            kind: &self.kind,
            flags: &self.flags,
            size: &self.size,
            alloc: &self.alloc,
            mtime: &self.mtime,
            ctime: &self.ctime,
            atime: &self.atime,
            ino: &self.ino,
            nlink: &self.nlink,
        }
    }

    fn len(&self) -> usize {
        self.name_lens.len()
    }

    fn name_at(&self, i: usize) -> &[u8] {
        let start: usize = self.name_lens[..i].iter().map(|&l| l as usize).sum();
        &self.names[start..start + self.name_lens[i] as usize]
    }
}

/// A node store of plain vectors (`name_off` 64-bit, or 32-bit with `narrow`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Store {
    parent: Vec<i32>,
    depth: Vec<u16>,
    kind: Vec<u8>,
    flags: Vec<u32>,
    name_len: Vec<u16>,
    ext: Vec<u16>,
    size: Vec<i64>,
    alloc: Vec<i64>,
    mtime: Vec<u32>,
    ctime: Vec<u32>,
    atime: Vec<u32>,
    ino: Vec<u64>,
    nlink: Vec<u16>,
    child_start: Vec<i32>,
    child_count: Vec<i32>,
    tot_size: Vec<i64>,
    tot_alloc: Vec<i64>,
    tot_files: Vec<i32>,
    tot_dirs: Vec<i32>,
    off: Vec<u64>,
    off32: Vec<u32>,
    narrow: bool,
    heap: Vec<u8>,
}

/// What new rows hold before a write fills them: anything.
trait Junk: Copy {
    const JUNK: Self;
}
macro_rules! junk {
    ($($t:ty = $v:expr),*) => {$(impl Junk for $t { const JUNK: $t = $v; })*};
}
junk!(
    u8 = 0xA5,
    u16 = 0xA5A5,
    u32 = 0xA5A5_A5A5,
    u64 = 0xA5A5_A5A5_A5A5_A5A5,
    i32 = 0x5A5A_5A5A,
    i64 = -0x5A5A_5A5A_5A5A_5A5A
);

fn grow_junk<T: Junk>(v: &mut Vec<T>, n: usize) {
    v.resize(n, T::JUNK);
}

impl Store {
    /// Node 0 and two roots (folders, `PENDING`), as the store starts a scan; names `\0name`
    /// each, a name's offset just past its NUL.
    fn new(narrow: bool) -> Store {
        let mut s = Store {
            narrow,
            ..Store::default()
        };
        for (parent, name, f) in [
            (-1, &b""[..], flag::VIRTUAL),
            (0, b"C:\\a", flag::PENDING),
            (0, b"D:\\b", flag::PENDING),
        ] {
            s.heap.push(0);
            s.off.push(s.heap.len() as u64);
            s.off32.push(s.heap.len() as u32);
            s.heap.extend_from_slice(name);
            s.parent.push(parent);
            s.depth.push((parent + 1) as u16);
            s.kind.push(kind::DIR);
            s.flags.push(f);
            s.name_len.push(name.len() as u16);
            s.ext.push(0);
            s.size.push(0);
            s.alloc.push(0);
            s.mtime.push(0);
            s.ctime.push(0);
            s.atime.push(0);
            s.ino.push(0);
            s.nlink.push(1);
            s.child_start.push(0);
            s.child_count.push(0);
            s.tot_size.push(0);
            s.tot_alloc.push(0);
            s.tot_files.push(0);
            s.tot_dirs.push(0);
        }
        s.child_start[0] = 1;
        s.child_count[0] = 2;
        s.tot_dirs[0] = 2;
        s
    }

    fn len(&self) -> usize {
        self.parent.len()
    }

    /// Where a node's name starts in the heap.
    fn off_of(&self, i: usize) -> usize {
        if self.narrow {
            self.off32[i] as usize
        } else {
            self.off[i] as usize
        }
    }

    fn name(&self, i: usize) -> &[u8] {
        let o = self.off_of(i);
        &self.heap[o..o + self.name_len[i] as usize]
    }

    /// `rows` more rows of junk and `bytes` more heap bytes of junk.
    fn grow(&mut self, rows: usize, bytes: usize) {
        let n = self.len() + rows;
        grow_junk(&mut self.parent, n);
        grow_junk(&mut self.depth, n);
        grow_junk(&mut self.kind, n);
        grow_junk(&mut self.flags, n);
        grow_junk(&mut self.name_len, n);
        grow_junk(&mut self.ext, n);
        grow_junk(&mut self.size, n);
        grow_junk(&mut self.alloc, n);
        grow_junk(&mut self.mtime, n);
        grow_junk(&mut self.ctime, n);
        grow_junk(&mut self.atime, n);
        grow_junk(&mut self.ino, n);
        grow_junk(&mut self.nlink, n);
        grow_junk(&mut self.child_start, n);
        grow_junk(&mut self.child_count, n);
        grow_junk(&mut self.tot_size, n);
        grow_junk(&mut self.tot_alloc, n);
        grow_junk(&mut self.tot_files, n);
        grow_junk(&mut self.tot_dirs, n);
        grow_junk(&mut self.off, n);
        grow_junk(&mut self.off32, n);
        let bytes = self.heap.len() + bytes;
        grow_junk(&mut self.heap, bytes);
    }

    /// Rows `from..from + rows` and the heap's bytes `at..at + bytes`, lent to a write.
    fn lend(&mut self, from: usize, rows: usize, at: usize, bytes: usize) -> RowsOut<'_> {
        let r = from..from + rows;
        let offsets = if self.narrow {
            NameOffsets::Narrow(&mut self.off32[r.clone()])
        } else {
            NameOffsets::Wide(&mut self.off[r.clone()])
        };
        RowsOut {
            cols: NodeColumns {
                parent: &mut self.parent[r.clone()],
                depth: &mut self.depth[r.clone()],
                kind: &mut self.kind[r.clone()],
                flags: &mut self.flags[r.clone()],
                name_len: &mut self.name_len[r.clone()],
                ext: &mut self.ext[r.clone()],
                size: &mut self.size[r.clone()],
                alloc: &mut self.alloc[r.clone()],
                mtime: &mut self.mtime[r.clone()],
                ctime: &mut self.ctime[r.clone()],
                atime: &mut self.atime[r.clone()],
                ino: &mut self.ino[r.clone()],
                nlink: &mut self.nlink[r.clone()],
                child_start: &mut self.child_start[r.clone()],
                child_count: &mut self.child_count[r.clone()],
                tot_size: &mut self.tot_size[r.clone()],
                tot_alloc: &mut self.tot_alloc[r.clone()],
                tot_files: &mut self.tot_files[r.clone()],
                tot_dirs: &mut self.tot_dirs[r],
            },
            offsets,
            names: &mut self.heap[at..at + bytes],
            names_at: at as u64,
        }
    }
    /// Adds `t` to `node`'s totals and its ancestors'.
    fn propagate(&mut self, node: i32, t: Totals) {
        let mut v = node;
        while v >= 0 {
            let i = v as usize;
            self.tot_size[i] = self.tot_size[i].wrapping_add(t[0]);
            self.tot_alloc[i] = self.tot_alloc[i].wrapping_add(t[1]);
            self.tot_files[i] = self.tot_files[i].wrapping_add(t[2] as i32);
            self.tot_dirs[i] = self.tot_dirs[i].wrapping_add(t[3] as i32);
            v = self.parent[i];
        }
    }

    /// A slice the way the store folds it: plan, grow, write, publish.
    fn fold(
        &mut self,
        folders: &[Folder],
        bounds: &[u32],
        b: &Batch,
    ) -> Result<Vec<Option<u32>>, FoldError> {
        let mut plan = SliceFold::plan(&*self, folders, bounds, &b.cols())?;
        assert_eq!(plan.first_row(), self.len());
        let (n0, at) = (self.len(), self.heap.len());
        self.grow(plan.rows(), plan.name_bytes());
        plan.write(self.lend(n0, plan.rows(), at, plan.name_bytes()), &b.cols());
        let listed: Vec<Listed> = plan.listed_nodes().collect();
        for l in listed {
            let d = l.node as usize;
            self.flags[d] &= !flag::PENDING;
            self.child_start[d] = l.start as i32;
            self.child_count[d] = l.count as i32;
            if l.totals != [0; 4] {
                self.propagate(d as i32, l.totals);
            }
        }
        Ok(plan.starts())
    }

    /// The reference: one listing at a time, row by row.
    fn fold_listings(&mut self, folders: &[Folder], bounds: &[u32], b: &Batch) -> Vec<u32> {
        let mut starts: Vec<u32> = Vec::new();
        for (k, f) in folders.iter().enumerate() {
            let dir = match *f {
                Folder::Node(d) => d as usize,
                Folder::Entry { listing, entry } => (starts[listing as usize] + entry) as usize,
            };
            let start = self.len();
            starts.push(start as u32);
            let mut acc: Totals = [0; 4];
            for i in bounds[k] as usize..bounds[k + 1] as usize {
                let (kd, f) = (b.kind[i], b.flags[i]);
                let at = self.heap.len() as u64 + 1;
                self.heap.push(0);
                self.heap.extend_from_slice(b.name_at(i));
                self.parent.push(dir as i32);
                self.depth.push(self.depth[dir].wrapping_add(1));
                self.kind.push(kd);
                self.flags.push(if kd == kind::DIR && f & flag::MOUNT == 0 {
                    f | flag::PENDING
                } else {
                    f
                });
                self.name_len.push(b.name_lens[i]);
                self.ext.push(b.ext[i]);
                self.size.push(b.size[i]);
                self.alloc.push(b.alloc[i]);
                self.mtime.push(b.mtime[i]);
                self.ctime.push(b.ctime[i]);
                self.atime.push(b.atime[i]);
                self.ino.push(b.ino[i]);
                self.nlink.push(b.nlink[i]);
                for v in [
                    &mut self.child_start,
                    &mut self.child_count,
                    &mut self.tot_files,
                    &mut self.tot_dirs,
                ] {
                    v.push(0);
                }
                self.tot_size.push(0);
                self.tot_alloc.push(0);
                self.off.push(at);
                self.off32.push(at as u32);
                if f & flag::HARDLINK_DUP == 0 {
                    acc[0] = acc[0].wrapping_add(b.size[i]);
                    acc[1] = acc[1].wrapping_add(b.alloc[i]);
                }
                acc[2] += (kd == kind::FILE) as i64;
                acc[3] += (kd == kind::DIR) as i64;
            }
            self.flags[dir] &= !flag::PENDING;
            self.child_start[dir] = start as i32;
            self.child_count[dir] = (bounds[k + 1] - bounds[k]) as i32;
            self.propagate(dir as i32, acc);
        }
        starts
    }
}

impl Tree for Store {
    fn rows(&self) -> usize {
        self.len()
    }
    fn flags(&self, node: u32) -> u32 {
        self.flags[node as usize]
    }
    fn is_dir(&self, node: u32) -> bool {
        self.kind[node as usize] == kind::DIR
    }
    fn child_count(&self, node: u32) -> i32 {
        self.child_count[node as usize]
    }
    fn depth(&self, node: u32) -> u16 {
        self.depth[node as usize]
    }
}

/// A slice: its folders, bounds, entries, and the folders it found but did not list.
type Slice = (Vec<Folder>, Vec<u32>, Batch, Vec<(u32, u32)>);

/// A random slice over `pending` store folders and folders found earlier in the slice: its
/// folders, bounds, entries, and the folders it found but did not list.
fn random_slice(rng: &mut Rng, pending: &mut Vec<u32>) -> Slice {
    let mut folders = Vec::new();
    let mut found: Vec<(u32, u32)> = Vec::new();
    let mut bounds = vec![0u32];
    let mut b = Batch::default();
    for k in 0..rng.below(14) as u32 {
        if !found.is_empty() && (pending.is_empty() || rng.chance(0.7)) {
            let (j, i) = found.swap_remove(rng.below(found.len() as u64) as usize);
            folders.push(Folder::Entry {
                listing: j,
                entry: i,
            });
        } else if !pending.is_empty() {
            folders.push(Folder::Node(
                pending.swap_remove(rng.below(pending.len() as u64) as usize),
            ));
        } else {
            break;
        }
        for i in 0..rng.below(7) as u32 {
            let k_ =
                [kind::FILE, kind::FILE, kind::DIR, kind::LINK, kind::OTHER][rng.below(5) as usize];
            let mut f = [
                0,
                flag::HIDDEN,
                flag::HARDLINK_DUP,
                flag::HIDDEN | flag::HARDLINK_DUP,
            ][rng.below(4) as usize];
            if k_ == kind::DIR && rng.chance(0.15) {
                f |= flag::MOUNT;
            }
            let size = if rng.chance(0.3) {
                rng.below(1 << 45) as i64
            } else {
                rng.below(5000) as i64
            };
            let name = format!("n{k}.{i}{}", "é".repeat(rng.below(3) as usize));
            let seed = rng.next();
            let size = if k_ == kind::DIR { 0 } else { size };
            b.push(name.as_bytes(), k_, f, size, size / 2, seed);
            if k_ == kind::DIR && f & flag::MOUNT == 0 {
                found.push((k, i));
            }
        }
        bounds.push(b.len() as u32);
    }
    (folders, bounds, b, found)
}

/// Every node's columns and name, compared with the reference.
fn assert_same(a: &Store, b: &Store, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: rows");
    let cols = |s: &Store| {
        (
            s.parent.clone(),
            s.depth.clone(),
            s.kind.clone(),
            s.flags.clone(),
            s.name_len.clone(),
            s.ext.clone(),
            s.size.clone(),
            s.alloc.clone(),
            (s.mtime.clone(), s.ctime.clone(), s.atime.clone()),
            (s.ino.clone(), s.nlink.clone()),
            (s.child_start.clone(), s.child_count.clone()),
            (
                s.tot_size.clone(),
                s.tot_alloc.clone(),
                s.tot_files.clone(),
                s.tot_dirs.clone(),
            ),
        )
    };
    assert_eq!(cols(a), cols(b), "{what}: columns");
    assert_eq!(a.heap, b.heap, "{what}: names");
    for i in 0..a.len() {
        assert_eq!(a.name(i), b.name(i), "{what}: name of {i}");
    }
}

#[test]
fn random_slices_fold_as_listings_one_at_a_time() {
    for case in 0..300u64 {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (case + 1));
        let narrow = case % 2 == 0;
        let (mut got, mut want) = (Store::new(narrow), Store::new(narrow));
        let mut pending = vec![1, 2];
        for round in 0..1 + rng.below(4) {
            let (folders, bounds, b, left) = random_slice(&mut rng, &mut pending);
            let starts = got.fold(&folders, &bounds, &b).unwrap();
            let refs = want.fold_listings(&folders, &bounds, &b);
            assert_eq!(
                starts,
                refs.iter().map(|&s| Some(s)).collect::<Vec<_>>(),
                "case {case}"
            );
            assert_same(&got, &want, &format!("case {case} round {round}"));
            pending.extend(left.iter().map(|&(j, i)| starts[j as usize].unwrap() + i));
        }
    }
}

#[test]
fn listings_of_folders_that_left_the_tree_are_skipped() {
    let mut s = Store::new(false);
    s.flags[2] |= flag::GONE;
    let mut b = Batch::default();
    b.push(b"kept-dir", kind::DIR, 0, 0, 0, 1); // listing 0 (node 1)
    b.push(b"kept.txt", kind::FILE, 0, 10, 4096, 2);
    b.push(b"gone-dir", kind::DIR, 0, 0, 0, 3); // listing 1 (node 2, gone)
    b.push(b"inside-gone", kind::FILE, 0, 7, 7, 4); // listing 2: entry 0 of listing 1
    b.push(b"inside-kept", kind::FILE, 0, 5, 8, 5); // listing 3: entry 0 of listing 0
    let folders = [
        Folder::Node(1),
        Folder::Node(2),
        Folder::Entry {
            listing: 1,
            entry: 0,
        },
        Folder::Entry {
            listing: 0,
            entry: 0,
        },
    ];
    let bounds = [0, 2, 3, 4, 5];
    let plan = SliceFold::plan(&s, &folders, &bounds, &b.cols()).unwrap();
    assert_eq!(plan.kept(), 2);
    assert_eq!(plan.rows(), 3);
    assert_eq!(
        plan.name_bytes(),
        "kept-dir".len() + "kept.txt".len() + "inside-kept".len() + 3
    );
    let starts = s.fold(&folders, &bounds, &b).unwrap();
    assert_eq!(starts, vec![Some(3), None, None, Some(5)]);
    assert_eq!(s.len(), 6);
    assert_eq!(s.name(3), b"kept-dir");
    assert_eq!(s.name(4), b"kept.txt");
    assert_eq!(s.name(5), b"inside-kept");
    assert_eq!(s.parent[5], 3);
    assert_eq!(s.flags[3], 0, "listed in the slice: not PENDING");
    assert_eq!((s.child_start[3], s.child_count[3]), (5, 1));
    assert_eq!(
        (s.tot_size[3], s.tot_alloc[3], s.tot_files[3], s.tot_dirs[3]),
        (5, 8, 1, 0)
    );
    assert_eq!(
        (s.tot_size[1], s.tot_alloc[1], s.tot_files[1], s.tot_dirs[1]),
        (15, 4104, 2, 1)
    );
    assert_eq!(s.flags[1], 0);
    assert_eq!(s.child_count[2], 0, "the gone folder is not listed");
    // only gone listings: nothing to fold
    let plan = SliceFold::plan(
        &s,
        &[Folder::Node(2)],
        &[0, 1],
        &{
            let mut b = Batch::default();
            b.push(b"x", kind::FILE, 0, 1, 1, 9);
            b
        }
        .cols(),
    )
    .unwrap();
    assert_eq!((plan.kept(), plan.rows(), plan.name_bytes()), (0, 0, 0));
    assert_eq!(plan.starts(), vec![None]);
}

#[test]
fn bad_slices_are_refused_before_anything_is_written() {
    let s = Store::new(false);
    let mut one = Batch::default();
    one.push(b"f", kind::FILE, 0, 1, 1, 1);
    let mut two = one.clone();
    two.push(b"g", kind::FILE, 0, 1, 1, 2);
    let mut torn = one.clone();
    torn.size.pop();
    let mut short_names = one.clone();
    short_names.names.pop();
    let cases: Vec<(Vec<Folder>, Vec<u32>, &Batch, FoldError)> = vec![
        (
            vec![Folder::Entry {
                listing: 0,
                entry: 0,
            }],
            vec![0, 1],
            &one,
            FoldError::BadSlice("a listing can only name an entry of an earlier listing"),
        ),
        (
            vec![Folder::Node(99)],
            vec![0, 1],
            &one,
            FoldError::BadSlice("a listing names a node that is not in the store"),
        ),
        (
            vec![
                Folder::Node(1),
                Folder::Entry {
                    listing: 0,
                    entry: 0,
                },
            ],
            vec![0, 1, 2],
            &two,
            FoldError::NotADirectory(3), // entry 0 of listing 0 (row 3) is a file
        ),
        (
            vec![
                Folder::Node(1),
                Folder::Entry {
                    listing: 0,
                    entry: 5,
                },
            ],
            vec![0, 1, 2],
            &two,
            FoldError::BadSlice("a listing names an entry its listing does not have"),
        ),
        (
            vec![Folder::Node(1), Folder::Node(1)],
            vec![0, 1, 2],
            &two,
            FoldError::AlreadyListed(1),
        ),
        (
            vec![Folder::Node(0)],
            vec![0, 1],
            &one,
            FoldError::AlreadyListed(0), // node 0 has its roots
        ),
        (
            vec![Folder::Node(1)],
            vec![0, 2],
            &one,
            FoldError::BadSlice("bounds do not match the entries"),
        ),
        (
            vec![Folder::Node(1)],
            vec![0, 1],
            &torn,
            FoldError::BadBatch,
        ),
        (
            vec![Folder::Node(1)],
            vec![0, 1],
            &short_names,
            FoldError::BadBatch,
        ),
    ];
    for (folders, bounds, b, want) in cases {
        assert_eq!(
            SliceFold::plan(&s, &folders, &bounds, &b.cols()).map(|_| ()),
            Err(want)
        );
    }
    // a file is not a folder to list
    let mut f = s.clone();
    f.kind[2] = kind::FILE;
    assert_eq!(
        SliceFold::plan(&f, &[Folder::Node(2)], &[0, 1], &one.cols()).map(|_| ()),
        Err(FoldError::NotADirectory(2))
    );
    // an empty slice folds to nothing
    let plan = SliceFold::plan(&s, &[], &[0], &Batch::default().cols()).unwrap();
    assert_eq!((plan.kept(), plan.rows(), plan.starts()), (0, 0, vec![]));
}

#[test]
fn write_rows_fills_every_lent_row() {
    for (narrow, pending) in [(false, true), (true, false)] {
        let mut s = Store::new(narrow);
        let mut b = Batch::default();
        b.push(b"sub", kind::DIR, 0, 0, 4096, 1);
        b.push(b"mnt", kind::DIR, flag::MOUNT, 0, 0, 2);
        b.push(
            "файл.txt".as_bytes(),
            kind::FILE,
            flag::HIDDEN,
            100,
            4096,
            3,
        );
        b.push(b"link2", kind::FILE, flag::HARDLINK_DUP, 100, 4096, 4);
        b.push(b"", kind::OTHER, 0, 0, 0, 5);
        let (n0, at) = (s.len(), s.heap.len());
        let need = b.names.len() + b.len();
        s.grow(b.len(), need);
        // one more row and heap byte past the lent ones: never touched
        s.grow(1, 1);
        let end = s.len() - 1;
        let before = (s.parent[end], s.flags[end], *s.heap.last().unwrap());
        let totals = write_rows(s.lend(n0, 5, at, need), &b.cols(), 1, 2, pending);
        // the dup's bytes do not count, the folders' own allocation does
        assert_eq!(totals, [100, 4096 + 4096, 2, 2]);
        assert_eq!(
            (s.parent[end], s.flags[end], *s.heap.last().unwrap()),
            before
        );
        for (j, i) in (n0..n0 + 5).enumerate() {
            assert_eq!(s.parent[i], 1);
            assert_eq!(s.depth[i], 2);
            assert_eq!(s.name(i), b.name_at(j));
            assert_eq!(s.heap[s.off_of(i) - 1], 0, "the NUL before");
            assert_eq!(
                (
                    s.child_start[i],
                    s.child_count[i],
                    s.tot_files[i],
                    s.tot_dirs[i]
                ),
                (0, 0, 0, 0)
            );
            assert_eq!((s.tot_size[i], s.tot_alloc[i]), (0, 0));
            assert_eq!(
                (s.ino[i], s.nlink[i], s.ext[i]),
                (b.ino[j], b.nlink[j], b.ext[j])
            );
            assert_eq!(
                (s.mtime[i], s.ctime[i], s.atime[i]),
                (b.mtime[j], b.ctime[j], b.atime[j])
            );
        }
        let pend = |i: usize| s.flags[i] & flag::PENDING != 0;
        assert_eq!(pend(n0), pending, "a folder still to list");
        assert!(!pend(n0 + 1), "never a mount point");
        assert!(!pend(n0 + 2) && !pend(n0 + 3) && !pend(n0 + 4));
        assert_eq!(s.flags[n0 + 2], flag::HIDDEN);
    }
}

#[test]
#[should_panic(expected = "do not fit")]
fn rows_of_another_shape_are_refused() {
    let mut s = Store::new(false);
    let mut b = Batch::default();
    b.push(b"a", kind::FILE, 0, 1, 1, 1);
    let (n0, at) = (s.len(), s.heap.len());
    s.grow(2, 2); // one row too many
    write_rows(s.lend(n0, 2, at, 2), &b.cols(), 1, 2, true);
}
