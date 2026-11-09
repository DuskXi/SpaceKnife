//! The listing controller (docs/native.md 2.2 「列目录线程」): the coldness model, the two lanes
//! and what follows from them (the calls a scanner should make, the timeline of a scan). Pure and
//! clock-free: the process's pace (mod.rs) runs it under its lock with the real clock, the
//! simulator (sim.rs) with simulated time.

use super::super::{DeviceHint, Medium};
use super::lane::{Decision, Lane, LaneCfg, Phase};
use super::model::{Model, Params};

/// The lanes: folders expected to wait for the disk, and the others.
pub const IO: usize = 0;
pub const CPU: usize = 1;

/// No listing this long: a new scan (or a pause) — the lanes start over.
pub const IDLE_NS: u64 = 1_000_000_000;
/// The read counters are looked at no more often than this, and over at least this many folders.
pub const COUNTER_NS: u64 = 2_000_000;
const COUNTER_DIRS: u64 = 16;
/// The lanes' occupancy (wall time of their listings): memory in samples.
const OCCUPANCY: f64 = 1.0 / 256.0;
/// The lanes' CPU share per listing and the IO lane's listings in flight: memories in samples.
const SHARE: f64 = 1.0 / 64.0;
const IO_INFLIGHT: f64 = 1.0 / 16.0;
/// IO lane samples older than this: none in flight any more, as far as the CPU budget goes.
const IO_STALE_NS: u64 = 20_000_000;
/// The CPU lane's width counts as the knee of the CPUs and the file system's locks when it was
/// cruising with full rounds while the IO lane used hardly any CPU (an eighth of its width at
/// most), for this long after.
const KNEE_FRESH_NS: u64 = 1_000_000_000;
/// The IO lane's listings may keep this many times the CPU knee busy on the CPUs (they are not
/// all on a CPU at the same moment their share says).
const IO_CPU_MARGIN: f64 = 1.25;
pub const MAX_CALLS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Two lanes, each following its curve.
    Lanes,
    /// SPACEKNIFE_WALK_LANES=0: one lane following its curve.
    OneLane,
    /// SPACEKNIFE_SCAN_THREADS: one lane of this many, always.
    Fixed,
    /// SPACEKNIFE_WALK_ADAPT=0: one lane at the start width, always.
    Start,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub mode: Mode,
    pub n_cpu: usize,
    /// The CPU lane's start width (the CPUs this process may use, 2–16; macOS twice that).
    pub start: usize,
    /// The narrowest the CPU lane goes.
    pub least: usize,
    /// The widest the CPU lane goes.
    pub cpu_most: usize,
    /// The widest the IO lane goes (SPACEKNIFE_WALK_MAX, within the descriptors).
    pub io_most: usize,
    /// Mode::Fixed: the width.
    pub fixed: usize,
    /// Folders of the CPU lane a call lists at once (Windows 2, else 1): the calls wanted.
    pub cpu_per_call: usize,
}

/// One listing, as the controller learns from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub lane: usize,
    pub wall_ns: u64,
    /// `model::feature` of its wait.
    pub x: f32,
    /// The folder's prior (its parent's posterior).
    pub prior: f32,
    /// Its weight in the rate: 1 + entries / 64.
    pub units: f32,
    /// Listings of its lane in flight when it finished, itself included.
    pub inflight: u32,
    /// The CPU time its thread used while listing it (0: no CPU clock).
    pub cpu_ns: u64,
}

/// Entries that weigh as much as one folder in the rate (a warm folder's open and close against
/// its entries: big folders are not slow rounds).
pub const ENTRIES_PER_DIR: f32 = 64.0;

pub fn units(entries: usize) -> f32 {
    1.0 + entries as f32 / ENTRIES_PER_DIR
}

/// Where the IO lane starts on a medium, and the widest it goes.
pub fn io_start(m: Medium) -> usize {
    match m {
        Medium::Nvme => 16,
        Medium::Flash => 4,
        Medium::Ssd | Medium::Virtual | Medium::Network | Medium::Unknown | Medium::Hdd => 8,
    }
}

