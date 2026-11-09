//! Backend-wide disk-load registry: samples every local disk from start-up, one governor per disk
//! (Python `io/registry.py`).
//!
//! Sampling runs at 1 Hz while no scan is using a disk (enough for the W2 pre-scan baseline,
//! UI-W2-40) and at 4 Hz while one is: the governor takes a control step every window, and its
//! baseline probes hold dispatch for one window, so short windows keep probes cheap. The W2
//! history stays at one point per second. Status pushes go out at 2 Hz to the listeners of each
//! governor ([`IoGovernor::subscribe`]).
//!
//! Two threads: the sampler's (it blocks: system calls, `ioreg`) and the pushes'.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use super::disks::{DiskId, DiskKind, DiskRef, disk_ref_for_path};
use super::governor::{Clock, IoGovernor, IoStatus, Level};
use super::sampler::{DiskSample, DiskSampler, default_sampler, monotonic};
use crate::media::Media;

pub type SamplerFactory = Box<dyn Fn() -> Box<dyn DiskSampler> + Send + Sync>;

/// Timing of the registry's loops.
#[derive(Clone, Copy, Debug)]
pub struct RegistryOptions {
    pub max_workers: usize,
    pub idle_interval: f64,
    pub active_interval: f64,
    pub push_interval: f64,
}

impl Default for RegistryOptions {
    fn default() -> Self {
        Self {
            max_workers: 8,
            idle_interval: 1.0,
            active_interval: 0.25,
            push_interval: 0.5,
        }
    }
}

struct Shared {
    opts: RegistryOptions,
    clock: Clock,
    govs: Mutex<Vec<Arc<IoGovernor>>>,
    seen: Mutex<HashSet<DiskId>>,
    unmatched: Mutex<HashSet<DiskId>>,
    sampler: Mutex<Option<Box<dyn DiskSampler>>>,
    sampler_info: Mutex<(&'static str, bool, f64)>, // name, supported, min_interval
    defaults: Mutex<(Level, Option<f64>, bool)>,
    stop: Mutex<bool>,
    wake: Condvar,
}

impl Shared {
    /// Sleep up to `secs`; false when stopped.
    fn sleep(&self, secs: f64) -> bool {
        let mut stop = self.stop.lock();
        if !*stop && secs > 0.0 {
            self.wake.wait_for(&mut stop, Duration::from_secs_f64(secs));
        }
        !*stop
    }

    fn find(&self, id: &str) -> Option<Arc<IoGovernor>> {
        self.govs.lock().iter().find(|g| g.disk().id == id).cloned()
    }

    fn ensure(&self, r: &DiskRef) -> Arc<IoGovernor> {
        let mut govs = self.govs.lock();
        if let Some(g) = govs.iter().find(|g| g.disk().id == r.id) {
            if g.disk().media == Media::Unknown && r.media != Media::Unknown {
                g.set_media(r.media);
            }
            return g.clone();
        }
        let gov = IoGovernor::with_clock(r.clone(), self.opts.max_workers, self.clock.clone());
        let (level, custom, yield_busy) = *self.defaults.lock();
        gov.set_level(level, custom);
        gov.set_yield_when_busy(yield_busy);
        govs.push(gov.clone());
        gov
    }

    fn interval(&self) -> f64 {
        let busy = self.govs.lock().iter().any(|g| g.active());
        let floor = self.sampler_info.lock().2;
        floor.max(if busy {
            self.opts.active_interval
        } else {
            self.opts.idle_interval
        })
    }

    fn sample_loop(&self) {
        let mut failures = 0u32;
        let mut next_at = monotonic();
        loop {
            next_at += self.interval();
            let mut delay = next_at - monotonic();
            if delay < 0.0 {
                next_at = monotonic();
                delay = 0.0;
            }
            if !self.sleep(delay) {
                return;
            }
            let samples = {
                let mut sampler = self.sampler.lock();
                let Some(s) = sampler.as_mut() else { return };
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.sample())) {
                    Ok(samples) => {
                        failures = 0;
                        samples
                    }
                    Err(_) => {
                        failures += 1;
                        if failures == 1 || failures == 30 {
                            tracing::warn!("disk load sampling failed");
                        }
                        HashMap::new()
                    }
                }
            };
            self.dispatch(&samples);
        }
    }

    fn dispatch(&self, samples: &HashMap<DiskId, DiskSample>) {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let (name, supported, _) = *self.sampler_info.lock();
        for (disk, sample) in samples {
            let gov = match self.find(disk) {
                Some(g) => g,
                None => {
                    let r = self.sampler.lock().as_ref().and_then(|s| s.disk_ref(disk));
                    self.ensure(&r.unwrap_or_else(|| DiskRef::physical(disk.clone())))
                }
            };
            gov.observe(Some(sample), Some(wall));
            self.seen.lock().insert(disk.clone());
        }
        let govs: Vec<Arc<IoGovernor>> = self.govs.lock().clone();
        for gov in govs {
            let d = gov.disk();
            if samples.contains_key(&d.id) {
                continue;
            }
            // a measured disk may miss one sample (counters primed again); one never measured
            // while the sampler measures others is a mapping mismatch: run open loop, say so once
            let seen = self.seen.lock().contains(&d.id);
            if supported && d.sampled() && seen {
                continue;
            }
            if !samples.is_empty()
                && supported
                && d.sampled()
                && self.unmatched.lock().insert(d.id.clone())
            {
                let mut measured: Vec<&String> = samples.keys().collect();
                measured.sort();
                let measured: Vec<&str> = measured.into_iter().map(String::as_str).collect();
                tracing::warn!(
                    "disk load: {name} measures {}, never {}: its load is not limited by measurement",
                    measured.join(", "),
                    d.id
                );
            }
            gov.observe(None, Some(wall)); // open loop: keep the governor ticking
        }
    }

    fn push_loop(&self) {
        while self.sleep(self.opts.push_interval) {
            let govs: Vec<Arc<IoGovernor>> = self.govs.lock().clone();
            for gov in govs {
                if gov.has_listeners() {
                    gov.push();
                }
            }
        }
    }
}

