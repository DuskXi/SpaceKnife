//! Parallel depth-first walk of a slice's tasks, then a preorder pass that turns the walked tree
//! into worker.py's slice layout.
//!
//! A directory is listed only while it holds a slot of each limit its call has (slots.rs): the
//! shared carriers' width (pace.rs) and the disk's gate (the backend's disk-load limit). A thread
//! checks the budget, lists the directory into its arena and parks its subdirectories on the
//! call's stack (`Parked`, the first one on top); the directory on top then takes over the slots
//! and the same thread goes on with it (depth-first, no job spawned), and parked directories that
//! get free slots become rayon jobs that idle threads take. Threads never wait for a slot: a call
//! whose directories all wait has its calling thread wait for one (and settle what the budget or
//! a paused gate puts off). Parked directories live on the heap, not on the stack, so tree depth
//! costs no stack.
//! Results hang off the tree and are serialised afterwards in preorder, which is exactly the
//! order worker.scan_slice lists in, so ``(1, L, j)`` references always point at earlier
//! listings and an untruncated slice is identical to the worker's, whatever the width.
//!
//! A listed directory may hand its subdirectories a way to open them from it (`sys::Via`; on
//! Windows its open handle, docs/native.md 2.2). Each subdirectory takes it when it is listed (or
//! put off), so a directory stays open only until its last subdirectory has started.
//!
//! Windows folders a name cannot reach (case-only siblings, case.rs) are not left to the frontier
//! like the others once the budget is spent: as a task a path would open them by name. A folder
//! opened by file id, and everything below it, is *pinned*: listed in this call, up to a cap, and
//! past it an error, never a task. Folders whose name another entry shares are listed in this call
//! up to the same cap too; past it the ones a task can still check go to the frontier.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::{Scope, ThreadPool, ThreadPoolBuilder};

use super::case::{mark, msg};
use super::common::{Arena, NativePath, RootOpts};
use super::slots::{self, Slots};
use super::{pace, sys};

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
    /// The width of the shared carriers (pace.rs), for a call listing on them.
    pub width: Option<&'static Slots>,
    /// The disk's gate (slots.rs `ListingGate`), when the call names one.
    pub gate: Option<Arc<Slots>>,
    /// The adaptive width, told about every listing of a call on the carriers.
    pub pace: Option<&'static pace::Pace>,
    /// Folders of this call holding their slots, and how many may at once: the threads of its
    /// pool (an admitted folder never waits in a queue holding slots another call could use);
    /// on the carriers also its share of the width (`Ctx::share`).
    pub running: AtomicUsize,
    pub most_running: usize,
}

/// Calls listing on the shared carriers now (`walk`).
static CALLS: AtomicUsize = AtomicUsize::new(0);

/// What becomes of a node offered a slot (`Ctx::admit`).
enum Admit {
    /// Put off or failed without a listing (its state is set).
    Resolved,
    /// It holds a slot of each limit: list it.
    Taken,
    /// A limit is full (or the gate paused): it waits.
    Refused,
}

impl Ctx {
    /// worker.py's loop condition: stop once something was listed and the budget is spent.
    fn should_stop(&self) -> bool {
        self.listed.load(Ordering::Relaxed) > 0
            && (self.entries.load(Ordering::Relaxed) >= self.max_entries
                || self.deadline.is_some_and(|d| Instant::now() >= d))
    }

    /// Past the budget, or the disk's gate paused: only what cannot wait is listed (`past_budget`).
    fn stopped(&self) -> bool {
        self.should_stop() || self.gate.as_ref().is_some_and(|g| g.paused())
    }

    /// What `past_budget` says for `node` now (`stopped`: `Ctx::stopped`), its state set when it
    /// is not to be listed. True: list it.
    fn settle(&self, node: &Node, stopped: bool) -> bool {
        if !stopped {
            return true;
        }
        let cap_left = self.extra.load(Ordering::Relaxed) < self.extra_cap;
        match past_budget(node.care, self.budget_positive, cap_left) {
            Past::List => true,
            Past::Defer => {
                drop(node.take_via());
                let _ = node.state.set(State::Deferred);
                false
            }
            Past::Fail(why) => {
                drop(node.take_via());
                if why == msg::CAP {
                    sys::cap_hit();
                }
                let _ = node.state.set(State::Failed(why.to_owned()));
                false
            }
        }
    }

