//! The Windows watcher, live (port of backend/tests/test_monitor_win.py; decoding is tested in
//! the module).
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sk_core::monitor::win::{RdcwWatcher, is_network_path};
use sk_core::monitor::{Change, ChangeKind, Sink, WatchState, Watcher, create_watcher};

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Collector {
    batches: Mutex<Vec<Vec<Change>>>,
    threads: Mutex<Vec<std::thread::ThreadId>>,
    cv: Condvar,
}

impl Collector {
    fn sink(self: &Arc<Self>) -> Sink {
        let me = self.clone();
        Arc::new(move |b| {
            me.batches.lock().unwrap().push(b);
            me.threads.lock().unwrap().push(std::thread::current().id());
            me.cv.notify_all();
        })
    }
    fn changes(&self) -> Vec<Change> {
        self.batches.lock().unwrap().iter().flatten().cloned().collect()
    }
    fn wait_for(&self, pred: impl Fn(&Change) -> bool) -> Change {
        let end = Instant::now() + TIMEOUT;
        let mut g = self.batches.lock().unwrap();
        loop {
            if let Some(c) = g.iter().flatten().find(|c| pred(c)) {
                return c.clone();
            }
            let left = end.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no matching change; got {:?}", g.iter().flatten().collect::<Vec<_>>());
            g = self.cv.wait_timeout(g, left).unwrap().0;
        }
    }
}

fn wait_until(f: impl Fn() -> bool) {
    let end = Instant::now() + TIMEOUT;
    while !f() {
        assert!(Instant::now() < end, "condition not met");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn start(root: &Path, buffer: Option<usize>) -> (RdcwWatcher, Arc<Collector>) {
    let w = RdcwWatcher::new(root, buffer);
    let c = Arc::new(Collector::default());
    w.start(c.sink()).unwrap();
    wait_until(|| w.status().state == WatchState::Running);
    (w, c)
}

fn is(kind: ChangeKind, p: &Path) -> impl Fn(&Change) -> bool + '_ {
    move |c| c.kind == kind && c.path == p
}

fn rooted() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    (tmp, root)
}

#[test]
fn file_lifecycle() {
    let (_t, root) = rooted();
    let (w, sink) = start(&root, None);
    let p = root.join("a.txt");
    std::fs::write(&p, b"x").unwrap();
    assert_eq!(sink.wait_for(is(ChangeKind::Created, &p)).is_dir, Some(false));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[b'y'; 5000]).unwrap();
    }
    let m = sink.wait_for(|c| is(ChangeKind::Modified, &p)(c) && c.size == Some(5001));
    assert_eq!(m.is_dir, Some(false));
    assert!(m.alloc.unwrap() >= 5001);
    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert!((m.mtime.unwrap() - mtime).abs() <= 2);
    let q = root.join("b.txt");
    std::fs::rename(&p, &q).unwrap();
    let r = sink.wait_for(is(ChangeKind::Renamed, &q));
    assert_eq!((r.old_path, r.is_dir), (Some(p.clone()), Some(false)));
    std::fs::remove_file(&q).unwrap();
    assert_eq!(sink.wait_for(is(ChangeKind::Deleted, &q)).is_dir, Some(false));
    let st = w.status();
    assert_eq!((st.state, st.detail.as_str(), st.watched_dirs, st.overflows), (WatchState::Running, "", -1, 0));
    assert!(st.events as usize == sink.changes().len() && st.events >= 4);
    assert!(!sink.threads.lock().unwrap().contains(&std::thread::current().id()));
    assert!(sink.changes().iter().all(|c| c.path.starts_with(&root)));
    w.stop();
}

#[test]
fn folders() {
    let (_t, root) = rooted();
    let (w, sink) = start(&root, None);
    let d = root.join("d");
    std::fs::create_dir(&d).unwrap();
    assert_eq!(sink.wait_for(is(ChangeKind::Created, &d)).is_dir, Some(true));
    std::fs::write(d.join("c.bin"), b"").unwrap();
    sink.wait_for(is(ChangeKind::Created, &d.join("c.bin")));
    let e = root.join("e");
    std::fs::rename(&d, &e).unwrap();
    let r = sink.wait_for(is(ChangeKind::Renamed, &e));
    assert_eq!((r.old_path, r.is_dir), (Some(d.clone()), Some(true)));
    std::fs::write(e.join("c.bin"), [b'z'; 10]).unwrap();
    sink.wait_for(|c| is(ChangeKind::Modified, &e.join("c.bin"))(c) && c.size == Some(10));
    assert!(sink.changes().iter().filter(|c| c.kind == ChangeKind::Modified).all(|c| c.is_dir == Some(false)));
    w.stop();
}

