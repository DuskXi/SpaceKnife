//! Delete-basket benchmark: builds a test tree in a folder of your own, scans it, puts it in a
//! basket and executes the basket the way the backend does (move to the trash or delete
//! permanently), then prints one JSON line: phases, the file-system calls counted by
//! `ops::probe`, results. Needs the `probe` feature.
//!
//! ```text
//! cargo run --release -p sk-core --features probe --example ops_bench -- <dir> <shape> <trash|delete> [label]
//! shapes: tree:<files>  one folder item, <files> small files in nested folders (+ 4 × 16 MB)
//!         flat:<files>  one folder item holding <files> small files itself
//!         group:<files> <files> small files of one folder as a small-item group
//!         items:<k>:<files> k folder items sharing <files> small files
//! ```
//!
//! Prints `MARK exec_start` / `MARK exec_end` around the execution (for a disk sampler). What a
//! trash run moved to the recycle bin is taken out of it again: exactly the `$I`/`$R` pairs whose
//! recorded original path lies below `<dir>` (Windows), or the trash entries it made (elsewhere).
//! `SPACEKNIFE_OPS_DEVICE=<µs>[,<qd>]` puts a simulated slow device in front of the counted calls.
//! `OPS_BENCH_EXPECT=ok` makes it fail (exit 1) unless every item was done, nothing is left of
//! them and what went to the trash was all taken out again; `OPS_BENCH_EXPECT=no_trash` unless
//! every item was refused for want of a trash and is still there (CI's file-system matrix).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sk_core::ops::probe;
use sk_core::ops::{Basket, OpOptions, OpResult};
use sk_core::scan::{ScanOptions, Scanner};

const SMALL: usize = 512;
const PER_FOLDER: usize = 100;

fn small(path: &Path) {
    std::fs::write(path, [b's'; SMALL]).unwrap();
}

/// `n` small files below `top` in folders of [`PER_FOLDER`], ten such folders per level.
fn fill(top: &Path, n: usize) {
    std::fs::create_dir_all(top).unwrap();
    let folders = n.div_ceil(PER_FOLDER);
    let mut made = 0;
    for f in 0..folders {
        let dir = top.join(format!("a{:03}", f / 10)).join(format!("b{f:04}"));
        std::fs::create_dir_all(&dir).unwrap();
        for k in 0..PER_FOLDER.min(n - made) {
            small(&dir.join(format!("f{k:03}.txt")));
            made += 1;
        }
    }
}

enum Shape {
    Tree(usize),
    Flat(usize),
    Group(usize),
    Items(usize, usize),
}

fn parse_shape(s: &str) -> Shape {
    let parts: Vec<&str> = s.split(':').collect();
    let n = |i: usize| parts.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
    match parts[0] {
        "tree" => Shape::Tree(n(1)),
        "flat" => Shape::Flat(n(1)),
        "group" => Shape::Group(n(1)),
        "items" => Shape::Items(n(1).max(1), n(2)),
        other => panic!("unknown shape {other}"),
    }
}

/// Makes the shape below `root`; returns (paths of the basket items, whether they form a group).
fn make(root: &Path, shape: &Shape) -> (Vec<PathBuf>, bool) {
    match *shape {
        Shape::Tree(n) => {
            let top = root.join("tree");
            fill(&top, n);
            for i in 0..4 {
                std::fs::write(top.join(format!("big{i}.bin")), vec![b'b'; 16 << 20]).unwrap();
            }
            (vec![top], false)
        }
        Shape::Flat(n) => {
            let top = root.join("flat");
            std::fs::create_dir_all(&top).unwrap();
            for i in 0..n {
                small(&top.join(format!("f{i:06}.txt")));
            }
            (vec![top], false)
        }
        Shape::Group(n) => {
            let top = root.join("grp");
            std::fs::create_dir_all(&top).unwrap();
            let files: Vec<PathBuf> = (0..n).map(|i| top.join(format!("g{i:06}.txt"))).collect();
            for f in &files {
                small(f);
            }
            (files, true)
        }
        Shape::Items(k, n) => {
            let items: Vec<PathBuf> = (0..k).map(|i| root.join(format!("item{i:03}"))).collect();
            for it in &items {
                fill(it, n / k);
            }
            (items, false)
        }
    }
}

