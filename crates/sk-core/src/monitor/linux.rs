//! Linux watcher: inotify within a watch budget, mtime polling beyond it (docs/monitor.md §2.2).
//!
//! inotify watches one folder at a time and the kernel caps the watches per user, so a budget —
//! 80 % of that cap, shared by every watcher in this process — is handed out breadth-first: a
//! folder only gets a watch when its parent has one. Everything else is kept in sync by mtime
//! polling ([`DirPoller`]), driven from the same thread.
//!
//! Watched folders form a tree rooted at the scan root ([`WatchTree`]: wd → (parent wd, name)), so
//! renaming a watched folder re-keys one entry and every path below it follows. Folders that
//! appear later are watched as soon as their event is read and listed once, reporting `created`
//! for whatever is already inside (`mkdir` + write races).
//!
//! Decoding ([`decode_events`]) and interpretation ([`interpret`]) make no system calls (they
//! compile and are tested on every platform); the watcher itself is Linux only.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::{Change, ChangeKind};

pub const IN_ACCESS: u32 = 0x0000_0001;
pub const IN_MODIFY: u32 = 0x0000_0002;
pub const IN_ATTRIB: u32 = 0x0000_0004;
pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
pub const IN_MOVED_FROM: u32 = 0x0000_0040;
pub const IN_MOVED_TO: u32 = 0x0000_0080;
pub const IN_CREATE: u32 = 0x0000_0100;
pub const IN_DELETE: u32 = 0x0000_0200;
pub const IN_DELETE_SELF: u32 = 0x0000_0400;
pub const IN_MOVE_SELF: u32 = 0x0000_0800;
pub const IN_UNMOUNT: u32 = 0x0000_2000;
pub const IN_Q_OVERFLOW: u32 = 0x0000_4000;
pub const IN_IGNORED: u32 = 0x0000_8000;
pub const IN_ONLYDIR: u32 = 0x0100_0000;
pub const IN_DONT_FOLLOW: u32 = 0x0200_0000;
pub const IN_ISDIR: u32 = 0x4000_0000;

const EVENTS: u32 = IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO | IN_CLOSE_WRITE | IN_ATTRIB | IN_DELETE_SELF | IN_MOVE_SELF;
pub const WATCH_MASK: u32 = EVENTS | IN_ONLYDIR | IN_DONT_FOLLOW;
/// the scan root itself may be a symbolic link to a folder
pub const ROOT_MASK: u32 = EVENTS | IN_ONLYDIR;
const SELF: u32 = IN_DELETE_SELF | IN_MOVE_SELF | IN_UNMOUNT | IN_IGNORED;
const HEADER: usize = 16; // struct inotify_event: wd, mask, cookie, len
const MAX_DEPTH: usize = 4096;
pub const DEFAULT_MAX_USER_WATCHES: usize = 8192;

/// One decoded `struct inotify_event`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub wd: i32,
    pub mask: u32,
    pub cookie: u32,
    pub name: Vec<u8>,
}

impl RawEvent {
    pub fn new(wd: i32, mask: u32, cookie: u32, name: &[u8]) -> RawEvent {
        RawEvent { wd, mask, cookie, name: name.to_vec() }
    }
}

/// Splits what `read()` returned from an inotify descriptor into events (names without padding).
pub fn decode_events(buf: &[u8]) -> Vec<RawEvent> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + HEADER <= buf.len() {
        let wd = i32::from_ne_bytes(buf[pos..pos + 4].try_into().unwrap());
        let mask = u32::from_ne_bytes(buf[pos + 4..pos + 8].try_into().unwrap());
        let cookie = u32::from_ne_bytes(buf[pos + 8..pos + 12].try_into().unwrap());
        let len = u32::from_ne_bytes(buf[pos + 12..pos + 16].try_into().unwrap()) as usize;
        pos += HEADER;
        if pos + len > buf.len() {
            break; // the kernel never splits an event; a truncated buffer ends here
        }
        let raw = &buf[pos..pos + len];
        let name = raw.split(|&b| b == 0).next().unwrap_or(&[]);
        pos += len;
        out.push(RawEvent { wd, mask, cookie, name: name.to_vec() });
    }
    out
}

