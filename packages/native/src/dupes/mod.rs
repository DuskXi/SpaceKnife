//! Duplicate files (docs/dupes.md, docs/native.md 2.4): the IO stages the backend's
//! `spaceknife.dupes` engine drives batch by batch, each with the GIL released.
//!
//! - `dupes_identity(paths, want_usn)`: S1, metadata only (identity, fresh size, ns times,
//!   attributes, reparse tag, USN on Windows, whether the name reached was the one asked for).
//! - `dupes_extents(paths)`: S1b, block maps folded into 16-byte digests (metadata only).
//! - `DupesReader`: S2 (`probe`) and S3 (`step`), see reader.rs.
//!
//! Inputs are plain lists / numpy arrays, outputs dicts of numpy arrays (digests as `bytes`,
//! fixed width per row). Reason codes: codes.rs, the same as `spaceknife.dupes.codes`.

mod codes;
mod open;
mod reader;
mod share;

use std::time::Duration;

use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

fn slice<'a, T: numpy::Element>(a: &'a PyReadonlyArray1<'_, T>, name: &str) -> PyResult<&'a [T]> {
    a.as_slice()
        .map_err(|e| PyValueError::new_err(format!("{name}: {e}")))
}

/// S1 for each path: metadata only. Rows whose `reason` is not 0 may still carry what was
/// learnt (a directory's identity, for instance).
#[pyfunction]
#[pyo3(signature = (paths, want_usn = false))]
fn dupes_identity<'py>(py: Python<'py>, paths: Vec<String>, want_usn: bool) -> PyResult<Bound<'py, PyDict>> {
    let metas: Vec<Result<open::Meta, u8>> =
        py.detach(|| paths.iter().map(|p| open::meta_of_path(p, want_usn)).collect());
    let n = metas.len();
    let mut reason = Vec::with_capacity(n);
    let (mut dev, mut lo, mut hi) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut nlink, mut size, mut alloc) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut mtime, mut change, mut atime, mut btime) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let (mut attrs, mut tag, mut usn, mut case_ok) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    for m in metas {
        let (r, m) = match m {
            Ok(m) => {
                let r = if m.kind_reason != codes::OK {
                    m.kind_reason
                } else if !m.case_ok {
                    codes::AMBIGUOUS
                } else {
                    codes::OK
                };
                (r, m)
            }
            Err(r) => (r, open::Meta::default()),
        };
        reason.push(r);
        dev.push(m.stamp.dev);
        lo.push(m.stamp.fid_lo);
        hi.push(m.stamp.fid_hi);
        nlink.push(m.nlink);
        size.push(m.stamp.size as i64);
        alloc.push(m.alloc);
        mtime.push(m.stamp.mtime);
        change.push(m.stamp.change);
        atime.push(m.atime);
        btime.push(m.btime);
        attrs.push(m.attrs);
        tag.push(m.tag);
        usn.push(m.usn);
        case_ok.push(m.case_ok as u8);
    }
    let d = PyDict::new(py);
    d.set_item("reason", PyArray1::from_vec(py, reason))?;
    d.set_item("dev", PyArray1::from_vec(py, dev))?;
    d.set_item("fid_lo", PyArray1::from_vec(py, lo))?;
    d.set_item("fid_hi", PyArray1::from_vec(py, hi))?;
    d.set_item("nlink", PyArray1::from_vec(py, nlink))?;
    d.set_item("size", PyArray1::from_vec(py, size))?;
    d.set_item("alloc", PyArray1::from_vec(py, alloc))?;
    d.set_item("mtime", PyArray1::from_vec(py, mtime))?;
    d.set_item("change", PyArray1::from_vec(py, change))?;
    d.set_item("atime", PyArray1::from_vec(py, atime))?;
    d.set_item("btime", PyArray1::from_vec(py, btime))?;
    d.set_item("attrs", PyArray1::from_vec(py, attrs))?;
    d.set_item("tag", PyArray1::from_vec(py, tag))?;
    d.set_item("usn", PyArray1::from_vec(py, usn))?;
    d.set_item("case_ok", PyArray1::from_vec(py, case_ok))?;
    Ok(d)
}

