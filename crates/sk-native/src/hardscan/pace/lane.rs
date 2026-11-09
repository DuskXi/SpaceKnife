//! One listing lane's width (docs/native.md 2.2 「列目录线程」, 每条道跟着自己的曲线): it follows
//! the rate-against-listings-in-flight curve, BBR-like. Clock-free (times in nanoseconds from any
//! origin): the controller (ctl.rs) feeds it, the simulator too.
//!
//! - A *round* is at least max(32, 3 × width) completions and 3 ms, longer in proportion to the
//!   noise past 10 % (up to 24 ms), or 250 ms and 4 completions on slow devices. It measures this lane's rate (folders plus entries / 64 per second,
//!   `units`), every lane's together over the same time (the *total*), the mean listings in flight
//!   (sampled at each completion) and the time per unit. A round whose mean in flight is below 0.8
//!   of the width is *limited*: the lane had no more folders to list, or the disk's gate or the
//!   calls held it; it says nothing about the width. After a width change the completions of the
//!   listings then in flight are skipped before the next round starts.
//! - Steps are judged by the lane's own rate, against the round-to-round noise of that rate at
//!   one width (`noise`); the total keeps a veto: a step after which the total fell by more than
//!   its own noise (as a share of this lane's rate) did not pay, whatever this lane gained — the
//!   lanes share the CPUs and the disk, and a lane that widens can gain what the other loses. The
//!   total is no measure otherwise: the other lane's rate swings with its demand (a warm subtree
//!   comes in a burst), ±40 % from round to round in a mixed tree. Only one lane takes steps at a
//!   time (`may_step`, ctl.rs).
//! - Startup: from the start width, ×`grow` per full round while the rate gains a quarter of
//!   what the step would give on a straight line (×2: 25 %, ×1.5: 12.5 %), up to `startup_hi`
//!   (wider only by probes); after two rounds that do not, clamp(1.25 × BDP, the best round's
//!   width, the width now), but not below `home` (0.3.5's width) if the lane got that far. BDP =
//!   the highest own rate × the lowest time per unit of the last 16 rounds: the listings in flight
//!   at the curve's knee (Little's law). Startup never goes narrower: a round or two decide
//!   nothing against rates that swing tenfold from round to round (W: (ReFS) cold: one 80 ms round
//!   at 32 took the IO lane back to 16 for the whole scan, 1.5 times slower); narrower is the
//!   probes' to find, against the noise.
//! - Cruise: every 4 full rounds a probe: a round ×1.25 wider, a round back, a round ×0.8
//!   narrower, a round back. Wider stays if its rate beat the mean of the rounds around it by
//!   `keep_wider` − 1 plus twice the noise; else narrower stays if its rate was at least
//!   `keep_narrower` of that mean plus twice the noise. A probe that stayed is followed at once by another one the
//!   same way; after a cycle that changed nothing the next waits twice as long (up to 64
//!   rounds), back to 4 after a kept probe or a cruising round 3 × the noise off the last.
//! - Too noisy to judge (the noise above 25 % even with the longest rounds): no probes; every 4
//!   rounds a step (×1.25 / ×0.8) towards home — where startup settled — until the noise falls
//!   (W: (ReFS) cold: rates ten times apart from round to round, the lanes drifted).
//! - The knee (`knee`, the CPU lane's: ctl.rs bounds the IO lane by it): the width a probe cycle
//!   ended at with a wider width measured no better; none until one did, none once the width
//!   moved otherwise (the BDP guard, home, a cap). A width a lane merely settled at, or was
//!   narrowed to for its small BDP, is no knee (on W: such a width, 4 or 8, capped the IO lane at
//!   18 where 32 lists 1.5 times faster).
//! - BDP guard: a width above 2 × gain × BDP goes down to 1.25 × gain × BDP at once.
//! - Limited for 1 s: the next full round starts up again from the width now.

