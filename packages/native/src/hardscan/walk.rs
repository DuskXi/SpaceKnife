//! Parallel depth-first walk of a slice's tasks, then a preorder pass that turns the walked tree
//! into worker.py's slice layout.
//!
//! Every directory is one rayon job: it checks the budget, lists the directory into its thread's
//! arena and spawns one job per subdirectory (in reverse, so the owning thread continues with the
//! first one: depth-first; idle threads steal the oldest, i.e. the biggest, pending subtrees).
//! Spawned jobs live on rayon's deques, not on the stack, so tree depth costs no stack.
//! Results hang off the tree and are serialised afterwards in preorder, which is exactly the
//! order worker.scan_slice lists in, so ``(1, L, j)`` references always point at earlier
//! listings and an untruncated slice is identical to the worker's.
//!
//! A listed directory may hand its subdirectories a way to open them from it (`sys::Via`; on
//! Windows its open handle, docs/native.md 2.2). Each subdirectory takes it when its job starts,
//! so a directory stays open only until its last subdirectory has started.
//!
//! Windows folders a name cannot reach (case-only siblings, case.rs) are not left to the frontier
//! like the others once the budget is spent: as a task a path would open them by name. A folder
//! opened by file id, and everything below it, is *pinned*: listed in this call, up to a cap, and
//! past it an error, never a task. Folders whose name another entry shares are listed in this call
//! up to the same cap too; past it the ones a task can still check go to the frontier.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use rayon::{Scope, ThreadPool, ThreadPoolBuilder};

use super::case::{mark, msg};
use super::common::{Arena, NativePath, RootOpts};
use super::sys;

pub type Ref = [i64; 3];

pub struct Node {
    pub path: NativePath,
    pub root: u32,
    /// Found by this call (open without following a symlink) rather than given as a task.
    pub discovered: bool,
    pub care: Care,
    /// How to open it from its listed parent (taken when its job starts).
    pub via: Mutex<Option<sys::Via>>,
    pub state: OnceLock<State>,
}

/// How a node may be put off once the budget is spent (`past_budget`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Care {
    /// A task, or a subfolder no other entry shares its name with: to the frontier.
    Plain,
    /// A subfolder whose name another entry shares (`case::mark` bits): as a task its path could
    /// open the other entry.
    Strict(u8),
    /// Below a folder opened by file id: no path leads there at all.
    Pinned,
}

/// What `process` does with a node once the budget is spent (docs/native.md 2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Past {
    Defer,
    List,
    Fail(&'static str),
}

/// `budget_positive`: the call has a time budget (at 0 the monitor lists one folder alone: what
/// lies below it stays as it is). `cap_left`: the entries listed past the budget (`Ctx::extra`)
/// are still below the call's cap.
fn past_budget(care: Care, budget_positive: bool, cap_left: bool) -> Past {
    let listed = budget_positive && cap_left;
    match care {
        Care::Plain => Past::Defer,
        Care::Strict(_) | Care::Pinned if listed => Past::List,
        // past the cap: a task could not tell these apart from the entry its name reaches
        Care::Strict(marks) if marks & (mark::BADNAME | mark::DUP) != 0 && budget_positive => {
            Past::Fail(msg::CAP)
        }
        // at budget 0 (the relisted folder alone): the same, in their own words
        Care::Strict(marks) if marks & mark::BADNAME != 0 => Past::Fail(msg::BADNAME),
        Care::Strict(marks) if marks & mark::DUP != 0 => Past::Fail(msg::DUP),
        // a task: on Btrfs its stored names are checked (win.rs check_task)
        Care::Strict(_) => Past::Defer,
        // no path leads there at all
        Care::Pinned => Past::Fail(msg::CAP),
    }
}

pub enum State {
    Listed(Listed),
    Failed(String),
    Deferred,
}

