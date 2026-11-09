//! Scanning (port of `spaceknife.scan.scanner`, `worker`, `mft`).
//!
//! * [`Scanner`]: the hard scan. Calling threads run [`sk_native::hardscan::scan_slice`] on
//!   batches of folders (`concurrency` calls in flight, listed on the walker's shared carriers
//!   with its adaptive width and the disk's [`sk_native::hardscan::ListingGate`]); the thread
//!   that calls [`Scanner::run`] folds every result into the [`Store`] with
//!   [`sk_store::Appender::ingest_slice`] and hands the frontier back to the queue. Readers (views)
//!   run meanwhile (sk-store threading contract); the appender is taken per fold only, so a
//!   monitor's writer is never starved. Disk load: an optional [`IoGate`] (the io governor) paces
//!   the batches; pause stops dispatching, cancel ends slices in flight within milliseconds.
//! * [`mft`]: an `$MFT` table (sk-native's parser, or the elevated helper) → a whole [`Store`].
//! * [`fold_slice`]: one slice into a store (the monitor scans new folders with it too).
//!
//! The Python worker (`worker.py`) is gone: `sk_native::hardscan` is the only lister.

pub mod gate;
pub mod mft;
mod scanner;
pub mod winopts;

use std::path::{Path, PathBuf};

use sk_native::hardscan::{Entries, LinkSet, Ref, SliceResult, Task};
use sk_store::{Appender, EntryBatch, Flags, NodeId, SliceRef, StoreError};

pub use gate::{IoGate, Permit};
pub use scanner::{
    BYTES_CEILING, ESTIMATE_AFTER_S, MAX_RECORDED_DENIED, RATE_WINDOW_S, RootInfo, ScanError,
    ScanFault, ScanOptions, ScanProfile, ScanProgress, Scanner, panic_message, scan_path,
};
pub use sk_protocol::scan::ScanStateName as ScanState;

use crate::volume::VolumeInfo;

/// Directories per batch handed to one listing thread.
pub const NATIVE_BATCH: usize = 256;
/// How long one slice lists before it returns what it has.
pub const NATIVE_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

/// Whether the scanner lists on the walker's shared carriers (`threads` 0: the walker decides
/// how many folders are listed at once, docs/native.md 2.2 「列目录线程」). `SPACEKNIFE_SCAN_SHARED=0`
/// goes back to one listing thread per call (`threads` 1, [`native_threads`] calls), for
/// measurements.
pub fn shared_listing() -> bool {
    !matches!(
        std::env::var("SPACEKNIFE_SCAN_SHARED")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Ok("0" | "false" | "no" | "off")
    )
}

/// Threads that call the walker, one slice each. On the shared carriers the walker decides how
/// many folders are listed at once (its width: from the start width up to the cold width while the
/// disk is read, narrower where fewer list faster); the scanner calls it from as many threads as
/// its start width: the CPUs this process may use ([`sk_native::cpus::usable`]), 2–16 (macOS twice
/// that, 4–16: APFS listings wait in the kernel; Windows at most 8). `SPACEKNIFE_SCAN_THREADS` (1–64) fixes both,
/// for measurements. Without the carriers ([`shared_listing`] off): one per CPU, 2–16 (macOS
/// 4–16, Windows at most 8: NTFS's directory locks).
pub fn native_threads() -> usize {
    if let Ok(v) = std::env::var("SPACEKNIFE_SCAN_THREADS")
        && let Ok(n) = v.trim().parse::<usize>()
        && n > 0
    {
        return n.min(64); // the walker's width is fixed to it too (read once per process)
    }
    if shared_listing() {
        let start = sk_native::hardscan::default_threads().max(1);
        // Windows: at most 8 callers (the walker still lists up to its width, two folders a call
        // at width 16): C:\Windows warm on 32 CPUs, 1.37–1.43 s with 8 callers, 1.89–2.04 s with
        // 16 (NTFS's directory locks), 1.48–2.05 s with the old one-thread-per-call mode
        return if cfg!(windows) { start.min(8) } else { start };
    }
    let cpus = sk_native::cpus::usable();
    if cfg!(target_os = "macos") {
        (2 * cpus).clamp(4, 16)
    } else if cfg!(windows) {
        cpus.clamp(2, 8)
    } else {
        cpus.clamp(2, 16)
    }
}

/// The most folders the walker lists at once (its widest width): what the disk-load governor's
/// concurrency, counted in listings, may reach (`RegistryOptions::max_workers`,
/// `IoGovernor::set_max_workers`).
pub fn listing_max() -> usize {
    if shared_listing() {
        sk_native::hardscan::listing_threads(false).max.max(1)
    } else {
        native_threads()
    }
}

/// Whether listings of this volume check folders by the names the file system stores (Windows
/// on WinBtrfs, where trees Linux made can hold names that differ only in case).
pub fn verify_names(volume: Option<&VolumeInfo>) -> bool {
    cfg!(windows) && volume.is_some_and(|v| v.fs_type.eq_ignore_ascii_case("btrfs"))
}

/// A root as the scanner keeps it: absolute, `~` expanded, `C:` → `C:\`.
pub fn normalise_root(path: &str) -> PathBuf {
    let mut p = PathBuf::from(path);
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        if let Some(h) = home {
            p = PathBuf::from(h).join(path[1..].trim_start_matches(['/', '\\']));
        }
    }
    let mut abs = std::path::absolute(&p).unwrap_or(p);
    let s = abs.to_string_lossy();
    if cfg!(windows) && s.len() == 2 && s.as_bytes()[1] == b':' {
        abs = PathBuf::from(format!("{s}\\"));
    } else {
        // normpath: no "." / ".." parts, no trailing separator (except on a root)
        let raw = sk_store::names::normpath(abs.as_os_str().as_encoded_bytes());
        abs = sk_store::names::name_to_os(&raw).into();
    }
    abs
}

