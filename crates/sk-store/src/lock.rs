//! The structure lock of a store: readers (renders, summaries, saves) share it, a change to
//! published rows ([`crate::Writer`]) takes it alone.
//!
//! Who waits for whom (store/lock.py): a writer waits for the readers already running when it
//! asked. While it does, new readers still start — the writer could not start before those older
//! readers finish anyway — so one long reader (a cleanup plan built over a large scan, say) holds
//! up the change, never every other reader with it. Once the older readers are done, new readers
//! wait for the writer; it waits at most for the readers that started until then, so a steady
//! stream of short readers cannot starve it. A writer that gives up waiting
//! ([`StructureLock::write_until`] timing out) lets the readers it held back go on.
//!
//! A reader taking the lock again on the same thread does not deadlock behind a waiting writer
//! (its first guard is older than the writer, so new readers are still admitted), but it would
//! once that writer runs between the two — do not nest.

use std::collections::BTreeSet;
use std::time::Instant;

use parking_lot::{Condvar, Mutex};

#[derive(Default)]
struct State {
    readers: usize,
    writing: bool,
    /// writers waiting to start
    waiting: usize,
    /// ticket of the next reader
    next: u64,
    /// tickets of the readers running now
    running: BTreeSet<u64>,
    /// readers with a smaller ticket were running when the waiting writers asked
    cut: u64,
}

impl State {
    /// Whether a new reader waits: while a change runs, and while a writer waits that no longer
    /// waits for any reader older than itself.
    fn held_back(&self) -> bool {
        if self.writing {
            return true;
        }
        if self.waiting == 0 {
            return false;
        }
        !self.running.first().is_some_and(|&t| t < self.cut)
    }
}

#[derive(Default)]
pub(crate) struct StructureLock {
    state: Mutex<State>,
    readers_cv: Condvar,
    writers_cv: Condvar,
}

pub(crate) struct ReadGuard<'a> {
    lock: &'a StructureLock,
    ticket: u64,
}

pub(crate) struct WriteGuard<'a> {
    lock: &'a StructureLock,
}

impl StructureLock {
    pub(crate) fn read(&self) -> ReadGuard<'_> {
        let mut st = self.state.lock();
        while st.held_back() {
            self.readers_cv.wait(&mut st);
        }
        let ticket = st.next;
        st.next += 1;
        st.running.insert(ticket);
        st.readers += 1;
        ReadGuard { lock: self, ticket }
    }

    pub(crate) fn write(&self) -> WriteGuard<'_> {
        self.write_until(None).expect("no deadline")
    }

    /// Waits until the lock is free or `deadline` passes; `None` when it gave up (the readers it
    /// held back go on).
    pub(crate) fn write_until(&self, deadline: Option<Instant>) -> Option<WriteGuard<'_>> {
        let mut st = self.state.lock();
        if st.waiting == 0 {
            st.cut = st.next; // the readers running now are the ones it waits for
        }
        st.waiting += 1;
        loop {
            if st.readers == 0 && !st.writing {
                st.writing = true;
                st.waiting -= 1;
                return Some(WriteGuard { lock: self });
            }
            match deadline {
                None => self.writers_cv.wait(&mut st),
                Some(d) => {
                    if self.writers_cv.wait_until(&mut st, d).timed_out()
                        && !(st.readers == 0 && !st.writing)
                    {
                        st.waiting -= 1;
                        if st.waiting == 0 {
                            // given up: the readers held back go on
                            self.readers_cv.notify_all();
                        }
                        return None;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    fn readers(&self) -> usize {
        self.state.lock().readers
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        let mut st = self.lock.state.lock();
        st.running.remove(&self.ticket);
        st.readers -= 1;
        if st.readers == 0 {
            self.lock.writers_cv.notify_all();
        }
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        let mut st = self.lock.state.lock();
        st.writing = false;
        if st.waiting > 0 {
            st.cut = st.next; // no reader runs now: the next writer waits for none
        }
        self.lock.readers_cv.notify_all();
        self.lock.writers_cv.notify_all(); // another writer may be waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn writer_gives_up_and_nobody_reads_while_writing() {
        let lock = StructureLock::default();
        let r = lock.read();
        assert!(
            lock.write_until(Some(Instant::now() + Duration::from_millis(20)))
                .is_none()
        );
        let _r2 = lock.read(); // nothing waits any more
        drop(r);
        drop(_r2);
        let stop = AtomicBool::new(false);
        let bad = AtomicBool::new(false);
        let writing = AtomicBool::new(false);
        std::thread::scope(|s| {
            for _ in 0..6 {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let _g = lock.read();
                        if writing.load(Ordering::SeqCst) {
                            bad.store(true, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_micros(300));
                    }
                });
            }
            for _ in 0..200 {
                let _w = lock.write();
                writing.store(true, Ordering::SeqCst);
                if lock.readers() != 0 {
                    bad.store(true, Ordering::SeqCst);
                }
                writing.store(false, Ordering::SeqCst);
            }
            stop.store(true, Ordering::Relaxed);
        });
        assert!(!bad.load(Ordering::SeqCst));
    }
}
