//! The streaming $MFT scan (docs/native.md 2.1).
//!
//! The $MFT is cut into windows. Up to `depth` windows are read at once (all their pieces in
//! flight on the [`Engine`]); a window is handed to the rayon pool as soon as its last piece is
//! in and parsed there, one task per window ([`parse_window`]), while the next windows are read.
//! Parsed windows are pushed into the [`Stream`] in record order and their rows appended to the
//! output columns at once, so the rows, their order included, are those of reading and parsing
//! one window after the other, whatever the window size or the order in which windows finish.
//! Memory: the output table (sized for one row per record up front) plus at most
//! `depth + spare` window buffers and the per-window tables, all reused.
//!
//! `duty` < 1 (the scan's I/O limit): the reads go in bursts of at most `depth` windows; after
//! each burst the reader pauses for `busy × (1 − duty) / duty`, `busy` being the time the burst
//! had reads in flight, so reads are in flight `duty` of the time.

use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::io::{Buf, Engine, Read};
use super::parse::{Part, Stream, Table, parse_window};
use super::runs::{Geometry, Layout, Piece, plan_window, round_up, zero_tail};

/// Window size and how many windows are read and held at once.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub window: usize,    // bytes of $MFT per window (rounded down to whole records)
    pub depth_min: usize, // windows read at once: where the search starts (see Depth)
    pub depth_max: usize, // and how far it may go
    pub spare: usize,     // more window buffers, for windows read but not parsed yet
    pub piece: usize,     // bytes per read request at most (rounded to whole clusters)
}

/// What a scan did, for benchmarks (`scan_mft(stats=...)`).
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub windows: usize,
    pub reads: usize,
    pub bounced: usize, // reads through a bounce buffer (a window not aligned to sectors)
    pub buffers: usize, // window buffers allocated
    pub max_in_flight: usize, // reads
    pub max_parsing: usize, // windows
    pub depth: usize,   // windows read at once, where the search settled (or ended)
    pub depth_trace: Vec<(usize, f64)>, // the search's epochs: (depth, bytes/s)
    pub read_s: f64,    // first read submitted to last read finished
    pub busy_s: f64,    // time with a read in flight
    pub paused_s: f64,  // paused for the duty
    pub starved_s: f64, // no read in flight because every buffer waited for parsing
    pub parse_cpu_s: f64, // parsing, summed over windows
    pub drain_s: f64,   // last read finished to last window parsed
    pub push_s: f64,    // appending parsed windows to the output columns (this thread)
    pub finish_s: f64,  // Stream::finish (bases with extension records, records read again)
    pub deferred: usize, // base records with an $ATTRIBUTE_LIST, kept for the end
    pub extensions: usize, // extension records
    pub fetched: usize, // records read again at the end
    pub redone: usize,  // bases parsed again with extension records found later
}

/// How many windows are read at once, found while the scan runs (one rule for every device):
/// start at `min`, measure the throughput over an epoch (at least `EPOCH` of busy time and
/// `EPOCH_WINDOWS` windows, counted once the reads submitted before the last change are in),
/// double the depth
/// while that raises the throughput by more than `GAIN`, and at the first doubling that does not,
/// go back to the depth before it and stay. A device that is saturated already (a SATA SSD, a
/// disk, a USB stick) keeps its few reads in flight, so other programs' reads do not queue behind
/// a deep queue of ours; one that is not (NVMe) gets as many as raise its throughput, at most `max`.
pub struct Depth {
    now: usize,
    max: usize,
    settled: bool,
    skip: usize,       // completions still to ignore (submitted at the previous depth)
    windows: usize,    // windows completed in this epoch
    bytes: u64,        // their bytes
    busy: Duration,    // busy time in this epoch
    prev: Option<f64>, // throughput at the previous depth
    pub trace: Vec<(usize, f64)>, // (depth, bytes/s) of every epoch measured
}

const EPOCH: Duration = Duration::from_millis(150);
const EPOCH_WINDOWS: usize = 16;
const GAIN: f64 = 1.10;

impl Depth {
    pub fn new(min: usize, max: usize) -> Depth {
        let min = min.max(1);
        Depth {
            now: min,
            max: max.max(min),
            settled: min >= max,
            skip: 0,
            windows: 0,
            bytes: 0,
            busy: Duration::ZERO,
            prev: None,
            trace: Vec::new(),
        }
    }

    pub fn now(&self) -> usize {
        self.now
    }

