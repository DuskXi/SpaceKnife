//! Disk-load governor (`sk_core::io`): the control logic against a simulated disk (deterministic,
//! simulated time; Python `test_io_governor.py`), permits on real threads and the registry's
//! sampling / push loops (short, real time; Python `test_io_async.py`).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sk_core::io::disks::{DiskKind, DiskRef};
use sk_core::io::governor::{GovernorCore, IoGovernor, IoStatus, Level};
use sk_core::io::registry::{IoRegistry, RegistryOptions};
use sk_core::io::sampler::{DiskSample, DiskSampler, RefSet, monotonic};
use sk_core::media::Media;

const TICK: f64 = 0.002;

/// A tiny deterministic generator (the simulation's noise).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.next()
    }
}

struct Batch {
    scan: usize,
    started: f64,
    work: f64, // remaining nominal seconds
}

#[derive(Default)]
struct Scan {
    waiting: usize,
    running: usize,
    done: usize,
    active: bool,
}

#[derive(Clone, Copy, Debug)]
struct Point {
    t: f64,
    self_true: f64,
    self_est: f64,
    yielding: bool,
    concurrency: usize,
    u: f64,
}

/// A disk with `q_eff` parallel units; each batch in flight adds `q_worker` to the queue.
///
/// Measured like the real samplers: busy = share of time with I/O outstanding, queue = mean
/// depth; the governor sees `rho = min(busy, queue / q_eff)`. Batches slow down when the queue
/// exceeds the disk's parallelism. `others(t)` is the other programs' load in rho units.
struct Sim {
    q_eff: f64,
    q_worker: f64,
    batch_s: f64,
    others: Box<dyn Fn(f64) -> f64>,
    noise: f64,
    spikes: f64,
    sample_s: f64,
    t: f64,
    scans: Vec<Scan>,
    points: Vec<Point>,
    core: GovernorCore,
    rng: Rng,
    batches: Vec<Batch>,
    keys: HashMap<u64, usize>,
    seq: u64,
    acc: [f64; 4], // busy, queue, others-alone rho, span
}

struct SimOpts {
    media: Media,
    q_eff: f64,
    q_worker: f64,
    max_workers: usize,
    others: Box<dyn Fn(f64) -> f64>,
}

impl Default for SimOpts {
    fn default() -> Self {
        Self {
            media: Media::SataSsd,
            q_eff: 8.0,
            q_worker: 0.8,
            max_workers: 16,
            others: Box::new(|_| 0.05),
        }
    }
}

impl Sim {
    fn new(o: SimOpts) -> Self {
        Self {
            q_eff: o.q_eff,
            q_worker: o.q_worker,
            batch_s: 0.03,
            others: o.others,
            noise: 0.02,
            spikes: 0.0,
            sample_s: 0.25, // the registry samples at 4 Hz while a scan runs
            t: 0.0,
            scans: Vec::new(),
            points: Vec::new(),
            core: GovernorCore::new(DiskRef::physical("sim0").with_media(o.media), o.max_workers, 0.0),
            rng: Rng(0x2545_F491_4F6C_DD1D),
            batches: Vec::new(),
            keys: HashMap::new(),
            seq: 0,
            acc: [0.0; 4],
        }
    }

    fn add_scan(&mut self, workers: usize) -> usize {
        self.scans.push(Scan {
            active: true,
            ..Default::default()
        });
        let idx = self.scans.len() - 1;
        for _ in 0..workers {
            self.want(idx);
        }
        idx
    }

    fn want(&mut self, idx: usize) {
        if !self.scans[idx].active {
            return;
        }
        if self.core.try_acquire(self.t) {
            self.start_batch(idx);
            return;
        }
        self.seq += 1;
        self.keys.insert(self.seq, idx);
        self.scans[idx].waiting += 1;
        self.core.request(self.seq, self.t);
    }

    fn start_batch(&mut self, idx: usize) {
        self.scans[idx].running += 1;
        self.batches.push(Batch {
            scan: idx,
            started: self.t,
            work: self.batch_s,
        });
    }

