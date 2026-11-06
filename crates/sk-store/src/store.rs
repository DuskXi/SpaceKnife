//! The columnar node store: [`Store`], its read view [`Nodes`] and the three guards
//! [`Reader`], [`Appender`] and [`Writer`]. See the crate docs for the layout invariants and the
//! threading contract.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut, Range};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, MutexGuard, RwLock};

use crate::lock::{ReadGuard, StructureLock, WriteGuard};

use crate::heap::{self, Chunk, HeapWriter, chunk_of};
use crate::mapped::{Buf, Policy, RamUser};
use crate::names::{self, decode_lossy, join_into, name_to_os, normcase, split_below};
use crate::types::*;

/// Column growth while a scan adds rows (docs/memory.md §3).
pub const GROWTH: f64 = 1.5;
/// Once a store is complete (compacted or loaded), a change adds 1/16 of its rows ...
pub const SETTLED_SHARE: usize = 16;
/// ... or this many, whichever is more.
pub const SETTLED_MIN_STEP: usize = 1 << 16;
/// Name offsets fit u32 below this many bytes of names; past it `name_off` widens to u64.
pub const OFF_NARROW_LIMIT: u64 = 1 << 32;
/// Default capacity of a new store (rows).
pub const DEFAULT_CAPACITY: usize = 1 << 14;

const MAX_NODES: usize = i32::MAX as usize;

// --------------------------------------------------------------------------- columns

/// One generation of the columns: every column a never-moving buffer. A grown or compacted
/// column is a new buffer in a new `ColSet`; readers holding the old set keep valid data.
pub(crate) struct ColSet {
    pub(crate) bufs: [Arc<Buf>; NCOL],
    pub(crate) wide: bool,
}

impl ColSet {
    fn alloc(cap: usize, wide: bool, alloc: &mut dyn FnMut(usize) -> Buf) -> ColSet {
        let bufs = Col::ALL.map(|c| Arc::new(alloc(cap * item_of(c, wide))));
        ColSet { bufs, wide }
    }

    #[inline]
    pub(crate) fn item(&self, c: Col) -> usize {
        item_of(c, self.wide)
    }

    #[inline]
    pub(crate) fn p<T>(&self, c: Col) -> *mut T {
        self.bufs[c as usize].ptr() as *mut T
    }

    fn rows(&self, c: Col) -> usize {
        self.bufs[c as usize].bytes() / self.item(c)
    }

    fn cap(&self) -> usize {
        Col::ALL.iter().map(|&c| self.rows(c)).min().unwrap_or(0)
    }

    fn ram(&self) -> usize {
        self.bufs.iter().map(|b| b.ram()).sum()
    }

    fn with(&self, c: Col, buf: Buf, wide: bool) -> ColSet {
        let mut bufs = self.bufs.clone();
        bufs[c as usize] = Arc::new(buf);
        ColSet { bufs, wide }
    }
}

#[inline]
fn item_of(c: Col, wide: bool) -> usize {
    if c == Col::NameOff && wide {
        8
    } else {
        c.item()
    }
}

// --------------------------------------------------------------------------- the store

/// How a new store is set up. The defaults are what the app uses; tests shrink the sizes.
#[derive(Clone)]
pub struct StoreOptions {
    /// rows reserved up front
    pub capacity: usize,
    pub policy: Arc<Policy>,
    /// first name-heap chunk (doubles up to `max_chunk`)
    pub first_chunk: usize,
    pub max_chunk: usize,
    /// smallest growth of a complete store ([`SETTLED_MIN_STEP`])
    pub settled_min_step: usize,
    /// name bytes past which `name_off` widens ([`OFF_NARROW_LIMIT`])
    pub off_narrow_limit: u64,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions {
            capacity: DEFAULT_CAPACITY,
            policy: Policy::global().clone(),
            first_chunk: heap::FIRST_CHUNK,
            max_chunk: heap::CHUNK,
            settled_min_step: SETTLED_MIN_STEP,
            off_narrow_limit: OFF_NARROW_LIMIT,
        }
    }
}

#[derive(Clone)]
struct Published {
    n: usize,
    cols: Arc<ColSet>,
    chunks: Arc<[Chunk]>,
}

pub(crate) struct ExtTable {
    names: Vec<Arc<str>>,
    index: HashMap<Arc<str>, u16>,
}

impl ExtTable {
    fn new(names: Vec<String>) -> ExtTable {
        let mut names: Vec<Arc<str>> = names.into_iter().map(Arc::from).collect();
        if names.is_empty() {
            names.push(Arc::from(""));
        }
        let index = names
            .iter()
            .enumerate()
            .map(|(i, e)| (e.clone(), i as u16))
            .collect();
        ExtTable { names, index }
    }
}

struct AppendState {
    n: usize,
    cap: usize,
    cols: Arc<ColSet>,
    heap: HeapWriter,
    chunks: Arc<[Chunk]>,
    mapped: bool,
    settled: bool,
    opts: StoreOptions,
}

pub(crate) struct Inner {
    structure: StructureLock,
    append: Mutex<AppendState>,
    published: Mutex<Published>,
    ext: RwLock<ExtTable>,
    version: AtomicU64,
    ram: AtomicUsize,
    policy: Arc<Policy>,
}

impl RamUser for Inner {
    fn ram_bytes(&self) -> usize {
        self.ram.load(Ordering::Relaxed)
    }
}

/// A scanned tree: a cheap, cloneable handle (an `Arc`) to columns that readers, one appender
/// and an exclusive writer share. See the crate docs.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl Default for Store {
    fn default() -> Self {
        Store::new()
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("len", &self.len())
            .field("version", &self.version())
            .finish()
    }
}

impl Store {
    /// An empty store: node 0 (the virtual top) only.
    pub fn new() -> Store {
        Store::with_options(StoreOptions::default())
    }

    pub fn with_capacity(capacity: usize) -> Store {
        Store::with_options(StoreOptions {
            capacity,
            ..StoreOptions::default()
        })
    }

    pub fn with_options(opts: StoreOptions) -> Store {
        let cap = opts.capacity.max(16);
        let cols = Arc::new(ColSet::alloc(cap, false, &mut Buf::heap));
        let heap = HeapWriter::new(opts.first_chunk, opts.max_chunk);
        let store = Store::from_state(
            AppendState {
                n: 0,
                cap,
                cols,
                heap,
                chunks: Arc::from(Vec::new()),
                mapped: false,
                settled: false,
                opts,
            },
            ExtTable::new(Vec::new()),
        );
        {
            let mut app = store.appender();
            app.append_one(-1, 0, Kind::Dir, Flags::VIRTUAL, b"", Times::default())
                .expect("node 0");
        }
        store.inner.version.store(0, Ordering::Relaxed);
        store
    }

    fn from_state(st: AppendState, ext: ExtTable) -> Store {
        let policy = st.opts.policy.clone();
        let published = Published {
            n: st.n,
            cols: st.cols.clone(),
            chunks: st.chunks.clone(),
        };
        let ram = st.cols.ram() + st.heap.reserved_in_ram();
        let inner = Arc::new(Inner {
            structure: StructureLock::default(),
            append: Mutex::new(st),
            published: Mutex::new(published),
            ext: RwLock::new(ext),
            version: AtomicU64::new(0),
            ram: AtomicUsize::new(ram),
            policy: policy.clone(),
        });
        let weak: std::sync::Weak<dyn RamUser> =
            Arc::downgrade(&(inner.clone() as Arc<dyn RamUser>));
        policy.register(weak);
        Store { inner }
    }

    fn snapshot(&self) -> Nodes {
        let p = self.inner.published.lock().clone();
        Nodes {
            n: p.n,
            cols: p.cols,
            chunks: p.chunks,
            inner: self.inner.clone(),
        }
    }

