//! Deleting a folder with everything inside, for good (docs/manage.md §2.5, permanent deletion).
//!
//! What a folder holds is never reached by joining a listed name to the folder's path and asking
//! by that path again without checking the answer, because it can be another item:
//!
//! * a listed name may hold a path separator (WinBtrfs lists a Linux file `..\x` as it is; joined
//!   to `I:\sel` that is `I:\x`), a `:` (a stream of another file, Windows) or be `.` / `..`:
//!   such names are refused (`bad_name_inside`), never joined;
//! * a folder can be replaced by a link between the moment it is listed and the moment it is
//!   entered or its entries are deleted (what CVE-2022-21658 fixed in `std::fs::remove_dir_all`):
//!   the deletion never follows a link, and checks that each entry it opens is the one listed
//!   (`moved_inside` otherwise);
//! * another file system mounted inside (on Linux also a bind mount of the same one) is not
//!   entered, and a folder mount point on Windows is not removed (`mount_inside`).
//!
//! POSIX: every entry is reached relative to its folder's open descriptor (`fstatat`, `openat`
//! with `O_NOFOLLOW`, `unlinkat`), so a folder above that is renamed or replaced changes nothing;
//! a folder opened must be the one `fstatat` saw (device and inode) and on the same mount (Linux:
//! the `statx` mount id, else `/proc/self/mountinfo`). Only the deleted folder and the
//! `OPEN_FOLDERS` deepest levels keep their descriptor open (a tree thousands of levels deep
//! would otherwise run out of them: 1024 on Linux, 256 for a macOS program); a level closed on
//! the way down is opened again on the way back, as `..` of the level below it or, failing that,
//! by the names from the nearest open level, and it must still be that folder (device and inode)
//! or it is left alone (`moved_inside`). A folder is removed from its parent by name only while
//! that name still is the folder (device and inode).
//!
//! Windows: every entry is opened by its full `\\?\` path (no Win32 rewriting of trailing dots or
//! spaces, no MAX_PATH) without following a reparse point, and checked on that handle: on the same
//! volume, and opened under the name its folder's and the listed name give (a link swapped in
//! on the way resolves to another name). A name that matches only in case (SMB redirectors,
//! WinFsp and Dokan report the case the first program to open it used) counts only for the very
//! entry the folder lists under that name: its file id (FileIdBothDirectoryInformation, listed
//! through a handle checked to be the folder) is the handle's file index; in a case-sensitive
//! folder another case is another entry. It is then deleted through that very handle (POSIX
//! semantics where the file system has them; a read-only item loses the flag first, as Explorer
//! does after asking). A folder is listed through a handle to it, checked to be the same folder.
//!
//! **Several threads at once** ([`remove_trees`], as many as the device takes:
//! [`super::par`]): every item is a task, and so is every folder of a tree whose listing an idle
//! thread can take — a folder found while another thread waits is handed over, and a folder with
//! many names left lends half of them. Each step is the same as one thread's: the checks above
//! are made on each entry as it is opened. A folder counts what is still to do in it (its own
//! names, its subfolders, the names it lent); it is removed when that reaches zero, by whichever
//! thread finished the last of it, and only then does its parent count it done: children always
//! go before their folder, and a folder's handle is closed before its parent is removed. On
//! POSIX the threads work on the top [`PAR_DEPTH`] levels; a folder below is deleted in place by
//! the thread that met it, as one thread does it (the descriptors stay few). Items nested in each
//! other (never from the basket) wait for the items inside them.
//!
//! `cancel` is polled before each item and before each folder is entered; once it said stop, no
//! thread starts anything new and no folder is removed any more.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::{Condvar, Mutex};

use super::probe::{self, Call};
use super::text::trash_text as text;
use super::trash::{CancelFn, seams};

/// What a tree deletion did.
#[derive(Debug, Default)]
pub(super) struct Removed {
    /// the first thing that failed
    pub first: Option<io::Error>,
    pub cancelled: bool,
    /// anything at all was deleted
    pub deleted: bool,
}

impl Removed {
    fn fail(&mut self, e: io::Error) {
        if self.first.is_none() {
            self.first = Some(e);
        }
    }

    /// What one thread did in place, into the item's record.
    #[cfg_attr(windows, allow(dead_code))]
    fn fail_from(&mut self, other: Removed) {
        if let Some(e) = other.first {
            self.fail(e);
        }
        self.deleted |= other.deleted;
        self.cancelled |= other.cancelled;
    }
}

pub(super) fn mount_inside() -> io::Error {
    io::Error::other(text("mount_inside"))
}

#[cfg(target_os = "linux")]
fn subvolume_inside() -> io::Error {
    io::Error::other(text("subvolume_inside"))
}

fn bad_name_inside() -> io::Error {
    io::Error::other(text("bad_name_inside"))
}

fn moved_inside() -> io::Error {
    io::Error::other(text("moved_inside"))
}

/// Something that went wrong in the deletion itself (a bug), as the item's error.
fn internal(what: &str) -> io::Error {
    io::Error::other(super::text::fill(
        text("internal"),
        &[("detail", what.into())],
    ))
}

/// What a panic said.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// POSIX: folder levels below the deleted folder that keep their descriptor open at a time.
#[cfg(unix)]
const OPEN_FOLDERS: usize = 32;

/// POSIX: the levels of a tree (its top is 0) whose folders several threads work on; a folder
/// deeper is deleted by one thread as a whole, keeping its descriptors few.
#[cfg(unix)]
pub(super) const PAR_DEPTH: usize = 4;

/// A folder with more names than this left lends half of them to a thread that has nothing to do.
const LEND_ABOVE: usize = 64;

/// [`OPEN_FOLDERS`] for one of `workers` threads (they share what one thread had), or what a test
/// set; whether a test set it.
#[cfg(unix)]
fn open_folders(workers: usize) -> (usize, bool) {
    match seams::OPEN_FOLDERS.load(Ordering::Relaxed) {
        0 => ((OPEN_FOLDERS / workers.max(1)).max(4), false),
        n => (n, true),
    }
}

/// Whether a listed name (its UTF-16 units or bytes) can be joined to its folder's path and still
/// name it: not empty, `.` or `..`, no NUL, no separator (Windows: `\`, `/` and the stream
/// separator `:`; POSIX: `/`).
fn joinable<T: Copy + Into<u32>>(name: &[T]) -> bool {
    let units: Vec<u32> = name.iter().map(|&c| c.into()).collect();
    let dot = u32::from(b'.');
    if units.is_empty() || units == [dot] || units == [dot, dot] || units.contains(&0) {
        return false;
    }
    let seps: &[u8] = if cfg!(windows) { b"\\/:" } else { b"/" };
    !units
        .iter()
        .any(|c| seps.iter().any(|&s| u32::from(s) == *c))
}

/// The names of a listing, with the test seam applied.
fn seam_names(folder: &Path, mut names: Vec<OsString>) -> Vec<OsString> {
    seams::listed(folder, &mut names);
    names
}

/// Delete the folders `tops` (real folders, not links: the caller looked), each with everything
/// in it, on `workers` threads together; `done(k, what)` as each is finished (or, after a cancel,
/// at the end for those it stopped). Each is called once.
pub(super) fn remove_trees(
    tops: &[PathBuf],
    workers: usize,
    cancel: Option<CancelFn<'_>>,
    done: &(dyn Fn(usize, Removed) + Sync),
) {
    #[cfg(unix)]
    let walk = {
        let (keep, set) = open_folders(workers);
        imp::Posix {
            keep,
            // a test that sets how many levels stay open is about one thread's way down
            par_depth: if set { 0 } else { PAR_DEPTH },
        }
    };
    #[cfg(windows)]
    let walk = imp::Win;
    Engine::new(&walk, tops, workers, cancel, done).run();
}

// --------------------------------------------------------------------------- the threads

/// One platform's way through a tree ([`Engine`] runs it on several threads).
trait Walk: Sync {
    /// A folder opened, checked and listed, being emptied.
    type Dir: Send + Sync;
    /// Per thread (Windows: its listing buffer).
    type Scratch;
    fn scratch(&self) -> Self::Scratch;
    /// The whole item at once when this walk takes no threads for it (None: [`Walk::start`]).
    fn whole(&self, _top: &Path, _cancel: &dyn Fn() -> bool) -> Option<Removed> {
        None
    }
    /// Open the item's top folder, check it is one, list it.
    fn start(&self, top: &Path, s: &mut Self::Scratch) -> io::Result<(Self::Dir, Vec<OsString>)>;
    /// One name listed in `dir` (`depth` levels below the top).
    fn visit(
        &self,
        dir: &Self::Dir,
        depth: usize,
        name: OsString,
        s: &mut Self::Scratch,
        cancel: &dyn Fn() -> bool,
    ) -> Visit<Self::Dir>;
    /// Remove `dir`, emptied: from `parent` (None: the item's top, `top`).
    fn remove(&self, dir: &Self::Dir, parent: Option<&Self::Dir>, top: &Path) -> io::Result<()>;
    /// `dir` stays (something in it could not go): let go of what it holds open.
    fn keep(&self, _dir: &Self::Dir) {}
}

/// What became of one listed name.
enum Visit<D> {
    Deleted,
    /// gone meanwhile
    Gone,
    Failed(io::Error),
    /// a folder to empty first: opened, checked, listed
    Folder(D, Vec<OsString>),
    /// a folder deleted in place, as one thread does it
    #[cfg_attr(windows, allow(dead_code))]
    Whole(Removed),
    /// cancelled before entering a folder
    Stop,
}

/// A folder being emptied; shared by the threads working in it.
struct Node<D> {
    dir: D,
    /// what is still to do in it: its own names (1), each subfolder not removed yet, each share
    /// of its names lent out
    pending: AtomicUsize,
    /// something in it stays (failed, refused, not entered): it is not asked to go, nor is any
    /// folder above it (asking would only fail, and on Windows wait for deletions still pending)
    kept: AtomicBool,
    parent: Option<Arc<Node<D>>>,
    item: usize,
    depth: usize,
}

/// A folder being worked through by one thread: its node and its names still to do (reversed).
type Frame<D> = (Arc<Node<D>>, Vec<OsString>);

enum Task<D> {
    /// start an item
    Item(usize),
    /// names of a folder (all its own, or a share lent out), in reverse: the next one last
    Names(Arc<Node<D>>, Vec<OsString>),
}

