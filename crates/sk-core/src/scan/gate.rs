//! Disk-load throttling as the scanner sees it: a permit per listing batch (the batch is in
//! flight while the permit lives), the parallelism allowed, pause / resume of its owner.
//! [`crate::io::IoGovernor`] (behind an `Arc`) implements [`IoGate`]; tests use their own.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// A permit for one batch; dropping it tells the gate the batch finished.
pub struct Permit(Option<Box<dyn FnOnce() + Send>>);

impl Permit {
    /// A permit that calls `release` when dropped.
    pub fn new(release: impl FnOnce() + Send + 'static) -> Permit {
        Permit(Some(Box::new(release)))
    }

    /// A permit that does nothing (tests, ungated work).
    pub fn free() -> Permit {
        Permit(None)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Permit")
    }
}

/// What paces a scan so the disk load stays under a limit (io/governor.py's `IoGovernor`).
pub trait IoGate: Send + Sync {
    /// Waits up to `timeout` for the right to start another batch (`None`: not yet).
    fn permit(&self, timeout: Duration) -> Option<Permit>;
    /// Batches allowed in flight now.
    fn concurrency(&self) -> usize;
    /// The scan paused: nothing of its own in flight (a clean baseline for the governor).
    fn pause(&self) {}
    fn resume(&self) {}
    /// The disk's gate for the native walker's listings (docs/native.md 2.2 「列目录线程」): the
    /// walker lists the folders of one batch on several threads at once, and the gate holds its
    /// listings in flight to the concurrency and starts none while paused. None: batches only.
    fn listing_gate(&self) -> Option<sk_native::hardscan::ListingGate> {
        None
    }
}

/// The owner name the scanner pauses the governor under.
pub const GATE_OWNER: &str = "scan";

impl IoGate for Arc<crate::io::IoGovernor> {
    fn permit(&self, timeout: Duration) -> Option<Permit> {
        let got = match self.try_permit() {
            Some(p) => Some(p),
            None if timeout.is_zero() => None,
            None => {
                let end = Instant::now() + timeout;
                self.permit_until(&|| Instant::now() >= end)
            }
        };
        got.map(|p| Permit::new(move || drop(p)))
    }

    fn concurrency(&self) -> usize {
        crate::io::IoGovernor::concurrency(self)
    }

    fn pause(&self) {
        crate::io::IoGovernor::pause(self, GATE_OWNER);
    }

    fn resume(&self) {
        crate::io::IoGovernor::resume(self, GATE_OWNER);
    }

    fn listing_gate(&self) -> Option<sk_native::hardscan::ListingGate> {
        Some(crate::io::IoGovernor::listing_gate(self))
    }
}
