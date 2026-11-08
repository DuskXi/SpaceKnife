//! Logging: `tracing` everywhere; plain lines or JSON lines on stderr, a size-rotated log file, and
//! an in-memory bus feeding the `log` topic (W8 log panel).
//!
//! Logs always go to **stderr**; stdout is reserved for the machine-readable `ready` line the
//! launcher waits for. Records of the `log` crate (dependencies) are routed into `tracing`, so there
//! is a single pipeline ([`init`]).
//!
//! * stderr: [`LogFormat::Plain`] (`HH:MM:SS.mmm | LEVEL    | target:line - message k=v`) or
//!   [`LogFormat::Json`] (one JSON object per line), at the level given to [`init`].
//! * the log file ([`set_file_logging`]): `logs/spaceknife.log` (see [`crate::paths::log_file`]),
//!   rotated at 10 MB, 5 old files kept (`spaceknife.log.1` … `.5`), its level changeable at any
//!   time (setting 「日志文件级别」).
//! * the bus ([`LogBus`]): every record at DEBUG or above, the last 5000 kept, delivered to
//!   subscribers with a minimum level.
//!
//! Levels are named as the protocol names them ([`LogLevel`]): `WARN` is `WARNING`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sk_protocol::streams::{LogLevel, LogRecordEvent};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

pub const FILE_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const FILE_KEEP: usize = 5;
pub const BUS_CAPACITY: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Plain,
    Json,
}

impl LogFormat {
    pub fn parse(s: &str) -> Option<LogFormat> {
        match s.to_ascii_lowercase().as_str() {
            "plain" | "rich" | "text" => Some(LogFormat::Plain),
            "json" => Some(LogFormat::Json),
            _ => None,
        }
    }
}

/// Numeric level as loguru numbered them (TRACE 5 … CRITICAL 50).
pub fn level_no(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 5,
        LogLevel::Debug => 10,
        LogLevel::Info => 20,
        LogLevel::Success => 25,
        LogLevel::Warning => 30,
        LogLevel::Error => 40,
        LogLevel::Critical => 50,
    }
}

pub fn from_tracing(level: &Level) -> LogLevel {
    match *level {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warning,
        Level::INFO => LogLevel::Info,
        Level::DEBUG => LogLevel::Debug,
        Level::TRACE => LogLevel::Trace,
    }
}

/// A level name (`INFO`, `DEBUG`, `WARNING`, `warn`, …) -> a tracing filter.
pub fn parse_level(name: &str) -> Option<LevelFilter> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "TRACE" => LevelFilter::TRACE,
        "DEBUG" => LevelFilter::DEBUG,
        "INFO" | "SUCCESS" => LevelFilter::INFO,
        "WARN" | "WARNING" => LevelFilter::WARN,
        "ERROR" | "CRITICAL" => LevelFilter::ERROR,
        "OFF" => LevelFilter::OFF,
        _ => return None,
    })
}

fn level_name(level: &Level) -> &'static str {
    from_tracing(level).as_str()
}

/// Chatty dependencies: only their warnings and errors.
fn noisy(meta: &Metadata<'_>) -> bool {
    const NOISY: &[&str] = &[
        "ureq",
        "rustls",
        "hyper",
        "h2",
        "tungstenite",
        "tokio_tungstenite",
        "mio",
        "want",
        "axum",
        "tower",
        "reqwest",
    ];
    *meta.level() > Level::WARN && {
        let target = meta.target();
        NOISY.iter().any(|n| {
            target
                .strip_prefix(n)
                .is_some_and(|r| r.is_empty() || r.starts_with("::"))
        })
    }
}

// --------------------------------------------------------------------------- the bus

/// One record on the bus.
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub level: LogLevel,
    /// The `source` field, else the record's target (module path)
    pub source: String,
    pub message: String,
    /// The record's other fields (`event`, `scan_id`, `path`, …) as text
    pub extra: BTreeMap<String, String>,
}

impl LogEntry {
    pub fn level_no(&self) -> u8 {
        level_no(self.level)
    }

    /// The `log.record` event for subscription `sub`.
    pub fn to_event(&self, sub: &str) -> LogRecordEvent {
        LogRecordEvent {
            sub: sub.to_owned(),
            seq: self.seq as i64,
            time: self.time,
            level: self.level,
            source: self.source.clone(),
            message: self.message.clone(),
            extra: self.extra.clone(),
        }
    }
}