/// Encodes events as the kernel writes them (tests).
pub fn encode_events(events: &[RawEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in events {
        let padded = if e.name.is_empty() { 0 } else { (e.name.len() + 1).div_ceil(16) * 16 };
        out.extend_from_slice(&e.wd.to_ne_bytes());
        out.extend_from_slice(&e.mask.to_ne_bytes());
        out.extend_from_slice(&e.cookie.to_ne_bytes());
        out.extend_from_slice(&(padded as u32).to_ne_bytes());
        out.extend_from_slice(&e.name);
        out.resize(out.len() + padded - e.name.len(), 0);
    }
    out
}

pub(crate) fn path_of(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn bytes_of(p: &std::path::Path) -> Vec<u8> {
    p.as_os_str().as_encoded_bytes().to_vec()
}

fn join(folder: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = folder.to_vec();
    if !out.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

/// Watched folders as a tree: wd → (parent wd, name). Every folder but the root needs its parent
/// watched, so a path is always the root plus a chain of names.
#[derive(Clone, Debug)]
pub struct WatchTree {
    pub root: Vec<u8>,
    pub root_wd: i32,
    node: HashMap<i32, (i32, Vec<u8>)>,
    child: HashMap<(i32, Vec<u8>), i32>,
    order: Vec<i32>,
}

impl WatchTree {
    pub fn new(root: &[u8]) -> WatchTree {
        WatchTree { root: root.to_vec(), root_wd: -1, node: HashMap::new(), child: HashMap::new(), order: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.node.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node.is_empty()
    }

    pub fn contains(&self, wd: i32) -> bool {
        self.node.contains_key(&wd)
    }

    pub fn set_root(&mut self, wd: i32) {
        self.root_wd = wd;
        self.node.insert(wd, (-1, Vec::new()));
        self.order.push(wd);
    }

    pub fn add(&mut self, wd: i32, parent: i32, name: &[u8]) {
        self.node.insert(wd, (parent, name.to_vec()));
        self.child.insert((parent, name.to_vec()), wd);
        self.order.push(wd);
    }

    pub fn lookup(&self, parent: i32, name: &[u8]) -> Option<i32> {
        self.child.get(&(parent, name.to_vec())).copied()
    }

    pub fn remove(&mut self, wd: i32) {
        if let Some(key) = self.node.remove(&wd)
            && self.child.get(&key) == Some(&wd)
        {
            self.child.remove(&key);
        }
    }

    pub fn move_to(&mut self, wd: i32, parent: i32, name: &[u8]) {
        self.remove(wd);
        self.add(wd, parent, name);
    }

    /// Every watch, in the order the folders were added (shallow first).
    pub fn wds(&mut self) -> Vec<i32> {
        let node = &self.node;
        let mut seen = HashSet::new();
        self.order.retain(|w| node.contains_key(w) && seen.insert(*w));
        self.order.clone()
    }

    /// Absolute path of a watched folder (None: unknown, or cut off from the root).
    pub fn path(&self, wd: i32) -> Option<Vec<u8>> {
        let mut names: Vec<&[u8]> = Vec::new();
        let mut cur = wd;
        while cur != self.root_wd {
            let (p, name) = self.node.get(&cur)?;
            if names.len() > MAX_DEPTH {
                return None;
            }
            names.push(name);
            cur = *p;
        }
        if !self.node.contains_key(&cur) {
            return None;
        }
        let mut out = self.root.clone();
        if names.is_empty() {
            return Some(out);
        }
        while out.ends_with(b"/") {
            out.pop();
        }
        for n in names.iter().rev() {
            out.push(b'/');
            out.extend_from_slice(n);
        }
        Some(out)
    }

    /// The watch of the folder at `path` (None: not watched).
    pub fn find(&self, path: &[u8]) -> Option<i32> {
        if !self.node.contains_key(&self.root_wd) {
            return None;
        }
        if path == self.root.as_slice() {
            return Some(self.root_wd);
        }
        let prefix = if self.root.ends_with(b"/") { self.root.clone() } else { join(&self.root, b"") };
        let rest = path.strip_prefix(prefix.as_slice())?;
        let mut wd = self.root_wd;
        for name in rest.split(|&b| b == b'/') {
            wd = *self.child.get(&(wd, name.to_vec()))?;
        }
        Some(wd)
    }

    /// Watches whose chain of parents no longer reaches the root.
    pub fn orphans(&self) -> Vec<i32> {
        let mut reach: HashMap<i32, bool> = HashMap::new();
        reach.insert(self.root_wd, self.node.contains_key(&self.root_wd));
        let mut out = Vec::new();
        for &start in self.node.keys() {
            let mut chain = Vec::new();
            let mut wd = start;
            while !reach.contains_key(&wd) {
                match self.node.get(&wd) {
                    Some((p, _)) if chain.len() <= MAX_DEPTH => {
                        chain.push(wd);
                        wd = *p;
                    }
                    _ => {
                        reach.insert(wd, false);
                        break;
                    }
                }
            }
            let ok = reach[&wd];
            for w in chain {
                reach.insert(w, ok);
            }
            if !ok {
                out.push(start);
            }
        }
        out.sort_unstable();
        out
    }
}

/// What a batch of events means ([`interpret`]); the watcher carries out the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interpreted {
    pub changes: Vec<Change>,
    /// files to stat, deduplicated (last event wins)
    pub modified: Vec<PathBuf>,
    /// folders that appeared: watch and list them
    pub new_dirs: Vec<PathBuf>,
    /// watches cut from the tree: remove (with orphans)
    pub detached: Vec<i32>,
    /// (old, new): polled folders follow
    pub moved_dirs: Vec<(PathBuf, PathBuf)>,
    pub overflows: usize,
    /// the kernel dropped events: the tree may hold stale watches
    pub queue_overflow: bool,
    pub root_lost: bool,
}

/// Turns decoded events into changes, updating `tree` for folders renamed or gone (no I/O).
///
/// `IN_MOVED_FROM` / `IN_MOVED_TO` with the same cookie in the batch become one `renamed`; an
/// unpaired `IN_MOVED_FROM` is `deleted`, an unpaired `IN_MOVED_TO` `created`. `modified` changes
/// are left to the watcher (they need a stat).
pub fn interpret(events: &[RawEvent], tree: &mut WatchTree) -> Interpreted {
    let mut out = Interpreted::default();
    let mut modified: Vec<Vec<u8>> = Vec::new();
    let mut moved_to: HashMap<u32, usize> = HashMap::new();
    for (i, ev) in events.iter().enumerate() {
        if ev.mask & IN_MOVED_TO != 0 && ev.cookie != 0 {
            moved_to.entry(ev.cookie).or_insert(i);
        }
    }
    let root_path = path_of(&tree.root);
    let mut paired: HashSet<usize> = HashSet::new();
    for (i, ev) in events.iter().enumerate() {
        let mask = ev.mask;
        if mask & IN_Q_OVERFLOW != 0 {
            out.changes.push(Change::overflow(root_path.clone()));
            out.overflows += 1;
            out.queue_overflow = true;
            continue;
        }
        if paired.contains(&i) || !tree.contains(ev.wd) {
            continue;
        }
        if mask & SELF != 0 {
            if ev.wd == tree.root_wd {
                out.changes.push(Change::overflow(root_path.clone()));
                out.overflows += 1;
                out.root_lost = true;
                break;
            }
            if mask & IN_DELETE_SELF != 0 {
                tree.remove(ev.wd); // the parent's IN_DELETE reports it
            } else if mask & (IN_UNMOUNT | IN_IGNORED) != 0 {
                // the watch went away under a folder still in the tree
                let lost = tree.path(ev.wd);
                tree.remove(ev.wd);
                out.detached.push(ev.wd);
                if let Some(lost) = lost {
                    out.changes.push(Change::overflow(path_of(&lost)));
                    out.overflows += 1;
                }
            }
            continue; // IN_MOVE_SELF: the parent's IN_MOVED_FROM / IN_MOVED_TO carry the move
        }
        let Some(folder) = tree.path(ev.wd) else { continue };
        if ev.name.is_empty() {
            continue; // about the folder itself (IN_ATTRIB)
        }
        let path = join(&folder, &ev.name);
        let is_dir = mask & IN_ISDIR != 0;
        if mask & IN_MOVED_FROM != 0 {
            modified.retain(|m| *m != path);
            let j = if ev.cookie != 0 { moved_to.get(&ev.cookie).copied() } else { None };
            let dst = j.filter(|&j| j > i).map(|j| (j, &events[j]));
            let dst_folder = dst.and_then(|(_, d)| tree.path(d.wd));
            let child = if is_dir { tree.lookup(ev.wd, &ev.name) } else { None };
            if let (Some((j, dst)), Some(dst_folder)) = (dst, dst_folder) {
                paired.insert(j);
                let new = join(&dst_folder, &dst.name);
                out.changes.push(Change::renamed(path_of(&new), Some(path_of(&path)), Some(is_dir)));
                if is_dir {
                    if let Some(victim) = tree.lookup(dst.wd, &dst.name)
                        && Some(victim) != child
                    {
                        tree.remove(victim); // an empty folder replaced by the move
                        out.detached.push(victim);
                    }
                    if let Some(c) = child {
                        tree.move_to(c, dst.wd, &dst.name);
                    }
                    out.moved_dirs.push((path_of(&path), path_of(&new)));
                }
            } else {
                out.changes.push(Change::new(ChangeKind::Deleted, path_of(&path), Some(is_dir)));
                if let Some(c) = child {
                    tree.remove(c);
                    out.detached.push(c);
                }
            }
        } else if mask & IN_MOVED_TO != 0 {
            out.changes.push(Change::new(ChangeKind::Created, path_of(&path), Some(is_dir)));
            if is_dir {
                if let Some(victim) = tree.lookup(ev.wd, &ev.name) {
                    tree.remove(victim);
                    out.detached.push(victim);
                }
                out.new_dirs.push(path_of(&path));
            }
        } else if mask & IN_CREATE != 0 {
            out.changes.push(Change::new(ChangeKind::Created, path_of(&path), Some(is_dir)));
            if is_dir {
                out.new_dirs.push(path_of(&path));
            }
        } else if mask & IN_DELETE != 0 {
            modified.retain(|m| *m != path);
            out.changes.push(Change::new(ChangeKind::Deleted, path_of(&path), Some(is_dir)));
            if is_dir && let Some(c) = tree.lookup(ev.wd, &ev.name) {
                tree.remove(c); // rmdir: the kernel drops that (childless) watch itself
            }
        } else if mask & (IN_CLOSE_WRITE | IN_ATTRIB) != 0 && !is_dir {
            modified.retain(|m| *m != path);
            modified.push(path);
        }
    }
    out.modified = modified.iter().map(|m| path_of(m)).collect();
    out
}

/// A batch ending in an `IN_MOVED_FROM` whose `IN_MOVED_TO` has not arrived yet.
pub fn dangling_moves(events: &[RawEvent]) -> bool {
    let froms: HashSet<u32> = events.iter().filter(|e| e.mask & IN_MOVED_FROM != 0 && e.cookie != 0).map(|e| e.cookie).collect();
    if froms.is_empty() {
        return false;
    }
    let tos: HashSet<u32> = events.iter().filter(|e| e.mask & IN_MOVED_TO != 0).map(|e| e.cookie).collect();
    !froms.is_subset(&tos)
}

/// The per-user inotify watch cap (`DEFAULT_MAX_USER_WATCHES` when it cannot be read).
pub fn max_user_watches() -> usize {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .map_or(DEFAULT_MAX_USER_WATCHES, |n| n.max(1))
}

#[cfg(target_os = "linux")]
pub use imp::InotifyWatcher;

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::VecDeque;
    use std::ffi::CString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::super::poll::{DirPoller, Emitter, Worker, stat_file};
    use super::super::{Change, ChangeKind, Sink, WatchState, WatchStatus, Watcher, t};
    use super::*;

    const READ_LIMIT: usize = 1 << 20;
    const PAIR_WAIT_MS: i32 = 2;
    const ADD_CHUNK: usize = 2048;
    const WALK_CHUNK: usize = 32;
    const BUDGET_SHARE: f64 = 0.8;

    /// Watches held by every watcher of this process (the kernel cap is per user).
    static BUDGET: AtomicUsize = AtomicUsize::new(0);

    fn take_budget(limit: usize) -> bool {
        BUDGET
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |u| (u < limit).then_some(u + 1))
            .is_ok()
    }

    fn give_budget(n: usize) {
        let _ = BUDGET.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |u| Some(u.saturating_sub(n)));
    }

    fn why(err: &std::io::Error) -> String {
        match err.raw_os_error() {
            Some(libc::EMFILE) => t("inotify 实例数已达上限（fs.inotify.max_user_instances）"),
            Some(libc::ENFILE) => t("系统打开的文件过多"),
            Some(libc::ENOSYS) => t("内核不支持"),
            Some(libc::EACCES) | Some(libc::EPERM) => t("没有权限"),
            Some(libc::ENOMEM) => t("内存不足"),
            _ => crate::volume::os_message(err),
        }
    }

    fn cpath(p: &[u8]) -> CString {
        CString::new(p.to_vec()).unwrap_or_default()
    }

    struct Inotify {
        fd: i32,
    }

    impl Inotify {
        fn open() -> std::io::Result<Inotify> {
            // SAFETY: plain syscall.
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if fd < 0 { Err(std::io::Error::last_os_error()) } else { Ok(Inotify { fd }) }
        }
        fn add_watch(&self, path: &[u8], mask: u32) -> std::io::Result<i32> {
            let c = cpath(path);
            // SAFETY: NUL-terminated path.
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), mask) };
            if wd < 0 { Err(std::io::Error::last_os_error()) } else { Ok(wd) }
        }
        fn rm_watch(&self, wd: i32) {
            // SAFETY: EINVAL when the kernel already dropped it: nothing to do.
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
        fn read(&self, limit: usize) -> Vec<u8> {
            let mut out = Vec::new();
            let mut buf = vec![0u8; 65536];
            while out.len() < limit {
                // SAFETY: buffer of the stated size.
                let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
            }
            out
        }
    }

    impl Drop for Inotify {
        fn drop(&mut self) {
            // SAFETY: our descriptor (drops every watch of this instance).
            unsafe { libc::close(self.fd) };
        }
    }

    fn wait_readable(fds: &[i32], timeout_ms: i32) -> Vec<i32> {
        let mut p: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
        // SAFETY: valid pollfd array.
        let r = unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, timeout_ms) };
        if r <= 0 {
            return Vec::new();
        }
        p.iter().filter(|x| x.revents & libc::POLLIN != 0).map(|x| x.fd).collect()
    }

    struct State {
        tree: WatchTree,
        poller: DirPoller,
        walk: VecDeque<PathBuf>,
        recheck: VecDeque<i32>,
        ino: Option<Inotify>,
        fallback: String,
        short: bool,
        kernel_full: bool,
        held: usize,
        parent_hint: Option<(Vec<u8>, i32)>,
    }

    struct Shared {
        root: PathBuf,
        prefix: Vec<u8>,
        max_watches: Option<usize>,
        limit: usize,
        st: Mutex<State>,
        incoming: Mutex<VecDeque<PathBuf>>,
        wake_w: AtomicI64,
        emitter: Emitter,
        state: Mutex<(WatchState, String)>,
        stopped: AtomicBool,
    }

    /// One scan root: inotify for the folders within the budget, mtime polling for the rest.
    /// `max_watches` caps this watcher's own watches (tests); the process-wide budget applies.
    pub struct InotifyWatcher {
        sh: Arc<Shared>,
        worker: Worker,
    }

    impl InotifyWatcher {
        pub fn new(root: &Path, poll_interval: f64, poll_rate: u32, max_watches: Option<usize>) -> InotifyWatcher {
            let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
            let rb = bytes_of(&root);
            let prefix = if rb.ends_with(b"/") { rb.clone() } else { [rb.as_slice(), b"/"].concat() };
            InotifyWatcher {
                sh: Arc::new(Shared {
                    prefix,
                    max_watches,
                    limit: (max_user_watches() as f64 * BUDGET_SHARE) as usize,
                    st: Mutex::new(State {
                        tree: WatchTree::new(&rb),
                        poller: DirPoller::new(&root, Duration::from_secs_f64(poll_interval.max(0.01)), poll_rate),
                        walk: VecDeque::new(),
                        recheck: VecDeque::new(),
                        ino: None,
                        fallback: String::new(),
                        short: false,
                        kernel_full: false,
                        held: 0,
                        parent_hint: None,
                    }),
                    root,
                    incoming: Mutex::new(VecDeque::new()),
                    wake_w: AtomicI64::new(-1),
                    emitter: Emitter::new(),
                    state: Mutex::new((WatchState::Starting, String::new())),
                    stopped: AtomicBool::new(false),
                }),
                worker: Worker::new(),
            }
        }

        fn wake(sh: &Shared) {
            let fd = sh.wake_w.load(Ordering::SeqCst);
            if fd >= 0 {
                // SAFETY: a pipe we own; a full pipe means the thread is awake anyway.
                unsafe { libc::write(fd as i32, b"\0".as_ptr().cast(), 1) };
            }
        }
    }

    impl State {
        fn reserve(&mut self, sh: &Shared) -> bool {
            if self.kernel_full || sh.max_watches.is_some_and(|m| self.held >= m) {
                return false;
            }
            if !take_budget(sh.limit) {
                return false;
            }
            self.held += 1;
            true
        }

        fn release(&mut self, n: usize) {
            give_budget(n);
            self.held -= n.min(self.held);
        }

        fn sync_budget(&mut self) {
            let extra = self.held.saturating_sub(self.tree.len());
            if extra > 0 {
                self.release(extra);
                self.kernel_full = false; // a watch was freed: worth trying again
            }
        }

        fn watch_root(&mut self, sh: &Shared) {
            if !self.reserve(sh) {
                self.short = true;
                return;
            }
            let root = bytes_of(&sh.root);
            match self.ino.as_ref().unwrap().add_watch(&root, ROOT_MASK) {
                Ok(wd) => self.tree.set_root(wd),
                Err(e) => {
                    self.release(1);
                    if e.raw_os_error() == Some(libc::ENOSPC) {
                        self.kernel_full = true;
                        self.short = true;
                    } else {
                        self.fallback = crate::tr!("无法监视扫描根目录（{reason}），全部文件夹改为轮询", reason = why(&e));
                    }
                    tracing::warn!("{}", crate::tr!("inotify 无法监视 {path}：{error}", path = sh.root.display().to_string(), error = e.to_string()));
                }
            }
        }

        /// Watch a folder whose parent is watched; poll it when that is not possible (None).
        fn watch(&mut self, sh: &Shared, path: &Path, parent: i32, name: &[u8]) -> Option<i32> {
            if self.ino.is_none() || !self.reserve(sh) {
                self.short = self.short || self.ino.is_some();
                self.poller.add(path.to_path_buf());
                return None;
            }
            match self.ino.as_ref().unwrap().add_watch(&bytes_of(path), WATCH_MASK) {
                Err(e) => {
                    self.release(1);
                    match e.raw_os_error() {
                        Some(libc::ENOENT) | Some(libc::ENOTDIR) | Some(libc::ELOOP) => return None,
                        Some(libc::ENOSPC) => {
                            self.kernel_full = true;
                            self.short = true;
                        }
                        _ => tracing::debug!("inotify_add_watch({}) failed: {e}", path.display()),
                    }
                    self.poller.add(path.to_path_buf());
                    None
                }
                Ok(wd) if self.tree.contains(wd) => {
                    // the same folder under another path (bind mount): one path per watch
                    self.release(1);
                    self.poller.add(path.to_path_buf());
                    None
                }
                Ok(wd) => {
                    self.tree.add(wd, parent, name);
                    Some(wd)
                }
            }
        }

        fn split(path: &[u8]) -> (Vec<u8>, Vec<u8>) {
            match path.iter().rposition(|&b| b == b'/') {
                Some(i) => {
                    let parent = if i == 0 { b"/".to_vec() } else { path[..i].to_vec() };
                    (parent, path[i + 1..].to_vec())
                }
                None => (b"/".to_vec(), path.to_vec()),
            }
        }

        /// A folder from add_dirs: watch it (parent watched, budget left) or poll it.
        fn place(&mut self, sh: &Shared, path: PathBuf) {
            let pb = bytes_of(&path);
            if path == sh.root || !pb.starts_with(&sh.prefix) {
                return;
            }
            let (parent_path, name) = Self::split(&pb);
            if name.is_empty() {
                return;
            }
            let hinted = self.parent_hint.as_ref().filter(|(p, _)| *p == parent_path).map(|(_, wd)| *wd);
            let parent = match hinted {
                Some(wd) => Some(wd),
                None => {
                    let found = self.tree.find(&parent_path);
                    if let Some(wd) = found {
                        self.parent_hint = Some((parent_path.clone(), wd));
                    }
                    found
                }
            };
            let Some(parent) = parent else {
                self.poller.add(path); // the parent is polled (or unwatchable): so is this one
                return;
            };
            if self.tree.lookup(parent, &name).is_none() && !self.poller.contains(&path) {
                self.watch(sh, &path, parent, &name);
            }
        }

        /// Watch folders that appeared and report what is already inside them.
        fn walk_some(&mut self, sh: &Shared) -> Vec<Change> {
            let mut changes = Vec::new();
            let mut done = 0;
            while done < WALK_CHUNK {
                let Some(path) = self.walk.pop_front() else { break };
                let pb = bytes_of(&path);
                let (parent_path, name) = Self::split(&pb);
                let Some(parent) = self.tree.find(&parent_path) else { continue };
                if self.tree.lookup(parent, &name).is_some() || self.poller.contains(&path) {
                    continue;
                }
                done += 1;
                if self.watch(sh, &path, parent, &name).is_none() {
                    continue;
                }
                let Ok(rd) = std::fs::read_dir(&path) else { continue };
                for entry in rd.flatten() {
                    let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                    changes.push(Change::new(ChangeKind::Created, entry.path(), Some(is_dir)));
                    if is_dir {
                        self.walk.push_back(entry.path());
                    }
                }
            }
            changes
        }

        /// After a queue overflow: drop watches whose path now names another folder (or none).
        /// True when the root itself is no longer the folder being watched.
        fn recheck_some(&mut self, sh: &Shared) -> (bool, Vec<Change>) {
            let mut stale = false;
            let ino = self.ino.as_ref().unwrap();
            for _ in 0..self.recheck.len().min(ADD_CHUNK) {
                let wd = self.recheck.pop_front().unwrap();
                let Some(path) = self.tree.path(wd).filter(|_| self.tree.contains(wd)) else { continue };
                let root = wd == self.tree.root_wd;
                let got = ino.add_watch(&path, if root { ROOT_MASK } else { WATCH_MASK }).unwrap_or(-1);
                if got == wd {
                    continue;
                }
                if root {
                    *sh.state.lock().unwrap() = (WatchState::Failed, t("扫描根目录已被删除或移走，实时监控已停止"));
                    return (true, vec![Change::overflow(sh.root.clone())]);
                }
                stale = true;
                self.tree.remove(wd);
                ino.rm_watch(wd);
                if got >= 0 {
                    if !self.tree.contains(got) {
                        ino.rm_watch(got); // the walk adds it again and reports what is inside
                    }
                    self.walk.push_back(path_of(&path));
                }
            }
            if stale {
                self.parent_hint = None;
                for wd in self.tree.orphans() {
                    self.tree.remove(wd);
                    self.ino.as_ref().unwrap().rm_watch(wd);
                }
                self.sync_budget();
            }
            (false, Vec::new())
        }

        /// Read and apply one batch of events; true when the root is gone.
        fn handle(&mut self, sh: &Shared) -> (bool, Vec<Change>) {
            let ino = self.ino.as_ref().unwrap();
            let mut events = decode_events(&ino.read(READ_LIMIT));
            if dangling_moves(&events) && wait_readable(&[ino.fd], PAIR_WAIT_MS).contains(&ino.fd) {
                events.extend(decode_events(&ino.read(READ_LIMIT)));
            }
            let res = interpret(&events, &mut self.tree);
            self.parent_hint = None;
            if res.queue_overflow {
                self.recheck = self.tree.wds().into();
            }
            for &wd in &res.detached {
                self.ino.as_ref().unwrap().rm_watch(wd);
            }
            if !res.detached.is_empty() {
                for wd in self.tree.orphans() {
                    self.tree.remove(wd);
                    self.ino.as_ref().unwrap().rm_watch(wd);
                }
            }
            self.sync_budget();
            for (old, new) in &res.moved_dirs {
                self.poller.rename_tree(old, new);
            }
            self.walk.extend(res.new_dirs);
            let mut changes = res.changes;
            for path in res.modified {
                // vanished meanwhile: its deletion is (or will be) reported
                if let Some((size, alloc, mtime)) = stat_file(&path) {
                    changes.push(Change::modified(path, size, Some(alloc), mtime));
                }
            }
            if res.root_lost {
                *sh.state.lock().unwrap() = (WatchState::Failed, t("扫描根目录已被删除或移走，实时监控已停止"));
            }
            (res.root_lost, changes)
        }
    }

    fn run(sh: Arc<Shared>, wake_r: i32) {
        let ino_fd = sh.st.lock().unwrap().ino.as_ref().map(|i| i.fd);
        let fds: Vec<i32> = std::iter::once(wake_r).chain(ino_fd).collect();
        while !sh.emitter.stopping.load(Ordering::SeqCst) {
            let mut emit = Vec::new();
            let timeout_ms;
            {
                let mut st = sh.st.lock().unwrap();
                let incoming: Vec<PathBuf> = {
                    let mut inc = sh.incoming.lock().unwrap();
                    let n = inc.len().min(ADD_CHUNK);
                    inc.drain(..n).collect()
                };
                for p in incoming {
                    st.place(&sh, p);
                }
                if st.ino.is_some() && !st.recheck.is_empty() {
                    let (lost, ch) = st.recheck_some(&sh);
                    if lost {
                        drop(st);
                        sh.emitter.emit(ch);
                        return;
                    }
                }
                emit.extend(st.walk_some(&sh));
                emit.extend(st.poller.step(Instant::now()));
                if st.poller.root_lost {
                    *sh.state.lock().unwrap() = (WatchState::Failed, t("扫描根目录已不存在，实时监控已停止"));
                    drop(st);
                    sh.emitter.emit(emit);
                    return;
                }
                timeout_ms = if !sh.incoming.lock().unwrap().is_empty() || !st.walk.is_empty() || !st.recheck.is_empty() {
                    0
                } else {
                    st.poller
                        .wake_time()
                        .map_or(1000, |w| w.saturating_duration_since(Instant::now()).as_millis().min(1000) as i32)
                };
            }
            sh.emitter.emit(emit);
            let ready = wait_readable(&fds, timeout_ms);
            if ready.contains(&wake_r) {
                let mut b = [0u8; 4096];
                // SAFETY: draining our non-blocking pipe.
                while unsafe { libc::read(wake_r, b.as_mut_ptr().cast(), b.len()) } > 0 {}
            }
            if let Some(fd) = ino_fd
                && ready.contains(&fd)
            {
                let (lost, changes) = sh.st.lock().unwrap().handle(&sh);
                sh.emitter.emit(changes);
                if lost {
                    return;
                }
            }
        }
    }

    impl Watcher for InotifyWatcher {
        fn name(&self) -> &'static str {
            "inotify"
        }

        fn start(&self, sink: Sink) -> std::io::Result<()> {
            if self.worker.started() {
                return Err(std::io::Error::other(t("监视器已经启动")));
            }
            *self.sh.emitter.sink.lock().unwrap() = Some(sink);
            let mut fds = [0i32; 2];
            // SAFETY: plain syscall.
            if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            self.sh.wake_w.store(fds[1] as i64, Ordering::SeqCst);
            {
                let mut st = self.sh.st.lock().unwrap();
                match Inotify::open() {
                    Ok(i) => st.ino = Some(i),
                    Err(e) => {
                        st.fallback = crate::tr!("inotify 不可用（{reason}），全部文件夹改为轮询", reason = why(&e));
                        tracing::warn!("{}", crate::tr!("inotify 不可用（{error}）：{path} 改为轮询", error = e.to_string(), path = self.sh.root.display().to_string()));
                    }
                }
                if st.ino.is_some() {
                    st.watch_root(&self.sh);
                }
                let root_wd = st.tree.root_wd;
                if !st.tree.contains(root_wd) {
                    let r = self.sh.root.clone();
                    st.poller.add(r);
                }
            }
            *self.sh.state.lock().unwrap() = (WatchState::Running, String::new());
            let sh = self.sh.clone();
            let wake_r = fds[0];
            self.worker.spawn("sk-inotify", move || {
                let body = std::panic::AssertUnwindSafe(|| run(sh.clone(), wake_r));
                if std::panic::catch_unwind(body).is_err() {
                    tracing::error!("{}", crate::tr!("inotify 监视线程出错：{path}", path = sh.root.display().to_string()));
                    *sh.state.lock().unwrap() = (WatchState::Failed, t("实时监控出错，已停止"));
                    sh.emitter.emit(vec![Change::overflow(sh.root.clone())]);
                }
                // close everything this watcher holds
                let w = sh.wake_w.swap(-1, Ordering::SeqCst);
                // SAFETY: our pipe ends.
                unsafe {
                    libc::close(wake_r);
                    if w >= 0 {
                        libc::close(w as i32);
                    }
                }
                let mut st = sh.st.lock().unwrap();
                st.ino = None;
                st.tree = WatchTree::new(&bytes_of(&sh.root));
                let held = st.held;
                st.release(held);
            })
        }

        fn add_dirs(&self, dirs: Vec<PathBuf>) {
            self.sh.incoming.lock().unwrap().extend(dirs);
            InotifyWatcher::wake(&self.sh);
        }

        fn stop(&self) {
            if self.sh.stopped.swap(true, Ordering::SeqCst) {
                return;
            }
            self.sh.emitter.stopping.store(true, Ordering::SeqCst);
            InotifyWatcher::wake(&self.sh);
            self.worker.finish(Duration::from_millis(900));
            let mut s = self.sh.state.lock().unwrap();
            if s.0 != WatchState::Failed {
                s.0 = WatchState::Stopped;
            }
        }

        fn status(&self) -> WatchStatus {
            let (mut state, mut detail) = self.sh.state.lock().unwrap().clone();
            let st = self.sh.st.lock().unwrap();
            let polled = st.poller.len();
            if state == WatchState::Running {
                if !st.fallback.is_empty() {
                    (state, detail) = (WatchState::Degraded, st.fallback.clone());
                } else if polled > 0 && st.short {
                    (state, detail) = (WatchState::Degraded, crate::tr!("inotify 监视额度不足，{count} 个文件夹改为轮询", count = polled));
                } else if polled > 0 {
                    (state, detail) = (WatchState::Degraded, crate::tr!("{count} 个文件夹无法用 inotify 监视，改为轮询", count = polled));
                }
            }
            WatchStatus {
                state,
                detail,
                watched_dirs: st.tree.len() as i64,
                polled_dirs: polled as i64,
                overflows: self.sh.emitter.overflows.load(Ordering::Relaxed),
                events: self.sh.emitter.events.load(Ordering::Relaxed),
            }
        }
    }

    impl Drop for InotifyWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn tree() -> WatchTree {
        let mut t = WatchTree::new(b"/r");
        t.set_root(1);
        t.add(2, 1, b"a");
        t.add(3, 2, b"b");
        t
    }

    #[test]
    fn decode_events_strips_padding() {
        let evs = vec![RawEvent::new(1, IN_CREATE, 0, b"x.txt"), RawEvent::new(2, IN_ATTRIB, 0, b"")];
        let buf = encode_events(&evs);
        assert_eq!(decode_events(&buf), evs);
        assert_eq!(decode_events(&buf[..buf.len() - 1]).len(), 1); // a truncated event ends it
    }

    #[test]
    fn queue_overflow_is_reported_for_the_root() {
        let mut t = tree();
        let r = interpret(&[RawEvent::new(-1, IN_Q_OVERFLOW, 0, b"")], &mut t);
        assert_eq!(r.changes, vec![Change::overflow(p("/r"))]);
        assert!(r.queue_overflow && r.overflows == 1);
    }

    #[test]
    fn create_delete_modify() {
        let mut t = tree();
        let r = interpret(
            &[
                RawEvent::new(2, IN_CREATE, 0, b"f"),
                RawEvent::new(3, IN_CREATE | IN_ISDIR, 0, b"d"),
                RawEvent::new(2, IN_CLOSE_WRITE, 0, b"f"),
                RawEvent::new(2, IN_ATTRIB, 0, b"g"),
                RawEvent::new(2, IN_CLOSE_WRITE, 0, b"f"),
                RawEvent::new(1, IN_DELETE, 0, b"old"),
                RawEvent::new(2, IN_ATTRIB, 0, b""),
            ],
            &mut t,
        );
        assert_eq!(r.changes, vec![
            Change::new(ChangeKind::Created, p("/r/a/f"), Some(false)),
            Change::new(ChangeKind::Created, p("/r/a/b/d"), Some(true)),
            Change::new(ChangeKind::Deleted, p("/r/old"), Some(false)),
        ]);
        assert_eq!(r.modified, vec![p("/r/a/g"), p("/r/a/f")]);
        assert_eq!(r.new_dirs, vec![p("/r/a/b/d")]);
    }

    #[test]
    fn deleted_file_is_not_stat_ed() {
        let mut t = tree();
        let r = interpret(&[RawEvent::new(2, IN_CLOSE_WRITE, 0, b"f"), RawEvent::new(2, IN_DELETE, 0, b"f")], &mut t);
        assert!(r.modified.is_empty());
    }

    #[test]
    fn rename_pairing_by_cookie() {
        let mut t = tree();
        let r = interpret(
            &[
                RawEvent::new(2, IN_MOVED_FROM, 7, b"x"),
                RawEvent::new(1, IN_MOVED_TO, 7, b"y"),
                RawEvent::new(2, IN_MOVED_FROM, 8, b"gone"),
                RawEvent::new(1, IN_MOVED_TO, 9, b"came"),
            ],
            &mut t,
        );
        assert_eq!(r.changes, vec![
            Change::renamed(p("/r/y"), Some(p("/r/a/x")), Some(false)),
            Change::new(ChangeKind::Deleted, p("/r/a/gone"), Some(false)),
            Change::new(ChangeKind::Created, p("/r/came"), Some(false)),
        ]);
        assert!(dangling_moves(&[RawEvent::new(2, IN_MOVED_FROM, 5, b"x")]));
        assert!(!dangling_moves(&[RawEvent::new(2, IN_MOVED_FROM, 5, b"x"), RawEvent::new(2, IN_MOVED_TO, 5, b"y")]));
    }

    #[test]
    fn folder_rename_rekeys_the_subtree() {
        let mut t = tree();
        let r = interpret(&[RawEvent::new(1, IN_MOVED_FROM | IN_ISDIR, 3, b"a"), RawEvent::new(1, IN_MOVED_TO | IN_ISDIR, 3, b"z")], &mut t);
        assert_eq!(r.changes, vec![Change::renamed(p("/r/z"), Some(p("/r/a")), Some(true))]);
        assert_eq!(r.moved_dirs, vec![(p("/r/a"), p("/r/z"))]);
        assert_eq!(t.path(3).unwrap(), b"/r/z/b");
        assert_eq!(t.find(b"/r/z/b"), Some(3));
        assert_eq!(t.find(b"/r/a/b"), None);
    }

    #[test]
    fn folder_moved_out_detaches_its_watches() {
        let mut t = tree();
        let r = interpret(&[RawEvent::new(1, IN_MOVED_FROM | IN_ISDIR, 4, b"a")], &mut t);
        assert_eq!(r.changes, vec![Change::new(ChangeKind::Deleted, p("/r/a"), Some(true))]);
        assert_eq!(r.detached, vec![2]);
        assert_eq!(t.orphans(), vec![3]);
    }

    #[test]
    fn folder_moved_in_is_walked_and_rmdir() {
        let mut t = tree();
        let r = interpret(&[RawEvent::new(1, IN_MOVED_TO | IN_ISDIR, 4, b"new")], &mut t);
        assert_eq!(r.new_dirs, vec![p("/r/new")]);
        let r = interpret(&[RawEvent::new(2, IN_DELETE | IN_ISDIR, 0, b"b"), RawEvent::new(3, IN_DELETE_SELF, 0, b"")], &mut t);
        assert_eq!(r.changes, vec![Change::new(ChangeKind::Deleted, p("/r/a/b"), Some(true))]);
        assert!(!t.contains(3));
    }

    #[test]
    fn lost_watch_is_an_overflow_and_root_lost() {
        let mut t = tree();
        let r = interpret(&[RawEvent::new(3, IN_IGNORED, 0, b"")], &mut t);
        assert_eq!(r.changes, vec![Change::overflow(p("/r/a/b"))]);
        assert_eq!(r.detached, vec![3]);
        for mask in [IN_DELETE_SELF, IN_MOVE_SELF, IN_UNMOUNT] {
            let mut t = tree();
            let r = interpret(&[RawEvent::new(1, mask, 0, b""), RawEvent::new(2, IN_CREATE, 0, b"late")], &mut t);
            assert!(r.root_lost);
            assert_eq!(r.changes, vec![Change::overflow(p("/r"))]);
        }
    }

    #[test]
    fn watch_tree_at_the_file_system_root() {
        let mut t = WatchTree::new(b"/");
        t.set_root(1);
        t.add(2, 1, b"usr");
        t.add(3, 2, b"lib");
        assert_eq!(t.path(3).unwrap(), b"/usr/lib");
        assert_eq!(t.find(b"/usr/lib"), Some(3));
        assert_eq!(t.find(b"/"), Some(1));
        assert_eq!(t.wds(), vec![1, 2, 3]);
    }
}
