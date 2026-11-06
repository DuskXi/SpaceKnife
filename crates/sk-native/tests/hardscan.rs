//! `scan_slice` against a reference walker built on `std::fs` (the port of
//! packages/native/tests/test_hardscan.py, where the reference was the Python worker).
//!
//! Untruncated slices (no budget) are compared field by field on varied trees; truncated slices
//! (budget 0, tiny `max_entries`) are checked for valid references and for reassembling into the
//! same tree as one untruncated call.
//!
//! Every test takes [`serial`]: the open statistics are process-wide, and a few tests count them.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[cfg(windows)]
use sk_native::hardscan::messages;
use sk_native::hardscan::{
    self, LinkSet, ListingGate, Ref, RootOptions, SliceOptions, SliceResult, Task, flag, kind,
    lower_ext, scan_slice,
};

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------ helpers

fn dev_of(p: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(p).unwrap().dev()
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        0
    }
}

fn root_opts(folder: &Path) -> RootOptions {
    RootOptions {
        dev: dev_of(folder),
        ..RootOptions::default()
    }
}

fn task(node: u64, path: &Path, root: u32) -> Task {
    Task {
        target: Ref::Node(node),
        path: path.to_path_buf(),
        root,
    }
}

fn scan(tasks: Vec<Task>, opts: &SliceOptions) -> SliceResult {
    scan_slice(tasks, opts).expect("scan_slice")
}

/// The path to open with std (Windows: verbatim, so names with a trailing dot or space and paths
/// past MAX_PATH work as the NT paths of the walker do).
fn os_path(p: &Path) -> PathBuf {
    if cfg!(windows) && !p.as_os_str().to_string_lossy().starts_with(r"\\?\") {
        let mut v = OsString::from(r"\\?\");
        v.push(p.as_os_str());
        return v.into();
    }
    p.to_path_buf()
}

fn name_strings(res: &SliceResult) -> Vec<String> {
    res.entries
        .names()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect()
}

#[cfg(windows)]
fn listed(res: &SliceResult) -> BTreeMap<PathBuf, Vec<String>> {
    let names = name_strings(res);
    (0..res.listings())
        .map(|k| (res.paths[k].clone(), names[res.listing(k)].to_vec()))
        .collect()
}

/// worker._secs: `sec + nsec * 1e-9` as a double, truncated (common.rs `secs`).
fn secs(sec: i64, nsec: u64) -> u32 {
    let t = sec as f64 + nsec as f64 * 1e-9;
    if t.is_nan() || t <= 0.0 {
        0
    } else if t >= u32::MAX as f64 {
        u32::MAX
    } else {
        t as u32
    }
}

#[cfg(windows)]
fn mtime_of(md: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    let ft = md.last_write_time() as i64;
    let sec = ft / 10_000_000 - 11_644_473_600;
    let nsec = ((ft % 10_000_000) * 100) as i32 as u32;
    secs(sec, nsec as u64)
}

#[cfg(unix)]
fn mtime_of(md: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    secs(md.mtime(), md.mtime_nsec() as u64)
}

/// Python code points of a name's bytes (Windows: WTF-8; POSIX: surrogateescape).
fn code_points(b: &[u8]) -> usize {
    if cfg!(windows) {
        b.iter().filter(|&&x| x & 0xC0 != 0x80).count()
    } else {
        b.utf8_chunks()
            .map(|c| c.valid().chars().count() + c.invalid().len())
            .sum()
    }
}

/// worker._Acc.ext_id's extension of a file name, lowercased.
fn ext_of(name: &[u8]) -> Vec<u8> {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
        return Vec::new();
    };
    if dot == 0 {
        return Vec::new();
    }
    let tail = &name[dot + 1..];
    if tail.is_empty() || tail.len() > 64 || code_points(tail) > 16 {
        return Vec::new();
    }
    lower_ext(tail)
}

// ------------------------------------------------------------------------------ reference walker

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefEntry {
    name: Vec<u8>,
    kind: u8,
    size: i64,
    mtime: u32,
    ext: Vec<u8>,
}

#[derive(Debug, Default)]
struct Reference {
    refs: Vec<Ref>,
    paths: Vec<PathBuf>,
    listings: Vec<Vec<RefEntry>>,
    errors: Vec<(Ref, PathBuf)>,
}

/// worker.scan_slice without a budget: depth-first, every directory listed in std's order.
fn reference(tasks: &[Task], roots: &[RootOptions]) -> Reference {
    let mut out = Reference::default();
    for t in tasks {
        walk_ref(&mut out, t.target, &t.path, &roots[t.root as usize]);
    }
    out
}

fn walk_ref(out: &mut Reference, target: Ref, path: &Path, root: &RootOptions) {
    let rd = match fs::read_dir(os_path(path)) {
        Ok(rd) => rd,
        Err(_) => {
            out.errors.push((target, path.to_path_buf()));
            return;
        }
    };
    let k = out.refs.len();
    out.refs.push(target);
    out.paths.push(path.to_path_buf());
    out.listings.push(Vec::new());
    let mut entries = Vec::new();
    let mut descend = Vec::new();
    for e in rd {
        let e = e.unwrap();
        let ft = e.file_type().unwrap();
        let md = e.metadata().unwrap();
        let k_ = if ft.is_symlink() {
            kind::LINK
        } else if ft.is_dir() {
            kind::DIR
        } else if ft.is_file() {
            kind::FILE
        } else {
            kind::OTHER
        };
        let name = e.file_name().as_encoded_bytes().to_vec();
        #[cfg(unix)]
        let same_dev = {
            use std::os::unix::fs::MetadataExt;
            md.dev() == root.dev || root.cross_mounts
        };
        #[cfg(not(unix))]
        let same_dev = {
            let _ = root;
            true
        };
        if k_ == kind::DIR && same_dev {
            descend.push((entries.len(), path.join(e.file_name())));
        }
        entries.push(RefEntry {
            ext: if k_ == kind::FILE {
                ext_of(&name)
            } else {
                Vec::new()
            },
            name,
            kind: k_,
            size: if k_ == kind::DIR { 0 } else { md.len() as i64 },
            mtime: mtime_of(&md),
        });
    }
    out.listings[k] = entries;
    for (j, p) in descend {
        let r = Ref::Entry {
            listing: k as u32,
            entry: j as u32,
        };
        walk_ref(out, r, &p, root);
    }
}

fn assert_same(res: &SliceResult, want: &Reference) {
    assert_eq!(res.paths, want.paths, "paths");
    assert_eq!(res.refs, want.refs, "refs");
    let mut bounds = vec![0usize];
    for l in &want.listings {
        bounds.push(bounds.last().unwrap() + l.len());
    }
    assert_eq!(res.bounds, bounds, "bounds");
    let e = &res.entries;
    assert_eq!(
        e.names.len(),
        e.name_lens.iter().map(|&n| n as usize).sum::<usize>()
    );
    for col_len in [
        e.ext.len(),
        e.flags.len(),
        e.size.len(),
        e.alloc.len(),
        e.mtime.len(),
        e.ctime.len(),
        e.atime.len(),
        e.ino.len(),
        e.nlink.len(),
        e.dev.len(),
        e.name_lens.len(),
    ] {
        assert_eq!(col_len, e.len());
    }
    let wanted: Vec<&RefEntry> = want.listings.iter().flatten().collect();
    for (i, (name, w)) in e.names().zip(&wanted).enumerate() {
        let what = format!("entry {i} {:?}", String::from_utf8_lossy(&w.name));
        assert_eq!(name, w.name.as_slice(), "{what}");
        assert_eq!(e.kind[i], w.kind, "{what}: kind");
        assert_eq!(e.size[i], w.size, "{what}: size");
        assert_eq!(e.mtime[i], w.mtime, "{what}: mtime");
        assert_eq!(res.ext_table[e.ext[i] as usize], w.ext, "{what}: ext");
    }
    // the table holds each lowercased extension once, "" first
    assert_eq!(res.ext_table[0], b"");
    let mut seen = std::collections::HashSet::new();
    assert!(res.ext_table.iter().all(|x| seen.insert(x.clone())));
    let errors: Vec<(Ref, PathBuf)> = res
        .errors
        .iter()
        .map(|e| (e.target, e.path.clone()))
        .collect();
    assert_eq!(errors, want.errors, "errors");
    assert!(res.frontier.is_empty(), "frontier: {:?}", res.frontier);
    assert!(!res.cancelled);
}

/// One untruncated call, compared with the reference walker (read twice first: a first read can
/// still bump timestamps).
fn compare(tasks: Vec<Task>, opts: &SliceOptions) -> SliceResult {
    settle_tasks(&tasks);
    let want = reference(&tasks, &opts.roots);
    let res = scan(tasks, opts);
    assert_same(&res, &want);
    res
}

fn settle_tasks(tasks: &[Task]) {
    for t in tasks {
        settle(&t.path);
    }
}

/// Read every directory once: the first read of fresh directories can update their timestamps
/// (relatime atimes; NTFS refreshes a directory's entry in its parent lazily).
fn settle(root: &Path) {
    if let Ok(rd) = fs::read_dir(os_path(root)) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                settle(&root.join(e.file_name()));
            }
        }
    }
}

fn scan_one(root: &Path, ro: RootOptions) -> SliceResult {
    compare(vec![task(1, root, 0)], &SliceOptions::unbounded(vec![ro]))
}

// ------------------------------------------------------------------------------ trees

