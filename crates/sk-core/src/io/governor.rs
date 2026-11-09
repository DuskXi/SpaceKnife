//! Per-disk I/O governor: closed-loop disk-load limiting (Python `io/governor.py`, docs/native.md
//! §4, UI-W2-29…45, G3).
//!
//! Measured load `rho = min(busy, queue / Q_eff)` (`Q_eff`: the disk's effective parallelism, a
//! prior by media refined online). SpaceKnife's own share `self = rho - others` where *others* is
//! an EWMA baseline measured in windows during which SpaceKnife had nothing in flight: before a
//! scan, while paused, and in short probes (dispatch held for one sample window) that run
//! periodically and whenever the load moves in a way our own activity does not explain.
//!
//! Two actuators, both behind [`IoGovernor::permit`] (called before every dispatched batch):
//!
//! * the allowed parallelism `concurrency` (integer, 1..max workers), and
//! * a duty-cycle gate, a token bucket in worker-seconds: on average at most `u` batches are in
//!   flight (`u` = concurrency × duty), so fractional intensities (an HDD at 35 %) are exact.
//!
//! The controller adjusts the intensity `u` every sample window, AIMD-shaped: additive increase
//! of at most one worker per period while under the setpoint (proportional when the gain is known),
//! multiplicative decrease (at most halving) when over the limit. Yielding (G3): when the others
//! alone exceed the limit, the scan drops to 5 % duty until they fall back.
//!
//! [`GovernorCore`] is the synchronous, clock-free logic (tests drive it with a simulated disk);
//! [`IoGovernor`] wraps it for threads (blocking permits). One governor per physical disk, shared
//! by every scan on it.

use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
pub use sk_protocol::io::{IoStatus, LoadLevel as Level, LoadPoint};

use super::disks::DiskRef;
use super::sampler::{DiskSample, monotonic};
use crate::media::Media;

pub const DEFAULT_CUSTOM: f64 = 70.0;
/// Lowest intensity: 5 % of one worker ("最低进度").
pub const U_MIN: f64 = 0.05;
pub const U_YIELD: f64 = 0.05;
/// Mean in-flight below this: the window shows the others alone.
pub const CLEAN_N: f64 = 0.05;
/// Idle level: others above this make the scan yield.
pub const IDLE_OTHERS: f64 = 0.10;
/// Whole-disk load from which "total minus baseline" no longer tells our share.
pub const SATURATED: f64 = 0.95;
/// Doubles up to PROBE_EVERY_MAX while the baseline holds still.
pub const PROBE_EVERY: f64 = 8.0;
pub const PROBE_EVERY_MAX: f64 = 32.0;
pub const PROBE_EVERY_IDLE: f64 = 3.0;
pub const PROBE_EVERY_YIELDING: f64 = 2.0;
pub const PROBE_MIN_GAP: f64 = 1.0;
/// Additive increase: at most one worker per this much time.
pub const STEP_PERIOD: f64 = 0.5;
/// Unused this long: start the next scan from the media prior again.
pub const IDLE_RESET_S: f64 = 30.0;
pub const HISTORY: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaProfile {
    /// Prior effective parallelism; None = not measurable (open loop).
    pub q_eff: Option<f64>,
    /// Initial intensity, in-flight batches.
    pub initial: f64,
    /// Concurrency ceiling for this media (None: max workers).
    pub cap: Option<usize>,
}

pub fn profile(media: Media) -> MediaProfile {
    let p = |q_eff: Option<f64>, initial: f64, cap: Option<usize>| MediaProfile {
        q_eff,
        initial,
        cap,
    };
    match media {
        Media::NvmeSsd => p(Some(32.0), 4.0, None),
        Media::SataSsd | Media::Ssd | Media::Virtual => p(
            Some(8.0),
            if media == Media::Virtual { 2.0 } else { 3.0 },
            None,
        ),
        Media::Hdd | Media::UsbHdd => p(Some(1.0), 1.0, Some(2)), // low concurrency: seeks thrash (UI-W2-37)
        Media::UsbSsd => p(Some(2.0), 2.0, Some(4)),
        Media::UsbFlash => p(Some(2.0), 1.0, Some(2)),
        Media::SdCard => p(Some(1.0), 1.0, Some(2)),
        Media::Network => p(None, 2.0, Some(8)),
        Media::Unknown => p(Some(4.0), 2.0, None),
    }
}

/// Limit of a level as a fraction (max and idle do not cap SpaceKnife's own load).
pub fn level_limit(level: Level, custom: Option<f64>) -> f64 {
    match level {
        Level::Max | Level::Idle => 1.0,
        Level::Balanced => 0.70,
        Level::Quiet => 0.35,
        Level::Custom => (custom.unwrap_or(DEFAULT_CUSTOM) / 100.0).clamp(0.01, 1.0),
    }
}

/// Where the controller aims: a little under the limit so noise rarely crosses it.
pub fn setpoint(limit: f64) -> f64 {
    limit - (0.03 * limit).max(0.01)
}

/// `SPACEKNIFE_IO_TRACE=<file>`: one line per disk per sample window appended to that file (what
/// the governor measured and decided; for measurements, not an interface).
fn trace_sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("SPACEKNIFE_IO_TRACE")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

fn wall_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn round_to(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round() / f
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Armed,
    Holding,
}

// --------------------------------------------------------------------------- core

