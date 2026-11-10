//! File change monitoring (port of `spaceknife.monitor` and the change pipeline of
//! `server/monitors.py`; docs/monitor.md).
//!
//! A [`Watcher`] reports *where* something changed; the backend works out *what* by listing the
//! folder again ([`pipeline::ChangePipeline`]). Only a file whose size or times changed may be
//! updated from the event's own numbers. Watchers never coalesce or drop silently: losing events
//! is reported as an [`ChangeKind::Overflow`] change.
//!
//! Watchers per platform:
//! * Windows: [`win::RdcwWatcher`], one recursive `ReadDirectoryChangesExW` per scan root;
//! * Linux: [`linux::InotifyWatcher`], inotify within a watch budget, mtime polling beyond it;
//! * macOS: [`mac::FsEventsWatcher`], one recursive FSEvents stream (untested: no Mac here);
//! * anywhere: [`poll::PollWatcher`], mtime polling of the folders handed in.

pub mod linux;
pub mod mac;
pub mod pipeline;
pub mod poll;
pub mod win;

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// [`std::sync::Mutex::lock`] that shrugs off poisoning. A watcher catches a panic of its own
/// thread (and of the sink it calls) and reports the watch as failed; a lock it held at that
/// moment must not turn every later call (`status`, `stop`, the next batch) into another panic.
pub(crate) trait LockExt<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What a condition variable's wait gives back, poisoned or not (see [`LockExt`]).
pub(crate) fn unpoison<G>(r: std::sync::LockResult<G>) -> G {
    r.unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a [`Change`] says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Modified,
    Created,
    Deleted,
    Renamed,
    /// The folder's entries changed (list it again).
    Dir,
    /// Events were lost below `path`.
    Overflow,
}

/// One change a watcher saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// absolute; renamed: the new path; dir: the folder whose entries changed; overflow: lost below here
    pub path: PathBuf,
    /// renamed: the old path (None when only the new name is known)
    pub old_path: Option<PathBuf>,
    pub is_dir: Option<bool>,
    /// modified, when the watcher knows: logical size, allocation, mtime (Unix s)
    pub size: Option<i64>,
    pub alloc: Option<i64>,
    pub mtime: Option<i64>,
}

impl Change {
    pub fn new(kind: ChangeKind, path: impl Into<PathBuf>, is_dir: Option<bool>) -> Change {
        Change {
            kind,
            path: path.into(),
            old_path: None,
            is_dir,
            size: None,
            alloc: None,
            mtime: None,
        }
    }

    pub fn overflow(path: impl Into<PathBuf>) -> Change {
        Change::new(ChangeKind::Overflow, path, Some(true))
    }

    pub fn renamed(new: impl Into<PathBuf>, old: Option<PathBuf>, is_dir: Option<bool>) -> Change {
        Change {
            old_path: old,
            ..Change::new(ChangeKind::Renamed, new, is_dir)
        }
    }

    /// A file whose numbers changed.
    pub fn modified(path: impl Into<PathBuf>, size: i64, alloc: Option<i64>, mtime: i64) -> Change {
        Change {
            size: Some(size),
            alloc,
            mtime: Some(mtime),
            ..Change::new(ChangeKind::Modified, path, Some(false))
        }
    }
}

/// A watcher's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchState {
    Starting,
    Running,
    Degraded,
    Failed,
    Stopped,
}

/// What a watcher reports about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchStatus {
    pub state: WatchState,
    /// one sentence the UI shows as is (degraded / failed reasons), in the interface language
    pub detail: String,
    /// folders under live watch; -1: the whole tree (recursive watch)
    pub watched_dirs: i64,
    /// folders kept in sync by polling
    pub polled_dirs: i64,
    pub overflows: i64,
    pub events: i64,
}

impl WatchStatus {
    pub fn new(state: WatchState, detail: impl Into<String>) -> WatchStatus {
        WatchStatus {
            state,
            detail: detail.into(),
            watched_dirs: 0,
            polled_dirs: 0,
            overflows: 0,
            events: 0,
        }
    }
}

/// Where a watcher delivers its batches (called from the watcher's own thread).
pub type Sink = Arc<dyn Fn(Vec<Change>) + Send + Sync>;

/// What every platform watcher implements.
pub trait Watcher: Send + Sync {
    /// `"rdcw"` | `"inotify"` | `"fsevents"` | `"poll"`
    fn name(&self) -> &'static str;
    /// Begin watching; `sink` is called from the watcher's thread, a batch at least every 50 ms
    /// while there are events. Fails when watching cannot even start (the caller polls instead).
    fn start(&self, sink: Sink) -> std::io::Result<()>;
    /// Folders that are now part of the scanned tree, shallow first (per-folder watchers and
    /// polling need them; recursive watchers ignore the call). Safe from any thread.
    ///
    /// `mtimes`: each folder's mtime as the scan recorded it (whole seconds, the tree's `mtime`
    /// column; same length as `dirs`); `since_ns`: a wall-clock time (ns since the Unix epoch)
    /// no later than any of those listings (the scan's start). A per-folder watcher places a
    /// folder's watch a moment after the scan found it, maybe after the scan listed it too, and a
    /// change in between raises no event: once the folder is watched (or its polling baseline
    /// taken) it is checked against the record ([`poll::listed_changed`]) and reported as
    /// [`ChangeKind::Dir`] when it may have changed. Without `mtimes` nothing is checked.
    fn add_dirs(&self, dirs: Vec<PathBuf>, mtimes: Option<Vec<u32>>, since_ns: i64);
    /// Stop within a second; `sink` is not called afterwards. Idempotent.
    fn stop(&self);
    fn status(&self) -> WatchStatus;
}

/// The best watcher this platform offers for `root` (a scan root folder). `poll_interval`:
/// seconds between polling rounds; `poll_rate`: folders stat'ed per second at most while polling.
pub fn create_watcher(root: &Path, poll_interval: f64, poll_rate: u32) -> Box<dyn Watcher> {
    #[cfg(windows)]
    {
        let _ = (poll_interval, poll_rate);
        Box::new(win::RdcwWatcher::new(root, None))
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::InotifyWatcher::new(
            root,
            poll_interval,
            poll_rate,
            None,
        ))
    }
    #[cfg(target_os = "macos")]
    {
        match mac::FsEventsWatcher::new(root) {
            Ok(w) => Box::new(w),
            Err(_) => Box::new(poll::PollWatcher::new(root, poll_interval, poll_rate)),
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Box::new(poll::PollWatcher::new(root, poll_interval, poll_rate))
    }
}

/// `msg` (a Chinese message id) in the interface language.
pub(crate) fn t(msg: &str) -> String {
    crate::i18n::t(msg).to_owned()
}

/// Seconds as Unix time, clamped to u32 like the scanner stores them.
pub(crate) fn secs(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as i64)
}