/// Windows: takes the bin entries this run made out of the recycle bin again (only those whose
/// `$I` record names a path below `root`). Returns how many pairs went.
#[cfg(windows)]
fn unbin(results: &[OpResult], root: &Path) -> usize {
    use sk_core::ops::trash::{parse_recycle_info, path_of_ref};
    let key = |p: &str| p.to_lowercase().replace('/', "\\");
    let below = format!("{}\\", key(&root.to_string_lossy()).trim_end_matches('\\'));
    let mut gone = 0;
    for r in results.iter().filter(|r| r.ok && !r.trash_ref.is_empty()) {
        let info = path_of_ref(&r.trash_ref);
        let name = info.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("$I"), "{name}");
        let data = std::fs::read(&info).unwrap();
        let (_, _, original) = parse_recycle_info(&data).unwrap();
        // the record may hold the long form of an 8.3 name in the temp path
        let original_long = sk_core::ops::win::long_names(&original).unwrap_or(original.clone());
        let mine = key(&original).starts_with(&below)
            || key(&original_long).starts_with(&below)
            || key(&r.path).starts_with(&below);
        assert!(mine, "not ours: {original}");
        let payload = info.with_file_name(format!("$R{}", &name[2..]));
        let meta = std::fs::symlink_metadata(&payload).unwrap();
        if meta.is_dir() {
            std::fs::remove_dir_all(&payload).unwrap();
        } else {
            std::fs::remove_file(&payload).unwrap();
        }
        std::fs::remove_file(&info).unwrap();
        gone += 1;
    }
    gone
}

/// macOS: removes what this run put into the trash (the trashed item; its reference is a JSON
/// pair of the trashed and the original path).
#[cfg(target_os = "macos")]
fn unbin(results: &[OpResult], _root: &Path) -> usize {
    let mut gone = 0;
    for r in results.iter().filter(|r| r.ok && !r.trash_ref.is_empty()) {
        let v: serde_json::Value = serde_json::from_str(&r.trash_ref).unwrap();
        let trashed = PathBuf::from(v["trashed"].as_str().unwrap());
        assert!(
            trashed
                .components()
                .any(|c| c.as_os_str() == ".Trash" || c.as_os_str() == ".Trashes"),
            "not in a trash: {}",
            trashed.display()
        );
        let _ = std::fs::remove_dir_all(&trashed).or_else(|_| std::fs::remove_file(&trashed));
        gone += 1;
    }
    gone
}

/// Linux: removes what this run put into the trash (its info file and its payload).
#[cfg(all(unix, not(target_os = "macos")))]
fn unbin(results: &[OpResult], _root: &Path) -> usize {
    let mut gone = 0;
    for r in results.iter().filter(|r| r.ok && !r.trash_ref.is_empty()) {
        let info = sk_core::ops::trash::path_of_ref(&r.trash_ref);
        let name = info.file_name().unwrap().to_string_lossy().into_owned();
        let stem = name.trim_end_matches(".trashinfo");
        let payload = info
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("files")
            .join(stem);
        let _ = std::fs::remove_dir_all(&payload).or_else(|_| std::fs::remove_file(&payload));
        let _ = std::fs::remove_file(&info);
        gone += 1;
    }
    gone
}

/// `MARK <what>` on stdout; with `OPS_BENCH_HANDSHAKE=1` it then waits for a line on stdin (a
/// sampler outside reads the disk's counters exactly there).
fn mark(what: &str) {
    println!("MARK {what}");
    std::io::stdout().flush().unwrap();
    if std::env::var("OPS_BENCH_HANDSHAKE").is_ok_and(|v| v == "1") {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
}

/// This process's I/O so far: read, write and other operations, bytes read and written.
#[cfg(not(windows))]
fn io_counters() -> [u64; 5] {
    // /proc/self/io: syscr, syscw, (no "other"), rchar, wchar
    let text = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|v| v.trim().parse::<u64>().ok()))
            .unwrap_or(0)
    };
    [
        get("syscr:"),
        get("syscw:"),
        0,
        get("rchar:"),
        get("wchar:"),
    ]
}

