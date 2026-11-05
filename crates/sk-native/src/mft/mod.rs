//! NTFS `$MFT` fast scan (docs/native.md 2.1).
//!
//! [`parse_mft`] is pure computation and works everywhere (tests drive it with synthetic images);
//! [`volume_info`] / [`read_mft`] / [`scan_mft`] open `\\.\X:` and need administrator rights
//! (Windows only; elsewhere they fail with [`io::ErrorKind::Unsupported`]). [`scan_mft`] streams:
//! it reads the `$MFT` window by window and parses each while the next is read, so memory stays at
//! the output table plus a few windows ([`read_mft`] holds the whole `$MFT`: about 9 GB on a large
//! system volume). [`image_info`] / [`read_mft_image`] / [`scan_mft_image`] run the same code on an
//! image file holding a volume.
//!
//! Every error is an [`io::Error`]: `InvalidInput` for bad arguments, `InvalidData` for what is
//! not NTFS (or a corrupt `$MFT` layout), `Interrupted` when [`ScanOptions::cancel`] was set.

mod parse;
mod record;
mod runs;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use parse::Table as MftTable;
use runs::{Geometry, Layout, ReadAt};

#[cfg(windows)]
const READERS: usize = 4; // parallel 16 MiB requests while reading the $MFT
/// Bytes of `$MFT` per streaming window (two in flight) when [`ScanOptions::window`] is 0.
pub const DEFAULT_WINDOW: usize = 32 << 20;

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
    /// Disk-load limit in (0, 1]: the reader pauses after each window so that it is busy only
    /// this share of the time. 1 = no pause.
    pub duty: f64,
    /// Bytes per read window (rounded down to whole records, at least one); 0 = [`DEFAULT_WINDOW`].
    /// Small windows exercise the boundaries in tests.
    pub window: usize,
    /// `progress(done, total)` in bytes of `$MFT`: once with `done = 0`, then after each parsed
    /// window. Runs on the calling thread.
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
    /// Set it to stop the scan: the call returns an `Interrupted` error after the current window.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for ScanOptions<'_> {
    fn default() -> Self {
        Self {
            duty: 1.0,
            window: 0,
            progress: None,
            cancel: None,
        }
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn check_sizes(record_size: usize, cluster_size: usize) -> io::Result<()> {
    if !(512..=65536).contains(&record_size) || !record_size.is_multiple_of(512) {
        return Err(invalid(format!("invalid record_size {record_size}")));
    }
    if cluster_size == 0 || !cluster_size.is_power_of_two() {
        return Err(invalid(format!("invalid cluster_size {cluster_size}")));
    }
    Ok(())
}

fn check_duty(duty: f64) -> io::Result<()> {
    if duty > 0.0 && duty <= 1.0 {
        Ok(())
    } else {
        Err(invalid(format!("duty must be in (0, 1], not {duty}")))
    }
}

fn parse_error(e: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Parse a raw `$MFT` (whole records; a partial record at the end is ignored) into the
/// [`MftTable`] columns, one row per file name, in record order. Parallel (rayon).
pub fn parse_mft(data: &[u8], record_size: usize, cluster_size: usize) -> io::Result<MftTable> {
    check_sizes(record_size, cluster_size)?;
    parse::parse(data, record_size).map_err(parse_error)
}

/// The streaming parser of [`scan_mft`] over an in-memory `$MFT`, `window_records` records at a
/// time: the same rows as [`parse_mft`], except that records with an `$ATTRIBUTE_LIST` come last.
pub fn parse_mft_stream(
    data: &[u8],
    record_size: usize,
    cluster_size: usize,
    window_records: usize,
) -> io::Result<MftTable> {
    check_sizes(record_size, cluster_size)?;
    let step = window_records.max(1) * record_size;
    let whole = data.len() / record_size * record_size;
    let mut stream = parse::Stream::new(record_size);
    for window in data[..whole].chunks(step) {
        stream.feed(window);
    }
    stream
        .finish(|record| {
            let at = usize::try_from(record).ok()?.checked_mul(record_size)?;
            data.get(at..at.checked_add(record_size)?)
                .map(<[u8]>::to_vec)
        })
        .map_err(parse_error)
}

/// The whole `$MFT` read into a new buffer (bytes past the valid data length are zero).
fn read_all<R: ReadAt>(devs: &[R], geo: &Geometry, lay: &Layout) -> io::Result<Vec<u8>> {
    let len = usize::try_from(lay.data_size)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "$MFT too large"))?;
    // zeroed allocations of this size come straight from the OS: no fill pass
    let mut out = vec![0u8; len];
    runs::read_into(devs, geo, lay, &mut out)?;
    Ok(out)
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "$MFT scan cancelled")
}