#[test]
fn unusual_names() {
    let (_t, root) = rooted();
    let (w, sink) = start(&root, None);
    let p = root.join("报告 😀.txt");
    std::fs::write(&p, b"1").unwrap();
    sink.wait_for(is(ChangeKind::Created, &p));
    use std::os::windows::ffi::OsStringExt;
    let lone = root.join(std::ffi::OsString::from_wide(&[0x6C, 0xD800, 0x2E, 0x74]));
    if std::fs::write(&lone, b"1").is_ok() {
        sink.wait_for(is(ChangeKind::Created, &lone)); // lone surrogates kept, as in the scan
    }
    w.stop();
}

#[test]
fn overflow_reported_and_watching_goes_on() {
    let (_t, root) = rooted();
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let c = Arc::new(Collector::default());
    let inner = c.sink();
    let (en, re) = (entered.clone(), release.clone());
    let gated: Sink = Arc::new(move |b| {
        inner(b);
        let first = {
            let mut g = en.0.lock().unwrap();
            !std::mem::replace(&mut *g, true)
        };
        if first {
            en.1.notify_all();
            let g = re.0.lock().unwrap();
            let _ = re.1.wait_timeout_while(g, TIMEOUT, |r| !*r).unwrap();
        }
    });
    let w = RdcwWatcher::new(&root, Some(512));
    assert_eq!(w.buffer_size(), 512);
    w.start(gated).unwrap();
    wait_until(|| w.status().state == WatchState::Running);
    std::fs::write(root.join("first.txt"), b"").unwrap();
    {
        let g = entered.0.lock().unwrap();
        let (g, _) = entered.1.wait_timeout_while(g, TIMEOUT, |e| !*e).unwrap();
        assert!(*g);
    }
    for i in 0..300 {
        std::fs::write(root.join(format!("f{i:03}.txt")), b"").unwrap();
    }
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    assert_eq!(c.wait_for(|c| c.kind == ChangeKind::Overflow).path, root);
    let st = w.status();
    assert!(st.state == WatchState::Running && st.overflows >= 1);
    std::fs::write(root.join("after.txt"), b"").unwrap();
    c.wait_for(is(ChangeKind::Created, &root.join("after.txt")));
    w.stop();
}

