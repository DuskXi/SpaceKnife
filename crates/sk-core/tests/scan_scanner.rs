//! The hard scan (port of backend/tests/test_scanner.py and the scanner parts of
//! test_robust_scan_manager / test_m4_scan / test_robust_surrogates).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sk_core::scan::{
    IoGate, Permit, ScanError, ScanOptions, ScanProgress, ScanState, Scanner, native_threads,
};
use sk_store::testing::{TreeFacts, build_disk_tree, check_invariants};
use sk_store::{Flags, Kind, NodeId, Nodes};

fn tree() -> (tempfile::TempDir, PathBuf, TreeFacts) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let facts = build_disk_tree(&root).unwrap();
    (tmp, root, facts)
}

fn opts(budget_ms: u64) -> ScanOptions {
    ScanOptions { budget: Duration::from_millis(budget_ms), ..Default::default() }
}

fn scanner(root: &Path, o: ScanOptions) -> Scanner {
    Scanner::new(&[root.to_string_lossy()], o).unwrap()
}

fn run(s: &Scanner) -> ScanProgress {
    s.run(None, Duration::from_millis(250))
}

fn check_totals(s: &Scanner, facts: &TreeFacts) {
    let root = s.roots()[0].node;
    let r = s.store().read();
    assert_eq!(r.tot_files(root) as usize, facts.files);
    assert_eq!(r.tot_dirs(root) as usize, facts.dirs);
    assert_eq!(r.tot_size(root) as u64, facts.size);
    check_invariants(&r).unwrap();
}

fn find(r: &Nodes, p: &Path) -> NodeId {
    r.find_path(p).unwrap_or_else(|| panic!("{} not in the tree", p.display()))
}

#[test]
fn scan_matches_tree() {
    let (_t, root, facts) = tree();
    let s = scanner(&root, ScanOptions::default());
    let p = run(&s);
    assert_eq!(p.state, ScanState::Done);
    assert_eq!(p.errors, 0);
    check_totals(&s, &facts);
    let r = s.store().read();
    let movie = find(&r, &root.join("a").join("sub").join("movie.mp4"));
    let v = r.view(movie);
    assert_eq!((v.size, v.ext.as_str(), v.kind), (123456, "mp4", Kind::File));
    let photo = find(&r, &root.join("中文 文件夹").join("照片.jpg"));
    assert_eq!(r.name(photo), "照片.jpg");
    assert_eq!(p.items as usize, facts.files + facts.dirs + 1);
    assert!(p.rate > 0.0);
    assert_eq!(p.progress, None); // neither a whole volume nor an expected count
}

#[test]
fn zero_budget_and_one_thread_give_identical_result() {
    let (_t, root, facts) = tree();
    for (budget, threads) in [(0, 1), (0, 4), (1, 2)] {
        let s = scanner(&root, ScanOptions { concurrency: threads, ..opts(budget) });
        let p = run(&s);
        assert_eq!(p.state, ScanState::Done);
        check_totals(&s, &facts);
    }
}

#[test]
fn cancel_leaves_consistent_partial_tree() {
    let (_t, root, facts) = tree();
    let s = scanner(&root, ScanOptions { concurrency: 1, ..opts(0) });
    let mut cb = |_: &ScanProgress| s.cancel();
    let p = s.run(Some(&mut cb), Duration::ZERO);
    assert_eq!(p.state, ScanState::Cancelled);
    assert!((p.files as usize) < facts.files);
    let r = s.store().read();
    assert!((0..r.len() as NodeId).any(|i| r.flags(i).intersects(Flags::PENDING)));
    check_invariants(&r).unwrap();
    drop(r);
    assert!(s.has_queue());
    // a cancel that only switched methods: the scan goes on from its queue
    s.uncancel();
    let p = run(&s);
    assert_eq!(p.state, ScanState::Done);
    check_totals(&s, &facts);
}