const EXT_NAMES: [&str; 14] = [
    "a.TXT",
    "b.txt",
    ".hidden",
    "noext",
    "trailing.",
    "x.0123456789abcdef",
    "x.0123456789abcdefg",
    "multi.part.tar.GZ",
    "ÜBER.ÄÖÜ",
    "sigma.ΣΑΣ",
    "dotted-İ.İJ",
    "照片.JPG",
    "emoji😀.😀X",
    "MiXeD.JpG",
];

fn create(path: &Path, bytes: &[u8]) {
    fs::write(os_path(path), bytes).unwrap();
}

fn build_tree(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    for (i, size) in [0usize, 1, 511, 512, 513, 4095, 4096, 4097, 100_000]
        .into_iter()
        .enumerate()
    {
        create(&root.join(format!("size{i}.bin")), &vec![b'x'; size]);
    }
    for name in EXT_NAMES {
        create(&root.join(name), &vec![b'e'; name.len()]);
    }
    fs::create_dir(root.join("empty")).unwrap();
    fs::create_dir(root.join("中文 文件夹")).unwrap();
    create(&root.join("中文 文件夹").join("照片.jpg"), &[b'p'; 1234]);
    let mut deep = root.join("deep");
    for i in 0..40 {
        deep = deep.join(format!("level{i}"));
    }
    fs::create_dir_all(&deep).unwrap();
    create(&deep.join("bottom.dat"), &[b'b'; 77]);
    let big = root.join("big");
    fs::create_dir(&big).unwrap();
    for i in 0..3000 {
        // several directory-query buffers
        create(
            &big.join(format!("file-with-a-fairly-long-name-{i:05}.log")),
            b"",
        );
    }
    let nest = root.join("nest");
    for a in 0..4 {
        for b in 0..3 {
            let d = nest.join(format!("a{a}")).join(format!("b{b}"));
            fs::create_dir_all(&d).unwrap();
            create(&d.join(format!("f{a}{b}.py")), &vec![b'x'; a * 10 + b]);
        }
    }
    settle(root);
    root.to_path_buf()
}

struct Tree {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn tree() -> Tree {
    let dir = tempfile::tempdir().unwrap();
    let root = build_tree(&dir.path().join("root"));
    Tree { _dir: dir, root }
}

// ------------------------------------------------------------------------------ equality

#[test]
fn varied_tree_is_identical() {
    let _s = serial();
    let t = tree();
    let res = scan_one(&t.root, root_opts(&t.root));
    assert!(res.listings() > 50 && res.entries.len() > 3000);
    for ext in [
        "jpg",
        "äöü",
        "σας",
        "i\u{307}j",
        "gz",
        "😀x",
        "0123456789abcdef",
    ] {
        assert!(
            res.ext_table.iter().any(|e| e == ext.as_bytes()),
            "{ext} missing from {:?}",
            res.ext_table
        );
    }
    // files only: folders have no extension
    let names = name_strings(&res);
    let i = names.iter().position(|n| n == "中文 文件夹").unwrap();
    assert_eq!(res.entries.ext[i], 0);
    assert_eq!(res.entries.kind[i], kind::DIR);
}

#[test]
fn thread_count_does_not_change_output() {
    let _s = serial();
    let t = tree();
    let mut opts = SliceOptions::unbounded(vec![root_opts(&t.root)]);
    let first = scan(vec![task(1, &t.root, 0)], &opts);
    for threads in [1, 2, 3, 5, 7, 64] {
        // more sizes than pools are kept
        opts.threads = threads;
        let res = scan(vec![task(1, &t.root, 0)], &opts);
        assert_eq!(res.refs, first.refs, "{threads}");
        assert_eq!(res.paths, first.paths, "{threads}");
        assert_eq!(res.bounds, first.bounds, "{threads}");
        assert_eq!(res.ext_table, first.ext_table, "{threads}");
        let (mut a, mut b) = (res.entries.clone(), first.entries.clone());
        a.atime.clear();
        b.atime.clear();
        assert_eq!(a, b, "{threads}");
    }
}

#[test]
fn root_options_apply() {
    let _s = serial();
    let t = tree();
    let ro = RootOptions {
        cluster_size: 8192,
        resident_max: 512,
        ..root_opts(&t.root)
    };
    let res = scan_one(&t.root, ro);
    let alloc: BTreeMap<String, i64> = name_strings(&res)
        .into_iter()
        .zip(res.entries.alloc.iter().copied())
        .collect();
    if cfg!(windows) {
        assert_eq!(alloc["size2.bin"], 0); // 511 bytes: resident
        assert_eq!(alloc["size4.bin"], 8192); // 513
        assert_eq!(alloc["size7.bin"], 8192); // 4097
        assert_eq!(alloc["size8.bin"], 106_496); // 100,000 in 8 KiB clusters
    } else {
        assert!(alloc["size8.bin"] >= 100_000 || alloc["size8.bin"] == 0);
    }
    scan_one(
        &t.root,
        RootOptions {
            cluster_size: 0,
            resident_max: 0,
            ..root_opts(&t.root)
        },
    );
    // the listing rule as each file system has it (whatever this volume is; ignored on POSIX)
    for listed_alloc in [
        hardscan::ListedAlloc::Ntfs,
        hardscan::ListedAlloc::Refs,
        hardscan::ListedAlloc::Btrfs,
    ] {
        scan_one(
            &t.root,
            RootOptions {
                listed_alloc,
                ..root_opts(&t.root)
            },
        );
    }
}

#[test]
fn several_tasks_roots_and_failures() {
    let _s = serial();
    let t = tree();
    let tmp = t.root.parent().unwrap();
    let other = tmp.join("other");
    fs::create_dir(&other).unwrap();
    create(&other.join("z.Z"), b"zz");
    fs::create_dir(other.join("sub")).unwrap();
    let tasks = vec![
        task(5, &t.root.join("nest"), 0),
        task(9, &tmp.join("missing"), 1),
        task(7, &other, 1),
        task(8, &t.root.join("size1.bin"), 0),
        task(6, &t.root.join("中文 文件夹"), 0),
    ];
    let roots = vec![
        root_opts(&t.root),
        RootOptions {
            cluster_size: 512,
            ..root_opts(&other)
        },
    ];
    let res = compare(tasks, &SliceOptions::unbounded(roots));
    let targets: Vec<Ref> = res.errors.iter().map(|e| e.target).collect();
    assert_eq!(targets, [Ref::Node(9), Ref::Node(8)]);
    assert!(res.errors.iter().all(|e| !e.message.is_empty()));
}

#[test]
fn only_failures_give_empty_columns() {
    let _s = serial();
    let tmp = tempfile::tempdir().unwrap();
    let res = compare(
        vec![task(1, &tmp.path().join("nope"), 0)],
        &SliceOptions::unbounded(vec![RootOptions::default()]),
    );
    assert_eq!(res.listings(), 0);
    assert_eq!(res.bounds, [0]);
    assert!(res.entries.is_empty() && res.errors.len() == 1);
}

#[test]
fn empty_task_list() {
    let _s = serial();
    let res = compare(
        Vec::new(),
        &SliceOptions::unbounded(vec![RootOptions::default()]),
    );
    assert_eq!(res.listings(), 0);
    assert_eq!(res.ext_table, [b"".to_vec()]);
}

#[test]
fn a_task_root_out_of_range_is_refused() {
    let _s = serial();
    let tmp = tempfile::tempdir().unwrap();
    let err = scan_slice(
        vec![task(1, tmp.path(), 1)],
        &SliceOptions::unbounded(vec![RootOptions::default()]),
    )
    .unwrap_err();
    assert_eq!(err, hardscan::Error::RootOutOfRange { task: 0, root: 1 });
    assert!(err.to_string().contains("out of range"));
}

#[test]
fn symlinks_are_not_followed() {
    let _s = serial();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("links");
    let target = root.join("target");
    fs::create_dir_all(&target).unwrap();
    create(&target.join("inside.txt"), b"inside");
    create(&root.join("file.txt"), b"file");
    let mut made = 0;
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        made += symlink(&target, root.join("dirlink")).is_ok() as usize;
        made += symlink(root.join("file.txt"), root.join("filelink")).is_ok() as usize;
        made += symlink(root.join("gone"), root.join("dangling")).is_ok() as usize;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        // unprivileged Windows without developer mode: no symlinks
        made += symlink_dir(&target, root.join("dirlink")).is_ok() as usize;
        made += symlink_file(root.join("file.txt"), root.join("filelink")).is_ok() as usize;
        made += symlink_file(root.join("gone"), root.join("dangling")).is_ok() as usize;
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("junction"))
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success());
        made += ok as usize;
    }
    if made == 0 {
        eprintln!("skipped: cannot create links here");
        return;
    }
    let res = scan_one(&root, root_opts(&root));
    assert_eq!(res.paths, [root.clone(), target]); // links are entries, never listings
    let links = res
        .entries
        .kind
        .iter()
        .filter(|&&k| k == kind::LINK)
        .count();
    assert_eq!(links, made);
}

// ------------------------------------------------------------------------------ slicing

fn check_slice(res: &SliceResult, ntasks: usize) {
    let l = res.listings();
    assert_eq!(res.paths.len(), l);
    assert_eq!(res.bounds.len(), l + 1);
    let is_dir = |listing: u32, entry: u32| {
        res.entries.kind[res.bounds[listing as usize] + entry as usize] == kind::DIR
    };
    for (k, r) in res.refs.iter().enumerate() {
        if let Ref::Entry { listing, entry } = *r {
            assert!((listing as usize) < k);
            assert!(is_dir(listing, entry));
        }
    }
    for t in &res.frontier {
        if let Ref::Entry { listing, entry } = t.target {
            assert!((listing as usize) < l);
            assert!(is_dir(listing, entry));
        }
    }
    for e in &res.errors {
        if let Ref::Entry { listing, .. } = e.target {
            assert!((listing as usize) < l);
        }
    }
    if ntasks > 0 {
        assert!(l >= 1 || res.errors.len() == ntasks);
    }
}