pub struct Listed {
    pub arena: usize,
    pub start: usize,
    pub count: usize,
    pub name_start: usize,
    pub name_end: usize,
    pub children: Box<[Child]>,
}

pub struct Child {
    pub j: u32,
    pub node: Node,
}

impl Node {
    pub fn new(path: NativePath, root: u32, discovered: bool) -> Self {
        Self::with_via(path, root, discovered, Care::Plain, None)
    }

    pub fn with_via(
        path: NativePath,
        root: u32,
        discovered: bool,
        care: Care,
        via: Option<sys::Via>,
    ) -> Self {
        Self {
            path,
            root,
            discovered,
            care,
            via: Mutex::new(via),
            state: OnceLock::new(),
        }
    }

    fn take_via(&self) -> Option<sys::Via> {
        self.via.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn take_children(&mut self, into: &mut Vec<Node>) {
        if let Some(State::Listed(l)) = self.state.take() {
            into.extend(l.children.into_vec().into_iter().map(|c| c.node));
        }
    }
}

impl Drop for Node {
    /// Iterative: walked trees can be thousands of levels deep.
    fn drop(&mut self) {
        let mut stack = Vec::new();
        self.take_children(&mut stack);
        while let Some(mut node) = stack.pop() {
            node.take_children(&mut stack);
        }
    }
}

pub struct Ctx {
    pub roots: Vec<RootOpts>,
    pub root_state: Vec<sys::RootState>,
    pub deadline: Option<Instant>,
    /// A time budget above 0 (at 0 a call lists its first folder only: the monitor's relists).
    pub budget_positive: bool,
    pub max_entries: usize,
    /// Entries listed once the budget was spent (of pinned folders and of folders whose name
    /// another entry shares, the only ones listed then), and how many may be: it bounds the extra
    /// time and memory of a call (about two slices' worth) and the wait on cancel or pause.
    pub extra_cap: usize,
    pub extra: AtomicUsize,
    pub listed: AtomicUsize,
    pub entries: AtomicUsize,
    pub arenas: Vec<Mutex<Arena>>,
}

impl Ctx {
    /// worker.py's loop condition: stop once something was listed and the budget is spent.
    fn should_stop(&self) -> bool {
        self.listed.load(Ordering::Relaxed) > 0
            && (self.entries.load(Ordering::Relaxed) >= self.max_entries
                || self.deadline.is_some_and(|d| Instant::now() >= d))
    }
}

/// Pools kept alive between calls (by size, most recently used last); the IO limiter may vary
/// the thread count, and an evicted pool's threads exit once its last call returns.
const KEEP_POOLS: usize = 4;

/// The thread pool of a given size, reused across calls. A forked child inherits the pool
/// objects but not their threads, so pools from another pid are leaked and rebuilt.
///
/// A one-thread pool belongs to the calling thread: the scanner runs several one-thread slices at
/// once, one per Python thread, and a shared one-thread pool would list them one after another.
pub fn pool(threads: usize) -> Result<Arc<ThreadPool>, String> {
    if threads == 1 {
        return own_pool();
    }
    type Pools = (u32, Vec<(usize, Arc<ThreadPool>)>); // (owning pid, pools)
    static POOLS: Mutex<Pools> = Mutex::new((0, Vec::new()));
    let mut guard = POOLS.lock().unwrap_or_else(|e| e.into_inner());
    let (pid, pools) = &mut *guard;
    if *pid != std::process::id() {
        std::mem::forget(std::mem::take(pools));
        *pid = std::process::id();
    }
    if let Some(i) = pools.iter().position(|(n, _)| *n == threads) {
        let hit = pools.remove(i);
        pools.push(hit.clone());
        return Ok(hit.1);
    }
    let p = Arc::new(build(threads)?);
    pools.push((threads, p.clone()));
    if pools.len() > KEEP_POOLS {
        pools.remove(0);
    }
    Ok(p)
}

/// The calling thread's one-thread pool; it ends with that thread.
fn own_pool() -> Result<Arc<ThreadPool>, String> {
    thread_local! {
        static OWN: RefCell<Option<(u32, Arc<ThreadPool>)>> = const { RefCell::new(None) };
    }
    OWN.with(|own| {
        let mut own = own.borrow_mut();
        match own.take() {
            Some((pid, p)) if pid == std::process::id() => {
                *own = Some((pid, p.clone()));
                return Ok(p);
            }
            Some((_, p)) => std::mem::forget(p), // a forked child: the pool's thread is gone
            None => {}
        }
        let p = Arc::new(build(1)?);
        *own = Some((std::process::id(), p.clone()));
        Ok(p)
    })
}

fn build(threads: usize) -> Result<ThreadPool, String> {
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("spaceknife-scan-{i}"))
        .build()
        .map_err(|e| e.to_string())
}