/// Synchronous governor logic; every method takes the current monotonic time in seconds.
#[derive(Clone, Debug)]
pub struct GovernorCore {
    pub disk: DiskRef,
    pub max_workers: usize,
    pub profile: MediaProfile,
    pub q_eff: Option<f64>,
    pub measured: bool,
    open_loop: bool,
    /// Whole disk, last sample (shown, not controlled).
    pub read_bps: Option<f64>,
    /// Mean seconds per read, last sample.
    pub read_latency: Option<f64>,
    // settings
    pub level: Level,
    pub custom: f64,
    pub manual: Option<usize>,
    pub yield_when_busy: bool,
    pauses: HashSet<String>,
    // actuators
    pub u: f64,
    pub tokens: f64,
    pub inflight: usize,
    waiters: VecDeque<u64>,
    t: f64,
    /// EWMA of batch duration.
    pub batch_s: f64,
    last_dispatch: f64,
    // per-window accounting
    area: f64,
    win_t0: f64,
    win_area0: f64,
    win_peak: usize,
    win_blocked: bool,
    last_n: f64,
    // estimator
    lat_ref: [Option<f64>; 9],
    saturated: u32,
    pub others: Option<f64>,
    /// Own load per in-flight batch, learned when others are known.
    pub g_ref: Option<f64>,
    last_clean: f64,
    anomalies: u32,
    pub total: f64,
    pub self_load: f64,
    pub active: f64,
    shown: [f64; 3],
    pub yielding: bool,
    u_before_yield: f64,
    gain: Option<f64>,
    gains: VecDeque<f64>,
    // probes
    probe: Option<Probe>,
    hold_from: f64,
    last_probe: f64,
    probe_every: f64,
    confirm: bool,
    dt: f64,
    // history (one point per second)
    pub history: VecDeque<LoadPoint>,
    bucket: (f64, f64, f64, bool, bool, f64), // self·dt, total·dt, dt, yielding, paused, active·dt
}

impl GovernorCore {
    pub const GAIN_ALPHA: f64 = 0.3; // EWMA weight of a new gain measurement (per sample window)
    pub const INCREASE_GAIN: f64 = 0.3; // share of the gap to the setpoint closed per window when under it

    pub fn new(disk: DiskRef, max_workers: usize, now: f64) -> Self {
        let profile = profile(disk.media);
        let mut me = Self {
            disk,
            max_workers: max_workers.max(1),
            profile,
            q_eff: profile.q_eff,
            measured: false,
            open_loop: false,
            read_bps: None,
            read_latency: None,
            level: Level::Balanced,
            custom: DEFAULT_CUSTOM,
            manual: None,
            yield_when_busy: true,
            pauses: HashSet::new(),
            u: 0.0,
            tokens: 0.0,
            inflight: 0,
            waiters: VecDeque::new(),
            t: now,
            batch_s: 0.03,
            last_dispatch: f64::NEG_INFINITY,
            area: 0.0,
            win_t0: now,
            win_area0: 0.0,
            win_peak: 0,
            win_blocked: false,
            last_n: 0.0,
            lat_ref: [None; 9],
            saturated: 0,
            others: None,
            g_ref: None,
            last_clean: f64::NEG_INFINITY,
            anomalies: 0,
            total: 0.0,
            self_load: 0.0,
            active: 0.0,
            shown: [0.0; 3],
            yielding: false,
            u_before_yield: 0.0,
            gain: None,
            gains: VecDeque::with_capacity(3),
            probe: None,
            hold_from: 0.0,
            last_probe: f64::NEG_INFINITY,
            probe_every: PROBE_EVERY,
            confirm: false,
            dt: 1.0,
            history: VecDeque::with_capacity(HISTORY),
            bucket: (0.0, 0.0, 0.0, false, false, 0.0),
        };
        me.u = (me.cap() as f64).min(me.initial(level_limit(me.level, None)));
        me.u_before_yield = me.u;
        me
    }

    // ------------------------------------------------------------------ derived

    pub fn limit(&self) -> f64 {
        level_limit(self.level, Some(self.custom))
    }

    pub fn paused(&self) -> bool {
        !self.pauses.is_empty()
    }

    pub fn auto(&self) -> bool {
        self.manual.is_none()
    }

    pub fn concurrency(&self) -> usize {
        if let Some(m) = self.manual {
            return m;
        }
        ((self.u - 1e-9).ceil().max(0.0) as usize).clamp(1, self.cap())
    }

    pub fn duty(&self) -> f64 {
        (self.u / self.concurrency() as f64).min(1.0)
    }

    pub fn low_io_priority(&self) -> bool {
        self.level == Level::Idle
    }

    pub fn waiting(&self) -> usize {
        self.waiters.len()
    }

    pub fn active_recently(&self, now: f64, within: f64) -> bool {
        self.inflight > 0 || !self.waiters.is_empty() || now - self.last_dispatch < within
    }

    fn cap(&self) -> usize {
        if let Some(m) = self.manual {
            return m;
        }
        self.max_workers
            .min(self.profile.cap.unwrap_or(self.max_workers))
            .max(1)
    }

    /// Start intensity: the media prior, or what one I/O-bound worker per `Q_eff` predicts.
    fn initial(&self, limit: f64) -> f64 {
        match self.q_eff {
            Some(q) if limit < 1.0 => U_MIN.max(self.profile.initial.min(setpoint(limit) * q)),
            _ => self.profile.initial,
        }
    }