#[test]
fn stop_is_quick_and_final() {
    let (_t, root) = rooted();
    let (w, sink) = start(&root, None);
    std::fs::write(root.join("x.txt"), b"").unwrap();
    sink.wait_for(is(ChangeKind::Created, &root.join("x.txt")));
    let t0 = Instant::now();
    w.stop();
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(w.finished());
    let calls = sink.batches.lock().unwrap().len();
    for i in 0..20 {
        std::fs::write(root.join(format!("y{i}.txt")), b"").unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(sink.batches.lock().unwrap().len(), calls);
    let st = w.status();
    assert_eq!((st.state, st.watched_dirs), (WatchState::Stopped, 0));
    let t0 = Instant::now();
    w.stop(); // idempotent
    assert!(t0.elapsed() < Duration::from_millis(100));
    assert!(w.start(sink.sink()).is_err());
}

#[test]
fn stop_from_the_sink() {
    let (_t, root) = rooted();
    let holder: Arc<OnceLock<Arc<RdcwWatcher>>> = Arc::new(OnceLock::new());
    let c = Arc::new(Collector::default());
    let inner = c.sink();
    let h = holder.clone();
    let stopper: Sink = Arc::new(move |b| {
        inner(b);
        h.get().unwrap().stop(); // on the watcher's own thread: no deadlock
    });
    let w = Arc::new(RdcwWatcher::new(&root, None));
    let _ = holder.set(w.clone());
    w.start(stopper).unwrap();
    wait_until(|| matches!(w.status().state, WatchState::Running | WatchState::Stopped));
    std::fs::write(root.join("a.txt"), b"").unwrap();
    std::fs::write(root.join("b.txt"), b"").unwrap();
    wait_until(|| w.finished());
    assert_eq!(c.batches.lock().unwrap().len(), 1);
    assert_eq!(w.status().state, WatchState::Stopped);
}

#[test]
fn deleted_root_fails() {
    let (_t, root) = rooted();
    let (w, sink) = start(&root, None);
    std::fs::write(root.join("z.txt"), b"").unwrap();
    sink.wait_for(is(ChangeKind::Created, &root.join("z.txt")));
    std::fs::remove_dir_all(&root).unwrap();
    sink.wait_for(is(ChangeKind::Overflow, &root));
    let st = w.status();
    assert_eq!((st.state, st.detail.as_str(), st.watched_dirs), (WatchState::Failed, sk_core::i18n::t("监视的文件夹已被删除"), 0));
    assert_eq!(st.overflows, 1);
    wait_until(|| w.finished());
    w.stop();
    assert_eq!(w.status().state, WatchState::Failed);
}

#[test]
fn moved_root_fails() {
    let (t, root) = rooted();
    let (w, sink) = start(&root, None);
    std::fs::rename(&root, t.path().join("moved")).unwrap();
    sink.wait_for(is(ChangeKind::Overflow, &root));
    let st = w.status();
    assert_eq!((st.state, st.detail.as_str()), (WatchState::Failed, sk_core::i18n::t("监视的文件夹已被移动或改名")));
    w.stop();
}

#[test]
fn missing_root_fails_without_overflow() {
    let tmp = tempfile::tempdir().unwrap();
    let w = RdcwWatcher::new(&tmp.path().join("nope"), None);
    let c = Arc::new(Collector::default());
    w.start(c.sink()).unwrap();
    wait_until(|| w.status().state == WatchState::Failed);
    let prefix = sk_core::i18n::t("无法监视此文件夹：{reason}").replace("{reason}", "");
    assert!(w.status().detail.starts_with(&prefix));
    wait_until(|| w.finished());
    assert!(c.batches.lock().unwrap().is_empty());
    w.stop();
}

#[test]
fn plain_call_fallback() {
    let (_t, root) = rooted();
    let w = RdcwWatcher::new(&root, None);
    w.use_extended.store(false, std::sync::atomic::Ordering::SeqCst);
    let c = Arc::new(Collector::default());
    w.start(c.sink()).unwrap();
    wait_until(|| w.status().state == WatchState::Running);
    assert_eq!(w.status().detail, sk_core::i18n::t("此卷不提供扩展变化信息，文件大小变化需重新列出目录"));
    let p = root.join("a.txt");
    std::fs::write(&p, b"x").unwrap();
    assert_eq!(c.wait_for(is(ChangeKind::Created, &p)).is_dir, None);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[b'y'; 100]).unwrap();
    }
    let m = c.wait_for(is(ChangeKind::Modified, &p));
    assert_eq!((m.size, m.alloc, m.mtime, m.is_dir), (None, None, None, None));
    let q = root.join("b.txt");
    std::fs::rename(&p, &q).unwrap();
    assert_eq!(c.wait_for(is(ChangeKind::Renamed, &q)).old_path, Some(p));
    w.stop();
}

#[test]
fn buffer_sizes_and_factory() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!is_network_path(tmp.path()));
    assert!(is_network_path(Path::new(r"\\server\share\folder")));
    assert!(is_network_path(Path::new(r"\\?\UNC\server\share")));
    assert_eq!(RdcwWatcher::new(tmp.path(), Some(1001)).buffer_size(), 1008);
    let (w, _) = start(tmp.path(), None);
    assert_eq!(w.buffer_size(), 1 << 20);
    assert!(w.extended());
    w.stop();
    let made = create_watcher(tmp.path(), 30.0, 2000);
    assert_eq!(made.name(), "rdcw");
    made.add_dirs(vec![tmp.path().to_path_buf()]);
    made.stop(); // never started: fine
}