struct Engine<'a, W: Walk> {
    walk: &'a W,
    tops: &'a [PathBuf],
    workers: usize,
    cancel: Option<CancelFn<'a>>,
    done: &'a (dyn Fn(usize, Removed) + Sync),
    queue: Mutex<VecDeque<Task<W::Dir>>>,
    wake: Condvar,
    /// tasks queued or running
    outstanding: AtomicUsize,
    queued: AtomicUsize,
    /// threads waiting for a task
    idle: AtomicUsize,
    stop: AtomicBool,
    items: Vec<Mutex<Removed>>,
    finished: Vec<AtomicBool>,
    /// items inside this one (nested paths) not finished yet
    waits: Vec<AtomicUsize>,
    /// the items waiting for this one
    waiters: Vec<Vec<usize>>,
}

impl<'a, W: Walk> Engine<'a, W> {
    fn new(
        walk: &'a W,
        tops: &'a [PathBuf],
        workers: usize,
        cancel: Option<CancelFn<'a>>,
        done: &'a (dyn Fn(usize, Removed) + Sync),
    ) -> Self {
        let n = tops.len();
        // an item inside another one goes first: the nearest item above each waits for it
        let keys: Vec<String> = tops
            .iter()
            .map(|t| super::basket::path_key(&t.to_string_lossy()))
            .collect();
        let at: HashMap<&str, usize> = keys
            .iter()
            .enumerate()
            .map(|(k, s)| (s.as_str(), k))
            .collect();
        let waits: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
        let mut waiters: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (k, key) in keys.iter().enumerate() {
            let mut up = key.clone();
            loop {
                let next = parent_key(&up);
                if next == up || next.is_empty() {
                    break;
                }
                if let Some(&a) = at.get(next.as_str())
                    && a != k
                {
                    waits[a].fetch_add(1, Ordering::Relaxed);
                    waiters[k].push(a);
                    break;
                }
                up = next;
            }
        }
        Engine {
            walk,
            tops,
            workers: workers.max(1),
            cancel,
            done,
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            outstanding: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            items: (0..n).map(|_| Mutex::new(Removed::default())).collect(),
            finished: (0..n).map(|_| AtomicBool::new(false)).collect(),
            waits,
            waiters,
        }
    }

    fn run(&self) {
        for k in 0..self.tops.len() {
            if self.waits[k].load(Ordering::Relaxed) == 0 {
                self.push(Task::Item(k));
            }
        }
        if self.workers == 1 || self.tops.is_empty() {
            self.worker();
        } else {
            // the calling thread is one of them; a thread the system refuses to start leaves the
            // work to those there are
            std::thread::scope(|s| {
                for t in 1..self.workers {
                    let started = std::thread::Builder::new()
                        .name(format!("spaceknife-delete-{t}"))
                        .spawn_scoped(s, || self.worker());
                    if started.is_err() {
                        break;
                    }
                }
                self.worker();
            });
        }
        // what a cancel left: stopped where it was; else what something that went wrong in it
        // (it says what) or in an item inside it left unfinished
        let stopped = self.stop.load(Ordering::Acquire);
        for k in 0..self.tops.len() {
            if !self.finished[k].swap(true, Ordering::AcqRel) {
                let mut r = std::mem::take(&mut *self.items[k].lock());
                if stopped {
                    r.cancelled = true;
                } else if r.first.is_none() {
                    r.fail(internal("unfinished"));
                }
                (self.done)(k, r);
            }
        }
    }

    fn push(&self, t: Task<W::Dir>) {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        self.queued.fetch_add(1, Ordering::AcqRel);
        self.queue.lock().push_back(t);
        self.wake.notify_one();
    }

    /// A thread waits with nothing to do (and no task queued for it yet).
    fn someone_idle(&self) -> bool {
        self.workers > 1 && self.idle.load(Ordering::Acquire) > self.queued.load(Ordering::Acquire)
    }

    fn cancelled(&self) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return true;
        }
        if self.cancel.is_some_and(|c| c()) {
            self.stop.store(true, Ordering::Release);
            let _q = self.queue.lock();
            self.wake.notify_all();
            return true;
        }
        false
    }

    fn worker(&self) {
        let mut scratch = self.walk.scratch();
        loop {
            let task = {
                let mut q = self.queue.lock();
                loop {
                    if self.stop.load(Ordering::Acquire) {
                        self.wake.notify_all(); // the others stop too
                        return;
                    }
                    if let Some(t) = q.pop_back() {
                        self.queued.fetch_sub(1, Ordering::AcqRel);
                        break t;
                    }
                    if self.outstanding.load(Ordering::Acquire) == 0 {
                        return;
                    }
                    self.idle.fetch_add(1, Ordering::AcqRel);
                    self.wake.wait(&mut q);
                    self.idle.fetch_sub(1, Ordering::AcqRel);
                }
            };
            let item = match &task {
                Task::Item(k) => *k,
                Task::Names(node, _) => node.item,
            };
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match task {
                Task::Item(k) => self.item(k, &mut scratch),
                Task::Names(node, names) => self.names(node, names, &mut scratch),
            }));
            if let Err(panic) = ran {
                // a bug, not the file system: the item says so and the threads go on (a thread
                // gone with its task would leave the others waiting for it for ever)
                let what = panic_text(&*panic);
                tracing::error!("deleting {}: {what}", self.tops[item].display());
                self.items[item].lock().fail(internal(&what));
            }
            if self.outstanding.fetch_sub(1, Ordering::AcqRel) == 1 {
                let _q = self.queue.lock();
                self.wake.notify_all(); // all done
            }
        }
    }

    fn item(&self, k: usize, scratch: &mut W::Scratch) {
        if self.cancelled() {
            return;
        }
        let top = &self.tops[k];
        if let Some(r) = self.walk.whole(top, &|| self.cancelled()) {
            if r.cancelled {
                *self.items[k].lock() = r;
                self.stop.store(true, Ordering::Release);
                return;
            }
            *self.items[k].lock() = r;
            self.finish(k);
            return;
        }
        match self.walk.start(top, scratch) {
            Ok((dir, mut names)) => {
                names.reverse();
                let node = Arc::new(Node {
                    dir,
                    pending: AtomicUsize::new(1),
                    kept: AtomicBool::new(false),
                    parent: None,
                    item: k,
                    depth: 0,
                });
                self.names(node, names, scratch);
            }
            Err(e) => {
                self.items[k].lock().fail(e);
                self.finish(k);
            }
        }
    }

    /// Work through names of folders, depth first; folders and shares of names go to idle threads.
    fn names(&self, node: Arc<Node<W::Dir>>, names: Vec<OsString>, scratch: &mut W::Scratch) {
        let mut stack: Vec<Frame<W::Dir>> = vec![(node, names)];
        while !stack.is_empty() {
            if self.stop.load(Ordering::Acquire) {
                return; // what is left stays, its folders too
            }
            let top = stack.len() - 1;
            if stack[top].1.len() > LEND_ABOVE && self.someone_idle() {
                let (node, names) = &mut stack[top];
                let lent: Vec<OsString> = names.drain(..names.len() / 2).collect();
                node.pending.fetch_add(1, Ordering::AcqRel);
                self.push(Task::Names(node.clone(), lent));
            }
            let Some(name) = stack[top].1.pop() else {
                let (node, _) = stack.pop().unwrap();
                self.release(node);
                continue;
            };
            let node = stack[top].0.clone();
            let item = node.item;
            match self
                .walk
                .visit(&node.dir, node.depth, name, scratch, &|| self.cancelled())
            {
                Visit::Deleted => self.items[item].lock().deleted = true,
                Visit::Gone => {}
                Visit::Failed(e) => {
                    node.kept.store(true, Ordering::Release);
                    self.items[item].lock().fail(e);
                }
                Visit::Whole(r) => {
                    let stop = r.cancelled;
                    if r.first.is_some() || stop {
                        node.kept.store(true, Ordering::Release); // the folder stayed
                    }
                    self.items[item].lock().fail_from(r);
                    if stop {
                        self.stop.store(true, Ordering::Release);
                        return;
                    }
                }
                Visit::Stop => return,
                Visit::Folder(dir, mut names) => {
                    names.reverse();
                    node.pending.fetch_add(1, Ordering::AcqRel);
                    let child = Arc::new(Node {
                        dir,
                        pending: AtomicUsize::new(1),
                        kept: AtomicBool::new(false),
                        parent: Some(node.clone()),
                        item,
                        depth: node.depth + 1,
                    });
                    if self.someone_idle() {
                        self.push(Task::Names(child, names));
                    } else {
                        stack.push((child, names));
                    }
                }
            }
        }
    }

    /// One thing less to do in `node`: when nothing is left, it is removed, and its parent has
    /// one thing less (the item is finished at its top).
    fn release(&self, node: Arc<Node<W::Dir>>) {
        let mut cur = node;
        loop {
            if cur.pending.fetch_sub(1, Ordering::AcqRel) != 1 {
                return;
            }
            if self.stop.load(Ordering::Acquire) {
                return; // nothing is removed after a cancel
            }
            let item = cur.item;
            if cur.kept.load(Ordering::Acquire) {
                // what stays in it was reported; it stays too, and so does its parent
                self.walk.keep(&cur.dir);
            } else {
                let gone = self.walk.remove(
                    &cur.dir,
                    cur.parent.as_deref().map(|p| &p.dir),
                    &self.tops[item],
                );
                let mut it = self.items[item].lock();
                match gone {
                    Ok(()) => it.deleted = true,
                    Err(e) => {
                        it.fail(e);
                        cur.kept.store(true, Ordering::Release);
                    }
                }
            }
            if cur.kept.load(Ordering::Acquire)
                && let Some(p) = &cur.parent
            {
                p.kept.store(true, Ordering::Release);
            }
            match cur.parent.clone() {
                Some(p) => cur = p,
                None => {
                    drop(cur);
                    self.finish(item);
                    return;
                }
            }
        }
    }

    fn finish(&self, k: usize) {
        if self.finished[k].swap(true, Ordering::AcqRel) {
            return;
        }
        let r = std::mem::take(&mut *self.items[k].lock());
        (self.done)(k, r);
        for &a in &self.waiters[k] {
            if self.waits[a].fetch_sub(1, Ordering::AcqRel) == 1 {
                self.push(Task::Item(a));
            }
        }
    }
}