fn io_hi(m: Medium, most: usize) -> usize {
    match m {
        Medium::Flash => most.min(16),
        _ => most,
    }
}

/// 0.3.5's cold width: what a hard disk's IO lane is fixed at (the HDD hook: docs/native.md
/// 2.2, 机械盘的钩子).
pub fn cold_width(start: usize, most: usize) -> usize {
    (2 * start).max(32).min(most.max(1))
}

/// The lanes of a mode and a medium.
pub fn lane_cfgs(cfg: &Config, medium: Medium) -> [LaneCfg; 2] {
    // from below, as the IO lane: the knee of a contended file system can lie far below the
    // CPUs (ReFS warm: 8 of 32), and a ramp finds it in a few rounds where probes down from the
    // start width take many; at most 8 CPUs it starts at the start width (the knee is not far
    // below, and a ramp from 2 would cost a small machine more than it finds)
    let cpu = LaneCfg {
        start: if cfg.start > 8 {
            (cfg.start / 2).max(8)
        } else {
            cfg.start
        },
        lo: cfg.least.min(cfg.start),
        hi: cfg.cpu_most.max(cfg.start),
        grow: 1.5,
        keep_wider: 1.04,
        keep_narrower: 0.98,
        gain: 1.0,
        fixed: false,
    };
    let io = |start: usize| LaneCfg {
        start,
        lo: 2.min(cfg.io_most),
        hi: io_hi(medium, cfg.io_most),
        grow: 2.0,
        keep_wider: 1.05,
        // a listing waiting for the disk costs a thread, not CPU: narrower only when clearly
        // faster (the disk's or the CPUs' contention), not merely as fast
        keep_narrower: 1.05,
        gain: 1.5,
        fixed: false,
    };
    match cfg.mode {
        Mode::Fixed => [LaneCfg::fixed(cfg.fixed), LaneCfg::fixed(cfg.fixed)],
        Mode::Start => [LaneCfg::fixed(cfg.start), LaneCfg::fixed(cfg.start)],
        Mode::OneLane => {
            let mut one = io(io_start(medium).max(cfg.start));
            one.lo = cfg.least.min(one.start);
            one.hi = (cfg.io_most + cfg.cpu_most).max(one.start);
            [one, LaneCfg::fixed(1)]
        }
        Mode::Lanes if medium == Medium::Hdd => {
            [LaneCfg::fixed(cold_width(cfg.start, cfg.io_most)), cpu]
        }
        Mode::Lanes => [io(io_start(medium).min(cfg.io_most)), cpu],
    }
}

/// The lane a folder of prior `prior` is listed in.
pub fn lane_for(mode: Mode, prior: f32) -> usize {
    match mode {
        Mode::Lanes if prior >= 0.5 => IO,
        Mode::Lanes => CPU,
        Mode::OneLane => IO,
        Mode::Fixed | Mode::Start => CPU,
    }
}

/// A stretch of a scan (the timeline's point): what was listed and how.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Seconds since the timeline began, and the stretch's length.
    pub t: f64,
    pub dt: f64,
    pub listings: u64,
    /// Of them in the IO lane.
    pub io: u64,
    /// Mean posterior (with the veto), and from the latencies alone.
    pub cold: f64,
    pub raw: f64,
    /// Time-mean widths of the lanes.
    pub width: [f64; 2],
    /// Listings the read counters' veto moved from cold to warm.
    pub vetoed: u64,
}

/// One point per second of listing, two merged into one past MAX_POINTS.
#[derive(Clone, Debug, Default)]
pub struct Timeline {
    origin: Option<u64>,
    bucket_ns: u64,
    pub points: Vec<Point>,
    cur: Option<(u64, Point, f64, f64)>,
    last: Option<u64>,
}

const MAX_POINTS: usize = 256;

impl Timeline {
    fn new() -> Self {
        Timeline {
            bucket_ns: 1_000_000_000,
            ..Timeline::default()
        }
    }

