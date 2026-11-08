//! NTFS `$MFT` fast scan (docs/native.md 2.1).
//!
//! [`parse_mft`] is pure computation and works everywhere (tests drive it with synthetic images);
//! [`volume_info`] / [`read_mft`] / [`scan_mft`] open `\\.\X:` and need administrator rights
//! (Windows only; elsewhere they fail with [`io::ErrorKind::Unsupported`]). [`scan_mft`] streams
//! (`scan.rs`): the `$MFT` is cut into 8 MiB windows, several are read at once (one overlapped,
//! unbuffered volume handle and an I/O completion port on Windows, straight into aligned window
//! buffers), each is parsed on a rayon pool as soon as it is in, and the parts are joined in
//! record order — so the rows and their order are those of parsing one window after the other,
//! and memory stays at the output table plus the windows in flight ([`read_mft`] holds the whole
//! `$MFT`: about 9 GB on a large system volume). How many windows are read at once is found while
//! the scan runs (`scan::Depth`: from 2, doubling while that gains more than a tenth, at most
//! 128 MiB in flight). [`image_info`] / [`read_mft_image`] / [`scan_mft_image`] run the same code
//! on an image file holding a volume (on Windows through the overlapped engine too, so tests
//! cover it without administrator rights; reader threads elsewhere).
//!
//! The parse runs on a rayon pool of its own, as wide as the CPUs this process may use
//! ([`crate::cpus::usable`]); it goes, threads and all, when the call returns.
//!
//! Benchmarks only (not an interface): `SPACEKNIFE_MFT_WINDOW` (bytes per window),
//! `SPACEKNIFE_MFT_DEPTH` (a fixed depth), `SPACEKNIFE_MFT_SPARE` (more window buffers) and
//! `SPACEKNIFE_MFT_PIECE` (bytes per read at most) apply when [`ScanOptions`] leaves them at 0.
//!
//! Every error is an [`io::Error`]: `InvalidInput` for bad arguments, `InvalidData` for what is
//! not NTFS (or a corrupt `$MFT` layout), `Interrupted` when [`ScanOptions::cancel`] was set.

pub mod build;
mod io;
mod parse;
mod record;
mod runs;
mod scan;

use std::fs::File;
use std::io as stdio;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use build::{BuildError, BuildReport, PseudoNames, TreeOptions, TreeSink, build_tree};
pub use parse::Table as MftTable;
use runs::{Geometry, Layout, ReadAt};
pub use scan::{Stats as ScanStats, Tuning as ScanTuning};

#[cfg(windows)]
const READERS: usize = 4; // parallel 16 MiB requests of read_mft
/// Bytes of `$MFT` per streaming window when [`ScanOptions::window`] is 0 (one read each).
pub const DEFAULT_WINDOW: usize = 8 << 20;
const DEPTH_MIN: usize = 2; // windows read at once: where the search starts (scan::Depth)
const IN_FLIGHT_MAX: usize = 128 << 20; // and the bytes in flight it may reach
const DEPTH_MAX: usize = 64; // windows, whatever their size (reader threads on an image)
const SPARE: usize = 4; // more window buffers: read, waiting to be parsed

/// Geometry of an NTFS volume and its `$MFT` (`mft_volume_info`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VolumeInfo {
    pub bytes_per_sector: usize,
    pub cluster_size: usize,
    pub record_size: usize,
    /// First cluster of the `$MFT`.
    pub mft_lcn: u64,
    /// Bytes of `$MFT` `$DATA`.
    pub mft_size: u64,
    /// Its valid data length (past it every record is unused).
    pub mft_valid_size: u64,
    /// Data runs of the `$MFT`.
    pub mft_fragments: usize,
    /// Bytes, including the backup boot sector.
    pub volume_size: u64,
}