    fn limited(&self) -> bool {
        !matches!(self.level, Level::Max | Level::Idle) && self.limit() < 1.0
    }

    // ------------------------------------------------------------------ settings

    pub fn set_media(&mut self, media: Media) {
        if media == self.disk.media {
            return;
        }
        self.disk.media = media;
        self.profile = profile(media);
        self.q_eff = self.profile.q_eff;
        self.lat_ref = [None; 9];
        self.u = self.u.min(self.cap() as f64);
    }

    pub fn set_level(&mut self, level: Level, custom: Option<f64>, now: f64) {
        self.integrate(now);
        let old = self.limit();
        self.level = level;
        if let Some(c) = custom {
            self.custom = c.clamp(1.0, 100.0);
        }
        let new = self.limit();
        if !self.limited() {
            self.yielding = self.yielding && level == Level::Idle;
            self.u = self.cap() as f64;
        } else if self.open_loop {
            self.u = self.open_loop_u();
        } else if new < old || old >= 1.0 {
            // apply the new limit at once (UI-W2-33), scaled by the last measurement
            let base = self.u.min(self.last_n.max(U_MIN));
            if self.self_load > 0.0 {
                self.u = base * (setpoint(new) / self.self_load).clamp(0.1, 1.0);
            } else {
                self.u = self.u.min(self.initial(new));
            }
        }
        self.u = self.u.max(U_MIN).min(self.cap() as f64);
        self.tokens = self.tokens.min(self.burst());
        if old != new {
            tracing::debug!(
                "io {}: level {} ({:.0}%) u={:.2}",
                self.disk.id,
                level.as_str(),
                new * 100.0,
                self.u
            );
        }
    }

    pub fn set_manual_concurrency(&mut self, n: Option<usize>, now: f64) {
        self.integrate(now);
        self.manual = n.map(|n| n.max(1));
        self.u = self.u.max(U_MIN).min(self.cap() as f64);
        if !self.limited() {
            self.u = self.cap() as f64;
        }
    }

    pub fn set_yield_when_busy(&mut self, on: bool) {
        self.yield_when_busy = on;
        if !on && self.level != Level::Idle && self.yielding {
            self.leave_yield();
        }
    }

    pub fn set_max_workers(&mut self, n: usize) {
        self.max_workers = n.max(1);
        self.u = self.u.min(self.cap() as f64);
    }

    pub fn pause(&mut self, owner: &str) {
        self.pauses.insert(owner.to_owned());
    }

    pub fn resume(&mut self, owner: &str) {
        self.pauses.remove(owner);
    }

    // ------------------------------------------------------------------ dispatch gate

    fn burst(&self) -> f64 {
        0.05 + 0.05 * self.u
    }

    fn integrate(&mut self, now: f64) {
        let dt = now - self.t;
        if dt <= 0.0 {
            return;
        }
        self.area += self.inflight as f64 * dt;
        self.tokens =
            (self.tokens + (self.u - self.inflight as f64) * dt).clamp(-1.0, self.burst());
        self.t = now;
    }

    fn duty_limited(&self) -> bool {
        self.u < self.concurrency() as f64 - 1e-9
    }

    /// Credit one more batch needs: its expected share above the intensity `u` (pre-paid, so
    /// several free slots do not all start at once and leave a long debt behind).
    fn price(&self) -> f64 {
        (self.inflight as f64 + 1.0 - self.u).max(0.0) * self.batch_s
    }

    fn probe_blocks(&mut self, now: f64) -> bool {
        let Some(p) = self.probe else { return false };
        if now > self.hold_from + 2.5 * self.dt.max(0.25) {
            self.probe = None; // sampling stalled: never hang a scan
            return false;
        }
        p == Probe::Holding || now >= self.hold_from
    }

    pub fn can_dispatch(&mut self, now: f64) -> bool {
        self.integrate(now);
        if !self.pauses.is_empty() || self.probe_blocks(now) || self.inflight >= self.concurrency()
        {
            return false;
        }
        !self.duty_limited() || self.tokens >= self.price()
    }

    /// Dispatch at once if nobody is queued and the gate is open.
    pub fn try_acquire(&mut self, now: f64) -> bool {
        if !self.waiters.is_empty() || !self.can_dispatch(now) {
            return false;
        }
        self.dispatch(now);
        true
    }

    pub fn request(&mut self, key: u64, _now: f64) {
        self.waiters.push_back(key);
        self.win_blocked = true;
    }

    pub fn cancel(&mut self, key: u64) {
        if let Some(i) = self.waiters.iter().position(|&k| k == key) {
            self.waiters.remove(i);
        }
    }

    /// Pop (FIFO) and dispatch every waiter the gate lets through now.
    pub fn grant(&mut self, now: f64) -> Vec<u64> {
        let mut out = Vec::new();
        while !self.waiters.is_empty() && self.can_dispatch(now) {
            out.push(self.waiters.pop_front().expect("non-empty"));
            self.dispatch(now);
        }
        if !self.waiters.is_empty() {
            self.win_blocked = true;
        }
        out
    }