/// One per logical CPU, at most 16: directory opens contend on file-system locks, and on a
/// warm cache more threads stop paying off (measured on NTFS) while still costing CPU.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 16)
}

fn process<'s>(scope: &Scope<'s>, node: &'s Node, ctx: &'s Ctx) {
    let via = node.take_via(); // the parent's handle goes once its last subdirectory has started
    let stopped = ctx.should_stop();
    if stopped {
        let cap_left = ctx.extra.load(Ordering::Relaxed) < ctx.extra_cap;
        match past_budget(node.care, ctx.budget_positive, cap_left) {
            Past::List => {}
            Past::Defer => {
                let _ = node.state.set(State::Deferred);
                return;
            }
            Past::Fail(why) => {
                if why == msg::CAP {
                    sys::cap_hit();
                }
                let _ = node.state.set(State::Failed(why.to_owned()));
                return;
            }
        }
    }
    let slot = rayon::current_thread_index().unwrap_or(0) % ctx.arenas.len();
    let mut arena = ctx.arenas[slot].lock().unwrap_or_else(|e| e.into_inner());
    let mark = arena.mark();
    let mut subdirs: Vec<(u32, NativePath, sys::Hint)> = Vec::new();
    let root = node.root as usize;
    let res = sys::list_dir(
        &node.path,
        node.discovered,
        via,
        &ctx.roots[root],
        &ctx.root_state[root],
        &mut arena,
        &mut subdirs,
    );
    match res {
        Ok((parent, pinned)) => {
            let count = arena.len() - mark.entries;
            let name_end = arena.names.len();
            drop(arena);
            ctx.entries.fetch_add(count, Ordering::Relaxed);
            ctx.listed.fetch_add(1, Ordering::Relaxed);
            if stopped {
                ctx.extra.fetch_add(count, Ordering::Relaxed); // strict or pinned: nothing else
            }
            let children: Box<[Child]> = subdirs
                .into_iter()
                .map(|(j, path, hint)| {
                    let marks = sys::marks(&hint) & mark::STRICT;
                    let care = if pinned {
                        Care::Pinned
                    } else if marks != 0 {
                        Care::Strict(marks)
                    } else {
                        Care::Plain
                    };
                    let via = parent.as_ref().and_then(|p| sys::via(p, hint, pinned));
                    Child {
                        j,
                        node: Node::with_via(path, node.root, true, care, via),
                    }
                })
                .collect();
            let listed = Listed {
                arena: slot,
                start: mark.entries,
                count,
                name_start: mark.names,
                name_end,
                children,
            };
            if node.state.set(State::Listed(listed)).is_err() {
                return;
            }
            if let Some(State::Listed(l)) = node.state.get() {
                for c in l.children.iter().rev() {
                    scope.spawn(move |s| process(s, &c.node, ctx));
                }
            }
        }
        Err(msg) => {
            arena.truncate(mark);
            drop(arena);
            let _ = node.state.set(State::Failed(msg));
        }
    }
}