impl VolumeInfo {
    fn new(geo: &Geometry, lay: &Layout) -> Self {
        Self {
            bytes_per_sector: geo.sector,
            cluster_size: geo.cluster,
            record_size: geo.record,
            mft_lcn: geo.mft_lcn,
            mft_size: lay.data_size,
            mft_valid_size: lay.valid_size,
            mft_fragments: lay.runs.len(),
            volume_size: geo.volume_size,
        }
    }
}

/// Options of [`scan_mft`] / [`scan_mft_image`].
pub struct ScanOptions<'a> {
    /// Disk-load limit in (0, 1]: the disk is busy with the scan this share of the time. Reads
    /// go in bursts that keep the disk busy at least 50 ms each, then pause for
    /// `busy × (1 − duty) / duty`. 1 = no pause.
    pub duty: f64,
    /// Bytes per read window (rounded down to whole records, at least one); 0 =
    /// `SPACEKNIFE_MFT_WINDOW` or [`DEFAULT_WINDOW`]. Small windows exercise the boundaries in
    /// tests.
    pub window: usize,
    /// Windows read at once, fixed; 0 = found while the scan runs (or `SPACEKNIFE_MFT_DEPTH`).
    pub depth: usize,
    /// `progress(done, total)` in bytes of `$MFT`: once with `done = 0`, then after each parsed
    /// window. Runs on the calling thread.
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
    /// Set it to stop the scan: the call returns an `Interrupted` error after the current window
    /// (reads still in flight are waited for).
    pub cancel: Option<Arc<AtomicBool>>,
    /// Filled with what the scan did: timings, counts, the depth it settled at (benchmarks).
    pub stats: Option<&'a mut ScanStats>,
    /// Test seam: called on the parse threads with each window's first record number before the
    /// window is parsed; a panic there stands for a bug in the parser.
    #[doc(hidden)]
    pub before_parse: Option<ParseHook>,
}

/// [`ScanOptions::before_parse`].
#[doc(hidden)]
pub type ParseHook = Arc<dyn Fn(u64) + Send + Sync>;

impl Default for ScanOptions<'_> {
    fn default() -> Self {
        Self {
            duty: 1.0,
            window: 0,
            depth: 0,
            progress: None,
            cancel: None,
            stats: None,
            before_parse: None,
        }
    }
}

fn invalid(msg: String) -> stdio::Error {
    stdio::Error::new(stdio::ErrorKind::InvalidInput, msg)
}

pub(crate) fn interrupted() -> stdio::Error {
    stdio::Error::new(stdio::ErrorKind::Interrupted, "$MFT scan cancelled")
}

fn check_sizes(record_size: usize, cluster_size: usize) -> stdio::Result<()> {
    if !(512..=65536).contains(&record_size) || !record_size.is_multiple_of(512) {
        return Err(invalid(format!("invalid record_size {record_size}")));
    }
    if cluster_size == 0 || !cluster_size.is_power_of_two() {
        return Err(invalid(format!("invalid cluster_size {cluster_size}")));
    }
    Ok(())
}

fn check_duty(duty: f64) -> stdio::Result<()> {
    if duty > 0.0 && duty <= 1.0 {
        Ok(())
    } else {
        Err(invalid(format!("duty must be in (0, 1], not {duty}")))
    }
}

fn parse_error(e: String) -> stdio::Error {
    stdio::Error::new(stdio::ErrorKind::InvalidData, e)
}

/// The message of a panic's payload.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

/// A panic while scanning (a bug): an error of the scan, the process goes on.
pub(crate) fn panicked(payload: &(dyn std::any::Any + Send)) -> stdio::Error {
    stdio::Error::other(format!("the $MFT scan failed: {}", panic_message(payload)))
}

