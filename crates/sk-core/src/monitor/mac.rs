//! macOS watcher: FSEvents (docs/monitor.md §2.3).
//!
//! **Untested on macOS** (as the Python version): written from the CoreServices headers without
//! a Mac to run it on. The event mapping ([`map_events`]) is plain code tested everywhere.
//!
//! One stream per scan root — file-level events, no deferral, `WatchRoot`, 50 ms latency — is
//! scheduled on the CFRunLoop of a dedicated thread; each callback becomes one batch for the sink.
//! The stream is recursive: `add_dirs` is ignored. FSEvents merges the flags of one path within
//! the latency window, so structural events are decided by whether the path exists now. A rename
//! arrives as two events with consecutive ids (old path, then new path). Event paths are
//! canonical (`/private/var/…`), so they are mapped back onto the root as the scan spelled it; a
//! path inside the boot volume group's Data volume first goes back through the firmlinks.

use std::path::{Path, PathBuf};

use super::{Change, ChangeKind};

pub const F_MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
pub const F_USER_DROPPED: u32 = 0x0000_0002;
pub const F_KERNEL_DROPPED: u32 = 0x0000_0004;
pub const F_EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
pub const F_HISTORY_DONE: u32 = 0x0000_0010;
pub const F_ROOT_CHANGED: u32 = 0x0000_0020;
pub const F_MOUNT: u32 = 0x0000_0040;
pub const F_UNMOUNT: u32 = 0x0000_0080;
pub const F_ITEM_CREATED: u32 = 0x0000_0100;
pub const F_ITEM_REMOVED: u32 = 0x0000_0200;
pub const F_ITEM_INODE_META_MOD: u32 = 0x0000_0400;
pub const F_ITEM_RENAMED: u32 = 0x0000_0800;
pub const F_ITEM_MODIFIED: u32 = 0x0000_1000;
pub const F_ITEM_IS_FILE: u32 = 0x0001_0000;
pub const F_ITEM_IS_DIR: u32 = 0x0002_0000;
pub const F_ITEM_IS_SYMLINK: u32 = 0x0004_0000;
const ITEM_ANY: u32 = 0x007F_FF00;
const DROPPED: u32 = F_USER_DROPPED | F_KERNEL_DROPPED;

/// One FSEvents event: (path, flags, event id).
pub type Event = (PathBuf, u32, u64);

/// `(size, alloc, mtime)` of a file ([`super::poll::stat_file`]).
pub type Numbers = dyn Fn(&Path) -> Option<(i64, i64, i64)>;

/// Changes for one callback's events, and whether the root itself was moved or deleted.
pub fn map_events(
    events: &[Event],
    root: &Path,
    exists: &dyn Fn(&Path) -> bool,
    numbers: &Numbers,
) -> (Vec<Change>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let (path, flags, eid) = &events[i];
        let (flags, eid) = (*flags, *eid);
        i += 1;
        if flags & (DROPPED | F_MUST_SCAN_SUBDIRS) != 0 {
            let at = if flags & DROPPED != 0 { root } else { path.as_path() };
            out.push(Change::overflow(at));
            continue;
        }
        if flags & F_ROOT_CHANGED != 0 {
            out.push(Change::overflow(root));
            return (out, true);
        }
        if flags & (F_MOUNT | F_UNMOUNT) != 0 {
            out.push(Change::overflow(path.clone()));
            continue;
        }
        if flags & ITEM_ANY == 0 {
            if flags & (F_HISTORY_DONE | F_EVENT_IDS_WRAPPED) == 0 {
                out.push(Change::new(ChangeKind::Dir, path.clone(), Some(true))); // list it again
            }
            continue;
        }
        let kinds = F_ITEM_IS_DIR | F_ITEM_IS_FILE | F_ITEM_IS_SYMLINK;
        let is_dir = (flags & kinds != 0).then_some(flags & F_ITEM_IS_DIR != 0);
        if flags & F_ITEM_RENAMED != 0 {
            let here = exists(path);
            if !here && i < events.len() {
                let (new, new_flags, new_id) = &events[i];
                if new_flags & F_ITEM_RENAMED != 0 && *new_id == eid + 1 && exists(new) {
                    out.push(Change::renamed(new.clone(), Some(path.clone()), is_dir));
                    i += 1;
                    continue;
                }
            }
            let kind = if here { ChangeKind::Created } else { ChangeKind::Deleted };
            out.push(Change::new(kind, path.clone(), is_dir));
        } else if flags & (F_ITEM_CREATED | F_ITEM_REMOVED) != 0 {
            let kind = if exists(path) { ChangeKind::Created } else { ChangeKind::Deleted };
            out.push(Change::new(kind, path.clone(), is_dir));
        } else if flags & (F_ITEM_MODIFIED | F_ITEM_INODE_META_MOD) != 0
            && is_dir != Some(true)
            && let Some((size, alloc, mtime)) = numbers(path)
        {
            out.push(Change::modified(path.clone(), size, Some(alloc), mtime));
        }
    }
    (out, false)
}