    /// When [`grant`](Self::grant) may succeed next; None: only after an event (release, resume,
    /// sample).
    pub fn wake_time(&mut self, now: f64) -> Option<f64> {
        if self.waiters.is_empty() || !self.pauses.is_empty() {
            return None;
        }
        self.integrate(now);
        if let Some(p) = self.probe {
            let stall = self.hold_from + 2.5 * self.dt.max(0.25) + 1e-3;
            if p == Probe::Holding || now >= self.hold_from {
                return Some(stall);
            }
        }
        if self.inflight >= self.concurrency() {
            return None;
        }
        let price = self.price();
        if self.duty_limited() && self.tokens < price {
            let rate = self.u - self.inflight as f64;
            return if rate <= 0.0 {
                None
            } else {
                Some(now + (price - self.tokens) / rate + 1e-4)
            };
        }
        Some(now)
    }

    fn dispatch(&mut self, now: f64) {
        self.integrate(now);
        self.inflight += 1;
        self.win_peak = self.win_peak.max(self.inflight);
        self.last_dispatch = now;
    }

    pub fn release(&mut self, now: f64, started: Option<f64>) {
        self.integrate(now);
        self.inflight = self.inflight.saturating_sub(1);
        if let Some(s) = started
            && now > s
        {
            self.batch_s += 0.2 * ((now - s).min(2.0) - self.batch_s);
        }
    }

    // ------------------------------------------------------------------ measurement

    /// Refine Q_eff from read statistics by Little's law: of `queue` outstanding I/Os,
    /// `queue · L_ref / L` are in service and the rest wait. `L_ref` is the lowest read latency
    /// seen for transfers of about this size, learned only while our own reads dominate the disk.
    ///
    /// More I/Os in service than Q_eff at unchanged latency: the disk is wider than assumed, raise
    /// it. Read latency stuck far above the reference at a deep queue: the disk is saturated at
    /// what is in service, lower it (slowly, never below a quarter of the prior). Otherwise drift
    /// back to the prior, which forgets passing interference.
    fn learn_q_eff(&mut self, s: &DiskSample, own_share: f64) {
        let (Some(prior), Some(mut q)) = (self.profile.q_eff, self.q_eff) else {
            return;
        };
        let (lat, iops) = (s.read_latency.unwrap_or(0.0), s.read_iops.unwrap_or(0.0));
        if lat == 0.0 || iops == 0.0 || iops * s.dt < 20.0 || own_share < 0.7 {
            self.q_eff = Some(q + 0.002 * (prior - q));
            self.saturated = 0;
            return;
        }
        let size = s.read_bps.unwrap_or(0.0) / iops;
        // 4 KiB … 1 MiB+
        let class = if size <= 4096.0 {
            0
        } else {
            ((size / 4096.0).log2().round() as usize).min(8)
        };
        let drift = 1.0 + 0.004 * s.dt; // old minima are forgotten over minutes
        let r = match self.lat_ref[class] {
            None => lat,
            Some(r) => (r * drift).min(lat),
        };
        self.lat_ref[class] = Some(r);
        let in_service = s.queue * r / lat;
        let saturated = s.queue >= 4.0 && s.busy >= 0.95 && lat >= 4.0 * r;
        self.saturated = if saturated { self.saturated + 1 } else { 0 };
        if lat <= 1.5 * r && in_service > q {
            q += 0.2 * (in_service - q);
        } else if self.saturated >= 4 {
            q += 0.05 * (in_service.max(1.0) - q);
        } else {
            q += 0.002 * (prior - q);
        }
        self.q_eff = Some(q.max((prior / 4.0).max(1.0)).min(prior * 4.0));
    }

    fn rho(&self, s: &DiskSample) -> f64 {
        match self.q_eff {
            None => s.busy,
            Some(q) => s.busy.min(s.queue / q).clamp(0.0, 1.0),
        }
    }

    /// Feed the load of the window that just ended (None: no data) and run one control step.
    pub fn observe(&mut self, sample: Option<&DiskSample>, now: f64, wall: Option<f64>) {
        self.observe_listings(sample, now, wall, None);
    }