/// The parse's rayon pool: as many threads as the CPUs this process may use. It goes with its
/// threads when dropped (nothing stays in the process after a scan, docs/memory.md §6). A job
/// reports its own panic (`scan::dispatch`); the handler only keeps an unforeseen one from
/// aborting the process (rayon's default for a panic in a spawned job), the panic hook has
/// logged it already.
fn pool() -> stdio::Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(crate::cpus::usable())
        .thread_name(|i| format!("sk-mft-{i}"))
        .panic_handler(|_| {})
        .build()
        .map_err(|e| stdio::Error::other(format!("cannot start the $MFT parse threads: {e}")))
}

/// Window size and reads in flight of a scan: the options, else the `SPACEKNIFE_MFT_*`
/// variables (benchmarks only), else the defaults.
fn tuning(window: usize, depth: usize) -> ScanTuning {
    let env = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&v| v > 0)
    };
    let window = if window > 0 {
        window
    } else {
        env("SPACEKNIFE_MFT_WINDOW").unwrap_or(DEFAULT_WINDOW)
    };
    let fixed = if depth > 0 {
        Some(depth)
    } else {
        env("SPACEKNIFE_MFT_DEPTH")
    };
    let (depth_min, depth_max) = match fixed {
        Some(d) => (d, d),
        None => (
            DEPTH_MIN,
            (IN_FLIGHT_MAX / window.max(1)).clamp(DEPTH_MIN, DEPTH_MAX),
        ),
    };
    ScanTuning {
        window,
        depth_min,
        depth_max,
        spare: std::env::var("SPACEKNIFE_MFT_SPARE")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(SPARE),
        piece: env("SPACEKNIFE_MFT_PIECE").unwrap_or(window.max(1)),
    }
}

/// Parse a raw `$MFT` (whole records; a partial record at the end is ignored) into the
/// [`MftTable`] columns, one row per file name, in record order. Parallel (rayon).
pub fn parse_mft(data: &[u8], record_size: usize, cluster_size: usize) -> stdio::Result<MftTable> {
    check_sizes(record_size, cluster_size)?;
    pool()?
        .install(|| parse::parse(data, record_size))
        .map_err(parse_error)
}

/// The streaming parser of [`scan_mft`] over an in-memory `$MFT`, `window_records` records at a
/// time: the same rows as [`parse_mft`], except that records with an `$ATTRIBUTE_LIST` come last.
pub fn parse_mft_stream(
    data: &[u8],
    record_size: usize,
    cluster_size: usize,
    window_records: usize,
) -> stdio::Result<MftTable> {
    check_sizes(record_size, cluster_size)?;
    stream_in_memory(data, record_size, window_records, None)
}

/// Test hook: [`parse_mft_stream`] with the windows parsed in parallel, in an order shuffled by
/// `seed`, and pushed back in record order, as [`scan_mft`] does. The table is the sequential
/// stream's, row order and name blob included.
#[doc(hidden)]
pub fn parse_mft_stream_concurrent(
    data: &[u8],
    record_size: usize,
    cluster_size: usize,
    window_records: usize,
    seed: u64,
) -> stdio::Result<MftTable> {
    check_sizes(record_size, cluster_size)?;
    stream_in_memory(data, record_size, window_records, Some(seed))
}