    fn rho(&self, busy: f64, queue: f64) -> f64 {
        busy.min(queue / self.q_eff).clamp(0.0, 1.0)
    }

    fn granted(&mut self) {
        for key in self.core.grant(self.t) {
            let idx = self.keys.remove(&key).expect("a waiter");
            self.scans[idx].waiting -= 1;
            self.start_batch(idx);
        }
    }

    fn run(&mut self, until: f64) {
        while self.t < until - 1e-9 {
            self.t = ((self.t + TICK) * 1e9).round() / 1e9;
            let others_q = (self.others)(self.t) * self.q_eff;
            let q = others_q + self.q_worker * self.batches.len() as f64;
            let slow = (q / self.q_eff).max(1.0);
            let mut finished = Vec::new();
            for (i, b) in self.batches.iter_mut().enumerate() {
                b.work -= TICK / slow;
                if b.work <= 0.0 {
                    finished.push(i);
                }
            }
            for i in finished.into_iter().rev() {
                let b = self.batches.remove(i);
                self.core.release(self.t, Some(b.started));
                let scan = &mut self.scans[b.scan];
                scan.running -= 1;
                scan.done += 1;
                self.want(b.scan);
            }
            self.granted();
            let alone = self.rho(others_q.min(1.0), others_q);
            self.acc[0] += q.min(1.0) * TICK;
            self.acc[1] += q * TICK;
            self.acc[2] += alone * TICK;
            self.acc[3] += TICK;
            if self.acc[3] >= self.sample_s - 1e-9 {
                self.sample();
            }
        }
    }

    fn sample(&mut self) {
        let [busy, queue, alone, span] = self.acc;
        let (busy, queue, alone) = (busy / span, queue / span, alone / span);
        let noise = self.rng.uniform(-self.noise, self.noise);
        let stall = if self.rng.next() < self.spikes { self.rng.uniform(1.5, 2.0) } else { 1.0 };
        let sample = DiskSample::new(self.t, span, (busy + noise * 0.3).clamp(0.0, 1.0), (queue * (1.0 + noise) * stall).max(0.0));
        let total_true = self.rho(busy, queue);
        self.core.observe(Some(&sample), self.t, Some(self.t));
        self.granted();
        self.points.push(Point {
            t: self.t,
            self_true: (total_true - alone).max(0.0),
            self_est: self.core.self_load,
            yielding: self.core.yielding,
            concurrency: self.core.concurrency(),
            u: self.core.u,
        });
        self.acc = [0.0; 4];
    }

    // ------------------------------------------------------------------ metrics

    fn window(&self, t0: f64, t1: f64) -> Vec<Point> {
        self.points.iter().filter(|p| t0 < p.t && p.t <= t1 + 1e-9).copied().collect()
    }

    fn mean_self(&self, t0: f64, t1: f64) -> f64 {
        let pts = self.window(t0, t1);
        pts.iter().map(|p| p.self_true).sum::<f64>() / pts.len() as f64
    }

    fn moving_averages(&self, t0: f64, t1: f64) -> Vec<f64> {
        let mut out = Vec::new();
        let mut t = t0 + 5.0;
        while t <= t1 + 1e-9 {
            out.push(self.mean_self(t - 5.0, t));
            t += 1.0;
        }
        out
    }

    /// Per-second means (what the W2 curve shows).
    fn seconds(&self, t0: f64, t1: f64) -> Vec<f64> {
        let mut out = Vec::new();
        let mut t = t0;
        while t + 1.0 <= t1 + 1e-9 {
            out.push(self.mean_self(t, t + 1.0));
            t += 1.0;
        }
        out
    }
}

fn start(sim: &mut Sim, level: Level, custom: Option<f64>, workers: usize) -> usize {
    let t = sim.t;
    sim.core.set_level(level, custom, t);
    sim.run(3.0); // pre-scan baseline (UI-W2-40)
    sim.add_scan(workers)
}

fn max(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::MIN, f64::max)
}

fn min(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::MAX, f64::min)
}

