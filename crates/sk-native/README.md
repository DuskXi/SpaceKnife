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
    threads: 1,                 // 0 = one per CPU (max 16)
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
| `RootOptions` | `dev` (POSIX `st_dev`: other devices become `MOUNT`), `cluster_size` (4096), `resident_max` (0; NTFS resident files allocate 0), `cross_mounts`, `skip` / `protect` (POSIX: folders not entered, flagged `MOUNT` / `ACCESS_DENIED`), `path: Option<PathBuf>` + `verify_names` (Windows/WinBtrfs: tasks checked by stored names below the root) |
| `SliceOptions` | `roots`, `budget: Duration` (default 30 ms; `ZERO` lists the first folder only), `max_entries` (100,000), `threads` (0 = auto; 1 = a pool owned by the calling thread), `keep_access_time` (true), `relative_open` (true), `cancel: Option<Arc<AtomicBool>>`; `SliceOptions::unbounded(roots)` = no budget. `seams: TestSeams` is a hidden test hook |

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
  `relative`, `by_path`, `strict_by_id`, `pinned`, `pinned_cap_hit`, …; empty on POSIX).
- `bulk_listing() -> String` — macOS `getattrlistbulk` state (`"on"`, `"off: why"`, `"untested"`),
  `"unsupported"` elsewhere.
- `default_threads()` — what `threads: 0` means.

Thread pools are cached by size (four kept). `threads: 1` uses a pool owned by the calling
thread, so a scanner running N one-thread slices on N threads lists them side by side.

## `mft`

```rust
use sk_native::mft::{scan_mft, ScanOptions};

let mut progress = |done: u64, total: u64| { /* bytes of $MFT */ };
let (table, info) = scan_mft("C:", ScanOptions {
    duty: 0.5,                     // disk-load limit: reader busy half the time
    progress: Some(&mut progress), // on the calling thread, once per window (and once at 0)
    cancel: Some(flag),            // Arc<AtomicBool> -> io::ErrorKind::Interrupted
    ..Default::default()
})?;
```

| function | |
|---|---|
| `parse_mft(&[u8], record_size, cluster_size) -> io::Result<MftTable>` | parallel parse of a raw `$MFT`, rows in record order |
| `parse_mft_stream(&[u8], rs, cs, window_records)` | the streaming parser over memory (records with an `$ATTRIBUTE_LIST` come last) |
| `volume_info(volume) -> io::Result<VolumeInfo>` | boot sector + `$MFT` layout of `"C:"` (admin, Windows) |
| `read_mft(volume) -> io::Result<Vec<u8>>` | the whole `$MFT` (admin, Windows) |
| `scan_mft(volume, ScanOptions) -> io::Result<(MftTable, VolumeInfo)>` | read + parse, streaming in 32 MiB windows (two in flight, buffers reused) |
| `image_info` / `read_mft_image` / `scan_mft_image` | the same on an image file holding a volume |
| `device_path("c")` -> `\\.\C:`; `decode_runs(&[u8])` | helpers |

`MftTable` columns (one row per file name; a DOS alias beside a long name in the same folder is
dropped): `record, parent: Vec<u64>`, `name_off: Vec<u32>`, `name_len: Vec<u16>`, `kind: Vec<u8>`
(0 file, 1 dir, 2 link), `flags: Vec<u32>`, `size, alloc, ads_size, ads_alloc: Vec<i64>`,
`mtime, ctime, atime: Vec<i64>` (unix seconds), `nlink: Vec<u16>`, `names: Vec<u8>` (UTF-8 /
WTF-8), `bad_records: u64`; `rows()`, `name(i)`.

`VolumeInfo`: `bytes_per_sector, cluster_size, record_size, mft_lcn, mft_size, mft_valid_size,
mft_fragments, volume_size`.

Errors are `io::Error`: `InvalidInput` (bad sizes, duty outside (0, 1], not a volume like `C:`),
`InvalidData` (not NTFS, corrupt layout), `PermissionDenied` (not elevated), `Unsupported`
(volume functions off Windows), `Interrupted` (cancelled).

## Tests

`cargo test -p sk-native`: unit tests next to the code, `tests/hardscan.rs` (the walker against
a `std::fs` reference walker, slicing, cancellation, Windows attributes / long paths / lone
surrogates / ACLs / trailing dots / relative opens / path limit / case-sensitive folders, POSIX
names / modes / mounts / skip / protect, `LinkSet`), `tests/mft.rs` with `tests/mft_image`
(a port of `backend/tests/mft_image.py`: synthetic records, volumes and fuzzing). Tests that need
case-sensitive folders, symlinks or ACL changes skip themselves (printing why) where the system
does not allow them.