    /// Shared access for a computation (render, summary, search, snapshot save). Blocks while a
    /// [`Writer`] holds the store, and while one waits that no longer waits for an older reader. The view is the store as it was when the guard
    /// was taken, plus whatever a running scan publishes into the atomic columns meanwhile.
    pub fn read(&self) -> Reader<'_> {
        let guard = self.inner.structure.read();
        Reader {
            nodes: self.snapshot(),
            _guard: guard,
        }
    }

    /// The single appender (the scanner). Coexists with readers; excludes other appenders and
    /// writers. Never take [`Store::read`] or [`Store::write`] on a thread that holds one.
    pub fn appender(&self) -> Appender<'_> {
        let st = self.inner.append.lock();
        let view = Nodes {
            n: st.n,
            cols: st.cols.clone(),
            chunks: st.chunks.clone(),
            inner: self.inner.clone(),
        };
        Appender {
            inner: &self.inner,
            st,
            view,
        }
    }

    /// Exclusive access (file-change monitor, adopting a compacted store): waits for the readers
    /// running when it asked, then for the appender. Readers that arrive while those older readers
    /// still run start anyway; once they are done, new readers wait for this writer (neither side
    /// starves; see the crate docs).
    pub fn write(&self) -> Writer<'_> {
        let guard = self.inner.structure.write();
        let app = self.appender();
        Writer { app, _guard: guard }
    }

    /// [`Store::write`] that gives up at `deadline` (returns `None`): a writer that stops waiting lets
    /// the readers it held back go on. The deadline covers the structure lock only; the appender
    /// (a running scan's) is waited for after it.
    pub fn write_until(&self, deadline: Instant) -> Option<Writer<'_>> {
        let guard = self.inner.structure.write_until(Some(deadline))?;
        let app = self.appender();
        Some(Writer { app, _guard: guard })
    }

    /// [`Store::write_until`] `now + wait`.
    pub fn try_write_for(&self, wait: Duration) -> Option<Writer<'_>> {
        self.write_until(Instant::now() + wait)
    }

    /// Published node count.
    pub fn len(&self) -> usize {
        self.inner.published.lock().n
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bumped by every change (caches key on it).
    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Acquire)
    }

    /// Bytes held in plain memory: columns (with headroom) and name chunks.
    pub fn ram_bytes(&self) -> usize {
        self.inner.ram_bytes()
    }

    pub fn policy(&self) -> &Arc<Policy> {
        &self.inner.policy
    }

    /// The extension table (index = `ext` column value).
    pub fn ext_table(&self) -> Vec<Arc<str>> {
        self.inner.ext.read().names.clone()
    }

    pub fn ptr_eq(&self, other: &Store) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

// --------------------------------------------------------------------------- read view

/// A consistent view of the first [`Nodes::len`] rows: every accessor of the tree. Reached
/// through a [`Reader`], an [`Appender`] or a [`Writer`] (all deref to it).
pub struct Nodes {
    n: usize,
    cols: Arc<ColSet>,
    chunks: Arc<[Chunk]>,
    inner: Arc<Inner>,
}