#[test]
fn converges_within_3s() {
    for (media, q_eff, q_worker, level, limit) in [
        (Media::SataSsd, 8.0, 0.8, Level::Balanced, 0.70),
        (Media::SataSsd, 8.0, 0.8, Level::Quiet, 0.35),
        (Media::Hdd, 1.0, 0.9, Level::Quiet, 0.35),
        (Media::Hdd, 1.0, 0.9, Level::Balanced, 0.70),
        (Media::NvmeSsd, 32.0, 0.9, Level::Quiet, 0.35),
    ] {
        let mut sim = Sim::new(SimOpts {
            media,
            q_eff,
            q_worker,
            max_workers: if media == Media::NvmeSsd { 32 } else { 16 },
            ..Default::default()
        });
        start(&mut sim, level, None, 16);
        sim.run(25.0);
        let case = format!("{media:?} {level:?}");
        let per_second = sim.seconds(6.0, 25.0); // 3 s after the start
        assert!(max(&per_second) <= limit + 0.05, "{case}: {per_second:?}"); // single points: at most 5 points over
        let mas = sim.moving_averages(6.0, 25.0);
        assert!(max(&mas) <= limit + 0.005, "{case}: {mas:?}"); // steady state: 5 s average under the limit
        assert!(min(&mas) >= 0.9 * limit, "{case}: {mas:?}"); // … and within 10 % of it
        let est: Vec<f64> = sim.window(8.0, 25.0).iter().map(|p| p.self_est).collect();
        let mean_est = est.iter().sum::<f64>() / est.len() as f64;
        assert!((mean_est - sim.mean_self(8.0, 25.0)).abs() < 0.03, "{case}"); // the estimate tracks the truth
    }
}

#[test]
fn level_change_applies_within_3s() {
    let mut sim = Sim::new(SimOpts::default());
    start(&mut sim, Level::Balanced, None, 16);
    sim.run(12.0);
    assert!(sim.mean_self(7.0, 12.0) > 0.6);
    let t = sim.t;
    sim.core.set_level(Level::Quiet, None, t);
    sim.run(25.0);
    assert!(max(&sim.seconds(15.0, 25.0)) <= 0.40);
    let m = sim.mean_self(15.0, 25.0);
    assert!((0.9 * 0.35..=0.35).contains(&m), "{m}");
    let t = sim.t;
    sim.core.set_level(Level::Custom, Some(50.0), t);
    sim.run(35.0);
    let m = sim.mean_self(30.0, 35.0);
    assert!((0.45..=0.50).contains(&m), "{m}");
}

#[test]
fn max_level_does_not_throttle() {
    let mut sim = Sim::new(SimOpts {
        max_workers: 12,
        ..Default::default()
    });
    start(&mut sim, Level::Max, None, 16);
    sim.run(10.0);
    assert_eq!(sim.core.concurrency(), 12);
    assert_eq!(sim.core.duty(), 1.0);
    assert!(sim.mean_self(5.0, 10.0) > 0.9);
}

#[test]
fn hdd_concurrency_stays_low() {
    let ssd = Sim::new(SimOpts {
        media: Media::NvmeSsd,
        q_eff: 32.0,
        ..Default::default()
    });
    let mut hdd = Sim::new(SimOpts {
        media: Media::Hdd,
        q_eff: 1.0,
        q_worker: 0.9,
        ..Default::default()
    });
    assert!(hdd.core.concurrency() < ssd.core.concurrency()); // UI-W2-37: initial concurrency by media
    start(&mut hdd, Level::Max, None, 16);
    hdd.run(8.0);
    assert!(hdd.core.concurrency() <= 2);
}

