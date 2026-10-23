//! NTFS $MFT fast scan (docs/native.md 2.1).
//!
//! `parse_mft` is pure computation and works everywhere (tests drive it with synthetic images);
//! `mft_volume_info` / `read_mft` / `scan_mft` open `\\.\X:` and need administrator rights
//! (Windows only). `scan_mft` streams (`scan.rs`): several windows of the $MFT are read at once
//! (overlapped, unbuffered reads on Windows), each is parsed on the rayon pool as soon as it is
//! in, and the parts are joined in record order, so memory stays at the output table plus a few
//! windows (`read_mft` holds the whole $MFT: about 9 GB on a large system volume).
//! `_mft_image_info` / `_read_mft_image` / `_scan_mft_image` run the same code on an image file
//! (tests).

mod io;
mod parse;
mod record;
mod runs;
mod scan;

use std::fs::File;
use std::path::PathBuf;

use numpy::IntoPyArray;
use pyo3::buffer::PyBuffer;
#[cfg(not(windows))]
use pyo3::exceptions::PyNotImplementedError;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use runs::{Geometry, Layout, ReadAt};

#[cfg(windows)]
const READERS: usize = 4; // parallel 16 MiB requests of read_mft
const WINDOW: usize = 8 << 20; // bytes of $MFT per streaming window (one read each)
const DEPTH_MIN: usize = 2; // windows read at once: where the search starts (scan::Depth)
const IN_FLIGHT_MAX: usize = 128 << 20; // and the bytes in flight it may reach
const DEPTH_MAX: usize = 64; // windows, whatever their size (reader threads on an image)
const SPARE: usize = 4; // more window buffers: read, waiting to be parsed

fn check_sizes(record_size: usize, cluster_size: usize) -> PyResult<()> {
    if !(512..=65536).contains(&record_size) || !record_size.is_multiple_of(512) {
        return Err(PyValueError::new_err(format!(
            "invalid record_size {record_size}"
        )));
    }
    if cluster_size == 0 || !cluster_size.is_power_of_two() {
        return Err(PyValueError::new_err(format!(
            "invalid cluster_size {cluster_size}"
        )));
    }
    Ok(())
}

/// Window size and reads in flight of `scan_mft`. `SPACEKNIFE_MFT_WINDOW` / `_DEPTH` (a fixed
/// depth) / `_SPARE` / `_PIECE` override them (benchmarks only; not an interface).
fn tuning(window: usize) -> scan::Tuning {
    let env = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&v| v > 0)
    };
    let window = if window > 0 {
        window
    } else {
        env("SPACEKNIFE_MFT_WINDOW").unwrap_or(WINDOW)
    };
    let (depth_min, depth_max) = match env("SPACEKNIFE_MFT_DEPTH") {
        Some(d) => (d, d),
        None => (
            DEPTH_MIN,
            (IN_FLIGHT_MAX / window.max(1)).clamp(DEPTH_MIN, DEPTH_MAX),
        ),
    };
    scan::Tuning {
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

/// Parse a $MFT image into the `MftTable` columns (one row per file name).
#[pyfunction]
fn parse_mft<'py>(
    py: Python<'py>,
    data: &Bound<'py, PyAny>,
    record_size: usize,
    cluster_size: usize,
) -> PyResult<Bound<'py, PyDict>> {
    check_sizes(record_size, cluster_size)?;
    let buf = PyBuffer::<u8>::get(data)?;
    if !buf.is_c_contiguous() {
        return Err(PyValueError::new_err("data must be a contiguous buffer"));
    }
    let len = buf.len_bytes();
    // SAFETY: the buffer stays exported (and its memory alive) until `buf` drops after the call;
    // the parser only reads it.
    let bytes: &[u8] = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(buf.buf_ptr() as *const u8, len) }
    };
    let table = py
        .detach(|| parse::parse(bytes, record_size))
        .map_err(PyValueError::new_err)?;
    let names = PyBytes::new(py, &table.names);
    table_dict(py, table, names)
}

