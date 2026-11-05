//! Bounded blocking work for calls that walk a whole scan: renders, filters, summaries, searches
//! (Python `server/compute.py`).
//!
//! Each such call allocates temporaries proportional to the scan; a burst of requests running them
//! all at once makes the machine page. [`compute`] runs at most [`workers`] of them at a time on
//! tokio's blocking pool; a call still waiting for its turn is dropped when the request is
//! cancelled. [`Latest`] drops waiting calls that a newer request with the same key made pointless
//! (the side panel asking for the summary of every frame while a scan runs).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::LazyLock;

use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::rpc::{RpcError, RpcResult};

/// Calls running at once.
pub fn workers() -> usize {
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    (cpus / 2).clamp(2, 4)
}

static PERMITS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(workers()));

/// Run `f` on a blocking thread once one of the [`workers`] slots is free.
pub async fn compute<T, F>(f: F) -> RpcResult<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let _permit = PERMITS
        .acquire()
        .await
        .map_err(|_| RpcError::internal("compute pool closed"))?;
    tokio::task::spawn_blocking(f).await.map_err(|e| {
        if e.is_panic() {
            tracing::error!("a computation panicked: {e}");
        }
        RpcError::internal("internal error")
    })
}

/// Run `f` on a blocking thread right away (blocking I/O: volume probes, snapshot files).
pub async fn blocking<T, F>(f: F) -> RpcResult<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| RpcError::internal("internal error"))
}

/// Latest-wins slots: a call still waiting when a newer one with the same key arrives is dropped
/// (answered with REQUEST_CANCELLED "superseded").
pub struct Latest<K> {
    gens: Mutex<HashMap<K, u64>>,
}

impl<K: Hash + Eq + Clone + Send + 'static> Default for Latest<K> {
    fn default() -> Self {
        Latest {
            gens: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Hash + Eq + Clone + Send + Sync + 'static> Latest<K> {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run<T, F>(&self, key: K, f: F) -> RpcResult<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let generation = {
            let mut g = self.gens.lock();
            let e = g.entry(key.clone()).or_insert(0);
            *e += 1;
            *e
        };
        // forget the slot when done, also when the request is cancelled meanwhile
        struct Slot<'a, K: Hash + Eq> {
            gens: &'a Mutex<HashMap<K, u64>>,
            key: K,
            generation: u64,
        }
        impl<K: Hash + Eq> Drop for Slot<'_, K> {
            fn drop(&mut self) {
                let mut g = self.gens.lock();
                if g.get(&self.key) == Some(&self.generation) {
                    g.remove(&self.key);
                }
            }
        }
        let slot = Slot {
            gens: &self.gens,
            key,
            generation,
        };
        let permit = PERMITS.acquire().await;
        let stale = slot.gens.lock().get(&slot.key) != Some(&slot.generation);
        match (permit, stale) {
            (Err(_), _) => Err(RpcError::internal("compute pool closed")),
            (_, true) => Err(RpcError::superseded()),
            (Ok(_p), false) => tokio::task::spawn_blocking(f)
                .await
                .map_err(|_| RpcError::internal("internal error")),
        }
    }

    pub fn len(&self) -> usize {
        self.gens.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Free slots and the slot count (diagnostics).
pub fn stats() -> (usize, usize) {
    (PERMITS.available_permits(), workers())
}
