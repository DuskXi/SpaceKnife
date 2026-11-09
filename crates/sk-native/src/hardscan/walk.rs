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
//! On the shared carriers a folder is listed in one of the pace's two lanes (pace/, docs/native.md
//! 2.2 「列目录线程」): its prior — its parent's posterior that it waits for the disk — sends it to
//! the IO lane or the CPU lane, and its listing (wall time, the CPU its thread used, entries) is a
//! sample the pace learns from; its posterior becomes its subfolders' prior. A call keeps one
//! parked stack per lane, so a folder waiting for a full lane does not hold back the other's.
//!
//! Windows folders a name cannot reach (case-only siblings, case.rs) are not left to the frontier
//! like the others once the budget is spent: as a task a path would open them by name. A folder
//! opened by file id, and everything below it, is *pinned*: listed in this call, up to a cap, and
//! past it an error, never a task. Folders whose name another entry shares are listed in this call
//! up to the same cap too; past it the ones a task can still check go to the frontier.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::{Scope, ThreadPool, ThreadPoolBuilder};

use super::case::{mark, msg};
use super::common::{Arena, NativePath, RootOpts};
use super::pace::{self, CPU, IO, Pace};
use super::slots::{self, Slots};
use super::{Entries, Error, ListingError, Prior, Ref, SliceResult, Task, native_to_path, sys};
use crate::util::FxHashMap;
pub use crate::util::lower_ext;

pub struct Node {
    pub path: NativePath,
    pub root: u32,
    /// Found by this call (open without following a symlink) rather than given as a task.
    pub discovered: bool,
    pub care: Care,
    /// The probability that its listing waits for the disk, before it is listed: its parent's
    /// posterior (NaN: none; the pace's global cold fraction stands in).
    pub prior: f32,
    /// The lane it was admitted to (pace::IO / pace::CPU).
    pub lane: AtomicU8,
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
    pub fn new(path: NativePath, root: u32, discovered: bool, prior: f32) -> Self {
        Self::with_via(path, root, discovered, Care::Plain, None, prior)
    }

    pub fn with_via(
        path: NativePath,
        root: u32,
        discovered: bool,
        care: Care,
        via: Option<sys::Via>,
        prior: f32,
    ) -> Self {
        Self {
            path,
            root,
            discovered,
            care,
            prior,
            lane: AtomicU8::new(CPU as u8),
            via: Mutex::new(via),
            state: OnceLock::new(),
        }
    }

    fn lane(&self) -> usize {
        self.lane.load(Ordering::Relaxed) as usize
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
    /// The disk's gate (slots.rs `ListingGate`), when the call names one.
    pub gate: Option<Arc<Slots>>,
    /// The shared carriers' pace (pace/): the lanes' limits, told about every listing.
    pub pace: Option<&'static Pace>,
    /// Folders of this call holding their slots, and how many may at once: the threads of its
    /// pool (an admitted folder never waits in a queue holding slots another call could use);
    /// on the carriers also its share of the lanes' widths (`Ctx::share`).
    pub running: AtomicUsize,
    pub most_running: usize,
    /// Set by the caller to end the call early: nothing more is listed, what is left waits in
    /// the frontier (folders a path cannot stand for become CAP errors).
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Calls listing on the shared carriers now (`walk`).
static CALLS: AtomicUsize = AtomicUsize::new(0);

/// What becomes of a node offered a slot (`Ctx::admit`).
enum Admit {
    /// Put off or failed without a listing (its state is set).
    Resolved,
    /// It holds a slot of each limit: list it.
    Taken,
    /// Its lane is full: it waits (the other lane's folders may still start).
    LaneFull,
    /// The call's share or the gate is full (or the gate paused): nothing more starts now.
    Refused,
}

impl Ctx {
    pub fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

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
        if self.cancelled() {
            let _ = node.take_via();
            let _ = node.state.set(State::Deferred);
            return false;
        }
        if !stopped {
            return true;
        }
        let cap_left = self.extra.load(Ordering::Relaxed) < self.extra_cap;
        match past_budget(node.care, self.budget_positive, cap_left) {
            Past::List => true,
            Past::Defer => {
                let _ = node.take_via();
                let _ = node.state.set(State::Deferred);
                false
            }
            Past::Fail(why) => {
                let _ = node.take_via();
                if why == msg::CAP {
                    sys::cap_hit();
                }
                let _ = node.state.set(State::Failed(why.to_owned()));
                false
            }
        }
    }

    /// The limit of `lane`'s listings in flight (none off the carriers).
    fn lane_slots(&self, lane: usize) -> Option<&'static Slots> {
        self.pace.map(|p| p.lane(lane))
    }