#[test]
fn zero_budget_still_lists_and_returns_a_valid_frontier() {
    let _s = serial();
    let t = tree();
    let tasks = || vec![task(1, &t.root, 0)];
    let opts = SliceOptions {
        roots: vec![root_opts(&t.root)],
        budget: Duration::ZERO,
        ..SliceOptions::default()
    };
    let res = scan(tasks(), &opts);
    check_slice(&res, 1);
    assert_eq!(res.listings(), 1);
    assert!(!res.frontier.is_empty());
    let opts = SliceOptions {
        roots: vec![root_opts(&t.root)],
        max_entries: 1,
        threads: 1,
        ..SliceOptions::default()
    };
    let res = scan(tasks(), &opts);
    check_slice(&res, 1);
    assert_eq!(res.listings(), 1);
    assert!(!res.frontier.is_empty());
    // the frontier holds the root's folders, in order, as entries of listing 0
    let dirs: Vec<u32> = (0..res.entries.len() as u32)
        .filter(|&j| res.entries.kind[j as usize] == kind::DIR)
        .collect();
    let front: Vec<Ref> = res.frontier.iter().map(|t| t.target).collect();
    let want: Vec<Ref> = dirs
        .iter()
        .map(|&j| Ref::Entry {
            listing: 0,
            entry: j,
        })
        .collect();
    assert_eq!(front, want);
}

type Facts = (u8, u32, i64, i64, u32, Vec<u8>);

/// Full path (bytes) of every entry -> what its listing said.
fn add_to_snapshot(snap: &mut BTreeMap<Vec<u8>, Facts>, res: &SliceResult) {
    let offs = res.entries.name_offsets();
    let e = &res.entries;
    for k in 0..res.listings() {
        let dir = res.paths[k].as_os_str().as_encoded_bytes();
        for i in res.listing(k) {
            let mut key = dir.to_vec();
            key.push(b'/');
            key.extend_from_slice(&e.names[offs[i]..offs[i + 1]]);
            let facts = (
                e.kind[i],
                e.flags[i] & !flag::HARDLINK_DUP,
                e.size[i],
                e.alloc[i],
                e.mtime[i],
                res.ext_table[e.ext[i] as usize].clone(),
            );
            assert!(snap.insert(key, facts).is_none(), "listed twice");
        }
    }
}

/// As the scanner drives the walker: `workers` threads take batches of up to `batch` tasks from
/// one queue, each call cut short by its budget, the frontier going back to the queue.
fn sliced_snapshot(
    root: &Path,
    opts: &SliceOptions,
    batch: usize,
    workers: usize,
) -> (BTreeMap<Vec<u8>, Facts>, usize) {
    let queue = Mutex::new(VecDeque::from([task(1, root, 0)]));
    let snap = Mutex::new(BTreeMap::new());
    let busy = AtomicUsize::new(0);
    let calls = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let tasks: Vec<Task> = {
                        let mut q = queue.lock().unwrap();
                        let n = q.len().min(batch);
                        if n > 0 {
                            busy.fetch_add(1, Ordering::SeqCst);
                        }
                        q.drain(..n).collect()
                    };
                    if tasks.is_empty() {
                        if busy.load(Ordering::SeqCst) == 0 {
                            return;
                        }
                        std::thread::yield_now();
                        continue;
                    }
                    let n = tasks.len();
                    let res = scan(tasks, opts);
                    check_slice(&res, n);
                    assert!(res.errors.is_empty(), "{:?}", res.errors);
                    calls.fetch_add(1, Ordering::SeqCst);
                    add_to_snapshot(&mut snap.lock().unwrap(), &res);
                    {
                        let mut q = queue.lock().unwrap();
                        for t in res.frontier {
                            // a task for a later call: the caller would give it its node id
                            q.push_back(Task {
                                target: Ref::Node(2),
                                ..t
                            });
                        }
                    }
                    busy.fetch_sub(1, Ordering::SeqCst);
                }
            });
        }
    });
    (snap.into_inner().unwrap(), calls.into_inner())
}

#[test]
fn sliced_scans_build_the_same_tree() {
    let _s = serial();
    let t = tree();
    let all = SliceOptions::unbounded(vec![root_opts(&t.root)]);
    let mut whole = BTreeMap::new();
    add_to_snapshot(&mut whole, &scan(vec![task(1, &t.root, 0)], &all));
    let base = SliceOptions {
        roots: vec![root_opts(&t.root)],
        threads: 1,
        ..SliceOptions::default()
    };
    let cases = [
        // one directory per call: maximal slicing
        (
            SliceOptions {
                budget: Duration::ZERO,
                ..base.clone()
            },
            1,
            1,
        ),
        (
            SliceOptions {
                max_entries: 1,
                ..base.clone()
            },
            256,
            4,
        ),
        (
            SliceOptions {
                budget: Duration::from_micros(500),
                ..base.clone()
            },
            256,
            4,
        ),
        (
            SliceOptions {
                budget: Duration::from_millis(5),
                threads: 2,
                ..base.clone()
            },
            256,
            4,
        ),
        (
            SliceOptions {
                budget: Duration::from_secs(1),
                ..base.clone()
            },
            256,
            4,
        ),
    ];
    for (opts, batch, workers) in cases {
        let (snap, calls) = sliced_snapshot(&t.root, &opts, batch, workers);
        assert!(calls >= 1);
        if opts.budget.is_zero() {
            assert!(calls > 50, "{calls}");
        }
        assert_eq!(snap.len(), whole.len(), "{opts:?}");
        assert!(snap == whole, "{opts:?}");
    }
}

#[test]
fn gates_and_the_shared_carriers_do_not_change_output() {
    // untruncated slices are the same whatever the width and the gate (docs/native.md 2.2
    // 「列目录线程」); the gate's limit holds
    let _s = serial();
    let t = tree();
    let mut opts = SliceOptions::unbounded(vec![root_opts(&t.root)]);
    opts.threads = 1;
    let first = scan(vec![task(1, &t.root, 0)], &opts);
    for (threads, limit) in [
        (0, None),
        (0, Some(1)),
        (0, Some(2)),
        (0, Some(5)),
        (7, Some(2)),
        (8, Some(64)),
    ] {
        opts.threads = threads;
        let gate = limit.map(ListingGate::new);
        opts.gate = gate.clone();
        let res = scan(vec![task(1, &t.root, 0)], &opts);
        let what = format!("threads {threads}, gate {limit:?}");
        assert_eq!(res.refs, first.refs, "{what}");
        assert_eq!(res.paths, first.paths, "{what}");
        assert_eq!(res.bounds, first.bounds, "{what}");
        let (mut a, mut b) = (res.entries.clone(), first.entries.clone());
        a.atime.clear();
        b.atime.clear();
        assert_eq!(a, b, "{what}");
        if let (Some(g), Some(n)) = (gate, limit) {
            let (busy, peak, _) = g.window();
            assert!((1..=n).contains(&peak), "{what}: peak {peak}");
            assert!(busy > 0.0, "{what}");
            assert_eq!(g.inflight(), 0, "{what}");
        }
    }
    let info = hardscan::listing_threads(false);
    assert!(info.cpus >= 1 && info.least <= info.start && info.start <= info.max);
    assert!(info.threads >= 1 && info.width >= 1 && info.low <= info.high);
    assert_eq!(hardscan::default_threads(), info.start);
}

#[test]
fn a_paused_gate_hands_back_what_can_wait() {
    let _s = serial();
    let t = tree();
    let gate = ListingGate::new(4);
    gate.set(None, Some(true));
    let opts = SliceOptions {
        gate: Some(gate.clone()),
        ..SliceOptions::unbounded(vec![root_opts(&t.root)])
    };
    let tasks = vec![task(1, &t.root, 0), task(2, &t.root.join("nest"), 0)];
    let res = scan(tasks.clone(), &opts);
    assert_eq!(res.listings(), 0);
    assert_eq!(res.frontier, tasks);
    assert_eq!(gate.inflight(), 0);
    // resumed while a call waits: it goes on
    gate.set(Some(2), Some(false));
    assert!(!gate.paused() && gate.limit() == 2);
    let res = scan(tasks, &opts);
    assert!(res.frontier.is_empty() && res.listings() > 50);
}

#[test]
fn a_cancelled_call_hands_everything_back() {
    let _s = serial();
    let t = tree();
    let cancel = Arc::new(AtomicBool::new(true));
    let opts = SliceOptions {
        cancel: Some(cancel.clone()),
        ..SliceOptions::unbounded(vec![root_opts(&t.root)])
    };
    let tasks = vec![task(1, &t.root, 0), task(2, &t.root.join("nest"), 0)];
    let res = scan(tasks.clone(), &opts);
    assert!(res.cancelled);
    assert_eq!(res.listings(), 0);
    assert_eq!(res.frontier, tasks);
    cancel.store(false, Ordering::Relaxed);
    let res = scan(tasks, &opts);
    assert!(!res.cancelled && res.frontier.is_empty() && res.listings() > 50);
}