fn table_dict<'py>(
    py: Python<'py>,
    table: parse::Table,
    names: Bound<'py, PyBytes>,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("record", table.record.into_pyarray(py))?;
    d.set_item("parent", table.parent.into_pyarray(py))?;
    d.set_item("name_off", table.name_off.into_pyarray(py))?;
    d.set_item("name_len", table.name_len.into_pyarray(py))?;
    d.set_item("kind", table.kind.into_pyarray(py))?;
    d.set_item("flags", table.flags.into_pyarray(py))?;
    d.set_item("size", table.size.into_pyarray(py))?;
    d.set_item("alloc", table.alloc.into_pyarray(py))?;
    d.set_item("ads_size", table.ads_size.into_pyarray(py))?;
    d.set_item("ads_alloc", table.ads_alloc.into_pyarray(py))?;
    d.set_item("mtime", table.mtime.into_pyarray(py))?;
    d.set_item("ctime", table.ctime.into_pyarray(py))?;
    d.set_item("atime", table.atime.into_pyarray(py))?;
    d.set_item("nlink", table.nlink.into_pyarray(py))?;
    d.set_item("names", names)?;
    d.set_item("bad_records", table.bad_records)?;
    Ok(d)
}

/// A `bytes` of `len` bytes filled by `fill` with the GIL released (no zero-fill, no second copy).
fn bytes_filled<'py>(
    py: Python<'py>,
    len: usize,
    fill: impl FnOnce(&mut [u8]) + Send,
) -> PyResult<Bound<'py, PyBytes>> {
    // SAFETY: the object is private until returned and `fill` writes every byte of it (the
    // callers copy exactly `len` bytes in); on an error it is dropped unseen.
    unsafe {
        let ptr =
            pyo3::ffi::PyBytes_FromStringAndSize(std::ptr::null(), len as pyo3::ffi::Py_ssize_t);
        let obj = Bound::from_owned_ptr_or_err(py, ptr)?;
        let out = SendPtr(pyo3::ffi::PyBytes_AsString(ptr).cast::<u8>());
        py.detach(move || {
            let out = out;
            fill(std::slice::from_raw_parts_mut(out.0, len))
        });
        Ok(obj.cast_into_unchecked())
    }
}

struct SendPtr(*mut u8);
// SAFETY: the pointer is only used by the one closure it is moved into, while its owner waits.
unsafe impl Send for SendPtr {}

/// The table of a streaming scan as the `MftTable` dict: the columns as they are (no copy), the
/// name pieces copied once into the `bytes` object, in parallel with the GIL released.
fn rows_dict(py: Python<'_>, rows: scan::Rows) -> PyResult<Bound<'_, PyDict>> {
    let pieces = rows.names;
    let names = bytes_filled(py, rows.names_len, move |dst| {
        parse::Joined::copy_names(pieces, dst)
    })?;
    table_dict(py, rows.table, names)
}

fn info_dict<'py>(py: Python<'py>, geo: &Geometry, lay: &Layout) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("bytes_per_sector", geo.sector)?;
    d.set_item("cluster_size", geo.cluster)?;
    d.set_item("record_size", geo.record)?;
    d.set_item("mft_lcn", geo.mft_lcn)?;
    d.set_item("mft_size", lay.data_size)?;
    d.set_item("mft_valid_size", lay.valid_size)?;
    d.set_item("mft_fragments", lay.runs.len())?;
    d.set_item("volume_size", geo.volume_size)?;
    Ok(d)
}

fn stats_into(
    d: &Bound<'_, PyDict>,
    st: &scan::Stats,
    tune: &scan::Tuning,
    extra: &[(&str, f64)],
) -> PyResult<()> {
    d.set_item("windows", st.windows)?;
    d.set_item("reads", st.reads)?;
    d.set_item("bounced", st.bounced)?;
    d.set_item("buffers", st.buffers)?;
    d.set_item("max_in_flight", st.max_in_flight)?;
    d.set_item("max_parsing", st.max_parsing)?;
    d.set_item("read_s", st.read_s)?;
    d.set_item("busy_s", st.busy_s)?;
    d.set_item("paused_s", st.paused_s)?;
    d.set_item("starved_s", st.starved_s)?;
    d.set_item("parse_cpu_s", st.parse_cpu_s)?;
    d.set_item("drain_s", st.drain_s)?;
    d.set_item("finish_s", st.finish_s)?;
    d.set_item("push_s", st.push_s)?;
    d.set_item("deferred", st.deferred)?;
    d.set_item("extensions", st.extensions)?;
    d.set_item("fetched", st.fetched)?;
    d.set_item("redone", st.redone)?;
    d.set_item("window", tune.window)?;
    d.set_item("depth", st.depth)?;
    d.set_item("depth_min", tune.depth_min)?;
    d.set_item("depth_max", tune.depth_max)?;
    let trace: Vec<(usize, f64)> = st.depth_trace.iter().map(|&(d, r)| (d, r / 1e6)).collect();
    d.set_item("depth_trace_mb_s", trace)?;
    d.set_item("spare", tune.spare)?;
    d.set_item("piece", tune.piece)?;
    for (k, v) in extra {
        d.set_item(*k, *v)?;
    }
    Ok(())
}