/// This process's I/O so far: read, write and other operations, bytes read and written.
#[cfg(windows)]
fn io_counters() -> [u64; 5] {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessIoCounters, IO_COUNTERS,
    };
    let mut c: IO_COUNTERS = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer.
    unsafe { GetProcessIoCounters(GetCurrentProcess(), &mut c) };
    [
        c.ReadOperationCount,
        c.WriteOperationCount,
        c.OtherOperationCount,
        c.ReadTransferCount,
        c.WriteTransferCount,
    ]
}

/// Windows: the shell's recycle engine alone, `n` small files (`files` = 0) or `n` folders of
/// `files` small files each, `batch` items per operation; prints each operation's time.
#[cfg(windows)]
fn shell_only(base: &Path, n: usize, files: usize, batch: usize) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = base.join(format!("shell-{stamp}"));
    std::fs::create_dir_all(&root).unwrap();
    let items: Vec<PathBuf> = (0..n)
        .map(|i| {
            if files == 0 {
                let p = root.join(format!("s{i:05}.txt"));
                small(&p);
                p
            } else {
                let p = root.join(format!("d{i:05}"));
                fill(&p, files);
                p
            }
        })
        .collect();
    let mut all: Vec<OpResult> = Vec::new();
    let mut times = Vec::new();
    let io0 = io_counters();
    for chunk in items.chunks(batch.max(1)) {
        let t = Instant::now();
        let mut on_item = |_: usize, _: &sk_core::ops::win::Recycled| {};
        let run = sk_core::ops::win::recycle(chunk, None, &mut on_item).unwrap();
        times.push(t.elapsed().as_secs_f64());
        for (p, it) in chunk.iter().zip(&run.items) {
            let info = it.recycled.as_ref().map(|r| {
                let rp = sk_core::ops::trash::path_of_ref(r);
                let name = rp.file_name().unwrap().to_string_lossy().into_owned();
                sk_core::ops::trash::ref_of(&rp.with_file_name(format!("$I{}", &name[2..])))
            });
            all.push(OpResult::new(
                &p.to_string_lossy(),
                if info.is_some() {
                    sk_core::ops::OpCode::Ok
                } else {
                    sk_core::ops::OpCode::Error
                },
                "",
                &info.unwrap_or_default(),
            ));
        }
    }
    let io1 = io_counters();
    let gone = unbin(&all, &root);
    let _ = std::fs::remove_dir_all(&root);
    let total: f64 = times.iter().sum();
    println!(
        "  process I/O: {} reads ({} KiB), {} writes ({} KiB), {} other",
        io1[0] - io0[0],
        (io1[3] - io0[3]) / 1024,
        io1[1] - io0[1],
        (io1[4] - io0[4]) / 1024,
        io1[2] - io0[2]
    );
    println!(
        "SHELL n={n} files={files} batch={batch} total={total:.3}s per_item={:.2}ms ops={} unbinned={gone}",
        total * 1000.0 / n as f64,
        times.len()
    );
    let shown: Vec<String> = times.iter().map(|t| format!("{:.0}", t * 1000.0)).collect();
    println!("  op ms: {}", shown.join(" "));
}