#[test]
fn yields_when_others_exceed_the_limit() {
    let mut sim = Sim::new(SimOpts {
        others: Box::new(|t| if (12.0..24.0).contains(&t) { 0.85 } else { 0.05 }),
        ..Default::default()
    });
    start(&mut sim, Level::Balanced, None, 16);
    sim.run(12.0);
    assert!(!sim.core.yielding);
    sim.run(16.0);
    assert!(sim.core.yielding); // detected within 4 s
    sim.run(24.0);
    assert!(sim.core.duty() <= 0.06 && sim.core.concurrency() == 1); // lowest progress: 5 % duty
    assert!(sim.window(16.0, 24.0).iter().all(|p| p.yielding));
    let progress = sim.scans[0].done;
    sim.run(29.0);
    assert!(!sim.core.yielding); // recovers within 5 s of the others falling back
    assert!(sim.scans[0].done > progress);
    sim.run(40.0);
    assert!(sim.mean_self(35.0, 40.0) > 0.6);
}

#[test]
fn no_yield_when_disabled() {
    let mut sim = Sim::new(SimOpts {
        others: Box::new(|t| if t >= 12.0 { 0.85 } else { 0.05 }),
        ..Default::default()
    });
    sim.core.set_yield_when_busy(false);
    start(&mut sim, Level::Balanced, None, 16);
    sim.run(30.0);
    assert!(!sim.points.iter().any(|p| p.yielding));
    let us: Vec<f64> = sim.window(20.0, 30.0).iter().map(|p| p.u).collect();
    assert!(min(&us) >= 1.0, "{us:?}"); // external load does not slow the scan
}

#[test]
fn others_rise_is_not_billed_to_us() {
    // a moderate external load (below the limit) is re-baselined by a probe, not throttled away
    let mut sim = Sim::new(SimOpts {
        others: Box::new(|t| if t >= 12.0 { 0.25 } else { 0.05 }),
        ..Default::default()
    });
    start(&mut sim, Level::Balanced, None, 16);
    sim.run(30.0);
    assert!(!sim.points.iter().any(|p| p.yielding));
    assert!((sim.core.others.unwrap_or(0.0) - 0.25).abs() < 0.05, "{:?}", sim.core.others);
    assert!(sim.mean_self(22.0, 30.0) > 0.5); // still near the limit: capped by 100 % - others
}

#[test]
fn others_drop_does_not_overshoot() {
    // when the others stop, a stale baseline would let our real load exceed the limit
    let mut sim = Sim::new(SimOpts {
        others: Box::new(|t| if t < 12.0 { 0.20 } else { 0.0 }),
        ..Default::default()
    });
    sim.core.set_yield_when_busy(false);
    start(&mut sim, Level::Quiet, None, 16);
    sim.run(35.0);
    assert!(max(&sim.moving_averages(20.0, 35.0)) <= 0.36);
    assert!(max(&sim.seconds(20.0, 35.0)) <= 0.40);
}

#[test]
fn pause_drops_self_to_zero_and_resume_recovers() {
    let mut sim = Sim::new(SimOpts::default());
    start(&mut sim, Level::Balanced, None, 16);
    sim.run(10.0);
    let done = sim.scans[0].done;
    sim.core.pause("scan-1");
    sim.run(11.0); // within 1 s
    assert_eq!(sim.core.inflight, 0);
    let last = *sim.points.last().unwrap();
    assert!(last.self_est == 0.0 && last.self_true < 0.01, "{last:?}");
    sim.run(14.0);
    assert!(sim.scans[0].done - done < 16); // only batches already in flight finished
    assert!(sim.core.status(true).paused);
    assert!((sim.core.others.unwrap_or(0.0) - 0.05).abs() < 0.03); // baseline re-measured while paused
    sim.core.resume("scan-1");
    sim.run(20.0);
    assert!(sim.mean_self(16.0, 20.0) > 0.6);
}

#[test]
fn two_scans_share_one_budget() {
    let mut sim = Sim::new(SimOpts::default());
    start(&mut sim, Level::Balanced, None, 8);
    sim.add_scan(8); // a second scan on another partition of the same disk
    sim.run(25.0);
    assert!(max(&sim.moving_averages(6.0, 25.0)) <= 0.705);
    assert!(max(&sim.seconds(6.0, 25.0)) <= 0.75);
    let (a, b) = (sim.scans[0].done as f64, sim.scans[1].done as f64);
    assert!(a.min(b) / (a + b) > 0.4); // FIFO permits: both progress at about the same pace
}

