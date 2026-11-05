//! Parallel hard scan (docs/native.md 2.2): [`scan_slice`] lists a *slice* of directories — the
//! given tasks and everything below them — until a time or entry budget is spent, and returns
//! what it saw as columns plus the directories it did not get to (the *frontier*), which the
//! caller hands to later calls. It is the twin of `spaceknife.scan.worker.scan_slice`, with the
//! same layout:
//!
//! * listing `k` lists the directory [`SliceResult::refs`]`[k]` names: [`Ref::Node`] for a node the
//!   caller already knows (a task), [`Ref::Entry`] for entry `j` of an earlier listing `L` of the
//!   same slice (always earlier: listings are in preorder);
//! * its entries are rows `bounds[k]..bounds[k + 1]` of [`SliceResult::entries`].
//!
//! Directories are listed in parallel (one rayon job per directory), so a *truncated* slice is not
//! the worker's DFS prefix but some subtree of listed directories (still in preorder, references
//! valid, frontier in preorder too). Budgets are checked before each directory, by every thread,
//! so a slice can overshoot `max_entries` by up to one directory per thread. Untruncated slices are
//! identical to the worker's.
//!
//! Platform notes:
//! - Windows paths are opened in NT form: an empty FAT root is an empty listing rather than an
//!   error; names Win32 would rewrite (trailing dot or space) are listed as they are.
//! - POSIX: subdirectories found during the call are opened with O_NOFOLLOW.
//! - Windows: a folder whose name another entry of its listing shares in another case, which an
//!   open by name does not reach (WinBtrfs, case-sensitive NTFS folders), is opened by its file id
//!   and listed (case.rs, docs/native.md 2.2). Such folders, and everything below them, never go
//!   to the frontier: past the budget they are listed in the same call up to a cap
//!   ([`TestSeams::pinned_cap`]), past that they are listing errors ([`messages::CAP`]).

mod case;
mod common;
mod links;
#[cfg(unix)]
mod posix;
mod walk;
#[cfg(windows)]
mod win;

#[cfg(unix)]
use posix as sys;
#[cfg(windows)]
use win as sys;

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::{Arena, NativePath, RootOpts, StateOpts};
use walk::{Ctx, Node};

pub use common::{flag, kind};
pub use links::LinkSet;
pub use walk::lower_ext;

/// Listing errors of folders their name does not reach (Windows), as `worker.py` words them.
pub mod messages {
    pub use super::case::msg::{BADNAME, CAP, CASE, DUP, INCOMPLETE};
}

/// What a listing (or a task) stands for in the caller's tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ref {
    /// A node the caller already knows (worker.py's `(0, node, 0)`).
    Node(u64),
    /// Entry `entry` of listing `listing` of the same slice (worker.py's `(1, L, j)`).
    Entry { listing: u32, entry: u32 },
}

impl Ref {
    /// The same reference once `listings` listings are put in front of its slice's.
    #[must_use]
    pub fn shifted(self, listings: u32) -> Ref {
        match self {
            Ref::Node(_) => self,
            Ref::Entry { listing, entry } => Ref::Entry {
                listing: listing + listings,
                entry,
            },
        }
    }
}

/// A directory to list: what it stands for, its absolute path, and the index of its root in
/// [`SliceOptions::roots`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub target: Ref,
    pub path: PathBuf,
    pub root: u32,
}

/// Per-root options (worker.RootOptions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootOptions {
    /// POSIX: `st_dev` of the root; directories on another device are not entered (MOUNT).
    pub dev: u64,
    /// Windows: allocation = size rounded up to clusters (0 is treated as 1).
    pub cluster_size: i64,
    /// NTFS: files this small live inside the MFT record (0 clusters).
    pub resident_max: i64,
    /// POSIX: enter directories on other devices.
    pub cross_mounts: bool,
    /// POSIX: folders not to enter, as mount points (macOS: the other half of the APFS volume
    /// group). Ignored on Windows.
    pub skip: Vec<PathBuf>,
    /// POSIX: folders not to enter, as not accessible (macOS privacy-protected places). Ignored
    /// on Windows.
    pub protect: Vec<PathBuf>,
    /// The scan root as given. Windows: with `verify_names`, tasks are checked by the names the
    /// file system stores below it (the root part itself is never compared).
    pub path: Option<PathBuf>,
    /// Windows: names fold onto case-sensitive storage here (WinBtrfs): every task is checked by
    /// its stored names below `path` (the walker also finds out about Btrfs by itself).
    pub verify_names: bool,
}

impl Default for RootOptions {
    fn default() -> Self {
        Self {
            dev: 0,
            cluster_size: 4096,
            resident_max: 0,
            cross_mounts: false,
            skip: Vec::new(),
            protect: Vec::new(),
            path: None,
            verify_names: false,
        }
    }
}