#[test]
fn slices_append_like_merge_slices() {
    let _s = serial();
    let t = tree();
    let opts = SliceOptions::unbounded(vec![root_opts(&t.root)]);
    let a = scan(vec![task(1, &t.root.join("nest"), 0)], &opts);
    let b = scan(vec![task(2, &t.root.join("中文 文件夹"), 0)], &opts);
    let c = scan(
        vec![
            task(1, &t.root.join("nest"), 0),
            task(2, &t.root.join("中文 文件夹"), 0),
        ],
        &opts,
    );
    let mut merged = a.clone();
    merged.append(b);
    assert_eq!(merged.refs, c.refs);
    assert_eq!(merged.paths, c.paths);
    assert_eq!(merged.bounds, c.bounds);
    assert_eq!(merged.ext_table, c.ext_table);
    let (mut x, mut y) = (merged.entries.clone(), c.entries.clone());
    x.atime.clear();
    y.atime.clear();
    assert_eq!(x, y);
    // into an empty result
    let mut empty = SliceResult::default();
    empty.append(a.clone());
    assert_eq!(empty.refs, a.refs);
    assert_eq!(empty.bounds, a.bounds);
    assert_eq!(empty.entries.ext, a.entries.ext);
}

// ------------------------------------------------------------------------------ access times