const MIN_ROUND_NS: u64 = 3_000_000;
const MIN_ROUND_N: u32 = 32;
const SLOW_ROUND_NS: u64 = 250_000_000;
const SLOW_ROUND_N: u32 = 4;
const LIMITED_BELOW: f64 = 0.8;
const STARTUP_STALLS: u32 = 2;
const CRUISE_ROUNDS: u32 = 4;
/// After a probe cycle that changed nothing the next one waits twice as long, up to this many
/// rounds (a probe costs: a narrower round on a lane at its knee lists slower); a kept probe, or
/// a cruising round that lists clearly differently from the last (3 × the noise: the tree or the
/// cache changed), brings it back to CRUISE_ROUNDS.
const CRUISE_ROUNDS_MAX: u32 = 64;
const PROBE_UP: f64 = 1.25;
const PROBE_DOWN: f64 = 0.8;
const RESTART_AFTER_NS: u64 = 1_000_000_000;
/// A lane counts as experimenting (holding the other back) only while it had a full round this
/// recently.
const HOLD_NS: u64 = 10_000_000;
const HISTORY: usize = 16;
/// A probe must beat the round-to-round noise this many times (about two standard deviations:
/// one lets a plateau ratchet a lane wider by luck, a probe in six).
const NOISE_MARGIN: f64 = 2.0;
/// Round-to-round noise of the rates at one width (relative to the lane's rate, smoothed): a
/// probe must beat it.
const NOISE_START: f64 = 0.05;
/// Above this noise rounds last longer, in proportion (MIN_ROUND_NS × noise / this, up to 8×).
const NOISE_LONGER: f64 = 0.1;
/// Above this noise (even with the longest rounds) a probe cannot tell a better width from
/// luck: the lane steps towards its home width instead (`LaneCfg::home`).
const NOISE_UNSURE: f64 = 0.25;
const NOISE_MEMORY: f64 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaneCfg {
    pub start: usize,
    pub lo: usize,
    pub hi: usize,
    /// Startup: the width times this per full round that gained.
    pub grow: f64,
    pub keep_wider: f64,
    pub keep_narrower: f64,
    /// BDP guard: in flight beyond the knee this lane tolerates (a standing queue).
    pub gain: f64,
    /// The width never changes (a hard disk's IO lane, the fixed modes).
    pub fixed: bool,
    /// Startup grows no wider than this; beyond it only probes widen (a scan's first rounds also
    /// see its tree open up: the demand's ramp passes for the width's).
    pub startup_hi: usize,
    /// What 0.3.5 would list at (the IO lane: its cold width; the CPU lane: its start width):
    /// startup settles no narrower once it got this wide. A lane too noisy to judge
    /// (NOISE_UNSURE) goes back to where startup settled (here before it did).
    pub home: usize,
}

impl LaneCfg {
    pub fn fixed(width: usize) -> Self {
        let w = width.max(1);
        LaneCfg {
            start: w,
            lo: w,
            hi: w,
            grow: 1.0,
            keep_wider: 1.0,
            keep_narrower: 1.0,
            gain: 1.0,
            fixed: true,
            startup_hi: w,
            home: w,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Startup,
    Cruise,
    Wide,
    BackWide,
    Narrow,
    BackNarrow,
}

/// What a round measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Round {
    /// When it ended (ns) and how long it lasted (s).
    pub end: u64,
    pub dt: f64,
    pub n: u32,
    /// This lane's units (folders + entries / 64) per second, and every lane's.
    pub rate: f64,
    pub total: f64,
    /// Seconds of listing per unit.
    pub per_unit: f64,
    /// Mean listings in flight at the completions.
    pub inflight: f64,
    pub width: usize,
    pub limited: bool,
}

/// A round's outcome (the trace's line).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub round: Round,
    pub what: &'static str,
    pub from: usize,
    pub to: usize,
    pub phase: Phase,
    pub bdp: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    t0: Option<u64>,
    total0: f64,
    n: u32,
    units: f64,
    lat_ns: f64,
    inflight: u64,
    limit: u64,
}

