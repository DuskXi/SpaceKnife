//! Whether a listing waited for the disk (docs/native.md 2.2 「列目录线程」, 冷的比例): an online
//! two-cluster mixture on each listing's wait, the global cold fraction it gives, and the read
//! counters' veto. Clock-free: the controller (ctl.rs) feeds it, the simulator too.
//!
//! The feature of a listing is `x = ln(1 + wait in µs)`, the wait being its wall time less the CPU
//! time its thread used meanwhile: a warm listing hardly waits (preemption and locks aside), a cold
//! one waits for at least one read (NVMe about 80 µs, SATA 150 µs, a hard disk milliseconds, a
//! network share a round trip). Each cluster is a normal distribution on `x` with decayed
//! sufficient statistics (one online EM step per listing) and a prior centre (warm 2 µs, cold from
//! the medium) worth a few samples, so neither cluster vanishes when every listing is of the other
//! kind. A folder's posterior starts from its own prior (its parent's posterior, walk.rs), not
//! from the global mixing weight: a subtree that is warm stays warm in a cold scan.

use super::super::Medium;

pub const WARM: usize = 0;
pub const COLD: usize = 1;

/// Decay of the clusters' statistics per sample (a memory of about 512 listings).
const DECAY: f64 = 1.0 / 512.0;
/// The global cold fraction: fast and slow memories (the trend is their difference).
const FAST: f64 = 1.0 / 64.0;
const SLOW: f64 = 1.0 / 512.0;
/// What a prior centre weighs, in samples.
const PRIOR_SAMPLES: f64 = 16.0;
/// Responsibilities this close to 0 or 1 count as 0 or 1 in the clusters' statistics: thousands
/// of listings of one kind would otherwise drag the other cluster's centre into their tail.
const TRIM: f64 = 0.05;
/// The cold centre stays at least this far above the warm one (e^1: 2.7 times the wait).
const MIN_GAP: f64 = 1.0;
const SD_MIN: f64 = 0.35;
const SD_MAX: f64 = 2.5;
/// A cold listing reads the disk at least this often on average (C: 1.0, K: 0.64, F: 0.88 reads
/// per folder on a cold cache): the read counters bound the cold fraction by reads / this.
const READS_PER_COLD: f64 = 0.25;
/// The veto's bounds, and how fast it moves per look at the counters.
const KAPPA_MIN: f64 = 1e-8;
/// Below this bound on the cold fraction the disk is quiet: every listing is warm, whatever it
/// waited (it waited for something else: locks, the CPU, the kernel).
const QUIET: f64 = 0.02;
const KAPPA_RELAX: f64 = 2.0;
/// Priors are kept off 0 and 1, so that evidence always moves them.
const P_MIN: f32 = 0.02;
const POST_MIN: f32 = 0.01;
/// A subfolder's prior: its parent's posterior, and this much of the global cold fraction, kept
/// within PRIOR_MIN – 1 − PRIOR_MIN (a subtree's state is a lean, its own listing decides).
const GLOBAL_SHARE: f32 = 0.2;
const PRIOR_MIN: f32 = 0.1;

/// `x` of a listing that waited `wait_ns`.
pub fn feature(wait_ns: u64) -> f32 {
    (1.0 + wait_ns as f64 / 1000.0).ln() as f32
}

/// The cold centre's prior for a medium: the wait of one cold read, µs.
pub fn cold_wait_us(m: Medium) -> f64 {
    match m {
        Medium::Nvme => 80.0,
        Medium::Ssd | Medium::Virtual => 150.0,
        Medium::Hdd => 8000.0,
        Medium::Flash => 500.0,
        Medium::Network => 1500.0,
        Medium::Unknown => 200.0,
    }
}

const WARM_WAIT_US: f64 = 2.0;
const PRIOR_SD: f64 = 0.8;