macro_rules! plain_col {
    ($(#[$m:meta])* $all:ident, $one:ident, $col:expr, $t:ty) => {
        $(#[$m])*
        #[inline]
        pub fn $all(&self) -> &[$t] {
            // SAFETY: rows below `n` were published before this view was made and are only
            // written again under the exclusive Writer (which no view can coexist with).
            unsafe { std::slice::from_raw_parts(self.cols.p::<$t>($col), self.n) }
        }
        #[inline]
        pub fn $one(&self, id: NodeId) -> $t {
            self.$all()[id as usize]
        }
    };
}

macro_rules! atomic_col {
    ($(#[$m:meta])* $all:ident, $one:ident, $col:expr, $at:ty, $t:ty) => {
        $(#[$m])*
        #[inline]
        pub fn $all(&self) -> &[$at] {
            // SAFETY: zero-initialised memory of the atomic's size and alignment; these columns
            // are only ever accessed atomically while views exist.
            unsafe { std::slice::from_raw_parts(self.cols.p::<$at>($col), self.n) }
        }
        #[inline]
        pub fn $one(&self, id: NodeId) -> $t {
            self.$all()[id as usize].load(Ordering::Relaxed)
        }
    };
}

/// How [`Nodes::sorted_children`] orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    /// effective allocated bytes
    Size,
    /// name, case-insensitively
    Name,
}

/// Names of consecutive nodes in one heap chunk ([`Nodes::names_run`]).
pub struct NamesRun<'a> {
    /// first node after the run
    pub stop: NodeId,
    /// `\0name\0name…` for nodes `a..stop`
    pub run: &'a [u8],
}

impl Nodes {
    /// Number of nodes in this view.
    #[inline]
    pub fn len(&self) -> usize {
        self.n
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    plain_col!(
        /// parent id, -1 for node 0 and `GONE` rows
        parents, parent, Col::Parent, i32);
    plain_col!(depths, depth, Col::Depth, u16);
    plain_col!(kinds_raw, kind_raw, Col::Kind, u8);
    plain_col!(name_lens, name_len, Col::NameLen, u16);
    plain_col!(exts, ext, Col::Ext, u16);
    plain_col!(sizes, size, Col::Size, i64);
    plain_col!(allocs, alloc, Col::Alloc, i64);
    plain_col!(mtimes, mtime, Col::Mtime, u32);
    plain_col!(ctimes, ctime, Col::Ctime, u32);
    plain_col!(atimes, atime, Col::Atime, u32);
    plain_col!(inos, ino, Col::Ino, u64);
    plain_col!(nlinks, nlink, Col::Nlink, u16);
    atomic_col!(flags_col, flags_raw, Col::Flags, AtomicU32, u32);
    atomic_col!(child_starts, child_start, Col::ChildStart, AtomicI32, i32);
    atomic_col!(child_counts, child_count, Col::ChildCount, AtomicI32, i32);
    atomic_col!(tot_sizes, tot_size, Col::TotSize, AtomicI64, i64);
    atomic_col!(tot_allocs, tot_alloc, Col::TotAlloc, AtomicI64, i64);
    atomic_col!(tot_files_col, tot_files, Col::TotFiles, AtomicI32, i32);
    atomic_col!(tot_dirs_col, tot_dirs, Col::TotDirs, AtomicI32, i32);

    #[inline]
    pub fn kind(&self, id: NodeId) -> Kind {
        Kind::from_u8(self.kind_raw(id))
    }

    #[inline]
    pub fn flags(&self, id: NodeId) -> Flags {
        Flags(self.flags_raw(id))
    }

    #[inline]
    pub fn is_dir(&self, id: NodeId) -> bool {
        self.kind_raw(id) == Kind::Dir as u8
    }

    /// Global heap offset of a node's name.
    #[inline]
    pub fn name_off(&self, id: NodeId) -> u64 {
        let i = id as usize;
        assert!(i < self.n, "node {id} out of range");
        // SAFETY: in range; the column holds u32 or u64 per `wide`.
        unsafe {
            if self.cols.wide {
                self.cols.p::<u64>(Col::NameOff).add(i).read()
            } else {
                self.cols.p::<u32>(Col::NameOff).add(i).read() as u64
            }
        }
    }

    /// Whether `name_off` is 64-bit (names past 4 GiB).
    pub fn wide_offsets(&self) -> bool {
        self.cols.wide
    }

    /// The bytes of a node's name (zero-copy).
    pub fn name_bytes(&self, id: NodeId) -> &[u8] {
        let off = self.name_off(id);
        let len = self.name_len(id) as usize;
        let k = chunk_of(&self.chunks, off);
        let chunk = &self.chunks[k];
        let start = (off - chunk.base) as usize;
        assert!(
            start + len <= chunk.cap(),
            "name of node {id} outside its chunk"
        );
        // SAFETY: a published name inside a chunk that never moves or changes.
        unsafe { std::slice::from_raw_parts(chunk.buf.ptr().add(start), len) }
    }

    /// The name for display (invalid sequences as U+FFFD).
    pub fn name(&self, id: NodeId) -> std::borrow::Cow<'_, str> {
        decode_lossy(self.name_bytes(id))
    }

    /// `(start, stop)` of a folder's children; safe while a scan adds listings.
    ///
    /// The count is read before the start (ingest publishes them the other way round), and a
    /// range that starts at node 0 or runs past this view reads as empty: a folder being listed
    /// at this very moment is simply not listed yet. (Children usually come after their folder,
    /// but a folder moved by a change keeps its old children: ranges may also lie before it.)
    #[inline]
    pub fn child_range(&self, id: NodeId) -> (NodeId, NodeId) {
        let count = self.child_counts()[id as usize].load(Ordering::Acquire);
        if count <= 0 {
            return (0, 0);
        }
        let start = self.child_starts()[id as usize].load(Ordering::Acquire);
        if start < 1 || start as usize + count as usize > self.n {
            return (0, 0);
        }
        (start as NodeId, (start + count) as NodeId)
    }

    pub fn children(&self, id: NodeId) -> Range<NodeId> {
        let (a, b) = self.child_range(id);
        a..b
    }

    pub fn roots(&self) -> Range<NodeId> {
        self.children(0)
    }

    /// `(logical, allocated)` bytes of a node: its subtree totals for folders, its own size
    /// otherwise (0 for hard-link duplicates).
    pub fn effective_size(&self, id: NodeId) -> (i64, i64) {
        if self.is_dir(id) {
            return (self.tot_size(id), self.tot_alloc(id));
        }
        if self.flags(id).intersects(Flags::HARDLINK_DUP) {
            return (0, 0);
        }
        (self.size(id), self.alloc(id))
    }

    /// The extension string of an `ext` id ("" when unknown).
    pub fn ext_name(&self, ext: u16) -> Arc<str> {
        let t = self.inner.ext.read();
        t.names
            .get(ext as usize)
            .cloned()
            .unwrap_or_else(|| t.names[0].clone())
    }

    /// Ancestors of a node, nearest first, below node 0.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut node = self.parent(id);
        std::iter::from_fn(move || {
            if node > 0 {
                let out = node as NodeId;
                node = self.parent(out);
                Some(out)
            } else {
                None
            }
        })
    }

    /// The path of a node as name bytes joined with the platform separator ("" for node 0).
    pub fn path_bytes(&self, id: NodeId) -> Vec<u8> {
        let mut parts = Vec::new();
        let mut node = id as i32;
        while node > 0 {
            parts.push(node as NodeId);
            node = self.parent(node as NodeId);
        }
        let mut out = Vec::new();
        for &p in parts.iter().rev() {
            join_into(&mut out, self.name_bytes(p));
        }
        out
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        PathBuf::from(name_to_os(&self.path_bytes(id)))
    }

    /// The path for display.
    pub fn path_lossy(&self, id: NodeId) -> String {
        decode_lossy(&self.path_bytes(id)).into_owned()
    }

    /// Children sorted by effective allocation or by name (stable).
    pub fn sorted_children(&self, id: NodeId, by: SortBy, descending: bool) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.children(id).collect();
        match by {
            SortBy::Size => {
                let key =
                    |i: NodeId| self.tot_alloc(i) + if self.is_dir(i) { 0 } else { self.alloc(i) };
                if descending {
                    ids.sort_by_key(|&i| std::cmp::Reverse(key(i)));
                } else {
                    ids.sort_by_key(|&i| key(i));
                }
            }
            SortBy::Name => {
                let mut keyed: Vec<(String, NodeId)> = ids
                    .iter()
                    .map(|&i| (self.name(i).to_lowercase(), i))
                    .collect();
                if descending {
                    keyed.sort_by(|a, b| b.0.cmp(&a.0));
                } else {
                    keyed.sort_by(|a, b| a.0.cmp(&b.0));
                }
                ids = keyed.into_iter().map(|(_, i)| i).collect();
            }
        }
        ids
    }

    pub fn view(&self, id: NodeId) -> NodeView {
        let (tot_size, tot_alloc) = self.effective_size(id);
        NodeView {
            id,
            name: self.name(id).into_owned(),
            kind: self.kind(id),
            flags: self.flags(id),
            parent: self.parent(id),
            depth: self.depth(id),
            size: self.size(id),
            alloc: self.alloc(id),
            tot_size,
            tot_alloc,
            tot_files: self.tot_files(id),
            tot_dirs: self.tot_dirs(id),
            mtime: self.mtime(id),
            ctime: self.ctime(id),
            atime: self.atime(id),
            child_count: self.child_count(id),
            ext: self.ext_name(self.ext(id)).to_string(),
        }
    }

    /// Node id for an absolute path (encoded bytes) inside one of the roots, or None. Each name
    /// is matched in its exact case first, case-insensitively ([`normcase`]) only when a single
    /// entry has it that way: a folder can hold names that differ only in case, and `A` must
    /// never resolve to `a`.
    pub fn find(&self, path: &[u8]) -> Option<NodeId> {
        for root in self.roots() {
            let Some(parts) = split_below(path, self.name_bytes(root)) else {
                continue;
            };
            let mut node = root;
            for part in &parts {
                let key = normcase(part);
                let mut found = None;
                let mut folded = Vec::new();
                for ch in self.children(node) {
                    let own = self.name_bytes(ch);
                    if own == part.as_slice() {
                        found = Some(ch);
                        break;
                    }
                    if normcase(own) == key {
                        folded.push(ch);
                    }
                }
                node = match found {
                    Some(f) => f,
                    None if folded.len() == 1 => folded[0],
                    None => return None,
                };
            }
            return Some(node);
        }
        None
    }

    /// [`Nodes::find`] for a `Path`.
    pub fn find_path(&self, path: &std::path::Path) -> Option<NodeId> {
        self.find(names::encode_name(path.as_os_str()))
    }

    /// Zero-copy view of the names of nodes `a` up to `b` (or fewer: one heap chunk). Safe while
    /// a scan appends. Use [`NamesRun::seps`] for each name's NUL position.
    pub fn names_run(&self, a: NodeId, b: NodeId) -> NamesRun<'_> {
        assert!(a < b && b as usize <= self.n);
        let first = self.name_off(a);
        let k = chunk_of(&self.chunks, first);
        let chunk = &self.chunks[k];
        let end = chunk.end();
        // nodes whose names lie in this chunk: offsets increase with ids
        let (mut lo, mut hi) = (a + 1, b);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.name_off(mid) < end {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        let stop = lo;
        let start = (first - 1 - chunk.base) as usize;
        let last = stop - 1;
        let stop_byte = (self.name_off(last) - chunk.base) as usize + self.name_len(last) as usize;
        // SAFETY: published names inside a chunk that never moves.
        let run =
            unsafe { std::slice::from_raw_parts(chunk.buf.ptr().add(start), stop_byte - start) };
        NamesRun { stop, run }
    }

    /// The whole name image (`\0name` per node in id order) as runs, without joining them.
    pub fn name_runs(&self) -> impl Iterator<Item = &[u8]> + '_ {
        let mut a = 0 as NodeId;
        let n = self.n as NodeId;
        std::iter::from_fn(move || {
            if a >= n {
                return None;
            }
            let r = self.names_run(a, n);
            a = r.stop;
            Some(r.run)
        })
    }

    /// Bytes of names in use (each name plus its NUL).
    pub fn names_size(&self) -> usize {
        self.name_lens().iter().map(|&l| l as usize + 1).sum()
    }

    /// Bytes of the live columns and names.
    pub fn memory_bytes(&self) -> usize {
        let cols: usize = Col::ALL.iter().map(|&c| self.cols.item(c) * self.n).sum();
        cols + self.names_size()
    }

    /// Whether every column of this view lives in a mapped file.
    pub fn columns_mapped(&self) -> bool {
        self.cols.bufs.iter().all(|b| b.is_mapped())
    }

    /// Whether the first name chunk lives in a mapped file.
    pub fn names_mapped(&self) -> bool {
        self.chunks.first().is_some_and(|c| c.buf.is_mapped())
    }

    /// Number of name-heap chunks.
    pub fn name_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Element size of a column in this view (`name_off`: 4 or 8).
    pub fn item_size(&self, c: Col) -> usize {
        self.cols.item(c)
    }

    /// Little-endian bytes of rows `rows` of a column: borrowed for plain columns, copied value
    /// by value (atomically) for the atomic ones, which a running scan may be updating.
    pub fn column_bytes(&self, c: Col, rows: Range<usize>) -> std::borrow::Cow<'_, [u8]> {
        assert!(rows.end <= self.n);
        let item = self.cols.item(c);
        if !c.is_atomic() {
            // SAFETY: rows below `n` of a plain column; see plain_col!.
            let all = unsafe { std::slice::from_raw_parts(self.cols.p::<u8>(c), self.n * item) };
            return std::borrow::Cow::Borrowed(&all[rows.start * item..rows.end * item]);
        }
        let mut out = Vec::with_capacity(rows.len() * item);
        match item {
            8 => {
                let col: &[AtomicI64] =
                    unsafe { std::slice::from_raw_parts(self.cols.p(c), self.n) };
                for v in &col[rows] {
                    out.extend_from_slice(&v.load(Ordering::Relaxed).to_le_bytes());
                }
            }
            _ => {
                let col: &[AtomicU32] =
                    unsafe { std::slice::from_raw_parts(self.cols.p(c), self.n) };
                for v in &col[rows] {
                    out.extend_from_slice(&v.load(Ordering::Relaxed).to_le_bytes());
                }
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// The store this view belongs to.
    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Acquire)
    }

    /// The extension table.
    pub fn ext_table(&self) -> Vec<Arc<str>> {
        self.inner.ext.read().names.clone()
    }
}