/// The lock check alone: `k` folders sharing `n` small files; times a plain walk and
/// `find_lockers` (walk plus Restart Manager) over them.
fn locks_only(base: &Path, k: usize, n: usize) {
    let root = base.join(format!(
        "locks-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let items: Vec<PathBuf> = (0..k).map(|i| root.join(format!("item{i:03}"))).collect();
    for it in &items {
        fill(it, n / k);
    }
    let t = Instant::now();
    let mut seen = 0usize;
    let mut stack: Vec<PathBuf> = items.clone();
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                seen += 1;
            }
        }
    }
    let walk = t.elapsed().as_secs_f64();
    let asked: Vec<String> = items
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let t = Instant::now();
    let found = sk_core::ops::locks::find_lockers(&asked);
    let all = t.elapsed().as_secs_f64();
    // the Restart Manager alone over every file, in 1, 2, 4, 8 sessions at once
    #[cfg(windows)]
    let split: Vec<String> = {
        let mut files: Vec<String> = Vec::new();
        let mut stack: Vec<PathBuf> = items.clone();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    files.push(e.path().to_string_lossy().into_owned());
                }
            }
        }
        [1usize, 2, 4, 8]
            .iter()
            .map(|&n| {
                let t = Instant::now();
                std::thread::scope(|s| {
                    for chunk in files.chunks(files.len().div_ceil(n)) {
                        s.spawn(move || sk_core::ops::win::rm_lockers(chunk).unwrap());
                    }
                });
                format!("{n}:{:.3}s", t.elapsed().as_secs_f64())
            })
            .collect()
    };
    #[cfg(not(windows))]
    let split: Vec<String> = Vec::new();
    let _ = std::fs::remove_dir_all(&root);
    println!(
        "LOCKS k={k} files={seen} walk={walk:.3}s find_lockers={all:.3}s held={} rm sessions {}",
        found.len(),
        split.join(" ")
    );
}

/// How deletion scales with threads here: `k` folders sharing `n` small files removed with
/// `std::fs::remove_dir_all` by `threads` threads at once.
fn par_only(base: &Path, k: usize, n: usize, threads: usize) {
    let root = base.join(format!(
        "par-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let items: Vec<PathBuf> = (0..k).map(|i| root.join(format!("item{i:03}"))).collect();
    for it in &items {
        fill(it, n / k);
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let t = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(it) = items.get(i) else { break };
                    std::fs::remove_dir_all(it).unwrap();
                }
            });
        }
    });
    let secs = t.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&root);
    println!("PAR k={k} n={n} threads={threads} {secs:.3}s");
}