#[test]
fn progress_callback_and_final_report() {
    let (_t, root, facts) = tree();
    let s = scanner(&root, opts(0));
    let mut reports = Vec::new();
    let mut cb = |p: &ScanProgress| reports.push(p.clone());
    let p = s.run(Some(&mut cb), Duration::ZERO);
    assert!(!reports.is_empty());
    assert_eq!(reports.last().unwrap().state, ScanState::Done);
    assert_eq!(p.items as usize, facts.files + facts.dirs + 1);
    assert!(p.started_at.is_some() && p.finished_at.is_some());
    let mut status = sk_protocol::scan::ScanStatus {
        scan_id: "x".into(),
        state: ScanState::Queued,
        roots: s.roots().iter().map(|r| r.to_protocol()).collect(),
        items: 0, files: 0, dirs: 0, bytes_alloc: 0, bytes_size: 0, errors: 0, pending_dirs: 0,
        current_path: String::new(), elapsed_s: 0.0, rate: 0.0, byte_rate: 0.0, progress: None,
        remaining_s: None, expected_items: None, started_at: None, finished_at: None,
        denied: vec![], message: String::new(), free: None, unknown: None, snapshot: None,
        source: Default::default(), method: Default::default(), phase: String::new(),
        fallback: String::new(), mft_available: false, paused: false, io: None, monitor: None,
    };
    p.apply_to(&mut status);
    assert_eq!(status.items, p.items);
    assert_eq!(status.state, ScanState::Done);
}

#[test]
fn bad_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    let e = Scanner::new(&[missing.to_string_lossy()], ScanOptions::default()).unwrap_err();
    assert!(matches!(e, ScanError::Access { .. }), "{e}");
    assert!(e.to_string().contains("cannot access"));
    std::fs::write(tmp.path().join("f.txt"), "x").unwrap();
    let e = Scanner::new(&[tmp.path().join("f.txt").to_string_lossy()], ScanOptions::default()).unwrap_err();
    assert!(e.to_string().contains("not a directory"));
    let none: [&str; 0] = [];
    assert!(matches!(Scanner::new(&none, ScanOptions::default()), Err(ScanError::NoRoots)));
}

#[test]
fn nested_roots_are_deduplicated() {
    let (_t, root, _) = tree();
    let s = Scanner::new(&[root.to_string_lossy(), root.join("a").to_string_lossy()], ScanOptions::default()).unwrap();
    let roots = s.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, root);
}

#[test]
fn multiple_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("one"), tmp.path().join("two"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let fa = build_disk_tree(&a).unwrap();
    let fb = build_disk_tree(&b).unwrap();
    let s = Scanner::new(&[a.to_string_lossy(), b.to_string_lossy()], ScanOptions::default()).unwrap();
    run(&s);
    assert_eq!(s.roots().len(), 2);
    assert_eq!(s.store().read().tot_size(0) as u64, fa.size + fb.size);
}

#[cfg(windows)]
#[test]
fn windows_allocation_rounding() {
    let tmp = tempfile::tempdir().unwrap();
    let vol = sk_core::volume::volume_for(tmp.path()).unwrap();
    if !vol.fs_type.eq_ignore_ascii_case("NTFS") {
        eprintln!("skipped: temp dir is not on NTFS");
        return;
    }
    std::fs::write(tmp.path().join("small.txt"), vec![b'x'; 100]).unwrap();
    std::fs::write(tmp.path().join("big.bin"), vec![b'x'; 5000]).unwrap();
    let s = scanner(tmp.path(), ScanOptions::default());
    run(&s);
    let cluster = s.roots()[0].volume.as_ref().unwrap().cluster_size;
    let r = s.store().read();
    assert_eq!(r.alloc(find(&r, &tmp.path().join("small.txt"))), 0);
    assert_eq!(r.alloc(find(&r, &tmp.path().join("big.bin"))), (5000 + cluster - 1) / cluster * cluster);
}