/// S1b for each path: the block map's digest, shared bytes, and whether it is complete.
#[pyfunction]
fn dupes_extents<'py>(py: Python<'py>, paths: Vec<String>) -> PyResult<Bound<'py, PyDict>> {
    let maps: Vec<share::ExtMap> = py.detach(|| paths.iter().map(|p| share::extents(p)).collect());
    let mut hash = Vec::with_capacity(16 * maps.len());
    let d = PyDict::new(py);
    for m in &maps {
        hash.extend_from_slice(&m.hash);
    }
    d.set_item("reason", PyArray1::from_vec(py, maps.iter().map(|m| m.reason).collect()))?;
    d.set_item("hash", PyBytes::new(py, &hash))?;
    d.set_item("shared", PyArray1::from_vec(py, maps.iter().map(|m| m.shared).collect()))?;
    d.set_item(
        "complete",
        PyArray1::from_vec(py, maps.iter().map(|m| m.complete as u8).collect()),
    )?;
    d.set_item("extents", PyArray1::from_vec(py, maps.iter().map(|m| m.extents).collect()))?;
    Ok(d)
}

/// The reader of one disk queue (reader.rs). Thread-safe: concurrent calls must name
/// different members.
#[pyclass(module = "spaceknife_native._native", frozen)]
struct DupesReader {
    inner: reader::Reader,
}

#[pymethods]
impl DupesReader {
    #[new]
    #[pyo3(signature = (unbuffered = true, keep_atime = "skip", small_whole = 131072, lease_s = 2.0, chunk = 1048576))]
    fn new(unbuffered: bool, keep_atime: &str, small_whole: u64, lease_s: f64, chunk: usize) -> PyResult<Self> {
        let keep_read = match keep_atime {
            "skip" => false,
            "read" => true,
            other => return Err(PyValueError::new_err(format!("keep_atime: {other}"))),
        };
        if chunk == 0 || !chunk.is_multiple_of(reader::BLOCK as usize) {
            return Err(PyValueError::new_err("chunk must be a positive multiple of 4096"));
        }
        Ok(DupesReader {
            inner: reader::Reader::new(reader::Opts {
                unbuffered,
                keep_read,
                small_whole,
                lease: Duration::from_secs_f64(lease_s.max(0.0)),
                chunk,
            }),
        })
    }