/// The $MFT read straight into a new `bytes` (no zero-fill pass: gigabytes on a large volume).
fn read_all<'py, R: ReadAt>(
    py: Python<'py>,
    devs: Vec<R>,
    geo: Geometry,
    lay: Layout,
) -> PyResult<Bound<'py, PyBytes>> {
    let len = lay.data_size as usize;
    // SAFETY: the object is private until returned, and read_into writes every byte of it (or
    // fails, and the object is dropped unseen).
    unsafe {
        let ptr =
            pyo3::ffi::PyBytes_FromStringAndSize(std::ptr::null(), len as pyo3::ffi::Py_ssize_t);
        let obj = Bound::from_owned_ptr_or_err(py, ptr)?;
        let out =
            std::slice::from_raw_parts_mut(pyo3::ffi::PyBytes_AsString(ptr).cast::<u8>(), len);
        py.detach(|| runs::read_into(&devs, &geo, &lay, out))?;
        Ok(obj.cast_into_unchecked())
    }
}

/// One record read again after the scan (`Stream::finish`); None past the valid data.
fn fetcher<'a, R: ReadAt>(
    dev: &'a R,
    geo: &'a Geometry,
    lay: &'a Layout,
) -> impl Fn(u64) -> Option<Vec<u8>> + 'a {
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

/// `scan::scan`'s window callback for an optional Python callable `progress(done, total)`; it
/// runs on the scanning thread with the GIL taken briefly (once per window). A failing callback
/// is ignored: progress reports never stop a scan.
fn report_to(progress: Option<Py<PyAny>>) -> impl FnMut(u64, u64) {
    move |done, total| {
        if let Some(cb) = &progress {
            Python::attach(|py| {
                let _ = cb.call1(py, (done, total));
            });
        }
    }
}

fn check_duty(duty: f64) -> PyResult<()> {
    if duty > 0.0 && duty <= 1.0 {
        Ok(())
    } else {
        Err(PyValueError::new_err(format!(
            "duty must be in (0, 1], not {duty}"
        )))
    }
}

/// Normalise "C", "C:", "C:\" to the volume device path `\\.\C:`.
#[cfg_attr(not(windows), allow(dead_code))]
fn device_path(volume: &str) -> Result<String, String> {
    let v = volume.trim_end_matches(['\\', '/']).as_bytes();
    let ok = (v.len() == 1 || (v.len() == 2 && v[1] == b':')) && v[0].is_ascii_alphabetic();
    if !ok {
        return Err(format!("not a local volume like 'C:': {volume:?}"));
    }
    Ok(format!(r"\\.\{}:", (v[0] as char).to_ascii_uppercase()))
}

#[cfg(windows)]
fn open_volume(path: &str, overlapped: bool) -> std::io::Result<File> {
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

/// The engine of an image file: overlapped reads on Windows (so tests cover that engine without
/// administrator rights), reader threads elsewhere.
fn image_engine(path: &PathBuf, depth: usize) -> std::io::Result<Box<dyn io::Engine>> {
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
        let file = std::sync::Arc::new(File::open(path)?);
        Ok(Box::new(io::Threads::new(file, depth)?))
    }
}

impl io::Engine for Box<dyn io::Engine> {
    fn submit(&mut self, read: io::Read) -> std::io::Result<()> {
        (**self).submit(read)
    }
    fn wait(&mut self) -> Option<(usize, std::io::Result<()>)> {
        (**self).wait()
    }
    fn in_flight(&self) -> usize {
        (**self).in_flight()
    }
}

