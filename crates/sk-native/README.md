# sk-native

Pure-Rust scanning core of SpaceKnife (formerly the PyO3 extension `packages/native`):

- `sk_native::hardscan` — parallel directory walker ("hard scan"), Windows (NT API, file ids,
  case-only siblings, relative opens, access times kept) and POSIX (`getdents64` /
  `getattrlistbulk` + `fstatat`, `O_NOFOLLOW`, paths past `PATH_MAX`).
- `sk_native::mft` — NTFS `$MFT` reader and parallel parser (Windows volumes; image files
  everywhere).

Features: `hardscan` and `mft`, both on by default. Nothing here touches the node store: results are
plain column vectors meant to be appended to it.

## `hardscan`

```rust
use sk_native::hardscan::{scan_slice, Ref, RootOptions, SliceOptions, Task};

let opts = SliceOptions {
    roots: vec![RootOptions { dev, cluster_size: 4096, ..Default::default() }],
    budget: Duration::from_millis(50),
    max_entries: 100_000,
    threads: 0,                 // 0 = the shared carriers (adaptive width); N = own pool
    gate: Some(disk_gate.clone()), // ListingGate: the disk-load limit, shared by calls
    cancel: Some(cancel_flag),  // Arc<AtomicBool>: ends the call early
    ..Default::default()
};
let res = scan_slice(vec![Task { target: Ref::Node(root_id), path, root: 0 }], &opts)?;
```

### Inputs

| type | fields |
|---|---|
| `Task` | `target: Ref`, `path: PathBuf` (absolute), `root: u32` (index into `roots`) |
| `Ref` | `Node(u64)` — a node the caller knows; `Entry { listing, entry }` — entry `j` of an earlier listing of the *same* slice (`Ref::shifted(n)` when slices are concatenated) |
| `RootOptions` | `dev` (POSIX `st_dev`: other devices become `MOUNT`), `cluster_size` (4096), `resident_max` (0; NTFS resident files allocate 0), `cross_mounts`, `skip` / `protect` (POSIX: folders not entered, flagged `MOUNT` / `ACCESS_DENIED`), `path: Option<PathBuf>` + `verify_names` (Windows/WinBtrfs: tasks checked by stored names below the root), `listed_alloc: ListedAlloc` (`Off`; Windows, see below) |
| `SliceOptions` | `roots`, `budget: Duration` (default 30 ms; `ZERO` lists the first folder only), `max_entries` (100,000), `threads` (0 = the process's shared carriers with the adaptive width; N = a pool of the call's own; 1 = a pool owned by the calling thread), `gate: Option<ListingGate>` (a limit on listings in flight and a pause shared by the calls that name it), `keep_access_time` (true), `relative_open` (true), `skip_final` (false), `dot_check` (false), `keep_where_updated` (true), `cancel: Option<Arc<AtomicBool>>`; `SliceOptions::unbounded(roots)` = no budget. `seams: TestSeams` is a hidden test hook (`fold_strict`, `extd_self_ids`, `pinned_cap`, `verify_final`) |

Windows-only options (docs/native.md 2.2; ignored on POSIX):

- `listed_alloc` (per root; `ListedAlloc::{Off, Ntfs, Refs, Btrfs}`, `ListedAlloc::from_fs_name`):
  a file's size on disk is its listing entry's AllocationSize, as that file system counts it;
  a value that is not whole clusters is data kept in the file record or directory (NTFS
  resident, ReFS inline, small WinBtrfs sparse files) and counts 0. The file itself is asked
  (`NtQueryInformationByName(FileStatInformation)` relative to the listed folder's handle, else a
  relative open + `FileStandardInformation`) only where the entry lags or hides the count:
  sparse / compressed files written or changed in the last minute (NTFS, ReFS) and NTFS files
  listing 0 clusters though not empty, sparse or compressed (WOF). Never on Btrfs or in folders
  opened by file id. `Off`: estimated from the size (`cluster_size`, `resident_max`,
  `GetCompressedFileSizeW` by path for sparse and compressed files). The scanner
  (`sk_core::scan::winopts::listed_alloc`) turns it on for local NTFS, ReFS and Btrfs volumes;
  `SPACEKNIFE_WIN_LISTED_ALLOC=0` goes back to the estimate.