/// What a carrier needs to judge a listing without the controller's lock (published by it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub mu: [f32; 2],
    pub sd: [f32; 2],
    /// The read counters' veto: the cold odds times this (KAPPA_MIN–1).
    pub kappa: f32,
    /// The global cold fraction (with the veto): the prior of a folder without one.
    pub cold: f32,
    /// The read counters say the disk is not read (QUIET): every listing is warm.
    pub quiet: bool,
}

fn sigmoid(l: f32) -> f32 {
    1.0 / (1.0 + (-l).exp())
}

impl Params {
    /// Before any sample: the priors of `medium`, the cold fraction unknown (0.5).
    pub fn prior(medium: Medium) -> Self {
        Params {
            mu: [
                (1.0 + WARM_WAIT_US).ln() as f32,
                (1.0 + cold_wait_us(medium)).ln() as f32,
            ],
            sd: [PRIOR_SD as f32; 2],
            kappa: 1.0,
            cold: 0.5,
            quiet: false,
        }
    }

    /// Log likelihood ratio, cold to warm, of a listing of feature `x`.
    pub fn llr(&self, x: f32) -> f32 {
        let z = |k: usize| (x - self.mu[k]) / self.sd[k];
        let (zw, zc) = (z(WARM), z(COLD));
        0.5 * (zw * zw - zc * zc) + (self.sd[WARM] / self.sd[COLD]).ln()
    }

    /// The posterior that a folder of prior `prior` whose listing has feature `x` waited for the
    /// disk: (with the read counters' veto, without it).
    pub fn posterior(&self, prior: f32, x: f32) -> (f32, f32) {
        let p = prior.clamp(P_MIN, 1.0 - P_MIN);
        if self.quiet {
            return (
                POST_MIN,
                POST_MIN.max(sigmoid(
                    (p / (1.0 - p)).ln() + self.llr(x).clamp(-20.0, 20.0),
                )),
            );
        }
        let l = (p / (1.0 - p)).ln() + self.llr(x).clamp(-20.0, 20.0);
        let raw = sigmoid(l);
        let vetoed = sigmoid(l + self.kappa.max(KAPPA_MIN as f32).ln());
        // the veto only ever lowers
        (
            vetoed.clamp(POST_MIN, 1.0 - POST_MIN),
            raw.clamp(POST_MIN, 1.0 - POST_MIN),
        )
    }

    /// The prior of a subfolder of a folder whose posterior is `post`.
    pub fn child_prior(&self, post: f32) -> f32 {
        ((1.0 - GLOBAL_SHARE) * post + GLOBAL_SHARE * self.cold).clamp(PRIOR_MIN, 1.0 - PRIOR_MIN)
    }

    /// A folder's prior when it has none of its own (a scan root, a task without one).
    pub fn unknown_prior(&self) -> f32 {
        self.cold
    }
}

pub struct Model {
    medium: Medium,
    prior: Params,
    /// Decayed sums per cluster: weight, Σx, Σx².
    n: [f64; 2],
    s1: [f64; 2],
    s2: [f64; 2],
    params: Params,
    /// Cold fraction from the latency alone (fast), with the veto (fast and slow).
    raw_fast: f64,
    fast: f64,
    slow: f64,
    /// Disk reads per folder listed, smoothed (None: no count to go by).
    reads_per_dir: Option<f64>,
    kappa: f64,
    /// The read counters may veto (not on network shares: no count there means nothing).
    veto: bool,
    pub samples: u64,
    /// Samples the veto pushed from cold to warm.
    pub vetoed: u64,
}

impl Model {
    pub fn new(medium: Medium) -> Self {
        let prior = Params::prior(medium);
        Model {
            medium,
            prior,
            n: [0.0; 2],
            s1: [0.0; 2],
            s2: [0.0; 2],
            params: prior,
            raw_fast: 0.5,
            fast: 0.5,
            slow: 0.5,
            reads_per_dir: None,
            kappa: 1.0,
            veto: medium != Medium::Network,
            samples: 0,
            vetoed: 0,
        }
    }

    pub fn medium(&self) -> Medium {
        self.medium
    }

    pub fn params(&self) -> Params {
        self.params
    }