/// Rates of a round: (this lane's, the total).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rates {
    own: f64,
    total: f64,
}

impl Rates {
    const NONE: Rates = Rates {
        own: f64::NAN,
        total: f64::NAN,
    };

    fn of(r: &Round) -> Rates {
        Rates {
            own: r.rate,
            total: r.total,
        }
    }

    fn known(self) -> bool {
        self.own.is_finite()
    }

    fn mean(a: Rates, b: Rates) -> Rates {
        match (a.known(), b.known()) {
            (true, true) => Rates {
                own: (a.own + b.own) / 2.0,
                total: (a.total + b.total) / 2.0,
            },
            (true, false) => a,
            _ => b,
        }
    }

    /// This lane's rate against `reference`'s.
    fn own_gain(self, reference: Rates) -> f64 {
        (self.own - reference.own) / reference.own.max(1e-9)
    }

    /// The total against `reference`'s, as a share of the reference's own rate.
    fn total_gain(self, reference: Rates) -> f64 {
        (self.total - reference.total) / reference.own.max(1e-9)
    }
}

pub struct Lane {
    pub cfg: LaneCfg,
    w: usize,
    phase: Phase,
    /// The width a probe goes back to, and the one it tries.
    base: usize,
    probe_w: usize,
    best: Rates,
    best_w: usize,
    stalls: u32,
    cruise: u32,
    /// Full cruising rounds between probes now (CRUISE_ROUNDS doubling to CRUISE_ROUNDS_MAX).
    due: u32,
    /// +1 / -1: the last probe stayed that way, the next one goes the same way at once.
    momentum: i8,
    /// Where startup settled (the noisy lane's home).
    settled: Option<usize>,
    /// The width a probe cycle last ended at with a wider width measured no better.
    probed: Option<usize>,
    /// The last two rounds at the cruising width.
    before: [Rates; 2],
    probe: Rates,
    back: Rates,
    acc: Acc,
    skip: u32,
    last_inflight: u32,
    hist: [Option<Round>; HISTORY],
    next: usize,

    limited_since: Option<u64>,
    last_full: Option<u64>,
    /// The widest this lane may go now (ctl.rs: the IO lane's CPU budget), within `cfg.hi`.
    cap: usize,
    /// The last full round's rates at this width (None after a change), and the noise of this
    /// lane's rate and of the total.
    prev: Option<Rates>,
    pub noise: f64,
    pub noise_total: f64,
    pub rounds: u64,
}