    /// A slot of each limit for `node` in `lane`, unless it is settled without a listing. A
    /// folder that must be listed in this call goes through a paused gate (never past its limit).
    fn admit(&self, node: &Node, stopped: bool, lane: usize) -> Admit {
        if !self.settle(node, stopped) {
            return Admit::Resolved;
        }
        if self.running.load(Ordering::Relaxed) >= self.share() {
            return Admit::Refused;
        }
        let slots = self.lane_slots(lane);
        if let Some(w) = slots
            && !w.try_take(true)
        {
            return Admit::LaneFull;
        }
        if let Some(g) = &self.gate
            && !g.try_take(stopped)
        {
            if let Some(w) = slots {
                w.untake();
            }
            return Admit::Refused;
        }
        node.lane.store(lane as u8, Ordering::Relaxed);
        self.running.fetch_add(1, Ordering::Relaxed);
        Admit::Taken
    }

    /// How many of its folders a call may list at once. On the carriers: its share of the lanes'
    /// widths together among the calls on them, rounded up (all of it for a call alone, as at a
    /// scan's start; about two each for as many calls as the scanner keeps, scanner.rs). A call
    /// that lists several folders at once lists siblings at the same time, which costs the file
    /// system more: on ReFS, warm, about 15 % more kernel time than one folder at a time per call,
    /// for the same wall time.
    fn share(&self) -> usize {
        match self.pace {
            Some(p) => p
                .width()
                .div_ceil(CALLS.load(Ordering::Relaxed).max(1))
                .clamp(1, self.most_running),
            None => self.most_running,
        }
    }