fn stream_in_memory(
    data: &[u8],
    rs: usize,
    window_records: usize,
    shuffled: Option<u64>,
) -> stdio::Result<MftTable> {
    let step = window_records.max(1) * rs;
    let whole = data.len() / rs * rs;
    pool()?
        .install(|| {
            let mut stream = parse::Stream::new(rs);
            if let Some(seed) = shuffled {
                use rayon::prelude::*;
                let windows: Vec<&[u8]> = data[..whole].chunks(step).collect();
                let mut order: Vec<usize> = (0..windows.len()).collect();
                shuffle(&mut order, seed);
                let parts: std::sync::Mutex<Vec<Option<parse::Part>>> =
                    std::sync::Mutex::new((0..windows.len()).map(|_| None).collect());
                order.par_iter().for_each(|&i| {
                    let first = (i * step / rs) as u64;
                    let part = parse::parse_window(windows[i], first, rs, MftTable::default());
                    parts.lock().unwrap()[i] = Some(part);
                });
                for part in parts.into_inner().unwrap() {
                    stream.push(part.expect("every window parsed"));
                }
            } else {
                // in parallel, a few windows at a time, pushed in record order
                use rayon::prelude::*;
                let windows: Vec<&[u8]> = data[..whole].chunks(step).collect();
                let batch = rayon::current_num_threads().max(1) * 2;
                for (b, group) in windows.chunks(batch).enumerate() {
                    let parts: Vec<parse::Part> = group
                        .par_iter()
                        .enumerate()
                        .map(|(k, w)| {
                            let first = ((b * batch + k) * step / rs) as u64;
                            parse::parse_window(w, first, rs, MftTable::default())
                        })
                        .collect();
                    for part in parts {
                        stream.push(part);
                    }
                }
            }
            stream.finish(|record| {
                let at = usize::try_from(record).ok()?.checked_mul(rs)?;
                data.get(at..at.checked_add(rs)?).map(<[u8]>::to_vec)
            })
        })
        .map_err(parse_error)
}

/// A deterministic Fisher-Yates shuffle (xorshift), for the tests' window orders.
fn shuffle(v: &mut [usize], seed: u64) {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

/// The whole `$MFT` read into a new buffer (bytes past the valid data length are zero).
fn read_all<R: ReadAt>(devs: &[R], geo: &Geometry, lay: &Layout) -> stdio::Result<Vec<u8>> {
    let len = usize::try_from(lay.data_size)
        .map_err(|_| stdio::Error::new(stdio::ErrorKind::OutOfMemory, "$MFT too large"))?;
    // zeroed allocations of this size come straight from the OS: no fill pass
    let mut out = vec![0u8; len];
    runs::read_into(devs, geo, lay, &mut out)?;
    Ok(out)
}

/// One record read again after the scan (`Stream::finish`); None past the valid data.
fn fetcher<'a, R: ReadAt>(
    dev: &'a R,
    geo: &'a Geometry,
    lay: &'a Layout,
) -> impl Fn(u64) -> Option<Vec<u8>> + Send + 'a {
    let rs = geo.record;
    let total = ((lay.valid_size as usize).div_ceil(rs) * rs).min(lay.data_size as usize) as u64;
    move |record: u64| {
        let at = record.checked_mul(rs as u64)?;
        if at + rs as u64 > total {
            return None;
        }
        let mut raw = vec![0u8; rs];
        runs::read_range(std::slice::from_ref(dev), geo, lay, at, &mut raw)
            .ok()
            .map(|()| raw)
    }
}

/// The streaming scan on `engine` (see the module docs); `sync` is a plain handle of the same
/// device for the records read again at the end.
fn stream_scan<E: io::Engine, R: ReadAt>(
    engine: E,
    sync: &R,
    geo: &Geometry,
    lay: &Layout,
    opts: ScanOptions<'_>,
    t0: Instant,
) -> stdio::Result<MftTable> {
    let ScanOptions {
        duty,
        window,
        depth,
        mut progress,
        cancel,
        stats,
        before_parse,
    } = opts;
    let tune = tuning(window, depth);
    let mut st = ScanStats {
        layout_s: t0.elapsed().as_secs_f64(),
        ..Default::default()
    };
    let pool = pool()?;
    let report = |done: u64, total: u64| {
        if let Some(p) = progress.as_mut() {
            p(done, total);
        }
    };
    let cancelled = || cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
    // a panic anywhere in the scan (a bug) is the scan's error: the engine waits for its reads
    // while unwinding, so nothing writes into freed buffers afterwards
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rows = scan::scan(
            engine,
            &pool,
            geo,
            lay,
            duty,
            &tune,
            fetcher(sync, geo, lay),
            report,
            cancelled,
            before_parse.as_ref(),
            &mut st,
        )?;
        let t = Instant::now();
        let table = pool.install(|| join(rows));
        st.join_s = t.elapsed().as_secs_f64();
        Ok(table)
    }))
    .unwrap_or_else(|payload| Err(panicked(payload.as_ref())));
    st.total_s = t0.elapsed().as_secs_f64();
    if let Some(out) = stats {
        *out = st;
    }
    res
}