pub type Listener = Arc<dyn Fn(&Arc<LogEntry>) + Send + Sync>;

struct BusInner {
    buffer: VecDeque<Arc<LogEntry>>,
    seq: u64,
    listeners: HashMap<u64, (u8, Listener)>,
    next_token: u64,
}

/// In-process log bus feeding the `log` topic: a bounded backlog plus live subscribers.
///
/// Records may arrive from any thread; listeners are called on the logging thread, outside the
/// bus's lock, and must not block or log themselves (hand the entry to a channel).
pub struct LogBus {
    capacity: usize,
    inner: Mutex<BusInner>,
}

static GLOBAL_BUS: LazyLock<Arc<LogBus>> = LazyLock::new(|| Arc::new(LogBus::new(BUS_CAPACITY)));

impl LogBus {
    pub fn new(capacity: usize) -> LogBus {
        LogBus {
            capacity: capacity.max(1),
            inner: Mutex::new(BusInner {
                buffer: VecDeque::with_capacity(capacity.clamp(1, 1024)),
                seq: 0,
                listeners: HashMap::new(),
                next_token: 0,
            }),
        }
    }

    /// The process-wide bus (the one [`init`] attaches when asked to).
    pub fn global() -> Arc<LogBus> {
        GLOBAL_BUS.clone()
    }

    /// Add a record (the tracing layer does this; usable directly too).
    pub fn push(
        &self,
        level: LogLevel,
        source: String,
        message: String,
        extra: BTreeMap<String, String>,
    ) -> Arc<LogEntry> {
        let no = level_no(level);
        let (entry, listeners) = {
            let mut g = self.inner.lock();
            g.seq += 1;
            let entry = Arc::new(LogEntry {
                seq: g.seq,
                time: Utc::now(),
                level,
                source,
                message,
                extra,
            });
            if g.buffer.len() >= self.capacity {
                g.buffer.pop_front();
            }
            g.buffer.push_back(entry.clone());
            let listeners: Vec<Listener> = g
                .listeners
                .values()
                .filter(|(min, _)| no >= *min)
                .map(|(_, l)| l.clone())
                .collect();
            (entry, listeners)
        };
        for l in listeners {
            l(&entry);
        }
        entry
    }

    /// The last `limit` records at `min_level` or above, oldest first (`limit` 0: none).
    pub fn recent(&self, limit: usize, min_level: LogLevel) -> Vec<Arc<LogEntry>> {
        if limit == 0 {
            return Vec::new();
        }
        let floor = level_no(min_level);
        let g = self.inner.lock();
        let mut out: Vec<_> = g
            .buffer
            .iter()
            .rev()
            .filter(|e| e.level_no() >= floor)
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    /// Call `listener` with every new record at `min_level` or above; returns a token for
    /// [`LogBus::unsubscribe`].
    pub fn subscribe(
        &self,
        min_level: LogLevel,
        listener: impl Fn(&Arc<LogEntry>) + Send + Sync + 'static,
    ) -> u64 {
        let mut g = self.inner.lock();
        g.next_token += 1;
        let token = g.next_token;
        g.listeners
            .insert(token, (level_no(min_level), Arc::new(listener)));
        token
    }

    /// The backlog (as [`LogBus::recent`]) and a subscription, atomically: no record is missed
    /// or delivered twice in between.
    pub fn subscribe_with_backlog(
        &self,
        backlog: usize,
        min_level: LogLevel,
        listener: impl Fn(&Arc<LogEntry>) + Send + Sync + 'static,
    ) -> (Vec<Arc<LogEntry>>, u64) {
        let floor = level_no(min_level);
        let mut g = self.inner.lock();
        let mut recent: Vec<_> = if backlog == 0 {
            Vec::new()
        } else {
            g.buffer
                .iter()
                .rev()
                .filter(|e| e.level_no() >= floor)
                .take(backlog)
                .cloned()
                .collect()
        };
        recent.reverse();
        g.next_token += 1;
        let token = g.next_token;
        g.listeners.insert(token, (floor, Arc::new(listener)));
        (recent, token)
    }

    pub fn unsubscribe(&self, token: u64) {
        self.inner.lock().listeners.remove(&token);
    }

    pub fn subscribers(&self) -> usize {
        self.inner.lock().listeners.len()
    }

    /// The tracing layer that feeds this bus (DEBUG and above).
    pub fn layer<S: Subscriber + for<'a> LookupSpan<'a>>(
        self: &Arc<Self>,
    ) -> impl Layer<S> + Send + Sync + 'static {
        BusLayer { bus: self.clone() }
            .with_filter(filter_fn(|m| *m.level() <= Level::DEBUG && !noisy(m)))
    }
}