impl NamesRun<'_> {
    /// Position of every name's NUL in [`NamesRun::run`].
    pub fn seps(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < self.run.len() {
            out.push(i as u32);
            i += 1;
            while i < self.run.len() && self.run[i] != 0 {
                i += 1;
            }
        }
        out
    }
}

// --------------------------------------------------------------------------- guards

/// Shared access: a [`Nodes`] view plus the structure read lock (see [`Store::read`]).
pub struct Reader<'a> {
    nodes: Nodes,
    _guard: ReadGuard<'a>,
}

impl Deref for Reader<'_> {
    type Target = Nodes;
    fn deref(&self) -> &Nodes {
        &self.nodes
    }
}

/// The single appender: builds the tree while readers run (see [`Store::appender`]). Derefs to
/// a view of what it has published so far.
pub struct Appender<'a> {
    inner: &'a Arc<Inner>,
    st: MutexGuard<'a, AppendState>,
    view: Nodes,
}

impl Deref for Appender<'_> {
    type Target = Nodes;
    fn deref(&self) -> &Nodes {
        &self.view
    }
}

/// Exclusive access (see [`Store::write`]): everything an [`Appender`] does, plus changes to
/// published rows ([`crate::mutate`]).
pub struct Writer<'a> {
    app: Appender<'a>,
    _guard: WriteGuard<'a>,
}

impl<'a> Deref for Writer<'a> {
    type Target = Appender<'a>;
    fn deref(&self) -> &Appender<'a> {
        &self.app
    }
}

impl<'a> DerefMut for Writer<'a> {
    fn deref_mut(&mut self) -> &mut Appender<'a> {
        &mut self.app
    }
}

// --------------------------------------------------------------------------- appending