#[test]
fn manual_concurrency_keeps_the_limit() {
    let mut sim = Sim::new(SimOpts {
        media: Media::Hdd,
        q_eff: 1.0,
        q_worker: 0.9,
        ..Default::default()
    });
    sim.core.set_manual_concurrency(Some(4), 0.0);
    start(&mut sim, Level::Quiet, None, 16);
    sim.run(20.0);
    assert!(sim.points.iter().all(|p| p.concurrency == 4));
    assert!(!sim.core.auto());
    assert!(max(&sim.moving_averages(6.0, 20.0)) <= 0.355);
    let t = sim.t;
    sim.core.set_manual_concurrency(None, t);
    assert!(sim.core.auto());
}

#[test]
fn idle_level_runs_only_when_the_disk_is_quiet() {
    let mut sim = Sim::new(SimOpts {
        others: Box::new(|t| if (10.0..20.0).contains(&t) { 0.30 } else { 0.0 }),
        ..Default::default()
    });
    start(&mut sim, Level::Idle, None, 16);
    sim.run(10.0);
    assert!(sim.mean_self(5.0, 10.0) > 0.85 && !sim.core.yielding); // minus frequent probes
    assert!(sim.core.low_io_priority());
    sim.run(20.0);
    assert!(sim.core.yielding);
    assert!(sim.mean_self(15.0, 20.0) < 0.08);
    sim.run(30.0);
    assert!(!sim.core.yielding);
    assert!(sim.mean_self(26.0, 30.0) > 0.9);
}

#[test]
fn status_model_and_history() {
    let mut sim = Sim::new(SimOpts::default());
    start(&mut sim, Level::Quiet, None, 16);
    sim.run(70.0);
    let st = sim.core.status(true);
    assert!(st.disk == "sim0" && st.level == Level::Quiet && st.limit == 35.0);
    assert_eq!(st.history.len(), 60); // one point per second, last 60 s
    assert!(st.history.iter().all(|p| p.self_pct <= p.total_pct + 1e-6)); // the line never tops the area
    assert!(st.measured && st.auto && !st.paused);
    let json = serde_json::to_value(&st).unwrap();
    for key in ["t", "self_pct", "total_pct", "yielding"] {
        assert!(json["history"][0].get(key).is_some(), "{key}");
    }
}

#[test]
fn open_loop_without_samples() {
    let mut core = GovernorCore::new(DiskRef::new("net:\\\\nas\\share", DiskKind::Network).with_media(Media::Network), 8, 0.0);
    core.set_level(Level::Quiet, None, 0.0);
    assert!(core.try_acquire(0.0));
    core.observe(None, 1.0, None);
    let st = core.status(true);
    assert!(!st.measured && st.self_pct == 0.0);
    assert!((core.u - 8.0 * 0.35).abs() < 1e-9);
}

#[test]
fn status_reports_the_disks_read_rate_and_latency() {
    let mut core = GovernorCore::new(DiskRef::physical("disk0").with_media(Media::NvmeSsd), 8, 0.0);
    let sample = DiskSample {
        read_iops: Some(2000.0),
        read_latency: Some(0.00042),
        read_bps: Some(1.5e8),
        ..DiskSample::new(1.0, 0.5, 0.2, 0.4)
    };
    core.observe(Some(&sample), 1.0, None);
    let st = core.status(true);
    assert!(st.measured && st.read_bps == Some(150_000_000.0));
    assert!((st.read_latency_ms.unwrap() - 0.42).abs() < 1e-9);
    core.observe(None, 1.5, None); // no data: nothing to show
    let st = core.status(true);
    assert!(!st.measured && st.read_bps.is_none() && st.read_latency_ms.is_none());
}

// =========================================================================== real threads

fn disk() -> DiskRef {
    DiskRef::physical("fake0").with_media(Media::SataSsd)
}

fn governor(max_workers: usize) -> Arc<IoGovernor> {
    let gov = IoGovernor::new(disk(), max_workers);
    gov.set_level(Level::Max, None);
    gov
}