struct BusLayer {
    bus: Arc<LogBus>,
}

#[derive(Default)]
struct FieldCollector {
    message: String,
    source: Option<String>,
    log_target: Option<String>,
    extra: BTreeMap<String, String>,
}

impl FieldCollector {
    fn put(&mut self, name: &str, value: String) {
        match name {
            "message" => self.message = value,
            "source" => self.source = Some(value),
            "log.target" => self.log_target = Some(value),
            n if n.starts_with('_') || n.starts_with("log.") => {}
            n => {
                self.extra.insert(n.to_owned(), value);
            }
        }
    }
}

impl Visit for FieldCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field.name(), value.to_owned());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field.name(), format!("{value:?}"));
    }
}

impl<S: Subscriber> Layer<S> for BusLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut f = FieldCollector::default();
        event.record(&mut f);
        let meta = event.metadata();
        let source = f
            .source
            .filter(|s| !s.is_empty())
            .or(f.log_target)
            .unwrap_or_else(|| meta.target().to_owned());
        self.bus
            .push(from_tracing(meta.level()), source, f.message, f.extra);
    }
}

// --------------------------------------------------------------------------- line format

/// `HH:MM:SS.mmm | LEVEL    | target:line - message k=v` (the file adds the date).
struct LineFormat {
    date: bool,
}

impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        let now = chrono::Local::now();
        let time = if self.date {
            now.format("%Y-%m-%d %H:%M:%S%.3f")
        } else {
            now.format("%H:%M:%S%.3f")
        };
        write!(
            writer,
            "{time} | {:<8} | {}:{} - ",
            level_name(meta.level()),
            meta.target(),
            meta.line().unwrap_or(0)
        )?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

// --------------------------------------------------------------------------- the log file

/// A file rotated by size: `name` → `name.1` → … → `name.<keep>` (the oldest goes).
pub struct RotatingFile {
    path: PathBuf,
    file: File,
    size: u64,
    max: u64,
    keep: usize,
}

impl RotatingFile {
    pub fn open(path: &Path, max: u64, keep: usize) -> io::Result<RotatingFile> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata()?.len();
        Ok(RotatingFile {
            path: path.to_owned(),
            file,
            size,
            max,
            keep,
        })
    }

    fn numbered(&self, n: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        if self.keep == 0 {
            self.file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)?;
            self.size = 0;
            return Ok(());
        }
        let _ = std::fs::remove_file(self.numbered(self.keep));
        for n in (1..self.keep).rev() {
            let from = self.numbered(n);
            if from.exists() {
                let _ = std::fs::rename(&from, self.numbered(n + 1));
            }
        }
        // std opens files with FILE_SHARE_DELETE on Windows: the open file can be renamed
        let renamed = std::fs::rename(&self.path, self.numbered(1));
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.size = if renamed.is_ok() {
            0
        } else {
            self.file.metadata()?.len()
        };
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size + buf.len() as u64 > self.max {
            self.rotate()?;
        }
        self.file.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Deletes the log files 0.3.x left in `dir`: loguru rotated `spaceknife.log` to
/// `spaceknife.<YYYY-MM-DD_HH-MM-SS_ffffff>[.<n>].log` (10 MB, 5 kept); this build rotates to
/// `spaceknife.log.<n>`, so those would stay for good (up to 50 MB). Only names of exactly that
/// form go; returns them. Called when the log file is opened (once there are none, it finds
/// none).
pub fn remove_old_rotations(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(is_loguru_rotation))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| std::fs::remove_file(p).is_ok())
        .collect()
}