/// Walks `nodes` (the slice's tasks, in order) on `pool`.
pub fn walk(pool: &ThreadPool, nodes: &[Node], ctx: &Ctx) {
    pool.install(|| {
        rayon::scope(|s| {
            for node in nodes.iter().rev() {
                s.spawn(move |s| process(s, node, ctx));
            }
        })
    });
}

/// Where a path or task in the output comes from.
pub enum Origin<'a> {
    /// The input task with this index (its Python objects are reused).
    Input(usize),
    /// A subdirectory found by this call.
    Found(&'a Node),
}

pub enum ErrRef {
    Input(usize),
    Child(Ref),
}

/// The slice in worker.py's layout, before the Python objects are made.
#[derive(Default)]
pub struct Flat<'a> {
    pub refs: Vec<i64>,
    pub listing_paths: Vec<Origin<'a>>,
    pub bounds: Vec<i64>,
    pub names: Vec<u8>,
    pub name_lens: Vec<u16>,
    pub kind: Vec<u8>,
    pub flags: Vec<u32>,
    pub size: Vec<i64>,
    pub alloc: Vec<i64>,
    pub mtime: Vec<u32>,
    pub ctime: Vec<u32>,
    pub atime: Vec<u32>,
    pub ino: Vec<u64>,
    pub nlink: Vec<u16>,
    pub dev: Vec<u64>,
    /// Per entry: index into `raw_exts` of its not-yet-lowercased extension (0 = "").
    pub raw_ext: Vec<u32>,
    pub raw_exts: Vec<&'a [u8]>,
    pub frontier: Vec<(Option<Ref>, Origin<'a>)>,
    pub errors: Vec<(ErrRef, Origin<'a>, &'a str)>,
}

