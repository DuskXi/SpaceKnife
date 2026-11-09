//! 0.3.5's width rules (v1, docs/native.md 2.2 「列目录线程」), kept as the simulator's baseline:
//! one width for every listing, decided over windows of at least 0.12 s and 32 folders (or 1 s)
//! from `u` (the process's CPU time per second and CPU), `s` (the share of the width's slots spent
//! listing), `io` (disk reads per folder listed) and the rate (folders plus entries / 64 per
//! second):
//!
//! - the disk is read (`io` >= 0.2) and the slots are busy (`s` > 0.5): straight to the cold width,
//!   max(2 × start, 32); beyond it, with CPU to spare (`u` < 0.6), the width doubles and goes back
//!   unless the next window lists 15 % faster (the next probe then waits, 0.25 s doubling to 4 s);
//! - above the start width, nothing read (`io` < 0.05), or the CPUs busy (`u` > 0.9) with few
//!   reads: back to the start width;
//! - at or below the start width, nothing read, the slots busy: a warm probe tries half the width
//!   (below the start width also twice it, in turn) for one window, then the width it came from for
//!   one more; the new width stays if it listed 15 % faster than the mean around it;
//! - no read count: never above the start width; a 2 s window with almost nothing listed starts
//!   over; no probe in the first half second.
//!
//! The code is pace.rs's of 0.3.5 with its clock taken out (times in seconds from any origin).

/// Shortest window a decision is made on, and the most folders it needs (fewer are enough once it
/// lasted MAX_WINDOW).
const MIN_WINDOW: f64 = 0.12;
const MIN_DIRS: u64 = 32;
const MAX_WINDOW: f64 = 1.0;
const IDLE: f64 = 2.0;
const GROW_BELOW_U: f64 = 0.6;
const BUSY_ABOVE_S: f64 = 0.5;
const SHRINK_ABOVE_U: f64 = 0.9;
const NARROW_BELOW_U: f64 = 0.8;
const IO_COLD: f64 = 0.2;
const IO_WARM: f64 = 0.05;
const PROBE_GAIN: f64 = 1.15;
const WARM_GAIN: f64 = 1.15;
const BACKOFF_MIN: f64 = 0.25;
const BACKOFF_MAX: f64 = 4.0;
const FIRST_PROBE: f64 = 0.5;
const ENTRIES_PER_DIR: f64 = 64.0;

/// The widths a width moves between: the start width, the narrowest a warm probe tries, the cold
/// width and the most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widths {
    pub start: usize,
    pub least: usize,
    pub cold: usize,
    pub most: usize,
}