- `skip_final`: on local NTFS, ReFS and Btrfs, a listing's last `NtQueryDirectoryFile` (which
  would only return `STATUS_NO_MORE_FILES`) is left out when the previous buffer still had room
  for an entry with a 255-unit name. Not for listings of reparse points; per call and root the
  first 64 skippable listings, then one in 64, send it anyway, and one that returns entries turns
  skipping off for the root. `TestSeams::verify_final` sends every one and counts it.
- `dot_check`: a relative open is checked by the file id of its listing's "." entry instead of
  querying the handle (mismatch: the handle is queried, and a wrong folder's listing is dropped
  and redone as without the option).
- `keep_where_updated`: `keep_access_time` is skipped on volumes whose file system does not
  update access times by the system's setting (`NtfsDisableLastAccessUpdate` 1 / 3 with or
  without the high bit, `RefsDisableLastAccessUpdate` = 1; read once per process).

The scanner sets the last three from `SPACEKNIFE_WIN_SKIP_FINAL=1`, `SPACEKNIFE_WIN_DOT_CHECK=1`,
`SPACEKNIFE_WIN_KEEP_WHERE_UPDATED=0` (`sk_core::scan::winopts::apply_env`; `0`, `false`, `no`,
`off` = off).

### Output: `SliceResult`

```text
refs:      Vec<Ref>        per listing: the directory it lists (preorder; Entry refs point back)
paths:     Vec<PathBuf>    per listing: the directory's path
bounds:    Vec<usize>      listings + 1 offsets into `entries`  (listing(k) -> Range)
entries:   Entries         struct-of-arrays, one row per entry, listings back to back:
           names: Vec<u8>  (concatenated UTF-8; Windows lone surrogates as WTF-8; POSIX raw bytes)
           name_lens: Vec<u16>, ext: Vec<u16> (index into ext_table, 0 = none),
           kind: Vec<u8>, flags: Vec<u32>, size/alloc: Vec<i64>, mtime/ctime/atime: Vec<u32>
           (unix seconds, 0 = unknown), ino: Vec<u64>, nlink: Vec<u16>, dev: Vec<u64>
ext_table: Vec<Vec<u8>>    lowercased extensions (Python str.lower()), id 0 = ""
frontier:  Vec<Task>       folders not listed (budget spent / cancelled), in preorder
errors:    Vec<ListingError { target, path, message }>   folders that could not be listed
elapsed:   Duration        time spent listing
cancelled: bool
```

The columns are exactly the node store's `ENTRY_COLUMNS` (`kind flags size alloc mtime ctime atime
ino nlink`) plus `dev` (for hard links), names and extension ids: the store can `extend_from_slice`
each column. `Entries::names()` iterates names, `name_offsets()` gives the offsets.
`SliceResult::append(other)` concatenates slices (worker.merge_slices: refs shifted, one extension
table).

Every task ends up in exactly one place: a listing, an error, or the frontier. A task that is
not a directory (or cannot be opened) is an `errors` entry; `scan_slice` itself fails only for
a task whose root index is out of range (`Error::RootOutOfRange`), a thread pool that cannot
start, or more than 65,536 extensions in one slice.

Behaviour (as `spaceknife.scan.worker.scan_slice` / docs/native.md 2.2): directories are listed
in parallel, one rayon job per directory, depth-first; budgets are checked before each
directory by every thread (a slice can overshoot `max_entries` by one directory per thread). An
untruncated slice equals the worker's DFS output. On Windows, folders whose name another entry
shares in another case are opened by file id and never go to the frontier; past the budget they
are listed up to a cap, past that they are errors with `messages::CAP`. The other texts in
`messages` (`CASE`, `DUP`, `BADNAME`, `INCOMPLETE`) are listing errors of such folders.

### Constants and helpers

- `kind::{FILE, DIR, LINK, OTHER}`, `flag::{ACCESS_DENIED, MOUNT, HARDLINK_DUP, COMPRESSED, SPARSE,
  HIDDEN, SYSTEM, READONLY, REPARSE, CLOUD, ERROR, ARCHIVE, ENCRYPTED, TEMPORARY, NOT_INDEXED,
  OFFLINE, ADS}` — the store's `Kind` / `Flag` values.
- `LinkSet` — `(dev, ino)` of hard-linked files already counted, across slices:
  `mark(&dev, &ino, &nlink, &kind, &mut flags) -> usize` or `mark_entries(&mut Entries)` sets
  `HARDLINK_DUP` on every repeat (panics if the columns differ in length); `len`, `contains`.
- `lower_ext(&[u8]) -> Vec<u8>` — Python's `str.lower()` on extension bytes.
- `open_stats(reset) -> Vec<(&'static str, u64)>` — process-wide open counters (Windows:
  `relative`, `by_path`, `strict_by_id`, `pinned`, `pinned_cap_hit`, `final_skipped`,
  `final_verified`, `final_wrong`, `dot_matched`, `dot_unmatched`, `keep_off`, `alloc_asked`,
  `alloc_ask_opened`, `alloc_ask_failed`, …; empty on POSIX).