/// Geometry of an NTFS volume and its $MFT (needs administrator rights).
#[pyfunction]
fn mft_volume_info<'py>(py: Python<'py>, volume: &str) -> PyResult<Bound<'py, PyDict>> {
    #[cfg(windows)]
    {
        let path = device_path(volume).map_err(PyValueError::new_err)?;
        let (geo, lay) = py.detach(|| runs::open_layout(&open_volume(&path, false)?))?;
        info_dict(py, &geo, &lay)
    }
    #[cfg(not(windows))]
    {
        let _ = (py, volume);
        Err(PyNotImplementedError::new_err(
            "MFT access is only available on Windows",
        ))
    }
}

/// The whole $MFT `$DATA` of an NTFS volume (needs administrator rights).
#[pyfunction]
fn read_mft<'py>(py: Python<'py>, volume: &str) -> PyResult<Bound<'py, PyBytes>> {
    #[cfg(windows)]
    {
        let path = device_path(volume).map_err(PyValueError::new_err)?;
        let (geo, lay, devs) = py.detach(|| -> std::io::Result<_> {
            let first = open_volume(&path, false)?;
            let (geo, lay) = runs::open_layout(&first)?;
            let mut devs = vec![first];
            for _ in 1..READERS {
                devs.push(open_volume(&path, false)?);
            }
            Ok((geo, lay, devs))
        })?;
        read_all(py, devs, geo, lay)
    }
    #[cfg(not(windows))]
    {
        let _ = (py, volume);
        Err(PyNotImplementedError::new_err(
            "MFT access is only available on Windows",
        ))
    }
}

/// Read and parse the whole $MFT of a volume, streaming (needs administrator rights):
/// `(table, info)` with the `parse_mft` columns and the `mft_volume_info` keys. `stats`: a dict
/// the scan's timings and counts are written into (benchmarks).
#[pyfunction]
#[pyo3(signature = (volume, duty = 1.0, progress = None, stats = None))]
fn scan_mft<'py>(
    py: Python<'py>,
    volume: &str,
    duty: f64,
    progress: Option<Py<PyAny>>,
    stats: Option<Bound<'py, PyDict>>,
) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    check_duty(duty)?;
    #[cfg(windows)]
    {
        let path = device_path(volume).map_err(PyValueError::new_err)?;
        let tune = tuning(0);
        let mut st = scan::Stats::default();
        let t0 = std::time::Instant::now();
        let (geo, lay, rows, layout_s) = py.detach(|| -> std::io::Result<_> {
            let sync = open_volume(&path, false)?;
            let (geo, lay) = runs::open_layout(&sync)?;
            let layout_s = t0.elapsed().as_secs_f64();
            let engine = io::Overlapped::new(open_volume(&path, true)?)?;
            let rows = scan::scan(
                engine,
                &geo,
                &lay,
                duty,
                &tune,
                fetcher(&sync, &geo, &lay),
                report_to(progress),
                &mut st,
            )?;
            Ok((geo, lay, rows, layout_s))
        })?;
        let scanned_s = t0.elapsed().as_secs_f64();
        let table = rows_dict(py, rows)?;
        if let Some(d) = &stats {
            let total_s = t0.elapsed().as_secs_f64();
            stats_into(
                d,
                &st,
                &tune,
                &[
                    ("layout_s", layout_s),
                    ("join_s", total_s - scanned_s),
                    ("total_s", total_s),
                ],
            )?;
        }
        Ok((table, info_dict(py, &geo, &lay)?))
    }
    #[cfg(not(windows))]
    {
        let _ = (py, volume, progress, stats);
        Err(PyNotImplementedError::new_err(
            "MFT access is only available on Windows",
        ))
    }
}

/// Test hook: `scan_mft` on an image file holding a volume (`window`: bytes per read window,
/// 0 for the default; small windows exercise the boundaries).
#[pyfunction]
#[pyo3(signature = (path, duty = 1.0, window = 0, progress = None, stats = None))]
fn _scan_mft_image<'py>(
    py: Python<'py>,
    path: PathBuf,
    duty: f64,
    window: usize,
    progress: Option<Py<PyAny>>,
    stats: Option<Bound<'py, PyDict>>,
) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    check_duty(duty)?;
    let tune = tuning(window);
    let mut st = scan::Stats::default();
    let t0 = std::time::Instant::now();
    let (geo, lay, rows) = py.detach(|| -> std::io::Result<_> {
        let sync = File::open(&path)?;
        let (geo, lay) = runs::open_layout(&sync)?;
        let engine = image_engine(&path, tune.depth_max)?;
        let rows = scan::scan(
            engine,
            &geo,
            &lay,
            duty,
            &tune,
            fetcher(&sync, &geo, &lay),
            report_to(progress),
            &mut st,
        )?;
        Ok((geo, lay, rows))
    })?;
    let table = rows_dict(py, rows)?;
    if let Some(d) = &stats {
        stats_into(d, &st, &tune, &[("total_s", t0.elapsed().as_secs_f64())])?;
    }
    Ok((table, info_dict(py, &geo, &lay)?))
}