    fn add(
        &mut self,
        now: u64,
        lane: usize,
        post: f32,
        raw: f32,
        vetoed: bool,
        widths: [usize; 2],
    ) {
        let origin = *self.origin.get_or_insert(now);
        let since = self.last.map_or(0, |l| now.saturating_sub(l));
        self.last = Some(now);
        let k = now.saturating_sub(origin) / self.bucket_ns;
        if self.cur.is_some_and(|c| c.0 != k) {
            self.flush();
        }
        let (_, p, post_sum, raw_sum) = self.cur.get_or_insert((
            k,
            Point {
                t: (k * self.bucket_ns) as f64 / 1e9,
                ..Point::default()
            },
            0.0,
            0.0,
        ));
        p.listings += 1;
        p.io += (lane == IO) as u64;
        p.vetoed += vetoed as u64;
        *post_sum += post as f64;
        *raw_sum += raw as f64;
        let s = since.min(self.bucket_ns) as f64 / 1e9;
        p.dt += s;
        p.width[0] += widths[0] as f64 * s;
        p.width[1] += widths[1] as f64 * s;
    }

    fn flush(&mut self) {
        if let Some((_, mut p, post, raw)) = self.cur.take() {
            let n = p.listings.max(1) as f64;
            p.cold = post / n;
            p.raw = raw / n;
            if p.dt > 0.0 {
                p.width = [p.width[0] / p.dt, p.width[1] / p.dt];
            }
            p.dt = self.bucket_ns as f64 / 1e9;
            self.points.push(p);
            if self.points.len() > MAX_POINTS {
                self.merge();
            }
        }
    }

    fn merge(&mut self) {
        self.bucket_ns *= 2;
        let merged = self
            .points
            .chunks(2)
            .map(|c| {
                let mut p = c[0];
                if let Some(q) = c.get(1) {
                    let (a, b) = (p.listings.max(1) as f64, q.listings.max(1) as f64);
                    p.cold = (p.cold * a + q.cold * b) / (a + b);
                    p.raw = (p.raw * a + q.raw * b) / (a + b);
                    p.width = [
                        (p.width[0] + q.width[0]) / 2.0,
                        (p.width[1] + q.width[1]) / 2.0,
                    ];
                    p.listings += q.listings;
                    p.io += q.io;
                    p.vetoed += q.vetoed;
                    p.dt += q.dt;
                }
                p
            })
            .collect();
        self.points = merged;
    }

    /// The points so far, the one being filled included.
    pub fn snapshot(&self) -> Vec<Point> {
        let mut t = self.clone();
        t.flush();
        t.points
    }
}

pub struct Ctl {
    pub cfg: Config,
    pub hint: DeviceHint,
    pub model: Model,
    pub lanes: [Lane; 2],
    /// Decayed wall time of each lane's listings.
    occ: [f64; 2],
    /// Decayed CPU and wall time of each lane's listings: the CPU share of a listing.
    cpu: [(f64, f64); 2],
    /// The IO lane's listings in flight (smoothed, from its samples) and when it last had one.
    io_inflight: f64,
    io_at: Option<u64>,
    /// The CPUs the IO lane's listings use (in flight × their CPU share).
    squeeze: usize,
    /// The CPU lane's knee found alone (its width, and when), for the IO lane's CPU budget.
    knee: Option<(usize, u64)>,
    last: Option<u64>,
    /// The read counters when last looked at: (when, total, folders listed then).
    counter: Option<(u64, u64, u64)>,
    pub dirs: u64,
    /// Units every lane listed (the total the lanes' steps are judged by).
    pub units: f64,
    /// Lines for SPACEKNIFE_WALK_TRACE (None: not traced).
    pub trace: Option<Vec<String>>,
    pub timeline: Timeline,
    pub restarts: u64,
}

impl Ctl {
    pub fn new(cfg: Config, hint: DeviceHint) -> Self {
        let lanes = lane_cfgs(&cfg, hint.medium);
        Ctl {
            cfg,
            hint,
            model: Model::new(hint.medium),
            lanes: [Lane::new(lanes[IO]), Lane::new(lanes[CPU])],
            occ: [0.0; 2],
            cpu: [(0.0, 0.0); 2],
            io_inflight: 0.0,
            io_at: None,
            squeeze: 0,
            knee: None,
            last: None,
            counter: None,
            dirs: 0,
            units: 0.0,
            trace: None,
            timeline: Timeline::new(),
            restarts: 0,
        }
    }

