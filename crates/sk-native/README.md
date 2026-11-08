# sk-native

Pure-Rust scanning core of SpaceKnife (formerly the PyO3 extension `packages/native`):

- `sk_native::hardscan` — parallel directory walker ("hard scan"), Windows (NT API, file ids,
  case-only siblings, relative opens, access times kept) and POSIX (`getdents64` /
  `getattrlistbulk` + `fstatat`, `O_NOFOLLOW`, paths past `PATH_MAX`).
- `sk_native::mft` — NTFS `$MFT` reader and parallel parser (Windows volumes; image files
  everywhere), and the parallel build of a whole volume's node columns from its table
  (`build_tree`).
- `sk_native::fold` — the fold of listings into a node store's new rows: the checks and the
  per-entry work of the store's `ingest_slice`, `ingest_listing` and relist.
- `sk_native::columns` — the node store's columns as plain slices (`NodeColumns`, `NameOffsets`)
  and its `kind` / `flag` values.

Features: `hardscan` and `mft`, both on by default (`fold`, `columns` and `cpus` are always there).
Nothing here owns the node store: scan results are plain column vectors meant to be appended to it,
and the fold and the `$MFT` build write into rows the store lends them (`columns::NodeColumns`);
the store keeps its memory, its locks and what readers see.

Layering: this crate is the performance layer with its own API and tests; the backend crates
(sk-core, sk-store, sk-server) use it only through that API, and it depends on no other crate of the
workspace and on nothing by path (`tests/layering.rs` checks the manifest, in `cargo test`, and that
the backend's scan glue — sk-core's `scan/mft.rs` and `scan/mod.rs`, sk-store's `ingest_slice` —
leaves the parallel and per-entry work to this crate).

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
  OFFLINE, ADS}` — the store's `Kind` / `Flag` values (`sk_native::columns`, which also has the
  bits only the fold, the `$MFT` build and the store set: `PENDING`, `VIRTUAL`, `GONE`; sk-store's
  tests check they agree).
- `LinkSet` — `(dev, ino)` of hard-linked files already counted, across slices:
  `mark(&dev, &ino, &nlink, &kind, &mut flags) -> usize` or `mark_entries(&mut Entries)` sets
  `HARDLINK_DUP` on every repeat (panics if the columns differ in length); `len`, `contains`.
- `lower_ext(&[u8]) -> Vec<u8>` — Python's `str.lower()` on extension bytes.
- `Entries::map_ext(&[u16])` — moves the extension ids onto another table (a store's; ids past
  the map become 0).
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
WTF-8), `bad_records: u64`; `rows()`, `name(i)`.

`VolumeInfo`: `bytes_per_sector, cluster_size, record_size, mft_lcn, mft_size, mft_valid_size,
mft_fragments, volume_size`.

Errors are `io::Error`: `InvalidInput` (bad sizes, duty outside (0, 1], not a volume like `C:`),
`InvalidData` (not NTFS, corrupt layout), `PermissionDenied` (not elevated), `Unsupported`
(volume functions off Windows), `Interrupted` (cancelled); a read that fails mid-scan is its OS
error (every read in flight is waited for first).

### The tree build: `build_tree`

```rust
use sk_native::mft::{build_tree, PseudoNames, TreeOptions, TreeSink};

