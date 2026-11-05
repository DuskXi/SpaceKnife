//! The change pipeline against real folders (port of backend/tests/test_monitor_pipeline.py,
//! without the server around it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sk_core::monitor::pipeline::{MonitorHooks, MonitorOptions, ScanMonitor, UsnRead, UsnSource};
use sk_core::monitor::{Change, ChangeKind};
use sk_core::scan::{ScanOptions, Scanner};
use sk_store::testing::{build_disk_tree, check_invariants, facts};
use sk_store::{NodeId, Store};

fn scanned(roots: &[&Path]) -> Scanner {
    let roots: Vec<String> = roots.iter().map(|r| r.to_string_lossy().into_owned()).collect();
    let s = Scanner::new(&roots, ScanOptions::default()).unwrap();
    s.run(None, Duration::from_millis(250));
    s
}

fn monitor(s: &Scanner, watch: bool, usn: Option<Arc<dyn UsnSource>>) -> ScanMonitor {
    let mut m = ScanMonitor::new(
        s.store().clone(),
        s.roots(),
        s.root_options(),
        MonitorOptions { watch, coalesce: Duration::from_millis(50), poll_interval: 0.2, usn, ..Default::default() },
    );
    m.start();
    m
}

/// The tree equals a fresh scan of the same roots.
fn same_as_disk(store: &Store, roots: &[&Path]) -> bool {
    let fresh = scanned(roots);
    let a = facts(&store.read());
    let b = facts(&fresh.store().read());
    a == b
}

#[derive(Default)]
struct Recorder {
    flashes: Arc<Mutex<Vec<HashMap<NodeId, i64>>>>,
    messages: Arc<Mutex<Vec<String>>>,
}

impl MonitorHooks for Recorder {
    fn applied(&mut self, f: &HashMap<NodeId, i64>) {
        self.flashes.lock().unwrap().push(f.clone());
    }
    fn message(&mut self, text: String) {
        self.messages.lock().unwrap().push(text);
    }
}

/// Steps the monitor (on this thread) until `cond` holds.
fn until(mon: &mut ScanMonitor, hooks: &mut dyn MonitorHooks, secs: u64, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        mon.step(hooks);
        if cond() {
            return;
        }
        assert!(Instant::now() < end, "the tree did not catch up: {:?}", mon.status());
    }
}

fn tree(tmp: &Path) -> PathBuf {
    let root = tmp.join("root");
    std::fs::create_dir_all(&root).unwrap();
    build_disk_tree(&root).unwrap();
    root
}

fn deepest_dir(root: &Path) -> PathBuf {
    let mut best = root.to_path_buf();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if d.components().count() > best.components().count() {
            best = d.clone();
        }
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
        }
    }
    best
}

#[test]
fn changes_reach_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let s = scanned(&[&root]);
    let mut mon = monitor(&s, true, None);
    let mut hooks = Recorder::default();
    mon.step(&mut hooks); // feed the folders to per-folder watchers
    std::thread::sleep(Duration::from_millis(300)); // polling baselines (where polling is used)
    let deep = deepest_dir(&root);
    std::fs::write(deep.join("new.bin"), vec![b'n'; 9000]).unwrap();
    std::fs::write(root.join("a/one.bin"), vec![b'g'; 101_000]).unwrap(); // bigger
    std::fs::remove_file(root.join("c.txt")).unwrap();
    std::fs::create_dir_all(root.join("fresh/inner")).unwrap();
    std::fs::write(root.join("fresh/inner/x.dat"), vec![b'x'; 4321]).unwrap();
    std::fs::rename(root.join("b"), root.join("renamed")).unwrap();
    let store = s.store().clone();
    let r2 = root.clone();
    until(&mut mon, &mut hooks, 20, move || same_as_disk(&store, &[&r2]));
    check_invariants(&s.store().read()).unwrap();
    let st = mon.status();
    assert!(st.changes > 0 && st.events > 0, "{st:?}");
    assert!(sk_store::mutate::gone_count(&s.store().read()) > 0);
    assert!(!hooks.flashes.lock().unwrap().is_empty());
    mon.stop();
}

#[test]
fn storm_rescans_quietly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let s = scanned(&[&root]);
    let mut mon = monitor(&s, false, None);
    let mut hooks = Recorder::default();
    for i in 0..6 {
        let d = root.join(format!("storm{i}"));
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("f.txt"), vec![b's'; 100 + i]).unwrap();
    }
    std::fs::remove_dir_all(root.join("deep")).unwrap();
    // more events than a batch takes one by one: the common folder is rescanned once quiet
    let storm: Vec<Change> = (0..20_001).map(|i| Change::new(ChangeKind::Created, root.join(format!("storm{}/f.txt", i % 6)), Some(false))).collect();
    mon.handle().push(storm);
    let store = s.store().clone();
    let r2 = root.clone();
    until(&mut mon, &mut hooks, 20, move || same_as_disk(&store, &[&r2]));
    assert_eq!(hooks.messages.lock().unwrap().len(), 1);
    check_invariants(&s.store().read()).unwrap();
}