#[cfg(unix)]
#[test]
fn posix_links_hardlinks_and_denied() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir_all(root.join("dir")).unwrap();
    std::fs::write(root.join("dir/data"), vec![b'x'; 10_000]).unwrap();
    std::fs::hard_link(root.join("dir/data"), root.join("hard")).unwrap();
    std::os::unix::fs::symlink(root.join("dir"), root.join("link-to-dir")).unwrap();
    let locked = root.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("secret"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let s = scanner(&root, ScanOptions::default());
    let p = run(&s);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let r = s.store().read();
    let link = find(&r, &root.join("link-to-dir"));
    assert_eq!(r.kind(link), Kind::Link);
    assert_eq!(r.child_count(link), 0);
    let data = r.flags(find(&r, &root.join("dir/data"))).intersects(Flags::HARDLINK_DUP);
    let hard = r.flags(find(&r, &root.join("hard"))).intersects(Flags::HARDLINK_DUP);
    assert!(data != hard);
    assert_eq!(r.tot_size(s.roots()[0].node), 10_000 + r.size(link));
    if unsafe { libc::geteuid() } != 0 {
        assert!(r.flags(find(&r, &locked)).intersects(Flags::ACCESS_DENIED));
        assert_eq!(p.errors, 1);
        assert_eq!(s.denied_reasons().len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn hard_links_count_once_in_many_slices_or_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("links");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b/c")).unwrap();
    for i in 0..30 {
        let first = root.join(format!("a/f{i}"));
        std::fs::write(&first, vec![b'x'; 100 + i]).unwrap();
        for other in [format!("b/g{i}"), format!("b/c/h{i}"), format!("a/f{i}-again")] {
            std::fs::hard_link(&first, root.join(other)).unwrap();
        }
    }
    std::fs::write(root.join("single"), b"yyyyyyy").unwrap();
    for budget in [0, 1000] {
        let s = scanner(&root, opts(budget));
        run(&s);
        let r = s.store().read();
        let counted = (1..r.len() as NodeId)
            .filter(|&i| r.kind(i) == Kind::File && !r.flags(i).intersects(Flags::HARDLINK_DUP))
            .count();
        assert_eq!(counted, 31);
        assert_eq!(r.tot_size(s.roots()[0].node), (0..30).map(|i| 100 + i).sum::<i64>() + 7);
    }
}

#[test]
fn progress_by_items_else_bytes_and_unknown_past_the_ceiling() {
    // Used space is no exact total (APFS clones can pass it): past 95 % of it the progress is
    // unknown instead of stuck at 99 %; the last scan's item count wins when there is one.
    let (_t, root, _) = tree();
    let s = scanner(&root, ScanOptions::default());
    let p = run(&s);
    assert_eq!(p.progress, None); // neither a whole volume nor an expected count
    let alloc = s.store().read().tot_alloc(0);
    assert!(alloc > 0);
    let vol = sk_core::volume::volume_for(&root).unwrap();
    s.begin_external(); // scanning again (as far as the progress goes)
    let used = |u: i64| {
        let mut v = vol.clone();
        v.total = u * 4;
        v.free = u * 4 - u;
        v.reserved = 0;
        s.update_root(0, |r| {
            r.volume = Some(v);
            r.whole_volume = true;
        });
    };
    used(alloc * 2);
    assert!((s.progress().progress.unwrap() - 0.5).abs() < 1e-9);
    used(alloc * 100 / 98);
    assert_eq!(s.progress().progress, None); // past the ceiling
    used(alloc / 2);
    assert_eq!(s.progress().progress, None); // more than the volume says it uses
    s.set_expected_items(Some(p.items * 2)); // by items, whatever the bytes say
    assert!((s.progress().progress.unwrap() - 0.5).abs() < 1e-9);
    s.finish_external(ScanState::Done);
    assert_eq!(s.progress().progress, Some(1.0));
    let summary = s.space_summary(&s.roots()[0]).unwrap();
    assert_eq!(summary.1, 0); // unknown: used minus scanned, never negative
}

#[test]
fn profile_accounts_for_the_scan() {
    let (_t, root, facts) = tree();
    let s = scanner(&root, opts(0));
    assert_eq!(s.profile().slices, 0);
    let p = run(&s);
    let prof = s.profile();
    assert_eq!(prof.items as i64, p.items - 1);
    assert!(prof.slices as usize >= 1 && prof.slices as usize <= facts.dirs + 1);
    assert!(prof.wall_s > 0.0 && prof.ingest_s > 0.0 && prof.ingest_s < prof.wall_s);
    assert_eq!(prof.limited_s, 0.0);
    assert_eq!(prof.threads, s.concurrency());
    assert!(prof.text().contains("listing threads busy"));
}

#[test]
fn profile_leaves_out_the_time_paused() {
    let (_t, root, _) = tree();
    let s = Arc::new(scanner(&root, ScanOptions::default()));
    s.pause();
    let s2 = s.clone();
    let h = std::thread::spawn(move || run(&s2));
    std::thread::sleep(Duration::from_millis(300));
    assert!(s.paused());
    assert_eq!(s.progress().items, 1); // the root only: nothing dispatched while paused
    s.resume();
    let p = h.join().unwrap();
    assert_eq!(p.state, ScanState::Done);
    assert!(s.profile().wall_s < 0.25 && 0.25 < s.progress().elapsed_s);
}

#[test]
fn readers_run_while_the_scan_appends() {
    let (_t, root, facts) = tree();
    let s = Arc::new(scanner(&root, ScanOptions { concurrency: 2, ..opts(0) }));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (s2, d2) = (s.clone(), done.clone());
    let reader = std::thread::spawn(move || {
        let mut reads = 0;
        while !d2.load(Ordering::SeqCst) {
            let r = s2.store().read();
            let n = r.len();
            // every published row has a live parent below it
            for i in 1..n as NodeId {
                assert!(r.parent(i) >= 0 && (r.parent(i) as NodeId) < i);
            }
            reads += 1;
        }
        reads
    });
    run(&s);
    done.store(true, Ordering::SeqCst);
    assert!(reader.join().unwrap() > 0);
    check_totals(&s, &facts);
}

#[test]
fn scan_threads_can_be_set_for_measurements() {
    // one test touches the variable (tests run in parallel)
    unsafe { std::env::set_var("SPACEKNIFE_SCAN_THREADS", "24") };
    assert_eq!(native_threads(), 24);
    unsafe { std::env::set_var("SPACEKNIFE_SCAN_THREADS", "500") };
    assert_eq!(native_threads(), 64);
    for bad in ["0", "-3", "many", ""] {
        unsafe { std::env::set_var("SPACEKNIFE_SCAN_THREADS", bad) };
        assert!((2..=16).contains(&native_threads()));
    }
    unsafe { std::env::remove_var("SPACEKNIFE_SCAN_THREADS") };
}

/// A gate that allows `n` batches in flight and counts what it saw.
struct CountingGate {
    limit: usize,
    inflight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    permits: Arc<AtomicUsize>,
    paused: Mutex<Vec<bool>>,
}

impl IoGate for CountingGate {
    fn permit(&self, timeout: Duration) -> Option<Permit> {
        let end = Instant::now() + timeout;
        loop {
            let cur = self.inflight.load(Ordering::SeqCst);
            if cur < self.limit
                && self.inflight.compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
            {
                self.peak.fetch_max(cur + 1, Ordering::SeqCst);
                self.permits.fetch_add(1, Ordering::SeqCst);
                let inflight = self.inflight.clone();
                return Some(Permit::new(move || {
                    inflight.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn concurrency(&self) -> usize {
        self.limit
    }
    fn pause(&self) {
        self.paused.lock().unwrap().push(true);
    }
    fn resume(&self) {
        self.paused.lock().unwrap().push(false);
    }
}

#[test]
fn hard_scans_run_under_the_disk_governor() {
    let (_t, root, facts) = tree();
    let gate = Arc::new(CountingGate {
        limit: 2,
        inflight: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        permits: Arc::new(AtomicUsize::new(0)),
        paused: Mutex::new(Vec::new()),
    });
    let s = Arc::new(scanner(&root, ScanOptions { concurrency: 8, gate: Some(gate.clone()), ..opts(0) }));
    s.pause();
    let s2 = s.clone();
    let h = std::thread::spawn(move || run(&s2));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(gate.permits.load(Ordering::SeqCst), 0); // paused: nothing dispatched
    s.resume();
    let p = h.join().unwrap();
    assert_eq!(p.state, ScanState::Done);
    check_totals(&s, &facts);
    assert!(gate.permits.load(Ordering::SeqCst) >= 1);
    assert!(gate.peak.load(Ordering::SeqCst) <= 2);
    assert_eq!(gate.inflight.load(Ordering::SeqCst), 0); // every permit given back
    assert_eq!(*gate.paused.lock().unwrap(), vec![true, false]);
}

#[test]
fn a_failing_progress_callback_is_the_callers_business_but_scans_go_on() {
    // Rust callbacks cannot throw; a panicking one would unwind the caller. The scanner itself
    // never calls user code on its listing threads: check the scan finishes with a slow callback.
    let (_t, root, facts) = tree();
    let s = scanner(&root, opts(0));
    let mut calls = 0;
    let mut cb = |_: &ScanProgress| {
        calls += 1;
        std::thread::sleep(Duration::from_millis(1));
    };
    let p = s.run(Some(&mut cb), Duration::ZERO);
    assert_eq!(p.state, ScanState::Done);
    assert!(calls > 0);
    check_totals(&s, &facts);
}

#[cfg(windows)]
#[test]
fn undecodable_names_are_scanned() {
    // test_robust_surrogates: a lone surrogate in a name survives the scan as WTF-8
    use std::os::windows::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let name = std::ffi::OsString::from_wide(&[0x61, 0xD800, 0x62]);
    let p = tmp.path().join(&name);
    if std::fs::write(&p, b"xyz").is_err() {
        eprintln!("skipped: the file system refuses lone surrogates");
        return;
    }
    let s = scanner(tmp.path(), ScanOptions::default());
    run(&s);
    let r = s.store().read();
    let node = find(&r, &p);
    assert_eq!(r.size(node), 3);
    assert_eq!(sk_store::names::name_to_os(r.name_bytes(node)), name);
}

#[test]
fn hard_scans_run_under_the_io_governor() {
    // the system task's governor through the scanner's gate (pause, resume, permits)
    let (_t, root, facts) = tree();
    let gov = sk_core::io::IoGovernor::new(sk_core::io::disk_ref_for_path(&root), 4);
    let gate: Arc<dyn IoGate> = Arc::new(gov.clone());
    let s = Arc::new(scanner(&root, ScanOptions { gate: Some(gate), ..opts(0) }));
    s.pause();
    assert!(gov.paused());
    let s2 = s.clone();
    let h = std::thread::spawn(move || run(&s2));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(s.progress().items, 1);
    s.resume();
    assert!(!gov.paused());
    let p = h.join().unwrap();
    assert_eq!(p.state, ScanState::Done);
    check_totals(&s, &facts);
}

/// Readings a moment apart (the times below are compared with measured ones).
const TICK: f64 = 0.01;

/// Runs `s` held paused for `secs` (nothing listed), then stops it with every folder still to
/// read; the wall time it took.
fn stopped_after(s: &Arc<Scanner>, secs: f64) -> f64 {
    let t0 = Instant::now();
    s.pause();
    let s2 = s.clone();
    let h = std::thread::spawn(move || run(&s2));
    std::thread::sleep(Duration::from_secs_f64(secs));
    s.cancel();
    let p = h.join().unwrap();
    assert_eq!(p.state, ScanState::Cancelled);
    assert!(s.has_queue());
    t0.elapsed().as_secs_f64()
}

#[test]
fn a_continued_scan_takes_the_time_of_both_parts() {
    // stopped, then continued: both parts count, the wait between them does not (it used to be
    // the second part alone, and below zero while that part ran)
    struct SlowOnce(std::sync::atomic::AtomicBool);
    impl IoGate for SlowOnce {
        fn permit(&self, _timeout: Duration) -> Option<Permit> {
            if self.0.swap(false, Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(500));
            }
            Some(Permit::free())
        }
        fn concurrency(&self) -> usize {
            4
        }
    }
    let (_t, root, facts) = tree();
    let gate = Arc::new(SlowOnce(std::sync::atomic::AtomicBool::new(false)));
    let s = Arc::new(scanner(&root, ScanOptions { gate: Some(gate.clone()), ..Default::default() }));
    let wall = stopped_after(&s, 0.3);
    let first = s.progress();
    assert!(0.25 < first.elapsed_s && first.elapsed_s <= wall + TICK, "{}", first.elapsed_s);
    std::thread::sleep(Duration::from_millis(600)); // stopped: does not count
    s.uncancel();
    gate.0.store(true, Ordering::SeqCst); // the continued part lists for a while
    let t1 = Instant::now();
    let s2 = s.clone();
    let h = std::thread::spawn(move || run(&s2));
    std::thread::sleep(Duration::from_millis(150));
    let meanwhile = s.progress();
    let so_far = t1.elapsed().as_secs_f64();
    assert_eq!(meanwhile.state, ScanState::Scanning);
    assert!(
        first.elapsed_s + 0.1 < meanwhile.elapsed_s
            && meanwhile.elapsed_s <= first.elapsed_s + so_far + TICK,
        "{} {}",
        first.elapsed_s,
        meanwhile.elapsed_s
    );
    assert!(meanwhile.finished_at.is_none() && meanwhile.started_at == first.started_at);
    let done = h.join().unwrap();
    let second = t1.elapsed().as_secs_f64();
    assert_eq!(done.state, ScanState::Done);
    assert_eq!(done.items as usize, facts.files + facts.dirs + 1);
    assert!(
        first.elapsed_s + 0.45 < done.elapsed_s && done.elapsed_s <= first.elapsed_s + second + TICK,
        "{} {}",
        first.elapsed_s,
        done.elapsed_s
    );
    assert!(done.started_at == first.started_at && done.finished_at.is_some());
    assert!((done.rate - done.items as f64 / done.elapsed_s).abs() < 1e-6 * done.rate.max(1.0));
}

#[test]
fn switching_to_the_mft_read_times_only_that_read() {
    let (_t, root, _) = tree();
    let s = Arc::new(scanner(&root, ScanOptions::default()));
    stopped_after(&s, 0.3);
    let started_at = s.progress().started_at;
    s.uncancel();
    let t0 = Instant::now();
    s.begin_external();
    std::thread::sleep(Duration::from_millis(100));
    let reading = s.progress();
    assert!(0.05 < reading.elapsed_s && reading.elapsed_s <= t0.elapsed().as_secs_f64() + TICK);
    assert!(reading.finished_at.is_none() && reading.started_at == started_at);
    s.finish_external(ScanState::Done);
    let e = s.progress().elapsed_s;
    assert!(0.05 < e && e <= t0.elapsed().as_secs_f64() + TICK);
}

#[test]
fn a_failed_mft_read_goes_on_with_the_hard_parts_time() {
    let (_t, root, _) = tree();
    let s = Arc::new(scanner(&root, ScanOptions::default()));
    stopped_after(&s, 0.3);
    let hard = s.progress().elapsed_s;
    s.uncancel();
    s.begin_external();
    std::thread::sleep(Duration::from_millis(300));
    s.reset_external();
    assert!((s.progress().elapsed_s - hard).abs() < 1e-9); // the attempt does not count
    let t1 = Instant::now();
    let done = run(&s);
    assert_eq!(done.state, ScanState::Done);
    assert!(hard < done.elapsed_s && done.elapsed_s <= hard + t1.elapsed().as_secs_f64() + TICK);
}