- `bulk_listing() -> String` — macOS `getattrlistbulk` state (`"on"`, `"off: why"`, `"untested"`),
  `"unsupported"` elsewhere.
- `default_threads()` — the carriers' start width (as many calling threads as a scanner should use).
- `listing_threads(reset) -> ListingThreads` — `cpus`, `start`, `least`, `cold`, `max`, `width` now, `threads` (the carriers' pool), `mode` (`adaptive` / `fixed` / `start`), `low` / `high` since the last reset.
- `ListingGate::new(limit)`, `set(limit, paused)`, `limit()`, `paused()`, `inflight()`, `window() -> (busy_s, peak, refused)`.
- `sk_native::cpus::usable()` — the CPUs this process may use (Windows affinity across processor groups, Linux affinity and cgroup v2 `cpu.max`).

Listing threads (port of main's `pace.rs` / `slots.rs`, docs/native.md 2.2 「列目录线程」): calls with
`threads: 0` list on one process-wide rayon pool (the *carriers*). The *width* is the most folders
listed at once (not the thread count): a folder is listed only while it holds a slot of the width and
of its call's gate; others wait on the call's parked stack, and a carrier that finished a folder goes
on with the next one of its call (depth-first, no job spawned). A call takes at most its share of the
width among the calls on the carriers. The width adapts (`pace.rs` `decide`): start = usable CPUs
2–16 (macOS 2×, 4–16); disk reads per folder ≥ 0.2 with busy slots → the cold width max(2×start, 32)
(max 64); warm and contended → probes half the width and keeps it when ≥ 15 % faster. Windows counts
reads with the volume's `IOCTL_DISK_PERFORMANCE`, POSIX with `getrusage` `ru_inblock` (Linux in 512 B
units → 4 KiB reads; macOS reads as they are). Environment (measurements): `SPACEKNIFE_SCAN_THREADS=N`
fixes the width, `SPACEKNIFE_WALK_ADAPT=0` keeps the start width, `SPACEKNIFE_WALK_MAX`,
`SPACEKNIFE_WALK_TRACE=<file>`. A paused gate starts no folder that can wait (it goes back in the
frontier). Private pools (`threads` ≥ 1) are cached by size (four kept); `threads: 1` uses a pool owned
by the calling thread. Linux: a found subfolder is opened with `O_NOATIME` in the `open` itself when the
parent's `fstatat` says its owner allows it (else without trying).

## `mft`

```rust
use sk_native::mft::{scan_mft, ScanOptions, ScanStats};

let mut progress = |done: u64, total: u64| { /* bytes of $MFT */ };
let mut stats = ScanStats::default();
let (table, info) = scan_mft("C:", ScanOptions {
    duty: 0.5,                     // disk-load limit: the disk busy with the scan half the time
    progress: Some(&mut progress), // on the calling thread, once per window (and once at 0)
    cancel: Some(flag),            // Arc<AtomicBool> -> io::ErrorKind::Interrupted
    stats: Some(&mut stats),       // optional: timings and counts (benchmarks)
    ..Default::default()
})?;
```

| function | |
|---|---|
| `parse_mft(&[u8], record_size, cluster_size) -> io::Result<MftTable>` | parallel parse of a raw `$MFT`, rows in record order |
| `parse_mft_stream(&[u8], rs, cs, window_records)` | the streaming parser over memory (records with an `$ATTRIBUTE_LIST` come last) |
| `parse_mft_stream_concurrent(&[u8], rs, cs, window_records, seed)` | test hook (`#[doc(hidden)]`): windows parsed in parallel in a shuffled order, pushed back in record order — the same table as `parse_mft_stream`, byte for byte |
| `volume_info(volume) -> io::Result<VolumeInfo>` | boot sector + `$MFT` layout of `"C:"` (admin, Windows) |
| `read_mft(volume) -> io::Result<Vec<u8>>` | the whole `$MFT` (admin, Windows) |
| `scan_mft(volume, ScanOptions) -> io::Result<(MftTable, VolumeInfo)>` | read + parse, streaming (below) |
| `image_info` / `read_mft_image` / `scan_mft_image` | the same on an image file holding a volume (on Windows through the overlapped engine too) |
| `device_path("c")` -> `\\.\C:`; `decode_runs(&[u8])` | helpers |

`scan_mft` (docs/native.md 2.1): the `$MFT` is cut into 8 MiB windows (`DEFAULT_WINDOW`), several
read at once — on Windows one overlapped, unbuffered volume handle with an I/O completion port,
reading straight into sector-aligned window buffers (reader threads on images elsewhere). How
many are in flight is found while the scan runs: from 2, doubling while that raises the
throughput by more than 10 %, at most 128 MiB (a SATA SSD stays at 2, NVMe goes further). Each
window is parsed on a rayon pool as soon as it is in, and the parts are joined in record order,
so the rows and their order are those of parsing one window after the other, whatever the window
size, depth or completion order. The output columns are reserved by the density of the records
seen so far. With `duty` < 1 reads go in bursts that keep the disk busy at least 50 ms, then
pause `busy × (1 − duty) / duty`. Memory: the table plus the windows in flight.

`ScanOptions`: `duty` (0, 1], `window` (bytes, 0 = default), `depth` (windows in flight, fixed;
0 = adaptive), `progress`, `cancel`, `stats: Option<&mut ScanStats>`. `ScanStats`: `tuning`
(`ScanTuning`: window, depth_min/max, spare, piece), `windows`, `reads`, `bounced`, `buffers`,
`max_in_flight`, `max_parsing`, `depth` (where it settled), `depth_trace` ((depth, bytes/s) per
epoch), seconds `read_s`, `busy_s`, `paused_s`, `starved_s`, `parse_cpu_s`, `drain_s`, `push_s`,
`finish_s`, `join_s`, `layout_s`, `total_s`, and counts `deferred`, `extensions`, `fetched`,
`redone`. Benchmarks only: `SPACEKNIFE_MFT_WINDOW` / `_DEPTH` / `_SPARE` / `_PIECE` apply when
the options leave window and depth at 0.

Thread pools: every parse (`parse_mft*`, `scan_mft*`) runs on a rayon pool of its own, as wide as
`cpus::usable()`, that goes with its threads when the call returns (rayon's global pool is not
used).

`MftTable` columns (one row per file name; a DOS alias beside a long name in the same folder is
dropped): `record, parent: Vec<u64>`, `name_off: Vec<u32>`, `name_len: Vec<u16>`, `kind: Vec<u8>`
(0 file, 1 dir, 2 link), `flags: Vec<u32>`, `size, alloc, ads_size, ads_alloc: Vec<i64>`,
`mtime, ctime, atime: Vec<i64>` (unix seconds), `nlink: Vec<u16>`, `names: Vec<u8>` (UTF-8 /
WTF-8), `bad_records: u64`; `rows()`, `name(i)`. The table → node store build is sk-core's
(`sk_core::scan::mft::table_to_store`).

`VolumeInfo`: `bytes_per_sector, cluster_size, record_size, mft_lcn, mft_size, mft_valid_size,
mft_fragments, volume_size`.

Errors are `io::Error`: `InvalidInput` (bad sizes, duty outside (0, 1], not a volume like `C:`),
`InvalidData` (not NTFS, corrupt layout), `PermissionDenied` (not elevated), `Unsupported`
(volume functions off Windows), `Interrupted` (cancelled); a read that fails mid-scan is its OS
error (every read in flight is waited for first).

## Tests

`cargo test -p sk-native`: unit tests next to the code, `tests/hardscan.rs` (the walker against
a `std::fs` reference walker, slicing, cancellation, Windows attributes / long paths / lone
surrogates / ACLs / trailing dots / relative opens / path limit / case-sensitive folders, POSIX
names / modes / mounts / skip / protect, `LinkSet`), `tests/mft.rs` with `tests/mft_image`
(a port of `backend/tests/mft_image.py`: synthetic records, volumes and fuzzing). Tests that need
case-sensitive folders, symlinks or ACL changes skip themselves (printing why) where the system
does not allow them.