    /// [`observe`](Self::observe) with the native walker's gate over the same window
    /// (`listings`: seconds spent listing, the most listings in flight at once, whether a listing
    /// had to wait for the gate): the concurrency is then counted in listings in flight rather
    /// than in batches.
    pub fn observe_listings(
        &mut self,
        sample: Option<&DiskSample>,
        now: f64,
        wall: Option<f64>,
        listings: Option<(f64, usize, bool)>,
    ) {
        self.integrate(now);
        let span = now - self.win_t0;
        let mut n_bar = if span > 0.0 {
            (self.area - self.win_area0) / span
        } else {
            0.0
        };
        let (mut peak, mut blocked) = (self.win_peak, self.win_blocked);
        if let Some((busy_s, lpeak, refused)) = listings {
            n_bar = if span > 0.0 { busy_s / span } else { 0.0 };
            peak = lpeak;
            blocked = blocked || refused;
        }
        self.win_t0 = now;
        self.win_area0 = self.area;
        self.win_peak = self.inflight;
        self.win_blocked = !self.waiters.is_empty();
        self.last_n = n_bar;
        let demand = peak > 0 || blocked;
        if !demand && self.limited() && now - self.last_dispatch > IDLE_RESET_S {
            self.u = self.initial(self.limit());
        }
        let Some(sample) = sample else {
            self.measured = false;
            self.open_loop = true;
            self.total = 0.0;
            self.self_load = 0.0;
            self.active = 0.0;
            self.read_bps = None;
            self.read_latency = None;
            if self.limited() && demand && self.pauses.is_empty() {
                self.u = self.open_loop_u();
            }
            self.record(span, wall);
            return;
        };
        self.measured = true;
        self.open_loop = false;
        self.read_bps = sample.read_bps;
        self.read_latency = sample.read_latency;
        if sample.dt > 0.0 {
            self.dt = sample.dt;
        }
        let rho = self.rho(sample);
        self.active = sample.busy;
        self.total = rho;
        let clean = n_bar <= CLEAN_N;
        let mut held = false; // this window was a probe: dispatch was held on purpose
        if self.probe == Some(Probe::Holding) {
            self.probe = None;
            self.last_probe = now;
            held = true;
            self.tokens = self.tokens.min(0.0); // no burst when dispatch resumes
        } else if self.probe == Some(Probe::Armed) && now >= self.hold_from - 0.1 * self.dt {
            self.probe = Some(Probe::Holding);
        }
        let previous = self.others;
        self.estimate(rho, n_bar, clean, now, held);
        self.learn_q_eff(sample, if rho > 0.0 { self.self_load / rho } else { 0.0 });
        if held && clean {
            // a quiet baseline needs fewer probes; a moving one, more
            let moved = previous.is_none_or(|p| (self.others.unwrap_or(0.0) - p).abs() >= 0.03);
            self.probe_every = if moved {
                PROBE_EVERY
            } else {
                PROBE_EVERY_MAX.min(2.0 * self.probe_every)
            };
        }
        if clean {
            self.update_yield();
        }
        let u_before = self.u;
        if self.pauses.is_empty() && demand && !held {
            self.control(n_bar, peak, blocked, sample.dt);
        }
        self.plan_probe(now, demand);
        if let Some(sink) = trace_sink() {
            let f = |x: Option<f64>| x.map_or("-".to_owned(), |v| format!("{v:.3}"));
            let _ = writeln!(
                sink.lock(),
                "{:.3} {} busy={:.3} queue={:.2} riops={} rlat_us={} rho={:.3} others={} self={:.3} g_ref={} gain={} nbar={:.2} peak={} blocked={} held={} probe={:?} yield={} u={:.2}->{:.2} conc={} q_eff={} limit={:.2}",
                wall.unwrap_or_else(wall_now),
                self.disk.id,
                sample.busy,
                sample.queue,
                sample
                    .read_iops
                    .map_or("-".to_owned(), |v| format!("{v:.0}")),
                sample
                    .read_latency
                    .map_or("-".to_owned(), |v| format!("{:.0}", v * 1e6)),
                rho,
                f(self.others),
                self.self_load,
                f(self.g_ref),
                f(self.gain),
                n_bar,
                peak,
                blocked,
                held,
                self.probe,
                self.yielding,
                u_before,
                self.u,
                self.concurrency(),
                f(self.q_eff),
                self.limit(),
            );
        }
        self.record(span, wall);
    }

    fn estimate(&mut self, rho: f64, n_bar: f64, clean: bool, now: f64, probe: bool) {
        let g = self.g_ref.unwrap_or(0.0);
        if clean {
            let measured = (rho - g * n_bar).max(0.0);
            // A probe is rare and deliberate: trust it, unless it moves the baseline a lot (a
            // device stall can spill into the probe window): then half-trust it and confirm.
            let alpha = if probe
                && self.others.is_some_and(|o| (measured - o).abs() > 0.1)
                && !self.confirm
            {
                self.confirm = true;
                0.5
            } else if probe {
                self.confirm = false;
                1.0
            } else if self.active_recently(now, 10.0) {
                0.6
            } else {
                0.3
            };
            self.others = Some(match self.others {
                None => measured,
                Some(o) => o + alpha * (measured - o),
            });
            self.last_clean = now;
            self.self_load = rho.min(g * n_bar);
            self.anomalies = 0;
            return;
        }
        let base = self.others.unwrap_or(0.0);
        self.self_load = (rho - base).min(rho).max(0.0);
        let saturated = rho >= SATURATED;
        if saturated
            && n_bar >= 0.2
            && let Some(g_ref) = self.g_ref
        {
            // a full disk hides who is using it: total minus baseline says nothing any more, so
            // fall back to the learned load per batch in flight (whichever is larger)
            self.self_load = rho.min(self.self_load.max(g_ref * n_bar));
        }
        let fresh = now - self.last_clean <= 2.5 * self.dt + 1e-6;
        if fresh && !saturated && n_bar >= 0.2 && self.self_load > 0.005 {
            let ratio = self.self_load / n_bar;
            self.g_ref = Some(match self.g_ref {
                None => ratio,
                Some(g) => g + 0.3 * (ratio - g),
            });
        }
        if let (Some(g_ref), Some(others)) = (self.g_ref, self.others)
            && n_bar >= 0.2
        {
            let predicted = (others + g_ref * n_bar).min(1.0);
            let miss = (rho - predicted).abs();
            let scale = predicted.max(rho);
            if miss > 0.15f64.max(0.4 * scale) {
                self.anomalies += 2;
            } else if miss > 0.08f64.max(0.25 * scale) {
                self.anomalies += 1;
            } else {
                self.anomalies = 0;
            }
        }
    }