impl Appender<'_> {
    fn bump(&self) {
        self.inner.version.fetch_add(1, Ordering::AcqRel);
    }

    fn refresh(&mut self) {
        self.view = Nodes {
            n: self.st.n,
            cols: self.st.cols.clone(),
            chunks: self.st.chunks.clone(),
            inner: self.inner.clone(),
        };
    }

    /// Makes the rows written so far (`st.n`), the current columns and chunks visible to new
    /// readers.
    fn publish(&mut self) {
        {
            let mut p = self.inner.published.lock();
            p.n = self.st.n;
            p.cols = self.st.cols.clone();
            p.chunks = self.st.chunks.clone();
        }
        let ram = self.st.cols.ram() + self.st.heap.reserved_in_ram();
        self.inner.ram.store(ram, Ordering::Relaxed);
        self.refresh();
    }

    #[inline]
    pub(crate) fn a32(&self, c: Col, i: usize) -> &AtomicI32 {
        assert!(i < self.st.cap);
        // SAFETY: in capacity; atomic column (see atomic_col!).
        unsafe { &*self.st.cols.p::<AtomicI32>(c).add(i) }
    }
    #[inline]
    pub(crate) fn au32(&self, c: Col, i: usize) -> &AtomicU32 {
        assert!(i < self.st.cap);
        // SAFETY: as a32.
        unsafe { &*self.st.cols.p::<AtomicU32>(c).add(i) }
    }
    #[inline]
    pub(crate) fn a64(&self, c: Col, i: usize) -> &AtomicI64 {
        assert!(i < self.st.cap);
        // SAFETY: as a32.
        unsafe { &*self.st.cols.p::<AtomicI64>(c).add(i) }
    }

    /// Writes one plain value.
    ///
    /// # Safety
    /// `i` must be an unpublished row, or the caller must hold the [`Writer`].
    #[inline]
    pub(crate) unsafe fn put<T: Copy>(&self, c: Col, i: usize, v: T) {
        assert!(i < self.st.cap && std::mem::size_of::<T>() == self.st.cols.item(c));
        // SAFETY: in capacity, right size; exclusivity per the caller's contract.
        unsafe { self.st.cols.p::<T>(c).add(i).write(v) }
    }

    /// Copies `src` into rows `start..`.
    ///
    /// # Safety
    /// As [`Appender::put`] for every row.
    unsafe fn put_all<T: Copy>(&self, c: Col, start: usize, src: &[T]) {
        assert!(
            start + src.len() <= self.st.cap && std::mem::size_of::<T>() == self.st.cols.item(c)
        );
        // SAFETY: in capacity; exclusivity per the caller's contract.
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr(),
                self.st.cols.p::<T>(c).add(start),
                src.len(),
            )
        }
    }

    /// Fills rows `start..start + len` with zero bytes.
    ///
    /// # Safety
    /// As [`Appender::put`] for every row.
    unsafe fn zero(&self, c: Col, start: usize, len: usize) {
        assert!(start + len <= self.st.cap);
        let item = self.st.cols.item(c);
        // SAFETY: in capacity; exclusivity per the caller's contract.
        unsafe { std::ptr::write_bytes(self.st.cols.p::<u8>(c).add(start * item), 0, len * item) }
    }

    /// # Safety
    /// As [`Appender::put`].
    unsafe fn put_off(&self, i: usize, off: u64) {
        // SAFETY: per the caller's contract; widened before an offset past the limit is stored.
        unsafe {
            if self.st.cols.wide {
                self.put::<u64>(Col::NameOff, i, off);
            } else {
                self.put::<u32>(Col::NameOff, i, off as u32);
            }
        }
    }

    fn alloc_buf(&mut self, bytes: usize, growable: bool) -> Buf {
        if self.st.mapped {
            match Buf::mapped(bytes, &self.inner.policy.columns_dir(), growable) {
                Ok(b) => return b,
                Err(_) => self.st.mapped = false, // no room for the file: plain memory still works
            }
        }
        Buf::heap(bytes)
    }

    /// Room for `need` more rows.
    fn grow(&mut self, need: usize) -> Result<(), StoreError> {
        let want = self.st.n + need;
        if want > MAX_NODES {
            return Err(StoreError::Full);
        }
        if want <= self.st.cap {
            return Ok(());
        }
        let cap = if self.st.settled {
            // a file change of a complete tree: never double a whole volume's columns
            want + self.st.opts.settled_min_step.max(want / SETTLED_SHARE)
        } else {
            let mut cap = self.st.cap;
            while want > cap {
                cap = (cap as f64 * GROWTH) as usize + 1;
            }
            cap
        };
        self.resize(cap.min(MAX_NODES));
        Ok(())
    }

    /// Room for `rows` nodes in one go: a scan that expects about as many as last time never
    /// copies its columns to grow. Never shrinks.
    pub fn reserve(&mut self, rows: usize) {
        if rows > self.st.cap {
            self.resize(rows.min(MAX_NODES));
        }
    }

    /// Current capacity in rows.
    pub fn capacity(&self) -> usize {
        self.st.cap
    }

    pub fn is_mapped(&self) -> bool {
        self.st.mapped
    }

    /// Complete: compacted or loaded (grows a little at a time).
    pub fn is_settled(&self) -> bool {
        self.st.settled
    }

    fn resize(&mut self, cap: usize) {
        let own = Arc::as_ptr(self.inner) as *const ();
        let need = (cap * ROW_BYTES + self.st.heap.reserved()) as u64;
        if !self.st.mapped && self.inner.policy.should_map(own, need) {
            self.st.mapped = true;
        }
        let n = self.st.n;
        for c in Col::ALL {
            let item = self.st.cols.item(c);
            let old = self.st.cols.bufs[c as usize].clone();
            // mapped: the file grows in place, nothing copied or rewritten
            let grown = if self.st.mapped {
                old.grow_mapped(cap * item)
            } else {
                None
            };
            let buf = match grown {
                Some(b) => b,
                None => {
                    let b = self.alloc_buf(cap * item, true);
                    // SAFETY: distinct buffers; rows below n of the old one are not written
                    // concurrently (only the appender writes atomics, and it is here).
                    unsafe { std::ptr::copy_nonoverlapping(old.ptr(), b.ptr(), n * item) };
                    b
                }
            };
            // one column at a time: the old one goes as soon as no reader holds it
            self.st.cols = Arc::new(self.st.cols.with(c, buf, self.st.cols.wide));
            drop(old);
            self.publish();
        }
        self.st.cap = self.st.cols.cap();
    }

    /// Gives back the growth headroom once nothing is added any more (the end of a scan).
    /// Copies one column at a time; readers holding an old column keep a valid, equal copy.
    /// Returns the bytes freed. From now on the columns grow a little at a time.
    pub fn compact(&mut self) -> usize {
        self.st.settled = true;
        let n = self.st.n.max(16);
        if self.st.cap <= n {
            return 0;
        }
        if self.st.cols.bufs.iter().all(|b| b.is_mapped()) {
            return 0; // the unused tail of a mapped file was never touched: no memory, no rewrite
        }
        let mut freed = 0;
        for c in Col::ALL {
            let item = self.st.cols.item(c);
            let old = self.st.cols.bufs[c as usize].clone();
            freed += old.bytes().saturating_sub(n * item);
            let buf = self.alloc_buf(n * item, true);
            // SAFETY: as in resize.
            unsafe { std::ptr::copy_nonoverlapping(old.ptr(), buf.ptr(), self.st.n * item) };
            self.st.cols = Arc::new(self.st.cols.with(c, buf, self.st.cols.wide));
            drop(old);
            self.publish();
        }
        self.st.cap = self.st.cols.cap();
        freed
    }

    /// Widens `name_off` to 64 bits before an offset at or past the limit is stored.
    fn fit_offsets(&mut self, end: u64) {
        if self.st.cols.wide || end < self.st.opts.off_narrow_limit {
            return;
        }
        let cap = self.st.cap;
        let buf = self.alloc_buf(cap * 8, true);
        let old = self.st.cols.p::<u32>(Col::NameOff);
        for i in 0..self.st.n {
            // SAFETY: rows below n of the narrow column; the new buffer is unshared.
            unsafe {
                (buf.ptr() as *mut u64)
                    .add(i)
                    .write(old.add(i).read() as u64)
            };
        }
        self.st.cols = Arc::new(self.st.cols.with(Col::NameOff, buf, true));
        self.publish();
    }

    /// Writes `batch`'s names into the heap and their offsets into rows `start..`.
    ///
    /// # Safety
    /// Rows `start..start + batch.len()` are unpublished.
    unsafe fn write_names(&mut self, start: usize, names: &[u8], lens: &[u16]) {
        if lens.is_empty() {
            return;
        }
        let need = names.len() + lens.len();
        let mapped = self.st.mapped;
        let dir = self.inner.policy.columns_dir();
        let mut fell_back = false;
        let added = self.st.heap.room(need, &mut |size| {
            if mapped && !fell_back {
                match Buf::mapped(size, &dir, false) {
                    Ok(b) => return b,
                    Err(_) => fell_back = true,
                }
            }
            Buf::heap(size)
        });
        if fell_back {
            self.st.mapped = false;
        }
        if added {
            self.st.chunks = Arc::from(self.st.heap.chunks.clone());
        }
        // SAFETY: past every published name.
        let first = unsafe { self.st.heap.write(names, lens) };
        let end = self.st.heap.end();
        self.fit_offsets(end);
        let mut off = first;
        for (k, &l) in lens.iter().enumerate() {
            // SAFETY: unpublished row (caller's contract).
            unsafe { self.put_off(start + k, off) };
            off += l as u64 + 1;
        }
    }

    /// Writes the rows of `batch` at `start..`: entry columns, names, zero totals and ranges.
    /// Each segment `(rows, parent, depth)` gives the parent and depth of the next rows.
    /// Folders get `PENDING` unless they are mount points or `listed(row)` says the same
    /// ingest lists them.
    ///
    /// # Safety
    /// The rows are unpublished and within capacity.
    unsafe fn write_rows(
        &mut self,
        start: usize,
        batch: &EntryBatch,
        segments: &[(usize, i32, u16)],
        listed: &dyn Fn(usize) -> bool,
    ) {
        let count = batch.len();
        // SAFETY: unpublished rows (caller's contract) for every write below.
        unsafe {
            let mut row = start;
            for &(rows, parent, depth) in segments {
                for i in row..row + rows {
                    self.put::<i32>(Col::Parent, i, parent);
                    self.put::<u16>(Col::Depth, i, depth);
                }
                row += rows;
            }
            self.put_all(Col::Kind, start, &batch.kind);
            self.put_all(Col::Size, start, &batch.size);
            self.put_all(Col::Alloc, start, &batch.alloc);
            self.put_all(Col::Mtime, start, &batch.mtime);
            self.put_all(Col::Ctime, start, &batch.ctime);
            self.put_all(Col::Atime, start, &batch.atime);
            self.put_all(Col::Ino, start, &batch.ino);
            self.put_all(Col::Nlink, start, &batch.nlink);
            self.put_all(Col::Ext, start, &batch.ext);
            self.put_all(Col::NameLen, start, &batch.name_lens);
            for k in 0..count {
                let mut f = batch.flags[k];
                if batch.kind[k] == Kind::Dir as u8 && f & Flags::MOUNT.0 == 0 && !listed(k) {
                    f |= Flags::PENDING.0;
                }
                self.put::<u32>(Col::Flags, start + k, f);
            }
            for c in [
                Col::ChildStart,
                Col::ChildCount,
                Col::TotSize,
                Col::TotAlloc,
                Col::TotFiles,
                Col::TotDirs,
            ] {
                self.zero(c, start, count);
            }
            self.write_names(start, &batch.names, &batch.name_lens);
        }
    }

    /// Adds `add` to the four totals of `node` and of every ancestor of it.
    pub(crate) fn propagate(&self, node: i32, add: [i64; 4]) {
        let mut node = node;
        while node >= 0 {
            let i = node as usize;
            self.a64(Col::TotSize, i)
                .fetch_add(add[0], Ordering::Relaxed);
            self.a64(Col::TotAlloc, i)
                .fetch_add(add[1], Ordering::Relaxed);
            self.a32(Col::TotFiles, i)
                .fetch_add(add[2] as i32, Ordering::Relaxed);
            self.a32(Col::TotDirs, i)
                .fetch_add(add[3] as i32, Ordering::Relaxed);
            // SAFETY: a published parent pointer (only the Writer changes them).
            node = unsafe { self.st.cols.p::<i32>(Col::Parent).add(i).read() };
        }
    }

    /// Appends one row (node 0, roots).
    fn append_one(
        &mut self,
        parent: i32,
        depth: u16,
        kind: Kind,
        flags: Flags,
        name: &[u8],
        times: Times,
    ) -> Result<NodeId, StoreError> {
        let len = u16::try_from(name.len()).map_err(|_| StoreError::NameTooLong(name.len()))?;
        self.grow(1)?;
        let i = self.st.n;
        // SAFETY: row i is unpublished.
        unsafe {
            self.put::<i32>(Col::Parent, i, parent);
            self.put::<u16>(Col::Depth, i, depth);
            self.put::<u8>(Col::Kind, i, kind as u8);
            self.put::<u32>(Col::Flags, i, flags.0);
            self.put::<u16>(Col::NameLen, i, len);
            self.put::<u32>(Col::Mtime, i, times.mtime);
            self.put::<u32>(Col::Ctime, i, times.ctime);
            self.put::<u32>(Col::Atime, i, times.atime);
            self.write_names(i, name, &[len]);
        }
        self.st.n += 1;
        self.publish();
        self.bump();
        Ok(i as NodeId)
    }

    /// Adds a scan root (a child of node 0) with its encoded path as its name. Roots must be
    /// added before any listing is ingested. Folders start `PENDING` (pass `flags`).
    pub fn add_root(
        &mut self,
        path: &[u8],
        kind: Kind,
        flags: Flags,
        times: Times,
    ) -> Result<NodeId, StoreError> {
        let count0 = self.view.child_count(0) as usize;
        if count0 != 0 && self.st.n != 1 + count0 {
            return Err(StoreError::RootsAfterScan);
        }
        let root = self.append_one(0, 1, kind, flags, path, times)?;
        self.a32(Col::ChildStart, 0).store(1, Ordering::Release);
        self.a32(Col::ChildCount, 0)
            .store(root as i32, Ordering::Release);
        if kind == Kind::Dir {
            self.propagate(0, [0, 0, 0, 1]);
        }
        Ok(root)
    }

    /// [`Appender::add_root`] for a folder path, `PENDING`.
    pub fn add_root_dir(
        &mut self,
        path: impl AsRef<std::ffi::OsStr>,
    ) -> Result<NodeId, StoreError> {
        self.add_root(
            names::encode_name(path.as_ref()),
            Kind::Dir,
            Flags::PENDING,
            Times::default(),
        )
    }

    /// Index of an extension in the table, added if new (0 = none, also once 65535 are known).
    pub fn ext_id(&self, ext: &str) -> u16 {
        if let Some(&i) = self.inner.ext.read().index.get(ext) {
            return i;
        }
        let mut t = self.inner.ext.write();
        if let Some(&i) = t.index.get(ext) {
            return i;
        }
        if t.names.len() >= 65535 {
            return 0;
        }
        let i = t.names.len() as u16;
        let e: Arc<str> = Arc::from(ext);
        t.names.push(e.clone());
        t.index.insert(e, i);
        i
    }

    pub fn mark(&mut self, node: NodeId, flags: Flags) {
        assert!((node as usize) < self.st.n);
        self.au32(Col::Flags, node as usize)
            .fetch_or(flags.0, Ordering::Relaxed);
        self.bump();
    }

    pub fn clear(&mut self, node: NodeId, flags: Flags) {
        assert!((node as usize) < self.st.n);
        self.au32(Col::Flags, node as usize)
            .fetch_and(!flags.0, Ordering::Relaxed);
        self.bump();
    }

    fn sums(batch: &EntryBatch, rows: Range<usize>) -> [i64; 4] {
        let mut acc = [0i64; 4];
        for k in rows {
            // a directory's own allocation (POSIX directory blocks) counts toward its parent
            if batch.flags[k] & Flags::HARDLINK_DUP.0 == 0 {
                acc[0] += batch.size[k];
                acc[1] += batch.alloc[k];
            }
            acc[2] += (batch.kind[k] == Kind::File as u8) as i64;
            acc[3] += (batch.kind[k] == Kind::Dir as u8) as i64;
        }
        acc
    }

    /// Appends one directory's entries as its children; returns the first new id.
    ///
    /// Folders among them start `PENDING` (unless `MOUNT`). Publish order for readers in other
    /// threads: the rows, then the store's count, then the range start, the range count last.
    pub fn ingest_listing(
        &mut self,
        dir: NodeId,
        batch: &EntryBatch,
    ) -> Result<NodeId, StoreError> {
        batch.check()?;
        let d = dir as usize;
        if d >= self.st.n {
            return Err(StoreError::NotInStore(dir as i64));
        }
        if !self.view.is_dir(dir) {
            return Err(StoreError::NotADirectory(dir));
        }
        if self.view.child_count(dir) != 0 {
            return Err(StoreError::AlreadyListed(dir));
        }
        let count = batch.len();
        let start = self.st.n;
        self.au32(Col::Flags, d)
            .fetch_and(!Flags::PENDING.0, Ordering::Relaxed);
        self.bump();
        if count == 0 {
            self.a32(Col::ChildStart, d)
                .store(start as i32, Ordering::Release);
            return Ok(start as NodeId);
        }
        self.grow(count)?;
        let depth = self.view.depth(dir) + 1;
        // SAFETY: rows start.. are unpublished and within capacity (grown above).
        unsafe { self.write_rows(start, batch, &[(count, dir as i32, depth)], &|_| false) };
        self.st.n = start + count;
        self.publish();
        self.a32(Col::ChildStart, d)
            .store(start as i32, Ordering::Release);
        self.a32(Col::ChildCount, d)
            .store(count as i32, Ordering::Release);
        self.propagate(dir as i32, Self::sums(batch, 0..count));
        Ok(start as NodeId)
    }

    /// Per listing of a slice: does it list a folder that left the tree (`GONE`), or lie inside
    /// one? (References out of range are left to the other checks.)
    fn gone_listings(&self, refs: &[SliceRef]) -> Vec<bool> {
        let mut gone = vec![false; refs.len()];
        for (k, r) in refs.iter().enumerate() {
            gone[k] = match *r {
                SliceRef::Node(d) => {
                    (d as usize) < self.st.n && self.view.flags(d).intersects(Flags::GONE)
                }
                SliceRef::Entry { listing, .. } => (listing as usize) < k && gone[listing as usize],
            };
        }
        gone
    }

    /// Appends a whole slice of listings at once (a scan worker's result); returns the first new
    /// id of every listing — `None` for a listing skipped because its folder left the tree
    /// meanwhile (a monitor relisted a folder above it and marked it `GONE`), and for every
    /// listing inside it. The result equals [`Appender::ingest_listing`] per listing.
    ///
    /// Listing `k` lists the directory `refs[k]` names, and its entries are rows
    /// `bounds[k]..bounds[k + 1]` of `batch`. Entries flagged `MOUNT` are never `PENDING`. A bad
    /// slice changes nothing.
    pub fn ingest_slice(
        &mut self,
        refs: &[SliceRef],
        bounds: &[u32],
        batch: &EntryBatch,
    ) -> Result<Vec<Option<NodeId>>, StoreError> {
        batch.check()?;
        let listings = refs.len();
        if bounds.len() != listings + 1
            || bounds[0] != 0
            || bounds.windows(2).any(|w| w[0] > w[1])
            || bounds[listings] as usize != batch.len()
        {
            return Err(StoreError::BadSlice("bounds do not match the entries"));
        }
        if listings == 0 {
            return Ok(Vec::new());
        }
        let gone = self.gone_listings(refs);
        if gone.iter().any(|&g| g) {
            let mut renumber = vec![0u32; listings];
            let mut kept_refs = Vec::new();
            let mut ranges = Vec::new();
            let mut kept_bounds = vec![0u32];
            for k in 0..listings {
                if gone[k] {
                    continue;
                }
                renumber[k] = kept_refs.len() as u32;
                kept_refs.push(match refs[k] {
                    SliceRef::Entry { listing, entry } => SliceRef::Entry {
                        listing: renumber[listing as usize],
                        entry,
                    },
                    r => r,
                });
                let (a, b) = (bounds[k] as usize, bounds[k + 1] as usize);
                ranges.push(a..b);
                kept_bounds.push(kept_bounds.last().unwrap() + (b - a) as u32);
            }
            let sub = batch.select(&ranges);
            let got = self.ingest_slice(&kept_refs, &kept_bounds, &sub)?;
            let mut out = vec![None; listings];
            let mut it = got.into_iter();
            for k in 0..listings {
                if !gone[k] {
                    out[k] = it.next().unwrap();
                }
            }
            return Ok(out);
        }

        let n0 = self.st.n;
        let total = batch.len();
        let count = |k: usize| (bounds[k + 1] - bounds[k]) as usize;
        let mut dirs = Vec::with_capacity(listings);
        let mut dir_depth = Vec::with_capacity(listings);
        let mut up = vec![usize::MAX; listings]; // listing whose entry a listing lists
        for (k, r) in refs.iter().enumerate() {
            match *r {
                SliceRef::Node(d) => {
                    if d as usize >= n0 {
                        return Err(StoreError::BadSlice(
                            "a listing names a node that is not in the store",
                        ));
                    }
                    if !self.view.is_dir(d) {
                        return Err(StoreError::NotADirectory(d));
                    }
                    if self.view.child_count(d) != 0 {
                        return Err(StoreError::AlreadyListed(d));
                    }
                    dirs.push(d as usize);
                    dir_depth.push(self.view.depth(d));
                }
                SliceRef::Entry { listing, entry } => {
                    let (j, i) = (listing as usize, entry as usize);
                    if j >= k {
                        return Err(StoreError::BadSlice(
                            "a listing can only name an entry of an earlier listing",
                        ));
                    }
                    if i >= count(j) {
                        return Err(StoreError::BadSlice(
                            "a listing names an entry its listing does not have",
                        ));
                    }
                    let row = bounds[j] as usize + i;
                    let id = n0 + row;
                    if batch.kind[row] != Kind::Dir as u8 {
                        return Err(StoreError::NotADirectory(id as NodeId));
                    }
                    dirs.push(id);
                    dir_depth.push(dir_depth[j] + 1);
                    up[k] = j;
                }
            }
        }
        {
            let mut seen = dirs.clone();
            seen.sort_unstable();
            if let Some(w) = seen.windows(2).find(|w| w[0] == w[1]) {
                return Err(StoreError::AlreadyListed(w[0] as NodeId));
            }
        }
        // totals inside the slice: each listing's own entries, folded deepest first (a listing
        // only names earlier ones, so reverse order is deepest first)
        let mut acc: Vec<[i64; 4]> = (0..listings)
            .map(|k| Self::sums(batch, bounds[k] as usize..bounds[k + 1] as usize))
            .collect();
        for k in (0..listings).rev() {
            if up[k] != usize::MAX {
                let a = acc[k];
                for (t, v) in acc[up[k]].iter_mut().zip(a) {
                    *t += v;
                }
            }
        }

        self.grow(total)?;
        // rows listed by this very slice are not PENDING
        let mut listed_here = vec![false; total];
        for (k, r) in refs.iter().enumerate() {
            if matches!(r, SliceRef::Entry { .. }) {
                listed_here[dirs[k] - n0] = true;
            }
        }
        let segments: Vec<(usize, i32, u16)> = (0..listings)
            .map(|k| (count(k), dirs[k] as i32, dir_depth[k] + 1))
            .collect();
        // SAFETY: rows n0.. are unpublished and within capacity.
        unsafe {
            self.write_rows(n0, batch, &segments, &|row| listed_here[row]);
            for k in 0..listings {
                if up[k] != usize::MAX {
                    let d = dirs[k];
                    self.put::<i64>(Col::TotSize, d, acc[k][0]);
                    self.put::<i64>(Col::TotAlloc, d, acc[k][1]);
                    self.put::<i32>(Col::TotFiles, d, acc[k][2] as i32);
                    self.put::<i32>(Col::TotDirs, d, acc[k][3] as i32);
                    self.put::<i32>(Col::ChildStart, d, (n0 + bounds[k] as usize) as i32);
                    self.put::<i32>(Col::ChildCount, d, count(k) as i32);
                }
            }
        }
        for k in 0..listings {
            if up[k] == usize::MAX {
                self.au32(Col::Flags, dirs[k])
                    .fetch_and(!Flags::PENDING.0, Ordering::Relaxed);
            }
        }
        self.bump();
        // publish for readers in other threads: rows, the store's count, range starts, counts last
        self.st.n = n0 + total;
        self.publish();
        for k in 0..listings {
            if up[k] == usize::MAX {
                self.a32(Col::ChildStart, dirs[k])
                    .store((n0 + bounds[k] as usize) as i32, Ordering::Release);
                self.a32(Col::ChildCount, dirs[k])
                    .store(count(k) as i32, Ordering::Release);
            }
        }
        // listed nodes of the store pass their subtree totals up their chain, themselves included
        for k in 0..listings {
            if up[k] == usize::MAX && acc[k] != [0; 4] {
                self.propagate(dirs[k] as i32, acc[k]);
            }
        }
        Ok((0..listings)
            .map(|k| Some((n0 + bounds[k] as usize) as NodeId))
            .collect())
    }

    /// Breadth-first order of the nodes reachable from node 0 through child ranges.
    pub(crate) fn bfs_order(nodes: &Nodes) -> Vec<NodeId> {
        let mut order = vec![0 as NodeId];
        let mut i = 0;
        while i < order.len() {
            let v = order[i];
            if v == 0 || nodes.is_dir(v) {
                let (a, b) = nodes.child_range(v);
                order.extend(a..b);
            }
            i += 1;
            if order.len() > nodes.len() {
                break; // inconsistent ranges: never run away
            }
        }
        order
    }

    /// Recomputes all subtree totals from scratch (one pass over a breadth-first order).
    pub fn reaggregate(&mut self) {
        let n = self.st.n;
        let order = Self::bfs_order(&self.view);
        let v = &self.view;
        let (kind, flags, parent, size, alloc) = (
            v.kinds_raw(),
            v.flags_col(),
            v.parents(),
            v.sizes(),
            v.allocs(),
        );
        let mut sub_size = vec![0i64; n];
        let mut sub_alloc = vec![0i64; n];
        let mut sub_files = vec![0i32; n];
        let mut sub_dirs = vec![0i32; n];
        for &x in order.iter().rev() {
            let x = x as usize;
            let p = parent[x];
            if x == 0 || p < 0 {
                continue;
            }
            let p = p as usize;
            let counted = flags[x].load(Ordering::Relaxed) & Flags::HARDLINK_DUP.0 == 0;
            sub_size[p] += sub_size[x] + if counted { size[x] } else { 0 };
            sub_alloc[p] += sub_alloc[x] + if counted { alloc[x] } else { 0 };
            sub_files[p] += sub_files[x] + (kind[x] == Kind::File as u8) as i32;
            sub_dirs[p] += sub_dirs[x] + (kind[x] == Kind::Dir as u8) as i32;
        }
        self.bump();
        let v = &self.view;
        for i in 0..n {
            v.tot_sizes()[i].store(sub_size[i], Ordering::Relaxed);
            v.tot_allocs()[i].store(sub_alloc[i], Ordering::Relaxed);
            v.tot_files_col()[i].store(sub_files[i], Ordering::Relaxed);
            v.tot_dirs_col()[i].store(sub_dirs[i], Ordering::Relaxed);
        }
    }

    // ----------------------------------------------------------------- crate-internal access

    /// Grows by `count` rows, writes them (parent `folder`) and returns their first id; the rows
    /// are published but no range points at them yet. For the Writer's relist.
    pub(crate) fn append_rows_unlinked(
        &mut self,
        folder: NodeId,
        batch: &EntryBatch,
    ) -> Result<usize, StoreError> {
        let count = batch.len();
        let n0 = self.st.n;
        self.grow(count)?;
        let depth = self.view.depth(folder) + 1;
        // SAFETY: rows n0.. are unpublished and within capacity.
        unsafe { self.write_rows(n0, batch, &[(count, folder as i32, depth)], &|_| true) };
        self.st.n = n0 + count;
        self.publish();
        Ok(n0)
    }
}