#[test]
fn permit_bounds_the_parallelism() {
    let gov = governor(8);
    gov.set_manual_concurrency(Some(2));
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let (gov, running, peak, done) = (gov.clone(), running.clone(), peak.clone(), done.clone());
            std::thread::spawn(move || {
                let permit = gov.permit();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                running.fetch_sub(1, Ordering::SeqCst);
                done.fetch_add(1, Ordering::SeqCst);
                drop(permit);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!((peak.load(Ordering::SeqCst), done.load(Ordering::SeqCst)), (2, 6));
    let st = gov.status(false);
    assert!(st.concurrency == 2 && !st.auto);
    assert_eq!(gov.with_core(|c| c.inflight), 0);
}

#[test]
fn pause_holds_new_permits_until_resume() {
    let gov = governor(8);
    gov.pause("scan-a");
    let g2 = gov.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let p = g2.permit();
        tx.send(()).unwrap();
        drop(p);
    });
    assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    assert!(gov.status(false).paused);
    gov.pause("scan-b");
    gov.resume("scan-a"); // still paused by the other owner
    assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
    gov.resume("scan-b");
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    waiter.join().unwrap();
    assert!(!gov.paused());
}

#[test]
fn cancelled_waiter_does_not_leak_a_slot() {
    let gov = governor(8);
    gov.set_manual_concurrency(Some(1));
    let first = gov.permit();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (g2, c2) = (gov.clone(), cancel.clone());
    let waiter = std::thread::spawn(move || g2.permit_until(&|| c2.load(Ordering::SeqCst)).is_none());
    std::thread::sleep(Duration::from_millis(10));
    cancel.store(true, Ordering::SeqCst);
    assert!(waiter.join().unwrap());
    drop(first);
    let second = gov.permit();
    assert_eq!(gov.with_core(|c| c.inflight), 1);
    second.release();
    assert_eq!(gov.with_core(|c| (c.inflight, c.waiting())), (0, 0));
}

#[test]
fn release_from_another_thread_is_idempotent() {
    let gov = governor(8);
    let permit = gov.permit();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        permit.release();
        permit.release(); // idempotent (and drop does not release twice)
    })
    .join()
    .unwrap();
    assert_eq!(gov.with_core(|c| c.inflight), 0);
}

#[test]
fn duty_gate_paces_fractional_intensity() {
    // open loop (no samples): quiet on one worker means about 35 % of the time in flight
    let gov = IoGovernor::new(DiskRef::new("net:x", DiskKind::Network).with_media(Media::Network), 1);
    gov.observe(None, None); // the registry ticks unsampled disks with None
    gov.set_level(Level::Quiet, None);
    assert!((gov.with_core(|c| c.u) - 0.35).abs() < 1e-9);
    let mut busy = 0.0;
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        let permit = gov.permit();
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        busy += t0.elapsed().as_secs_f64();
        drop(permit);
    }
    let share = busy / start.elapsed().as_secs_f64();
    assert!(0.25 < share && share < 0.45, "{share}");
}

// --------------------------------------------------------------------------- registry

/// A disk whose load follows the governor: 10 % per batch in flight, others 5 %.
struct FakeSampler {
    refs: RefSet,
    gov: Arc<Mutex<Option<Arc<IoGovernor>>>>,
    last: f64,
    watched: Arc<Mutex<Vec<DiskRef>>>,
}

impl DiskSampler for FakeSampler {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn sample(&mut self) -> HashMap<String, DiskSample> {
        let now = monotonic();
        let dt = now - self.last;
        self.last = now;
        let n = self.gov.lock().as_ref().map_or(0, |g| g.with_core(|c| c.inflight));
        let q = 8.0 * (0.05 + 0.1 * n as f64);
        HashMap::from([(disk().id, DiskSample::new(now, dt, q.min(1.0), q))])
    }
    fn refs(&self) -> Vec<DiskRef> {
        self.refs.refs()
    }
    fn disk_ref(&self, d: &str) -> Option<DiskRef> {
        self.refs.get(d)
    }
    fn watch(&mut self, r: DiskRef) {
        self.watched.lock().push(r.clone());
        self.refs.watch(r)
    }
}