/// `spaceknife.2026-10-26_09-00-00_123456.log`, or with a counter (`….2.log`).
fn is_loguru_rotation(name: &str) -> bool {
    let Some(mid) = name
        .strip_prefix("spaceknife.")
        .and_then(|r| r.strip_suffix(".log"))
    else {
        return false;
    };
    let (stamp, counter) = match mid.split_once('.') {
        Some((s, n)) => (s, Some(n)),
        None => (mid, None),
    };
    let b = stamp.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    b.len() == 26
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'_'
        && digits(11..13)
        && b[13] == b'-'
        && digits(14..16)
        && b[16] == b'-'
        && digits(17..19)
        && b[19] == b'_'
        && digits(20..26)
        && counter.is_none_or(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

static FILE_SINK: Mutex<Option<RotatingFile>> = Mutex::new(None);
/// The file's level as a `LevelFilter` ordinal: 0 off, 1 error … 5 trace.
static FILE_LEVEL: AtomicU8 = AtomicU8::new(0);
static FILE_ERRORS: AtomicU64 = AtomicU64::new(0);

fn level_code(level: LevelFilter) -> u8 {
    match level {
        LevelFilter::OFF => 0,
        LevelFilter::ERROR => 1,
        LevelFilter::WARN => 2,
        LevelFilter::INFO => 3,
        LevelFilter::DEBUG => 4,
        _ => 5,
    }
}

fn level_rank(level: &Level) -> u8 {
    match *level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

/// (Re)open the rotating log file at `level` (W7 高级 · 日志文件级别); `None` closes it.
/// Takes effect at once for every subscriber built by this module.
pub fn set_file_logging(path: Option<&Path>, level: LevelFilter) -> io::Result<()> {
    let mut sink = FILE_SINK.lock();
    if let Some(mut old) = sink.take() {
        let _ = old.flush();
    }
    FILE_LEVEL.store(0, Ordering::Relaxed);
    let Some(path) = path else { return Ok(()) };
    if let Some(dir) = path.parent() {
        remove_old_rotations(dir); // 0.3.x's, which nothing would delete any more
    }
    *sink = Some(RotatingFile::open(path, FILE_MAX_BYTES, FILE_KEEP)?);
    FILE_LEVEL.store(level_code(level), Ordering::Relaxed);
    Ok(())
}

/// Change the file's level, keeping the file.
pub fn set_file_level(level: LevelFilter) {
    if FILE_SINK.lock().is_some() {
        FILE_LEVEL.store(level_code(level), Ordering::Relaxed);
    }
}

struct FileWriter;

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(f) = FILE_SINK.lock().as_mut()
            && f.write(buf).is_err()
        {
            FILE_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = FILE_SINK.lock().as_mut() {
            let _ = f.flush();
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FileMakeWriter;

impl<'a> MakeWriter<'a> for FileMakeWriter {
    type Writer = FileWriter;
    fn make_writer(&'a self) -> FileWriter {
        FileWriter
    }
}

// --------------------------------------------------------------------------- setup

/// How [`init`] sets up logging.
#[derive(Clone)]
pub struct LogConfig {
    /// stderr's level
    pub level: LevelFilter,
    pub format: LogFormat,
    /// Feed this bus (the `log` topic); usually [`LogBus::global`]
    pub bus: Option<Arc<LogBus>>,
    /// Open the log file right away (path, level); [`set_file_logging`] changes it later
    pub file: Option<(PathBuf, LevelFilter)>,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            level: LevelFilter::INFO,
            format: LogFormat::Plain,
            bus: None,
            file: None,
        }
    }
}

/// The subscriber [`init`] installs (also usable with `tracing::subscriber::with_default`).
pub fn subscriber(cfg: &LogConfig) -> impl Subscriber + Send + Sync + for<'a> LookupSpan<'a> {
    let stderr_level = cfg.level;
    let stderr_filter = filter_fn(move |m| stderr_level >= *m.level() && !noisy(m));
    let stderr = match cfg.format {
        LogFormat::Plain => tracing_subscriber::fmt::layer()
            .event_format(LineFormat { date: false })
            .with_ansi(false)
            .with_writer(io::stderr)
            .with_filter(stderr_filter)
            .boxed(),
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(false)
            .with_span_list(false)
            .with_writer(io::stderr)
            .with_filter(stderr_filter)
            .boxed(),
    };
    let file = tracing_subscriber::fmt::layer()
        .event_format(LineFormat { date: true })
        .with_ansi(false)
        .with_writer(FileMakeWriter)
        .with_filter(
            filter_fn(|m| level_rank(m.level()) <= FILE_LEVEL.load(Ordering::Relaxed) && !noisy(m))
                .with_max_level_hint(LevelFilter::DEBUG),
        );
    let bus = cfg.bus.as_ref().map(|b| b.layer());
    tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .with(bus)
}

/// Install the process-wide subscriber (and route the `log` crate into it). Safe to call more
/// than once: later calls only change the log file.
pub fn init(cfg: &LogConfig) -> io::Result<()> {
    if let Some((path, level)) = &cfg.file {
        set_file_logging(Some(path), *level)?;
    }
    let _ = subscriber(cfg).try_init();
    Ok(())
}

/// Write the formatted fields of an event as `message k=v …` (used by tests and tools).
pub fn describe(entry: &LogEntry) -> String {
    let mut out = format!(
        "{} {} {}",
        entry.level.as_str(),
        entry.source,
        entry.message
    );
    for (k, v) in &entry.extra {
        let _ = write!(out, " {k}={v}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_keeps_a_bounded_backlog_and_filters_by_level() {
        let bus = Arc::new(LogBus::new(3));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let token = bus.subscribe(LogLevel::Warning, move |e| s.lock().push(e.seq));
        let sub = tracing_subscriber::registry().with(bus.layer());
        tracing::subscriber::with_default(sub, || {
            tracing::debug!("one");
            tracing::info!(event = "scan", scan_id = "s1", "two {}", 2);
            tracing::warn!(source = "monitor", "three");
            tracing::error!("four");
            tracing::trace!("not kept");
        });
        let all = bus.recent(10, LogLevel::Trace);
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [2, 3, 4]); // capacity 3
        assert_eq!(all[0].message, "two 2");
        assert_eq!(all[0].extra.get("event").map(String::as_str), Some("scan"));
        assert_eq!(all[0].extra.get("scan_id").map(String::as_str), Some("s1"));
        assert_eq!(all[0].source, module_path!());
        assert_eq!(all[1].source, "monitor");
        assert_eq!(all[1].level, LogLevel::Warning);
        assert_eq!(*seen.lock(), [3, 4]);
        assert_eq!(bus.recent(1, LogLevel::Trace)[0].seq, 4);
        assert_eq!(bus.recent(10, LogLevel::Error).len(), 1);
        assert!(bus.recent(0, LogLevel::Trace).is_empty());
        bus.unsubscribe(token);
        bus.push(LogLevel::Error, "x".into(), "five".into(), BTreeMap::new());
        assert_eq!(seen.lock().len(), 2);
        let (backlog, t2) = bus.subscribe_with_backlog(2, LogLevel::Info, |_| {});
        assert_eq!(backlog.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(bus.subscribers(), 1);
        bus.unsubscribe(t2);
        let ev = backlog[1].to_event("sub-1");
        assert_eq!(
            (ev.sub.as_str(), ev.seq, ev.message.as_str()),
            ("sub-1", 5, "five")
        );
    }

    #[test]
    fn the_file_rotates_by_size() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("logs").join("spaceknife.log");
        let mut f = RotatingFile::open(&path, 100, 2).unwrap();
        for i in 0..10 {
            f.write_all(format!("{i:0>59}\n").as_bytes()).unwrap(); // 60 bytes: one per file
        }
        drop(f);
        let read = |p: &Path| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(&path), format!("{:0>59}\n", 9));
        assert!(read(&tmp.path().join("logs/spaceknife.log.1")).ends_with("8\n"));
        assert!(read(&tmp.path().join("logs/spaceknife.log.2")).ends_with("7\n"));
        assert!(!tmp.path().join("logs/spaceknife.log.3").exists());
    }

    #[test]
    fn the_file_layer_follows_its_level() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("spaceknife.log");
        let cfg = LogConfig {
            level: LevelFilter::OFF,
            ..LogConfig::default()
        };
        set_file_logging(Some(&path), LevelFilter::INFO).unwrap();
        tracing::subscriber::with_default(subscriber(&cfg), || {
            tracing::info!(n = 1, "kept");
            tracing::debug!("dropped");
            set_file_level(LevelFilter::DEBUG);
            tracing::debug!("now kept");
        });
        set_file_logging(None, LevelFilter::INFO).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("| INFO     | sk_core::log:") && text.contains("kept n=1"),
            "{text}"
        );
        assert!(
            !text.contains("- dropped") && text.contains("now kept"),
            "{text}"
        );
        assert_eq!(text.lines().count(), 2);
    }
}