/// Private knobs that make Windows behave as WinBtrfs does, for tests (win.rs `RootState`).
/// Not part of the stable interface.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestSeams {
    /// A strict subfolder is opened by its upcased name.
    pub fold_strict: bool,
    /// FileIdExtd ids are the listed folder's own.
    pub extd_self_ids: bool,
    /// Entries listed past the budget below folders opened by file id and of folders whose name
    /// another entry shares (default: max(2 × max_entries, 10,000)).
    pub pinned_cap: Option<usize>,
}

/// Options of one [`scan_slice`] call (worker.SliceOptions plus the native knobs).
#[derive(Clone, Debug)]
pub struct SliceOptions {
    pub roots: Vec<RootOptions>,
    /// Time budget: directories not started by then go to the frontier. Zero lists the first
    /// directory only (the live monitor's relist of one folder).
    pub budget: Duration,
    /// Entry budget, checked like the time budget.
    pub max_entries: usize,
    /// Listing threads; 0 = one per logical CPU, at most 16. A one-thread pool belongs to the
    /// calling thread, so several one-thread slices on several threads run side by side.
    pub threads: usize,
    /// Windows and Linux: listing a folder does not update its last access time.
    pub keep_access_time: bool,
    /// Windows: a found subdirectory is opened by name relative to its parent's handle, checked
    /// against the listed file id.
    pub relative_open: bool,
    /// Set it to end calls early (pause, cancel): nothing more is listed and the rest is returned
    /// in the frontier; [`SliceResult::cancelled`] tells.
    pub cancel: Option<Arc<AtomicBool>>,
    #[doc(hidden)]
    pub seams: TestSeams,
}

impl Default for SliceOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            budget: Duration::from_millis(30),
            max_entries: 100_000,
            threads: 0,
            keep_access_time: true,
            relative_open: true,
            cancel: None,
            seams: TestSeams::default(),
        }
    }
}

impl SliceOptions {
    /// No time or entry budget: everything below the tasks is listed.
    pub fn unbounded(roots: Vec<RootOptions>) -> Self {
        Self {
            roots,
            budget: Duration::MAX,
            max_entries: usize::MAX,
            ..Self::default()
        }
    }
}

/// The entry columns of a slice (struct-of-arrays, one row per entry, listings one after
/// another). These are the store's `ENTRY_COLUMNS` plus `dev`, the names and the extension ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entries {
    /// The names concatenated, UTF-8 (Windows: lone surrogates as WTF-8, Python's surrogatepass;
    /// POSIX: the raw bytes).
    pub names: Vec<u8>,
    /// Byte length of each name in `names`.
    pub name_lens: Vec<u16>,
    /// Index into [`SliceResult::ext_table`] (0 = no extension).
    pub ext: Vec<u16>,
    /// [`kind`] values.
    pub kind: Vec<u8>,
    /// [`flag`] bits.
    pub flags: Vec<u32>,
    pub size: Vec<i64>,
    /// Bytes allocated on disk.
    pub alloc: Vec<i64>,
    /// Unix seconds (0 = unknown, saturating at u32::MAX).
    pub mtime: Vec<u32>,
    pub ctime: Vec<u32>,
    pub atime: Vec<u32>,
    /// Inode / file id (0 where unknown).
    pub ino: Vec<u64>,
    pub nlink: Vec<u16>,
    /// POSIX `st_dev` (0 on Windows).
    pub dev: Vec<u64>,
}

impl Entries {
    fn with_capacity(rows: usize, name_bytes: usize) -> Self {
        Self {
            names: Vec::with_capacity(name_bytes),
            name_lens: Vec::with_capacity(rows),
            ext: Vec::with_capacity(rows),
            kind: Vec::with_capacity(rows),
            flags: Vec::with_capacity(rows),
            size: Vec::with_capacity(rows),
            alloc: Vec::with_capacity(rows),
            mtime: Vec::with_capacity(rows),
            ctime: Vec::with_capacity(rows),
            atime: Vec::with_capacity(rows),
            ino: Vec::with_capacity(rows),
            nlink: Vec::with_capacity(rows),
            dev: Vec::with_capacity(rows),
        }
    }

    pub fn len(&self) -> usize {
        self.kind.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }

    /// Each entry's name as bytes, in order.
    pub fn names(&self) -> impl Iterator<Item = &[u8]> + '_ {
        let mut off = 0usize;
        self.name_lens.iter().map(move |&n| {
            let s = &self.names[off..off + n as usize];
            off += n as usize;
            s
        })
    }

    /// Byte offset of each entry's name in `names`, plus the end (`len() + 1` values).
    pub fn name_offsets(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.len() + 1);
        let mut off = 0usize;
        out.push(0);
        for &n in &self.name_lens {
            off += n as usize;
            out.push(off);
        }
        out
    }

    fn append(&mut self, other: &Entries, ext_map: &[u16]) {
        self.names.extend_from_slice(&other.names);
        self.name_lens.extend_from_slice(&other.name_lens);
        self.ext
            .extend(other.ext.iter().map(|&e| ext_map[e as usize]));
        self.kind.extend_from_slice(&other.kind);
        self.flags.extend_from_slice(&other.flags);
        self.size.extend_from_slice(&other.size);
        self.alloc.extend_from_slice(&other.alloc);
        self.mtime.extend_from_slice(&other.mtime);
        self.ctime.extend_from_slice(&other.ctime);
        self.atime.extend_from_slice(&other.atime);
        self.ino.extend_from_slice(&other.ino);
        self.nlink.extend_from_slice(&other.nlink);
        self.dev.extend_from_slice(&other.dev);
    }
}

/// A directory that could not be listed (its path, and the OS message or one of [`messages`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingError {
    pub target: Ref,
    pub path: PathBuf,
    pub message: String,
}

/// What one [`scan_slice`] call saw (worker.SliceResult).
#[derive(Clone, Debug, Default)]
pub struct SliceResult {
    /// Per listing: the directory it lists.
    pub refs: Vec<Ref>,
    /// Per listing: the directory's path.
    pub paths: Vec<PathBuf>,
    /// `listings + 1` offsets into `entries`.
    pub bounds: Vec<usize>,
    pub entries: Entries,
    /// Lowercased extensions (no dot) in first-appearance order; id 0 is "" (no extension).
    /// Bytes in the names' encoding (see [`Entries::names`]).
    pub ext_table: Vec<Vec<u8>>,
    /// Directories not listed (budget spent or cancelled), in preorder: tasks for later calls.
    pub frontier: Vec<Task>,
    pub errors: Vec<ListingError>,
    /// Time spent listing (not converting the result).
    pub elapsed: Duration,
    /// The call was ended by [`SliceOptions::cancel`].
    pub cancelled: bool,
}

impl SliceResult {
    /// Number of listings.
    pub fn listings(&self) -> usize {
        self.refs.len()
    }

    /// The entry rows of listing `k`.
    pub fn listing(&self, k: usize) -> Range<usize> {
        self.bounds[k]..self.bounds[k + 1]
    }

    /// Appends `other`'s listings after this slice's (worker.merge_slices): references inside
    /// `other` move by this slice's listing count, extensions get one table. Folding several
    /// slices into a store at once pays its per-call costs once.
    pub fn append(&mut self, other: SliceResult) {
        if self.bounds.is_empty() {
            self.bounds.push(0);
        }
        if self.ext_table.is_empty() {
            self.ext_table.push(Vec::new());
        }
        let shift = self.refs.len() as u32;
        let base = self.entries.len();
        let mut index: crate::util::FxHashMap<&[u8], u16> = self
            .ext_table
            .iter()
            .enumerate()
            .map(|(i, e)| (e.as_slice(), i as u16))
            .collect();
        let mut added = Vec::new();
        let ext_map: Vec<u16> = other
            .ext_table
            .iter()
            .map(|e| {
                let next = u16::try_from(self.ext_table.len() + added.len())
                    .expect("more than 65536 extensions in merged slices");
                *index.entry(e.as_slice()).or_insert_with(|| {
                    added.push(e.clone());
                    next
                })
            })
            .collect();
        drop(index);
        self.ext_table.extend(added);
        self.refs
            .extend(other.refs.iter().map(|r| r.shifted(shift)));
        self.paths.extend(other.paths);
        self.bounds
            .extend(other.bounds.iter().skip(1).map(|b| b + base));
        self.entries.append(&other.entries, &ext_map);
        self.frontier
            .extend(other.frontier.into_iter().map(|mut t| {
                t.target = t.target.shifted(shift);
                t
            }));
        self.errors.extend(other.errors.into_iter().map(|mut e| {
            e.target = e.target.shifted(shift);
            e
        }));
        self.elapsed += other.elapsed;
        self.cancelled |= other.cancelled;
    }
}

/// Why a [`scan_slice`] call could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A task names a root that is not in [`SliceOptions::roots`].
    RootOutOfRange { task: usize, root: u32 },
    /// The listing threads could not be started.
    ThreadPool(String),
    /// More than 65,536 distinct extensions in one slice (ids are u16).
    TooManyExtensions,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::RootOutOfRange { task, root } => {
                write!(f, "task {task}: root index {root} out of range")
            }
            Error::ThreadPool(e) => write!(f, "cannot start the listing threads: {e}"),
            Error::TooManyExtensions => f.write_str("more than 65536 extensions in one slice"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(windows)]
fn path_to_native(p: &Path) -> NativePath {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().collect()
}