#[test]
fn rescan_a_folder_without_monitoring_and_several_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let one = tmp.path().join("one");
    let two = tmp.path().join("two");
    for r in [&one, &two] {
        std::fs::create_dir_all(r).unwrap();
        build_disk_tree(r).unwrap();
    }
    let s = scanned(&[&one, &two]);
    let mut mon = monitor(&s, false, None);
    let mut hooks = Recorder::default();
    std::fs::write(one.join("later.bin"), vec![b'l'; 12345]).unwrap();
    std::fs::write(deepest_dir(&two).join("also.bin"), vec![b'a'; 999]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    mon.step(&mut hooks);
    assert!(!same_as_disk(s.store(), &[&one, &two])); // not watched: nothing seen
    for r in [&one, &two] {
        mon.handle().rescan(r);
    }
    let store = s.store().clone();
    let (a, b) = (one.clone(), two.clone());
    until(&mut mon, &mut hooks, 20, move || same_as_disk(&store, &[&a, &b]));
    assert!(mon.status().watcher.is_empty());
}

#[test]
fn concurrent_writers_end_consistent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let s = scanned(&[&root]);
    let mut mon = monitor(&s, true, None);
    let handle = mon.handle();
    let store = s.store().clone();
    let runner = std::thread::spawn(move || {
        let mut hooks = Recorder::default();
        mon.run(&mut hooks);
        mon
    });
    let writers: Vec<_> = (0..4)
        .map(|seed: u64| {
            let mine = root.join(format!("w{seed}"));
            std::thread::spawn(move || {
                let mut rnd = sk_store::testing::Rng::new(seed + 1);
                std::fs::create_dir(&mine).unwrap();
                let mut made: Vec<PathBuf> = Vec::new();
                for step in 0..150 {
                    let op = rnd.below(100);
                    if op < 40 || made.is_empty() {
                        let f = mine.join(format!("f{step}.bin"));
                        std::fs::write(&f, vec![b'w'; rnd.below(20_000) as usize]).unwrap();
                        made.push(f);
                    } else if op < 60 {
                        use std::io::Write;
                        let f = &made[rnd.below(made.len() as u64) as usize];
                        let mut h = std::fs::OpenOptions::new().append(true).open(f).unwrap();
                        h.write_all(&vec![b'a'; 1 + rnd.below(9000) as usize]).unwrap();
                    } else if op < 75 {
                        let f = made.remove(rnd.below(made.len() as u64) as usize);
                        std::fs::remove_file(f).unwrap();
                    } else if op < 90 {
                        let f = made.remove(rnd.below(made.len() as u64) as usize);
                        let to = mine.join(format!("r{step}.bin"));
                        std::fs::rename(f, &to).unwrap();
                        made.push(to);
                    } else {
                        let sub = mine.join(format!("d{step}"));
                        std::fs::create_dir(&sub).unwrap();
                        std::fs::write(sub.join("inner.txt"), vec![b'i'; rnd.below(3000) as usize]).unwrap();
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    let end = Instant::now() + Duration::from_secs(30);
    while !same_as_disk(&store, &[&root]) {
        assert!(Instant::now() < end, "the tree did not catch up");
        std::thread::sleep(Duration::from_millis(200));
    }
    handle.stop();
    let mon = runner.join().unwrap();
    check_invariants(&store.read()).unwrap();
    drop(mon);
}

#[cfg(windows)]
#[test]
fn sizes_follow_the_hard_scan_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let s = scanned(&[&root]);
    let mut mon = monitor(&s, true, None);
    let mut hooks = Recorder::default();
    std::fs::write(root.join("c.txt"), vec![b'r'; 600]).unwrap(); // resident? the hard scan's rule
    let store = s.store().clone();
    let r2 = root.clone();
    until(&mut mon, &mut hooks, 20, move || same_as_disk(&store, &[&r2]));
}

#[cfg(windows)]
#[test]
fn overflow_replays_the_usn_journal() {
    struct Journal {
        ino: u64,
        calls: Mutex<Vec<(String, u64, u64)>>,
    }
    impl UsnSource for Journal {
        fn query(&self, _volume: &str) -> Result<(u64, u64), String> {
            Ok((42, 1000))
        }
        fn read(&self, volume: &str, start: u64, jid: u64, _max: usize) -> Result<UsnRead, String> {
            self.calls.lock().unwrap().push((volume.into(), start, jid));
            Ok(UsnRead { parent_frn: vec![self.ino | (7 << 48)], truncated: false })
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tree(tmp.path());
    let s = scanned(&[&root]);
    if !s.roots()[0].volume.as_ref().is_some_and(|v| v.is_local_ntfs()) {
        eprintln!("skipped: not a local NTFS volume");
        return;
    }
    let target = deepest_dir(&root);
    let ino = {
        let r = s.store().read();
        r.ino(r.find_path(&target).unwrap())
    };
    let journal = Arc::new(Journal { ino, calls: Mutex::new(Vec::new()) });
    let mut mon = monitor(&s, false, Some(journal.clone()));
    mon.start_usn();
    let mut hooks = Recorder::default();
    std::fs::write(target.join("lost.bin"), vec![b'l'; 7000]).unwrap();
    mon.handle().push(vec![Change::overflow(root.clone())]);
    let store = s.store().clone();
    let r2 = root.clone();
    until(&mut mon, &mut hooks, 10, move || same_as_disk(&store, &[&r2]));
    let vol = root.to_string_lossy()[..2].to_uppercase();
    assert_eq!(*journal.calls.lock().unwrap(), vec![(vol, 1000, 42)]); // from the safe checkpoint
    assert_eq!(mon.status().pending, 0);
    if ino == 0 {
        // sk-native's Windows lister records no file ids yet: the journal names no known folder,
        // so the overflow fell back to listing the root's subtree again (same result, slower)
        eprintln!("note: hard-scanned tree without file ids: USN replay fell back to a rescan");
    }
}
