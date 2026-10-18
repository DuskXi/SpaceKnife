//! NTFS $MFT fast scan (docs/native.md 2.1).
//!
//! `parse_mft` is pure computation and works everywhere (tests drive it with synthetic images);
//! `mft_volume_info` / `read_mft` / `scan_mft` open `\\.\X:` and need administrator rights
//! (Windows only). `scan_mft` streams: it reads the $MFT window by window and parses each while
//! the next is read, so memory stays at the output table plus a few windows (`read_mft` holds
//! the whole $MFT: about 9 GB on a large system volume). `_mft_image_info` / `_read_mft_image` /
//! `_scan_mft_image` run the same code on an image file (tests).

mod parse;
mod record;
mod runs;

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
const READERS: usize = 4; // parallel 16 MiB requests while reading the $MFT
const WINDOW: usize = 32 << 20; // bytes of $MFT per streaming window (two in flight)

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
    table_dict(py, table)
}

fn table_dict(py: Python<'_>, table: parse::Table) -> PyResult<Bound<'_, PyDict>> {
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
    d.set_item("names", PyBytes::new(py, &table.names))?;
    d.set_item("bad_records", table.bad_records)?;
    Ok(d)
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

/// Read the valid part of the $MFT window by window on one thread while the previous window is
/// parsed (rayon); `duty` < 1 pauses the reader after each window so that it is busy only that
/// share of the time (the I/O limit of the scan). `on_window(done, total)` follows each parsed
/// window (bytes of $MFT).
fn stream_scan<R: ReadAt + Send>(
    devs: &[R],
    geo: &Geometry,
    lay: &Layout,
    duty: f64,
    window: usize,
    mut on_window: impl FnMut(u64, u64),
) -> std::io::Result<parse::Table> {
    let rs = geo.record;
    let window = (window / rs).max(1) * rs;
    let total = (lay.valid_size as usize).div_ceil(rs) * rs; // past it every record is unused
    let total = total.min(lay.data_size as usize);
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(1);
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut start = 0usize;
            while start < total {
                let len = window.min(total - start);
                let mut buf = vec![0u8; len];
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
        });
        let mut stream = parse::Stream::new(rs);
        let mut done = 0u64;
        on_window(0, total as u64);
        for window in rx {
            let window = window?;
            stream.feed(&window);
            done += window.len() as u64;
            on_window(done, total as u64);
        }
        let fetch = |record: u64| {
            let at = record.checked_mul(rs as u64)?;
            if at + rs as u64 > total as u64 {
                return None;
            }
            let mut raw = vec![0u8; rs];
            runs::read_range(devs, geo, lay, at, &mut raw).ok().map(|()| raw)
        };
        stream
            .finish(fetch)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    })
}

/// `stream_scan`'s window callback for an optional Python callable `progress(done, total)`; it
/// runs on the parsing thread with the GIL taken briefly (once per 32 MiB window). A failing
/// callback is ignored: progress reports never stop a scan.
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
fn open_volume(path: &str) -> std::io::Result<File> {
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

/// Geometry of an NTFS volume and its $MFT (needs administrator rights).
#[pyfunction]
fn mft_volume_info<'py>(py: Python<'py>, volume: &str) -> PyResult<Bound<'py, PyDict>> {
    #[cfg(windows)]
    {
        let path = device_path(volume).map_err(PyValueError::new_err)?;
        let (geo, lay) = py.detach(|| runs::open_layout(&open_volume(&path)?))?;
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
            let first = open_volume(&path)?;
            let (geo, lay) = runs::open_layout(&first)?;
            let mut devs = vec![first];
            for _ in 1..READERS {
                devs.push(open_volume(&path)?);
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
/// `(table, info)` with the `parse_mft` columns and the `mft_volume_info` keys.
#[pyfunction]
#[pyo3(signature = (volume, duty = 1.0, progress = None))]
fn scan_mft<'py>(
    py: Python<'py>,
    volume: &str,
    duty: f64,
    progress: Option<Py<PyAny>>,
) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    check_duty(duty)?;
    #[cfg(windows)]
    {
        let path = device_path(volume).map_err(PyValueError::new_err)?;
        let (geo, lay, table) = py.detach(|| -> std::io::Result<_> {
            let first = open_volume(&path)?;
            let (geo, lay) = runs::open_layout(&first)?;
            let mut devs = vec![first];
            for _ in 1..READERS {
                devs.push(open_volume(&path)?);
            }
            let table = stream_scan(&devs, &geo, &lay, duty, WINDOW, report_to(progress))?;
            Ok((geo, lay, table))
        })?;
        Ok((table_dict(py, table)?, info_dict(py, &geo, &lay)?))
    }
    #[cfg(not(windows))]
    {
        let _ = (py, volume, progress);
        Err(PyNotImplementedError::new_err(
            "MFT access is only available on Windows",
        ))
    }
}

/// Test hook: `scan_mft` on an image file holding a volume (`window`: bytes per read window,
/// 0 for the default; small windows exercise the boundaries).
#[pyfunction]
#[pyo3(signature = (path, duty = 1.0, window = 0, progress = None))]
fn _scan_mft_image(
    py: Python<'_>,
    path: PathBuf,
    duty: f64,
    window: usize,
    progress: Option<Py<PyAny>>,
) -> PyResult<(Bound<'_, PyDict>, Bound<'_, PyDict>)> {
    check_duty(duty)?;
    let window = if window == 0 { WINDOW } else { window };
    let (geo, lay, table) = py.detach(|| -> std::io::Result<_> {
        let first = File::open(&path)?;
        let (geo, lay) = runs::open_layout(&first)?;
        let devs = vec![first, File::open(&path)?];
        let table = stream_scan(&devs, &geo, &lay, duty, window, report_to(progress))?;
        Ok((geo, lay, table))
    })?;
    Ok((table_dict(py, table)?, info_dict(py, &geo, &lay)?))
}

/// Test hook: the streaming parser of `scan_mft` over an in-memory $MFT, `window_records` at a time.
#[pyfunction]
fn _parse_mft_stream<'py>(
    py: Python<'py>,
    data: &Bound<'py, PyAny>,
    record_size: usize,
    cluster_size: usize,
    window_records: usize,
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
            for window in bytes[..whole].chunks(step) {
                stream.feed(window);
            }
            stream.finish(|record| {
                let at = usize::try_from(record).ok()?.checked_mul(record_size)?;
                bytes.get(at..at + record_size).map(<[u8]>::to_vec)
            })
        })
        .map_err(PyValueError::new_err)?;
    table_dict(py, table)
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
}