/// Test hook: the streaming parser of `scan_mft` over an in-memory $MFT, `window_records` at a
/// time; `concurrent` parses the windows in parallel (in an order shuffled by `seed`) and pushes
/// them back in record order, as `scan_mft` does.
#[pyfunction]
#[pyo3(signature = (data, record_size, cluster_size, window_records, concurrent = false, seed = 0))]
fn _parse_mft_stream<'py>(
    py: Python<'py>,
    data: &Bound<'py, PyAny>,
    record_size: usize,
    cluster_size: usize,
    window_records: usize,
    concurrent: bool,
    seed: u64,
) -> PyResult<Bound<'py, PyDict>> {
    check_sizes(record_size, cluster_size)?;
    let buf = PyBuffer::<u8>::get(data)?;
    if !buf.is_c_contiguous() {
        return Err(PyValueError::new_err("data must be a contiguous buffer"));
    }
    let bytes = buf.to_vec(py)?;
    let step = window_records.max(1) * record_size;
    let table = py
        .detach(|| {
            let whole = bytes.len() / record_size * record_size;
            let mut stream = parse::Stream::new(record_size);
            if concurrent {
                use rayon::prelude::*;
                let windows: Vec<(usize, &[u8])> =
                    bytes[..whole].chunks(step).enumerate().collect();
                let mut order: Vec<usize> = (0..windows.len()).collect();
                shuffle(&mut order, seed);
                let parts: std::sync::Mutex<Vec<Option<parse::Part>>> =
                    std::sync::Mutex::new((0..windows.len()).map(|_| None).collect());
                order.par_iter().for_each(|&k| {
                    let (i, w) = windows[k];
                    let first = (i * step / record_size) as u64;
                    let part = parse::parse_window(w, first, record_size, parse::Table::default());
                    parts.lock().unwrap()[i] = Some(part);
                });
                for part in parts.into_inner().unwrap() {
                    stream.push(part.expect("every window parsed"));
                }
            } else {
                for window in bytes[..whole].chunks(step) {
                    stream.feed(window);
                }
            }
            stream.finish(|record| {
                let at = usize::try_from(record).ok()?.checked_mul(record_size)?;
                bytes.get(at..at + record_size).map(<[u8]>::to_vec)
            })
        })
        .map_err(PyValueError::new_err)?;
    let names = PyBytes::new(py, &table.names);
    table_dict(py, table, names)
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

/// Test hook: `mft_volume_info` on an image file holding a volume (boot sector at offset 0).
#[pyfunction]
fn _mft_image_info(py: Python<'_>, path: PathBuf) -> PyResult<Bound<'_, PyDict>> {
    let (geo, lay) = py.detach(|| runs::open_layout(&File::open(&path)?))?;
    info_dict(py, &geo, &lay)
}

/// Test hook: `read_mft` on an image file holding a volume.
#[pyfunction]
fn _read_mft_image(py: Python<'_>, path: PathBuf) -> PyResult<Bound<'_, PyBytes>> {
    let (geo, lay, devs) = py.detach(|| -> std::io::Result<_> {
        let first = File::open(&path)?;
        let (geo, lay) = runs::open_layout(&first)?;
        let second = File::open(&path)?;
        Ok((geo, lay, vec![first, second]))
    })?;
    read_all(py, devs, geo, lay)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_mft, m)?)?;
    m.add_function(wrap_pyfunction!(mft_volume_info, m)?)?;
    m.add_function(wrap_pyfunction!(read_mft, m)?)?;
    m.add_function(wrap_pyfunction!(_mft_image_info, m)?)?;
    m.add_function(wrap_pyfunction!(_read_mft_image, m)?)?;
    m.add_function(wrap_pyfunction!(scan_mft, m)?)?;
    m.add_function(wrap_pyfunction!(_scan_mft_image, m)?)?;
    m.add_function(wrap_pyfunction!(_parse_mft_stream, m)?)?;
    Ok(())
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
}