#[cfg(any(windows, target_os = "linux"))]
fn set_atime(dir: &Path, t: std::time::SystemTime) {
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .access_mode(0x100) // FILE_WRITE_ATTRIBUTES
            .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
            .open(dir)
            .unwrap()
    };
    #[cfg(not(windows))]
    let f = fs::File::open(dir).unwrap();
    let mtime = fs::metadata(dir).unwrap().modified().unwrap();
    f.set_times(fs::FileTimes::new().set_accessed(t).set_modified(mtime))
        .unwrap();
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn listing_keeps_folder_access_times() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let _s = serial();
    let t = tree();
    let old = UNIX_EPOCH + Duration::from_secs(978_307_200); // 2001-01-01
    let mut folders = vec![t.root.clone()];
    let mut i = 0;
    while i < folders.len() {
        for e in fs::read_dir(&folders[i]).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                folders.push(e.path());
            }
        }
        i += 1;
    }
    let age = || {
        for d in folders.iter().rev() {
            // children first: setting a folder's times touches its parent
            set_atime(d, old);
        }
    };
    let atime = |d: &Path| fs::metadata(d).unwrap().accessed().unwrap();
    let near = |a: SystemTime| {
        a.duration_since(old).unwrap_or_default() <= Duration::from_secs(2)
            && old.duration_since(a).unwrap_or_default() <= Duration::from_secs(2)
    };
    let changed = |keep: bool| {
        let opts = SliceOptions {
            keep_access_time: keep,
            ..SliceOptions::unbounded(vec![root_opts(&t.root)])
        };
        let res = scan(vec![task(1, &t.root, 0)], &opts);
        assert_eq!(res.listings(), folders.len());
        folders.iter().filter(|d| !near(atime(d))).count()
    };
    age();
    assert!(folders.iter().all(|d| near(atime(d))));
    assert_eq!(changed(true), 0, "folders got a new access time");
    age();
    if changed(false) == 0 {
        eprintln!("listing does not update access times here (noatime or similar)");
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn keeping_access_times_changes_nothing() {
    let _s = serial();
    let t = tree();
    for keep in [false, true] {
        let opts = SliceOptions {
            keep_access_time: keep,
            ..SliceOptions::unbounded(vec![root_opts(&t.root)])
        };
        compare(vec![task(1, &t.root, 0)], &opts);
    }
}

// ------------------------------------------------------------------------------ misc API

#[test]
fn bulk_listing_reports_a_state() {
    let state = hardscan::bulk_listing();
    if cfg!(target_os = "macos") {
        assert!(state == "untested" || state.starts_with("on") || state.starts_with("off"));
    } else {
        assert_eq!(state, "unsupported");
    }
}

#[test]
fn lower_ext_is_pythons_lower() {
    assert_eq!(lower_ext(b"TXT"), b"txt");
    assert_eq!(lower_ext("ÄÖÜ".as_bytes()), "äöü".as_bytes());
    assert_eq!(lower_ext("ΣΑΣ".as_bytes()), "σας".as_bytes()); // final sigma
    assert_eq!(lower_ext("İJ".as_bytes()), "i\u{307}j".as_bytes());
    // bytes that are not UTF-8 stay as they are (surrogatepass / surrogateescape)
    assert_eq!(lower_ext(b"A\xffB"), b"a\xffb");
    assert_eq!(lower_ext(b"\xed\xa0\x80AB"), b"\xed\xa0\x80ab");
}

#[test]
fn refs_shift() {
    assert_eq!(Ref::Node(3).shifted(10), Ref::Node(3));
    assert_eq!(
        Ref::Entry {
            listing: 1,
            entry: 2
        }
        .shifted(10),
        Ref::Entry {
            listing: 11,
            entry: 2
        }
    );
}

// ------------------------------------------------------------------------------ LinkSet

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn link_set_marks_what_the_reference_loop_marks() {
    let mut rng = Rng(7);
    let mut links = LinkSet::new();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..5 {
        let n = 2000;
        let dev: Vec<u64> = (0..n).map(|_| 1 + rng.below(2)).collect();
        let ino: Vec<u64> = (0..n).map(|_| 1 + rng.below(399)).collect();
        let nlink: Vec<u16> = (0..n).map(|_| 1 + rng.below(3) as u16).collect();
        let kinds: Vec<u8> = (0..n)
            .map(|_| [0, 0, 0, 1][rng.below(4) as usize])
            .collect();
        let mut flags: Vec<u32> = (0..n).map(|_| (rng.below(2) as u32) << 5).collect();
        let mut want = flags.clone();
        for i in 0..n {
            if nlink[i] > 1 && kinds[i] == kind::FILE && !seen.insert((dev[i], ino[i])) {
                want[i] |= flag::HARDLINK_DUP;
            }
        }
        let marked = links.mark(&dev, &ino, &nlink, &kinds, &mut flags);
        assert_eq!(flags, want);
        assert_eq!(
            marked,
            want.iter()
                .filter(|&&f| f & flag::HARDLINK_DUP != 0)
                .count()
        );
        assert_eq!(links.len(), seen.len());
    }
    assert!(seen.iter().all(|&(d, i)| links.contains(d, i)));
}

#[test]
#[should_panic(expected = "length")]
fn link_set_refuses_mismatched_columns() {
    let mut links = LinkSet::new();
    links.mark(&[0; 3], &[0, 1, 2], &[2; 3], &[0; 3], &mut [0; 2]);
}

#[test]
fn link_set_marks_slice_entries() {
    let _s = serial();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("hl");
    fs::create_dir(&root).unwrap();
    create(&root.join("orig.bin"), &[b'h'; 5000]);
    if fs::hard_link(root.join("orig.bin"), root.join("link.bin")).is_err() {
        return;
    }
    let mut res = scan(
        vec![task(1, &root, 0)],
        &SliceOptions::unbounded(vec![root_opts(&root)]),
    );
    let mut links = LinkSet::new();
    let marked = links.mark_entries(&mut res.entries);
    if cfg!(windows) {
        assert_eq!(marked, 0); // no link counts from a directory listing on Windows
    } else {
        assert_eq!(marked, 1);
        assert_eq!(links.len(), 1);
    }
}

// ------------------------------------------------------------------------------ Windows specifics

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;

    use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    fn wide(p: &Path) -> Vec<u16> {
        os_path(p).as_os_str().encode_wide().chain([0]).collect()
    }

    fn set_attrs(p: &Path, attrs: u32) {
        // SAFETY: a NUL-terminated path.
        assert!(unsafe { SetFileAttributesW(wide(p).as_ptr(), attrs) } != 0);
    }

    fn fsctl(p: &Path, code: u32, payload: &[u8]) -> bool {
        let f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(p)
            .unwrap();
        let mut ret = 0u32;
        // SAFETY: valid handle and buffers for their lengths.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle(),
                code,
                if payload.is_empty() {
                    std::ptr::null()
                } else {
                    payload.as_ptr().cast()
                },
                payload.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        ok != 0
    }

    /// The 64-bit file id (on NTFS the file reference the change journal names) and the file
    /// system of `p`.
    fn file_index(p: &Path) -> (u64, String) {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, GetVolumeInformationByHandleW,
        };
        let f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS: folders too
            .open(os_path(p))
            .unwrap();
        // SAFETY: valid handle and out structs/buffers of their sizes.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            assert!(GetFileInformationByHandle(f.as_raw_handle(), &mut info) != 0);
            let mut fs_name = [0u16; 64];
            let ok = GetVolumeInformationByHandleW(
                f.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            );
            let end = fs_name.iter().position(|&c| c == 0).unwrap_or(0);
            let name = if ok != 0 {
                String::from_utf16_lossy(&fs_name[..end])
            } else {
                String::new()
            };
            (
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
                name,
            )
        }
    }

    /// Rows carry the file id the listing gave (USN replay finds folders by it on hard-scanned
    /// trees): on NTFS every row's `ino` is its file reference.
    #[test]
    fn rows_carry_the_file_id() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ids");
        fs::create_dir_all(root.join("sub").join("deeper")).unwrap();
        create(&root.join("a.txt"), b"a");
        create(&root.join("sub").join("b.txt"), b"b");
        let (_, fs_name) = file_index(&root);
        let res = scan_one(&root, root_opts(&root));
        let mut checked = 0;
        let names: Vec<String> = name_strings(&res);
        assert_eq!(names.len(), res.entries.ino.len());
        // the names are unique in this tree
        let mut want: BTreeMap<String, u64> = BTreeMap::new();
        for rel in ["a.txt", "sub", "sub/b.txt", "sub/deeper"] {
            let (id, _) = file_index(&rel.split('/').fold(root.clone(), |p, part| p.join(part)));
            want.insert(rel.rsplit('/').next().unwrap().to_string(), id);
        }
        for (name, &ino) in names.iter().zip(&res.entries.ino) {
            let Some(&id) = want.get(name) else { continue };
            if fs_name.eq_ignore_ascii_case("NTFS") {
                assert_eq!(ino, id, "{name}");
                assert_ne!(ino, 0, "{name}");
            } else {
                assert!(ino == 0 || ino == id, "{name}: {ino} / {id} on {fs_name}");
            }
            checked += 1;
        }
        assert_eq!(checked, 4, "{names:?}");
    }

    #[test]
    fn attributes_compressed_and_sparse() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("attrs");
        fs::create_dir(&root).unwrap();
        for (name, attrs) in [
            ("hidden.txt", 0x2),
            ("system.txt", 0x4),
            ("readonly.txt", 0x1),
            ("plain.txt", 0x80),
            ("notindexed.txt", 0x2000),
            ("temp.tmp", 0x100),
            ("offline.dat", 0x1000),
        ] {
            create(&root.join(name), &[b'a'; 700]);
            set_attrs(&root.join(name), attrs);
        }
        let hdir = root.join("hiddendir");
        fs::create_dir(&hdir).unwrap();
        create(&hdir.join("in.txt"), b"in");
        set_attrs(&hdir, 0x2 | 0x4);
        let sparse = root.join("sparse.bin");
        create(&sparse, b"");
        let compressed = root.join("compressed.bin");
        create(&compressed, b"");
        let have_sparse = fsctl(&sparse, 0x900C4, &[]); // FSCTL_SET_SPARSE
        let have_comp = fsctl(&compressed, 0x9C040, &1u16.to_le_bytes()); // FSCTL_SET_COMPRESSION
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = fs::OpenOptions::new().write(true).open(&sparse).unwrap();
            f.seek(SeekFrom::Start(5_000_000)).unwrap();
            f.write_all(b"tail").unwrap();
            f.sync_all().unwrap();
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(&compressed)
                .unwrap();
            f.write_all(&vec![0u8; 1_000_000]).unwrap();
            f.sync_all().unwrap();
        }
        let res = scan_one(
            &root,
            RootOptions {
                cluster_size: 4096,
                resident_max: 512,
                ..root_opts(&root)
            },
        );
        for e in fs::read_dir(&root).unwrap().flatten() {
            set_attrs(&e.path(), 0x80);
        }
        let names = name_strings(&res);
        let get = |n: &str| names.iter().position(|x| x == n).unwrap();
        let flags = |n: &str| res.entries.flags[get(n)];
        assert!(flags("hidden.txt") & flag::HIDDEN != 0);
        assert!(flags("readonly.txt") & flag::READONLY != 0);
        assert!(flags("system.txt") & flag::SYSTEM != 0);
        assert!(flags("temp.tmp") & flag::TEMPORARY != 0);
        assert!(flags("notindexed.txt") & flag::NOT_INDEXED != 0);
        assert!(flags("offline.dat") & flag::OFFLINE != 0);
        assert!(flags("hiddendir") & flag::SYSTEM != 0);
        assert_eq!(
            flags("plain.txt") & (flag::HIDDEN | flag::SYSTEM | flag::READONLY),
            0
        );
        if have_sparse {
            assert!(flags("sparse.bin") & flag::SPARSE != 0);
            assert!(res.entries.alloc[get("sparse.bin")] < 5_000_000);
        }
        if have_comp {
            assert!(flags("compressed.bin") & flag::COMPRESSED != 0);
        }
    }

    #[test]
    fn long_paths() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("long");
        let mut deep = root.clone();
        for i in 0..30 {
            deep = deep.join(format!("directory-level-{i:02}"));
        }
        fs::create_dir_all(os_path(&deep)).unwrap();
        create(&deep.join("leaf.txt"), b"leaf");
        assert!(deep.as_os_str().len() > 600);
        let res = scan_one(&root, root_opts(&root));
        assert_eq!(res.listings(), 31);
        assert!(res.errors.is_empty());
    }

    #[test]
    fn lone_surrogate_names() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("surrogates");
        fs::create_dir(&root).unwrap();
        let name = |units: &[u16]| PathBuf::from(OsString::from_wide(units));
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let lone = [w("lone"), vec![0xD800], w(".TXT")].concat();
        let ext = [w("ext."), vec![0xDC00], w("AB")].concat();
        for units in [lone, ext, w("pair\u{1F600}.X")] {
            create(&root.join(name(&units)), b"s");
        }
        fs::create_dir(root.join(name(&[w("dir"), vec![0xDFFF]].concat()))).unwrap();
        let res = scan_one(&root, root_opts(&root));
        assert_eq!(res.listings(), 2);
        // the lone surrogate's extension: "\udc00ab" in WTF-8
        assert!(res.ext_table.iter().any(|e| e == b"\xed\xb0\x80ab"));
    }

    #[test]
    fn denied_directory() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("acl");
        let denied = root.join("denied");
        fs::create_dir_all(&denied).unwrap();
        create(&denied.join("secret.txt"), b"s");
        create(&root.join("open.txt"), b"o");
        let everyone = "*S-1-1-0";
        let icacls = |args: &[&str]| {
            Command::new("icacls")
                .arg(&denied)
                .args(args)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !icacls(&["/deny", &format!("{everyone}:(RD)")]) {
            eprintln!("skipped: icacls failed");
            return;
        }
        let bypassed = fs::read_dir(&denied).is_ok();
        let res = (!bypassed).then(|| scan_one(&root, root_opts(&root)));
        assert!(icacls(&["/remove:d", everyone]));
        let Some(res) = res else {
            eprintln!("skipped: this environment lists directories despite a deny ACE");
            return;
        };
        assert_eq!(res.errors.len(), 1);
        assert_eq!(res.errors[0].path, denied);
    }

    #[test]
    fn trailing_dot_names_are_listed() {
        // Win32 trims a name's trailing dot or space: by its plain path the folder `odd.` would
        // list its sibling `odd`. The walker lists each as it is, also as tasks.
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dots");
        fs::create_dir(&root).unwrap();
        for name in ["odd.", "odd", "space "] {
            fs::create_dir(os_path(&root.join(name))).unwrap();
            let file = format!("in-{}.txt", name.trim_matches(['.', ' ']));
            create(&root.join(name).join(file), name.as_bytes());
        }
        let res = scan_one(&root, root_opts(&root));
        let mut paths = res.paths.clone();
        paths.sort();
        let mut want = vec![
            root.clone(),
            root.join("odd"),
            root.join("odd."),
            root.join("space "),
        ];
        want.sort();
        assert_eq!(paths, want);
        assert!(res.errors.is_empty() && res.entries.len() == 6);
        let res = compare(
            vec![task(1, &root.join("odd."), 0)],
            &SliceOptions::unbounded(vec![root_opts(&root)]),
        );
        assert_eq!(name_strings(&res), ["in-odd.txt"]);
    }

    fn stats() -> BTreeMap<&'static str, u64> {
        hardscan::open_stats(true).into_iter().collect()
    }

    /// The file system name and cluster size of the volume `p` is on.
    fn volume_of(p: &Path) -> (String, i64) {
        use windows_sys::Win32::Storage::FileSystem::{
            GetDiskFreeSpaceW, GetVolumeInformationW, GetVolumePathNameW,
        };
        let mut vol = vec![0u16; 1024];
        // SAFETY: NUL-terminated path, buffers for their lengths.
        assert!(
            unsafe { GetVolumePathNameW(wide(p).as_ptr(), vol.as_mut_ptr(), vol.len() as u32) }
                != 0
        );
        let mut fs_name = vec![0u16; 64];
        // SAFETY: as above; optional outputs null.
        let ok = unsafe {
            GetVolumeInformationW(
                vol.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        assert!(ok != 0);
        let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
        // SAFETY: as above.
        assert!(
            unsafe { GetDiskFreeSpaceW(vol.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) }
                != 0
        );
        let n = fs_name
            .iter()
            .position(|&u| u == 0)
            .unwrap_or(fs_name.len());
        (
            String::from_utf16_lossy(&fs_name[..n]),
            i64::from(spc) * i64::from(bps),
        )
    }

    /// FileStandardInfo's AllocationSize: what the file system counts for the file.
    fn std_alloc(p: &Path) -> i64 {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
        };
        let f = fs::File::open(os_path(p)).unwrap();
        let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle, `info` writable for its size.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                f.as_raw_handle(),
                FileStandardInfo,
                (&raw mut info).cast(),
                size_of::<FILE_STANDARD_INFO>() as u32,
            )
        };
        assert!(ok != 0);
        info.AllocationSize
    }

    #[test]
    fn listed_alloc_is_what_the_file_system_counts() {
        // with listed_alloc a file's size on disk is its listing entry's (asking the file where
        // the entry lags: sparse and compressed files just written; WOF files): data kept in the
        // file record takes no cluster, a sparse file only its written clusters, a compressed
        // file its compressed clusters (test_hardscan.py)
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let (fs_name, cluster) = volume_of(tmp.path());
        let listed = hardscan::ListedAlloc::from_fs_name(&fs_name);
        if listed == hardscan::ListedAlloc::Off {
            eprintln!("skipped: listing rule not measured on {fs_name}");
            return;
        }
        let root = tmp.path().join("listed");
        fs::create_dir(&root).unwrap();
        for n in [0usize, 100, 700, 800, 3000, 5000] {
            create(&root.join(format!("w_{n:05}")), &vec![b'x'; n]);
        }
        fs::File::create(root.join("setlen_100k"))
            .unwrap()
            .set_len(100_000)
            .unwrap();
        let text: Vec<u8> = b"SpaceKnife listed allocation test, compressible text. "
            .iter()
            .copied()
            .cycle()
            .take(1 << 20)
            .collect();
        let sparse = root.join("sparse.bin");
        create(&sparse, b"");
        let have_sparse = fsctl(&sparse, 0x900C4, &[]); // FSCTL_SET_SPARSE
        let compressed = root.join("compressed.bin");
        create(&compressed, b"");
        let have_comp = fsctl(&compressed, 0x9C040, &1u16.to_le_bytes()); // FSCTL_SET_COMPRESSION
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = fs::OpenOptions::new().write(true).open(&sparse).unwrap();
            for off in [0u64, 8 << 20, (16 << 20) - 4096] {
                f.seek(SeekFrom::Start(off)).unwrap();
                f.write_all(&[0xa5; 4096]).unwrap();
            }
            f.sync_all().unwrap(); // written out: the file is final, its listing entry lags
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(&compressed)
                .unwrap();
            f.write_all(&text).unwrap();
            f.sync_all().unwrap();
        }
        let wof = root.join("wof.bin");
        create(&wof, &text);
        let have_wof = listed == hardscan::ListedAlloc::Ntfs
            && Command::new("compact.exe")
                .args(["/c", "/exe:xpress4k"])
                .arg(&wof)
                .output()
                .is_ok_and(|o| o.status.success());
        let res = scan_one(
            &root,
            RootOptions {
                cluster_size: cluster,
                resident_max: 512,
                listed_alloc: listed,
                ..root_opts(&root)
            },
        );
        let names = name_strings(&res);
        let alloc: BTreeMap<&str, i64> = names
            .iter()
            .map(String::as_str)
            .zip(res.entries.alloc.iter().copied())
            .collect();
        for (&name, &got) in &alloc {
            let counted = std_alloc(&root.join(name));
            let want = if counted % cluster != 0 { 0 } else { counted };
            assert_eq!(got, want, "{name}: counted {counted}");
        }
        if listed == hardscan::ListedAlloc::Ntfs {
            // resident (where 700 B stop being resident depends on the record size: 1 KiB
            // records keep ~600 B, 4 KiB records more; the per-file check above covers it)
            assert_eq!(alloc["w_00100"], 0);
            assert_eq!(alloc["w_05000"], (5000 + cluster - 1) / cluster * cluster);
        }
        if have_sparse {
            assert!(alloc["sparse.bin"] > 0 && alloc["sparse.bin"] < 1 << 20);
        }
        if have_comp && listed == hardscan::ListedAlloc::Ntfs {
            assert!(alloc["compressed.bin"] > 0 && alloc["compressed.bin"] < 1 << 20);
        }
        if have_wof {
            assert!(alloc["wof.bin"] > 0 && alloc["wof.bin"] < 1 << 20);
        }
        // the estimate stays as it was: 700 B past resident_max is a whole cluster
        let old = scan_one(
            &root,
            RootOptions {
                cluster_size: cluster,
                resident_max: 512,
                ..root_opts(&root)
            },
        );
        let i = name_strings(&old)
            .iter()
            .position(|n| n == "w_00700")
            .unwrap();
        assert_eq!(old.entries.alloc[i], cluster);
    }

    #[test]
    fn io_saving_options_list_the_same_tree() {
        // skip_final (and its verification mode), dot_check and keep_where_updated change how
        // folders are listed, never what is listed
        let _s = serial();
        let t = tree();
        let base = SliceOptions::unbounded(vec![root_opts(&t.root)]);
        stats();
        for (skip_final, verify_final, dot_check, keep_where_updated) in [
            (true, false, false, true),
            (true, true, false, true),
            (false, false, true, true),
            (true, false, true, false),
        ] {
            let opts = SliceOptions {
                skip_final,
                dot_check,
                keep_where_updated,
                seams: hardscan::TestSeams {
                    verify_final,
                    ..base.seams
                },
                ..base.clone()
            };
            compare(vec![task(1, &t.root, 0)], &opts);
            let s = stats();
            assert_eq!(s["final_wrong"], 0, "{s:?}");
            if verify_final {
                assert!(s["final_verified"] > 0 && s["final_skipped"] == 0, "{s:?}");
            }
            let (fs_name, _) = volume_of(&t.root);
            if skip_final && ["NTFS", "ReFS"].contains(&fs_name.as_str()) {
                // the first 64 listings of a root (each call) send the query to check the rule
                assert!(s["final_verified"] > 0, "{s:?}");
            }
            if dot_check {
                assert!(s["dot_matched"] > 10 && s["dot_unmatched"] == 0, "{s:?}");
            }
        }
    }

    #[test]
    fn relative_opens_list_the_same_tree() {
        // subdirectories opened by name relative to their parent's handle (checked against the
        // listed file id) list exactly what opens by full path list
        let _s = serial();
        let t = tree();
        let opts = |relative_open| SliceOptions {
            relative_open,
            ..SliceOptions::unbounded(vec![root_opts(&t.root)])
        };
        stats();
        compare(vec![task(1, &t.root, 0)], &opts(false));
        let off = stats();
        compare(vec![task(1, &t.root, 0)], &opts(true));
        let on = stats();
        assert!(off["relative"] == 0 && off["by_path"] > 10, "{off:?}");
        assert!(on["relative"] > 10 && on["by_path"] == 1, "{on:?}"); // only the task by path
        assert_eq!(on["relative"] + on["by_path"], off["by_path"]);
        assert_eq!(
            on["relative_failed"] + on["id_mismatch"] + on["id_unknown"],
            0
        );
    }

    fn chain_to(top: &Path, deepest: usize) -> Vec<PathBuf> {
        // folders top, top\d…, … down to one whose path is exactly `deepest` units long
        let len = |p: &Path| p.as_os_str().encode_wide().count() + 4; // with \\?\
        let mut chain = vec![top.to_path_buf()];
        fs::create_dir(os_path(top)).unwrap();
        while len(chain.last().unwrap()) + 241 < deepest {
            let next = chain.last().unwrap().join("d".repeat(240));
            fs::create_dir(os_path(&next)).unwrap();
            chain.push(next);
        }
        let last = chain.last().unwrap();
        let next = last.join("d".repeat(deepest - len(last) - 1));
        fs::create_dir(os_path(&next)).unwrap();
        chain.push(next);
        chain
    }

    #[test]
    fn folder_past_the_path_limit_is_a_listing_error() {
        // a folder whose path is longer than Win32 allows (32,767 units) is a listing error.
        // Renaming the top folder longer puts the last folder past the limit.
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("toolong");
        fs::create_dir(&root).unwrap();
        let grow = 199;
        let top = root.join("a");
        let mut chain = chain_to(&top, 32_600 - grow);
        let last = chain.last().unwrap().join("d".repeat(200));
        fs::create_dir(os_path(&last)).unwrap();
        create(&last.join("unreached.txt"), b"u");
        chain.push(last);
        let longer = root.join("a".repeat(1 + grow));
        fs::rename(os_path(&top), os_path(&longer)).unwrap();
        let moved = |p: &Path| longer.join(p.strip_prefix(&top).unwrap());
        let opts = |relative_open| SliceOptions {
            relative_open,
            ..SliceOptions::unbounded(vec![root_opts(&root)])
        };
        for relative in [false, true] {
            let res = scan(vec![task(1, &root, 0)], &opts(relative));
            let errs: Vec<&PathBuf> = res.errors.iter().map(|e| &e.path).collect();
            assert_eq!(errs, [&moved(chain.last().unwrap())], "relative {relative}");
            assert_eq!(res.listings(), chain.len()); // the root and every folder above the last
        }
        fs::rename(os_path(&longer), os_path(&top)).unwrap();
        fs::remove_dir_all(os_path(&root)).unwrap();
    }

    // -------------------------------------------------------------------- case-only siblings

    /// Creates `folder` as a case-sensitive NTFS folder, or returns false (fsutil
    /// setCaseSensitiveInfo needs the WSL feature and an NTFS volume; its exit code says nothing,
    /// so the folder is tried).
    fn case_sensitive(folder: &Path) -> bool {
        fs::create_dir_all(folder).unwrap();
        let _ = Command::new("fsutil")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(folder)
            .arg("enable")
            .output();
        fs::create_dir(folder.join("probe")).unwrap();
        let ok = fs::create_dir(folder.join("PROBE")).is_ok();
        fs::remove_dir(folder.join("probe")).unwrap();
        if ok {
            fs::remove_dir(folder.join("PROBE")).unwrap();
        }
        ok
    }

    /// root/cs is case-sensitive: `a` and `A` with other contents (a subtree and nested twins
    /// inside `a`), folder `b` next to file `B`, and a plain folder. root/other makes the first
    /// listing hold two folders.
    fn build_twins(root: &Path) -> Option<PathBuf> {
        fs::create_dir_all(root.join("other")).unwrap();
        create(&root.join("other").join("o.txt"), b"o");
        let cs = root.join("cs");
        if !case_sensitive(&cs) || !case_sensitive(&cs.join("a")) {
            eprintln!("skipped: cannot make a case-sensitive folder here");
            return None;
        }
        let a = cs.join("a");
        fs::create_dir_all(a.join("sub").join("deeper")).unwrap();
        create(&a.join("sub").join("deeper").join("d.txt"), &[b'd'; 10]);
        fs::create_dir(a.join("x")).unwrap();
        fs::create_dir(a.join("X")).unwrap();
        create(&a.join("x").join("lower.txt"), b"x");
        create(&a.join("X").join("upper.txt"), b"XX");
        create(&a.join("f.txt"), b"fff");
        fs::create_dir(cs.join("A")).unwrap();
        create(&cs.join("A").join("only.txt"), &[b'A'; 5]);
        fs::create_dir(cs.join("b")).unwrap();
        create(&cs.join("b").join("in.txt"), b"b");
        create(&cs.join("B"), &[b'B'; 7]);
        fs::create_dir(cs.join("plain")).unwrap();
        create(&cs.join("plain").join("p.txt"), b"p");
        settle(root);
        Some(cs)
    }

    /// As WinBtrfs behaves: a strict subfolder is opened by its upcased name, FileIdExtd ids
    /// are the listed folder's own.
    fn winbtrfs(opts: SliceOptions) -> SliceOptions {
        SliceOptions {
            seams: hardscan::TestSeams {
                fold_strict: true,
                extd_self_ids: true,
                pinned_cap: None,
                ..opts.seams
            },
            ..opts
        }
    }

    fn fold_strict(opts: SliceOptions) -> SliceOptions {
        SliceOptions {
            seams: hardscan::TestSeams {
                fold_strict: true,
                ..opts.seams
            },
            ..opts
        }
    }

    #[test]
    fn case_twins_are_listed_by_their_file_ids() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("twins");
        let Some(_cs) = build_twins(&root) else {
            return;
        };
        let all = || SliceOptions::unbounded(vec![root_opts(&root)]);
        stats();
        compare(
            vec![task(1, &root, 0)],
            &winbtrfs(SliceOptions {
                threads: 1,
                ..all()
            }),
        );
        let s = stats();
        assert!(s["strict_by_id"] >= 3 && s["pinned"] >= 5, "{s:?}"); // a, b, a\x; and below
        assert!(s["strict_failed"] == 0 && s["ids_distrusted"] >= 1, "{s:?}");
        compare(vec![task(1, &root, 0)], &fold_strict(all()));
        compare(
            vec![task(1, &root, 0)],
            &fold_strict(SliceOptions {
                relative_open: false,
                ..all()
            }),
        );
    }

    #[test]
    fn case_twins_never_wait_in_the_frontier() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let Some(cs) = build_twins(&tmp.path().join("twins")) else {
            return;
        };
        let tasks = || vec![task(1, &cs, 0)];
        let opts = fold_strict(SliceOptions {
            roots: vec![RootOptions::default()],
            max_entries: 1,
            threads: 1,
            ..SliceOptions::default()
        });
        let res = scan(tasks(), &opts);
        check_slice(&res, 1);
        let front: Vec<&PathBuf> = res.frontier.iter().map(|t| &t.path).collect();
        assert_eq!(front, [&cs.join("plain")]);
        assert!(res.errors.is_empty());
        for p in ["a", "A", "b", r"a\sub", r"a\sub\deeper", r"a\x", r"a\X"] {
            assert!(res.paths.contains(&cs.join(p)), "{p}");
        }
        // budget 0: one listing, the twins in the frontier with the plain folder, no errors
        let zero = SliceOptions {
            budget: Duration::ZERO,
            max_entries: usize::MAX,
            ..opts.clone()
        };
        let res = scan(tasks(), &zero);
        assert_eq!(res.paths, std::slice::from_ref(&cs));
        assert!(res.errors.is_empty());
        let mut front: Vec<PathBuf> = res.frontier.iter().map(|t| t.path.clone()).collect();
        front.sort();
        let mut want: Vec<PathBuf> = ["A", "a", "b", "plain"]
            .iter()
            .map(|n| cs.join(n))
            .collect();
        want.sort();
        assert_eq!(front, want);
        // a cap of two entries: below `a` (opened by id) nothing more, and nothing left as a task
        stats();
        let capped_opts = SliceOptions {
            seams: hardscan::TestSeams {
                pinned_cap: Some(2),
                ..opts.seams
            },
            ..opts.clone()
        };
        let res = scan(tasks(), &capped_opts);
        check_slice(&res, 1);
        assert!(stats()["pinned_cap_hit"] >= 3);
        let capped: std::collections::BTreeSet<PathBuf> = res
            .errors
            .iter()
            .filter(|e| e.message == messages::CAP)
            .map(|e| e.path.clone())
            .collect();
        let want: std::collections::BTreeSet<PathBuf> = ["sub", "x", "X"]
            .iter()
            .map(|n| cs.join("a").join(n))
            .collect();
        assert_eq!(capped, want);
        let waiting: std::collections::BTreeSet<PathBuf> =
            res.frontier.iter().map(|t| t.path.clone()).collect();
        assert!(waiting.contains(&cs.join("b")) && waiting.contains(&cs.join("plain")));
        assert!(
            waiting
                .iter()
                .all(|p| ["A", "b", "plain"].iter().any(|n| *p == cs.join(n)))
        );
    }

    #[test]
    fn case_twins_without_ids_are_decided_by_their_stored_names() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let Some(cs) = build_twins(&tmp.path().join("twins")) else {
            return;
        };
        let opts = winbtrfs(SliceOptions {
            threads: 1,
            ..SliceOptions::unbounded(vec![RootOptions::default()])
        });
        let res = scan(vec![task(1, &cs, 0)], &opts);
        let errors: BTreeMap<PathBuf, String> = res
            .errors
            .iter()
            .map(|e| (e.path.clone(), e.message.clone()))
            .collect();
        assert_eq!(
            errors.keys().cloned().collect::<Vec<_>>(),
            [cs.join("a"), cs.join("b")]
        );
        assert_eq!(errors[&cs.join("a")], messages::CASE);
        assert!(![messages::CASE, messages::DUP].contains(&errors[&cs.join("b")].as_str()));
        assert!(res.paths.contains(&cs.join("A")) && res.paths.contains(&cs.join("plain")));
        assert!(!res.paths.iter().any(|p| p.starts_with(cs.join("a"))));
    }

    #[test]
    fn a_twin_that_is_a_symlink_is_not_followed() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let other = tmp.path().join("other").join("a");
        fs::create_dir_all(&other).unwrap();
        create(&other.join("OTHER_CONTENT.txt"), b"o");
        let x = tmp.path().join("x");
        if !case_sensitive(&x) {
            eprintln!("skipped: cannot make a case-sensitive folder here");
            return;
        }
        fs::create_dir(x.join("a")).unwrap();
        create(&x.join("a").join("real_content.txt"), b"r");
        if let Err(e) = std::os::windows::fs::symlink_dir(r"..\other\a", x.join("A")) {
            eprintln!("skipped: cannot make a directory symlink here: {e}");
            return;
        }
        for relative_open in [true, false] {
            let opts = fold_strict(SliceOptions {
                threads: 1,
                relative_open,
                ..SliceOptions::default()
            });
            let opts = SliceOptions {
                roots: vec![RootOptions::default()],
                ..opts
            };
            let res = scan(vec![task(1, &x, 0)], &opts);
            let l = listed(&res);
            assert_eq!(
                l.get(&x.join("a")),
                Some(&vec!["real_content.txt".to_owned()])
            );
            assert!(!l.contains_key(&x.join("A")) && res.errors.is_empty());
        }
    }

    #[test]
    fn a_chain_of_twins_stops_at_the_cap() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().join("t");
        if !case_sensitive(&top) {
            eprintln!("skipped: cannot make a case-sensitive folder here");
            return;
        }
        let mut level = top.clone();
        for _ in 0..8 {
            for name in ["q", "Q"] {
                assert!(case_sensitive(&level.join(name)));
                create(&level.join(name).join("f.txt"), b"x");
            }
            level = level.join("Q");
        }
        let opts = |cap: Option<usize>| {
            fold_strict(SliceOptions {
                roots: vec![RootOptions::default()],
                threads: 1,
                max_entries: 1,
                seams: hardscan::TestSeams {
                    pinned_cap: cap,
                    ..Default::default()
                },
                ..SliceOptions::default()
            })
        };
        let res = scan(vec![task(1, &top, 0)], &opts(Some(0)));
        check_slice(&res, 1);
        assert_eq!(res.paths, std::slice::from_ref(&top));
        assert!(res.errors.is_empty());
        let mut front: Vec<PathBuf> = res.frontier.iter().map(|t| t.path.clone()).collect();
        front.sort();
        assert_eq!(front, [top.join("Q"), top.join("q")]);
        let res = scan(vec![task(1, &top, 0)], &opts(Some(5)));
        check_slice(&res, 1);
        assert!(1 < res.listings() && res.listings() < 6);
        assert!(!res.frontier.is_empty() && res.errors.is_empty());
        let all = fold_strict(SliceOptions {
            threads: 1,
            ..SliceOptions::unbounded(vec![RootOptions::default()])
        });
        let res = scan(vec![task(1, &top, 0)], &all);
        assert_eq!(res.listings(), 17);
        assert!(res.frontier.is_empty());
    }

    #[test]
    fn tasks_are_checked_by_their_stored_names() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("names");
        fs::create_dir_all(root.join("Mixed").join("inner")).unwrap();
        create(&root.join("Mixed").join("in.txt"), b"i");
        settle(&root);
        let checked = |path: &Path| {
            SliceOptions::unbounded(vec![RootOptions {
                path: Some(path.to_path_buf()),
                verify_names: true,
                ..root_opts(&root)
            }])
        };
        let res = scan(
            vec![
                task(1, &root.join("MIXED"), 0),
                task(2, &root.join("Mixed").join("INNER"), 0),
            ],
            &checked(&root),
        );
        let errors: Vec<(Ref, PathBuf, String)> = res
            .errors
            .iter()
            .map(|e| (e.target, e.path.clone(), e.message.clone()))
            .collect();
        assert_eq!(
            errors,
            [
                (Ref::Node(1), root.join("MIXED"), messages::CASE.to_owned()),
                (
                    Ref::Node(2),
                    root.join("Mixed").join("INNER"),
                    messages::CASE.to_owned()
                ),
            ]
        );
        let res = compare(vec![task(1, &root.join("Mixed"), 0)], &checked(&root));
        assert_eq!(
            res.paths,
            [root.join("Mixed"), root.join("Mixed").join("inner")]
        );
        // the root typed in another case: what lies below it is compared, the root part not
        let upper = PathBuf::from(root.as_os_str().to_string_lossy().to_uppercase());
        let res = scan(
            vec![task(1, &upper, 0), task(2, &upper.join("Mixed"), 0)],
            &checked(&upper),
        );
        assert!(
            res.errors.is_empty() && res.listings() == 5,
            "{:?}",
            res.errors
        );
        // without the option NTFS tasks are not checked
        let res = scan(
            vec![task(1, &root.join("MIXED"), 0)],
            &SliceOptions::unbounded(vec![RootOptions {
                path: Some(root.clone()),
                ..root_opts(&root)
            }]),
        );
        assert!(res.errors.is_empty());
    }

    #[test]
    fn open_stats_name_every_counter() {
        let names: Vec<&str> = hardscan::open_stats(false)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        for k in [
            "relative",
            "by_path",
            "strict_by_id",
            "pinned",
            "pinned_cap_hit",
            "final_skipped",
            "final_verified",
            "final_wrong",
            "dot_matched",
            "dot_unmatched",
            "keep_off",
            "alloc_asked",
            "alloc_ask_opened",
            "alloc_ask_failed",
        ] {
            assert!(names.contains(&k), "{k}");
        }
    }

    #[test]
    fn handles_do_not_leak_into_found_paths() {
        // found paths are plain Win32 paths (no \\?\ or \??\ prefix), joined like os.path.join
        let _s = serial();
        let t = tree();
        let res = scan(
            vec![task(1, &t.root, 0)],
            &SliceOptions::unbounded(vec![root_opts(&t.root)]),
        );
        assert!(res.paths.iter().all(|p| p.starts_with(&t.root)));
    }
}