let names = PseudoNames { root: b"C:\\", meta: meta.as_bytes(), orphans: orphans.as_bytes() };
let report = build_tree(table, &names, &TreeOptions { include_ads, count_links_each }, &mut sink)?;
// report: names (rows), nodes, orphans, cycles (parent cycles broken)
```

The table (consumed: its columns go as soon as the build is done with them) becomes the node
columns of the whole volume: node 0, the root (record 5), the metafile folder and the orphans
folder when they hold something, every other name breadth-first (children in row order, each
folder's children the next ids). The steps (docs/native.md 2.1) run over all rows in parallel on a
rayon pool of the build's own (`cpus::usable()` threads named `sk-mft-build-N`, gone when the
call returns): first rows of records and `HARDLINK_DUP`, parent rows, metafiles and orphans,
breadth-first levels, parent cycles broken (the smallest row of each; every row past
`CYCLE_LIMIT`), the extension table (distinct raw extensions sorted, lower-cased, numbered), the
`\0name` image, the columns, subtree totals level by level.

`TreeSink::columns(nodes, image_len, ext_table)` is asked once, when the shape is known, for
zero-filled columns of `nodes` rows and an image of `image_len` bytes (the store's
`StoreBuilder`: sk-core's `table_to_store` is the sink; tests use vectors). Errors:
`BuildError::Table` (columns of different lengths, name offsets out of range), `TooManyRows`,
`NameTooLong` (a pseudo folder's name), `Sink` (the sink's own error); a sink that gives other
shapes is a panic. Constants: `mft::build::{ROOT_RECORD, BADCLUS_RECORD, FIRST_USER_RECORD,
CYCLE_LIMIT}`.

## `fold`

```rust
use sk_native::fold::{Folder, RowsOut, SliceFold, Tree, write_rows};

// a slice: listing k lists folders[k], its entries are rows bounds[k]..bounds[k + 1]
let mut plan = SliceFold::plan(&store_view, &folders, &bounds, &entries)?; // nothing written yet
// the store grows by plan.rows(), makes room for plan.name_bytes(), lends the new rows
plan.write(RowsOut { cols, offsets, names, names_at }, &entries);
for l in plan.listed_nodes() { /* publish: range l.start / l.count of l.node, l.totals up its chain */ }
let starts = plan.starts(); // first row per listing, None for a skipped one
```

The per-entry work of folding listings into the node store (docs/native.md 2.3); the store
(sk-store's `Appender::ingest_slice`) lends the rows and publishes them in its order, under its
locks. `SliceFold::plan` reads the store through `Tree` (`rows`, `flags`, `is_dir`,
`child_count`, `depth`) and checks the slice as `ingest_slice` does — the bounds, then listing by
listing (a node in the store, a folder, not listed yet; an entry of an earlier listing, in range,
a folder), then folders listed twice — refusing it with `FoldError` (`BadBatch`, `BadSlice(why)`,
`NotADirectory(id)`, `AlreadyListed(id)`) before anything is written. Listings of folders that left
the tree (`GONE`) are skipped with everything inside them. `write` fills the lent rows
(`RowsOut`: `NodeColumns` of `rows()` rows, `NameOffsets`, `name_bytes()` bytes of names at the
store's offset `names_at`): the entry columns, `PENDING` on folders still to list (never on
`MOUNT` ones), zero totals and ranges, `\0name` and `name_off`; it sums each listing's entries
(`Totals`: bytes, allocated, files, folders; further hard links add no bytes) and folds the sums up
the slice deepest first; folders listed inside the slice get their range and subtree totals.
`write_rows(out, entries, parent, depth, pending)` writes one folder's entries the same way
(`ingest_listing`; a relist's new rows with `pending` false). `EntryColumns` borrows the entry
columns (`is_consistent()`); a write into rows of another shape is a panic.

## Tests

`cargo test -p sk-native`: unit tests next to the code, `tests/hardscan.rs` (the walker against
a `std::fs` reference walker, slicing, cancellation, Windows attributes / long paths / lone
surrogates / ACLs / trailing dots / relative opens / path limit / case-sensitive folders, POSIX
names / modes / mounts / skip / protect, `LinkSet`), `tests/mft.rs` with `tests/mft_image`
(a port of `backend/tests/mft_image.py`: synthetic records, volumes and fuzzing),
`tests/mft_build.rs` (the tree build into vectors: the tree's shape, every row once, metafiles,
orphans and cycles, extensions, totals, refused tables and sinks, the build's own pool),
`tests/fold.rs` (random slices against listings folded one at a time, skipped listings, refused
slices, every lent row filled and nothing past them), `tests/layering.rs`. Tests that need
case-sensitive folders, symlinks or ACL changes skip themselves (printing why) where the system
does not allow them. The store's side (lending rows, publishing) is tested in sk-store
(`tests/ingest_slice.rs`), the whole `$MFT` build against a serial reference in sk-core
(`tests/scan_mft_build.rs`).