    fn update_yield(&mut self) {
        let Some(others) = self.others else { return };
        let threshold = if self.level == Level::Idle {
            IDLE_OTHERS
        } else if self.yield_when_busy && self.limited() {
            self.limit()
        } else {
            if self.yielding {
                self.leave_yield();
            }
            return;
        };
        if !self.yielding && others > threshold {
            self.yielding = true;
            self.u_before_yield = self.u;
            self.u = U_YIELD;
            self.tokens = self.tokens.min(0.0);
            tracing::info!(
                "io {}: yielding (others {:.0}% > {:.0}%)",
                self.disk.id,
                others * 100.0,
                threshold * 100.0
            );
        } else if self.yielding && others < 0.85 * threshold {
            self.leave_yield();
        }
    }

    fn leave_yield(&mut self) {
        self.yielding = false;
        self.u = if self.limited() {
            U_MIN.max(0.5 * self.u_before_yield)
        } else {
            self.cap() as f64
        };
        tracing::info!("io {}: yield over, u={:.2}", self.disk.id, self.u);
    }

    fn open_loop_u(&self) -> f64 {
        if self.level == Level::Idle {
            return 1.0;
        }
        U_MIN.max((self.cap() as f64).min(self.cap() as f64 * self.limit()))
    }

    fn control(&mut self, n_bar: f64, peak: usize, blocked: bool, dt: f64) {
        let cap = self.cap() as f64;
        if self.yielding {
            self.u = U_YIELD;
            return;
        }
        if !self.limited() {
            self.u = cap;
            return;
        }
        let (limit, target, load) = (self.limit(), setpoint(self.limit()), self.self_load);
        // additive increase: +1 worker per STEP_PERIOD; far below the setpoint (scan start, after
        // a yield) grow by up to half the intensity per period instead, like TCP slow start
        let far = load < 0.5 * target;
        let per_period = if far {
            (0.5 * self.u).max(1.0).min(self.u.max(0.1))
        } else {
            1.0
        };
        let step = per_period * dt / STEP_PERIOD;
        // The gain (own load per batch in flight) hardly depends on the intensity, so filtering
        // it damps measurement noise without delaying the response to our own changes (n̄ is exact).
        let mut gain = None;
        if n_bar >= 0.05 && load > 0.005 {
            // median of the last three windows first: a single-window device stall is not a trend
            if self.gains.len() == 3 {
                self.gains.pop_front();
            }
            self.gains.push_back(load / n_bar);
            let mut sorted: Vec<f64> = self.gains.iter().copied().collect();
            sorted.sort_by(f64::total_cmp);
            let g_now = sorted[sorted.len() / 2];
            let g = match self.gain {
                None => g_now,
                Some(g) => g + Self::GAIN_ALPHA * (g_now - g),
            };
            self.gain = Some(g);
            gain = Some(g);
        }
        let estimate = gain.map_or(load, |g| g * n_bar);
        if estimate > limit {
            // multiplicative decrease, proportional to the excess, at most halving
            let base = if n_bar >= U_MIN { n_bar } else { self.u }; // what actually produced this load
            self.u = base * (target / estimate).max(0.5);
        } else if estimate < target {
            if let Some(g) = gain {
                let wanted = target / g; // linear plant: load ∝ batches in flight
                self.u += step.min((Self::INCREASE_GAIN * (wanted - self.u)).max(0.0));
            } else {
                self.u += if self.u >= 1.0 {
                    step
                } else {
                    self.u.max(0.1) * step
                };
            }
        }
        if !blocked {
            self.u = self.u.min(peak as f64 + 1.0); // anti-windup: demand, not the gate, is the bottleneck
        }
        self.u = self.u.max(U_MIN).min(cap);
        self.tokens = self.tokens.min(self.burst());
    }

    fn plan_probe(&mut self, now: f64, demand: bool) {
        if self.probe.is_some() || (!self.limited() && self.level != Level::Idle) {
            return;
        }
        if !demand || !self.pauses.is_empty() || now - self.last_probe < PROBE_MIN_GAP {
            return;
        }
        let every = if self.yielding {
            PROBE_EVERY_YIELDING
        } else if self.level == Level::Idle {
            PROBE_EVERY_IDLE
        } else {
            self.probe_every
        };
        if self.confirm || self.anomalies >= 2 || now - self.last_clean >= every {
            self.anomalies = 0;
            self.probe = Some(Probe::Armed);
            let guard = (0.4 * self.dt).min((1.5 * self.batch_s).max(0.05));
            self.hold_from = now + self.dt - guard;
        }
    }

    fn record(&mut self, span: f64, wall: Option<f64>) {
        let span = span.max(0.0);
        let a = span.min(1.0); // EWMA over about one second
        for (i, value) in [self.self_load, self.total, self.active]
            .into_iter()
            .enumerate()
        {
            self.shown[i] += a * (value - self.shown[i]);
        }
        let b = &mut self.bucket;
        b.0 += self.self_load * span;
        b.1 += self.total * span;
        b.2 += span;
        b.3 = b.3 || self.yielding;
        b.4 = b.4 || !self.pauses.is_empty();
        b.5 += self.active * span;
        if b.2 >= 0.95 {
            let point = LoadPoint {
                t: wall.unwrap_or_else(wall_now),
                self_pct: round_to(100.0 * b.0 / b.2, 2),
                total_pct: round_to(100.0 * b.1 / b.2, 2),
                active_pct: round_to(100.0 * b.5 / b.2, 2),
                yielding: b.3,
                paused: b.4,
            };
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(point);
            self.bucket = (0.0, 0.0, 0.0, false, false, 0.0);
        }
    }