/// The table of a streaming scan with its name pieces copied into one blob, in parallel.
fn join(rows: scan::Rows) -> MftTable {
    let parse::Joined {
        mut table,
        names,
        names_len,
    } = rows;
    // zeroed allocations of this size come straight from the OS: no fill pass
    let mut blob = vec![0u8; names_len];
    parse::Joined::copy_names(names, &mut blob);
    table.names = blob;
    table
}

/// Normalise "C", "C:", "C:\" to the volume device path `\\.\C:`.
pub fn device_path(volume: &str) -> stdio::Result<String> {
    let v = volume.trim_end_matches(['\\', '/']).as_bytes();
    let ok = (v.len() == 1 || (v.len() == 2 && v[1] == b':')) && v[0].is_ascii_alphabetic();
    if !ok {
        return Err(invalid(format!("not a local volume like 'C:': {volume:?}")));
    }
    Ok(format!(r"\\.\{}:", (v[0] as char).to_ascii_uppercase()))
}

#[cfg(windows)]
fn open_volume(path: &str, overlapped: bool) -> stdio::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    // read-only and unbuffered: gigabytes of $MFT must not evict the file cache
    let flags = FILE_FLAG_NO_BUFFERING | if overlapped { FILE_FLAG_OVERLAPPED } else { 0 };
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)
}

#[cfg(not(windows))]
fn unsupported(volume: &str) -> stdio::Error {
    match device_path(volume) {
        Err(e) => e,
        Ok(_) => stdio::Error::new(
            stdio::ErrorKind::Unsupported,
            "MFT access is only available on Windows",
        ),
    }
}

/// Geometry of an NTFS volume and its `$MFT` (needs administrator rights; Windows only).
pub fn volume_info(volume: &str) -> stdio::Result<VolumeInfo> {
    #[cfg(windows)]
    {
        let (geo, lay) = runs::open_layout(&open_volume(&device_path(volume)?, false)?)?;
        Ok(VolumeInfo::new(&geo, &lay))
    }
    #[cfg(not(windows))]
    {
        Err(unsupported(volume))
    }
}

/// The whole `$MFT` `$DATA` of an NTFS volume (needs administrator rights; Windows only).
pub fn read_mft(volume: &str) -> stdio::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        let path = device_path(volume)?;
        let first = open_volume(&path, false)?;
        let (geo, lay) = runs::open_layout(&first)?;
        let mut devs = vec![first];
        for _ in 1..READERS {
            devs.push(open_volume(&path, false)?);
        }
        read_all(&devs, &geo, &lay)
    }
    #[cfg(not(windows))]
    {
        Err(unsupported(volume))
    }
}

/// Read and parse the whole `$MFT` of a volume, streaming (needs administrator rights; Windows
/// only).
pub fn scan_mft(volume: &str, opts: ScanOptions<'_>) -> stdio::Result<(MftTable, VolumeInfo)> {
    check_duty(opts.duty)?;
    #[cfg(windows)]
    {
        let t0 = Instant::now();
        let path = device_path(volume)?;
        let sync = open_volume(&path, false)?;
        let (geo, lay) = runs::open_layout(&sync)?;
        let engine = io::Overlapped::new(open_volume(&path, true)?)?;
        let table = stream_scan(engine, &sync, &geo, &lay, opts, t0)?;
        Ok((table, VolumeInfo::new(&geo, &lay)))
    }
    #[cfg(not(windows))]
    {
        let _ = opts;
        Err(unsupported(volume))
    }
}