impl Writer<'_> {
    /// Sets the times of a root (or any node).
    pub fn set_times(&mut self, node: NodeId, times: Times) {
        let i = node as usize;
        assert!(i < self.app.st.n);
        // SAFETY: exclusive access.
        unsafe {
            self.app.put::<u32>(Col::Mtime, i, times.mtime);
            self.app.put::<u32>(Col::Ctime, i, times.ctime);
            self.app.put::<u32>(Col::Atime, i, times.atime);
        }
        self.app.bump();
    }

    /// Writes one column value of a published row (tests, mutate). Panics on a type of the
    /// wrong size for the column.
    pub fn set_raw<T: ColValue>(&mut self, c: Col, node: NodeId, v: T) {
        assert!((node as usize) < self.app.st.n);
        // SAFETY: exclusive access: no view of the store exists meanwhile.
        unsafe { self.app.put::<T>(c, node as usize, v) };
    }

    /// Bumps the version (after changes made with [`Writer::set_raw`]).
    pub fn touch(&mut self) {
        self.app.bump();
    }

    /// Takes over the content of a store built aside (a compacted tree, an MFT scan); `other`
    /// is left with this store's old content. Readers of this store see the new content from
    /// their next guard on; the version moves past both stores'.
    ///
    /// Waits for `other`'s readers too (its content becomes this store's, which this writer may
    /// change); do not call it while this thread holds a guard of `other`.
    pub fn adopt(&mut self, other: &Store) {
        assert!(
            !Arc::ptr_eq(self.app.inner, &other.inner),
            "a store cannot adopt itself"
        );
        let _theirs_excluded = other.inner.structure.write();
        {
            let mut theirs = other.inner.append.lock();
            std::mem::swap(&mut *self.app.st, &mut *theirs);
            // keep each side's policy: options other than the policy go with the content
            std::mem::swap(&mut self.app.st.opts.policy, &mut theirs.opts.policy);
            let mut p = other.inner.published.lock();
            p.n = theirs.n;
            p.cols = theirs.cols.clone();
            p.chunks = theirs.chunks.clone();
        }
        {
            let mut mine = self.app.inner.ext.write();
            let mut theirs = other.inner.ext.write();
            std::mem::swap(&mut *mine, &mut *theirs);
        }
        let v = self
            .app
            .inner
            .version
            .load(Ordering::Acquire)
            .max(other.version())
            + 1;
        self.app.inner.version.store(v, Ordering::Release);
        other.inner.version.store(v, Ordering::Release);
        {
            let theirs = other.inner.append.lock();
            other.inner.ram.store(
                theirs.cols.ram() + theirs.heap.reserved_in_ram(),
                Ordering::Relaxed,
            );
        }
        self.app.publish();
    }
}