    pub fn params(&self) -> Params {
        self.model.params()
    }

    /// The lanes' limits now. The IO lane's listings in flight use the CPUs too, by their CPU
    /// share (a cold NTFS listing on NVMe spends about two thirds of its time on the CPU, one on
    /// a hard disk almost none): the CPU lane gets at most the CPUs that leaves (its widest less
    /// that use), so that the two lanes together do not crowd the CPUs.
    pub fn widths(&self) -> [usize; 2] {
        let cpu = &self.lanes[CPU];
        let room = cpu.cfg.hi.saturating_sub(self.squeeze).max(cpu.cfg.lo);
        [self.lanes[IO].width(), cpu.width().min(room)]
    }

    /// The CPU share of a listing of `lane` lately (None: no CPU clock, or none listed).
    pub fn cpu_share(&self, lane: usize) -> Option<f64> {
        let (cpu, wall) = self.cpu[lane];
        (cpu > 0.0 && wall > 0.0).then(|| (cpu / wall).min(1.0))
    }

    /// The CPU lane's knee: the width it cruised at alone lately, else its widest (the CPUs).
    fn cpu_knee(&self, now: u64) -> usize {
        match self.knee {
            Some((w, t)) if now.saturating_sub(t) <= KNEE_FRESH_NS => w,
            _ => self.lanes[CPU].cfg.hi,
        }
    }

    /// After a CPU lane round: its width is the knee if it cruises with full rounds and the IO
    /// lane hardly uses the CPUs.
    fn note_knee(&mut self, now: u64) {
        let cpu = &self.lanes[CPU];
        if cpu.phase() == Phase::Cruise
            && cpu.last_full() == Some(now)
            && self.squeeze * 8 <= cpu.width()
        {
            self.knee = Some((cpu.width(), now));
        }
    }

    /// The widest the IO lane may go: its listings in flight times their CPU share keep at most
    /// IO_CPU_MARGIN × the CPU knee busy (a cold NTFS listing on NVMe spends two thirds of its
    /// time on the CPU, so the CPUs and the file system's locks bound it too; a hard disk's
    /// listings almost none). None: no bound (no CPU clock, one lane, a fixed lane).
    fn io_cap(&self, now: u64) -> Option<usize> {
        if self.cfg.mode != Mode::Lanes || self.lanes[IO].cfg.fixed {
            return None;
        }
        let share = self.cpu_share(IO)?.max(0.01);
        Some((IO_CPU_MARGIN * self.cpu_knee(now) as f64 / share).ceil() as usize)
    }

    fn update_squeeze(&mut self, now: u64) {
        let fresh = self
            .io_at
            .is_some_and(|t| now.saturating_sub(t) <= IO_STALE_NS);
        self.squeeze = match (self.cfg.mode, self.cpu_share(IO)) {
            (Mode::Lanes, Some(share)) if fresh => (share * self.io_inflight).round() as usize,
            _ => 0,
        };
    }

    #[cfg(test)]
    pub fn lane_for(&self, prior: f32) -> usize {
        lane_for(self.cfg.mode, prior)
    }

    /// Whether listings went on lately (`now` within IDLE_NS of the last one).
    pub fn active(&self, now: u64) -> bool {
        self.last.is_some_and(|t| now.saturating_sub(t) <= IDLE_NS)
    }

    /// A new scan of `hint`'s medium (or the first): the lanes start over; another medium also
    /// starts the clusters over (they belong to the device and the CPU, not to one scan).
    pub fn restart(&mut self, hint: DeviceHint) {
        if hint.medium != self.hint.medium {
            self.hint = hint;
            self.model = Model::new(hint.medium);
            let lanes = lane_cfgs(&self.cfg, hint.medium);
            self.lanes = [Lane::new(lanes[IO]), Lane::new(lanes[CPU])];
        } else {
            self.model.restart();
            for l in &mut self.lanes {
                l.restart();
            }
        }
        self.occ = [0.0; 2];
        self.io_inflight = 0.0;
        self.io_at = None;
        self.squeeze = 0;
        self.knee = None;
        self.counter = None;
        self.restarts += 1;
        self.log(format!("restart medium={:?}", self.hint.medium));
    }