/// One governor per physical disk, fed by one sampler.
pub struct IoRegistry {
    shared: Arc<Shared>,
    factory: SamplerFactory,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl IoRegistry {
    pub fn new(opts: RegistryOptions) -> Self {
        Self::with_sampler(opts, Box::new(default_sampler), Arc::new(monotonic))
    }

    /// With a sampler factory and a clock (tests).
    pub fn with_sampler(opts: RegistryOptions, factory: SamplerFactory, clock: Clock) -> Self {
        Self {
            shared: Arc::new(Shared {
                opts,
                clock,
                govs: Mutex::new(Vec::new()),
                seen: Mutex::new(HashSet::new()),
                unmatched: Mutex::new(HashSet::new()),
                sampler: Mutex::new(None),
                sampler_info: Mutex::new(("none", false, 0.0)),
                defaults: Mutex::new((Level::Balanced, None, true)),
                stop: Mutex::new(false),
                wake: Condvar::new(),
            }),
            factory,
            threads: Mutex::new(Vec::new()),
        }
    }

    pub fn running(&self) -> bool {
        !self.threads.lock().is_empty()
    }

    /// The name of the sampler in use ("pdh", "ioctl", "sysfs", "ioreg", "none").
    pub fn sampler_name(&self) -> &'static str {
        self.shared.sampler_info.lock().0
    }

    /// Start sampling (at backend start: the pre-scan baseline). Blocks while the sampler is
    /// created.
    pub fn start(&self) {
        let mut threads = self.threads.lock();
        if !threads.is_empty() {
            return;
        }
        *self.shared.stop.lock() = false;
        let sampler = (self.factory)();
        *self.shared.sampler_info.lock() =
            (sampler.name(), sampler.supported(), sampler.min_interval());
        let refs = sampler.refs();
        *self.shared.sampler.lock() = Some(sampler);
        for r in &refs {
            self.shared.ensure(r);
        }
        tracing::debug!(
            "io registry: sampler {}, {} disks",
            self.sampler_name(),
            self.shared.govs.lock().len()
        );
        let s = self.shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("sk-io-sample".into())
                .spawn(move || s.sample_loop())
                .expect("spawn"),
        );
        let s = self.shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("sk-io-push".into())
                .spawn(move || s.push_loop())
                .expect("spawn"),
        );
    }

    pub fn stop(&self) {
        let threads: Vec<JoinHandle<()>> = std::mem::take(&mut *self.threads.lock());
        *self.shared.stop.lock() = true;
        self.shared.wake.notify_all();
        for t in threads {
            let _ = t.join();
        }
        if let Some(mut s) = self.shared.sampler.lock().take() {
            s.close();
        }
    }

    // ------------------------------------------------------------------ governors

    /// Settings applied to governors created from now on (scans set their own level on start).
    pub fn set_defaults(&self, level: Level, custom: Option<f64>, yield_when_busy: bool) {
        *self.shared.defaults.lock() = (level, custom, yield_when_busy);
    }

    /// The governor of a disk (created on first use).
    pub fn governor(&self, disk: &str) -> Arc<IoGovernor> {
        if let Some(g) = self.shared.find(disk) {
            return g;
        }
        let known = self
            .shared
            .sampler
            .lock()
            .as_ref()
            .and_then(|s| s.disk_ref(disk));
        let r = known.unwrap_or_else(|| {
            let kind = if disk.starts_with("net:") || disk.starts_with("vol:") {
                DiskKind::Volume
            } else {
                DiskKind::Physical
            };
            DiskRef::new(disk, kind)
        });
        self.shared.ensure(&r)
    }

    /// The governor of a disk described by `r` (created on first use).
    pub fn governor_for_ref(&self, r: &DiskRef) -> Arc<IoGovernor> {
        self.shared.ensure(r)
    }

    /// The governor of the physical disk holding `path` (shared by every scan on that disk).
    /// Blocks (it asks the system).
    pub fn governor_for_path(&self, path: &Path) -> Arc<IoGovernor> {
        let r = disk_ref_for_path(path);
        let gov = self.shared.ensure(&r);
        if r.sampled()
            && let Some(s) = self.shared.sampler.lock().as_mut()
        {
            s.watch(r);
        }
        gov
    }

    pub fn governors(&self) -> Vec<Arc<IoGovernor>> {
        self.shared.govs.lock().clone()
    }

    pub fn statuses(&self, history: bool) -> Vec<IoStatus> {
        self.governors().iter().map(|g| g.status(history)).collect()
    }
}

impl Drop for IoRegistry {
    fn drop(&mut self) {
        self.stop();
    }
}
