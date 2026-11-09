//! A deterministic simulator of the walker's listings (docs/native.md 2.2 「列目录线程」, 模拟器):
//! simulated devices, CPUs and folder trees drive the real controller (ctl.rs: the model and the
//! two lanes, the code the walker runs), 0.3.5's rules (v1.rs) and fixed widths, in simulated time
//! from fixed seeds.
//!
//! - Tree: folders generated depth-first (ids in preorder), a few big top-level subtrees, entries
//!   per folder log-normal (median 3), the subtrees' depth and size varied.
//! - Cache: per folder, warm or cold: all cold, all warm, half (the first half in preorder warm:
//!   a scan that stopped half way), uneven (random subtrees warm, 30 % of the folders), or
//!   read-ahead (all cold, but listing a cold folder brings half of its subfolders' records in).
//! - Rough volumes (`Rough`, W: (ReFS on NVMe) cold): in a share of the subtrees a cold listing
//!   reads many times more (metadata scattered over the volume), and now and then the volume
//!   holds every read for tens of ms (a lock: reads in flight or issued meanwhile finish that much
//!   later). One round of listings then lists ten times slower than the next, a scan's first
//!   rounds included.
//! - Device: reads served by `channels` at once, the rest queued; a read takes `base` (log-normal
//!   jitter); a hard disk's reads get faster with the queue (NCQ reorders up to 32: base / (1 +
//!   0.35 ln q)). A cold listing reads 1 + entries / `entries_per_read` times, one after the other,
//!   then uses CPU.
//! - CPU: processor sharing on `n_cpu` CPUs; with k listings on the CPU each progresses at
//!   min(1, n_cpu / k) / (1 + c(k)), c the lock contention (none; mild, NTFS-like:
//!   0.04 (k − 12)⁺; strong, ReFS-like: 0.285 (k − 8)⁺ up to 32, slower past it: 16 listings at
//!   once list at 0.61 of 8's rate, 64 at 0.41); a share `block` of the contention is
//!   spent blocked (a wait without CPU), the rest spinning. Warm listings cost `warm_us` +
//!   `entry_us` per entry; cold ones `cold_us` + `cold_entry_us` per entry more.
//! - Walker: two depth-first stacks (one per lane; one lane for v1 and the fixed widths), as many
//!   listings in flight per lane as its width; callers never limit (the scanner's calls follow the
//!   walker, scanner.rs).
//!
//! Measured per listing as the walker measures it: wall time, the CPU its thread used (the wait is
//! the difference, plus up to a few µs of noise), entries, the lane's in flight; the read count
//! where the device has one (not on a network share).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use super::super::{DeviceHint, Medium};
use super::ctl::{self, CPU, Config, Ctl, IO, Mode, Sample};
use super::model::feature;
use super::v1::{self, V1};

// ------------------------------------------------------------------------------------ random

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.f()).ln()
    }

    fn normal(&mut self) -> f64 {
        let (a, b) = (self.f().max(1e-12), self.f());
        (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
    }

    fn lognormal(&mut self, median: f64, sigma: f64) -> f64 {
        median * (sigma * self.normal()).exp()
    }

    /// Geometric with this mean (0, 1, 2, …).
    fn geometric(&mut self, mean: f64) -> u32 {
        if mean <= 0.0 {
            return 0;
        }
        let q = mean / (1.0 + mean);
        ((1.0 - self.f()).ln() / q.ln()).floor().min(10_000.0) as u32
    }
}

// -------------------------------------------------------------------------------------- tree

struct Folder {
    children: Vec<u32>,
    entries: u32,
    depth: u16,
    /// Folders in its subtree (itself included): its preorder range is [id, id + size).
    size: u32,
}

struct Tree {
    f: Vec<Folder>,
}

/// Mean subfolders of a folder at `depth`.
fn branching(depth: u16) -> f64 {
    match depth {
        0 => 0.0, // the root's are drawn apart
        1 => 6.0,
        2 => 3.5,
        3 => 2.0,
        4..=7 => 1.0,
        8..=11 => 0.8,
        _ => 0.4,
    }
}

fn tree(rng: &mut Rng, target: usize) -> Tree {
    let mut f = vec![Folder {
        children: Vec::new(),
        entries: 0,
        depth: 0,
        size: 1,
    }];
    // (folder, subfolders still to make)
    let mut stack: Vec<(u32, u32)> = Vec::new();
    while f.len() < target {
        if stack.is_empty() {
            stack.push((0, 1)); // one more top-level subtree
        }
        let top = stack.len() - 1;
        let (parent, left) = stack[top];
        if left == 0 || f.len() >= target {
            stack.pop();
            continue;
        }
        stack[top].1 -= 1;
        let depth = f[parent as usize].depth + 1;
        let id = f.len() as u32;
        f.push(Folder {
            children: Vec::new(),
            entries: 0,
            depth,
            size: 1,
        });
        f[parent as usize].children.push(id);
        let k = rng.geometric(branching(depth));
        stack.push((id, k));
    }
    // entries: files plus subfolders; subtree sizes from the leaves up (ids are preorder)
    for i in (0..f.len()).rev() {
        let files = rng.lognormal(3.0, 1.3).floor().min(5000.0) as u32;
        f[i].entries = files + f[i].children.len() as u32;
        let size: u32 = 1 + f[i]
            .children
            .iter()
            .map(|&c| f[c as usize].size)
            .sum::<u32>();
        f[i].size = size;
    }
    Tree { f }
}