// ------------------------------------------------------------------------------ POSIX specifics

#[cfg(unix)]
mod posix {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn non_utf8_names_fifo_and_hardlinks() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("posix");
        fs::create_dir(&root).unwrap();
        for raw in [
            &b"bad\xff.TXT"[..],
            b"ext.\xc3\x89\xff",
            b"\xe4\xb8.\xe4\xb8\xad",
            b"latin1-\xe9.DAT",
        ] {
            match fs::write(root.join(std::ffi::OsStr::from_bytes(raw)), b"data") {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(libc::EILSEQ) => {
                    eprintln!("skipped: this file system rejects names that are not UTF-8");
                    return;
                }
                Err(e) => panic!("{e}"),
            }
        }
        fs::create_dir(root.join(std::ffi::OsStr::from_bytes(b"dir\xfe"))).unwrap();
        create(&root.join("orig.bin"), &[b'h'; 5000]);
        fs::hard_link(root.join("orig.bin"), root.join("link.bin")).unwrap();
        let fifo = std::ffi::CString::new(root.join("pipe").as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        let res = scan_one(&root, root_opts(&root));
        assert!(
            res.paths
                .iter()
                .any(|p| p.as_os_str().as_bytes().ends_with(b"dir\xfe"))
        );
        assert!(res.entries.kind.contains(&kind::OTHER));
        let names = name_strings(&res);
        let i = names.iter().position(|n| n == "link.bin").unwrap();
        assert_eq!(res.entries.nlink[i], 2);
    }