    pub fn status(&self, history: bool) -> IoStatus {
        IoStatus {
            disk: self.disk.id.clone(),
            media: self.disk.media.as_str().to_owned(),
            level: self.level,
            limit: round_to(100.0 * self.limit(), 2),
            self_pct: round_to(100.0 * self.shown[0], 2),
            total_pct: round_to(100.0 * self.shown[1], 2),
            others_pct: round_to(100.0 * (self.shown[1] - self.shown[0]).max(0.0), 2),
            active_pct: round_to(100.0 * self.shown[2], 2),
            concurrency: self.concurrency() as i64,
            inflight: self.inflight as i64,
            duty: round_to(self.duty(), 3),
            auto: self.auto(),
            yielding: self.yielding,
            paused: self.paused(),
            measured: self.measured,
            low_io_priority: self.low_io_priority(),
            q_eff: self.q_eff.map(|q| round_to(q, 2)),
            read_bps: self.read_bps.map(f64::round),
            read_latency_ms: self.read_latency.map(|l| round_to(1000.0 * l, 3)),
            history: if history {
                self.history.iter().cloned().collect()
            } else {
                Vec::new()
            },
        }
    }
}

// --------------------------------------------------------------------------- the thread-safe governor

/// The clock governors run on (seconds); tests pass a fake.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Listener = Arc<dyn Fn(&IoStatus) + Send + Sync>;

struct State {
    core: GovernorCore,
    seq: u64,
    granted: HashSet<u64>,
}

/// One dispatched batch. Released when dropped (or by [`release`](Self::release); idempotent,
/// from any thread).
pub struct Permit {
    gov: Arc<IoGovernor>,
    pub started: f64,
    done: AtomicBool,
}

impl Permit {
    pub fn release(&self) {
        if !self.done.swap(true, Ordering::AcqRel) {
            self.gov.release(self.started);
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.release();
    }
}

/// Thread-safe front end of [`GovernorCore`] for one physical disk.
///
/// ```ignore
/// let permit = governor.permit();   // paces and bounds the parallelism (blocks)
/// pool.spawn(move || { work(batch); drop(permit) });  // frees the slot when the batch is done
/// ```
pub struct IoGovernor {
    state: Mutex<State>,
    /// The native walker's gate for this disk, once [`IoGovernor::listing_gate`] made it.
    gate: std::sync::OnceLock<sk_native::hardscan::ListingGate>,
    cond: Condvar,
    clock: Clock,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_listener: AtomicU64,
}

impl IoGovernor {
    pub fn new(disk: DiskRef, max_workers: usize) -> Arc<Self> {
        Self::with_clock(disk, max_workers, Arc::new(monotonic))
    }

    pub fn with_clock(disk: DiskRef, max_workers: usize, clock: Clock) -> Arc<Self> {
        let now = clock();
        Arc::new(Self {
            state: Mutex::new(State {
                core: GovernorCore::new(disk, max_workers, now),
                seq: 0,
                granted: HashSet::new(),
            }),
            cond: Condvar::new(),
            gate: std::sync::OnceLock::new(),
            clock,
            listeners: Mutex::new(Vec::new()),
            next_listener: AtomicU64::new(1),
        })
    }

    /// Run `f` on the core (read its state; tests).
    pub fn with_core<R>(&self, f: impl FnOnce(&mut GovernorCore) -> R) -> R {
        let mut st = self.state.lock();
        let r = f(&mut st.core);
        self.pump(&mut st);
        r
    }

    pub fn disk(&self) -> DiskRef {
        self.state.lock().core.disk.clone()
    }

    pub fn level(&self) -> Level {
        self.state.lock().core.level
    }

    /// Current limit, percent.
    pub fn limit(&self) -> f64 {
        100.0 * self.state.lock().core.limit()
    }

    pub fn concurrency(&self) -> usize {
        self.state.lock().core.concurrency()
    }

    pub fn paused(&self) -> bool {
        self.state.lock().core.paused()
    }

    pub fn yielding(&self) -> bool {
        self.state.lock().core.yielding
    }

    /// Workers should read at OS low I/O priority.
    pub fn low_io_priority(&self) -> bool {
        self.state.lock().core.low_io_priority()
    }

    /// Scans are using this disk (the registry samples faster then).
    pub fn active(&self) -> bool {
        self.state.lock().core.active_recently((self.clock)(), 5.0)
    }

    // ------------------------------------------------------------------ dispatch

    /// Wait until one more batch may be dispatched on this disk (FIFO across scans).
    pub fn permit(self: &Arc<Self>) -> Permit {
        self.permit_until(&|| false).expect("never cancelled")
    }

    /// A permit if one is free right now (nobody queued, the gate open).
    pub fn try_permit(self: &Arc<Self>) -> Option<Permit> {
        let now = (self.clock)();
        let mut st = self.state.lock();
        st.core.try_acquire(now).then(|| self.new_permit(now))
    }