    /// The global cold fraction (with the veto) and its trend (fast less slow memory).
    pub fn cold(&self) -> f64 {
        self.fast
    }

    pub fn trend(&self) -> f64 {
        self.fast - self.slow
    }

    /// The cold fraction the latencies alone give.
    pub fn raw_cold(&self) -> f64 {
        self.raw_fast
    }

    pub fn reads_per_dir(&self) -> Option<f64> {
        self.reads_per_dir
    }

    /// A new scan: the fractions start over, the clusters (properties of the device and the CPU,
    /// not of this scan's cache) stay.
    pub fn restart(&mut self) {
        self.raw_fast = 0.5;
        self.fast = 0.5;
        self.slow = 0.5;
        self.reads_per_dir = None;
        self.kappa = 1.0;
        self.params.kappa = 1.0;
        self.params.quiet = false;
        self.params.cold = 0.5;
    }

    /// One listing of a folder of prior `prior` had feature `x`: one EM step. Returns its
    /// posterior (with the veto).
    pub fn learn(&mut self, x: f32, prior: f32) -> f32 {
        let (post, raw) = self.params.posterior(prior, x);
        let vetoed = raw >= 0.5 && post < 0.5;
        if vetoed {
            self.vetoed += 1;
        }
        // a listing the counters vetoed waited for something else than the disk (locks, the
        // CPU): it says nothing about either cluster (taught as warm, it would widen the warm one
        // until it swallowed the cold reads once the disk is read again)
        if !vetoed {
            let x = x as f64;
            let c = match post as f64 {
                p if p < TRIM => 0.0,
                p if p > 1.0 - TRIM => 1.0,
                p => p,
            };
            let r = [1.0 - c, c];
            for k in [WARM, COLD] {
                self.n[k] += DECAY * (r[k] - self.n[k]);
                self.s1[k] += DECAY * (r[k] * x - self.s1[k]);
                self.s2[k] += DECAY * (r[k] * x * x - self.s2[k]);
            }
        }
        self.raw_fast += FAST * (raw as f64 - self.raw_fast);
        self.fast += FAST * (post as f64 - self.fast);
        self.slow += SLOW * (post as f64 - self.slow);
        self.samples += 1;
        self.refit();
        post
    }

    /// The read counters over a stretch: `reads` disk reads while `dirs` folders were listed.
    /// They bound the cold fraction by reads / READS_PER_COLD; while the posteriors say more, the
    /// veto tightens (multiplicatively, towards the bound), else it relaxes as fast (the square
    /// of how far the bound is above, at least ×2): a cold region after a warm one is read at
    /// once.
    pub fn reads(&mut self, reads: u64, dirs: u64) {
        if dirs == 0 {
            return;
        }
        let r = reads as f64 / dirs as f64;
        let r = match self.reads_per_dir {
            None => r,
            Some(old) => old + 0.3 * (r - old),
        };
        self.reads_per_dir = Some(r);
        if self.veto {
            let bound = r / READS_PER_COLD;
            self.kappa = if bound < self.fast {
                self.kappa * (bound.max(1e-4) / self.fast).max(1e-3)
            } else {
                let up = (bound / self.fast.max(1e-3)).powi(2);
                self.kappa * up.max(KAPPA_RELAX)
            }
            .clamp(KAPPA_MIN, 1.0);
        }
        self.refit();
    }