    /// The slots of a listing of `lane` that took `took` go back.
    fn give(&self, took: Duration, lane: usize) {
        self.running.fetch_sub(1, Ordering::Relaxed);
        if let Some(g) = &self.gate {
            g.give(took);
        }
        if let Some(w) = self.lane_slots(lane) {
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

    /// The slots held in `lane` may go to the next folder of the lane: no limit was lowered below
    /// the listings in flight, and the gate is not paused (but for a folder that must be listed:
    /// `stopped`).
    fn may_keep(&self, stopped: bool, lane: usize) -> bool {
        self.lane_slots(lane)
            .is_none_or(|w| w.inflight() <= w.limit())
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

/// A call's folders waiting for a slot, one stack per lane, the next one on top (children are
/// pushed in reverse, so a carrier goes on with the first subfolder of the folder it listed:
/// depth-first within a lane).
pub struct Parked<'n> {
    stacks: Mutex<[Vec<&'n Node>; 2]>,
    any: AtomicBool,
    pace: Option<&'static Pace>,
}

impl<'n> Parked<'n> {
    fn new(nodes: &'n [Node], pace: Option<&'static Pace>) -> Self {
        let parked = Self {
            stacks: Mutex::new([Vec::new(), Vec::new()]),
            any: AtomicBool::new(!nodes.is_empty()),
            pace,
        };
        {
            let mut st = parked.stacks.lock().unwrap_or_else(|e| e.into_inner());
            for node in nodes.iter().rev() {
                st[parked.lane_of(node)].push(node);
            }
        }
        parked
    }

    /// The lane a node waits in: by its prior on the carriers, else the one lane.
    fn lane_of(&self, node: &Node) -> usize {
        self.pace.map_or(CPU, |p| p.lane_for(node.prior))
    }

    fn push(&self, children: &'n [Child]) {
        if children.is_empty() {
            return;
        }
        let mut st = self.stacks.lock().unwrap_or_else(|e| e.into_inner());
        for c in children.iter().rev() {
            st[self.lane_of(&c.node)].push(&c.node);
        }
        self.any.store(true, Ordering::Release);
    }

    /// Takes off the top of each lane's stack every node that is settled or gets its slots, until
    /// one is refused; those to list go to `out`.
    fn admit_locked(
        st: &mut [Vec<&'n Node>; 2],
        ctx: &Ctx,
        out: &mut Vec<&'n Node>,
        stopped: bool,
    ) {
        for lane in [IO, CPU] {
            while let Some(&node) = st[lane].last() {
                match ctx.admit(node, stopped, lane) {
                    Admit::Resolved => {}
                    Admit::Taken => out.push(node),
                    Admit::LaneFull => break,
                    Admit::Refused => return,
                }
                st[lane].pop();
            }
        }
    }

    fn admit(&self, ctx: &Ctx, out: &mut Vec<&'n Node>) {
        if !self.any.load(Ordering::Acquire) {
            return;
        }
        let mut st = self.stacks.lock().unwrap_or_else(|e| e.into_inner());
        Self::admit_locked(&mut st, ctx, out, ctx.stopped());
        self.any
            .store(st.iter().any(|s| !s.is_empty()), Ordering::Release);
    }

    /// After a listing of `lane` whose slots are still held: the lane's next folder takes them
    /// over (when `Ctx::may_keep`), then `admit` goes on. True: they were taken over (`out[0]`
    /// holds them).
    fn admit_after(&self, ctx: &Ctx, out: &mut Vec<&'n Node>, lane: usize) -> bool {
        if !self.any.load(Ordering::Acquire) {
            return false;
        }
        let mut st = self.stacks.lock().unwrap_or_else(|e| e.into_inner());
        let stopped = ctx.stopped();
        let mut kept = false;
        while let Some(&node) = st[lane].last() {
            if !ctx.settle(node, stopped) {
                st[lane].pop();
                continue;
            }
            if ctx.may_keep(stopped, lane) {
                node.lane.store(lane as u8, Ordering::Relaxed);
                out.push(node);
                st[lane].pop();
                kept = true;
            }
            break;
        }
        Self::admit_locked(&mut st, ctx, out, stopped);
        self.any
            .store(st.iter().any(|s| !s.is_empty()), Ordering::Release);
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
        // a panic in a scoped job reaches the caller; one in a spawned job (the tree freed after a
        // call) would abort the process without a handler (the panic hook has logged it)
        .panic_handler(|_| {})
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
        let lane = node.lane();
        if parked.admit_after(ctx, &mut ready, lane) {
            ctx.hand_over(took);
        } else {
            ctx.give(took, lane);
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
    let cpu0 = ctx.pace.and_then(|_| pace::cpu_mark());
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
    let cpu = match (cpu0, cpu0.and_then(|_| pace::cpu_mark())) {
        (Some(a), Some(b)) => Some(pace::cpu_ns(a, b, took.as_nanos() as u64)),
        _ => None,
    };
    match res {
        Ok((parent, pinned)) => {
            let count = arena.len() - mark.entries;
            let name_end = arena.names.len();
            drop(arena);
            let child_prior = match ctx.pace {
                Some(p) => learn(p, node, took, cpu, count),
                None => f32::NAN,
            };
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
                        node: Node::with_via(path, node.root, true, care, via, child_prior),
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

/// A listing of `node` that took `took` (`cpu`: of which its thread used the CPU; None: no
/// clock) and found `entries` entries: a sample for the pace, and the prior of its subfolders
/// (the node's posterior, mixed with the global cold fraction).
fn learn(p: &'static Pace, node: &Node, took: Duration, cpu: Option<u64>, entries: usize) -> f32 {
    let wall_ns = took.as_nanos() as u64;
    let wait_ns = cpu.map_or(wall_ns, |c| wall_ns.saturating_sub(c));
    let x = pace::feature(wait_ns);
    let params = p.params();
    let prior = if node.prior.is_nan() {
        params.unknown_prior()
    } else {
        node.prior
    };
    let lane = node.lane();
    p.listed(pace::Sample {
        lane,
        wall_ns,
        x,
        prior,
        units: pace::units(entries),
        inflight: p.lane(lane).inflight() as u32,
        cpu_ns: cpu.unwrap_or(0),
    });
    let (post, _) = params.posterior(prior, x);
    params.child_prior(post)
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
    let _call = OnCarriers(ctx.pace.is_some());
    if ctx.pace.is_some() {
        CALLS.fetch_add(1, Ordering::Relaxed);
    }
    let parked = Parked::new(nodes, ctx.pace);
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

/// The lowercased extension ids of a slice, in first-appearance order (worker._Acc.ext_id): a raw
/// extension is lowercased once, and raw spellings that lower alike share their id.
#[derive(Default)]
struct ExtIds<'a> {
    by_raw: FxHashMap<&'a [u8], u16>,
    by_lower: FxHashMap<Vec<u8>, u16>,
    last: Option<(&'a [u8], u16)>,
}

impl<'a> ExtIds<'a> {
    #[inline]
    fn id(&mut self, raw: &'a [u8], table: &mut Vec<Vec<u8>>) -> Result<u16, Error> {
        // files of one folder tend to share their extension: skip the hash then
        if let Some((last, id)) = self.last
            && last == raw
        {
            return Ok(id);
        }
        let id = match self.by_raw.get(raw) {
            Some(&id) => id,
            None => {
                let lower = lower_ext(raw);
                let id = match self.by_lower.get(&lower) {
                    Some(&id) => id,
                    None => {
                        let id =
                            u16::try_from(table.len()).map_err(|_| Error::TooManyExtensions)?;
                        table.push(lower.clone());
                        self.by_lower.insert(lower, id);
                        id
                    }
                };
                self.by_raw.insert(raw, id);
                id
            }
        };
        self.last = Some((raw, id));
        Ok(id)
    }
}

/// Preorder over the walked forest, straight into the result (worker.py's slice layout).
/// `tasks[i]` is the task of `nodes[i]`; their paths move into the result.
pub fn flatten(nodes: &[Node], tasks: Vec<Task>, arenas: &[Arena]) -> Result<SliceResult, Error> {
    let total: usize = arenas.iter().map(Arena::len).sum();
    let name_bytes: usize = arenas.iter().map(|a| a.names.len()).sum();
    let mut out = SliceResult {
        bounds: vec![0],
        entries: Entries::with_capacity(total, name_bytes),
        ext_table: vec![Vec::new()],
        ..SliceResult::default()
    };
    let mut exts = ExtIds::default();
    // (node, its reference, Some(task index) for an input task)
    let mut stack: Vec<(&Node, Ref, Option<usize>)> = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate().rev() {
        stack.push((node, tasks[i].target, Some(i)));
    }
    // an input task's path moves into the result (each task ends up in exactly one place)
    let mut paths: Vec<Option<PathBuf>> = tasks.into_iter().map(|t| Some(t.path)).collect();
    let mut take_path =
        |input: Option<usize>, node: &Node| match input.and_then(|i| paths[i].take()) {
            Some(p) => p,
            None => native_to_path(&node.path),
        };
    while let Some((node, r, input)) = stack.pop() {
        match node.state.get() {
            Some(State::Listed(l)) => {
                let listing = out.refs.len() as u32;
                out.refs.push(r);
                out.paths.push(take_path(input, node));
                let a = &arenas[l.arena];
                let (lo, hi) = (l.start, l.start + l.count);
                let names = &a.names[l.name_start..l.name_end];
                let e = &mut out.entries;
                e.names.extend_from_slice(names);
                e.name_lens.extend_from_slice(&a.name_lens[lo..hi]);
                e.kind.extend_from_slice(&a.kind[lo..hi]);
                e.flags.extend_from_slice(&a.flags[lo..hi]);
                e.size.extend_from_slice(&a.size[lo..hi]);
                e.alloc.extend_from_slice(&a.alloc[lo..hi]);
                e.mtime.extend_from_slice(&a.mtime[lo..hi]);
                e.ctime.extend_from_slice(&a.ctime[lo..hi]);
                e.atime.extend_from_slice(&a.atime[lo..hi]);
                e.ino.extend_from_slice(&a.ino[lo..hi]);
                e.nlink.extend_from_slice(&a.nlink[lo..hi]);
                e.dev.extend_from_slice(&a.dev[lo..hi]);
                let mut off = 0usize;
                for k in lo..hi {
                    let end = off + a.name_lens[k] as usize;
                    let el = a.ext_lens[k] as usize;
                    let id = if el == 0 {
                        0
                    } else {
                        exts.id(&names[end - el..end], &mut out.ext_table)?
                    };
                    e.ext.push(id);
                    off = end;
                }
                out.bounds.push(e.kind.len());
                for c in l.children.iter().rev() {
                    stack.push((
                        &c.node,
                        Ref::Entry {
                            listing,
                            entry: c.j,
                        },
                        None,
                    ));
                }
            }
            Some(State::Failed(why)) => out.errors.push(ListingError {
                target: r,
                path: take_path(input, node),
                message: why.clone(),
            }),
            // a pinned folder's path leads elsewhere, or nowhere: never a task
            Some(State::Deferred) | None if node.care == Care::Pinned => {
                out.errors.push(ListingError {
                    target: r,
                    path: take_path(input, node),
                    message: msg::CAP.to_owned(),
                })
            }
            Some(State::Deferred) | None => out.frontier.push(Task {
                target: r,
                path: take_path(input, node),
                root: node.root,
                prior: Prior::of(node.prior),
            }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            (
                Care::Strict(TWIN | DUP),
                [Past::List, Past::Fail(msg::CAP), Past::Fail(msg::DUP)],
            ),
            (
                Care::Strict(BADNAME),
                [Past::List, Past::Fail(msg::CAP), Past::Fail(msg::BADNAME)],
            ),
            (
                Care::Pinned,
                [Past::List, Past::Fail(msg::CAP), Past::Fail(msg::CAP)],
            ),
        ];
        for (care, want) in cells {
            assert_eq!(past_budget(care, true, true), want[0], "{care:?}");
            assert_eq!(past_budget(care, true, false), want[1], "{care:?}");
            for cap_left in [true, false] {
                assert_eq!(past_budget(care, false, cap_left), want[2], "{care:?}");
            }
        }
        assert_eq!(
            past_budget(Care::Strict(TWIN | BADNAME), false, true),
            Past::Fail(msg::BADNAME)
        );
    }

    #[test]
    fn bigger_pools_are_shared() {
        let a = pool(3).unwrap();
        let b = std::thread::spawn(|| pool(3).unwrap()).join().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