    /// [`permit`](Self::permit) that gives up (None) once `cancelled()` returns true (checked at
    /// least every 50 ms).
    pub fn permit_until(self: &Arc<Self>, cancelled: &dyn Fn() -> bool) -> Option<Permit> {
        let mut st = self.state.lock();
        let now = (self.clock)();
        if st.core.try_acquire(now) {
            return Some(self.new_permit(now));
        }
        st.seq += 1;
        let key = st.seq;
        st.core.request(key, now);
        self.pump(&mut st);
        loop {
            if st.granted.remove(&key) {
                return Some(self.new_permit((self.clock)()));
            }
            if cancelled() {
                st.core.cancel(key);
                if st.granted.remove(&key) {
                    // granted in the same moment it was cancelled
                    let now = (self.clock)();
                    st.core.release(now, None);
                    self.pump(&mut st);
                }
                return None;
            }
            let now = (self.clock)();
            let wait = st
                .core
                .wake_time(now)
                .map_or(Duration::from_millis(50), |w| {
                    Duration::from_secs_f64((w - now).clamp(0.0, 0.05))
                });
            if !wait.is_zero() {
                self.cond.wait_for(&mut st, wait);
            }
            self.pump(&mut st);
        }
    }

    fn new_permit(self: &Arc<Self>, started: f64) -> Permit {
        Permit {
            gov: self.clone(),
            started,
            done: AtomicBool::new(false),
        }
    }

    /// The disk's gate for the native walker's listings, made on first use: it holds the listings
    /// in flight to [`concurrency`](Self::concurrency) and lets no new one start while paused (but
    /// those a call must list where it found them, docs/native.md 2.2). From then on the governor
    /// counts in listings: the mean and the peak in flight of a window are the gate's.
    pub fn listing_gate(&self) -> sk_native::hardscan::ListingGate {
        self.gate
            .get_or_init(|| {
                let st = self.state.lock();
                let g = sk_native::hardscan::ListingGate::new(st.core.concurrency());
                g.set(None, Some(st.core.paused()));
                g
            })
            .clone()
    }

    fn sync_gate(&self, core: &GovernorCore) {
        if let Some(g) = self.gate.get() {
            g.set(Some(core.concurrency()), Some(core.paused()));
        }
    }

    fn pump(&self, st: &mut State) {
        self.sync_gate(&st.core);
        let now = (self.clock)();
        let granted = st.core.grant(now);
        if !granted.is_empty() {
            st.granted.extend(granted);
            self.cond.notify_all();
        }
    }

    fn release(&self, started: f64) {
        let mut st = self.state.lock();
        st.core.release((self.clock)(), Some(started));
        self.pump(&mut st);
        self.cond.notify_all();
    }

    // ------------------------------------------------------------------ control

    fn update(&self, f: impl FnOnce(&mut GovernorCore, f64)) {
        let mut st = self.state.lock();
        let now = (self.clock)();
        f(&mut st.core, now);
        self.pump(&mut st);
        self.cond.notify_all();
    }

    pub fn set_level(&self, level: Level, custom: Option<f64>) {
        self.update(|c, now| c.set_level(level, custom, now));
    }

    /// Fix the parallelism (UI-W2-45); the limit still holds through the duty gate. None: auto.
    pub fn set_manual_concurrency(&self, n: Option<usize>) {
        self.update(|c, now| c.set_manual_concurrency(n, now));
    }

    pub fn set_yield_when_busy(&self, on: bool) {
        self.update(|c, _| c.set_yield_when_busy(on));
    }

    pub fn set_max_workers(&self, n: usize) {
        self.update(|c, _| c.set_max_workers(n));
    }

    pub fn set_media(&self, media: Media) {
        self.update(|c, _| c.set_media(media));
    }

    /// Stop granting permits (batches in flight finish); several owners may pause at once.
    pub fn pause(&self, owner: &str) {
        self.update(|c, _| c.pause(owner));
    }

    pub fn resume(&self, owner: &str) {
        self.update(|c, _| c.resume(owner));
    }

    /// Called by the registry with every new sample of this disk (None: no data).
    pub fn observe(&self, sample: Option<&DiskSample>, wall: Option<f64>) {
        let listings = self.gate.get().map(|g| g.window());
        self.update(|c, now| c.observe_listings(sample, now, wall, listings));
    }

    // ------------------------------------------------------------------ status

    pub fn status(&self, history: bool) -> IoStatus {
        let mut status = self.state.lock().core.status(history);
        if let Some(g) = self.gate.get() {
            // in listings: those in flight now, and (automatic) no more than the walker's width
            if status.auto {
                status.concurrency = status
                    .concurrency
                    .min(sk_native::hardscan::listing_threads(false).width as _);
            }
            status.inflight = g.inflight() as _;
        }
        status
    }

    /// Status pushes (2 Hz, from the registry); returns the id for [`unsubscribe`](Self::unsubscribe).
    pub fn subscribe(&self, callback: Listener) -> u64 {
        let id = self.next_listener.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().push((id, callback));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.listeners.lock().retain(|(i, _)| *i != id);
    }

    pub fn has_listeners(&self) -> bool {
        !self.listeners.lock().is_empty()
    }

    pub fn push(&self) {
        let listeners: Vec<Listener> = self
            .listeners
            .lock()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        if listeners.is_empty() {
            return;
        }
        let status = self.status(true);
        for cb in listeners {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&status))).is_err() {
                tracing::error!("io status listener failed");
            }
        }
    }
}