    /// A slot of each limit for `node`, unless it is settled without a listing. A folder that must
    /// be listed in this call goes through a paused gate (never past its limit).
    fn admit(&self, node: &Node, stopped: bool) -> Admit {
        if !self.settle(node, stopped) {
            return Admit::Resolved;
        }
        if self.running.load(Ordering::Relaxed) >= self.share() {
            return Admit::Refused;
        }
        if let Some(w) = self.width
            && !w.try_take(true)
        {
            return Admit::Refused;
        }
        if let Some(g) = &self.gate
            && !g.try_take(stopped)
        {
            if let Some(w) = self.width {
                w.untake();
            }
            return Admit::Refused;
        }
        self.running.fetch_add(1, Ordering::Relaxed);
        Admit::Taken
    }

    /// How many of its folders a call may list at once. On the carriers: its share of the width
    /// among the calls on them, rounded up (all of it for a call alone, as at a scan's start; one
    /// each for as many calls as the width, as the scanner's calls are on a warm cache; two each
    /// at the cold width). A call that lists several folders at once lists siblings at the same
    /// time, which costs the file system more: on ReFS, warm, about 15 % more kernel time than
    /// one folder at a time per call, for the same wall time.
    fn share(&self) -> usize {
        match self.width {
            Some(w) => w
                .limit()
                .div_ceil(CALLS.load(Ordering::Relaxed).max(1))
                .clamp(1, self.most_running),
            None => self.most_running,
        }
    }

    /// The slots of a listing that took `took` go back.
    fn give(&self, took: Duration) {
        self.running.fetch_sub(1, Ordering::Relaxed);
        if let Some(g) = &self.gate {
            g.give(took);
        }
        if let Some(w) = self.width {
            w.give(took);
        }
    }

    /// A listing that took `took` hands its slots to the next folder of the call: counted, not
    /// given back (no waiting call is woken for a slot that is taken again at once).
    fn hand_over(&self, took: Duration) {
        if let Some(g) = &self.gate {
            g.note(took);
        }
    }

    /// The slots held may go to the next folder: no limit was lowered below the listings in
    /// flight, and the gate is not paused (but for a folder that must be listed: `stopped`).
    fn may_keep(&self, stopped: bool) -> bool {
        self.width.is_none_or(|w| w.inflight() <= w.limit())
            && self
                .gate
                .as_ref()
                .is_none_or(|g| g.inflight() <= g.limit() && (stopped || !g.paused()))
    }

    /// How long a call that can start none of its folders waits before it looks again (a slot
    /// given back wakes it sooner): until its deadline, when it could stop then.
    fn wait_time(&self) -> Duration {
        match self.deadline {
            Some(d) if self.listed.load(Ordering::Relaxed) > 0 => d
                .saturating_duration_since(Instant::now())
                .clamp(Duration::from_millis(1), WAIT_MAX),
            _ => WAIT_MAX,
        }
    }
}

const WAIT_MAX: Duration = Duration::from_millis(10);

/// A call's folders waiting for a slot, the next one on top (children are pushed in reverse, so a
/// carrier goes on with the first subfolder of the folder it listed: depth-first).
pub struct Parked<'n> {
    stack: Mutex<Vec<&'n Node>>,
    any: AtomicBool,
}

impl<'n> Parked<'n> {
    fn new(nodes: &'n [Node]) -> Self {
        Self {
            stack: Mutex::new(nodes.iter().rev().collect()),
            any: AtomicBool::new(!nodes.is_empty()),
        }
    }

    fn push(&self, children: &'n [Child]) {
        if children.is_empty() {
            return;
        }
        let mut st = self.stack.lock().unwrap_or_else(|e| e.into_inner());
        st.extend(children.iter().rev().map(|c| &c.node));
        self.any.store(true, Ordering::Release);
    }

    /// Takes off the top every node that is settled or gets its slots, until one is refused;
    /// those to list go to `out`, the next one first.
    fn admit(&self, ctx: &Ctx, out: &mut Vec<&'n Node>) {
        if !self.any.load(Ordering::Acquire) {
            return;
        }
        let mut st = self.stack.lock().unwrap_or_else(|e| e.into_inner());
        let stopped = ctx.stopped();
        while let Some(&node) = st.last() {
            match ctx.admit(node, stopped) {
                Admit::Resolved => {}
                Admit::Taken => out.push(node),
                Admit::Refused => break,
            }
            st.pop();
        }
        self.any.store(!st.is_empty(), Ordering::Release);
    }