    /// The share of the listing time spent in the IO lane lately (0.5 before any).
    pub fn io_share(&self) -> f64 {
        let total = self.occ[IO] + self.occ[CPU];
        if total > 0.0 {
            self.occ[IO] / total
        } else {
            0.5
        }
    }

    /// How many calls a scanner should keep in flight: the CPU lane's width over the folders a
    /// call lists there at once, and half the IO lane's by its share of the listing time.
    pub fn calls(&self) -> usize {
        match self.cfg.mode {
            Mode::Fixed => self.cfg.fixed,
            Mode::Start => self.cfg.start,
            Mode::OneLane => self.lanes[IO].width().div_ceil(2).max(2),
            Mode::Lanes => {
                let [io, cpu] = self.widths();
                let cpu_calls = cpu.div_ceil(self.cfg.cpu_per_call.max(1));
                let io_calls = (io as f64 * self.io_share() / 2.0).ceil() as usize;
                (cpu_calls + io_calls).clamp(2, MAX_CALLS)
            }
        }
    }

    fn log(&mut self, line: String) {
        if let Some(t) = &mut self.trace {
            t.push(line);
        }
    }

    /// One listing finished at `now`. True: a lane's width changed.
    pub fn sample(&mut self, s: &Sample, now: u64) -> bool {
        if self.last.is_some_and(|t| now.saturating_sub(t) > IDLE_NS) {
            self.restart(self.hint);
        }
        self.last = Some(now);
        let before = self.model.params();
        let (_, raw) = before.posterior(s.prior, s.x);
        let post = self.model.learn(s.x, s.prior);
        let vetoed = raw >= 0.5 && post < 0.5;
        let lane = s.lane.min(CPU);
        for (k, o) in self.occ.iter_mut().enumerate() {
            *o -= OCCUPANCY * *o;
            if k == lane {
                *o += OCCUPANCY * s.wall_ns as f64;
            }
        }
        if s.cpu_ns > 0 {
            let (c, w) = &mut self.cpu[lane];
            *c += SHARE * (s.cpu_ns.min(s.wall_ns) as f64 - *c);
            *w += SHARE * (s.wall_ns as f64 - *w);
        }
        if lane == IO {
            self.io_inflight += IO_INFLIGHT * (s.inflight as f64 - self.io_inflight);
            self.io_at = Some(now);
        }
        self.update_squeeze(now);
        if let Some(cap) = self.io_cap(now)
            && self.lanes[IO].set_cap(cap, now)
            && self.trace.is_some()
        {
            self.log(format!(
                "{:.3} io cap {} (cpu knee {}, cpu share {:.2})",
                now as f64 / 1e9,
                self.lanes[IO].width(),
                self.cpu_knee(now),
                self.cpu_share(IO).unwrap_or(0.0)
            ));
        }
        self.dirs += 1;
        self.units += s.units as f64;
        let widths = self.widths();
        self.timeline.add(now, lane, post, raw, vetoed, widths);
        // one lane experiments at a time (the lanes share the CPUs and the disk); the IO lane
        // first when both would
        let other = 1 - lane;
        let may_step =
            !self.lanes[other].holds(now) || (lane < other && self.lanes[lane].holds(now));
        let Some(d) = self.lanes[lane].sample(
            now,
            s.wall_ns,
            s.units as f64,
            s.inflight,
            widths[lane],
            self.units,
            may_step,
        ) else {
            return false;
        };
        if lane == CPU {
            self.note_knee(now);
        }
        if self.trace.is_some() {
            let line = self.describe(lane, &d);
            self.log(line);
        }
        d.to != d.from
    }