#[cfg(target_os = "macos")]
pub use imp::FsEventsWatcher;

/// Constructing an FSEvents watcher off macOS fails (the caller polls instead).
#[cfg(not(target_os = "macos"))]
pub struct FsEventsWatcher;

#[cfg(not(target_os = "macos"))]
impl FsEventsWatcher {
    pub fn new(_root: &Path) -> std::io::Result<FsEventsWatcher> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "FSEvents is macOS only"))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::super::poll::{Emitter, Worker, stat_file};
    use super::super::{Change, Sink, WatchState, WatchStatus, Watcher, t};
    use super::map_events;
    use crate::volume::{through_firmlinks, volume_group_twins};

    type CFRef = *const c_void;
    type Callback = extern "C" fn(CFRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

    #[repr(C)]
    struct StreamContext {
        version: isize,
        info: *mut c_void,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: CFRef;
        static kCFTypeArrayCallBacks: c_void;
        fn CFRelease(cf: CFRef);
        fn CFStringCreateWithFileSystemRepresentation(alloc: CFRef, buffer: *const c_char) -> CFRef;
        fn CFArrayCreate(alloc: CFRef, values: *const CFRef, num: isize, callbacks: *const c_void) -> CFRef;
        fn CFRunLoopGetCurrent() -> CFRef;
        fn CFRunLoopRunInMode(mode: CFRef, seconds: f64, return_after_source_handled: u8) -> i32;
        fn CFRunLoopStop(rl: CFRef);
        fn CFRunLoopWakeUp(rl: CFRef);
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn FSEventStreamCreate(
            alloc: CFRef,
            callback: Callback,
            context: *const StreamContext,
            paths: CFRef,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> CFRef;
        fn FSEventStreamScheduleWithRunLoop(stream: CFRef, rl: CFRef, mode: CFRef);
        fn FSEventStreamStart(stream: CFRef) -> u8;
        fn FSEventStreamStop(stream: CFRef);
        fn FSEventStreamInvalidate(stream: CFRef);
        fn FSEventStreamRelease(stream: CFRef);
    }

    const CREATE_FLAGS: u32 = 0x10 | 0x02 | 0x04; // file events, no defer, watch root
    const SINCE_NOW: u64 = u64::MAX;
    const LATENCY: f64 = 0.05;
    const RUN_FINISHED: i32 = 1;

    struct Shared {
        root: PathBuf,
        real: PathBuf,
        twins: Vec<String>,
        emitter: Emitter,
        state: Mutex<(WatchState, String)>,
        run_loop: AtomicUsize,
    }

    impl Shared {
        /// An event path as the scan spells it; None when it is not in the tree.
        fn local(&self, raw: &[u8]) -> Option<PathBuf> {
            let mut path = String::from_utf8_lossy(raw).into_owned();
            if path.len() > 1 {
                path = path.trim_end_matches('/').to_owned();
            }
            for twin in &self.twins {
                if path == *twin || path.starts_with(&format!("{twin}/")) {
                    path = through_firmlinks(&path, twin, None)?;
                    break;
                }
            }
            let real = self.real.to_string_lossy();
            let root = self.root.to_string_lossy();
            if real != root && (path == real || path.starts_with(&format!("{}/", real.trim_end_matches('/')))) {
                path = format!("{root}{}", &path[real.len()..]);
            }
            Some(PathBuf::from(path))
        }
    }

    extern "C" fn on_events(_s: CFRef, info: *mut c_void, n: usize, paths: *mut c_void, flags: *const u32, ids: *const u64) {
        // SAFETY: `info` is the Arc<Shared> pointer the stream was created with (alive while it runs).
        let sh = unsafe { &*(info as *const Shared) };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let paths = paths as *const *const c_char;
            let mut events = Vec::with_capacity(n);
            for k in 0..n {
                // SAFETY: FSEvents hands `n` C strings, flags and ids.
                let (p, f, id) = unsafe { (CStr::from_ptr(*paths.add(k)), *flags.add(k), *ids.add(k)) };
                if let Some(path) = sh.local(p.to_bytes()) {
                    events.push((path, f, id));
                }
            }
            let (changes, lost) = map_events(&events, &sh.root, &|p| std::fs::symlink_metadata(p).is_ok(), &stat_file);
            sh.emitter.emit(changes);
            if lost {
                *sh.state.lock().unwrap() = (WatchState::Failed, t("扫描根目录已被删除或移走，实时监控已停止"));
                // SAFETY: we are on the run loop's thread.
                unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
            }
        }));
        if r.is_err() {
            tracing::error!("{}", t("FSEvents 回调出错"));
        }
    }

    /// One recursive FSEvents stream for a scan root.
    pub struct FsEventsWatcher {
        sh: Arc<Shared>,
        worker: Worker,
    }

    impl FsEventsWatcher {
        pub fn new(root: &Path) -> std::io::Result<FsEventsWatcher> {
            let root = std::path::absolute(root)?;
            let real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            let rs = real.to_string_lossy().into_owned();
            // the boot volume group's Data volume, unless the root lies inside it
            let twins = volume_group_twins("/")
                .into_iter()
                .filter(|tw| rs != *tw && !rs.starts_with(&format!("{tw}/")))
                .collect();
            Ok(FsEventsWatcher {
                sh: Arc::new(Shared {
                    root,
                    real,
                    twins,
                    emitter: Emitter::new(),
                    state: Mutex::new((WatchState::Starting, String::new())),
                    run_loop: AtomicUsize::new(0),
                }),
                worker: Worker::new(),
            })
        }
    }

    fn run(sh: Arc<Shared>, ready: std::sync::mpsc::Sender<()>) {
        let mut started = false;
        let mut stream: CFRef = std::ptr::null();
        let result: Result<(), String> = (|| unsafe {
            let c = CString::new(sh.root.as_os_str().as_bytes()).map_err(|_| t("无法创建路径字符串"))?;
            let path = CFStringCreateWithFileSystemRepresentation(std::ptr::null(), c.as_ptr());
            if path.is_null() {
                return Err(t("无法创建路径字符串"));
            }
            let values = [path];
            let paths = CFArrayCreate(std::ptr::null(), values.as_ptr(), 1, &kCFTypeArrayCallBacks);
            CFRelease(path);
            if paths.is_null() {
                return Err(t("无法创建路径数组"));
            }
            let ctx = StreamContext {
                version: 0,
                info: Arc::as_ptr(&sh) as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            stream = FSEventStreamCreate(std::ptr::null(), on_events, &ctx, paths, SINCE_NOW, LATENCY, CREATE_FLAGS);
            CFRelease(paths);
            if stream.is_null() {
                return Err(t("FSEventStreamCreate 失败"));
            }
            let rl = CFRunLoopGetCurrent();
            FSEventStreamScheduleWithRunLoop(stream, rl, kCFRunLoopDefaultMode);
            if FSEventStreamStart(stream) == 0 {
                return Err(t("FSEventStreamStart 失败"));
            }
            started = true;
            sh.run_loop.store(rl as usize, Ordering::SeqCst);
            *sh.state.lock().unwrap() = (WatchState::Running, String::new());
            let _ = ready.send(());
            while !sh.emitter.stopping.load(Ordering::SeqCst) && sh.state.lock().unwrap().0 == WatchState::Running {
                if CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0) == RUN_FINISHED {
                    return Err(t("CFRunLoop 没有可运行的源"));
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("{}", crate::tr!("FSEvents 监视失败：{path}：{error}", path = sh.root.display().to_string(), error = e.clone()));
            *sh.state.lock().unwrap() = (WatchState::Failed, crate::tr!("FSEvents 不可用（{error}），实时监控已停止", error = e));
            if started {
                sh.emitter.emit(vec![Change::overflow(sh.root.clone())]);
            }
        }
        sh.run_loop.store(0, Ordering::SeqCst);
        if !stream.is_null() {
            // SAFETY: the stream we created.
            unsafe {
                if started {
                    FSEventStreamStop(stream);
                }
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
            }
        }
        let _ = ready.send(());
    }

    impl Watcher for FsEventsWatcher {
        fn name(&self) -> &'static str {
            "fsevents"
        }

        fn start(&self, sink: Sink) -> std::io::Result<()> {
            if self.worker.started() {
                return Err(std::io::Error::other(t("监视器已经启动")));
            }
            *self.sh.emitter.sink.lock().unwrap() = Some(sink);
            let (tx, rx) = std::sync::mpsc::channel();
            let sh = self.sh.clone();
            self.worker.spawn("sk-fsevents", move || run(sh, tx))?;
            let _ = rx.recv_timeout(Duration::from_secs(1));
            Ok(())
        }

        fn add_dirs(&self, _dirs: Vec<PathBuf>, _mtimes: Option<Vec<u32>>, _since_ns: i64) {} // recursive stream, from the scan's start

        fn stop(&self) {
            if self.sh.emitter.stopping.swap(true, Ordering::SeqCst) {
                return;
            }
            let rl = self.sh.run_loop.load(Ordering::SeqCst);
            if rl != 0 {
                // SAFETY: the run loop of our thread (alive while the slot is set).
                unsafe {
                    CFRunLoopStop(rl as CFRef);
                    CFRunLoopWakeUp(rl as CFRef);
                }
            }
            self.worker.finish(Duration::from_millis(900));
            let mut s = self.sh.state.lock().unwrap();
            if s.0 != WatchState::Failed {
                s.0 = WatchState::Stopped;
            }
        }

        fn status(&self) -> WatchStatus {
            let (state, detail) = self.sh.state.lock().unwrap().clone();
            WatchStatus {
                watched_dirs: if state == WatchState::Running { -1 } else { 0 },
                polled_dirs: 0,
                overflows: self.sh.emitter.overflows.load(Ordering::Relaxed),
                events: self.sh.emitter.events.load(Ordering::Relaxed),
                state,
                detail,
            }
        }
    }

    impl Drop for FsEventsWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsevents_mapping() {
        let root = PathBuf::from("/r");
        let exists = |p: &Path| matches!(p.to_str(), Some("/r/new" | "/r/here" | "/r/b"));
        let numbers = |p: &Path| (p.to_str() == Some("/r/file")).then_some((10, 4096, 99));
        let ev = |p: &str, f: u32, id: u64| (PathBuf::from(p), f, id);
        let (out, lost) = map_events(
            &[
                ev("/r/old", F_ITEM_RENAMED | F_ITEM_IS_DIR, 10),
                ev("/r/new", F_ITEM_RENAMED | F_ITEM_IS_DIR, 11),
                ev("/r/here", F_ITEM_CREATED | F_ITEM_REMOVED | F_ITEM_IS_FILE, 12),
                ev("/r/gone", F_ITEM_CREATED | F_ITEM_REMOVED | F_ITEM_IS_FILE, 13),
                ev("/r/file", F_ITEM_MODIFIED | F_ITEM_IS_FILE, 14),
                ev("/r/vanished", F_ITEM_MODIFIED | F_ITEM_IS_FILE, 15),
                ev("/r/a", 0, 16),
                ev("/r/sub", F_MUST_SCAN_SUBDIRS, 17),
                ev("/r/x", F_KERNEL_DROPPED, 18),
                ev("/r", F_HISTORY_DONE, 19),
                ev("/r/lonely", F_ITEM_RENAMED | F_ITEM_IS_FILE, 30),
                ev("/r/b", F_ITEM_RENAMED | F_ITEM_IS_FILE, 40),
            ],
            &root,
            &exists,
            &numbers,
        );
        assert!(!lost);
        assert_eq!(out, vec![
            Change::renamed("/r/new", Some("/r/old".into()), Some(true)),
            Change::new(ChangeKind::Created, "/r/here", Some(false)),
            Change::new(ChangeKind::Deleted, "/r/gone", Some(false)),
            Change::modified("/r/file", 10, Some(4096), 99),
            Change::new(ChangeKind::Dir, "/r/a", Some(true)),
            Change::overflow("/r/sub"),
            Change::overflow("/r"),
            Change::new(ChangeKind::Deleted, "/r/lonely", Some(false)),
            Change::new(ChangeKind::Created, "/r/b", Some(false)),
        ]);
        let (out, lost) = map_events(&[ev("/r", F_ROOT_CHANGED, 1), ev("/r/late", F_ITEM_CREATED, 2)], &root, &exists, &numbers);
        assert!(lost);
        assert_eq!(out, vec![Change::overflow("/r")]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn fsevents_constructor_fails_cleanly() {
        assert!(FsEventsWatcher::new(Path::new("/")).is_err());
    }
}
