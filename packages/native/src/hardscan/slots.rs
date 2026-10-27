//! Listings in flight under live limits (docs/native.md 2.2 「列目录线程」): the walker's width
//! (pace.rs) and a disk's gate (`ListingGate`, the backend's disk-load limit). A folder is listed
//! only while it holds a slot of each limit its call has; one that cannot get them waits in its
//! call's parked stack (walk.rs). Carriers never block on a limit: a gate letting two listings
//! through on a hard disk keeps no carrier from another disk's scan.
//!
//! A paused gate admits no folder that can wait (the walker hands it back as a task); folders that
//! must be listed in the call they were found in (below one opened by file id, or whose name
//! another entry shares: walk.rs `past_budget`) still are, within the limit.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use pyo3::prelude::*;

pub struct Slots {
    limit: AtomicUsize,
    inflight: AtomicUsize,
    paused: AtomicBool,
    /// Since the last `window`: the most listings in flight at once, the time spent listing, and
    /// whether a folder had to wait for this limit.
    peak: AtomicUsize,
    busy_ns: AtomicU64,
    refused: AtomicBool,
}

impl Slots {
    pub const fn new(limit: usize) -> Self {
        Self {
            limit: AtomicUsize::new(if limit == 0 { 1 } else { limit }),
            inflight: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
            peak: AtomicUsize::new(0),
            busy_ns: AtomicU64::new(0),
            refused: AtomicBool::new(false),
        }
    }

    /// One more listing, if the limit allows it (and the gate is not paused, unless `through_pause`).
    pub fn try_take(&self, through_pause: bool) -> bool {
        if !through_pause && self.paused.load(Ordering::Acquire) {
            self.refused.store(true, Ordering::Relaxed);
            return false;
        }
        let limit = self.limit.load(Ordering::Acquire);
        let mut now = self.inflight.load(Ordering::Relaxed);
        loop {
            if now >= limit {
                self.refused.store(true, Ordering::Relaxed);
                return false;
            }
            match self.inflight.compare_exchange_weak(
                now,
                now + 1,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.peak.fetch_max(now + 1, Ordering::Relaxed);
                    return true;
                }
                Err(seen) => now = seen,
            }
        }
    }

    /// A listing that took `took` is done: its slot goes back, and a waiting call is woken.
    pub fn give(&self, took: Duration) {
        self.busy_ns
            .fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        wake();
    }

    /// A listing that took `took` whose slot goes straight to the next folder: counted only.
    pub fn note(&self, took: Duration) {
        self.busy_ns.fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
    }

    /// A slot taken a moment ago and not used (its folder could not get the other limit's): back,
    /// without waking anyone (a caller may hold the parked stack's lock; waits time out anyway).
    pub fn untake(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::Acquire)
    }

    pub fn set_limit(&self, limit: usize) {
        if self.limit.swap(limit.max(1), Ordering::AcqRel) != limit.max(1) {
            wake();
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn set_paused(&self, on: bool) {
        if self.paused.swap(on, Ordering::AcqRel) != on {
            wake();
        }
    }

    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Relaxed)
    }

    /// (seconds listed, most in flight at once, whether a folder waited) since the last call; the
    /// next peak starts from the listings in flight now.
    pub fn window(&self) -> (f64, usize, bool) {
        let busy = self.busy_ns.swap(0, Ordering::Relaxed);
        let peak = self.peak.swap(self.inflight(), Ordering::Relaxed);
        let refused = self.refused.swap(false, Ordering::Relaxed);
        (busy as f64 / 1e9, peak, refused)
    }
}

/// Calls waiting for any slot: one condition variable for all limits (slots are given back all
/// the time, calls wait rarely: only when none of their folders can start).
static WAITERS: AtomicUsize = AtomicUsize::new(0);
static WAKE: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

fn wake() {
    if WAITERS.load(Ordering::SeqCst) > 0 {
        drop(WAKE.0.lock().unwrap_or_else(|e| e.into_inner()));
        WAKE.1.notify_all();
    }
}