/// The widths for a start width and a most.
pub fn widths(start: usize, most: usize) -> Widths {
    let most = most.max(start);
    Widths {
        start,
        least: (start / 4).max(2).min(start),
        cold: (2 * start).max(32).min(most),
        most,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Probe {
    Grow {
        from: usize,
        before: f64,
    },
    Warm {
        from: usize,
        to: usize,
        before: f64,
        trial: Option<f64>,
    },
}

struct Ctl {
    probe: Option<Probe>,
    hold_until: f64,
    backoff: f64,
    up_next: bool,
}

impl Ctl {
    fn failed(&mut self, now: f64) {
        self.backoff = (self.backoff * 2.0).min(BACKOFF_MAX);
        self.hold_until = now + self.backoff;
    }

    fn kept(&mut self, now: f64) {
        self.backoff = BACKOFF_MIN;
        self.hold_until = now + BACKOFF_MIN;
    }
}

/// What one window measured (`u` None: no CPU clock; `io` None: no read count).
struct Window {
    u: Option<f64>,
    s: f64,
    rate: f64,
    io: Option<f64>,
}

/// The rules of the module docs: the next width after window `w` at `width`, and what was done.
fn decide(c: &mut Ctl, w: &Window, width: usize, l: Widths, now: f64) -> (&'static str, usize) {
    let io = w.io.unwrap_or(0.0);
    let cold = io >= IO_COLD;
    match c.probe.take() {
        Some(Probe::Grow { from, before }) => {
            if w.rate < before * PROBE_GAIN || !cold {
                c.failed(now);
                return ("revert", from);
            }
            c.kept(now);
            return ("keep", width);
        }
        Some(Probe::Warm {
            from,
            to,
            before,
            trial,
        }) if !cold => {
            let Some(r) = trial else {
                c.probe = Some(Probe::Warm {
                    from,
                    to,
                    before,
                    trial: Some(w.rate),
                });
                return ("back", from);
            };
            c.up_next = to < from;
            if r >= WARM_GAIN * (before + w.rate) / 2.0 {
                c.kept(now);
                return (if to < from { "narrow" } else { "widen" }, to);
            }
            c.failed(now);
            return ("stay", from);
        }
        _ => {}
    }
    let busy_cpus = w.u.is_some_and(|u| u > SHRINK_ABOVE_U);
    if width > l.start && (io < IO_WARM || (busy_cpus && !cold)) {
        return ("warm", l.start);
    }
    if cold && w.s > BUSY_ABOVE_S {
        if width < l.cold {
            return ("cold", l.cold);
        }
        if w.u.is_some_and(|u| u < GROW_BELOW_U) && width < l.most && now >= c.hold_until {
            c.probe = Some(Probe::Grow {
                from: width,
                before: w.rate,
            });
            return ("grow", (width * 2).min(l.most));
        }
        return ("hold", width);
    }
    if io < IO_WARM && w.s > BUSY_ABOVE_S && width <= l.start && now >= c.hold_until {
        let may_narrow = width > l.least && w.u.is_none_or(|u| u < NARROW_BELOW_U);
        let down = may_narrow && !(c.up_next && width < l.start);
        let to = if down {
            (width / 2).max(l.least)
        } else {
            (width * 2).min(l.start)
        };
        if to != width {
            c.probe = Some(Probe::Warm {
                from: width,
                to,
                before: w.rate,
                trial: None,
            });
            return ("try", to);
        }
    }
    ("hold", width)
}

/// v1 as the simulator drives it: listings are reported as they finish, and after each one the
/// window behind is decided on once long enough (0.3.5 did that after every call).
pub struct V1 {
    pub widths: Widths,
    n_cpu: usize,
    width: usize,
    start: f64,
    busy: f64,
    dirs: u64,
    entries: u64,
    cpu: Option<f64>,
    reads: Option<u64>,
    ctl: Ctl,
    pub decisions: Vec<(f64, &'static str, usize, usize)>,
}

impl V1 {
    /// Starts at `now` with these widths; `cpu` and `reads` are the process's CPU seconds and the
    /// read count then (None: none to be had).
    pub fn new(widths: Widths, n_cpu: usize, now: f64, cpu: Option<f64>, reads: Option<u64>) -> V1 {
        V1 {
            widths,
            n_cpu,
            width: widths.start,
            start: now,
            busy: 0.0,
            dirs: 0,
            entries: 0,
            cpu,
            reads,
            ctl: Ctl {
                probe: None,
                hold_until: now + FIRST_PROBE,
                backoff: BACKOFF_MIN,
                up_next: false,
            },
            decisions: Vec::new(),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// One folder of `entries` entries was listed in `took` seconds.
    pub fn listed(&mut self, took: f64, entries: usize) {
        self.busy += took;
        self.dirs += 1;
        self.entries += entries as u64;
    }

    /// After a listing at `now` (the process's CPU seconds and the read count then): the width.
    pub fn after(&mut self, now: f64, cpu: Option<f64>, reads: Option<u64>) -> usize {
        let span = now - self.start;
        if span < MIN_WINDOW || (self.dirs < MIN_DIRS && span < MAX_WINDOW) {
            return self.width;
        }
        let (busy, dirs, entries) = (self.busy, self.dirs, self.entries);
        self.busy = 0.0;
        self.dirs = 0;
        self.entries = 0;
        let (prev_cpu, prev_reads) = (self.cpu, self.reads);
        self.cpu = cpu;
        self.reads = reads;
        self.start = now;
        let width = self.width;
        if span > IDLE && dirs < MIN_DIRS {
            self.ctl.probe = None;
            self.ctl.backoff = BACKOFF_MIN;
            self.ctl.hold_until = now + FIRST_PROBE;
            self.ctl.up_next = false;
            self.width = self.widths.start;
            return self.width;
        }
        let secs = span.max(1e-6);
        let w = Window {
            u: match (prev_cpu, cpu) {
                (Some(a), Some(b)) => Some((b - a).max(0.0) / secs / self.n_cpu as f64),
                _ => None,
            },
            s: (busy / secs / width as f64).min(1.0),
            rate: (dirs as f64 + entries as f64 / ENTRIES_PER_DIR) / secs,
            io: match (prev_reads, reads) {
                (Some(a), Some(b)) => Some(b.saturating_sub(a) as f64 / dirs.max(1) as f64),
                _ => None,
            },
        };
        let (what, next) = decide(&mut self.ctl, &w, width, self.widths, now);
        self.decisions.push((now, what, width, next));
        self.width = next;
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Widths = Widths {
        start: 16,
        least: 4,
        cold: 32,
        most: 64,
    };

    fn ctl(now: f64) -> Ctl {
        Ctl {
            probe: None,
            hold_until: now,
            backoff: BACKOFF_MIN,
            up_next: false,
        }
    }

    fn window(u: f64, s: f64, rate: f64, io: Option<f64>) -> Window {
        Window {
            u: Some(u),
            s,
            rate,
            io,
        }
    }

    fn cold(u: f64, s: f64, rate: f64) -> Window {
        window(u, s, rate, Some(0.5))
    }

    fn warm(u: f64, s: f64, rate: f64) -> Window {
        window(u, s, rate, Some(0.0))
    }

    fn some(u: f64, s: f64, rate: f64) -> Window {
        window(u, s, rate, Some(0.1))
    }

    // 0.3.5's tests of these rules (pace.rs), with its clock taken out

    #[test]
    fn widths_follow_the_start_width() {
        assert_eq!(widths(16, 64), L);
        assert_eq!(
            widths(4, 64),
            Widths {
                start: 4,
                least: 2,
                cold: 32,
                most: 64
            }
        );
        assert_eq!(
            widths(16, 8),
            Widths {
                start: 16,
                least: 4,
                cold: 16,
                most: 16
            }
        );
    }

    #[test]
    fn cold_reads_take_it_to_the_cold_width_whatever_the_cpu() {
        let t = 0.0;
        assert_eq!(
            decide(&mut ctl(t), &cold(0.3, 0.9, 1e4), 16, L, t),
            ("cold", 32)
        );
        assert_eq!(
            decide(&mut ctl(t), &cold(0.95, 0.9, 1e4), 16, L, t),
            ("cold", 32)
        );
        assert_eq!(
            decide(&mut ctl(t), &cold(0.3, 0.9, 1e4), 4, L, t),
            ("cold", 32)
        );
        assert_eq!(
            decide(&mut ctl(t), &cold(0.2, 0.3, 1e4), 16, L, t),
            ("hold", 16)
        );
        assert_eq!(
            decide(&mut ctl(t), &some(0.2, 0.95, 1e4), 16, L, t),
            ("hold", 16)
        );
    }

    #[test]
    fn without_a_read_count_it_never_widens() {
        let t = 0.0;
        let none = window(0.2, 0.95, 1e4, None);
        for width in [4, 8, 16] {
            let (_, next) = decide(&mut ctl(t), &none, width, L, t);
            assert!(next <= L.start, "{width} -> {next}");
        }
    }

    #[test]
    fn beyond_the_cold_width_a_probe_stays_only_if_it_lists_faster() {
        let t = 0.0;
        let mut c = ctl(t);
        assert_eq!(
            decide(&mut c, &cold(0.8, 0.9, 1000.0), 32, L, t),
            ("hold", 32)
        );
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, t),
            ("grow", 64)
        );
        assert_eq!(
            decide(&mut c, &cold(0.5, 0.9, 1100.0), 64, L, t),
            ("revert", 32)
        );
        assert_eq!(c.backoff, BACKOFF_MIN * 2.0);
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, t),
            ("hold", 32)
        );
        let later = t + BACKOFF_MIN * 2.0;
        assert_eq!(
            decide(&mut c, &cold(0.3, 0.9, 1000.0), 32, L, later),
            ("grow", 64)
        );
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.9, 5000.0), 64, L, later),
            ("revert", 32)
        );
    }

    #[test]
    fn warm_and_contended_a_narrower_width_stays_if_it_lists_faster() {
        let t = 0.0;
        let mut c = ctl(t);
        assert_eq!(
            decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t),
            ("try", 8)
        );
        assert_eq!(
            decide(&mut c, &warm(0.3, 0.95, 1600.0), 8, L, t),
            ("back", 16)
        );
        assert_eq!(
            decide(&mut c, &warm(0.4, 0.95, 1000.0), 16, L, t),
            ("narrow", 8)
        );
        assert_eq!(
            decide(&mut ctl(t), &warm(0.9, 0.95, 1000.0), 16, L, t),
            ("hold", 16)
        );
    }

    #[test]
    fn windows_are_decided_once_long_enough() {
        // cold and busy (a read per folder, 16 ms each on 16 slots): the cold width once 0.12 s
        // went by, not before
        let mut v = V1::new(L, 32, 0.0, Some(0.0), Some(0));
        for i in 1..=200u64 {
            let t = i as f64 * 0.001;
            v.listed(0.016, 10);
            let w = v.after(t, Some(t * 10.0), Some(i));
            assert_eq!(w, if t < 0.1199 { 16 } else { 32 }, "{t}");
        }
    }
}