    /// A window's reads are all in (`bytes` long); `busy` is the time with reads in flight since
    /// the last call.
    pub fn completed(&mut self, bytes: u64, busy: Duration) {
        if self.settled {
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        self.windows += 1;
        self.bytes += bytes;
        self.busy += busy;
        if self.windows < EPOCH_WINDOWS || self.busy < EPOCH {
            return;
        }
        let rate = self.bytes as f64 / self.busy.as_secs_f64();
        self.trace.push((self.now, rate));
        match self.prev {
            Some(prev) if rate < prev * GAIN => {
                self.now = (self.now / 2).max(1); // the doubling did not pay: back, and stay
                self.settled = true;
            }
            _ if self.now * 2 > self.max => self.settled = true,
            _ => {
                self.prev = Some(rate);
                self.skip = self.now; // reads still in flight were submitted at this depth
                self.now *= 2;
            }
        }
        self.windows = 0;
        self.bytes = 0;
        self.busy = Duration::ZERO;
    }
}

struct Window {
    buf: Buf,
    start: u64,
    len: usize,
    pieces: Vec<Piece>,
    pending: usize,
}

struct Op {
    w: usize,
    bounce: Option<(Buf, usize)>, // the buffer and the piece it is for
}

type Parsed = (usize, Part, Buf, Duration);

/// What `scan` hands back: the columns and the name blob in pieces.
pub type Rows = super::parse::Joined;

/// Read and parse the whole valid part of the $MFT; `on_window(done, total)` (bytes of $MFT
/// parsed) at the start and after each window. The engine has nothing in flight on return.
#[allow(clippy::too_many_arguments)]
pub fn scan<E: Engine>(
    engine: E,
    geo: &Geometry,
    lay: &Layout,
    duty: f64,
    tune: &Tuning,
    fetch: impl Fn(u64) -> Option<Vec<u8>>,
    mut on_window: impl FnMut(u64, u64),
    st: &mut Stats,
) -> io::Result<Rows> {
    let rs = geo.record;
    let sector = geo.sector;
    let window = (tune.window / rs).max(1) * rs;
    let piece = round_up(tune.piece.max(geo.cluster), geo.cluster);
    let total = (lay.valid_size as usize).div_ceil(rs) * rs; // past it every record is unused
    let total = total.min(lay.data_size as usize);
    let nwin = total.div_ceil(window);
    let mut depth = Depth::new(tune.depth_min, tune.depth_max);
    let pool = tune.depth_max.max(1) + tune.spare;
    let cap = window + sector; // the last read of a window may run on to a whole sector

    // buffers first, the engine after them: on unwinding the engine is dropped first and waits
    // for the reads still writing into them
    let mut free: Vec<Buf> = Vec::new();
    let mut reading: HashMap<usize, Window> = HashMap::new();
    let mut ops: Vec<Option<Op>> = Vec::new();
    let mut engine = engine;

    let mut free_ops: Vec<usize> = Vec::new();
    let (tx, rx) = mpsc::channel::<Parsed>();
    let mut parts: Vec<Option<Part>> = (0..nwin).map(|_| None).collect();
    let mut stream = Stream::with_records(rs, (total / rs) as u64);
    let mut tables: Vec<Table> = Vec::new(); // emptied per-window tables, for the next windows
    let (mut next, mut pushed, mut parsing) = (0usize, 0usize, 0usize);
    let mut done = 0u64;
    let mut failure: Option<io::Error> = None;
    let mut port_lost = false;

    let t0 = Instant::now();
    let mut busy_since: Option<Instant> = None; // reads in flight since then
    let mut busy_closed = Duration::ZERO; // busy time before that
    let mut busy_seen = Duration::ZERO; // busy time when the last window's reads were all in
    let mut first_read: Option<Instant> = None;
    let mut last_read = t0;
    let mut burst_windows = 0usize; // windows submitted in the current burst (duty < 1)
    let mut burst_busy = Duration::ZERO;
    let mut paused_until: Option<Instant> = None;
    let paced = duty < 1.0;

    on_window(0, total as u64);
    let mut take_parsed = |(w, part, buf, cpu): Parsed,
                           free: &mut Vec<Buf>,
                           tables: &mut Vec<Table>,
                           parsing: &mut usize,
                           st: &mut Stats| {
        *parsing -= 1;
        free.push(buf);
        st.parse_cpu_s += cpu.as_secs_f64();
        let len = window.min(total - w * window);
        done += len as u64;
        parts[w] = Some(part);
        let t = Instant::now();
        while pushed < nwin
            && let Some(p) = parts[pushed].take()
        {
            tables.push(stream.push(p));
            pushed += 1;
        }
        st.push_s += t.elapsed().as_secs_f64();
        on_window(done, total as u64);
        pushed
    };
    let mut pushed_now = 0usize;

    loop {
        while let Ok(msg) = rx.try_recv() {
            pushed_now = take_parsed(msg, &mut free, &mut tables, &mut parsing, st);
        }
        if failure.is_some() {
            if engine.in_flight() == 0 {
                break;
            }
        } else if pushed_now == nwin {
            break;
        }

        // a burst is over once nothing is in flight: pause in proportion to its busy time
        if paced && burst_windows > 0 && engine.in_flight() == 0 && reading.is_empty() {
            let pause = burst_busy.mul_f64((1.0 - duty) / duty);
            paused_until = Some(Instant::now() + pause);
            st.paused_s += pause.as_secs_f64();
            burst_windows = 0;
            burst_busy = Duration::ZERO;
        }

        // submit windows
        let now = Instant::now();
        if paused_until.is_some_and(|t| now >= t) {
            paused_until = None;
        }
        while failure.is_none()
            && next < nwin
            && reading.len() < depth.now()
            && paused_until.is_none()
            && !(paced && burst_windows >= depth.now())
        {
            let buf = match free.pop() {
                Some(b) => b,
                None if st.buffers < pool => match Buf::new(cap) {
                    Ok(b) => {
                        st.buffers += 1;
                        b
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                },
                None => break,
            };
            let start = (next * window) as u64;
            let len = window.min(total - next * window);
            let pieces = plan_window(geo, lay, start, len, piece);
            let mut win = Window {
                buf,
                start,
                len,
                pieces,
                pending: 0,
            };
            let w = next;
            next += 1;
            st.windows += 1;
            if paced {
                burst_windows += 1;
            }
            let count = win.pieces.len();
            for (k, p) in win.pieces.iter().copied().enumerate() {
                let Some(at) = p.at else {
                    win.buf.slice_mut(p.dst + p.len)[p.dst..].fill(0); // sparse
                    continue;
                };
                let want = round_up(p.skip + p.len, sector);
                let aligned = p.skip == 0
                    && p.dst.is_multiple_of(sector)
                    && (p.len.is_multiple_of(sector) || k + 1 == count)
                    && p.dst + want <= win.buf.cap();
                let tag = free_ops.pop().unwrap_or_else(|| {
                    ops.push(None);
                    ops.len() - 1
                });
                let (dst, bounce) = if aligned {
                    // SAFETY: inside the window's buffer (checked above)
                    (unsafe { win.buf.as_mut_ptr().add(p.dst) }, None)
                } else {
                    match Buf::new(want) {
                        Ok(mut b) => (b.as_mut_ptr(), Some((b, k))),
                        Err(e) => {
                            free_ops.push(tag);
                            failure = Some(e);
                            break;
                        }
                    }
                };
                if bounce.is_some() {
                    st.bounced += 1;
                }
                ops[tag] = Some(Op { w, bounce });
                let read = Read {
                    at,
                    dst,
                    len: want,
                    tag,
                };
                let idle = engine.in_flight() == 0;
                if let Err(e) = engine.submit(read) {
                    ops[tag] = None;
                    free_ops.push(tag);
                    failure = Some(e);
                    break;
                }
                let now = Instant::now();
                if idle {
                    busy_since = Some(now);
                }
                first_read.get_or_insert(now);
                win.pending += 1;
                st.reads += 1;
                st.max_in_flight = st.max_in_flight.max(engine.in_flight());
            }
            if failure.is_some() {
                reading.insert(w, win); // its submitted reads still write into the buffer
                break;
            }
            if win.pending == 0 {
                dispatch(w, win, rs, lay, &tx, tables.pop().unwrap_or_default());
                parsing += 1;
                st.max_parsing = st.max_parsing.max(parsing);
            } else {
                reading.insert(w, win);
            }
        }

        // wait for a read, a parsed window or the end of a pause
        if engine.in_flight() > 0 {
            let Some((tag, res)) = engine.wait() else {
                continue;
            };
            let now = Instant::now();
            last_read = now;
            if engine.in_flight() == 0
                && let Some(since) = busy_since.take()
            {
                let busy = now - since;
                busy_closed += busy;
                burst_busy += busy;
            }
            if tag == usize::MAX {
                // the completion port failed: reads may still be writing into their buffers
                port_lost = true;
                failure.get_or_insert(res.err().unwrap_or_else(|| io::Error::other("I/O failed")));
                break;
            }
            let Some(op) = ops[tag].take() else {
                continue;
            };
            free_ops.push(tag);
            if let Err(e) = res {
                failure.get_or_insert(e);
            }
            let Some(win) = reading.get_mut(&op.w) else {
                continue;
            };
            if let Some((b, k)) = op.bounce
                && failure.is_none()
            {
                let p = win.pieces[k];
                win.buf.slice_mut(p.dst + p.len)[p.dst..]
                    .copy_from_slice(&b.slice(p.skip + p.len)[p.skip..]);
            }
            win.pending -= 1;
            if win.pending == 0
                && failure.is_none()
                && let Some(win) = reading.remove(&op.w)
            {
                let busy = busy_closed + busy_since.map_or(Duration::ZERO, |s| now - s);
                depth.completed(win.len as u64, busy - busy_seen);
                busy_seen = busy;
                dispatch(op.w, win, rs, lay, &tx, tables.pop().unwrap_or_default());
                parsing += 1;
                st.max_parsing = st.max_parsing.max(parsing);
            }
        } else if failure.is_some() {
            break;
        } else if let Some(until) = paused_until {
            let wait = until.saturating_duration_since(Instant::now());
            if parsing > 0 {
                if let Ok(msg) = rx.recv_timeout(wait) {
                    pushed_now = take_parsed(msg, &mut free, &mut tables, &mut parsing, st);
                }
            } else {
                std::thread::sleep(wait);
            }
        } else if parsing > 0 {
            let t = Instant::now();
            let Ok(msg) = rx.recv() else {
                failure = Some(io::Error::other("a window was lost while parsing"));
                break;
            };
            if next < nwin {
                st.starved_s += t.elapsed().as_secs_f64();
            }
            pushed_now = take_parsed(msg, &mut free, &mut tables, &mut parsing, st);
        } else if next >= nwin || free.is_empty() && st.buffers >= pool {
            // nothing in flight, parsing or paused, yet windows are missing: cannot happen
            failure = Some(io::Error::other("the $MFT scan stalled"));
            break;
        }
    }
    let end_reads = last_read;
    st.busy_s = busy_closed.as_secs_f64();
    st.depth = depth.now();
    st.depth_trace = std::mem::take(&mut depth.trace);
    if let Some(first) = first_read {
        st.read_s = (end_reads - first).as_secs_f64();
    }
    st.drain_s = end_reads.elapsed().as_secs_f64();
    if let Some(err) = failure {
        if port_lost {
            // nothing tells when the outstanding reads finish: never free their buffers
            for (_, w) in reading.drain() {
                std::mem::forget(w.buf);
            }
            for op in ops.drain(..).flatten() {
                if let Some((b, _)) = op.bounce {
                    std::mem::forget(b);
                }
            }
        }
        return Err(err);
    }
    debug_assert_eq!(stream.next_record() as usize * rs, total);
    drop(tables);
    let t = Instant::now();
    let (rows, [deferred, extensions, fetched, redone]) = stream
        .finish_counted(fetch)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    st.finish_s = t.elapsed().as_secs_f64();
    (st.deferred, st.extensions, st.fetched, st.redone) = (deferred, extensions, fetched, redone);
    Ok(rows)
}

/// A window whose pieces are all in: clear what no piece wrote, then parse it on the pool.
fn dispatch(
    w: usize,
    mut win: Window,
    rs: usize,
    lay: &Layout,
    tx: &mpsc::Sender<Parsed>,
    table: Table,
) {
    zero_tail(lay, win.start, win.buf.slice_mut(win.len), &win.pieces);
    let tx = tx.clone();
    let first = win.start / rs as u64;
    rayon::spawn(move || {
        let t = Instant::now();
        let part = parse_window(win.buf.slice(win.len), first, rs, table);
        let _ = tx.send((w, part, win.buf, t.elapsed())); // the scan may have failed and gone
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `Depth` the completions of a device whose throughput at `d` reads in flight is
    /// `rate(d)` bytes/s; return where it settles.
    fn settle(rate: impl Fn(usize) -> f64, min: usize, max: usize) -> usize {
        let mut depth = Depth::new(min, max);
        let window = 8u64 << 20;
        for _ in 0..100_000 {
            if depth.settled {
                break;
            }
            let busy = Duration::from_secs_f64(window as f64 / rate(depth.now()));
            depth.completed(window, busy);
        }
        assert!(depth.settled, "never settled");
        depth.now()
    }

    #[test]
    fn depth_stops_where_more_reads_stop_paying() {
        // saturated at once (a SATA SSD, a disk): stays at the start
        assert_eq!(settle(|_| 500e6, 2, 16), 2);
        // gains up to 4 in flight, then flat (an NVMe drive)
        assert_eq!(settle(|d| 1.6e9 * (d.min(4) as f64), 2, 16), 4);
        // gains a little past the knee (less than a tenth per doubling): not worth the queue
        assert_eq!(settle(|d| 6e9 * (1.0 - 0.5 / d as f64), 2, 16), 4);
        // always gains: up to the cap
        assert_eq!(settle(|d| 1e8 * d as f64, 2, 16), 16);
        // a fixed depth never moves
        assert_eq!(settle(|d| 1e8 * d as f64, 5, 5), 5);
    }
}