// --------------------------------------------------------------------------- building whole

/// Every column of a store being built, as plain mutable slices (`name_off` is derived).
pub struct ColumnSlices<'a> {
    pub parent: &'a mut [i32],
    pub depth: &'a mut [u16],
    pub kind: &'a mut [u8],
    pub flags: &'a mut [u32],
    pub name_len: &'a mut [u16],
    pub ext: &'a mut [u16],
    pub size: &'a mut [i64],
    pub alloc: &'a mut [i64],
    pub mtime: &'a mut [u32],
    pub ctime: &'a mut [u32],
    pub atime: &'a mut [u32],
    pub ino: &'a mut [u64],
    pub nlink: &'a mut [u16],
    pub child_start: &'a mut [i32],
    pub child_count: &'a mut [i32],
    pub tot_size: &'a mut [i64],
    pub tot_alloc: &'a mut [i64],
    pub tot_files: &'a mut [i32],
    pub tot_dirs: &'a mut [i32],
}

/// A store from whole columns (snapshots, compaction, the MFT reader): allocate, fill the
/// columns and the name image in place, [`StoreBuilder::finish`]. The image is `\0name` per node
/// in id order (see [`separate_names`] for names given in another order); `name_off` follows
/// from `name_len`. The finished store is complete (grows a little at a time), in mapped files
/// when its policy says so.
pub struct StoreBuilder {
    n: usize,
    cols: ColSet,
    image: Buf,
    mapped: bool,
    ext: Vec<String>,
    opts: StoreOptions,
}