/// A folder of `n` small files renamed one by one into `to` (a folder of this run), `threads` at
/// once: what a plain rename costs here.
fn rename_only(base: &Path, n: usize, threads: usize) {
    let root = base.join(format!(
        "ren-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (from, to) = (root.join("from"), root.join("to"));
    std::fs::create_dir_all(&from).unwrap();
    std::fs::create_dir_all(&to).unwrap();
    let files: Vec<PathBuf> = (0..n).map(|i| from.join(format!("r{i:05}.txt"))).collect();
    for f in &files {
        small(f);
    }
    let t = Instant::now();
    sk_core::ops::par::for_each(n, threads, &|| false, |i| {
        let dst = to.join(format!("$RX{i:05}.txt"));
        sk_core::ops::trash::rename_noreplace(&files[i], &dst).unwrap();
    });
    let secs = t.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&root);
    println!(
        "RENAME n={n} threads={threads} {secs:.3}s = {:.3} ms each",
        secs * 1000.0 / n as f64
    );
}

/// One file and one folder moved to the bin and left there until a line comes on stdin (the
/// shell's own view of the bin can be checked meanwhile), then put back and removed.
fn bin_check(base: &Path) {
    let root = base.join(format!(
        "bincheck-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("folder.d")).unwrap();
    std::fs::write(root.join("folder.d").join("inside.txt"), b"inside").unwrap();
    std::fs::write(root.join("single.txt"), b"single").unwrap();
    let paths = [root.join("single.txt"), root.join("folder.d")];
    let res = sk_core::ops::move_to_trash(&paths, OpOptions::default());
    for r in &res {
        println!("BIN {} {} {}", r.code.as_str(), r.path, r.trash_ref);
    }
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let refs: Vec<String> = res.iter().map(|r| r.trash_ref.clone()).collect();
    for r in sk_core::ops::restore(&refs) {
        println!("BACK {} {}", r.code.as_str(), r.path);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// What renaming a folder of `n` small files costs here: beside itself, then into another folder.
fn dir_rename(base: &Path, n: usize) {
    let root = base.join(format!(
        "dirren-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fill(&root.join("a"), n);
    std::fs::create_dir_all(root.join("other")).unwrap();
    for (from, to) in [("a", "b"), ("b", "other/c")] {
        let io0 = io_counters();
        let t = Instant::now();
        std::fs::rename(root.join(from), root.join(to)).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let io1 = io_counters();
        println!(
            "DIRREN n={n} {from} -> {to}: {secs:.3}s, process reads {} other {}",
            io1[0] - io0[0],
            io1[2] - io0[2]
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && args[1].starts_with("dirren:") {
        dir_rename(Path::new(&args[0]), args[1][7..].parse().unwrap());
        return;
    }
    if args.len() >= 2 && args[1] == "bincheck" {
        bin_check(Path::new(&args[0]));
        return;
    }
    if let Some(n) = std::env::var("OPS_BENCH_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        sk_core::ops::par::seams::WORKERS.store(n, std::sync::atomic::Ordering::SeqCst);
    }
    if args.len() >= 2 && args[1].starts_with("ren:") {
        let v: Vec<usize> = args[1][4..]
            .split(':')
            .map(|x| x.parse().unwrap())
            .collect();
        rename_only(Path::new(&args[0]), v[0], v[1]);
        return;
    }
    if args.len() >= 2 && args[1].starts_with("par:") {
        let v: Vec<usize> = args[1][4..]
            .split(':')
            .map(|x| x.parse().unwrap())
            .collect();
        par_only(Path::new(&args[0]), v[0], v[1], v[2]);
        return;
    }
    if args.len() >= 2 && args[1].starts_with("locks:") {
        let v: Vec<usize> = args[1][6..]
            .split(':')
            .map(|x| x.parse().unwrap())
            .collect();
        locks_only(Path::new(&args[0]), v[0], v[1]);
        return;
    }
    #[cfg(windows)]
    if args.len() >= 2 && args[1].starts_with("shell:") {
        let v: Vec<usize> = args[1][6..]
            .split(':')
            .map(|x| x.parse().unwrap())
            .collect();
        shell_only(Path::new(&args[0]), v[0], v[1], v[2]);
        return;
    }
    if args.len() < 3 {
        eprintln!("usage: ops_bench <dir> <shape> <trash|delete> [label]");
        std::process::exit(2);
    }
    let base = PathBuf::from(&args[0]);
    let shape_s = args[1].clone();
    let shape = parse_shape(&shape_s);
    let trash = match args[2].as_str() {
        "trash" => true,
        "delete" => false,
        m => panic!("mode {m}"),
    };
    let label = args.get(3).cloned().unwrap_or_default();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = base.join(format!("run-{stamp}"));
    std::fs::create_dir_all(&root).unwrap();

    let t = Instant::now();
    let (items, group) = make(&root, &shape);
    let made_s = t.elapsed().as_secs_f64();

    // scan it, as the backend would have
    let root_s = root.to_string_lossy().into_owned();
    let scanner = Scanner::new(&[root_s.as_str()], ScanOptions::default()).unwrap();
    scanner.run(None, Duration::from_millis(250));
    let store = scanner.store().clone();
    let basket = Basket::new(None);
    {
        let nodes = store.read();
        let ids: Vec<i64> = items
            .iter()
            .map(|p| i64::from(nodes.find_path(p).expect("scanned")))
            .collect();
        let r = if group {
            let folder = nodes.find_path(items[0].parent().unwrap()).unwrap();
            basket
                .add(&nodes, "s1", &[i64::from(folder)], Some(&ids))
                .unwrap()
        } else {
            basket.add(&nodes, "s1", &ids, None).unwrap()
        };
        assert!(r.refused.is_empty(), "{:?}", r.refused);
    }
    let targets = basket.targets(None);
    let paths: Vec<PathBuf> = targets.iter().map(|t| t.os_path.clone()).collect();
    let sizes: HashMap<String, i64> = targets.iter().map(|t| (t.path.clone(), t.size)).collect();
    let longest: HashMap<String, usize> = targets
        .iter()
        .filter_map(|t| t.longest.map(|l| (t.path.clone(), l)))
        .collect();
    let roots: HashMap<String, String> = targets
        .iter()
        .map(|t| (t.path.clone(), t.root.clone()))
        .collect();
    let bytes: HashMap<String, i64> = targets
        .iter()
        .filter_map(|t| t.bytes.map(|b| (t.path.clone(), b)))
        .collect();

    probe::reset();
    let mut done = 0usize;
    let mut progress = |n: usize, _: usize, _: &OpResult| done = n;
    let never = || false;
    let opts = OpOptions {
        progress: Some(&mut progress),
        cancel: Some(&never),
        sizes: trash.then_some(&sizes),
        longest: trash.then_some(&longest),
        bytes: trash.then_some(&bytes),
        roots: Some(&roots),
        ..OpOptions::default()
    };
    mark("exec_start");
    let io0 = io_counters();
    let t = Instant::now();
    let results = if trash {
        sk_core::ops::move_to_trash(&paths, opts)
    } else {
        sk_core::ops::delete_permanently(&paths, opts)
    };
    let exec_s = t.elapsed().as_secs_f64();
    let io1 = io_counters();
    mark("exec_end");
    let (counts, phases) = probe::snapshot();

    // what the drawer asks right after: the trash of that volume and what it holds
    let t = Instant::now();
    let status = sk_core::ops::trash_status(&root_s, true);
    let status_s = t.elapsed().as_secs_f64();

    let ok = results.iter().filter(|r| r.ok).count();
    let mut codes: HashMap<&str, usize> = HashMap::new();
    for r in &results {
        *codes.entry(r.code.as_str()).or_default() += 1;
    }
    let first_error = results
        .iter()
        .find(|r| !r.ok)
        .map(|r| format!("{}: {}", r.path, r.message));
    let unbinned = if trash { unbin(&results, &root) } else { 0 };
    let left_over = std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&root);

    let line = serde_json::json!({
        "label": label,
        "shape": shape_s,
        "mode": if trash { "trash" } else { "delete" },
        "device": probe::device().map(|(l, q)| format!("{}us,qd{q}", l.as_micros())),
        "targets": targets.len(),
        "made_s": made_s,
        "exec_s": exec_s,
        "status_s": status_s,
        "status_used": status.used,
        "progress_done": done,
        "ok": ok,
        "codes": codes,
        "first_error": first_error,
        "unbinned": unbinned,
        "left_over_entries": left_over,
        "calls": counts.iter().map(|(c, n)| (c.name(), *n)).collect::<HashMap<_, _>>(),
        "phases": phases.iter().map(|(k, v)| (*k, *v)).collect::<HashMap<_, _>>(),
        "proc_io": {
            "reads": io1[0] - io0[0],
            "writes": io1[1] - io0[1],
            "other": io1[2] - io0[2],
            "read_kib": (io1[3] - io0[3]) / 1024,
            "write_kib": (io1[4] - io0[4]) / 1024,
        },
    });
    println!("RESULT {line}");
    if let Ok(want) = std::env::var("OPS_BENCH_EXPECT") {
        let n = targets.len();
        let wrong = match want.as_str() {
            "ok" => ok != n || left_over != 0 || (trash && unbinned != ok),
            "no_trash" => codes.get("no_trash").copied().unwrap_or(0) != n || left_over != n,
            other => panic!("OPS_BENCH_EXPECT={other}"),
        };
        if wrong {
            eprintln!("ops_bench: expected {want} for all {n} items: {line}");
            std::process::exit(1);
        }
    }
}