/// [`volume_info`] of an image file holding a volume (boot sector at offset 0).
pub fn image_info(path: impl AsRef<Path>) -> stdio::Result<VolumeInfo> {
    let (geo, lay) = runs::open_layout(&File::open(path)?)?;
    Ok(VolumeInfo::new(&geo, &lay))
}

/// [`read_mft`] of an image file holding a volume.
pub fn read_mft_image(path: impl AsRef<Path>) -> stdio::Result<Vec<u8>> {
    let path = path.as_ref();
    let first = File::open(path)?;
    let (geo, lay) = runs::open_layout(&first)?;
    let devs = [first, File::open(path)?];
    read_all(&devs, &geo, &lay)
}

/// The engine of an image file: overlapped reads on Windows (so tests cover that engine without
/// administrator rights), reader threads elsewhere.
fn image_engine(path: &Path, depth: usize) -> stdio::Result<Box<dyn io::Engine>> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
        let _ = depth;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(path)?;
        Ok(Box::new(io::Overlapped::new(file)?))
    }
    #[cfg(not(windows))]
    {
        let file = Arc::new(File::open(path)?);
        Ok(Box::new(io::Threads::new(file, depth)?))
    }
}

impl io::Engine for Box<dyn io::Engine> {
    fn submit(&mut self, read: io::Read) -> stdio::Result<()> {
        (**self).submit(read)
    }
    fn wait(&mut self) -> Option<(usize, stdio::Result<()>)> {
        (**self).wait()
    }
    fn in_flight(&self) -> usize {
        (**self).in_flight()
    }
}

/// [`scan_mft`] of an image file holding a volume.
pub fn scan_mft_image(
    path: impl AsRef<Path>,
    opts: ScanOptions<'_>,
) -> stdio::Result<(MftTable, VolumeInfo)> {
    check_duty(opts.duty)?;
    let t0 = Instant::now();
    let path = path.as_ref();
    let sync = File::open(path)?;
    let (geo, lay) = runs::open_layout(&sync)?;
    let engine = image_engine(path, tuning(opts.window, opts.depth).depth_max)?;
    let table = stream_scan(engine, &sync, &geo, &lay, opts, t0)?;
    Ok((table, VolumeInfo::new(&geo, &lay)))
}

/// Decoded NTFS mapping pairs `(vcn, lcn or None for sparse, clusters)` (tests, tools).
pub fn decode_runs(mapping_pairs: &[u8]) -> Option<Vec<(u64, Option<u64>, u64)>> {
    runs::decode_runs(mapping_pairs, 0)
        .map(|v| v.into_iter().map(|r| (r.vcn, r.lcn, r.len)).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_paths() {
        assert_eq!(super::device_path("c:").unwrap(), r"\\.\C:");
        assert_eq!(super::device_path("D:\\").unwrap(), r"\\.\D:");
        assert_eq!(super::device_path("e").unwrap(), r"\\.\E:");
        assert!(super::device_path("C:\\x").is_err());
        assert!(super::device_path("\\\\server\\share").is_err());
    }

    #[test]
    fn shuffles_are_permutations() {
        for seed in 0..20 {
            let mut v: Vec<usize> = (0..37).collect();
            super::shuffle(&mut v, seed);
            let mut s = v.clone();
            s.sort_unstable();
            assert_eq!(s, (0..37).collect::<Vec<_>>());
        }
    }

    #[test]
    fn tuning_follows_the_options() {
        let t = super::tuning(1 << 20, 0);
        assert_eq!((t.window, t.depth_min), (1 << 20, super::DEPTH_MIN));
        assert_eq!(t.depth_max, super::DEPTH_MAX.min(128)); // 128 MiB / 1 MiB, capped
        let t = super::tuning(super::DEFAULT_WINDOW, 3);
        assert_eq!((t.depth_min, t.depth_max), (3, 3));
    }
}