impl StoreBuilder {
    /// Columns for `n` nodes and an image of `image_len` bytes, zero-filled.
    pub fn new(n: usize, image_len: usize, opts: StoreOptions) -> StoreBuilder {
        let mapped = opts
            .policy
            .should_map_new((n.max(16) * ROW_BYTES + image_len) as u64);
        let image = Self::buffer(mapped, &opts, image_len, false);
        Self::with_buf(n, image, mapped, opts)
    }

    /// Columns for `n` nodes over an existing image (used as it is in plain memory).
    pub fn with_image(n: usize, image: Vec<u8>, opts: StoreOptions) -> StoreBuilder {
        let mapped = opts
            .policy
            .should_map_new((n.max(16) * ROW_BYTES + image.len()) as u64);
        let buf = if mapped {
            let b = Self::buffer(true, &opts, image.len(), false);
            // SAFETY: a fresh buffer of the image's length.
            unsafe { std::ptr::copy_nonoverlapping(image.as_ptr(), b.ptr(), image.len()) };
            b
        } else {
            Buf::from_vec(image)
        };
        Self::with_buf(n, buf, mapped, opts)
    }

    fn buffer(mapped: bool, opts: &StoreOptions, bytes: usize, growable: bool) -> Buf {
        if mapped && let Ok(b) = Buf::mapped(bytes, &opts.policy.columns_dir(), growable) {
            return b;
        }
        Buf::heap(bytes)
    }

    fn with_buf(n: usize, image: Buf, mapped: bool, opts: StoreOptions) -> StoreBuilder {
        let cap = n.max(16);
        let wide = image.bytes() as u64 >= opts.off_narrow_limit;
        let cols = ColSet::alloc(cap, wide, &mut |b| Self::buffer(mapped, &opts, b, true));
        StoreBuilder {
            n,
            cols,
            image,
            mapped,
            ext: Vec::new(),
            opts,
        }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn set_ext_table(&mut self, ext: Vec<String>) {
        self.ext = ext;
    }

    /// The columns to fill.
    pub fn columns(&mut self) -> ColumnSlices<'_> {
        let n = self.n;
        let c = &self.cols;
        // SAFETY: distinct, unshared buffers of at least n rows each.
        unsafe {
            ColumnSlices {
                parent: std::slice::from_raw_parts_mut(c.p(Col::Parent), n),
                depth: std::slice::from_raw_parts_mut(c.p(Col::Depth), n),
                kind: std::slice::from_raw_parts_mut(c.p(Col::Kind), n),
                flags: std::slice::from_raw_parts_mut(c.p(Col::Flags), n),
                name_len: std::slice::from_raw_parts_mut(c.p(Col::NameLen), n),
                ext: std::slice::from_raw_parts_mut(c.p(Col::Ext), n),
                size: std::slice::from_raw_parts_mut(c.p(Col::Size), n),
                alloc: std::slice::from_raw_parts_mut(c.p(Col::Alloc), n),
                mtime: std::slice::from_raw_parts_mut(c.p(Col::Mtime), n),
                ctime: std::slice::from_raw_parts_mut(c.p(Col::Ctime), n),
                atime: std::slice::from_raw_parts_mut(c.p(Col::Atime), n),
                ino: std::slice::from_raw_parts_mut(c.p(Col::Ino), n),
                nlink: std::slice::from_raw_parts_mut(c.p(Col::Nlink), n),
                child_start: std::slice::from_raw_parts_mut(c.p(Col::ChildStart), n),
                child_count: std::slice::from_raw_parts_mut(c.p(Col::ChildCount), n),
                tot_size: std::slice::from_raw_parts_mut(c.p(Col::TotSize), n),
                tot_alloc: std::slice::from_raw_parts_mut(c.p(Col::TotAlloc), n),
                tot_files: std::slice::from_raw_parts_mut(c.p(Col::TotFiles), n),
                tot_dirs: std::slice::from_raw_parts_mut(c.p(Col::TotDirs), n),
            }
        }
    }

    /// Raw bytes of every column except `name_off` (in [`Col::ALL`] order), and the image, all
    /// at once (snapshot loading fills them in parallel).
    pub(crate) fn raw_parts(&mut self) -> (Vec<(Col, &mut [u8])>, &mut [u8]) {
        let n = self.n;
        let c = &self.cols;
        // SAFETY: distinct, unshared buffers.
        unsafe {
            let cols = Col::ALL
                .iter()
                .filter(|&&k| k != Col::NameOff)
                .map(|&k| {
                    (
                        k,
                        std::slice::from_raw_parts_mut(c.p::<u8>(k), n * c.item(k)),
                    )
                })
                .collect();
            let image = std::slice::from_raw_parts_mut(self.image.ptr(), self.image.bytes());
            (cols, image)
        }
    }

    /// The name image to fill.
    pub fn image(&mut self) -> &mut [u8] {
        // SAFETY: the unshared image buffer.
        unsafe { std::slice::from_raw_parts_mut(self.image.ptr(), self.image.bytes()) }
    }

    /// Derives `name_off`, checks the structure (parents, ranges, names fit the image) and
    /// makes the store.
    pub fn finish(self) -> Result<Store, StoreError> {
        let n = self.n;
        if n == 0 {
            return Err(StoreError::Inconsistent("no node 0"));
        }
        let c = &self.cols;
        // SAFETY: unshared buffers, n rows each.
        let (parent, len, start, count, image) = unsafe {
            (
                std::slice::from_raw_parts(c.p::<i32>(Col::Parent), n),
                std::slice::from_raw_parts(c.p::<u16>(Col::NameLen), n),
                std::slice::from_raw_parts(c.p::<i32>(Col::ChildStart), n),
                std::slice::from_raw_parts(c.p::<i32>(Col::ChildCount), n),
                std::slice::from_raw_parts(self.image.ptr(), self.image.bytes()),
            )
        };
        let total: usize = len.iter().map(|&l| l as usize + 1).sum();
        if total != image.len() {
            return Err(StoreError::Inconsistent("the names do not fill the image"));
        }
        let mut off = 1u64;
        for i in 0..n {
            if parent[i] < -1 || parent[i] as i64 >= n as i64 {
                return Err(StoreError::Inconsistent("a parent outside the store"));
            }
            if count[i] < 0
                || (count[i] > 0 && (start[i] < 1 || start[i] as i64 + count[i] as i64 > n as i64))
            {
                return Err(StoreError::Inconsistent("a child range outside the store"));
            }
            if image[off as usize - 1] != 0 {
                return Err(StoreError::Inconsistent("a name without its separator"));
            }
            // SAFETY: unshared buffer of the column's width (u64 when wide).
            unsafe {
                if c.wide {
                    c.p::<u64>(Col::NameOff).add(i).write(off);
                } else {
                    c.p::<u32>(Col::NameOff).add(i).write(off as u32);
                }
            }
            off += len[i] as u64 + 1;
        }
        let cap = c.cap();
        let StoreBuilder {
            n,
            cols,
            image,
            mapped,
            ext,
            opts,
        } = self;
        let heap = HeapWriter::frozen(image, opts.first_chunk, opts.max_chunk);
        let chunks: Arc<[Chunk]> = Arc::from(heap.chunks.clone());
        let st = AppendState {
            n,
            cap,
            cols: Arc::new(cols),
            heap,
            chunks,
            mapped,
            settled: true,
            opts,
        };
        Ok(Store::from_state(st, ExtTable::new(ext)))
    }
}

/// A `\0name` image in id order from names concatenated in any order (`off`, `len` index
/// `names`), built in blocks so the temporaries stay small.
pub fn separate_names(names: &[u8], off: &[u64], len: &[u16]) -> Vec<u8> {
    let total: usize = len.iter().map(|&l| l as usize + 1).sum();
    let mut image = Vec::with_capacity(total);
    for (&o, &l) in off.iter().zip(len) {
        image.push(0);
        image.extend_from_slice(&names[o as usize..o as usize + l as usize]);
    }
    image
}