/// `path` is `root` or below it (normcase on Windows).
pub fn path_inside(path: &Path, root: &Path) -> bool {
    let n = |p: &Path| {
        let raw = sk_store::names::normpath(p.as_os_str().as_encoded_bytes());
        sk_store::names::normcase(&raw).into_owned()
    };
    let (p, r) = (n(path), n(root));
    if p == r {
        return true;
    }
    let sep = |b: u8| b == b'/' || (cfg!(windows) && b == b'\\');
    p.len() > r.len() && p.starts_with(&r) && (r.last().is_some_and(|&b| sep(b)) || sep(p[r.len()]))
}

/// A slice's entry columns as the store's [`EntryBatch`]: the columns are moved, not copied;
/// extension ids are mapped onto the store's table (`dev` is dropped: hard links are marked
/// before).
pub fn entry_batch(app: &Appender<'_>, mut entries: Entries, ext_table: &[Vec<u8>]) -> EntryBatch {
    let remap: Vec<u16> = ext_table
        .iter()
        // an extension that is not valid Unicode escaped, as 0.3.4 showed it (never U+FFFD)
        .map(|e| app.ext_id(&sk_store::names::printable(e)))
        .collect();
    if !remap.iter().enumerate().all(|(i, &v)| v as usize == i) {
        entries.map_ext(&remap);
    }
    EntryBatch {
        names: entries.names,
        name_lens: entries.name_lens,
        ext: entries.ext,
        kind: entries.kind,
        flags: entries.flags,
        size: entries.size,
        alloc: entries.alloc,
        mtime: entries.mtime,
        ctime: entries.ctime,
        atime: entries.atime,
        ino: entries.ino,
        nlink: entries.nlink,
    }
}

/// What [`fold_slice`] did.
#[derive(Debug, Default)]
pub struct Folded {
    /// Entries added to the store.
    pub entries: usize,
    pub listings: usize,
    /// Listings skipped because their folder left the tree while it was read.
    pub skipped: usize,
    /// Folders that could not be listed: node, path, OS message (marked `ACCESS_DENIED`).
    pub denied: Vec<(NodeId, PathBuf, String)>,
    /// Folders still to list, as tasks naming their node.
    pub frontier: Vec<Task>,
    /// Path of the last listing (the status bar's current path).
    pub last_path: Option<PathBuf>,
}

/// Folds one slice (or several merged with [`SliceResult::append`]) into the store: hard links
/// marked through `links` (None: every link counts), extensions mapped onto the store's table,
/// the columns moved (not copied) into one [`EntryBatch`], one `ingest_slice` (sk-native's fold
/// writes the rows, the store publishes them). Folders that could not be listed are marked
/// `ACCESS_DENIED` (no longer `PENDING`); the frontier comes back as tasks naming nodes.
/// References to folders that left the tree meanwhile are dropped.
pub fn fold_slice(
    app: &mut Appender<'_>,
    res: SliceResult,
    links: Option<&mut LinkSet>,
) -> Result<Folded, StoreError> {
    let SliceResult {
        refs,
        paths,
        bounds,
        mut entries,
        ext_table,
        frontier,
        errors,
        ..
    } = res;
    if let Some(links) = links
        && !entries.is_empty()
    {
        links.mark_entries(&mut entries);
    }
    let count = entries.len();
    let batch = entry_batch(app, entries, &ext_table);
    let srefs: Vec<SliceRef> = refs
        .iter()
        .map(|r| match *r {
            Ref::Node(n) => SliceRef::Node(n as NodeId),
            Ref::Entry { listing, entry } => SliceRef::Entry { listing, entry },
        })
        .collect();
    let sbounds: Vec<u32> = if bounds.is_empty() {
        vec![0]
    } else {
        bounds.iter().map(|&b| b as u32).collect()
    };
    let starts = if srefs.is_empty() {
        Vec::new()
    } else {
        app.ingest_slice(&srefs, &sbounds, &batch)?
    };
    let mut out = Folded {
        entries: count,
        listings: srefs.len(),
        skipped: starts.iter().filter(|s| s.is_none()).count(),
        last_path: paths.last().cloned(),
        ..Folded::default()
    };
    let resolve = |app: &Appender<'_>, r: Ref| -> Option<NodeId> {
        match r {
            Ref::Node(n) => {
                let n = n as NodeId;
                ((n as usize) < app.len() && !app.flags(n).intersects(Flags::GONE)).then_some(n)
            }
            Ref::Entry { listing, entry } => starts
                .get(listing as usize)
                .copied()
                .flatten()
                .map(|s| s + entry),
        }
    };
    for e in errors {
        if let Some(node) = resolve(app, e.target) {
            app.mark(node, Flags::ACCESS_DENIED);
            app.clear(node, Flags::PENDING);
            out.denied.push((node, e.path, e.message));
        }
    }
    out.frontier = frontier
        .into_iter()
        .filter_map(|t| {
            resolve(app, t.target).map(|node| Task {
                target: Ref::Node(node as u64),
                path: t.path,
                root: t.root,
                prior: t.prior, // the walker's lean for the folder, for the call that lists it
            })
        })
        .collect();
    Ok(out)
}
