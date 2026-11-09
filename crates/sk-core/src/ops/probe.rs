//! Measuring file operations (benchmarks only, `probe` feature): how many file-system calls of
//! each kind an operation makes, how long its phases take, and a slow device in front of those
//! calls.
//!
//! Without the feature every function here is an empty inline function (nothing is counted,
//! nothing waits): a release build has none of it. With it, `SPACEKNIFE_OPS_DEVICE=<µs>[,<qd>]`
//! makes each counted call that reaches the device wait for one of `qd` slots (default 1) and
//! hold it `<µs>` microseconds: a USB stick that serves one command at a time and takes that long
//! for each. The calls the shell makes inside its own engine (Windows, recycling) are not ours to
//! count or slow down; they show in the physical disk's counters only.

/// A kind of call an operation makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Call {
    /// metadata by path (stat, lstat, a path resolved)
    Stat,
    /// a handle or descriptor opened
    Open,
    /// facts read from an open handle (identity, attributes, its name)
    Query,
    /// a folder listed (one per folder, however many reads it takes)
    List,
    /// an entry deleted
    Delete,
    /// an entry renamed or moved
    Rename,
    /// a file written (a trash's info file, a size cache)
    Write,
    /// a shell operation performed (Windows' recycle engine)
    Shell,
    /// items handed to the shell
    ShellItem,
    /// a Restart Manager session (who holds files open)
    Locks,
    /// names registered with the Restart Manager
    LockFile,
}

impl Call {
    pub const ALL: [Call; 11] = [
        Call::Stat,
        Call::Open,
        Call::Query,
        Call::List,
        Call::Delete,
        Call::Rename,
        Call::Write,
        Call::Shell,
        Call::ShellItem,
        Call::Locks,
        Call::LockFile,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Call::Stat => "stat",
            Call::Open => "open",
            Call::Query => "query",
            Call::List => "list",
            Call::Delete => "delete",
            Call::Rename => "rename",
            Call::Write => "write",
            Call::Shell => "shell",
            Call::ShellItem => "shell_item",
            Call::Locks => "locks",
            Call::LockFile => "lock_file",
        }
    }

    /// Whether the call reaches the device (the simulated one waits for it).
    #[cfg_attr(not(feature = "probe"), allow(dead_code))]
    fn on_device(self) -> bool {
        matches!(
            self,
            Call::Stat
                | Call::Open
                | Call::Query
                | Call::List
                | Call::Delete
                | Call::Rename
                | Call::Write
        )
    }
}

/// One call of `kind` made (counted; the simulated device waits).
#[inline(always)]
pub fn call(kind: Call) {
    #[cfg(feature = "probe")]
    imp::call(kind, 1);
    #[cfg(not(feature = "probe"))]
    let _ = kind;
}

/// `n` calls of `kind` counted at once (no wait: what the shell or the Restart Manager did).
#[inline(always)]
pub fn count(kind: Call, n: u64) {
    #[cfg(feature = "probe")]
    imp::count(kind, n);
    #[cfg(not(feature = "probe"))]
    let _ = (kind, n);
}

/// Time spent in a named phase, added up while the returned guard lives.
#[inline(always)]
pub fn phase(name: &'static str) -> Phase {
    #[cfg(feature = "probe")]
    {
        Phase(Some((name, std::time::Instant::now())))
    }
    #[cfg(not(feature = "probe"))]
    {
        let _ = name;
        Phase(())
    }
}

/// See [`phase`].
#[must_use]
pub struct Phase(
    #[cfg(feature = "probe")] Option<(&'static str, std::time::Instant)>,
    #[cfg(not(feature = "probe"))] (),
);

#[cfg(feature = "probe")]
impl Drop for Phase {
    fn drop(&mut self) {
        if let Some((name, t)) = self.0.take() {
            imp::add_phase(name, t.elapsed());
        }
    }
}

/// The counts and phases so far: `(call, count)` and `(phase, seconds)`.
#[cfg(feature = "probe")]
pub fn snapshot() -> (Vec<(Call, u64)>, Vec<(&'static str, f64)>) {
    imp::snapshot()
}

/// Start counting again from zero.
#[cfg(feature = "probe")]
pub fn reset() {
    imp::reset()
}

/// The simulated device: `(latency, queue depth)`, None when there is none.
#[cfg(feature = "probe")]
pub fn device() -> Option<(std::time::Duration, usize)> {
    imp::device().map(|d| (d.lat, d.qd))
}

#[cfg(feature = "probe")]
mod imp {
    use std::collections::BTreeMap;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use parking_lot::{Condvar, Mutex};

    use super::Call;

    static COUNTS: [AtomicU64; 11] = [const { AtomicU64::new(0) }; 11];
    static PHASES: Mutex<BTreeMap<&'static str, Duration>> = Mutex::new(BTreeMap::new());

    pub(super) struct Device {
        pub lat: Duration,
        pub qd: usize,
        busy: Mutex<usize>,
        free: Condvar,
    }

    pub(super) fn device() -> Option<&'static Device> {
        static DEVICE: OnceLock<Option<Device>> = OnceLock::new();
        DEVICE
            .get_or_init(|| {
                let spec = std::env::var("SPACEKNIFE_OPS_DEVICE").ok()?;
                let mut parts = spec.split(',').map(str::trim);
                let us: u64 = parts.next()?.parse().ok()?;
                let qd: usize = parts.next().and_then(|q| q.parse().ok()).unwrap_or(1);
                (us > 0).then(|| Device {
                    lat: Duration::from_micros(us),
                    qd: qd.max(1),
                    busy: Mutex::new(0),
                    free: Condvar::new(),
                })
            })
            .as_ref()
    }

    impl Device {
        fn serve(&self) {
            {
                let mut busy = self.busy.lock();
                while *busy >= self.qd {
                    self.free.wait(&mut busy);
                }
                *busy += 1;
            }
            // spun, not slept: a sleep that short overshoots by a timer tick on some systems
            let until = Instant::now() + self.lat;
            while Instant::now() < until {
                std::hint::spin_loop();
                if until.saturating_duration_since(Instant::now()) > Duration::from_millis(2) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            *self.busy.lock() -= 1;
            self.free.notify_one();
        }
    }

    fn index(kind: Call) -> usize {
        Call::ALL.iter().position(|&c| c == kind).unwrap_or(0)
    }

    pub(super) fn call(kind: Call, n: u64) {
        COUNTS[index(kind)].fetch_add(n, Ordering::Relaxed);
        if kind.on_device()
            && let Some(d) = device()
        {
            d.serve();
        }
    }

    pub(super) fn count(kind: Call, n: u64) {
        COUNTS[index(kind)].fetch_add(n, Ordering::Relaxed);
    }

    pub(super) fn add_phase(name: &'static str, d: Duration) {
        *PHASES.lock().entry(name).or_default() += d;
    }

    pub(super) fn snapshot() -> (Vec<(Call, u64)>, Vec<(&'static str, f64)>) {
        let counts = Call::ALL
            .iter()
            .map(|&c| (c, COUNTS[index(c)].load(Ordering::Relaxed)))
            .collect();
        let phases = PHASES
            .lock()
            .iter()
            .map(|(k, v)| (*k, v.as_secs_f64()))
            .collect();
        (counts, phases)
    }

    pub(super) fn reset() {
        for c in &COUNTS {
            c.store(0, Ordering::Relaxed);
        }
        PHASES.lock().clear();
    }
}