    /// After a listing whose slots are still held: the first folder to list takes them over (when
    /// `Ctx::may_keep`), then `admit` goes on. True: they were taken over (`out[0]` holds them).
    fn admit_after(&self, ctx: &Ctx, out: &mut Vec<&'n Node>) -> bool {
        if !self.any.load(Ordering::Acquire) {
            return false;
        }
        let mut st = self.stack.lock().unwrap_or_else(|e| e.into_inner());
        let stopped = ctx.stopped();
        let mut kept = false;
        while let Some(&node) = st.last() {
            let admitted = if kept {
                ctx.admit(node, stopped)
            } else if !ctx.settle(node, stopped) {
                Admit::Resolved
            } else if ctx.may_keep(stopped) {
                kept = true;
                Admit::Taken
            } else {
                Admit::Refused
            };
            match admitted {
                Admit::Resolved => {}
                Admit::Taken => out.push(node),
                Admit::Refused => break,
            }
            st.pop();
        }
        self.any.store(!st.is_empty(), Ordering::Release);
        kept
    }

    fn is_empty(&self) -> bool {
        !self.any.load(Ordering::Acquire)
    }

    /// The calling thread: at least one node to list, or none left; waits for a slot meanwhile.
    fn admit_waiting(&self, ctx: &Ctx, out: &mut Vec<&'n Node>) {
        loop {
            self.admit(ctx, out);
            if !out.is_empty() || self.is_empty() {
                return;
            }
            slots::wait(
                || {
                    self.admit(ctx, out);
                    !out.is_empty() || self.is_empty()
                },
                ctx.wait_time(),
            );
        }
    }
}

/// Private pools kept alive between calls (by size, most recently used last); an evicted pool's
/// threads exit once its last call returns.
const KEEP_POOLS: usize = 4;

/// A private pool of `threads` threads (`threads` >= 1), reused across calls. A forked child
/// inherits the pool objects but not their threads, so pools from another pid are leaked and
/// rebuilt.
///
/// A one-thread pool belongs to the calling thread: several one-thread calls at once each list on
/// their own thread.
pub fn pool(threads: usize) -> Result<Arc<ThreadPool>, String> {
    if threads <= 1 {
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
    let p = Arc::new(build(threads, "spaceknife-scan")?);
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
        let p = Arc::new(build(1, "spaceknife-scan")?);
        *own = Some((std::process::id(), p.clone()));
        Ok(p)
    })
}

/// The process's shared carriers (`threads` 0), `capacity` threads (pace.rs): every call on them
/// lists its folders on any of them, within the width. A new capacity is a new pool; calls already
/// on the old one finish there, and its threads exit after the last. Rebuilt in a forked child.
pub fn carriers(capacity: usize) -> Result<Arc<ThreadPool>, String> {
    type Current = (u32, Option<(usize, Arc<ThreadPool>)>); // (owning pid, pool)
    static CARRIERS: Mutex<Current> = Mutex::new((0, None));
    let mut guard = CARRIERS.lock().unwrap_or_else(|e| e.into_inner());
    let (pid, current) = &mut *guard;
    if *pid != std::process::id() {
        std::mem::forget(current.take());
        *pid = std::process::id();
    }
    if let Some((n, p)) = current.as_ref()
        && *n == capacity
    {
        return Ok(p.clone());
    }
    let p = Arc::new(build(capacity, "spaceknife-carrier")?);
    *current = Some((capacity, p.clone()));
    Ok(p)
}

fn build(threads: usize, name: &'static str) -> Result<ThreadPool, String> {
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(move |i| format!("{name}-{i}"))
        .build()
        .map_err(|e| e.to_string())
}

/// Lists `node`, which holds a slot of each limit (`Ctx::admit`); then the call's next parked
/// folder takes its slots over and this thread goes on with it (no job, no thread woken, no slot
/// given back and taken again), and parked folders that get free slots become jobs for idle
/// threads. With nothing to take them over, the slots go back.
fn process<'s, 'n: 's>(scope: &Scope<'s>, node: &'n Node, ctx: &'s Ctx, parked: &'s Parked<'n>) {
    let mut node = node;
    let mut ready = Vec::new();
    loop {
        let took = list_one(node, ctx, parked);
        ready.clear();
        if parked.admit_after(ctx, &mut ready) {
            ctx.hand_over(took);
        } else {
            ctx.give(took);
        }
        let Some((&next, rest)) = ready.split_first() else {
            return;
        };
        for &other in rest.iter().rev() {
            scope.spawn(move |s| process(s, other, ctx, parked));
        }
        node = next;
    }
}