#[cfg(unix)]
fn path_to_native(p: &Path) -> NativePath {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().into()
}

#[cfg(windows)]
fn native_to_path(p: &[common::Unit]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    std::ffi::OsString::from_wide(p).into()
}

#[cfg(unix)]
fn native_to_path(p: &[common::Unit]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(p).into()
}

fn root_opts(r: &RootOptions) -> RootOpts {
    #[cfg(unix)]
    let bytes = |paths: &[PathBuf]| -> Vec<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt;
        paths
            .iter()
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .collect()
    };
    #[cfg(not(unix))]
    let bytes = |_: &[PathBuf]| -> Vec<Vec<u8>> { Vec::new() };
    RootOpts {
        dev: r.dev,
        cluster_size: r.cluster_size,
        resident_max: r.resident_max,
        cross_mounts: r.cross_mounts,
        skip: bytes(&r.skip),
        protect: bytes(&r.protect),
        path: r.path.as_deref().map(path_to_native).unwrap_or_default(),
    }
}

/// Lists `tasks` and what lies below them until the budget is spent (see the module docs).
///
/// Every task ends up in exactly one place: listed (`paths`), failed (`errors`), or handed
/// back (`frontier`). A task that is not a directory, or cannot be opened, is an error, not a
/// failure of the call.
pub fn scan_slice(tasks: Vec<Task>, opts: &SliceOptions) -> Result<SliceResult, Error> {
    let started = Instant::now();
    for (i, t) in tasks.iter().enumerate() {
        if t.root as usize >= opts.roots.len() {
            return Err(Error::RootOutOfRange {
                task: i,
                root: t.root,
            });
        }
    }
    let threads = if opts.threads == 0 {
        walk::default_threads()
    } else {
        opts.threads
    };
    let pool = walk::pool(threads).map_err(Error::ThreadPool)?;
    let budget_positive = !opts.budget.is_zero();
    let deadline = if budget_positive {
        started.checked_add(opts.budget)
    } else {
        Some(started)
    };
    let ctx = Ctx {
        root_state: opts
            .roots
            .iter()
            .map(|r| {
                sys::RootState::new(&StateOpts {
                    keep_access_time: opts.keep_access_time,
                    relative_open: opts.relative_open,
                    verify_names: r.verify_names,
                    fold_strict: opts.seams.fold_strict,
                    extd_self_ids: opts.seams.extd_self_ids,
                })
            })
            .collect(),
        roots: opts.roots.iter().map(root_opts).collect(),
        deadline,
        budget_positive,
        max_entries: opts.max_entries,
        extra_cap: opts
            .seams
            .pinned_cap
            .unwrap_or_else(|| opts.max_entries.saturating_mul(2).max(10_000)),
        extra: AtomicUsize::new(0),
        listed: AtomicUsize::new(0),
        entries: AtomicUsize::new(0),
        arenas: (0..pool.current_num_threads().max(1))
            .map(|_| Mutex::new(Arena::default()))
            .collect(),
        cancel: opts.cancel.clone(),
    };
    let nodes: Vec<Node> = tasks
        .iter()
        .map(|t| Node::new(path_to_native(&t.path), t.root, false))
        .collect();

    walk::walk(&pool, &nodes, &ctx);
    let elapsed = started.elapsed();
    let cancelled = ctx.cancelled();
    let arenas: Vec<Arena> = ctx
        .arenas
        .into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect();
    let res = walk::flatten(&nodes, tasks, &arenas);
    // freeing a big walked tree takes milliseconds: let a pool thread do it after we return
    pool.spawn(move || drop((nodes, arenas)));
    let mut res = res?;
    res.elapsed = elapsed;
    res.cancelled = cancelled;
    Ok(res)
}

/// How POSIX listings get their attributes on macOS: "on" (getattrlistbulk, checked against
/// fstatat; "on (folders too)" with SPACEKNIFE_BULK_DIRS=1), "off: why" (a difference, or
/// SPACEKNIFE_NO_BULK=1), "untested" (nothing listed yet); "unsupported" elsewhere (readdir /
/// getdents64 + fstatat, NtQueryDirectoryFile on Windows).
pub fn bulk_listing() -> String {
    #[cfg(target_os = "macos")]
    {
        posix::bulk_state()
    }
    #[cfg(not(target_os = "macos"))]
    {
        "unsupported".to_owned()
    }
}

/// How directories were opened since the last reset, process-wide (tests and measurements;
/// Windows: relative opens, opens by path, why relative opens fell back, case-only siblings…).
/// Empty on POSIX. `reset` zeroes the counters as they are read.
pub fn open_stats(reset: bool) -> Vec<(&'static str, u64)> {
    sys::open_stats(reset)
}

/// The listing threads [`SliceOptions::threads`] = 0 stands for.
pub fn default_threads() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(walk::default_threads)
}
