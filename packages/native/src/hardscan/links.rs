//! Hard links counted once while a scan runs (spaceknife.scan.scanner.Scanner._fold): the
//! `(dev, ino)` of every file with more than one link that was already counted, kept across the
//! scan's slices. The native twin of the Python set of tuples and its loop over each slice, which
//! cost about 0.5 µs per linked file (a quarter of a warm scan of a Linux system where uv's cache
//! and virtual environments share files by hard links).

use std::collections::HashSet;

use numpy::{PyReadonlyArray1, PyReadwriteArray1};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

/// worker.KIND_FILE and spaceknife.store.nodes.Flag.HARDLINK_DUP.
const KIND_FILE: u8 = 0;
const HARDLINK_DUP: u32 = 1 << 2;

#[pyclass(module = "spaceknife_native._native")]
#[derive(Default)]
pub struct LinkSet {
    seen: HashSet<(u64, u64)>,
}

fn slice<'a, T: numpy::Element>(a: &'a PyReadonlyArray1<'_, T>, name: &str) -> PyResult<&'a [T]> {
    a.as_slice()
        .map_err(|e| PyValueError::new_err(format!("{name}: {e}")))
}

#[pymethods]
impl LinkSet {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    /// Goes through a slice's entries in order: a file with more than one link whose
    /// `(dev, ino)` was counted before (earlier in this slice or in an earlier one) gets
    /// HARDLINK_DUP in `flags`; the first one is remembered. Returns how many were marked.
    fn mark(
        &mut self,
        py: Python<'_>,
        dev: PyReadonlyArray1<'_, u64>,
        ino: PyReadonlyArray1<'_, u64>,
        nlink: PyReadonlyArray1<'_, u16>,
        kind: PyReadonlyArray1<'_, u8>,
        mut flags: PyReadwriteArray1<'_, u32>,
    ) -> PyResult<usize> {
        let (dev, ino) = (slice(&dev, "dev")?, slice(&ino, "ino")?);
        let (nlink, kind) = (slice(&nlink, "nlink")?, slice(&kind, "kind")?);
        let flags = flags
            .as_slice_mut()
            .map_err(|e| PyValueError::new_err(format!("flags: {e}")))?;
        let n = flags.len();
        if [dev.len(), ino.len(), nlink.len(), kind.len()]
            .iter()
            .any(|&len| len != n)
        {
            return Err(PyValueError::new_err("columns differ in length"));
        }
        // the slice's own arrays (borrowed above), so the listing threads may go on meanwhile
        let seen = &mut self.seen;
        Ok(py.detach(move || {
            let mut marked = 0;
            for i in 0..n {
                if nlink[i] > 1 && kind[i] == KIND_FILE && !seen.insert((dev[i], ino[i])) {
                    flags[i] |= HARDLINK_DUP;
                    marked += 1;
                }
            }
            marked
        }))
    }

    fn __len__(&self) -> usize {
        self.seen.len()
    }

    fn __contains__(&self, key: (u64, u64)) -> bool {
        self.seen.contains(&key)
    }
}