    #[test]
    fn folders_past_path_max_are_listed() {
        let _s = serial();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("long");
        fs::create_dir(&root).unwrap();
        let name = "d".repeat(200);
        let mut path = root.clone();
        let mut sizes = 0i64;
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: plain libc calls on valid NUL-terminated strings and fds we own.
        unsafe {
            let mut fd = libc::open(c(&root).as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
            assert!(fd >= 0);
            let n = std::ffi::CString::new(name.clone()).unwrap();
            let f = std::ffi::CString::new("f.txt").unwrap();
            while path.as_os_str().len() <= 5000 {
                assert_eq!(libc::mkdirat(fd, n.as_ptr(), 0o755), 0);
                let below = libc::openat(fd, n.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
                assert!(below >= 0);
                libc::close(fd);
                fd = below;
                path = path.join(&name);
                let out = libc::openat(fd, f.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o644);
                let len = path.as_os_str().len();
                let data = vec![b'x'; len];
                assert_eq!(libc::write(out, data.as_ptr().cast(), len) as usize, len);
                sizes += len as i64;
                libc::close(out);
            }
            libc::close(fd);
        }
        assert!(fs::read_dir(&path).is_err()); // ENAMETOOLONG for std
        let opts = SliceOptions::unbounded(vec![root_opts(&root)]);
        let res = scan(vec![task(1, &root, 0)], &opts);
        assert!(res.errors.is_empty(), "{:?}", res.errors);
        assert_eq!(res.paths.last(), Some(&path));
        assert_eq!(res.entries.size.iter().sum::<i64>(), sizes);
        let res = scan(vec![task(1, &path, 0)], &opts);
        assert!(res.errors.is_empty());
        assert_eq!(res.paths, std::slice::from_ref(&path));
        assert_eq!(name_strings(&res), ["f.txt"]);
    }

    #[test]
    fn denied_and_unsearchable_directories() {
        let _s = serial();
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads everything
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("perm");
        let denied = root.join("denied");
        let blind = root.join("blind"); // readable, not searchable: names but no stat
        for d in [&denied, &blind] {
            fs::create_dir_all(d).unwrap();
            create(&d.join("f.txt"), b"f");
        }
        fs::create_dir(blind.join("sub")).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o0)).unwrap();
        fs::set_permissions(&blind, fs::Permissions::from_mode(0o400)).unwrap();
        let res = scan(
            vec![task(1, &root, 0)],
            &SliceOptions::unbounded(vec![root_opts(&root)]),
        );
        for d in [&denied, &blind] {
            fs::set_permissions(d, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let errs: Vec<&PathBuf> = res.errors.iter().map(|e| &e.path).collect();
        assert_eq!(errs, [&denied]);
        assert_eq!(res.errors[0].message, "Permission denied");
        let k = res.paths.iter().position(|p| *p == blind).unwrap();
        assert!(
            res.listing(k)
                .all(|i| res.entries.flags[i] & flag::ERROR != 0)
        );
    }

    #[test]
    fn mount_boundaries() {
        let _s = serial();
        let t = tree();
        let real = dev_of(&t.root);
        let other = RootOptions {
            dev: real + 1, // every subdirectory looks like another device
            ..RootOptions::default()
        };
        let res = scan_one(&t.root, other.clone());
        assert_eq!(res.paths, std::slice::from_ref(&t.root));
        let mounts = (0..res.entries.len())
            .filter(|&i| res.entries.kind[i] == kind::DIR)
            .all(|i| res.entries.flags[i] & flag::MOUNT != 0);
        assert!(mounts);
        let crossing = RootOptions {
            cross_mounts: true,
            ..other
        };
        assert!(scan_one(&t.root, crossing).listings() > 1);
    }

    fn root_flags(res: &SliceResult) -> BTreeMap<Vec<u8>, u32> {
        res.entries
            .names()
            .zip(&res.entries.flags)
            .take(res.bounds[1])
            .map(|(n, &f)| (n.to_vec(), f))
            .collect()
    }

    fn subdirs(root: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_type().unwrap().is_dir())
            .map(|e| e.path())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn skipped_folders_are_not_entered() {
        let _s = serial();
        let t = tree();
        let subs = subdirs(&t.root);
        let (skipped, kept) = (&subs[0], &subs[1]);
        let ro = RootOptions {
            skip: vec![skipped.clone()],
            ..root_opts(&t.root)
        };
        let res = scan(
            vec![task(1, &t.root, 0)],
            &SliceOptions::unbounded(vec![ro]),
        );
        assert!(!res.paths.iter().any(|p| p.starts_with(skipped)));
        assert!(res.paths.iter().any(|p| p.starts_with(kept)));
        let flags = root_flags(&res);
        let name = |p: &Path| p.file_name().unwrap().as_bytes().to_vec();
        assert!(flags[&name(skipped)] & flag::MOUNT != 0);
        assert!(flags[&name(kept)] & flag::MOUNT == 0);
    }

    #[test]
    fn protected_folders_are_not_entered() {
        let _s = serial();
        let t = tree();
        let guarded = subdirs(&t.root)[0].clone();
        let ro = RootOptions {
            protect: vec![guarded.clone()],
            ..root_opts(&t.root)
        };
        let res = scan(
            vec![task(1, &t.root, 0)],
            &SliceOptions::unbounded(vec![ro]),
        );
        assert!(!res.paths.iter().any(|p| p.starts_with(&guarded)));
        let flags = root_flags(&res);
        let name = guarded.file_name().unwrap().as_bytes().to_vec();
        assert!(flags[&name] & flag::ACCESS_DENIED != 0);
    }
}