impl Lane {
    pub fn new(cfg: LaneCfg) -> Self {
        let w = cfg.start.clamp(cfg.lo, cfg.hi);
        Lane {
            cfg,
            w,
            phase: if cfg.fixed {
                Phase::Cruise
            } else {
                Phase::Startup
            },
            base: w,
            probe_w: w,
            best: Rates::NONE,
            best_w: w,
            stalls: 0,
            cruise: 0,
            due: CRUISE_ROUNDS,
            settled: None,
            probed: None,
            momentum: 0,
            before: [Rates::NONE; 2],
            probe: Rates::NONE,
            back: Rates::NONE,
            acc: Acc::default(),
            skip: 0,
            last_inflight: 0,
            hist: [None; HISTORY],
            next: 0,

            limited_since: None,
            last_full: None,
            cap: usize::MAX,
            prev: None,
            noise: NOISE_START,
            noise_total: NOISE_START,
            rounds: 0,
        }
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// A new scan: from the start width, startup again.
    pub fn restart(&mut self) {
        let rounds = self.rounds;
        *self = Lane::new(self.cfg);
        self.rounds = rounds;
    }

    /// Mid-experiment (starting up or probing) with full rounds lately: the other lane waits
    /// before it takes a step.
    pub fn holds(&self, now: u64) -> bool {
        !self.cfg.fixed
            && self.phase != Phase::Cruise
            && self
                .last_full
                .is_some_and(|t| now.saturating_sub(t) <= HOLD_NS)
    }

    /// The listings in flight at the knee of the last rounds' curve (None: too few rounds).
    pub fn bdp(&self) -> Option<f64> {
        let (mut rate, mut per_unit, mut n) = (0.0f64, f64::INFINITY, 0);
        for r in self.hist.iter().flatten() {
            n += 1;
            if !r.limited {
                rate = rate.max(r.rate);
            }
            if r.per_unit > 0.0 {
                per_unit = per_unit.min(r.per_unit);
            }
        }
        (n >= 4 && rate > 0.0 && per_unit.is_finite()).then_some(rate * per_unit)
    }

    /// One listing of this lane finished at `now`: it took `wall_ns`, weighed `units`, and
    /// `inflight` listings of the lane were in flight then (itself included) under a limit of
    /// `limit` (the width, or less: the CPU lane's budget, ctl.rs); `total` is the units every
    /// lane listed so far, `may_step` whether this lane may change its width now. Some: a round
    /// ended (the width may have changed).
    #[allow(clippy::too_many_arguments)]
    pub fn sample(
        &mut self,
        now: u64,
        wall_ns: u64,
        units: f64,
        inflight: u32,
        limit: usize,
        total: f64,
        may_step: bool,
    ) -> Option<Decision> {
        self.last_inflight = inflight;
        if self.skip > 0 {
            self.skip -= 1;
            if self.skip == 0 {
                self.acc = Acc {
                    t0: Some(now),
                    total0: total,
                    ..Acc::default()
                };
            }
            return None;
        }
        let acc = &mut self.acc;
        let t0 = match acc.t0 {
            Some(t) => t,
            None => {
                acc.total0 = total - units;
                *acc.t0.insert(now.saturating_sub(wall_ns))
            }
        };
        acc.n += 1;
        acc.units += units;
        acc.lat_ns += wall_ns as f64;
        acc.inflight += inflight as u64;
        acc.limit += limit.max(1) as u64;
        let span = now.saturating_sub(t0);
        let need = MIN_ROUND_N.max(3 * self.w as u32);
        // a noisy tree is judged on longer rounds (up to 8 times: the mean of a round's rate
        // settles with the listings in it)
        let min_ns = (MIN_ROUND_NS as f64 * (self.noise / NOISE_LONGER).clamp(1.0, 8.0)) as u64;
        let full =
            (acc.n >= need && span >= min_ns) || (span >= SLOW_ROUND_NS && acc.n >= SLOW_ROUND_N);
        if !full {
            return None;
        }
        let dt = span.max(1) as f64 / 1e9;
        let inflight_mean = acc.inflight as f64 / acc.n as f64;
        let round = Round {
            end: now,
            dt,
            n: acc.n,
            rate: acc.units / dt,
            total: (total - acc.total0).max(acc.units) / dt,
            per_unit: acc.lat_ns / 1e9 / acc.units.max(1e-9),
            inflight: inflight_mean,
            width: self.w,
            limited: inflight_mean < LIMITED_BELOW * (acc.limit as f64 / acc.n as f64),
        };
        self.acc = Acc {
            t0: Some(now),
            total0: total,
            ..Acc::default()
        };
        self.hist[self.next] = Some(round);
        self.next = (self.next + 1) % HISTORY;

        self.rounds += 1;
        if !round.limited {
            self.last_full = Some(now);
        }
        if !round.limited {
            let rates = Rates::of(&round);
            if let Some(p) = self.prev {
                let own = (0.5 * (rates.own + p.own)).max(1e-9);
                let d = (rates.own - p.own).abs() / own;
                let dt = (rates.total - p.total).abs() / own;
                self.noise += NOISE_MEMORY * (d.min(1.0) - self.noise);
                self.noise_total += NOISE_MEMORY * (dt.min(1.0) - self.noise_total);
            }
            self.prev = Some(rates);
        }
        let from = self.w;
        // a lane held below its width (the CPU lane while the IO lane's listings use the CPUs)
        // takes no steps: they would change nothing it lists at, and drift by luck (on W: the
        // CPU lane's drifted width then passed for a knee of 4 and capped the IO lane at 8)
        let may_step = may_step && limit >= self.w;
        let (what, to) = self.decide(&round, now, may_step);
        // a width within the cap's slack stays (set_cap); a step goes within the cap
        let to = if to == from {
            to
        } else {
            to.clamp(self.cfg.lo, self.hi())
        };
        if to != from {
            // a knee holds until the width moves (a probe cycle names the new one at its end)
            if self.probed != Some(to) {
                self.probed = None;
            }
            self.prev = None;
            self.w = to;
            self.skip = self.last_inflight;
            if self.skip == 0 {
                self.acc.t0 = Some(now);
            }
        }
        Some(Decision {
            round,
            what,
            from,
            to,
            phase: self.phase,
            bdp: self.bdp(),
        })
    }

    fn wider(&self, w: usize) -> usize {
        ((w as f64 * PROBE_UP).ceil() as usize)
            .max(w + 1)
            .min(self.hi())
    }

    /// The widest now: `cfg.hi`, or less under a cap.
    fn hi(&self) -> usize {
        self.cfg.hi.min(self.cap).max(self.cfg.lo)
    }

    /// The widest this lane may go from now on (`usize::MAX`: no cap). A width more than an
    /// eighth above it comes down to it at once (true: it did; a probe in progress goes back to
    /// cruising); a cap that moves by a listing or two (it follows a smoothed CPU share) leaves
    /// the width and its rounds alone, it only bounds the next steps up.
    pub fn set_cap(&mut self, cap: usize, now: u64) -> bool {
        self.cap = cap;
        let hi = self.hi();
        if self.cfg.fixed || self.w <= hi + (hi / 8).max(1) {
            return false;
        }
        self.w = self.hi();
        self.probed = None;
        self.base = self.base.min(self.w);
        if !matches!(self.phase, Phase::Startup | Phase::Cruise) {
            self.cruise_on();
        }
        self.prev = None;
        self.skip = self.last_inflight;
        if self.skip == 0 {
            self.acc.t0 = Some(now);
        }
        true
    }

    /// When this lane last had a full round (a knee it found is current).
    pub fn last_full(&self) -> Option<u64> {
        self.last_full
    }

    /// The width now if a probe cycle found it the knee (a wider width listed no faster) and
    /// nothing moved it since.
    pub fn knee(&self) -> Option<usize> {
        self.probed.filter(|&k| k == self.w)
    }

    fn narrower(&self, w: usize) -> usize {
        ((w as f64 * PROBE_DOWN).floor() as usize)
            .min(w.saturating_sub(1))
            .max(self.cfg.lo)
    }

    fn cruise_on(&mut self) {
        self.phase = Phase::Cruise;
        self.cruise = 0;
    }

    fn decide(&mut self, r: &Round, now: u64, may_step: bool) -> (&'static str, usize) {
        let w = self.w;
        let cfg = self.cfg;
        if cfg.fixed {
            return ("fixed", w);
        }
        if r.limited {
            let since = *self
                .limited_since
                .get_or_insert(now.saturating_sub((r.dt * 1e9) as u64));
            if now.saturating_sub(since) >= RESTART_AFTER_NS && self.phase != Phase::Startup {
                self.phase = Phase::Startup;
                self.best = Rates::NONE;
                self.stalls = 0;
                self.momentum = 0;
                return ("idle", w);
            }
            if !matches!(self.phase, Phase::Startup | Phase::Cruise) {
                self.cruise_on();
                return ("abort", self.base);
            }
            return ("limited", w);
        }
        self.limited_since = None;
        let rates = Rates::of(r);
        match self.phase {
            Phase::Startup => {
                if !may_step {
                    // the other lane is experimenting: this round compares with nothing
                    return ("wait", w);
                }
                // a quarter of what the step would give if the curve were still linear (×2: 25 %)
                let need = (cfg.grow - 1.0) / 4.0;
                let gained = rates.own_gain(self.best) >= need
                    && rates.total_gain(self.best) >= -self.noise_total;
                if !self.best.known() || gained {
                    self.best = rates;
                    self.best_w = w;
                    self.stalls = 0;
                    let top = self.hi().min(cfg.startup_hi.max(cfg.lo));
                    if w < top {
                        let to = ((w as f64 * cfg.grow).ceil() as usize).max(w + 1).min(top);
                        return ("grow", to);
                    }
                    self.cruise_on();
                    self.settled = Some(w);
                    return ("top", w);
                }
                // a round that did not gain is no proof of a knee below (the rates swing): only
                // two in a row end the ramp, and not below where it was
                self.stalls += 1;
                if self.stalls < STARTUP_STALLS {
                    return ("hold", w);
                }
                self.cruise_on();
                self.before = [Rates::NONE, rates];
                let lo = self.best_w.max(cfg.home.min(w)).min(w);
                let to = self
                    .bdp()
                    .map_or(lo, |b| ((1.25 * b).round() as usize).clamp(lo, w));
                self.settled = Some(to);
                ("settle", to)
            }
            Phase::Cruise => {
                if may_step
                    && let Some(b) = self.bdp()
                    && w as f64 > 2.0 * cfg.gain * b
                {
                    let to = ((1.25 * cfg.gain * b).ceil() as usize).max(cfg.lo);
                    if to < w {
                        self.cruise = 0;
                        self.momentum = 0;
                        self.before = [Rates::NONE; 2];
                        return ("bdp", to);
                    }
                }
                let last = self.before[1];
                if last.known()
                    && (rates.own - last.own).abs() > 3.0 * self.noise * last.own.max(1e-9)
                {
                    self.due = CRUISE_ROUNDS;
                }
                self.before = [self.before[1], rates];
                self.cruise += 1;
                // too noisy to judge: towards home, a step per CRUISE_ROUNDS rounds, no probes;
                // home is where startup settled (no narrower than the start width, nor than
                // `cfg.home` if it got that far), not `cfg.home` itself: on 4 CPUs the IO lane's
                // knee lies far below 0.3.5's 32, and a noisy tree drove it there
                let home = self.settled.unwrap_or(cfg.home).clamp(cfg.lo, self.hi());
                if self.noise > NOISE_UNSURE && w != home {
                    if self.cruise < CRUISE_ROUNDS || !may_step {
                        return ("unsure", w);
                    }
                    self.cruise = 0;
                    self.momentum = 0;
                    let to = if w < home {
                        self.wider(w).min(home)
                    } else {
                        self.narrower(w).max(home)
                    };
                    return ("home", to);
                }
                let due = if self.momentum != 0 { 1 } else { self.due };
                if self.cruise >= due && may_step {
                    self.base = w;
                    if self.momentum >= 0 && w < self.hi() {
                        self.phase = Phase::Wide;
                        self.probe_w = self.wider(w);
                        return ("probe+", self.probe_w);
                    }
                    if w > cfg.lo {
                        self.back = Rates::NONE;
                        self.phase = Phase::Narrow;
                        self.probe_w = self.narrower(w);
                        return ("probe-", self.probe_w);
                    }
                    self.momentum = 0;
                    self.cruise = 0;
                }
                ("cruise", w)
            }
            Phase::Wide => {
                self.probe = rates;
                self.phase = Phase::BackWide;
                ("back", self.base)
            }
            Phase::BackWide => {
                let before = Rates::mean(self.before[0], self.before[1]);
                let reference = Rates::mean(before, rates);
                let paid = self.probe.own_gain(reference)
                    >= cfg.keep_wider - 1.0 + NOISE_MARGIN * self.noise
                    && self.probe.total_gain(reference) >= -self.noise_total;
                if paid {
                    self.cruise_on();
                    self.due = CRUISE_ROUNDS;
                    self.momentum = 1;
                    self.probed = None;
                    self.before = [Rates::NONE, self.probe];
                    return ("wider", self.probe_w);
                }
                self.back = rates;
                if self.momentum == 0 && self.base > cfg.lo {
                    self.phase = Phase::Narrow;
                    self.probe_w = self.narrower(self.base);
                    return ("probe-", self.probe_w);
                }
                self.cruise_on();
                self.momentum = 0;
                self.due = (self.due * 2).min(CRUISE_ROUNDS_MAX);
                self.before = [before, rates];
                self.probed = Some(self.base);
                ("stay", self.base)
            }
            Phase::Narrow => {
                self.probe = rates;
                self.phase = Phase::BackNarrow;
                ("back", self.base)
            }
            Phase::BackNarrow => {
                let before = if self.back.known() {
                    self.back
                } else {
                    Rates::mean(self.before[0], self.before[1])
                };
                let reference = Rates::mean(before, rates);
                self.cruise_on();
                let as_fast = self.probe.own_gain(reference)
                    >= cfg.keep_narrower - 1.0 + NOISE_MARGIN * self.noise;
                if as_fast {
                    self.due = CRUISE_ROUNDS;
                    self.momentum = -1;
                    self.before = [Rates::NONE, self.probe];
                    self.probed = Some(self.probe_w);
                    return ("narrower", self.probe_w);
                }
                self.momentum = 0;
                self.due = (self.due * 2).min(CRUISE_ROUNDS_MAX);
                self.before = [before, rates];
                self.probed = Some(self.base);
                ("stay", self.base)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LaneCfg {
        LaneCfg {
            start: 4,
            lo: 2,
            hi: 64,
            grow: 2.0,
            keep_wider: 1.05,
            keep_narrower: 1.0,
            gain: 1.5,
            fixed: false,
            startup_hi: 64,
            home: 4,
        }
    }

    /// A lane alone on a curve: `total(width)` listings in flight are served (listings per
    /// second), each listing is one unit. Runs `secs`; returns the decisions.
    fn run(lane: &mut Lane, curve: impl Fn(f64) -> f64, secs: f64) -> Vec<Decision> {
        let mut t = 0u64;
        let mut total = 0.0;
        let mut out = Vec::new();
        while (t as f64) < secs * 1e9 {
            let w = lane.width() as f64;
            let rate = curve(w);
            let wall = w / rate; // Little
            t += (1e9 / rate) as u64;
            total += 1.0;
            if let Some(d) = lane.sample(
                t,
                (wall * 1e9) as u64,
                1.0,
                w as u32,
                w as usize,
                total,
                true,
            ) {
                out.push(d);
            }
        }
        out
    }

    #[test]
    fn startup_finds_the_knee_within_a_few_rounds() {
        // a device that serves 24 at once, 300 µs each
        let knee = |w: f64| w.min(24.0) / 300e-6;
        let mut lane = Lane::new(cfg());
        let d = run(&mut lane, knee, 0.05);
        let grew: Vec<usize> = d
            .iter()
            .filter(|d| d.what == "grow")
            .map(|d| d.to)
            .collect();
        // 32 still gained (24 / 16), 64 did not: settled at 32, between 1.25 × BDP (30) and 64
        assert_eq!(grew, [8, 16, 32, 64]);
        let settle = d.iter().find(|d| d.what == "settle").expect("settled");
        assert_eq!(settle.to, 32, "{settle:?}");
        assert!(settle.round.end < 20_000_000, "{settle:?}"); // within 20 ms
        // cruising: wider never pays; narrower is as fast but not faster by more than the noise,
        // so it stays at or above the knee (in flight beyond it costs an IO lane little)
        let d = run(&mut lane, knee, 0.5);
        assert!(d.iter().any(|d| d.what == "probe+"));
        assert!((24..=32).contains(&lane.width()), "{}", lane.width());
    }

    #[test]
    fn a_curve_that_falls_past_the_knee_is_followed_down() {
        // lock contention: past 8 in flight the lane lists slower (ReFS warm), from 16
        let mut lane = Lane::new(LaneCfg {
            start: 16,
            lo: 4,
            hi: 32,
            grow: 1.5,
            keep_wider: 1.08,
            keep_narrower: 0.98,
            gain: 1.0,
            fixed: false,
            startup_hi: 32,
            home: 16,
        });
        let contended = |w: f64| {
            let total = if w <= 8.0 {
                w
            } else {
                w / (1.0 + 0.285 * (w - 8.0))
            };
            total / 100e-6
        };
        let d = run(&mut lane, contended, 0.1);
        // startup settles at the start width (never below it) with no knee yet, the probes take
        // it down and name the knee at the end of a cycle
        let settle = d.iter().position(|d| d.what == "settle").expect("settled");
        assert_eq!(d[settle].to, 16);
        assert!(d.iter().all(|d| d.what != "down"));
        let probed = d[settle..]
            .iter()
            .position(|d| d.what == "stay" || d.what == "narrower")
            .expect("a probe cycle");
        assert!(probed > 1, "{:?}", &d[settle..]);
        assert!((6..=9).contains(&lane.width()), "{}", lane.width());
        let at = d
            .iter()
            .rposition(|d| d.what == "stay" || d.what == "narrower");
        let last = &d[at.expect("a probe cycle")];
        assert!((6..=9).contains(&last.to), "{last:?}");
        if lane.phase() == Phase::Cruise && lane.width() == last.to {
            assert_eq!(lane.knee(), Some(last.to));
        }
    }

    #[test]
    fn limited_rounds_change_nothing() {
        let mut lane = Lane::new(cfg());
        let mut t = 0;
        for i in 0..10_000 {
            t += 100_000;
            // one in flight whatever the width: no demand
            if let Some(d) = lane.sample(t, 100_000, 1.0, 1, 4, i as f64, true) {
                assert!(d.round.limited && d.to == d.from, "{d:?}");
            }
        }
        assert_eq!(lane.width(), 4);
        assert_eq!(lane.phase(), Phase::Startup);
        assert!(!lane.holds(t));
    }

    #[test]
    fn a_lane_that_may_not_step_waits() {
        let mut lane = Lane::new(cfg());
        let mut t = 0u64;
        for i in 0..20_000 {
            t += 50_000;
            lane.sample(t, 200_000, 1.0, 4, 4, i as f64, false);
        }
        assert_eq!((lane.width(), lane.phase()), (4, Phase::Startup));
    }

    #[test]
    fn the_total_vetoes_a_step_that_costs_the_other_lane_more() {
        // widening this lane adds its own listings but takes twice as much from the other lane
        // (they share the CPUs): the total falls, so it does not widen, although by its own rate
        // alone it would double each round
        let mut lane = Lane::new(cfg());
        let mut t = 0u64;
        let mut total = 0.0;
        for _ in 0..200_000 {
            let w = lane.width() as f64;
            let own = w / 300e-6;
            let other = (64.0 - 2.0 * w).max(0.0) / 300e-6;
            t += (1e9 / own) as u64;
            total += 1.0 + other / own;
            lane.sample(
                t,
                (300e-6 * 1e9) as u64,
                1.0,
                w as u32,
                w as usize,
                total,
                true,
            );
        }
        // its first step (4 → 8) is the only one it took before the total said no
        assert!(lane.width() <= 8, "{}", lane.width());
    }

    #[test]
    fn a_fixed_lane_stays() {
        let mut lane = Lane::new(LaneCfg::fixed(32));
        run(&mut lane, |w| w.min(4.0) / 1e-3, 1.0);
        assert_eq!(lane.width(), 32);
        assert!(!lane.holds(u64::MAX / 2));
    }
}