    fn describe(&self, lane: usize, d: &Decision) -> String {
        let p = self.model.params();
        format!(
            "{:.3} {} {} {:?} width {} -> {} rate={:.0}/s total={:.0}/s inflight={:.1} per_unit={:.1}us n={} dt={:.1}ms{} bdp={} cold={:.2} raw={:.2} trend={:+.2} mu=[{:.2},{:.2}] sd=[{:.2},{:.2}] kappa={:.2} reads/dir={}",
            d.round.end as f64 / 1e9,
            if lane == IO { "io" } else { "cpu" },
            d.what,
            d.phase,
            d.from,
            d.to,
            d.round.rate,
            d.round.total,
            d.round.inflight,
            d.round.per_unit * 1e6,
            d.round.n,
            d.round.dt * 1e3,
            if d.round.limited { " limited" } else { "" },
            d.bdp.map_or("-".to_owned(), |b| format!("{b:.1}")),
            self.model.cold(),
            self.model.raw_cold(),
            self.model.trend(),
            p.mu[0],
            p.mu[1],
            p.sd[0],
            p.sd[1],
            p.kappa,
            self.model
                .reads_per_dir()
                .map_or("-".to_owned(), |r| format!("{r:.2}")),
        )
    }

    /// The read counters now (None: none to go by): fed to the model every COUNTER_NS and
    /// COUNTER_DIRS folders.
    pub fn counter(&mut self, total: Option<u64>, now: u64) {
        let Some(total) = total else {
            return;
        };
        match self.counter {
            Some((t, reads, dirs))
                if now.saturating_sub(t) >= COUNTER_NS && self.dirs - dirs >= COUNTER_DIRS =>
            {
                self.model
                    .reads(total.saturating_sub(reads), self.dirs - dirs);
                self.counter = Some((now, total, self.dirs));
            }
            Some(_) => {}
            None => self.counter = Some((now, total, self.dirs)),
        }
    }

    /// Whether the counters are due to be read (the caller reads them only then: a syscall).
    pub fn counter_due(&self, now: u64) -> bool {
        self.counter.is_none_or(|(t, _, dirs)| {
            now.saturating_sub(t) >= COUNTER_NS && self.dirs - dirs >= COUNTER_DIRS
        })
    }

    /// The timeline starts over (a scan's report begins).
    pub fn reset_timeline(&mut self) {
        self.timeline = Timeline::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: Mode) -> Config {
        Config {
            mode,
            n_cpu: 32,
            start: 16,
            least: 4,
            cpu_most: 32,
            io_most: 64,
            fixed: 5,
            cpu_per_call: 1,
        }
    }

    #[test]
    fn the_medium_sets_the_lanes() {
        let c = cfg(Mode::Lanes);
        let nvme = lane_cfgs(&c, Medium::Nvme);
        assert_eq!(
            (nvme[IO].start, nvme[IO].hi, nvme[IO].fixed),
            (16, 64, false)
        );
        assert_eq!((nvme[CPU].start, nvme[CPU].lo, nvme[CPU].hi), (8, 4, 32));
        // a hard disk's IO lane: 0.3.5's cold width, fixed
        let hdd = lane_cfgs(&c, Medium::Hdd);
        assert!(hdd[IO].fixed && hdd[IO].start == 32);
        assert_eq!(lane_cfgs(&c, Medium::Flash)[IO].hi, 16);
        // the fixed modes: one lane
        let f = lane_cfgs(&cfg(Mode::Fixed), Medium::Nvme);
        assert!(f[CPU].fixed && f[CPU].start == 5);
        assert_eq!(lane_for(Mode::Fixed, 0.9), CPU);
        assert_eq!(lane_for(Mode::OneLane, 0.1), IO);
        assert_eq!(lane_for(Mode::Lanes, 0.5), IO);
        assert_eq!(lane_for(Mode::Lanes, 0.49), CPU);
    }