/// The folder above a path key (empty above the top).
fn parent_key(key: &str) -> String {
    let trimmed = key.trim_end_matches(['\\', '/']);
    match trimmed.rfind(['\\', '/']) {
        Some(0) => key[..1].to_string(),
        Some(i) if cfg!(windows) && i == 2 && trimmed.as_bytes().get(1) == Some(&b':') => {
            key[..3].to_string()
        }
        Some(i) => trimmed[..i].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_that_can_be_joined() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        for ok in ["a", "a b", "...", "odd.", "a\u{FFFD}"] {
            assert!(super::joinable(&w(ok)), "{ok}");
            assert!(super::joinable(ok.as_bytes()), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "../x", "a\0b"] {
            assert!(!super::joinable(&w(bad)), "{bad:?}");
            assert!(!super::joinable(bad.as_bytes()), "{bad:?}");
        }
        for name in ["..\\x", "a:b"] {
            assert_eq!(super::joinable(&w(name)), !cfg!(windows), "{name}");
        }
    }

    #[test]
    fn parent_keys() {
        if cfg!(windows) {
            assert_eq!(super::parent_key("c:\\a\\b"), "c:\\a");
            assert_eq!(super::parent_key("c:\\a"), "c:\\");
            assert_eq!(super::parent_key("c:\\"), "");
        } else {
            assert_eq!(super::parent_key("/a/b"), "/a");
            assert_eq!(super::parent_key("/a"), "/");
            assert_eq!(super::parent_key("/"), "");
        }
    }

    // ------------------------------------------------------------ the threads, on a tree in memory

    use super::{Engine, Removed, Visit, Walk};
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsString;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    struct Entry {
        name: String,
        folder: bool,
        children: Vec<usize>,
    }

    /// A tree in memory that checks what the engine does to it: no folder removed before what
    /// it holds, how many entries are worked on at once.
    #[derive(Default)]
    struct Fake {
        tree: Vec<Entry>,
        tops: HashMap<PathBuf, usize>,
        /// entries whose deletion fails (held open)
        held: HashSet<usize>,
        /// entries whose visit panics (a bug in the deletion)
        bugs: HashSet<usize>,
        /// folders asked to go while something in them was left
        asked_too_soon: AtomicUsize,
        gone: Mutex<HashSet<usize>>,
        /// removals asked for after the stop
        late: AtomicUsize,
        stopped: AtomicBool,
        wrong_order: AtomicBool,
        busy: AtomicUsize,
        most_busy: AtomicUsize,
        visits: AtomicUsize,
        /// (item, its top) in the order items started
        started: Mutex<Vec<usize>>,
        pause: Duration,
    }

    impl Fake {
        fn add(&mut self, parent: Option<usize>, name: &str, folder: bool) -> usize {
            self.tree.push(Entry {
                name: name.into(),
                folder,
                children: Vec::new(),
            });
            let k = self.tree.len() - 1;
            if let Some(p) = parent {
                self.tree[p].children.push(k);
            }
            k
        }

        /// A folder `top` with `folders` folders of `files` files each, `depth` levels down.
        fn grow(&mut self, at: usize, folders: usize, files: usize, depth: usize) {
            for f in 0..files {
                self.add(Some(at), &format!("f{f}"), false);
            }
            if depth == 0 {
                return;
            }
            for d in 0..folders {
                let sub = self.add(Some(at), &format!("d{d}"), true);
                self.grow(sub, folders, files, depth - 1);
            }
        }

        fn top(&mut self, path: &str) -> usize {
            let k = self.add(None, path, true);
            self.tops.insert(PathBuf::from(path), k);
            k
        }

        fn names(&self, k: usize) -> Vec<OsString> {
            self.tree[k]
                .children
                .iter()
                .map(|&c| OsString::from(&self.tree[c].name))
                .collect()
        }

        fn all_gone(&self, k: usize) -> bool {
            let gone = self.gone.lock().unwrap();
            fn walk(f: &Fake, k: usize, gone: &HashSet<usize>) -> bool {
                gone.contains(&k) && f.tree[k].children.iter().all(|&c| walk(f, c, gone))
            }
            walk(self, k, &gone)
        }
    }

    impl Walk for Fake {
        type Dir = usize;
        type Scratch = ();

        fn scratch(&self) {}

        fn start(&self, top: &Path, _: &mut ()) -> io::Result<(usize, Vec<OsString>)> {
            let k = self.tops[top];
            self.started.lock().unwrap().push(k);
            Ok((k, self.names(k)))
        }

        fn visit(
            &self,
            dir: &usize,
            _depth: usize,
            name: OsString,
            _: &mut (),
            cancel: &dyn Fn() -> bool,
        ) -> Visit<usize> {
            let now = self.busy.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_busy.fetch_max(now, Ordering::SeqCst);
            self.visits.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.pause);
            let child = *self.tree[*dir]
                .children
                .iter()
                .find(|&&c| OsString::from(&self.tree[c].name) == name)
                .unwrap();
            if self.bugs.contains(&child) {
                panic!("a bug at {}", self.tree[child].name);
            }
            let got = if self.tree[child].folder {
                if cancel() {
                    self.stopped.store(true, Ordering::SeqCst);
                    Visit::Stop
                } else {
                    Visit::Folder(child, self.names(child))
                }
            } else if self.held.contains(&child) {
                Visit::Failed(io::Error::other("held"))
            } else {
                self.gone.lock().unwrap().insert(child);
                Visit::Deleted
            };
            self.busy.fetch_sub(1, Ordering::SeqCst);
            got
        }

        fn remove(&self, dir: &usize, _parent: Option<&usize>, _top: &Path) -> io::Result<()> {
            if self.stopped.load(Ordering::SeqCst) {
                self.late.fetch_add(1, Ordering::SeqCst);
            }
            let mut gone = self.gone.lock().unwrap();
            let left: Vec<usize> = self.tree[*dir]
                .children
                .iter()
                .copied()
                .filter(|c| !gone.contains(c))
                .collect();
            // a folder that is still being emptied must never be asked to go
            if left
                .iter()
                .any(|&c| self.tree[c].folder && !self.held_below(c))
            {
                self.wrong_order.store(true, Ordering::SeqCst);
            }
            if !left.is_empty() {
                self.asked_too_soon.fetch_add(1, Ordering::SeqCst);
                return Err(io::Error::other("not empty"));
            }
            gone.insert(*dir);
            Ok(())
        }
    }

    impl Fake {
        /// Something below `k` cannot go (so `k` stays, rightly).
        fn held_below(&self, k: usize) -> bool {
            self.held.contains(&k) || self.tree[k].children.iter().any(|&c| self.held_below(c))
        }
    }

    fn run(
        fake: &Fake,
        tops: &[&str],
        workers: usize,
        cancel: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Vec<(usize, Removed)> {
        let tops: Vec<PathBuf> = tops.iter().map(PathBuf::from).collect();
        let done: Mutex<Vec<(usize, Removed)>> = Mutex::new(Vec::new());
        Engine::new(fake, &tops, workers, cancel, &|k, r| {
            done.lock().unwrap().push((k, r))
        })
        .run();
        done.into_inner().unwrap()
    }

    /// A wide tree on four threads: all of it goes, every folder after what it held, with several
    /// entries worked on at once; each item is reported once.
    #[test]
    fn a_tree_goes_children_first_on_several_threads() {
        let mut fake = Fake {
            pause: Duration::from_micros(200),
            ..Fake::default()
        };
        let top = fake.top("r");
        fake.grow(top, 4, 6, 3);
        let other = fake.top("q");
        fake.grow(other, 2, 3, 1);
        let done = run(&fake, &["r", "q"], 4, None);
        assert_eq!(done.len(), 2);
        for (_, r) in &done {
            assert!(r.first.is_none() && r.deleted && !r.cancelled, "{r:?}");
        }
        assert!(fake.all_gone(top) && fake.all_gone(other));
        assert!(!fake.wrong_order.load(Ordering::SeqCst));
        assert!(
            fake.most_busy.load(Ordering::SeqCst) > 1,
            "never in parallel"
        );
        // one thread: the same, one at a time
        let mut one = Fake::default();
        let t = one.top("r");
        one.grow(t, 3, 2, 2);
        let done = run(&one, &["r"], 1, None);
        assert!(done[0].1.first.is_none() && one.all_gone(t));
        assert_eq!(one.most_busy.load(Ordering::SeqCst), 1);
    }

    /// A folder of many names lends them out: several threads delete in it at once.
    #[test]
    fn a_flat_folder_is_shared() {
        let mut fake = Fake {
            pause: Duration::from_micros(300),
            ..Fake::default()
        };
        let top = fake.top("flat");
        fake.grow(top, 0, 600, 0);
        let done = run(&fake, &["flat"], 4, None);
        assert!(done[0].1.first.is_none());
        assert!(fake.all_gone(top));
        assert!(fake.most_busy.load(Ordering::SeqCst) > 1);
    }

    /// What cannot be deleted stays with every folder above it, and is the item's error; the
    /// rest goes.
    #[test]
    fn a_held_entry_keeps_its_folders_and_the_rest_goes() {
        let mut fake = Fake::default();
        let top = fake.top("r");
        let a = fake.add(Some(top), "a", true);
        let deep = fake.add(Some(a), "deep", true);
        let held = fake.add(Some(deep), "held.docx", false);
        fake.add(Some(deep), "free.txt", false);
        let b = fake.add(Some(top), "b", true);
        fake.grow(b, 2, 5, 2);
        fake.held.insert(held);
        let done = run(&fake, &["r"], 4, None);
        let r = &done[0].1;
        assert_eq!(r.first.as_ref().map(|e| e.to_string()), Some("held".into()));
        assert!(r.deleted && !r.cancelled);
        assert!(fake.all_gone(b));
        let gone = fake.gone.lock().unwrap();
        assert!(!gone.contains(&top) && !gone.contains(&a) && !gone.contains(&deep));
        assert!(!fake.wrong_order.load(Ordering::SeqCst));
        // the folders that keep it are not asked to go (that could only fail, and on Windows
        // wait for deletions still pending first)
        assert_eq!(fake.asked_too_soon.load(Ordering::SeqCst), 0);
    }

    /// A bug that panics in one thread does not leave the others waiting for its task for ever:
    /// its item is reported failed (with what went wrong), the other items are deleted.
    #[test]
    fn a_panic_in_one_thread_fails_its_item_only() {
        let mut fake = Fake {
            pause: Duration::from_micros(100),
            ..Fake::default()
        };
        let top = fake.top("r");
        fake.grow(top, 3, 4, 2);
        let a = fake.tree[top].children.clone();
        let deep = *a.iter().rev().find(|&&c| fake.tree[c].folder).unwrap();
        let bug = fake.add(Some(deep), "boom.txt", false);
        fake.bugs.insert(bug);
        let other = fake.top("q");
        fake.grow(other, 2, 3, 1);
        // on a thread of its own: a hang fails the test instead of holding it
        let fake: &'static Fake = Box::leak(Box::new(fake));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run(fake, &["r", "q"], 4, None));
        });
        let done = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the threads hung");
        assert_eq!(done.len(), 2);
        let by: HashMap<usize, &Removed> = done.iter().map(|(k, r)| (*k, r)).collect();
        let r = by[&0];
        assert!(!r.cancelled, "{r:?}");
        let msg = r.first.as_ref().unwrap().to_string();
        assert!(msg.contains("a bug at boom.txt"), "{msg}");
        assert!(by[&1].first.is_none() && by[&1].deleted, "{:?}", by[&1]);
        assert!(fake.all_gone(other));
    }

    /// An item inside another one goes first; the outer one starts only once it is finished.
    #[test]
    fn an_item_inside_another_goes_first() {
        let mut fake = Fake::default();
        let outer = fake.top("r/a");
        let inner = fake.top("r/a/b");
        fake.grow(inner, 2, 3, 1);
        fake.add(Some(outer), "x.txt", false);
        let done = run(&fake, &["r/a", "r/a/b"], 4, None);
        assert_eq!(done.iter().map(|d| d.0).collect::<Vec<_>>(), [1, 0]);
        assert_eq!(*fake.started.lock().unwrap(), [inner, outer]);
        assert!(done.iter().all(|d| d.1.first.is_none()));
    }

    /// Once the cancel says stop, nothing new starts and no folder is removed any more; what
    /// was not finished is reported cancelled.
    #[test]
    fn a_cancel_stops_every_thread() {
        let mut fake = Fake {
            pause: Duration::from_micros(200),
            ..Fake::default()
        };
        let mut tops = Vec::new();
        for i in 0..6 {
            let t = fake.top(&format!("t{i}"));
            fake.grow(t, 3, 4, 2);
            tops.push(format!("t{i}"));
        }
        let cancel = || fake.visits.load(Ordering::SeqCst) >= 40;
        let names: Vec<&str> = tops.iter().map(String::as_str).collect();
        let done = run(&fake, &names, 4, Some(&cancel));
        assert_eq!(done.len(), 6);
        assert!(fake.stopped.load(Ordering::SeqCst));
        // at most what a thread had begun as the stop came (one each)
        assert!(fake.late.load(Ordering::SeqCst) <= 4, "{:?}", fake.late);
        assert!(done.iter().any(|d| d.1.cancelled));
        // a thread goes on to its next folder at most (four files each here), then stops
        assert!(
            fake.visits.load(Ordering::SeqCst) <= 40 + 4 * 6,
            "{:?}",
            fake.visits
        );
    }
}