/// Lists one admitted node (or settles it, when the budget was spent or the gate paused since it
/// was admitted) and parks its subfolders; its slots are still held. Returns how long the listing
/// took.
fn list_one<'n>(node: &'n Node, ctx: &Ctx, parked: &Parked<'n>) -> Duration {
    let stopped = ctx.stopped();
    if !ctx.settle(node, stopped) {
        return Duration::ZERO;
    }
    let via = node.take_via(); // the parent's handle goes once its last subdirectory has started
    let slot = rayon::current_thread_index().unwrap_or(0) % ctx.arenas.len();
    let mut arena = ctx.arenas[slot].lock().unwrap_or_else(|e| e.into_inner());
    let mark = arena.mark();
    let mut subdirs: Vec<(u32, NativePath, sys::Hint)> = Vec::new();
    let root = node.root as usize;
    let started = Instant::now();
    let res = sys::list_dir(
        &node.path,
        node.discovered,
        via,
        &ctx.roots[root],
        &ctx.root_state[root],
        &mut arena,
        &mut subdirs,
    );
    let took = started.elapsed();
    match res {
        Ok((parent, pinned)) => {
            let count = arena.len() - mark.entries;
            let name_end = arena.names.len();
            drop(arena);
            if let Some(p) = ctx.pace {
                p.listed(took, count);
            }
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
                    #[cfg(windows)]
                    let via = parent.as_ref().and_then(|p| sys::via(p, hint, pinned));
                    #[cfg(unix)]
                    let via = {
                        let _ = (&parent, pinned);
                        sys::via_hint(hint)
                    };
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
            if node.state.set(State::Listed(listed)).is_ok()
                && let Some(State::Listed(l)) = node.state.get()
            {
                parked.push(&l.children);
            }
        }
        Err(msg) => {
            arena.truncate(mark);
            drop(arena);
            let _ = node.state.set(State::Failed(msg));
        }
    }
    took
}