    fn refit(&mut self) {
        let w = PRIOR_SAMPLES * DECAY;
        let mut mu = [0.0f64; 2];
        let mut sd = [0.0f64; 2];
        for k in [WARM, COLD] {
            let (m0, s0) = (self.prior.mu[k] as f64, self.prior.sd[k] as f64);
            let n = self.n[k] + w;
            mu[k] = (self.s1[k] + w * m0) / n;
            let var = (self.s2[k] + w * (s0 * s0 + m0 * m0)) / n - mu[k] * mu[k];
            sd[k] = var.max(0.0).sqrt().clamp(SD_MIN, SD_MAX);
        }
        mu[COLD] = mu[COLD].max(mu[WARM] + MIN_GAP);
        self.params = Params {
            mu: [mu[0] as f32, mu[1] as f32],
            sd: [sd[0] as f32, sd[1] as f32],
            kappa: self.kappa as f32,
            cold: self.fast as f32,
            quiet: self.veto
                && self
                    .reads_per_dir
                    .is_some_and(|r| r / READS_PER_COLD < QUIET),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits in µs → features.
    fn x(us: f64) -> f32 {
        feature((us * 1000.0) as u64)
    }

    #[test]
    fn a_listing_that_waited_for_a_read_is_cold() {
        let p = Params::prior(Medium::Nvme);
        let (cold, _) = p.posterior(0.5, x(90.0));
        let (warm, _) = p.posterior(0.5, x(1.0));
        assert!(cold > 0.95 && warm < 0.05, "{cold} {warm}");
        // the prior counts: a folder of a warm subtree needs more evidence
        let (doubt, _) = p.posterior(0.05, x(25.0));
        let (sure, _) = p.posterior(0.9, x(25.0));
        assert!(doubt < sure);
        // never 0 or 1
        assert!(p.posterior(0.0, x(0.0)).0 > 0.0 && p.posterior(1.0, x(1e6)).0 < 1.0);
    }

    #[test]
    fn the_clusters_follow_what_listings_wait() {
        // an SSD whose cold reads take 400 µs, not the 150 µs of the prior
        let mut m = Model::new(Medium::Ssd);
        let mut s = 7u64;
        let mut noise = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            0.7 + 0.6 * ((s >> 33) as f64 / (1u64 << 31) as f64)
        };
        for i in 0..4000 {
            let wait = if i % 3 == 0 { 400.0 } else { 1.5 } * noise();
            m.learn(x(wait), 0.5);
        }
        let p = m.params();
        assert!((p.mu[COLD] - x(400.0)).abs() < 0.3, "{p:?}");
        assert!((p.mu[WARM] - x(1.5)).abs() < 0.3, "{p:?}");
        assert!((m.cold() - 1.0 / 3.0).abs() < 0.1, "{}", m.cold());
        // all warm from now on: the fraction falls within a few hundred listings, the cold
        // cluster stays where it was
        for _ in 0..400 {
            m.learn(x(1.5 * noise()), m.params().unknown_prior());
        }
        assert!(m.cold() < 0.05, "{}", m.cold());
        assert!(m.params().mu[COLD] > x(150.0));
    }

    #[test]
    fn reads_bound_the_cold_fraction() {
        // ReFS warm under lock contention: listings wait 100 µs without reading anything
        let mut m = Model::new(Medium::Nvme);
        for _ in 0..200 {
            m.learn(x(100.0), m.params().unknown_prior());
        }
        assert!(m.cold() > 0.8);
        for _ in 0..20 {
            m.reads(0, 20);
            for _ in 0..20 {
                m.learn(x(100.0), m.params().unknown_prior());
            }
        }
        assert!(m.cold() < 0.05, "{}", m.cold());
        assert!(m.vetoed > 0);
        // reads come back: the veto goes
        for _ in 0..40 {
            m.reads(2000, 2000);
        }
        assert_eq!(m.params().kappa, 1.0);
        // a network share: never vetoed
        let mut n = Model::new(Medium::Network);
        n.reads(0, 1000);
        assert_eq!(n.params().kappa, 1.0);
    }

    #[test]
    fn subfolders_lean_on_their_parent() {
        let mut p = Params::prior(Medium::Nvme);
        p.cold = 0.9;
        assert!(p.child_prior(0.02) < 0.3);
        p.cold = 0.1;
        assert!(p.child_prior(0.98) > 0.7);
        // a lean, not a verdict: a clear read below a warm parent is cold
        p.cold = 0.01;
        let prior = p.child_prior(0.01);
        assert_eq!(prior, 0.1);
        assert!(p.posterior(prior, x(90.0)).0 > 0.9);
    }
}