// --------------------------------------------------------------------------- POSIX

#[cfg(unix)]
mod imp {
    use std::ffi::{CStr, CString, OsString};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use std::collections::HashSet;

    use super::*;

    struct Frame {
        /// None while deeper levels hold the descriptors (the deleted folder's is never closed)
        fd: Option<OwnedFd>,
        /// what it is: a folder reached again must be this one
        dev: libc::dev_t,
        ino: libc::ino_t,
        /// its name in its parent's folder (the top: empty)
        name: CString,
        /// as given (messages, the seams)
        path: PathBuf,
        /// below the top resolved: what the mount table lists (only without mount ids)
        real: Option<PathBuf>,
        names: std::vec::IntoIter<OsString>,
        /// names the listing told are not folders (`d_type`): unlinked without a look first
        files: HashSet<OsString>,
    }

    impl Frame {
        fn raw(&self) -> RawFd {
            self.fd
                .as_ref()
                .expect("the folder being worked on is open")
                .as_raw_fd()
        }

        fn is(&self, st: &libc::stat) -> bool {
            st.st_dev == self.dev && st.st_ino == self.ino
        }
    }

    fn last() -> io::Error {
        io::Error::last_os_error()
    }

    /// A folder that is no longer where the deletion left it: renamed away, swapped for a link
    /// or a file, gone.
    fn moved(e: io::Error) -> io::Error {
        match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) | Some(libc::ENOENT) => super::moved_inside(),
            _ => e,
        }
    }

    fn open_dir_at(dir: RawFd, name: &CStr) -> io::Result<OwnedFd> {
        probe::call(Call::Open);
        // SAFETY: a NUL-terminated name; the descriptor is owned from here on.
        let fd = unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last());
        }
        // SAFETY: a descriptor just opened.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn stat_at(dir: RawFd, name: &CString) -> io::Result<libc::stat> {
        probe::call(Call::Stat);
        // SAFETY: zeroed is a valid stat; the name is NUL-terminated.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) != 0 {
                return Err(last());
            }
            Ok(st)
        }
    }

    fn stat_fd(fd: RawFd) -> io::Result<libc::stat> {
        probe::call(Call::Query);
        // SAFETY: zeroed is a valid stat.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut st) != 0 {
                return Err(last());
            }
            Ok(st)
        }
    }

    fn is_dir(st: &libc::stat) -> bool {
        st.st_mode & libc::S_IFMT == libc::S_IFDIR
    }

    /// Unlink an entry its folder's listing said is no folder: None when it is one now (looked
    /// at then as any entry is), else whether it was deleted (false: gone meanwhile).
    fn unlink_listed(dir: RawFd, name: &CString) -> Option<io::Result<bool>> {
        match unlink_at(dir, name, false) {
            Ok(()) => Some(Ok(true)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Ok(false)),
            // a folder: EISDIR on Linux, EPERM on macOS (EPERM may be a flag too: looked at)
            Err(e) if matches!(e.raw_os_error(), Some(libc::EISDIR) | Some(libc::EPERM)) => None,
            Err(e) => Some(Err(e)),
        }
    }

    fn unlink_at(dir: RawFd, name: &CString, folder: bool) -> io::Result<()> {
        let flags = if folder { libc::AT_REMOVEDIR } else { 0 };
        probe::call(Call::Delete);
        // SAFETY: a NUL-terminated name.
        if unsafe { libc::unlinkat(dir, name.as_ptr(), flags) } != 0 {
            return Err(last());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn clear_errno() {
        // SAFETY: this thread's errno.
        unsafe { *libc::__errno_location() = 0 }
    }

    #[cfg(target_os = "macos")]
    fn clear_errno() {
        // SAFETY: this thread's errno.
        unsafe { *libc::__error() = 0 }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn clear_errno() {}

    /// The names in the folder open as `fd` (`.` and `..` left out), and those of them the
    /// listing tells are not folders (`d_type`; `DT_UNKNOWN` tells nothing).
    fn list(fd: RawFd) -> io::Result<(Vec<OsString>, HashSet<OsString>)> {
        probe::call(Call::List);
        // SAFETY: the duplicate belongs to the DIR stream, closed below; each entry is copied
        // before the next readdir.
        unsafe {
            let dup = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0);
            if dup < 0 {
                return Err(last());
            }
            let dir = libc::fdopendir(dup);
            if dir.is_null() {
                let e = last();
                libc::close(dup);
                return Err(e);
            }
            let mut names = Vec::new();
            let mut files = HashSet::new();
            let result = loop {
                clear_errno();
                let ent = libc::readdir(dir);
                if ent.is_null() {
                    let e = last();
                    break match e.raw_os_error() {
                        Some(0) | None => Ok(()),
                        _ => Err(e),
                    };
                }
                let name = std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()).to_bytes();
                if name != b"." && name != b".." {
                    let t = (*ent).d_type;
                    if t != libc::DT_DIR && t != libc::DT_UNKNOWN {
                        files.insert(OsString::from_vec(name.to_vec()));
                    }
                    names.push(OsString::from_vec(name.to_vec()));
                }
            };
            libc::closedir(dir);
            result.map(|()| (names, files))
        }
    }

    fn frame(
        fd: OwnedFd,
        st: &libc::stat,
        name: CString,
        path: PathBuf,
        real: Option<PathBuf>,
    ) -> io::Result<Frame> {
        let (listed, files) = list(fd.as_raw_fd())?;
        let names = super::seam_names(&path, listed);
        Ok(Frame {
            fd: Some(fd),
            dev: st.st_dev,
            ino: st.st_ino,
            name,
            path,
            real,
            names: names.into_iter(),
            files,
        })
    }

    /// The `..` of `child`'s open folder, if that is `parent` (device and inode).
    fn parent_of(child: &Frame, parent: &Frame) -> Option<OwnedFd> {
        let fd = open_dir_at(child.fd.as_ref()?.as_raw_fd(), c"..").ok()?;
        let st = stat_fd(fd.as_raw_fd()).ok()?;
        parent.is(&st).then_some(fd)
    }

    /// Open the deepest level of `stack` again (its descriptor was closed on the way down): by
    /// the names from the nearest level that is open, each folder checked to be the one left
    /// there. Levels on the way keep their descriptor only within `keep` of the deepest.
    fn reach(stack: &mut [Frame], keep: usize) -> io::Result<()> {
        let deepest = stack.len() - 1;
        let Some(from) = (0..=deepest).rev().find(|&k| stack[k].fd.is_some()) else {
            return Err(super::moved_inside()); // the deleted folder is always open
        };
        let kept = stack.len().saturating_sub(keep);
        let mut held: Option<OwnedFd> = None; // a level on the way that does not keep it
        for k in from + 1..=deepest {
            let dir = held
                .as_ref()
                .map_or_else(|| stack[k - 1].raw(), |h| h.as_raw_fd());
            let fd = open_dir_at(dir, &stack[k].name).map_err(moved)?;
            if !stack[k].is(&stat_fd(fd.as_raw_fd())?) {
                return Err(super::moved_inside());
            }
            if k >= kept {
                stack[k].fd = Some(fd);
                held = None;
            } else {
                held = Some(fd);
            }
        }
        Ok(())
    }

    struct Tree {
        dev: libc::dev_t,
        /// the top's mount id (Linux 5.8+)
        mount: Option<u64>,
        /// the mount table, where there are no mount ids
        mounts: Option<Mounts>,
    }

    /// Open the folder `top` (by its path, not following a link at the end) and list it; what
    /// the tree below must stay on.
    fn open_top(top: &Path) -> io::Result<(Frame, Tree)> {
        let c = CString::new(top.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let fd = open_dir_at(libc::AT_FDCWD, &c).map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => super::moved_inside(),
            _ => e,
        })?;
        let st = stat_fd(fd.as_raw_fd())?;
        let mount = mount_id(fd.as_raw_fd());
        let mounts = if mount.is_none() {
            Mounts::read()
        } else {
            None
        };
        let real = mounts
            .as_ref()
            .map(|_| super::super::paths::resolve_lenient(top));
        let f = frame(fd, &st, CString::default(), top.to_path_buf(), real)?;
        Ok((
            f,
            Tree {
                dev: st.st_dev,
                mount,
                mounts,
            },
        ))
    }

    /// Open the folder `c` of the folder open as `dirfd` (`st`: what `fstatat` saw of it) and
    /// list it: it must be the very folder `fstatat` saw, on the tree's own mount.
    fn open_child(
        dirfd: RawFd,
        c: &CString,
        st: &libc::stat,
        path: &Path,
        real: Option<&PathBuf>,
        tree: &Tree,
    ) -> io::Result<Frame> {
        let cfd = open_dir_at(dirfd, c).map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => super::moved_inside(),
            _ => e,
        })?;
        let now = stat_fd(cfd.as_raw_fd())?;
        if now.st_dev != st.st_dev || now.st_ino != st.st_ino {
            return Err(super::moved_inside()); // replaced since it was looked at
        }
        let elsewhere = match (tree.mount, mount_id(cfd.as_raw_fd())) {
            (Some(a), Some(b)) => a != b,
            _ => match (&tree.mounts, real) {
                (Some(m), Some(r)) => m.contains(r),
                _ => false,
            },
        };
        if elsewhere {
            return Err(super::mount_inside());
        }
        frame(cfd, &now, c.clone(), path.to_path_buf(), real.cloned())
    }

    /// Remove the folder `name` from the folder open as `dirfd` while that name still is the
    /// folder `(dev, ino)`: a folder that took its name meanwhile stays.
    fn unlink_checked(
        dirfd: RawFd,
        name: &CString,
        id: (libc::dev_t, libc::ino_t),
    ) -> io::Result<()> {
        let st = stat_at(dirfd, name)?;
        if (st.st_dev, st.st_ino) != id {
            return Err(super::moved_inside());
        }
        unlink_at(dirfd, name, true)
    }

    /// One thread's way through what `stack` holds, depth first (its bottom is never closed);
    /// the bottom, emptied, is removed last by `remove_bottom`.
    fn run_stack(
        mut stack: Vec<Frame>,
        tree: &Tree,
        keep: usize,
        cancel: &dyn Fn() -> bool,
        out: &mut Removed,
        remove_bottom: &mut dyn FnMut(Frame) -> io::Result<()>,
    ) {
        loop {
            // back at a level closed on the way down (its own folder below could not reach it)
            if stack.last().is_some_and(|f| f.fd.is_none())
                && let Err(e) = reach(&mut stack, keep)
            {
                out.fail(e);
                stack.pop(); // what it still holds stays, and so does it
                continue;
            }
            let Some(f) = stack.last_mut() else { break };
            let Some(name) = f.names.next() else {
                let done = stack.pop().unwrap();
                let gone = if let Some(parent) = stack.last_mut() {
                    if parent.fd.is_none() {
                        parent.fd = parent_of(&done, parent);
                    }
                    if parent.fd.is_none()
                        && let Err(e) = reach(&mut stack, keep)
                    {
                        out.fail(e);
                        stack.pop(); // what it still holds stays, and so does it
                        continue;
                    }
                    unlink_at(stack.last().unwrap().raw(), &done.name, true)
                } else {
                    remove_bottom(done)
                };
                match gone {
                    Ok(()) => out.deleted = true,
                    Err(e) => out.fail(e),
                }
                continue;
            };
            if !super::joinable(name.as_bytes()) {
                out.fail(super::bad_name_inside());
                continue;
            }
            let Ok(c) = CString::new(name.as_bytes()) else {
                out.fail(super::bad_name_inside());
                continue;
            };
            let path = f.path.join(&name);
            let real = f.real.as_ref().map(|r| r.join(&name));
            seams::entering(&path);
            let dirfd = f.raw();
            // listed as no folder: unlinked without a look first (never a folder, never through
            // a link); one that turned into a folder meanwhile is looked at below
            if f.files.contains(&name) {
                match unlink_listed(dirfd, &c) {
                    Some(Ok(deleted)) => {
                        out.deleted |= deleted;
                        continue;
                    }
                    Some(Err(e)) => {
                        out.fail(e);
                        continue;
                    }
                    None => {}
                }
            }
            let st = match stat_at(dirfd, &c) {
                Ok(st) => st,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue, // gone meanwhile
                Err(e) => {
                    out.fail(e);
                    continue;
                }
            };
            if !is_dir(&st) {
                // a file or a link: the entry itself, never what it points to
                match unlink_at(dirfd, &c, false) {
                    Ok(()) => out.deleted = true,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => out.fail(e),
                }
                continue;
            }
            if st.st_dev != tree.dev {
                out.fail(other_device(dirfd, &c));
                continue;
            }
            if cancel() {
                out.cancelled = true;
                return;
            }
            match open_child(dirfd, &c, &st, &path, real.as_ref(), tree) {
                Ok(child) => {
                    stack.push(child);
                    // the deleted folder and the `keep` deepest levels stay open
                    let n = stack.len();
                    if n > keep + 1 {
                        stack[n - 1 - keep].fd = None;
                    }
                }
                Err(e) => out.fail(e),
            }
        }
    }

    /// Delete the folder `top` and everything in it, one thread (as before there were several).
    fn remove_tree(top: &Path, keep: usize, cancel: &dyn Fn() -> bool) -> Removed {
        let mut out = Removed::default();
        if cancel() {
            out.cancelled = true;
            return out;
        }
        seams::entering(top);
        let (first, tree) = match open_top(top) {
            Ok(s) => s,
            Err(e) => {
                out.fail(e);
                return out;
            }
        };
        run_stack(vec![first], &tree, keep, cancel, &mut out, &mut |done| {
            drop(done.fd);
            std::fs::remove_dir(top)
        });
        out
    }

    /// A folder of the top levels, which several threads empty.
    pub(in super::super) struct Dir {
        fd: OwnedFd,
        dev: libc::dev_t,
        ino: libc::ino_t,
        /// its name in its parent's folder (the top: empty)
        name: CString,
        path: PathBuf,
        real: Option<PathBuf>,
        tree: Arc<Tree>,
        files: HashSet<OsString>,
    }

    fn into_dir(f: Frame, tree: Arc<Tree>) -> (Dir, Vec<OsString>) {
        let Frame {
            fd,
            dev,
            ino,
            name,
            path,
            real,
            names,
            files,
        } = f;
        let dir = Dir {
            fd: fd.expect("a folder just opened"),
            dev,
            ino,
            name,
            path,
            real,
            tree,
            files,
        };
        (dir, names.collect())
    }

    /// POSIX: the top `par_depth` levels on several threads, deeper folders by the thread that
    /// met them, keeping `keep` levels open.
    pub(in super::super) struct Posix {
        pub keep: usize,
        pub par_depth: usize,
    }

    impl Walk for Posix {
        type Dir = Dir;
        type Scratch = ();

        fn scratch(&self) {}

        fn whole(&self, top: &Path, cancel: &dyn Fn() -> bool) -> Option<Removed> {
            (self.par_depth == 0).then(|| remove_tree(top, self.keep, cancel))
        }

        fn start(&self, top: &Path, _: &mut ()) -> io::Result<(Dir, Vec<OsString>)> {
            seams::entering(top);
            let (f, tree) = open_top(top)?;
            Ok(into_dir(f, Arc::new(tree)))
        }

        fn visit(
            &self,
            dir: &Dir,
            depth: usize,
            name: OsString,
            _: &mut (),
            cancel: &dyn Fn() -> bool,
        ) -> Visit<Dir> {
            if !super::joinable(name.as_bytes()) {
                return Visit::Failed(super::bad_name_inside());
            }
            let Ok(c) = CString::new(name.as_bytes()) else {
                return Visit::Failed(super::bad_name_inside());
            };
            let path = dir.path.join(&name);
            let real = dir.real.as_ref().map(|r| r.join(&name));
            seams::entering(&path);
            let dirfd = dir.fd.as_raw_fd();
            if dir.files.contains(&name) {
                match unlink_listed(dirfd, &c) {
                    Some(Ok(true)) => return Visit::Deleted,
                    Some(Ok(false)) => return Visit::Gone,
                    Some(Err(e)) => return Visit::Failed(e),
                    None => {}
                }
            }
            let st = match stat_at(dirfd, &c) {
                Ok(st) => st,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Visit::Gone,
                Err(e) => return Visit::Failed(e),
            };
            if !is_dir(&st) {
                // a file or a link: the entry itself, never what it points to
                return match unlink_at(dirfd, &c, false) {
                    Ok(()) => Visit::Deleted,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Visit::Gone,
                    Err(e) => Visit::Failed(e),
                };
            }
            if st.st_dev != dir.tree.dev {
                return Visit::Failed(other_device(dirfd, &c));
            }
            if cancel() {
                return Visit::Stop;
            }
            let child = match open_child(dirfd, &c, &st, &path, real.as_ref(), &dir.tree) {
                Ok(f) => f,
                Err(e) => return Visit::Failed(e),
            };
            if depth + 1 >= self.par_depth {
                // deeper down: this thread, the way one thread does it
                let mut out = Removed::default();
                let id = (child.dev, child.ino);
                run_stack(
                    vec![child],
                    &dir.tree,
                    self.keep,
                    cancel,
                    &mut out,
                    &mut |done| {
                        let name = done.name.clone();
                        drop(done);
                        unlink_checked(dirfd, &name, id)
                    },
                );
                return Visit::Whole(out);
            }
            let (d, names) = into_dir(child, dir.tree.clone());
            Visit::Folder(d, names)
        }

        fn remove(&self, dir: &Dir, parent: Option<&Dir>, top: &Path) -> io::Result<()> {
            match parent {
                Some(p) => unlink_checked(p.fd.as_raw_fd(), &dir.name, (dir.dev, dir.ino)),
                None => std::fs::remove_dir(top),
            }
        }
    }

    /// Why the folder `name` of the folder open as `dirfd`, on another device than its tree, is
    /// not entered: a mount point, or (Linux) a Btrfs subvolume — another device number on the
    /// same mount, not a mount's root.
    fn other_device(dirfd: RawFd, name: &CString) -> io::Error {
        #[cfg(target_os = "linux")]
        if mount_root(dirfd, name) == Some(false) {
            return super::subvolume_inside();
        }
        let _ = (dirfd, name);
        super::mount_inside()
    }

    /// Whether `name` in the folder open as `dirfd` is the root of a mount (`statx`'s
    /// STATX_ATTR_MOUNT_ROOT, Linux 5.8+); None when it cannot be told.
    #[cfg(target_os = "linux")]
    fn mount_root(dirfd: RawFd, name: &CString) -> Option<bool> {
        const STATX_ATTR_MOUNT_ROOT: u64 = 0x2000;
        probe::call(Call::Stat);
        // SAFETY: zeroed is a valid statx; a NUL-terminated name.
        unsafe {
            let mut stx: libc::statx = std::mem::zeroed();
            let rc = libc::statx(
                dirfd,
                name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_BASIC_STATS,
                &mut stx,
            );
            (rc == 0 && stx.stx_attributes_mask & STATX_ATTR_MOUNT_ROOT != 0)
                .then_some(stx.stx_attributes & STATX_ATTR_MOUNT_ROOT != 0)
        }
    }

    /// The id of the mount the open `fd` is on (Linux 5.8+), else None.
    #[cfg(target_os = "linux")]
    fn mount_id(fd: RawFd) -> Option<u64> {
        probe::call(Call::Query);
        // SAFETY: zeroed is a valid statx; an empty path with AT_EMPTY_PATH is `fd` itself.
        unsafe {
            let mut stx: libc::statx = std::mem::zeroed();
            let rc = libc::statx(
                fd,
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_MNT_ID,
                &mut stx,
            );
            (rc == 0 && stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn mount_id(_fd: RawFd) -> Option<u64> {
        None
    }

    /// The mount points `/proc/self/mountinfo` lists (Linux; None elsewhere or unreadable).
    struct Mounts(std::collections::HashSet<Vec<u8>>);

    impl Mounts {
        fn read() -> Option<Mounts> {
            if !cfg!(target_os = "linux") {
                return None;
            }
            let text = std::fs::read("/proc/self/mountinfo").ok()?;
            Some(Mounts(
                text.split(|&b| b == b'\n')
                    .filter_map(|line| line.split(|&b| b == b' ').nth(4))
                    .map(unescape)
                    .collect(),
            ))
        }

        fn contains(&self, path: &Path) -> bool {
            self.0.contains(path.as_os_str().as_bytes())
        }
    }

    /// mountinfo's octal escapes (`\040` for a space).
    fn unescape(field: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(field.len());
        let mut i = 0;
        while i < field.len() {
            if field[i] == b'\\'
                && i + 3 < field.len()
                && field[i + 1..i + 4]
                    .iter()
                    .all(|c| (b'0'..=b'7').contains(c))
            {
                let v = field[i + 1..i + 4]
                    .iter()
                    .fold(0u32, |a, c| a * 8 + u32::from(c - b'0'));
                out.push(v as u8);
                i += 4;
            } else {
                out.push(field[i]);
                i += 1;
            }
        }
        out
    }

    /// Whether the folder `path` is on a mount of its own (bind mounts of the same file system
    /// too): its mount is not its parent's.
    pub(crate) fn other_mount_than_parent(path: &Path) -> bool {
        let open = |p: &Path| -> Option<OwnedFd> {
            let c = CString::new(p.as_os_str().as_bytes()).ok()?;
            open_dir_at(libc::AT_FDCWD, &c).ok()
        };
        let (Some(me), Some(up)) = (open(path), open(&path.join(".."))) else {
            return false;
        };
        match (mount_id(me.as_raw_fd()), mount_id(up.as_raw_fd())) {
            (Some(a), Some(b)) => a != b,
            _ => Mounts::read()
                .is_some_and(|m| m.contains(&super::super::paths::resolve_lenient(path))),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn mountinfo_escapes() {
            assert_eq!(super::unescape(b"/mnt/a\\040b"), b"/mnt/a b");
            assert_eq!(super::unescape(b"/x\\134y"), b"/x\\y");
            assert_eq!(super::unescape(b"/plain"), b"/plain");
            assert_eq!(super::unescape(b"/end\\04"), b"/end\\04");
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn mount_ids_are_known() {
            use std::os::fd::AsRawFd;
            let dir = tempfile::tempdir().unwrap();
            let f = std::fs::File::open(dir.path()).unwrap();
            // every supported kernel (RHEL 9: 5.14, Ubuntu 22.04: 5.15) has them
            assert!(super::mount_id(f.as_raw_fd()).is_some());
            assert!(!super::other_mount_than_parent(dir.path()));
        }
    }
}

#[cfg(unix)]
pub(super) use imp::other_mount_than_parent;

// --------------------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use std::ffi::{OsString, c_void};
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};

    use std::collections::HashMap;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FULL_DIR_INFO, FILE_ID_BOTH_DIR_INFO,
        GetFileInformationByHandle, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    use super::*;

    const DELETE: u32 = 0x0001_0000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const SHARE_ALL: u32 = 7;
    const OPEN_EXISTING: u32 = 3;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    const ATTR_READONLY: u32 = 0x1;
    const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_NORMAL: u32 = 0x80;
    const ATTR_REPARSE_POINT: u32 = 0x400;
    const TAG_NAME_SURROGATE: u32 = 0x2000_0000;
    const TAG_MOUNT_POINT: u32 = 0xA000_0003;

    const FILE_BASIC_INFO: i32 = 0;
    const FILE_NAME_INFO: i32 = 2;
    const FILE_DISPOSITION_INFO: i32 = 4;
    const FILE_ATTRIBUTE_TAG_INFO: i32 = 9;
    const FILE_ID_BOTH_DIRECTORY_INFO: i32 = 10;
    const FILE_ID_BOTH_DIRECTORY_RESTART_INFO: i32 = 11;
    const FILE_FULL_DIRECTORY_INFO: i32 = 14;
    const FILE_FULL_DIRECTORY_RESTART_INFO: i32 = 15;
    const FILE_DISPOSITION_INFO_EX: i32 = 21;
    const DISPOSITION_DELETE: u32 = 0x1;
    const DISPOSITION_POSIX: u32 = 0x2;
    const DISPOSITION_IGNORE_READONLY: u32 = 0x10;
    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_NO_MORE_FILES: i32 = 18;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_DIR_NOT_EMPTY: i32 = 145;
    const STATUS_DELETE_PENDING: i32 = 0xC000_0056_u32 as i32;
    /// GetVolumeInformation: the file system can hold names that differ only in case
    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x1;

    /// Pauses before a folder whose every entry was deleted, but that is not empty yet, is asked
    /// to go again (about a second in all): [`remove_when_empty`].
    const SETTLE_MS: [u64; 6] = [10, 30, 60, 120, 250, 500];

    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        /// The NTSTATUS the last failed call of this thread ended with (finer than its Win32
        /// error: STATUS_DELETE_PENDING is ERROR_ACCESS_DENIED there).
        fn RtlGetLastNtStatus() -> i32;
    }

    const BACKSLASH: u16 = b'\\' as u16;

    struct Handle(HANDLE);

    // SAFETY: a kernel handle is the process's, used from any thread; closed once, when dropped.
    unsafe impl Send for Handle {}
    // SAFETY: as above; the calls made through a shared one only read what it is open on.
    unsafe impl Sync for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle this owns.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn last() -> io::Error {
        io::Error::last_os_error()
    }

    /// Open by a `\\?\` path; `follow`: through a reparse point at the end (else the entry
    /// itself).
    fn open(path: &[u16], access: u32, follow: bool) -> io::Result<Handle> {
        open_nt(path, access, follow).map_err(|(e, _)| e)
    }

    /// Whether an [`open_nt`] failed because what it opens is being deleted already (another
    /// program holds it open, sharing its deletion: FAT, exFAT and network shares keep its name
    /// until that program closes it, and so does NTFS where it was deleted the classic way).
    /// Nothing can open it then, and its Win32 error says only 「没有权限」.
    fn delete_pending((e, status): &(io::Error, i32)) -> bool {
        e.raw_os_error() == Some(ERROR_ACCESS_DENIED) && *status == STATUS_DELETE_PENDING
    }

    /// [`open`]; failed, with the NTSTATUS it failed with too (finer than its Win32 error).
    fn open_nt(path: &[u16], access: u32, follow: bool) -> Result<Handle, (io::Error, i32)> {
        probe::call(Call::Open);
        let mut w: Vec<u16> = path.to_vec();
        w.push(0);
        let flags = BACKUP_SEMANTICS | if follow { 0 } else { OPEN_REPARSE_POINT };
        // SAFETY: a NUL-terminated path; the handle is owned from here on.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                SHARE_ALL,
                null(),
                OPEN_EXISTING,
                flags,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            // SAFETY: this thread's status, read right after the call that set it.
            let status = unsafe { RtlGetLastNtStatus() };
            return Err((last(), status));
        }
        Ok(Handle(h))
    }

    /// What an open handle is.
    struct Facts {
        attrs: u32,
        tag: u32,
        volume: u32,
        index: u64,
        /// the name it was opened under, after any reparse (None: the file system cannot say)
        name: Option<Vec<u16>>,
        /// it could not be opened for deletion: why (a folder is still emptied)
        undeletable: Option<i32>,
    }

    impl Facts {
        fn dir(&self) -> bool {
            self.attrs & ATTR_DIRECTORY != 0
        }
        fn reparse(&self) -> bool {
            self.attrs & ATTR_REPARSE_POINT != 0
        }
        /// a link of some kind: symlink, junction, mount point
        fn link(&self) -> bool {
            self.reparse() && self.tag & TAG_NAME_SURROGATE != 0
        }
    }

    fn identity(h: &Handle) -> io::Result<(u32, u64)> {
        identity_attrs(h).map(|(volume, index, _)| (volume, index))
    }

    /// (volume, file index, attributes) of what `h` is open on: one query.
    fn identity_attrs(h: &Handle) -> io::Result<(u32, u64, u32)> {
        probe::call(Call::Query);
        // SAFETY: zeroed is a valid BY_HANDLE_FILE_INFORMATION; the handle is open.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(h.0, &mut info) == 0 {
                return Err(last());
            }
            Ok((
                info.dwVolumeSerialNumber,
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
                info.dwFileAttributes,
            ))
        }
    }

    /// The name `h` was opened under, below its volume, after reparse points on the way.
    fn opened_name(h: &Handle, buf: &mut [u64]) -> Option<Vec<u16>> {
        probe::call(Call::Query);
        let bytes = (buf.len() * 8) as u32;
        // SAFETY: the buffer holds `bytes` bytes, 8-aligned (FILE_NAME_INFO: a u32 length in
        // bytes, then the name).
        unsafe {
            if GetFileInformationByHandleEx(
                h.0,
                FILE_NAME_INFO,
                buf.as_mut_ptr() as *mut c_void,
                bytes,
            ) == 0
            {
                return None;
            }
            let len = (*(buf.as_ptr() as *const u32) as usize / 2).min((buf.len() * 8 - 4) / 2);
            let name = std::slice::from_raw_parts((buf.as_ptr() as *const u16).add(2), len);
            Some(name.to_vec())
        }
    }

    /// Open an entry for deletion and tell what it is; None: it is gone, or going (its deletion
    /// is pending: nothing is left to do to it, and its folder says why it stays, if it does —
    /// [`why_not_empty`]). Where deleting it is refused (no right to, another program holds it
    /// open without sharing that), it is opened to look only: a folder can still be emptied.
    fn open_entry(path: &[u16], buf: &mut [u64]) -> io::Result<Option<(Handle, Facts)>> {
        let (h, undeletable) =
            match open_nt(path, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE, false) {
                Ok(h) => (h, None),
                Err((e, _)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(failed) if delete_pending(&failed) => return Ok(None),
                Err((e, _))
                    if matches!(
                        e.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
                    ) =>
                {
                    match open(path, FILE_READ_ATTRIBUTES | SYNCHRONIZE, false) {
                        Ok(h) => (h, e.raw_os_error()),
                        Err(_) => return Err(e),
                    }
                }
                Err((e, _)) => return Err(e),
            };
        // the attributes come with the identity; the reparse tag is asked for only where there
        // is one (one query less for every plain file and folder)
        let (volume, index, attrs) = identity_attrs(&h)?;
        let mut tag = [attrs, 0u32];
        if attrs & ATTR_REPARSE_POINT != 0 {
            probe::call(Call::Query);
            // SAFETY: FILE_ATTRIBUTE_TAG_INFO is two u32s.
            if unsafe {
                GetFileInformationByHandleEx(
                    h.0,
                    FILE_ATTRIBUTE_TAG_INFO,
                    tag.as_mut_ptr() as *mut c_void,
                    8,
                )
            } == 0
            {
                return Err(last());
            }
        }
        let name = opened_name(&h, buf).map(|mut n| {
            seams::opened_name(&mut n);
            n
        });
        Ok(Some((
            h,
            Facts {
                attrs: tag[0],
                tag: tag[1],
                volume,
                index,
                name,
                undeletable,
            },
        )))
    }

    /// Whether a mount-point reparse point mounts a volume (`\??\Volume{…}\`, what `mountvol`
    /// writes) rather than being a junction to a folder. Unreadable: the volume lookup decides.
    fn mounts_a_volume(h: &Handle, path: &[u16]) -> bool {
        let mut buf = vec![0u64; 16 * 1024 / 8];
        let mut got = 0u32;
        // SAFETY: an output buffer of the size given; no input, no overlapped I/O.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                FSCTL_GET_REPARSE_POINT,
                null(),
                0,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * 8) as u32,
                &mut got,
                null_mut(),
            )
        } != 0;
        if !ok || (got as usize) < 16 {
            return super::super::paths::is_mount(&as_path(path));
        }
        // SAFETY: `got` bytes were written, the 16-byte header at least.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize) };
        let u16_at = |i: usize| usize::from(u16::from_le_bytes([bytes[i], bytes[i + 1]]));
        if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != TAG_MOUNT_POINT {
            return false;
        }
        // the substitute name, after the 16-byte header of a mount-point buffer
        let (off, len) = (16 + u16_at(8), u16_at(10));
        if off + len > bytes.len() {
            return true;
        }
        let units: Vec<u16> = bytes[off..off + len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        let name = String::from_utf16_lossy(&units).to_ascii_lowercase();
        name.starts_with("\\??\\volume{") || name.starts_with("\\\\?\\volume{")
    }

    fn set_disposition(h: &Handle, ex_flags: Option<u32>) -> io::Result<()> {
        probe::call(Call::Delete);
        // SAFETY: FILE_DISPOSITION_INFO_EX is one u32, FILE_DISPOSITION_INFO one BOOLEAN.
        let ok = unsafe {
            match ex_flags {
                Some(flags) => {
                    SetFileInformationByHandle(
                        h.0,
                        FILE_DISPOSITION_INFO_EX,
                        &flags as *const u32 as *const c_void,
                        4,
                    ) != 0
                }
                None => {
                    let delete: u8 = 1;
                    SetFileInformationByHandle(
                        h.0,
                        FILE_DISPOSITION_INFO,
                        &delete as *const u8 as *const c_void,
                        1,
                    ) != 0
                }
            }
        };
        if ok { Ok(()) } else { Err(last()) }
    }

    /// Delete what `h` is open on: POSIX semantics where the file system has them, else the
    /// classic flag; a read-only item loses the flag first (through another handle checked to
    /// be the same item), as Explorer does after asking.
    /// `posix`: the item's file system may have POSIX semantics; cleared at its first refusal of
    /// them (FAT, exFAT: not asked again for the rest of the item, one call less a file).
    fn delete(h: &Handle, path: &[u16], f: &Facts, posix: &AtomicBool) -> io::Result<()> {
        if let Some(code) = f.undeletable {
            return Err(io::Error::from_raw_os_error(code));
        }
        if posix.load(Ordering::Relaxed) {
            let all = DISPOSITION_DELETE | DISPOSITION_POSIX | DISPOSITION_IGNORE_READONLY;
            match set_disposition(h, Some(all)) {
                Ok(()) => return Ok(()),
                // no such information class here (invalid function / not supported / parameter)
                Err(e) if matches!(e.raw_os_error(), Some(1) | Some(50) | Some(87)) => {
                    posix.store(false, Ordering::Relaxed)
                }
                Err(_) => {}
            }
        }
        match set_disposition(h, None) {
            Ok(()) => Ok(()),
            Err(e)
                if f.attrs & ATTR_READONLY != 0
                    && e.raw_os_error() == Some(ERROR_ACCESS_DENIED) =>
            {
                let w = open(path, FILE_WRITE_ATTRIBUTES | SYNCHRONIZE, false)?;
                if identity(&w)? != (f.volume, f.index) {
                    return Err(super::moved_inside());
                }
                let attrs = match f.attrs & !ATTR_READONLY {
                    0 => ATTR_NORMAL,
                    a => a,
                };
                // FILE_BASIC_INFO: four times (0: unchanged), then the attributes
                let basic = [0u64, 0, 0, 0, u64::from(attrs)];
                // SAFETY: a FILE_BASIC_INFO-sized, 8-aligned buffer.
                if unsafe {
                    SetFileInformationByHandle(
                        w.0,
                        FILE_BASIC_INFO,
                        basic.as_ptr() as *const c_void,
                        40,
                    )
                } == 0
                {
                    return Err(last());
                }
                drop(w);
                if posix.load(Ordering::Relaxed) {
                    set_disposition(h, Some(DISPOSITION_DELETE | DISPOSITION_POSIX))
                        .or_else(|_| set_disposition(h, None))
                } else {
                    set_disposition(h, None)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// A directory information class: the restart and the next query, where an entry's name
    /// length and name are, and its file id (FileIdBoth).
    struct Class {
        restart: i32,
        next: i32,
        len_at: usize,
        name_at: usize,
        id_at: Option<usize>,
    }

    const FULL: Class = Class {
        restart: FILE_FULL_DIRECTORY_RESTART_INFO,
        next: FILE_FULL_DIRECTORY_INFO,
        len_at: std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength),
        name_at: std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName),
        id_at: None,
    };

    const ID_BOTH: Class = Class {
        restart: FILE_ID_BOTH_DIRECTORY_RESTART_INFO,
        next: FILE_ID_BOTH_DIRECTORY_INFO,
        len_at: std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength),
        name_at: std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
        id_at: Some(std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileId)),
    };

    /// Every entry of the folder `h` is open on (with listing access), `.` and `..` left out: its
    /// name and, where the class has them, its file id.
    fn entries(
        h: &Handle,
        buf: &mut [u64],
        class: &Class,
        mut each: impl FnMut(Vec<u16>, Option<u64>),
    ) -> io::Result<()> {
        let bytes = (buf.len() * 8) as u32;
        let malformed = || io::Error::other("malformed directory listing");
        probe::call(Call::List);
        let mut query = class.restart;
        loop {
            // SAFETY: the buffer holds `bytes` bytes, 8-aligned.
            let ok = unsafe {
                GetFileInformationByHandleEx(h.0, query, buf.as_mut_ptr() as *mut c_void, bytes)
            } != 0;
            if !ok {
                let e = last();
                if e.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                    return Ok(());
                }
                return Err(e);
            }
            query = class.next;
            // SAFETY: the buffer is initialised memory of `bytes` bytes.
            let raw =
                unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, bytes as usize) };
            let u32_at = |i: usize| -> io::Result<usize> {
                let b = raw.get(i..i + 4).ok_or_else(malformed)?;
                Ok(u32::from_le_bytes(b.try_into().unwrap()) as usize)
            };
            let mut at = 0usize;
            loop {
                let next = u32_at(at)?;
                let len = u32_at(at + class.len_at)?;
                let name = raw
                    .get(at + class.name_at..at + class.name_at + len)
                    .ok_or_else(malformed)?;
                let units: Vec<u16> = name
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_le_bytes(c))
                    .collect();
                let id = match class.id_at {
                    Some(i) => {
                        let b = raw.get(at + i..at + i + 8).ok_or_else(malformed)?;
                        Some(u64::from_le_bytes(b.try_into().unwrap()))
                    }
                    None => None,
                };
                if units != [b'.' as u16] && units != [b'.' as u16, b'.' as u16] {
                    each(units, id);
                }
                if next == 0 {
                    break;
                }
                at += next;
            }
        }
    }

    /// The names in the folder `h` is open on (with listing access).
    fn list(h: &Handle, buf: &mut [u64]) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        entries(h, buf, &FULL, |units, _| {
            names.push(OsString::from_wide(&units))
        })?;
        Ok(names)
    }

    /// Whether two names are the same but for case, as the system's file systems compare names
    /// (its upper-case table).
    fn same_but_case(a: &[u16], b: &[u16]) -> bool {
        let (Ok(la), Ok(lb)) = (i32::try_from(a.len()), i32::try_from(b.len())) else {
            return false;
        };
        // SAFETY: two buffers of the lengths given.
        unsafe { CompareStringOrdinal(a.as_ptr(), la, b.as_ptr(), lb, 1) == CSTR_EQUAL }
    }

    /// Whether the folder at `d`'s path is still `d` (volume and file index).
    fn still_there(d: &Dir) -> bool {
        open(
            &d.path,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            d.facts.reparse(),
        )
        .ok()
        .and_then(|h| identity(&h).ok())
            == Some((d.facts.volume, d.facts.index))
    }

    /// What [`listed_ids`] found.
    struct Listed {
        /// the folder at the path is the one being emptied, and it could be listed
        folder: bool,
        ids: HashMap<Vec<u16>, u64>,
    }

    /// The file id each name of the folder `d` lists has (FileIdBothDirectoryInformation, the id
    /// a handle's file index is), through a listing handle checked to be that folder. Ids that
    /// say nothing (0, all ones) are left out (FAT and exFAT list none); a folder that cannot be
    /// listed so has none.
    fn listed_ids(d: &Dir, buf: &mut [u64]) -> Listed {
        let mut ids = HashMap::new();
        let lister = match open(
            &d.path,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            d.facts.reparse(),
        ) {
            Ok(h) if identity(&h).ok() == Some((d.facts.volume, d.facts.index)) => h,
            _ => return Listed { folder: false, ids },
        };
        let listed = entries(&lister, buf, &ID_BOTH, |units, id| {
            if let Some(id) = id.filter(|&i| i != 0 && i != u64::MAX) {
                ids.insert(units, id);
            }
        });
        if listed.is_err() {
            ids.clear();
        }
        Listed {
            folder: listed.is_ok(),
            ids,
        }
    }

    /// `\\?\` and the absolute path (no Win32 rewriting, no MAX_PATH).
    fn verbatim(p: &Path) -> Vec<u16> {
        let abs = super::super::paths::absolute(p);
        let w: Vec<u16> = abs.as_os_str().encode_wide().collect();
        let starts = |s: &str| w.starts_with(&s.encode_utf16().collect::<Vec<u16>>());
        if starts("\\\\?\\") || starts("\\??\\") {
            w
        } else if starts("\\\\") {
            "\\\\?\\UNC\\"
                .encode_utf16()
                .chain(w[2..].iter().copied())
                .collect()
        } else {
            "\\\\?\\".encode_utf16().chain(w.iter().copied()).collect()
        }
    }

    fn join(parent: &[u16], name: &[u16]) -> Vec<u16> {
        let mut out = parent.to_vec();
        if out.last() != Some(&BACKSLASH) {
            out.push(BACKSLASH);
        }
        out.extend_from_slice(name);
        out
    }

    fn as_path(w: &[u16]) -> PathBuf {
        PathBuf::from(OsString::from_wide(w))
    }

    /// What all folders of one item share.
    struct Item {
        /// its volume
        volume: u32,
        /// its file system tells what a handle was opened as
        names_known: bool,
        /// its file system cannot hold names that differ only in case (FAT, exFAT)
        case_blind: bool,
        /// it may delete with POSIX semantics: cleared at the first refusal ([`delete`])
        posix: AtomicBool,
    }

    /// A folder being emptied (by one thread or several).
    pub(in super::super) struct Dir {
        /// what deletes it once it is empty: taken then, and closed right away (before its
        /// parent is deleted: a folder deleted without POSIX semantics goes when it is closed)
        h: Mutex<Option<Handle>>,
        facts: Facts,
        path: Vec<u16>,
        item: Arc<Item>,
        /// the file ids it lists its names with, read once a handle reports a name of it in
        /// another case ([`listed_ids`])
        ids: Mutex<Option<Listed>>,
    }

    /// List the folder `h` is open on: through a listing handle checked to be the same folder
    /// (for a folder that is a reparse point other than a link, cloud files, one its filter may
    /// fill in).
    fn frame(
        h: Handle,
        facts: Facts,
        path: Vec<u16>,
        item: Arc<Item>,
        buf: &mut [u64],
    ) -> io::Result<(Dir, Vec<OsString>)> {
        let lister = open(
            &path,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            facts.reparse(),
        )?;
        if identity(&lister)? != (facts.volume, facts.index) {
            return Err(super::moved_inside());
        }
        let names = super::seam_names(&as_path(&path), list(&lister, buf)?);
        Ok((
            Dir {
                h: Mutex::new(Some(h)),
                facts,
                path,
                item,
                ids: Mutex::new(None),
            },
            names,
        ))
    }

    /// Windows: every level on several threads.
    pub(in super::super) struct Win;

    impl Walk for Win {
        type Dir = Dir;
        type Scratch = Vec<u64>;

        fn scratch(&self) -> Vec<u64> {
            // 64 KiB exactly: WinBtrfs refuses a name buffer of a few bytes more
            vec![0u64; 65536 / 8]
        }

        fn start(&self, top: &Path, buf: &mut Vec<u64>) -> io::Result<(Dir, Vec<OsString>)> {
            seams::entering(top);
            let top_w = verbatim(top);
            let Some((h, f)) = open_entry(&top_w, buf)? else {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            };
            if !f.dir() || f.link() {
                return Err(super::moved_inside()); // replaced since the caller looked
            }
            let item = Arc::new(Item {
                volume: f.volume,
                names_known: f.name.is_some(),
                case_blind: case_blind(&h),
                posix: AtomicBool::new(true),
            });
            frame(h, f, top_w, item, buf)
        }

        fn visit(
            &self,
            dir: &Dir,
            _depth: usize,
            name: OsString,
            buf: &mut Vec<u64>,
            cancel: &dyn Fn() -> bool,
        ) -> Visit<Dir> {
            let units: Vec<u16> = name.encode_wide().collect();
            if !super::joinable(&units) {
                return Visit::Failed(super::bad_name_inside());
            }
            let path = join(&dir.path, &units);
            let expected = dir.facts.name.as_ref().map(|n| join(n, &units));
            seams::entering(&as_path(&path));
            let (h, facts) = match open_entry(&path, buf) {
                Ok(Some(found)) => found,
                Ok(None) => return Visit::Gone,
                Err(e) => return Visit::Failed(e),
            };
            // the very entry listed: on this volume, under this folder, by this name; a name
            // reported in another case (SMB redirectors, WinFsp and Dokan report the case the
            // first program to open it used) only as the entry the folder lists under the name,
            // by its file id (in a case-sensitive folder another case is another entry)
            let named = match (&facts.name, &expected) {
                _ if !dir.item.names_known => true,
                (Some(got), Some(want)) if got == want => true,
                (Some(got), Some(want)) if same_but_case(got, want) => {
                    let mut ids = dir.ids.lock();
                    let listed = ids.get_or_insert_with(|| listed_ids(dir, buf));
                    match listed.ids.get(&units) {
                        Some(&id) => id == facts.index,
                        // no file ids in its listing (FAT, exFAT): where names cannot differ
                        // only in case, this name in this very folder (checked, now, to be the
                        // one being emptied) is the entry listed
                        None => listed.folder && dir.item.case_blind && still_there(dir),
                    }
                }
                _ => false,
            };
            if facts.volume != dir.item.volume || !named {
                return Visit::Failed(super::moved_inside());
            }
            if facts.dir() && !facts.link() {
                if cancel() {
                    return Visit::Stop;
                }
                return match frame(h, facts, path, dir.item.clone(), buf) {
                    Ok((d, names)) => Visit::Folder(d, names),
                    Err(e) => Visit::Failed(e),
                };
            }
            if facts.dir() && facts.tag == TAG_MOUNT_POINT && mounts_a_volume(&h, &path) {
                return Visit::Failed(super::mount_inside());
            }
            // a file, or a link of any kind: the entry itself, never what it points to
            match delete(&h, &path, &facts, &dir.item.posix) {
                Ok(()) => Visit::Deleted,
                Err(e) => Visit::Failed(e),
            }
        }

        fn remove(&self, dir: &Dir, _parent: Option<&Dir>, _top: &Path) -> io::Result<()> {
            let Some(h) = dir.h.lock().take() else {
                return Ok(());
            };
            let gone = delete(&h, &dir.path, &dir.facts, &dir.item.posix);
            drop(h);
            match gone {
                Err(e) if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY) => remove_when_empty(dir),
                other => other,
            }
        }

        fn keep(&self, dir: &Dir) {
            drop(dir.h.lock().take());
        }
    }

    /// Whether the file system `h` is on cannot hold names that differ only in case (no
    /// FILE_CASE_SENSITIVE_SEARCH: FAT, exFAT); false when it cannot be told.
    fn case_blind(h: &Handle) -> bool {
        let mut flags = 0u32;
        probe::call(Call::Query);
        // SAFETY: no name buffers asked for; an out pointer for the flags.
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                h.0,
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                &mut flags,
                null_mut(),
                0,
            )
        } != 0;
        ok && flags & FILE_CASE_SENSITIVE_SEARCH == 0
    }

    /// A folder whose every entry was deleted is not empty yet: on a file system without POSIX
    /// semantics (FAT, exFAT, network shares) a file deleted while another program holds it open
    /// sharing its deletion (an antivirus scanner, a preview, a sync or backup program) keeps its
    /// name until that program closes it. Asked again for a moment ([`SETTLE_MS`]), the same
    /// folder (checked) by its path; then it says why it stays.
    fn remove_when_empty(dir: &Dir) -> io::Result<()> {
        for ms in SETTLE_MS {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let h = match open(
                &dir.path,
                DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                false,
            ) {
                Ok(h) => h,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()), // gone after all
                Err(e) => return Err(e),
            };
            if identity(&h)? != (dir.facts.volume, dir.facts.index) {
                return Err(super::moved_inside());
            }
            let gone = delete(&h, &dir.path, &dir.facts, &dir.item.posix);
            drop(h);
            match gone {
                Err(e) if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY) => continue,
                other => return other,
            }
        }
        Err(why_not_empty(dir))
    }

    /// Why a folder whose every entry was deleted is still not empty: its entries' deletions
    /// still pending (another program holds them open), or something put into it meanwhile.
    fn why_not_empty(dir: &Dir) -> io::Error {
        let generic = || io::Error::other(text("not_empty"));
        let mut buf = vec![0u64; 65536 / 8];
        let Ok(lister) = open(
            &dir.path,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            false,
        ) else {
            return generic();
        };
        if identity(&lister).ok() != Some((dir.facts.volume, dir.facts.index)) {
            return generic();
        }
        let Ok(names) = list(&lister, &mut buf) else {
            return generic();
        };
        let (mut pending, mut there) = (0usize, 0usize);
        for name in names.iter().take(256) {
            let units: Vec<u16> = name.encode_wide().collect();
            // to look only, not following a link at its end
            match open_nt(
                &join(&dir.path, &units),
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                false,
            ) {
                Ok(_) => there += 1,
                Err(failed) if delete_pending(&failed) => pending += 1,
                Err(_) => {}
            }
        }
        if there > 0 {
            io::Error::other(text("new_inside"))
        } else if pending > 0 {
            io::Error::other(text("pending_inside"))
        } else {
            generic()
        }
    }
}