/// Read the valid part of the `$MFT` window by window on one thread while the previous window
/// is parsed (rayon). Window buffers go back to the reader once parsed (two in flight, never
/// zero-filled again).
fn stream_scan<R: ReadAt + Send>(
    devs: &[R],
    geo: &Geometry,
    lay: &Layout,
    opts: ScanOptions<'_>,
) -> io::Result<MftTable> {
    check_duty(opts.duty)?;
    let ScanOptions {
        duty,
        window,
        mut progress,
        cancel,
    } = opts;
    let window = if window == 0 { DEFAULT_WINDOW } else { window };
    let cancelled = || cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
    let rs = geo.record;
    let window = (window / rs).max(1) * rs;
    let total = (lay.valid_size as usize).div_ceil(rs) * rs; // past it every record is unused
    let total = total.min(lay.data_size as usize);
    let (tx, rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    let (free_tx, free_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let mut report = |done: u64| {
        if let Some(p) = progress.as_mut() {
            p(done, total as u64);
        }
    };
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut start = 0usize;
            while start < total {
                if cancelled() {
                    let _ = tx.send(Err(interrupted()));
                    break;
                }
                let len = window.min(total - start);
                let mut buf = free_rx.try_recv().unwrap_or_default();
                // read_range writes every byte: only a first (or larger) buffer is zeroed
                buf.resize(len, 0);
                let t = std::time::Instant::now();
                let res = runs::read_range(devs, geo, lay, start as u64, &mut buf);
                let failed = res.is_err();
                if tx.send(res.map(|()| buf)).is_err() || failed {
                    break;
                }
                if duty < 1.0 {
                    std::thread::sleep(t.elapsed().mul_f64((1.0 - duty) / duty));
                }
                start += len;
            }
            drop(tx);
        });
        // owned here: an early return drops it, so a reader blocked on `send` wakes up and ends
        // before the scope joins it
        let rx = rx;
        let mut stream = parse::Stream::new(rs);
        let mut done = 0u64;
        report(0);
        for window in &rx {
            let window = window?;
            stream.feed(&window);
            done += window.len() as u64;
            let _ = free_tx.send(window);
            report(done);
            if cancelled() {
                return Err(interrupted());
            }
        }
        let fetch = |record: u64| {
            let at = record.checked_mul(rs as u64)?;
            if at + rs as u64 > total as u64 {
                return None;
            }
            let mut raw = vec![0u8; rs];
            runs::read_range(devs, geo, lay, at, &mut raw)
                .ok()
                .map(|()| raw)
        };
        stream.finish(fetch).map_err(parse_error)
    })
}

/// Normalise "C", "C:", "C:\" to the volume device path `\\.\C:`.
pub fn device_path(volume: &str) -> io::Result<String> {
    let v = volume.trim_end_matches(['\\', '/']).as_bytes();
    let ok = (v.len() == 1 || (v.len() == 2 && v[1] == b':')) && v[0].is_ascii_alphabetic();
    if !ok {
        return Err(invalid(format!("not a local volume like 'C:': {volume:?}")));
    }
    Ok(format!(r"\\.\{}:", (v[0] as char).to_ascii_uppercase()))
}

#[cfg(windows)]
fn open_volume(path: &str) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    // read-only and unbuffered: gigabytes of $MFT must not evict the file cache
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)
}

/// The volume's readers (several handles: parallel requests) and its geometry.
#[cfg(windows)]
fn open_readers(volume: &str) -> io::Result<(Vec<File>, Geometry, Layout)> {
    let path = device_path(volume)?;
    let first = open_volume(&path)?;
    let (geo, lay) = runs::open_layout(&first)?;
    let mut devs = vec![first];
    for _ in 1..READERS {
        devs.push(open_volume(&path)?);
    }
    Ok((devs, geo, lay))
}

#[cfg(not(windows))]
fn open_readers(volume: &str) -> io::Result<(Vec<File>, Geometry, Layout)> {
    device_path(volume)?;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MFT access is only available on Windows",
    ))
}

/// Geometry of an NTFS volume and its `$MFT` (needs administrator rights; Windows only).
pub fn volume_info(volume: &str) -> io::Result<VolumeInfo> {
    #[cfg(windows)]
    {
        let (geo, lay) = runs::open_layout(&open_volume(&device_path(volume)?)?)?;
        Ok(VolumeInfo::new(&geo, &lay))
    }
    #[cfg(not(windows))]
    {
        open_readers(volume).map(|_| unreachable!())
    }
}

/// The whole `$MFT` `$DATA` of an NTFS volume (needs administrator rights; Windows only).
pub fn read_mft(volume: &str) -> io::Result<Vec<u8>> {
    let (devs, geo, lay) = open_readers(volume)?;
    read_all(&devs, &geo, &lay)
}

/// Read and parse the whole `$MFT` of a volume, streaming (needs administrator rights; Windows
/// only).
pub fn scan_mft(volume: &str, opts: ScanOptions<'_>) -> io::Result<(MftTable, VolumeInfo)> {
    check_duty(opts.duty)?;
    let (devs, geo, lay) = open_readers(volume)?;
    let table = stream_scan(&devs, &geo, &lay, opts)?;
    Ok((table, VolumeInfo::new(&geo, &lay)))
}

fn open_image(path: &Path) -> io::Result<(Vec<File>, Geometry, Layout)> {
    let first = File::open(path)?;
    let (geo, lay) = runs::open_layout(&first)?;
    let second = File::open(path)?;
    Ok((vec![first, second], geo, lay))
}

/// [`volume_info`] of an image file holding a volume (boot sector at offset 0).
pub fn image_info(path: impl AsRef<Path>) -> io::Result<VolumeInfo> {
    let (geo, lay) = runs::open_layout(&File::open(path)?)?;
    Ok(VolumeInfo::new(&geo, &lay))
}

/// [`read_mft`] of an image file holding a volume.
pub fn read_mft_image(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let (devs, geo, lay) = open_image(path.as_ref())?;
    read_all(&devs, &geo, &lay)
}

/// [`scan_mft`] of an image file holding a volume.
pub fn scan_mft_image(
    path: impl AsRef<Path>,
    opts: ScanOptions<'_>,
) -> io::Result<(MftTable, VolumeInfo)> {
    check_duty(opts.duty)?;
    let (devs, geo, lay) = open_image(path.as_ref())?;
    let table = stream_scan(&devs, &geo, &lay, opts)?;
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
}