    /// S2: see reader.rs `Reader::probe`.
    #[allow(clippy::too_many_arguments)]
    fn probe<'py>(
        &self,
        py: Python<'py>,
        keys: PyReadonlyArray1<'py, u64>,
        paths: Vec<String>,
        dev: PyReadonlyArray1<'py, u64>,
        fid_lo: PyReadonlyArray1<'py, u64>,
        fid_hi: PyReadonlyArray1<'py, u64>,
        size: PyReadonlyArray1<'py, i64>,
        attrs: PyReadonlyArray1<'py, u32>,
        hand_off: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let (k, dv, lo, hi) = (
            slice(&keys, "keys")?,
            slice(&dev, "dev")?,
            slice(&fid_lo, "fid_lo")?,
            slice(&fid_hi, "fid_hi")?,
        );
        let (sz, at) = (slice(&size, "size")?, slice(&attrs, "attrs")?);
        let n = k.len();
        if [paths.len(), dv.len(), lo.len(), hi.len(), sz.len(), at.len()]
            .iter()
            .any(|&x| x != n)
        {
            return Err(PyValueError::new_err("columns differ in length"));
        }
        let items: Vec<reader::ProbeIn> = (0..n)
            .map(|i| reader::ProbeIn {
                key: k[i],
                path: paths[i].clone(),
                dev: dv[i],
                fid_lo: lo[i],
                fid_hi: hi[i],
                size: sz[i].max(0) as u64,
                attrs: at[i],
            })
            .collect();
        let outs = py.detach(|| self.inner.probe(&items, hand_off));
        let mut full = Vec::with_capacity(32 * n);
        for o in &outs {
            full.extend_from_slice(&o.full.unwrap_or([0; 32]));
        }
        let d = PyDict::new(py);
        d.set_item("key", PyArray1::from_vec(py, outs.iter().map(|o| o.key).collect()))?;
        d.set_item("reason", PyArray1::from_vec(py, outs.iter().map(|o| o.reason).collect()))?;
        d.set_item("quick", PyArray1::from_vec(py, outs.iter().map(|o| o.quick).collect()))?;
        d.set_item(
            "has_full",
            PyArray1::from_vec(py, outs.iter().map(|o| o.full.is_some() as u8).collect()),
        )?;
        d.set_item("full", PyBytes::new(py, &full))?;
        d.set_item("mtime", PyArray1::from_vec(py, outs.iter().map(|o| o.mtime).collect()))?;
        d.set_item("change", PyArray1::from_vec(py, outs.iter().map(|o| o.change).collect()))?;
        d.set_item(
            "atime_unkept",
            PyArray1::from_vec(py, outs.iter().map(|o| o.atime_unkept as u8).collect()),
        )?;
        d.set_item("bytes", PyArray1::from_vec(py, outs.iter().map(|o| o.bytes).collect()))?;
        Ok(d)
    }

    /// S3: see reader.rs `Reader::step`.
    fn step<'py>(
        &self,
        py: Python<'py>,
        keys: PyReadonlyArray1<'py, u64>,
        until: PyReadonlyArray1<'py, u64>,
        max_bytes: u64,
        max_s: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let (k, u) = (slice(&keys, "keys")?, slice(&until, "until")?);
        if k.len() != u.len() {
            return Err(PyValueError::new_err("keys and until differ in length"));
        }
        let (k, u) = (k.to_vec(), u.to_vec());
        let outs = py.detach(|| self.inner.step(&k, &u, max_bytes, max_s, None));
        let n = outs.len();
        let (mut interim, mut fin) = (Vec::with_capacity(16 * n), Vec::with_capacity(32 * n));
        for o in &outs {
            interim.extend_from_slice(&o.interim);
            fin.extend_from_slice(&o.fin);
        }
        let d = PyDict::new(py);
        d.set_item("key", PyArray1::from_vec(py, outs.iter().map(|o| o.key).collect()))?;
        d.set_item("reason", PyArray1::from_vec(py, outs.iter().map(|o| o.reason).collect()))?;
        d.set_item("offset", PyArray1::from_vec(py, outs.iter().map(|o| o.offset).collect()))?;
        d.set_item("at", PyArray1::from_vec(py, outs.iter().map(|o| o.at).collect()))?;
        d.set_item("interim", PyBytes::new(py, &interim))?;
        d.set_item("fin", PyBytes::new(py, &fin))?;
        d.set_item("bytes", PyArray1::from_vec(py, outs.iter().map(|o| o.bytes).collect()))?;
        d.set_item("reopens", PyArray1::from_vec(py, outs.iter().map(|o| o.reopens).collect()))?;
        Ok(d)
    }

    /// Closes the handles of these members (all with None); their hashers and offsets stay.
    #[pyo3(signature = (keys = None))]
    fn suspend(&self, py: Python<'_>, keys: Option<Vec<u64>>) {
        py.detach(|| self.inner.suspend(keys.as_deref()));
    }

    /// Forgets these members (all with None), closing their handles.
    #[pyo3(signature = (keys = None))]
    fn drop(&self, py: Python<'_>, keys: Option<Vec<u64>>) {
        py.detach(|| self.inner.drop_members(keys.as_deref()));
    }

    fn open_handles(&self) -> usize {
        self.inner.open_handles()
    }

    fn members(&self) -> usize {
        self.inner.members()
    }

    fn counters<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        use std::sync::atomic::Ordering::Relaxed;
        let c = &self.inner.counters;
        let d = PyDict::new(py);
        d.set_item("data_opens", c.data_opens.load(Relaxed))?;
        d.set_item("reopens", c.reopens.load(Relaxed))?;
        d.set_item("bytes_probe", c.bytes_probe.load(Relaxed))?;
        d.set_item("bytes_full", c.bytes_full.load(Relaxed))?;
        d.set_item("atime_unkept", c.atime_unkept.load(Relaxed))?;
        d.set_item("changed", c.changed.load(Relaxed))?;
        d.set_item("open_handles", self.inner.open_handles())?;
        Ok(d)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(dupes_identity, m)?)?;
    m.add_function(wrap_pyfunction!(dupes_extents, m)?)?;
    m.add_class::<DupesReader>()?;
    m.add("DUPES_PROBE_PARAMS", reader::PROBE_PARAMS)?;
    Ok(())
}