/// Subtrees whose cold listings read `reads` times as often, this share of the folders; stalls
/// of every read, `stall_us` long (log-normal) every `gap_us` (exponential).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rough {
    share: f64,
    reads: u32,
    gap_us: f64,
    stall_us: f64,
}

/// Per folder, how many times its cold listing reads (1 outside rough subtrees).
fn roughness(tree: &Tree, rough: Option<Rough>, rng: &mut Rng) -> Vec<u32> {
    let n = tree.f.len();
    let mut times = vec![1u32; n];
    let Some(r) = rough else {
        return times;
    };
    let want = (r.share * n as f64) as usize;
    let (mut count, mut tries) = (0usize, 0);
    while count < want && tries < 100_000 {
        tries += 1;
        let i = (rng.next() % n as u64) as usize;
        let size = tree.f[i].size as usize;
        if !(2..=5).contains(&tree.f[i].depth) || times[i] > 1 || size > n / 8 {
            continue;
        }
        for t in &mut times[i..i + size] {
            if *t == 1 {
                *t = r.reads;
                count += 1;
            }
        }
    }
    times
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cache {
    Cold,
    Warm,
    /// The first half in preorder warm.
    Half,
    /// Random subtrees warm, this share of the folders.
    Uneven(f64),
    /// All cold; listing a cold folder makes each subfolder warm with this probability.
    ReadAhead(f64),
}

fn warmth(tree: &Tree, cache: Cache, rng: &mut Rng) -> Vec<bool> {
    let n = tree.f.len();
    match cache {
        Cache::Cold | Cache::ReadAhead(_) => vec![false; n],
        Cache::Warm => vec![true; n],
        Cache::Half => (0..n).map(|i| i < n / 2).collect(),
        Cache::Uneven(share) => {
            let mut warm = vec![false; n];
            let mut count = 0usize;
            let want = (share * n as f64) as usize;
            let mut tries = 0;
            while count < want && tries < 100_000 {
                tries += 1;
                let i = (rng.next() % n as u64) as usize;
                let d = tree.f[i].depth;
                let size = tree.f[i].size as usize;
                if !(2..=5).contains(&d) || warm[i] || size > n / 8 {
                    continue;
                }
                for w in &mut warm[i..i + size] {
                    if !*w {
                        *w = true;
                        count += 1;
                    }
                }
            }
            warm
        }
    }
}

// ------------------------------------------------------------------------------------ device

#[derive(Clone, Copy, Debug, PartialEq)]
enum DiskKind {
    Ssd,
    /// NCQ: reads faster with the queue up to 32 deep.
    Hdd,
    Net,
}

#[derive(Clone, Copy, Debug)]
struct Device {
    medium: Medium,
    kind: DiskKind,
    channels: usize,
    base_us: f64,
    jitter: f64,
    entries_per_read: f64,
    cold_us: f64,
    cold_entry_us: f64,
    /// A read count to go by (Windows volume counters, POSIX ru_inblock; not on a share).
    counter: bool,
}

const NVME: Device = Device {
    medium: Medium::Nvme,
    kind: DiskKind::Ssd,
    channels: 32,
    base_us: 80.0,
    jitter: 0.3,
    entries_per_read: 100.0,
    cold_us: 110.0,
    cold_entry_us: 1.5,
    counter: true,
};

/// W:: ReFS on NVMe: its metadata reads take longer (several dependent pages each).
const REFS_NVME: Device = Device {
    base_us: 400.0,
    jitter: 0.5,
    ..NVME
};

const SATA: Device = Device {
    medium: Medium::Ssd,
    kind: DiskKind::Ssd,
    channels: 8,
    base_us: 160.0,
    jitter: 0.3,
    entries_per_read: 100.0,
    cold_us: 100.0,
    cold_entry_us: 1.5,
    counter: true,
};

const HDD: Device = Device {
    medium: Medium::Hdd,
    kind: DiskKind::Hdd,
    channels: 1,
    base_us: 8000.0,
    jitter: 0.4,
    entries_per_read: 200.0,
    cold_us: 60.0,
    cold_entry_us: 1.0,
    counter: true,
};

const NET: Device = Device {
    medium: Medium::Network,
    kind: DiskKind::Net,
    channels: 16,
    base_us: 1500.0,
    jitter: 0.3,
    entries_per_read: 400.0,
    cold_us: 40.0,
    cold_entry_us: 0.5,
    counter: false,
};

// --------------------------------------------------------------------------------------- cpu

#[derive(Clone, Copy, Debug, PartialEq)]
enum Contention {
    None,
    /// NTFS-like.
    Mild,
    /// ReFS-like.
    Strong,
}

#[derive(Clone, Copy, Debug)]
struct Cpu {
    n: f64,
    contention: Contention,
    block: f64,
    warm_us: f64,
    entry_us: f64,
}

impl Cpu {
    fn c(&self, k: usize) -> f64 {
        let k = k as f64;
        match self.contention {
            Contention::None => 0.0,
            Contention::Mild => 0.04 * (k - 12.0).max(0.0),
            // falls monotonically past 8: 16 lists at 0.61 of 8's rate, 64 at 0.41
            Contention::Strong if k <= 32.0 => 0.285 * (k - 8.0).max(0.0),
            Contention::Strong => 0.285 * 24.0 + 0.06 * (k - 32.0),
        }
    }
}

// An entry costs 1/32 of a folder's open and close: twice what the walker's units assume
// (entries / 64), so that the rate it measures is not exactly the work.
const NTFS_CPU: Cpu = Cpu {
    n: 32.0,
    contention: Contention::Mild,
    block: 0.3,
    warm_us: 60.0,
    entry_us: 1.9,
};

const FREE_CPU: Cpu = Cpu {
    n: 32.0,
    contention: Contention::None,
    block: 0.0,
    warm_us: 30.0,
    entry_us: 0.95,
};

const REFS_CPU: Cpu = Cpu {
    n: 32.0,
    contention: Contention::Strong,
    block: 0.5,
    warm_us: 40.0,
    entry_us: 1.25,
};

/// A process limited to 4 CPUs (affinity, a container), NTFS-like otherwise.
const FOUR_CPUS: Cpu = Cpu { n: 4.0, ..NTFS_CPU };

const NET_CPU: Cpu = Cpu {
    n: 32.0,
    contention: Contention::None,
    block: 0.0,
    warm_us: 20.0,
    entry_us: 0.6,
};

// -------------------------------------------------------------------------------- the policies

enum Policy {
    New(Box<Ctl>),
    V1(V1),
    Fixed(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Arm {
    New,
    V1,
    Fixed(usize),
}

/// The start width the walker would have on `n` CPUs (as on Windows and Linux, whatever the
/// platform the simulator runs on: its results do not depend on it).
fn start_width(n: usize) -> usize {
    n.clamp(2, 16)
}

fn new_config(n: usize) -> Config {
    let start = start_width(n);
    Config {
        mode: Mode::Lanes,
        n_cpu: n,
        start,
        least: (start / 4).max(2).min(start),
        cpu_most: n.max(start),
        io_most: 64,
        fixed: 0,
        cpu_per_call: 1,
    }
}

// --------------------------------------------------------------------------------- the run

struct Job {
    folder: u32,
    lane: usize,
    start: f64,
    prior: f32,
    reads_left: u32,
    /// CPU work (µs) once the reads are done.
    work: f64,
    /// The CPU clocks when its CPU part began.
    u0: f64,
}

#[derive(Debug, Clone)]
struct Outcome {
    arm: Arm,
    /// Simulated seconds, units per second.
    secs: f64,
    rate: f64,
    /// Units listed per millisecond.
    per_ms: Vec<f32>,
    /// Time-mean in-flight limit of each lane.
    width: [f64; 2],
    reads: u64,
}

impl Outcome {
    /// The first time the rate over the trailing `window` s reached `rate`.
    fn reached(&self, rate: f64, window: f64) -> Option<f64> {
        let w = ((window * 1000.0).round() as usize).max(1);
        let need = rate * w as f64 / 1000.0;
        let mut sum = 0.0f64;
        for (i, &u) in self.per_ms.iter().enumerate() {
            sum += u as f64;
            if i >= w {
                sum -= self.per_ms[i - w] as f64;
            }
            if i + 1 >= w && sum >= need {
                return Some((i + 1) as f64 / 1000.0);
            }
        }
        None
    }
}

struct Sim<'a> {
    tree: &'a Tree,
    dev: Device,
    cpu: Cpu,
    warm: Vec<bool>,
    /// Reads per cold listing, times (rough subtrees).
    rough: Vec<u32>,
    /// (every, long) of the stalls, µs; the current or next stall's [start, end).
    stalls: Option<(f64, f64)>,
    stall: (f64, f64),
    stall_rng: Rng,
    read_ahead: f64,
    rng: Rng,
    now: f64, // µs
    // cpu: processor sharing; v: work done by every job on the CPU, u: CPU used by each
    v: f64,
    u: f64,
    cpu_total: f64,                          // CPU seconds of the process, µs
    on_cpu: BinaryHeap<Reverse<(u64, u32)>>, // (v at which it is done, job)
    // disk
    busy: usize,
    queue: VecDeque<u32>,
    reading: BinaryHeap<Reverse<(u64, u32)>>, // (µs·1000 when done, job)
    reads: u64,
    jobs: Vec<Option<Job>>,
    free: Vec<u32>,
    stacks: [Vec<(u32, f32)>; 2],
    inflight: [usize; 2],
    policy: Policy,
    units: f64,
    per_ms: Vec<f32>,
    width_t: [f64; 2],
}

impl<'a> Sim<'a> {
    fn limits(&self) -> [usize; 2] {
        match &self.policy {
            Policy::New(c) => c.widths(),
            Policy::V1(v) => [0, v.width()],
            Policy::Fixed(w) => [0, *w],
        }
    }

    fn lane_for(&self, prior: f32) -> usize {
        match &self.policy {
            Policy::New(c) => c.lane_for(prior),
            _ => CPU,
        }
    }

    fn k(&self) -> usize {
        self.on_cpu.len()
    }

    /// Work per µs of each job on the CPU, and CPU used per µs by each.
    fn speeds(&self) -> (f64, f64) {
        let k = self.k();
        if k == 0 {
            return (0.0, 0.0);
        }
        let share = (self.cpu.n / k as f64).min(1.0);
        let c = self.cpu.c(k);
        let r = share / (1.0 + c);
        (r, r * (1.0 + c * (1.0 - self.cpu.block)))
    }

    fn advance(&mut self, to: f64) {
        let dt = to - self.now;
        if dt > 0.0 {
            let (r, used) = self.speeds();
            self.v += r * dt;
            self.u += used * dt;
            self.cpu_total += used * dt * self.k() as f64;
            let limits = self.limits();
            self.width_t[0] += limits[0] as f64 * dt;
            self.width_t[1] += limits[1] as f64 * dt;
        }
        self.now = to;
    }

    fn start(&mut self, folder: u32, prior: f32, lane: usize) {
        let f = &self.tree.f[folder as usize];
        let cold = !self.warm[folder as usize];
        let entries = f.entries as f64;
        let mut work = self.cpu.warm_us + self.cpu.entry_us * entries;
        let mut reads = 0;
        if cold {
            work += self.dev.cold_us + self.dev.cold_entry_us * entries;
            reads =
                (1 + (entries / self.dev.entries_per_read) as u32) * self.rough[folder as usize];
        }
        work *= self.rng.lognormal(1.0, 0.25);
        let job = Job {
            folder,
            lane,
            start: self.now,
            prior,
            reads_left: reads,
            work,
            u0: 0.0,
        };
        let id = match self.free.pop() {
            Some(id) => {
                self.jobs[id as usize] = Some(job);
                id
            }
            None => {
                self.jobs.push(Some(job));
                (self.jobs.len() - 1) as u32
            }
        };
        self.inflight[lane] += 1;
        if reads > 0 {
            self.issue(id);
        } else {
            self.on_cpu_start(id);
        }
    }

    fn issue(&mut self, id: u32) {
        if self.busy < self.dev.channels {
            self.serve(id);
        } else {
            self.queue.push_back(id);
        }
    }

    fn serve(&mut self, id: u32) {
        self.busy += 1;
        let base = match self.dev.kind {
            DiskKind::Ssd | DiskKind::Net => self.dev.base_us,
            DiskKind::Hdd => {
                let q = (self.busy + self.queue.len()).clamp(1, 32) as f64;
                self.dev.base_us / (1.0 + 0.35 * q.ln())
            }
        };
        let took = base * self.rng.lognormal(1.0, self.dev.jitter);
        let done = (self.held(self.now, took) * 1000.0) as u64;
        self.reading.push(Reverse((done, id)));
    }

    /// When a read issued at `t` that takes `took` finishes, the volume's stalls counted.
    fn held(&mut self, t: f64, took: f64) -> f64 {
        let Some((gap, long)) = self.stalls else {
            return t + took;
        };
        while self.stall.1 <= t {
            // the first one early: a cold volume's own tables load on its first reads (every
            // cold W: run traced had a round ten times slower within its first 100 ms)
            let gap = if self.stall.1 == 0.0 { gap / 10.0 } else { gap };
            let from = self.stall.1 + self.stall_rng.exp(gap);
            self.stall = (from, from + long * self.stall_rng.lognormal(1.0, 0.5));
        }
        let (from, to) = self.stall;
        if t >= from {
            to + took
        } else if t + took > from {
            t + took + (to - from)
        } else {
            t + took
        }
    }

    fn on_cpu_start(&mut self, id: u32) {
        let job = self.jobs[id as usize].as_mut().expect("a job");
        job.u0 = self.u;
        let at = self.v + job.work;
        self.on_cpu.push(Reverse((at.to_bits(), id)));
    }

    fn next_cpu(&self) -> Option<f64> {
        let Reverse((at, _)) = *self.on_cpu.peek()?;
        let (r, _) = self.speeds();
        Some(self.now + ((f64::from_bits(at) - self.v) / r).max(0.0))
    }

    fn read_done(&mut self, id: u32) {
        self.busy -= 1;
        self.reads += 1;
        if let Some(next) = self.queue.pop_front() {
            self.serve(next);
        }
        let job = self.jobs[id as usize].as_mut().expect("a job");
        job.reads_left -= 1;
        if job.reads_left > 0 {
            self.issue(id);
        } else {
            self.on_cpu_start(id);
        }
    }

    fn cpu_done(&mut self, id: u32) {
        let job = self.jobs[id as usize].take().expect("a job");
        self.free.push(id);
        let lane = job.lane;
        let f = &self.tree.f[job.folder as usize];
        let wall = self.now - job.start;
        let used = (self.u - job.u0).min(wall);
        let noise = self.rng.exp(1.5);
        let wait = (wall - used).max(0.0) + noise;
        let inflight = self.inflight[lane];
        self.inflight[lane] -= 1;
        let entries = f.entries as usize;
        let units = ctl::units(entries);
        self.units += units as f64;
        let ms = (self.now / 1000.0) as usize;
        if self.per_ms.len() <= ms {
            self.per_ms.resize(ms + 1, 0.0);
        }
        self.per_ms[ms] += units;
        let cold = !self.warm[job.folder as usize];
        if cold && self.read_ahead > 0.0 {
            for &c in &f.children {
                if self.rng.f() < self.read_ahead {
                    self.warm[c as usize] = true;
                }
            }
        }
        let now_ns = (self.now * 1000.0) as u64;
        let reads = self.dev.counter.then_some(self.reads);
        let child_prior = match &mut self.policy {
            Policy::New(c) => {
                let x = feature((wait * 1000.0) as u64);
                let params = c.params();
                let (post, _) = params.posterior(job.prior, x);
                let s = Sample {
                    lane,
                    wall_ns: ((wall + noise) * 1000.0) as u64,
                    x,
                    prior: job.prior,
                    units,
                    inflight: inflight as u32,
                    cpu_ns: (used * 1000.0) as u64,
                };
                c.sample(&s, now_ns);
                if c.counter_due(now_ns) {
                    c.counter(reads, now_ns);
                }
                params.child_prior(post)
            }
            Policy::V1(v) => {
                v.listed(wall / 1e6, entries);
                v.after(self.now / 1e6, Some(self.cpu_total / 1e6), reads);
                0.5
            }
            Policy::Fixed(_) => 0.5,
        };
        for &c in f.children.iter().rev() {
            let l = self.lane_for(child_prior);
            self.stacks[l].push((c, child_prior));
        }
    }

    fn dispatch(&mut self) {
        let limits = self.limits();
        for lane in [IO, CPU] {
            while self.inflight[lane] < limits[lane] {
                let Some((folder, prior)) = self.stacks[lane].pop() else {
                    break;
                };
                self.start(folder, prior, lane);
            }
        }
    }

    fn run(mut self, arm: Arm, watch: &mut dyn FnMut(&mut Ctl)) -> Outcome {
        let root_prior = match &self.policy {
            Policy::New(c) => c.params().unknown_prior(),
            _ => 0.5,
        };
        let lane = self.lane_for(root_prior);
        self.stacks[lane].push((0, root_prior));
        self.dispatch();
        loop {
            let disk = self
                .reading
                .peek()
                .map(|Reverse((t, _))| *t as f64 / 1000.0);
            let cpu = self.next_cpu();
            let (t, is_disk) = match (disk, cpu) {
                (None, None) => break,
                (Some(d), None) => (d, true),
                (None, Some(c)) => (c, false),
                (Some(d), Some(c)) => {
                    if d <= c {
                        (d, true)
                    } else {
                        (c, false)
                    }
                }
            };
            self.advance(t);
            if is_disk {
                let Reverse((_, id)) = self.reading.pop().expect("a read");
                self.read_done(id);
            } else {
                let Reverse((_, id)) = self.on_cpu.pop().expect("a job");
                self.cpu_done(id);
                if let Policy::New(c) = &mut self.policy {
                    watch(c);
                }
            }
            self.dispatch();
        }
        let secs = self.now / 1e6;
        Outcome {
            arm,
            secs,
            rate: self.units / secs.max(1e-9),
            per_ms: self.per_ms,
            width: [
                self.width_t[0] / self.now.max(1e-9),
                self.width_t[1] / self.now.max(1e-9),
            ],
            reads: self.reads,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Scenario {
    name: &'static str,
    dev: Device,
    cpu: Cpu,
    cache: Cache,
    folders: usize,
    seed: u64,
    rough: Option<Rough>,
    /// Checked from this many seeds (`seed`, `seed` + 1000, …).
    runs: u64,
}

fn simulate(sc: &Scenario, tree: &Tree, arm: Arm) -> Outcome {
    simulate_with(sc, tree, arm, &mut |_| {})
}

/// `simulate`, `watch` called with the new arm's controller after every listing.
fn simulate_with(sc: &Scenario, tree: &Tree, arm: Arm, watch: &mut dyn FnMut(&mut Ctl)) -> Outcome {
    let mut rng = Rng(sc.seed ^ 0x5EED);
    let warm = warmth(tree, sc.cache, &mut rng);
    let rough = roughness(tree, sc.rough, &mut rng);
    let n_cpu = sc.cpu.n as usize;
    let policy = match arm {
        Arm::New => Policy::New(Box::new(Ctl::new(
            new_config(n_cpu),
            DeviceHint {
                medium: sc.dev.medium,
            },
        ))),
        Arm::V1 => Policy::V1(V1::new(
            v1::widths(start_width(n_cpu), 64),
            n_cpu,
            0.0,
            Some(0.0),
            sc.dev.counter.then_some(0),
        )),
        Arm::Fixed(w) => Policy::Fixed(w),
    };
    let sim = Sim {
        tree,
        dev: sc.dev,
        cpu: sc.cpu,
        warm,
        rough,
        stalls: sc.rough.map(|r| (r.gap_us, r.stall_us)),
        stall: (0.0, 0.0),
        stall_rng: Rng(sc.seed.wrapping_mul(7) ^ 0x57A11),
        read_ahead: match sc.cache {
            Cache::ReadAhead(p) => p,
            _ => 0.0,
        },
        rng: Rng(sc.seed.wrapping_mul(31) ^ 0xC0FFEE),
        now: 0.0,
        v: 0.0,
        u: 0.0,
        cpu_total: 0.0,
        on_cpu: BinaryHeap::new(),
        busy: 0,
        queue: VecDeque::new(),
        reading: BinaryHeap::new(),
        reads: 0,
        jobs: Vec::new(),
        free: Vec::new(),
        stacks: [Vec::new(), Vec::new()],
        inflight: [0; 2],
        policy,
        units: 0.0,
        per_ms: Vec::new(),
        width_t: [0.0; 2],
    };
    sim.run(arm, watch)
}

const FIXED: [usize; 9] = [2, 4, 8, 16, 24, 32, 48, 64, 96];

struct Report {
    sc: Scenario,
    new: Outcome,
    v1: Outcome,
    fixed: Vec<Outcome>,
}

impl Report {
    fn best(&self) -> &Outcome {
        self.fixed
            .iter()
            .max_by(|a, b| a.rate.total_cmp(&b.rate))
            .expect("fixed widths")
    }

    /// When the new arm first listed at 80 % of the best fixed width's mean rate (over a trailing
    /// window of 2 % of that run, at least 10 ms).
    fn good_after(&self, o: &Outcome) -> Option<f64> {
        let best = self.best();
        o.reached(0.8 * best.rate, (best.secs * 0.02).max(0.01))
    }

    fn line(&self) -> String {
        let best = self.best();
        let fmt = |o: &Outcome| {
            format!(
                "{:.3}s ({:.0}/s, {:.0}%, widths {:.0}+{:.0}, good after {})",
                o.secs,
                o.rate,
                100.0 * o.rate / best.rate,
                o.width[0],
                o.width[1],
                self.good_after(o)
                    .map_or("never".to_owned(), |t| format!("{:.0}ms", t * 1e3))
            )
        };
        format!(
            "{:<16} new {} | v1 {} | best fixed {:?} {:.3}s | fixed: {}",
            self.sc.name,
            fmt(&self.new),
            fmt(&self.v1),
            best.arm,
            best.secs,
            self.fixed
                .iter()
                .map(|o| format!("{:?}={:.3}", o.arm, o.secs))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

fn report(sc: Scenario) -> Report {
    let mut rng = Rng(sc.seed);
    let tree = tree(&mut rng, sc.folders);
    let tree = &tree;
    std::thread::scope(|s| {
        let new = s.spawn(move || simulate(&sc, tree, Arm::New));
        let old = s.spawn(move || simulate(&sc, tree, Arm::V1));
        let fixed: Vec<_> = FIXED
            .iter()
            .map(|&w| s.spawn(move || simulate(&sc, tree, Arm::Fixed(w))))
            .collect();
        Report {
            sc,
            new: new.join().expect("new"),
            v1: old.join().expect("v1"),
            fixed: fixed
                .into_iter()
                .map(|h| h.join().expect("fixed"))
                .collect(),
        }
    })
}

/// The checks of every scenario (docs/native.md 2.2, 模拟器): the new arm lists at least 90 % as
/// fast as the best fixed width, is never slower than v1 (3 % for the simulator's own noise), and
/// reaches 80 % of the best fixed width's rate no later than that width itself did, give or take
/// 5 % of its run.
/// A scenario's checks: the new arm lists at 90 % of the best fixed width's rate at least, no
/// slower than v1 (3 % slack), and gets good (80 % of the best's rate) about as soon as the best
/// fixed width (5 % of its time later at most). A scenario of `runs` seeds (the rough ones: rates
/// ten times apart from round to round, where one tree's v1 or first stall can be lucky) is
/// checked on each for the rate, on their sums for the rest.
fn check(name: &str) {
    let base = *scenarios()
        .iter()
        .find(|s| s.name == name)
        .expect("a scenario");
    let (mut new, mut v1) = (0.0, 0.0);
    // when the new arm got good, and the bound: the best fixed width's time plus 5 % of its run
    let (mut good, mut bound) = (0.0, 0.0);
    for i in 0..base.runs {
        let mut sc = base;
        sc.seed += 1000 * i;
        let r = report(sc);
        let line = r.line();
        println!("{line}");
        let best = r.best();
        assert!(
            r.new.rate >= 0.9 * best.rate,
            "{} (seed {}): new lists at {:.0}% of the best fixed width
{line}",
            sc.name,
            sc.seed,
            100.0 * r.new.rate / best.rate
        );
        good += r.good_after(&r.new).unwrap_or(f64::INFINITY);
        bound += r.good_after(best).unwrap_or(best.secs) + 0.05 * best.secs;
        new += r.new.secs;
        v1 += r.v1.secs;
    }
    let seeds = base.runs;
    assert!(
        new <= 1.03 * v1,
        "{name}: new {new:.3}s, v1 {v1:.3}s (over {seeds} seeds)"
    );
    assert!(
        good <= bound,
        "{name}: good after {good:.3}s, at most {bound:.3}s (over {seeds} seeds)"
    );
    if seeds > 1 {
        println!(
            "{name}: over {seeds} seeds new {new:.3}s, v1 {v1:.3}s; good after {good:.3}s (at most {bound:.3}s)"
        );
    }
}

const ROUGH: Rough = Rough {
    share: 0.15,
    reads: 10,
    gap_us: 150_000.0,
    stall_us: 30_000.0,
};

fn scenarios() -> Vec<Scenario> {
    let s = |name, dev, cpu, cache, folders, seed| Scenario {
        name,
        dev,
        cpu,
        cache,
        folders,
        seed,
        rough: None,
        runs: 1,
    };
    // W: (ReFS on NVMe) cold: lock contention, slower metadata reads, subtrees whose listings
    // read ten times as often and stalls of the volume: rounds ten times apart
    let rough = |name, cache, seed| Scenario {
        rough: Some(ROUGH),
        runs: 12,
        ..s(name, REFS_NVME, REFS_CPU, cache, 60_000, seed)
    };
    vec![
        s("nvme_cold", NVME, NTFS_CPU, Cache::Cold, 30_000, 1),
        s("nvme_warm", NVME, NTFS_CPU, Cache::Warm, 30_000, 2),
        s("nvme_half", NVME, NTFS_CPU, Cache::Half, 30_000, 3),
        s("nvme_uneven", NVME, NTFS_CPU, Cache::Uneven(0.3), 30_000, 4),
        s(
            "nvme_read_ahead",
            NVME,
            NTFS_CPU,
            Cache::ReadAhead(0.5),
            30_000,
            5,
        ),
        s("nvme_cold_big", NVME, NTFS_CPU, Cache::Cold, 150_000, 6),
        s("sata_cold", SATA, FREE_CPU, Cache::Cold, 30_000, 7),
        s("sata_uneven", SATA, FREE_CPU, Cache::Uneven(0.3), 30_000, 8),
        s("refs_warm", NVME, REFS_CPU, Cache::Warm, 30_000, 9),
        s("refs_half", NVME, REFS_CPU, Cache::Half, 30_000, 10),
        s("hdd_cold", HDD, NTFS_CPU, Cache::Cold, 8_000, 11),
        s("hdd_half", HDD, NTFS_CPU, Cache::Half, 8_000, 12),
        s("net_cold", NET, NET_CPU, Cache::Cold, 20_000, 13),
        s("net_uneven", NET, NET_CPU, Cache::Uneven(0.3), 20_000, 14),
        s("nvme_cold_4cpu", NVME, FOUR_CPUS, Cache::Cold, 20_000, 15),
        s("nvme_warm_4cpu", NVME, FOUR_CPUS, Cache::Warm, 20_000, 16),
        s(
            "nvme_uneven_4cpu",
            NVME,
            FOUR_CPUS,
            Cache::Uneven(0.3),
            20_000,
            17,
        ),
        rough("refs_cold_rough", Cache::Cold, 18),
        rough("refs_uneven_rough", Cache::Uneven(0.4), 19),
    ]
}

macro_rules! scenario {
    ($($name:ident),*) => {
        $(
            #[test]
            fn $name() {
                check(stringify!($name));
            }
        )*
    };
}

scenario!(
    nvme_cold,
    nvme_warm,
    nvme_half,
    nvme_uneven,
    nvme_read_ahead,
    nvme_cold_big,
    sata_cold,
    sata_uneven,
    refs_warm,
    refs_half,
    hdd_cold,
    hdd_half,
    net_cold,
    net_uneven,
    nvme_cold_4cpu,
    nvme_warm_4cpu,
    nvme_uneven_4cpu,
    refs_cold_rough,
    refs_uneven_rough
);

/// `cargo test -p sk-native --lib sim::seeds -- --ignored --nocapture`: every scenario from three
/// more seeds (how much the margins move with the tree; not a check).
#[test]
#[ignore]
fn seeds() {
    for extra in [100, 200, 300] {
        for mut sc in scenarios() {
            sc.seed += extra;
            println!("seed+{extra} {}", report(sc).line());
        }
    }
}

/// The new arm's trace in a scenario.
fn traced(sc: &Scenario, tree: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    simulate_with(sc, tree, Arm::New, &mut |c: &mut Ctl| match &mut c.trace {
        None => c.trace = Some(Vec::new()),
        Some(t) => out.append(t),
    });
    out
}

#[test]
fn trees_look_like_trees() {
    let mut rng = Rng(1);
    let t = tree(&mut rng, 30_000);
    assert_eq!(t.f.len(), 30_000);
    assert_eq!(t.f[0].size, 30_000);
    let depth = t.f.iter().map(|f| f.depth).max().unwrap();
    let entries: u64 = t.f.iter().map(|f| f.entries as u64).sum();
    let mean = entries as f64 / t.f.len() as f64;
    println!(
        "depth {depth}, entries per folder {mean:.1}, top-level {}",
        t.f[0].children.len()
    );
    assert!((6..=40).contains(&depth), "{depth}");
    assert!((4.0..=20.0).contains(&mean), "{mean}");
    // the same seed, the same tree
    let again = tree(&mut Rng(1), 30_000);
    assert!(
        t.f.iter()
            .zip(&again.f)
            .all(|(a, b)| a.entries == b.entries && a.children == b.children)
    );
    let mut rng = Rng(2);
    let w = warmth(&t, Cache::Uneven(0.3), &mut rng);
    let share = w.iter().filter(|&&w| w).count() as f64 / w.len() as f64;
    assert!((0.28..=0.4).contains(&share), "{share}");
}

#[test]
fn the_simulation_is_deterministic() {
    let sc = Scenario {
        name: "det",
        dev: NVME,
        cpu: NTFS_CPU,
        cache: Cache::Uneven(0.3),
        folders: 5_000,
        seed: 9,
        rough: Some(ROUGH),
        runs: 1,
    };
    let tree = tree(&mut Rng(sc.seed), sc.folders);
    let a = simulate(&sc, &tree, Arm::New);
    let b = simulate(&sc, &tree, Arm::New);
    assert_eq!(a.secs, b.secs);
    assert_eq!(a.reads, b.reads);
}

/// `SK_SIM=<scenario> cargo test -p sk-native --lib sim::trace -- --ignored --nocapture`: the new
/// arm's decisions in that scenario.
#[test]
#[ignore]
fn trace() {
    let name = std::env::var("SK_SIM").unwrap_or_else(|_| "nvme_cold".to_owned());
    let all = scenarios();
    let mut sc = *all.iter().find(|s| s.name == name).expect("a scenario");
    sc.seed += std::env::var("SK_SIM_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let tree = tree(&mut Rng(sc.seed), sc.folders);
    let lines = traced(&sc, &tree);
    for l in lines {
        println!("{l}");
    }
}

/// `SK_SIM=<scenario> SK_SIM_SEEDS=<n> cargo test -p sk-native --lib sim::sweep -- --ignored
/// --nocapture`: that scenario from n seeds (the worst cases, not a check).
#[test]
#[ignore]
fn sweep() {
    let name = std::env::var("SK_SIM").unwrap_or_else(|_| "nvme_cold".to_owned());
    let n: u64 = std::env::var("SK_SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let base = *scenarios()
        .iter()
        .find(|s| s.name == name)
        .expect("a scenario");
    for extra in 0..n {
        let mut sc = base;
        sc.seed += 1000 * extra;
        let r = report(sc);
        let best = r.best().rate;
        println!(
            "seed+{:<6} new {:4.0}% v1 {:4.0}% new/v1 {:.2} | {}",
            1000 * extra,
            100.0 * r.new.rate / best,
            100.0 * r.v1.rate / best,
            r.new.secs / r.v1.secs,
            r.line()
        );
    }
}