/// Walks `nodes` (the slice's tasks, in order) on `pool`. The calling thread admits the first
/// folders (waiting for a slot when its limits are full), the carriers the rest as they go; when
/// the carriers run out of admitted folders while some still wait, it admits them again.
pub fn walk(pool: &ThreadPool, nodes: &[Node], ctx: &Ctx) {
    struct OnCarriers(bool);
    impl Drop for OnCarriers {
        fn drop(&mut self) {
            if self.0 {
                CALLS.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    let _call = OnCarriers(ctx.width.is_some());
    if ctx.width.is_some() {
        CALLS.fetch_add(1, Ordering::Relaxed);
    }
    let parked = Parked::new(nodes);
    let parked = &parked;
    let mut ready = Vec::new();
    loop {
        ready.clear();
        parked.admit_waiting(ctx, &mut ready);
        if ready.is_empty() {
            return;
        }
        let batch = &ready;
        pool.install(|| {
            rayon::scope(|s| {
                for &node in batch.iter().rev() {
                    s.spawn(move |s| process(s, node, ctx, parked));
                }
            })
        });
    }
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

    #[test]
    fn carriers_follow_their_capacity() {
        let a = carriers(5).unwrap();
        let b = std::thread::spawn(|| carriers(5).unwrap()).join().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.current_num_threads(), 5);
        let c = carriers(6).unwrap();
        assert_eq!(c.current_num_threads(), 6);
        assert!(Arc::ptr_eq(&c, &carriers(6).unwrap()));
        assert!(!Arc::ptr_eq(&a, &carriers(5).unwrap())); // the old pool is not kept
    }

    /// A tree of `fan`^1 + … + `fan`^`depth` folders below a new temporary folder, two files each.
    fn tree(name: &str, fan: usize, depth: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("sk-walk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut level = vec![root.clone()];
        std::fs::create_dir_all(&root).unwrap();
        for _ in 0..depth {
            let mut next = Vec::new();
            for dir in &level {
                for i in 0..fan {
                    let d = dir.join(format!("d{i}"));
                    std::fs::create_dir(&d).unwrap();
                    std::fs::write(d.join("a.txt"), b"a").unwrap();
                    std::fs::write(d.join("b.bin"), b"bb").unwrap();
                    next.push(d);
                }
            }
            level = next;
        }
        root
    }

    fn native(p: &std::path::Path) -> NativePath {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            p.as_os_str().encode_wide().collect()
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            p.as_os_str().as_bytes().into()
        }
    }

    fn root_opts(root: &std::path::Path) -> RootOpts {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            RootOpts {
                dev: std::fs::metadata(root).unwrap().dev(),
                ..RootOpts::default()
            }
        }
        #[cfg(windows)]
        {
            let _ = root;
            RootOpts::default()
        }
    }

    fn ctx_for(
        root: &std::path::Path,
        threads: usize,
        width: Option<&'static Slots>,
        gate: Option<Arc<Slots>>,
    ) -> Ctx {
        Ctx {
            root_state: vec![sys::RootState::new(&super::super::common::StateOpts {
                keep_access_time: true,
                relative_open: true,
                keep_where_updated: true,
                ..Default::default()
            })],
            roots: vec![root_opts(root)],
            deadline: None,
            budget_positive: true,
            max_entries: usize::MAX,
            extra_cap: 10_000,
            extra: AtomicUsize::new(0),
            listed: AtomicUsize::new(0),
            entries: AtomicUsize::new(0),
            arenas: (0..threads).map(|_| Mutex::new(Arena::default())).collect(),
            width,
            gate,
            pace: None,
            running: AtomicUsize::new(0),
            most_running: threads,
        }
    }

    /// (path, entries) of every folder listed, in preorder.
    fn listed(nodes: &[Node], arenas: &[Arena]) -> Vec<(NativePath, usize)> {
        let refs: Vec<Ref> = (0..nodes.len()).map(|i| [0, i as i64, 0]).collect();
        let f = flatten(nodes, &refs, arenas);
        assert!(f.frontier.is_empty() && f.errors.is_empty());
        let mut out = Vec::new();
        for (i, o) in f.listing_paths.iter().enumerate() {
            let path = match o {
                Origin::Input(k) => nodes[*k].path.clone(),
                Origin::Found(n) => n.path.clone(),
            };
            out.push((path, (f.bounds[i + 1] - f.bounds[i]) as usize));
        }
        out
    }

    fn run(
        root: &std::path::Path,
        threads: usize,
        width: Option<&'static Slots>,
        gate: Option<Arc<Slots>>,
    ) -> Vec<(NativePath, usize)> {
        let p = pool(threads).unwrap();
        let ctx = ctx_for(root, p.current_num_threads(), width, gate);
        let nodes = vec![Node::new(native(root), 0, false)];
        walk(&p, &nodes, &ctx);
        let arenas: Vec<Arena> = ctx
            .arenas
            .into_iter()
            .map(|m| m.into_inner().unwrap())
            .collect();
        listed(&nodes, &arenas)
    }

    #[test]
    fn limits_hold_and_the_walk_is_the_same_whatever_they_are() {
        let root = tree("limits", 5, 3); // 155 folders
        let want = run(&root, 1, None, None);
        assert_eq!(want.len(), 156);
        static WIDTH: Slots = Slots::new(3);
        let cases = [
            (8, None, Some(2)),
            (8, Some(&WIDTH), None),
            (8, Some(&WIDTH), Some(1)),
            (3, None, Some(64)),
        ];
        for (threads, width, gate) in cases {
            let g = gate.map(|n| Arc::new(Slots::new(n)));
            let got = run(&root, threads, width, g.clone());
            let w = width.map(|w| w.limit());
            assert_eq!(got, want, "threads {threads}, width {w:?}, gate {gate:?}");
            if let (Some(g), Some(n)) = (g, gate) {
                let (busy, peak, _) = g.window();
                assert!((1..=n).contains(&peak), "gate peak {peak}");
                assert!(busy > 0.0);
                assert_eq!(g.inflight(), 0);
            }
            if let Some(w) = width {
                let (_, peak, _) = w.window();
                assert!((1..=3).contains(&peak), "width peak {peak}");
                assert_eq!(w.inflight(), 0);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_paused_gate_lists_nothing_that_can_wait() {
        let root = tree("paused", 3, 2);
        let gate = Arc::new(Slots::new(4));
        gate.set_paused(true);
        let p = pool(4).unwrap();
        let ctx = ctx_for(&root, 4, None, Some(gate.clone()));
        let nodes = vec![
            Node::new(native(&root), 0, false),
            Node::new(native(&root.join("d1")), 0, false),
        ];
        walk(&p, &nodes, &ctx);
        assert_eq!(ctx.listed.load(Ordering::Relaxed), 0);
        assert!(nodes.iter().all(|n| matches!(n.state.get(), Some(State::Deferred))));
        assert_eq!(gate.inflight(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
