//! Port of the live-inotify part of 0.3.4's `tests/test_monitor_posix.py`: a real
//! [`InotifyWatcher`] on a temp folder (Linux), the choice of watcher on every platform, and
//! undecodable names through [`interpret`] (POSIX).
// the helpers serve the live tests, which only build on Linux
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sk_core::monitor::{Change, ChangeKind, create_watcher};

fn wait_until(secs: u64, what: &str, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What a watcher reported so far.
#[derive(Clone, Default)]
struct Collector(Arc<Mutex<Vec<Change>>>);

impl Collector {
    fn sink(&self) -> sk_core::monitor::Sink {
        let got = self.0.clone();
        Arc::new(move |batch| got.lock().unwrap().extend(batch))
    }

    fn changes(&self) -> Vec<Change> {
        self.0.lock().unwrap().clone()
    }

    fn has(&self, kind: ChangeKind, path: &Path, is_dir: Option<bool>) -> bool {
        self.changes()
            .iter()
            .any(|c| c.kind == kind && c.path == path && (is_dir.is_none() || c.is_dir == is_dir))
    }

    fn wait_for(&self, kind: ChangeKind, path: &Path, is_dir: Option<bool>) {
        wait_until(5, &format!("{kind:?} {}", path.display()), || {
            self.has(kind, path, is_dir)
        });
    }

    fn created_or_renamed(&self, path: &Path) -> bool {
        self.changes()
            .iter()
            .any(|c| c.path == path && matches!(c.kind, ChangeKind::Created | ChangeKind::Renamed))
    }
}

/// The watcher picked for a scan root: inotify on Linux, the platform's own elsewhere (0.3.4:
/// `create_watcher_picks_inotify`, `inotify_unavailable_off_linux_falls_back_to_polling` — off
/// Linux the Rust crate has no inotify watcher at all, so no polling stand-in can be picked).
#[test]
fn create_watcher_picks_the_platform_watcher() {
    let tmp = tempfile::tempdir().unwrap();
    let w = create_watcher(tmp.path(), 0.2, 2000);
    let want = if cfg!(target_os = "linux") {
        "inotify"
    } else if cfg!(windows) {
        "rdcw"
    } else if cfg!(target_os = "macos") {
        "fsevents"
    } else {
        "poll"
    };
    assert_eq!(w.name(), want);
}

/// A name that is not UTF-8 keeps its bytes (0.3.4: `surrogate_names`).
#[cfg(unix)]
#[test]
fn surrogate_names() {
    use std::os::unix::ffi::OsStrExt;

    use sk_core::monitor::linux::{IN_CREATE, RawEvent, WatchTree, interpret};
    let mut tree = WatchTree::new(b"/r");
    tree.set_root(1);
    tree.add(2, 1, b"a");
    tree.add(3, 2, b"b");
    let res = interpret(&[RawEvent::new(2, IN_CREATE, 0, b"bad\xff")], &mut tree);
    assert_eq!(res.changes.len(), 1);
    assert_eq!(res.changes[0].kind, ChangeKind::Created);
    assert_eq!(res.changes[0].path.as_os_str().as_bytes(), b"/r/a/bad\xff");
}

#[cfg(target_os = "linux")]
mod live {
    use super::*;
    use sk_core::monitor::linux::InotifyWatcher;
    use sk_core::monitor::{WatchState, Watcher};

    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path().join("root");
        std::fs::create_dir(&r).unwrap();
        (tmp, r)
    }

    fn watching(root: &Path, max_watches: Option<usize>) -> (InotifyWatcher, Collector) {
        let w = InotifyWatcher::new(root, 0.2, 2000, max_watches);
        let got = Collector::default();
        w.start(got.sink()).unwrap();
        (w, got)
    }

    fn mtime(p: &Path) -> i64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).unwrap().mtime()
    }

    #[test]
    fn live_files() {
        let (_tmp, root) = root();
        let (w, got) = watching(&root, None);
        assert_eq!(w.name(), "inotify");
        let st = w.status();
        assert_eq!(
            (st.state, st.watched_dirs, st.polled_dirs),
            (WatchState::Running, 1, 0)
        );
        let f = root.join("x.txt");
        std::fs::write(&f, b"hello").unwrap();
        got.wait_for(ChangeKind::Created, &f, Some(false));
        wait_until(5, "modified x.txt with its size", || {
            got.changes()
                .iter()
                .any(|c| c.kind == ChangeKind::Modified && c.path == f && c.size == Some(5))
        });
        let m = got
            .changes()
            .into_iter()
            .find(|c| c.kind == ChangeKind::Modified)
            .unwrap();
        assert!(m.alloc.is_some());
        assert_eq!(m.mtime, Some(mtime(&f)));
        let g = root.join("y.txt");
        std::fs::rename(&f, &g).unwrap();
        wait_until(5, "renamed x.txt → y.txt", || {
            got.changes().iter().any(|c| {
                c.kind == ChangeKind::Renamed
                    && c.path == g
                    && c.old_path.as_deref() == Some(f.as_path())
                    && c.is_dir == Some(false)
            })
        });
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        wait_until(5, "modified y.txt (chmod)", || {
            got.changes()
                .iter()
                .any(|c| c.kind == ChangeKind::Modified && c.path == g)
        });
        std::fs::remove_file(&g).unwrap();
        got.wait_for(ChangeKind::Deleted, &g, Some(false));
        assert_eq!(w.status().events, got.changes().len() as i64);
        w.stop();
    }

    #[test]
    fn live_new_folder_is_watched() {
        let (_tmp, root) = root();
        let (w, got) = watching(&root, None);
        let d = root.join("d");
        std::fs::create_dir(&d).unwrap();
        got.wait_for(ChangeKind::Created, &d, Some(true));
        wait_until(5, "two watches", || w.status().watched_dirs == 2);
        std::fs::write(d.join("inner.txt"), b"1").unwrap();
        wait_until(5, "inner.txt", || {
            got.created_or_renamed(&d.join("inner.txt"))
        });
        let e = root.join("e");
        std::fs::rename(&d, &e).unwrap();
        wait_until(5, "renamed d → e", || {
            got.changes().iter().any(|c| {
                c.kind == ChangeKind::Renamed
                    && c.path == e
                    && c.old_path.as_deref() == Some(d.as_path())
                    && c.is_dir == Some(true)
            })
        });
        std::fs::write(e.join("after.txt"), b"2").unwrap();
        // the watch followed the rename
        got.wait_for(ChangeKind::Created, &e.join("after.txt"), None);
        std::fs::remove_dir_all(&e).unwrap();
        got.wait_for(ChangeKind::Deleted, &e, Some(true));
        wait_until(5, "one watch again", || w.status().watched_dirs == 1);
        w.stop();
    }

    #[test]
    fn live_mkdir_race_reports_existing_entries() {
        let (tmp, root) = root();
        let (_w, got) = watching(&root, None);
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::fs::write(outside.join("a.txt"), b"a").unwrap();
        std::fs::write(outside.join("sub").join("b.txt"), b"b").unwrap();
        let moved = root.join("moved");
        // moved in from outside: one IN_MOVED_TO, the contents come from the walk
        std::fs::rename(&outside, &moved).unwrap();
        got.wait_for(ChangeKind::Created, &moved, Some(true));
        got.wait_for(ChangeKind::Created, &moved.join("a.txt"), Some(false));
        got.wait_for(ChangeKind::Created, &moved.join("sub"), Some(true));
        got.wait_for(
            ChangeKind::Created,
            &moved.join("sub").join("b.txt"),
            Some(false),
        );
        // the walk also watched the subfolder
        std::fs::write(moved.join("sub").join("c.txt"), b"c").unwrap();
        got.wait_for(ChangeKind::Created, &moved.join("sub").join("c.txt"), None);
    }

    #[test]
    fn live_add_dirs() {
        let (_tmp, root) = root();
        let (w, got) = watching(&root, None);
        let a = root.join("a");
        let b = a.join("b");
        std::fs::create_dir_all(&b).unwrap();
        got.wait_for(ChangeKind::Created, &a, Some(true));
        // already watched through the walk: no duplicates
        w.add_dirs(vec![a.clone(), b.clone()], None, 0);
        wait_until(5, "three watches", || w.status().watched_dirs == 3);
        std::fs::write(b.join("f"), b"").unwrap();
        got.wait_for(ChangeKind::Created, &b.join("f"), None);
        assert_eq!(w.status().watched_dirs, 3);
        w.stop();
    }

    #[test]
    fn live_budget_polls_the_rest() {
        let (_tmp, root) = root();
        let (a, b) = (root.join("a"), root.join("a").join("b"));
        let c = b.join("c");
        std::fs::create_dir_all(&c).unwrap();
        let (w, got) = watching(&root, Some(2));
        w.add_dirs(vec![a.clone(), b.clone(), c.clone()], None, 0);
        wait_until(5, "two polled folders", || w.status().polled_dirs == 2);
        let st = w.status();
        assert_eq!(st.watched_dirs, 2);
        assert_eq!(st.state, WatchState::Degraded);
        assert!(
            st.detail.contains("inotify") && st.detail.contains('2'),
            "{}",
            st.detail
        );
        // baselines are taken right away; the next change must be a new mtime
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(c.join("new.txt"), b"n").unwrap();
        got.wait_for(ChangeKind::Dir, &c, Some(true));
        // the watched part still reports at once
        std::fs::write(a.join("watched.txt"), b"w").unwrap();
        got.wait_for(ChangeKind::Created, &a.join("watched.txt"), None);
        std::fs::remove_dir_all(&b).unwrap();
        got.wait_for(ChangeKind::Deleted, &b, Some(true));
        w.stop();
    }

    /// After IN_Q_OVERFLOW the deletion of a watched folder may be among the lost events; a
    /// folder re-created under the same name must get a watch of its own. 0.3.4 drove the
    /// watcher by hand; here the sink holds the watcher's thread while the kernel queue
    /// (`max_queued_events`) overflows, the watched folder is replaced, then the thread goes on.
    #[test]
    fn overflow_revalidation_drops_stale_watches() {
        let (_tmp, root) = root();
        let d = root.join("d");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let queue: usize = std::fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(16384);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let got = Collector::default();
        let sink: sk_core::monitor::Sink = {
            let (gate, held, all) = (gate.clone(), held.clone(), got.0.clone());
            Arc::new(move |batch: Vec<Change>| {
                all.lock().unwrap().extend(batch);
                let (lock, cv) = &*gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    held.store(true, std::sync::atomic::Ordering::SeqCst);
                    open = cv.wait(open).unwrap();
                }
            })
        };
        let w = InotifyWatcher::new(&root, 0.2, 2000, None);
        w.start(sink).unwrap();
        w.add_dirs(vec![d.clone(), d.join("sub")], None, 0);
        wait_until(5, "three watches", || w.status().watched_dirs == 3);
        // the next batch holds the thread: nothing is read while the queue overflows
        std::fs::write(root.join("trigger"), b"t").unwrap();
        wait_until(5, "the sink to hold the thread", || {
            held.load(std::sync::atomic::Ordering::SeqCst)
        });
        let churn = root.join("churn");
        for _ in 0..queue / 2 + 64 {
            std::fs::write(&churn, b"").unwrap();
            std::fs::remove_file(&churn).unwrap();
        }
        std::fs::remove_dir_all(&d).unwrap(); // its IN_IGNORED is lost too
        std::fs::create_dir_all(d.join("new")).unwrap();
        {
            let (lock, cv) = &*gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        got.wait_for(ChangeKind::Overflow, &root, Some(true));
        // the stale watch of the old `d` is gone and the new `d` and `d/new` are watched
        wait_until(10, "the new d/new watched", || w.status().watched_dirs == 3);
        std::fs::write(d.join("new").join("after.txt"), b"a").unwrap();
        got.wait_for(ChangeKind::Created, &d.join("new").join("after.txt"), None);
        w.stop();
    }

    #[test]
    fn live_root_deleted() {
        let (_tmp, root) = root();
        let (w, got) = watching(&root, None);
        std::fs::remove_dir(&root).unwrap();
        got.wait_for(ChangeKind::Overflow, &root, Some(true));
        wait_until(5, "failed", || w.status().state == WatchState::Failed);
        assert!(!w.status().detail.is_empty());
    }

    #[test]
    fn live_stop_is_quick_and_silent() {
        let (_tmp, root) = root();
        let (w, got) = watching(&root, None);
        std::fs::write(root.join("one"), b"").unwrap();
        got.wait_for(ChangeKind::Created, &root.join("one"), None);
        let t0 = Instant::now();
        w.stop();
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
        let st = w.status();
        assert_eq!((st.state, st.watched_dirs), (WatchState::Stopped, 0));
        let seen = got.changes().len();
        std::fs::write(root.join("two"), b"").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(got.changes().len(), seen);
        w.stop(); // idempotent
    }
}