    #[test]
    fn calls_follow_the_lanes() {
        let mut ctl = Ctl::new(cfg(Mode::Lanes), DeviceHint::default());
        assert_eq!(ctl.calls(), 8 + 2); // 8 / 1 + ⌈8 × 0.5 / 2⌉
        // all the listing time in the IO lane
        for i in 0..2000u64 {
            let s = Sample {
                lane: IO,
                wall_ns: 300_000,
                x: 5.0,
                prior: 0.9,
                units: 1.0,
                inflight: 1,
                cpu_ns: 0,
            };
            ctl.sample(&s, i * 10_000);
        }
        assert!(ctl.io_share() > 0.99);
        assert_eq!(ctl.calls(), 8 + 4);
        let mut win = cfg(Mode::Lanes);
        win.cpu_per_call = 2;
        assert_eq!(Ctl::new(win, DeviceHint::default()).calls(), 4 + 2);
    }

    #[test]
    fn idle_starts_over_and_a_new_medium_starts_the_clusters_over() {
        let mut ctl = Ctl::new(cfg(Mode::Lanes), DeviceHint::default());
        let s = Sample {
            lane: IO,
            wall_ns: 100_000,
            x: 9.0,
            prior: 0.5,
            units: 1.0,
            inflight: 1,
            cpu_ns: 0,
        };
        for i in 0..1000u64 {
            ctl.sample(&s, i * 1000);
        }
        let mu = ctl.params().mu;
        ctl.sample(&s, 3_000_000_000);
        assert_eq!(ctl.restarts, 1);
        assert!((ctl.params().mu[1] - mu[1]).abs() < 0.1); // the clusters stay
        ctl.restart(DeviceHint {
            medium: Medium::Hdd,
        });
        assert!(ctl.lanes[IO].cfg.fixed);
        assert!(ctl.params().mu[1] > 8.0); // the hard disk's prior: 8 ms
    }

    #[test]
    fn the_io_lanes_cpu_use_counts_against_the_cpu_lane() {
        let mut ctl = Ctl::new(cfg(Mode::Lanes), DeviceHint::default());
        let cpu_w = ctl.lanes[CPU].width();
        // cold listings that spend two thirds of their time on the CPU, 36 in flight: 24 of the
        // 32 CPUs
        let io = Sample {
            lane: IO,
            wall_ns: 300_000,
            x: 4.4,
            prior: 0.9,
            units: 1.0,
            inflight: 36,
            cpu_ns: 200_000,
        };
        for i in 0..500u64 {
            ctl.sample(&io, i * 10_000);
        }
        assert_eq!(ctl.widths()[CPU], cpu_w.min(32 - 24));
        // the IO lane goes quiet: the budget comes back
        let warm = Sample {
            lane: CPU,
            cpu_ns: 50_000,
            wall_ns: 50_000,
            x: 0.5,
            prior: 0.1,
            inflight: 1,
            ..io
        };
        ctl.sample(&warm, 500 * 10_000 + IO_STALE_NS + 1);
        assert_eq!(ctl.widths()[CPU], cpu_w);
        // a hard disk's listings hardly use the CPU: nothing to count
        let mut hdd = Ctl::new(cfg(Mode::Lanes), DeviceHint::default());
        let slow = Sample {
            wall_ns: 8_000_000,
            cpu_ns: 60_000,
            inflight: 32,
            ..io
        };
        for i in 0..500u64 {
            hdd.sample(&slow, i * 250_000);
        }
        assert_eq!(hdd.widths()[CPU], cpu_w);
    }

    #[test]
    fn the_timeline_keeps_a_bounded_curve() {
        let mut ctl = Ctl::new(cfg(Mode::Lanes), DeviceHint::default());
        let s = Sample {
            lane: CPU,
            wall_ns: 50_000,
            x: 0.5,
            prior: 0.1,
            units: 1.0,
            inflight: 1,
            cpu_ns: 0,
        };
        // 600 s of listings, one every 100 ms
        for i in 0..6000u64 {
            ctl.sample(&s, i * 100_000_000 + 1);
        }
        let pts = ctl.timeline.snapshot();
        assert!(pts.len() <= MAX_POINTS && pts.len() > 100, "{}", pts.len());
        assert!(pts.iter().all(|p| p.cold < 0.2 && p.io == 0));
        let total: u64 = pts.iter().map(|p| p.listings).sum();
        assert_eq!(total, 6000);
    }
}