/// Preorder over the walked forest; `input_refs[i]` is task i's reference.
pub fn flatten<'a>(nodes: &'a [Node], input_refs: &[Ref], arenas: &'a [Arena]) -> Flat<'a> {
    let mut f = Flat {
        bounds: vec![0],
        raw_exts: vec![b"".as_slice()],
        ..Flat::default()
    };
    let total: usize = arenas.iter().map(Arena::len).sum();
    f.names.reserve(arenas.iter().map(|a| a.names.len()).sum());
    f.name_lens.reserve(total);
    f.kind.reserve(total);
    f.flags.reserve(total);
    f.size.reserve(total);
    f.alloc.reserve(total);
    f.mtime.reserve(total);
    f.ctime.reserve(total);
    f.atime.reserve(total);
    f.ino.reserve(total);
    f.nlink.reserve(total);
    f.dev.reserve(total);
    f.raw_ext.reserve(total);
    let mut ext_index: HashMap<&[u8], u32> = HashMap::new();
    ext_index.insert(b"", 0);

    // (node, its reference, Some(task index) for an input task)
    let mut stack: Vec<(&'a Node, Ref, Option<usize>)> = Vec::new();
    for (i, node) in nodes.iter().enumerate().rev() {
        stack.push((node, input_refs[i], Some(i)));
    }
    while let Some((node, r, input)) = stack.pop() {
        let origin = || match input {
            Some(i) => Origin::Input(i),
            None => Origin::Found(node),
        };
        match node.state.get() {
            Some(State::Listed(l)) => {
                let listing = (f.listing_paths.len()) as i64;
                f.refs.extend_from_slice(&r);
                f.listing_paths.push(origin());
                let a = &arenas[l.arena];
                let (lo, hi) = (l.start, l.start + l.count);
                let names = &a.names[l.name_start..l.name_end];
                f.names.extend_from_slice(names);
                f.name_lens.extend_from_slice(&a.name_lens[lo..hi]);
                f.kind.extend_from_slice(&a.kind[lo..hi]);
                f.flags.extend_from_slice(&a.flags[lo..hi]);
                f.size.extend_from_slice(&a.size[lo..hi]);
                f.alloc.extend_from_slice(&a.alloc[lo..hi]);
                f.mtime.extend_from_slice(&a.mtime[lo..hi]);
                f.ctime.extend_from_slice(&a.ctime[lo..hi]);
                f.atime.extend_from_slice(&a.atime[lo..hi]);
                f.ino.extend_from_slice(&a.ino[lo..hi]);
                f.nlink.extend_from_slice(&a.nlink[lo..hi]);
                f.dev.extend_from_slice(&a.dev[lo..hi]);
                let mut off = 0usize;
                for k in lo..hi {
                    let end = off + a.name_lens[k] as usize;
                    let el = a.ext_lens[k] as usize;
                    let id = if el == 0 {
                        0
                    } else {
                        let ext = &names[end - el..end];
                        let next = f.raw_exts.len() as u32;
                        *ext_index.entry(ext).or_insert_with(|| {
                            f.raw_exts.push(ext);
                            next
                        })
                    };
                    f.raw_ext.push(id);
                    off = end;
                }
                f.bounds.push(f.kind.len() as i64);
                for c in l.children.iter().rev() {
                    stack.push((&c.node, [1, listing, c.j as i64], None));
                }
            }
            Some(State::Failed(why)) => {
                let er = match input {
                    Some(i) => ErrRef::Input(i),
                    None => ErrRef::Child(r),
                };
                f.errors.push((er, origin(), why.as_str()));
            }
            // a pinned folder's path leads elsewhere, or nowhere: never a task
            Some(State::Deferred) | None if node.care == Care::Pinned => {
                f.errors.push((ErrRef::Child(r), origin(), msg::CAP));
            }
            Some(State::Deferred) | None => {
                f.frontier.push((input.is_none().then_some(r), origin()));
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_thread_pools_belong_to_their_caller() {
        let mine = pool(1).unwrap();
        assert!(Arc::ptr_eq(&mine, &pool(1).unwrap()));
        let theirs = std::thread::spawn(|| pool(1).unwrap()).join().unwrap();
        assert!(!Arc::ptr_eq(&mine, &theirs));
        // concurrent one-thread slices run side by side, not one after another
        let busy = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (busy, peak) = (busy.clone(), peak.clone());
                std::thread::spawn(move || {
                    pool(1).unwrap().install(|| {
                        let now = busy.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        busy.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) > 1);
    }

    #[test]
    fn what_waits_once_the_budget_is_spent() {
        use mark::{BADNAME, DUP, TWIN};
        // every cell of the table in docs/native.md 2.2: budget above 0 with entries left below
        // the cap, above 0 past the cap, then 0 (the monitor's relist of one folder)
        let cells: [(Care, [Past; 3]); 5] = [
            (Care::Plain, [Past::Defer, Past::Defer, Past::Defer]),
            (Care::Strict(TWIN), [Past::List, Past::Defer, Past::Defer]),
            (Care::Strict(TWIN | DUP), [Past::List, Past::Fail(msg::CAP), Past::Fail(msg::DUP)]),
            (Care::Strict(BADNAME), [Past::List, Past::Fail(msg::CAP), Past::Fail(msg::BADNAME)]),
            (Care::Pinned, [Past::List, Past::Fail(msg::CAP), Past::Fail(msg::CAP)]),
        ];
        for (care, want) in cells {
            assert_eq!(past_budget(care, true, true), want[0], "{care:?}");
            assert_eq!(past_budget(care, true, false), want[1], "{care:?}");
            for cap_left in [true, false] {
                assert_eq!(past_budget(care, false, cap_left), want[2], "{care:?}");
            }
        }
        assert_eq!(past_budget(Care::Strict(TWIN | BADNAME), false, true), Past::Fail(msg::BADNAME));
    }

    #[test]
    fn bigger_pools_are_shared() {
        let a = pool(3).unwrap();
        let b = std::thread::spawn(|| pool(3).unwrap()).join().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