/// Blocks the calling thread until `ready()` (called with the wait's lock held: it must not give a
/// slot back) or `timeout`. Lock order: this lock, then a call's parked stack.
pub fn wait(mut ready: impl FnMut() -> bool, timeout: Duration) {
    let guard = WAKE.0.lock().unwrap_or_else(|e| e.into_inner());
    WAITERS.fetch_add(1, Ordering::SeqCst);
    // a slot given back before this point was seen by `ready`; one after it finds a waiter and
    // notifies, once this thread is waiting (it holds the lock until then)
    if !ready() {
        drop(WAKE.1.wait_timeout(guard, timeout));
    }
    WAITERS.fetch_sub(1, Ordering::SeqCst);
}

/// A disk's limit on listings in flight, shared by every call that names it (the backend keeps one
/// per disk-load governor and sets it from the governor's concurrency and pause).
#[pyclass(frozen, name = "ListingGate", module = "spaceknife_native")]
pub struct ListingGate {
    pub slots: Arc<Slots>,
}

#[pymethods]
impl ListingGate {
    #[new]
    #[pyo3(signature = (limit = 1))]
    fn new(limit: usize) -> Self {
        Self {
            slots: Arc::new(Slots::new(limit)),
        }
    }

    /// Change the limit (at least 1) and/or the pause; calls waiting for a slot look again.
    #[pyo3(signature = (limit = None, paused = None))]
    fn set(&self, limit: Option<usize>, paused: Option<bool>) {
        if let Some(n) = limit {
            self.slots.set_limit(n);
        }
        if let Some(p) = paused {
            self.slots.set_paused(p);
        }
    }

    #[getter]
    fn limit(&self) -> usize {
        self.slots.limit()
    }

    #[getter]
    fn paused(&self) -> bool {
        self.slots.paused()
    }

    /// Listings in flight now.
    #[getter]
    fn inflight(&self) -> usize {
        self.slots.inflight()
    }

    /// (seconds spent listing, most listings in flight at once, whether a folder had to wait for
    /// the gate) since the last call: the governor's window.
    fn window(&self) -> (f64, usize, bool) {
        self.slots.window()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_limit_is_never_passed() {
        let s = Arc::new(Slots::new(3));
        let now = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (s, now, most) = (s.clone(), now.clone(), most.clone());
                std::thread::spawn(move || {
                    let mut done = 0;
                    while done < 200 {
                        if s.try_take(false) {
                            let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                            most.fetch_max(n, Ordering::SeqCst);
                            std::hint::spin_loop();
                            now.fetch_sub(1, Ordering::SeqCst);
                            s.give(Duration::from_micros(1));
                            done += 1;
                        } else {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        assert!(most.load(Ordering::SeqCst) <= 3);
        let (busy, peak, _refused) = s.window(); // refused: almost surely, not for certain
        assert!((1..=3).contains(&peak));
        assert!((busy - 1600e-6).abs() < 1e-9, "{busy}");
        assert_eq!(s.inflight(), 0);
    }

    #[test]
    fn pause_and_limit_changes() {
        let s = Slots::new(2);
        assert!(s.try_take(false) && s.try_take(false));
        assert!(!s.try_take(false) && !s.try_take(true)); // full: not even through the pause
        let (_, peak, refused) = s.window();
        assert_eq!((peak, refused), (2, true));
        s.set_limit(3);
        assert!(s.try_take(false));
        s.set_limit(1); // lowered below those in flight: nothing more until they are given back
        assert!(!s.try_take(true));
        for _ in 0..3 {
            s.give(Duration::ZERO);
        }
        s.set_paused(true);
        assert!(!s.try_take(false));
        assert!(s.try_take(true)); // a folder that must be listed in its call
        assert!(!s.try_take(true)); // still within the limit
        s.give(Duration::ZERO);
        s.set_paused(false);
        assert!(s.try_take(false));
        s.untake();
        assert_eq!(s.inflight(), 0);
        s.set_limit(0);
        assert_eq!(s.limit(), 1);
    }

    #[test]
    fn a_waiting_call_wakes_when_a_slot_comes_back() {
        let s = Arc::new(Slots::new(1));
        assert!(s.try_take(false));
        let giver = {
            let s = s.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                s.give(Duration::ZERO);
            })
        };
        let t = Instant::now();
        let mut got = false;
        while !got {
            wait(
                || {
                    got = s.try_take(false);
                    got
                },
                Duration::from_secs(5),
            );
        }
        assert!(t.elapsed() < Duration::from_secs(4), "woken, not timed out");
        giver.join().unwrap();
    }
}