type GovSlot = Arc<Mutex<Option<Arc<IoGovernor>>>>;
type Watched = Arc<Mutex<Vec<DiskRef>>>;

fn fake_registry(opts: RegistryOptions) -> (IoRegistry, GovSlot, Watched) {
    let gov_slot = Arc::new(Mutex::new(None));
    let watched = Arc::new(Mutex::new(Vec::new()));
    let (g, w) = (gov_slot.clone(), watched.clone());
    let factory = Box::new(move || {
        let mut refs = RefSet::default();
        refs.watch(disk());
        Box::new(FakeSampler {
            refs,
            gov: g.clone(),
            last: monotonic(),
            watched: w.clone(),
        }) as Box<dyn DiskSampler>
    });
    (IoRegistry::with_sampler(opts, factory, Arc::new(monotonic)), gov_slot, watched)
}

#[test]
fn registry_samples_controls_and_pushes() {
    let opts = RegistryOptions {
        max_workers: 8,
        idle_interval: 0.05,
        active_interval: 0.05,
        push_interval: 0.05,
    };
    let (registry, slot, watched) = fake_registry(opts);
    registry.start();
    let gov = registry.governor(&disk().id);
    *slot.lock() = Some(gov.clone());
    assert!(Arc::ptr_eq(&gov, &registry.governor_for_ref(&disk()))); // one governor per disk
    let pushes: Arc<Mutex<Vec<IoStatus>>> = Default::default();
    let p2 = pushes.clone();
    let id = gov.subscribe(Arc::new(move |st: &IoStatus| p2.lock().push(st.clone())));
    gov.set_level(Level::Quiet, None);
    std::thread::sleep(Duration::from_millis(300)); // pre-scan baseline
    let others = gov.with_core(|c| c.others).unwrap();
    assert!((others - 0.05).abs() < 0.01, "{others}");
    // a scan: one dispatcher, batches of 10 ms on other threads
    let until = Instant::now() + Duration::from_millis(1500);
    let mut workers = Vec::new();
    while Instant::now() < until {
        let permit = gov.permit();
        workers.push(std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            drop(permit);
        }));
    }
    for w in workers {
        w.join().unwrap();
    }
    std::thread::sleep(Duration::from_millis(50));
    gov.unsubscribe(id);
    assert!(pushes.lock().len() >= 10); // ~20 Hz here (push_interval), 2 Hz in production
    let st = gov.status(true);
    assert!(st.measured && st.limit == 35.0 && !st.history.is_empty(), "{st:?}");
    assert!(0 < st.concurrency && st.concurrency <= 8);
    let here = tempfile::tempdir().unwrap();
    let other = registry.governor_for_path(here.path()); // real mapping of this machine's disk
    assert!(other.disk().id != disk().id);
    assert!(registry.governors().iter().any(|g| Arc::ptr_eq(g, &other)));
    assert!(watched.lock().iter().any(|r| r.id == other.disk().id) || !other.disk().sampled());
    let net = registry.governor("net:\\\\nas\\share");
    std::thread::sleep(Duration::from_millis(150));
    assert!(!net.status(false).measured); // ticked open loop
    registry.stop();
    assert!(!registry.running());
}

#[test]
fn a_disk_the_sampler_never_measures_runs_open_loop() {
    // the sampler measures other disks but never this one (a mapping mismatch): the governor
    // ticks open loop (the mismatch is logged once, with names)
    let opts = RegistryOptions {
        idle_interval: 0.05,
        active_interval: 0.05,
        ..Default::default()
    };
    let (registry, _slot, _watched) = fake_registry(opts);
    registry.start();
    let stray = registry.governor_for_ref(&DiskRef::physical("disk9").with_name("disk9"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!stray.status(false).measured);
    assert!(registry.governor(&disk().id).status(false).measured);
    registry.stop();
}
