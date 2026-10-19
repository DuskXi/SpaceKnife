//! Parallel hard scan (docs/native.md 2.2): `scan_slice(tasks, opts)`, the GIL-free twin of
//! `spaceknife.scan.worker.scan_slice` with the same inputs and outputs.
//!
//! `opts` is a dict (or any object with these attributes, e.g. a worker.SliceOptions):
//!
//! | key | default | meaning |
//! |---|---|---|
//! | `roots` | required | per root: dict/object with `dev`, `cluster_size` (4096), `resident_max` (0), `cross_mounts` (False), `skip` and `protect` (folders not entered: as mount points, as not accessible) |
//! | `budget_s` | 0.03 | time budget; directories not started by then go to the frontier |
//! | `max_entries` | 100000 | entry budget, checked like the time budget |
//! | `threads` | 0 | listing threads (0 = one per logical CPU, at most 16) |
//!
//! Unknown dict keys are rejected. The result is a dict with the fields of worker.SliceResult
//! (`SliceResult(**scan_slice(tasks, opts))`). Differences from the worker, all deliberate:
//!
//! - Directories are listed in parallel, so a *truncated* slice is not the worker's DFS prefix:
//!   it is some subtree of listed directories (still in preorder, references valid, frontier in
//!   preorder too). Budgets are checked before each directory, by every thread, so a slice can
//!   overshoot `max_entries` by up to one directory per thread. Untruncated slices are identical.
//! - Windows paths are opened in NT form: names Win32 would rewrite (trailing dot or space) are
//!   listed correctly, and an empty FAT root is an empty listing rather than an error.
//! - POSIX: subdirectories found during the call are opened with O_NOFOLLOW (the worker would
//!   follow a directory that was swapped for a symlink in between).

mod common;
#[cfg(unix)]
mod posix;
mod walk;
#[cfg(windows)]
mod win;

#[cfg(unix)]
use posix as sys;
#[cfg(windows)]
use win as sys;

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use numpy::{PyArray1, PyArrayMethods};
use pyo3::exceptions::{PyOverflowError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};

use common::{Arena, NativePath, RootOpts};
use walk::{Ctx, ErrRef, Node, Origin, Ref};

const OPT_KEYS: [&str; 4] = ["roots", "budget_s", "max_entries", "threads"];
const ROOT_KEYS: [&str; 6] = [
    "dev",
    "cluster_size",
    "resident_max",
    "cross_mounts",
    "skip",
    "protect",
];

struct Opts {
    roots: Vec<RootOpts>,
    budget_s: f64,
    max_entries: usize,
    threads: usize,
}

/// `obj[key]` for a dict (rejecting unknown keys), else `getattr(obj, key, None)`.
fn field<'py>(obj: &Bound<'py, PyAny>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    if let Ok(d) = obj.cast::<PyDict>() {
        return d.get_item(key);
    }
    if obj.hasattr(key)? {
        Ok(Some(obj.getattr(key)?))
    } else {
        Ok(None)
    }
}

fn check_keys(obj: &Bound<'_, PyAny>, allowed: &[&str], what: &str) -> PyResult<()> {
    if let Ok(d) = obj.cast::<PyDict>() {
        for k in d.keys() {
            let k: String = k.extract()?;
            if !allowed.contains(&k.as_str()) {
                return Err(PyValueError::new_err(format!(
                    "unknown {what} option {k:?}"
                )));
            }
        }
    }
    Ok(())
}

fn parse_root(obj: &Bound<'_, PyAny>) -> PyResult<RootOpts> {
    check_keys(obj, &ROOT_KEYS, "root")?;
    let mut r = RootOpts::default();
    if let Some(v) = field(obj, "dev")? {
        // st_dev is unsigned on Linux, may be negative (int32) on macOS
        r.dev = v
            .extract::<u64>()
            .or_else(|_| v.extract::<i64>().map(|x| x as u64))?;
    }
    if let Some(v) = field(obj, "cluster_size")? {
        r.cluster_size = v.extract()?;
    }
    if let Some(v) = field(obj, "resident_max")? {
        r.resident_max = v.extract()?;
    }
    if let Some(v) = field(obj, "cross_mounts")? {
        r.cross_mounts = v.is_truthy()?;
    }
    if let Some(v) = field(obj, "skip")? {
        let paths: Vec<String> = v.extract()?;
        r.skip = paths.into_iter().map(String::into_bytes).collect();
    }
    if let Some(v) = field(obj, "protect")? {
        let paths: Vec<String> = v.extract()?;
        r.protect = paths.into_iter().map(String::into_bytes).collect();
    }
    Ok(r)
}

fn parse_opts(opts: &Bound<'_, PyAny>) -> PyResult<Opts> {
    check_keys(opts, &OPT_KEYS, "scan_slice")?;
    let roots_obj =
        field(opts, "roots")?.ok_or_else(|| PyValueError::new_err("opts needs 'roots'"))?;
    let mut roots = Vec::new();
    for r in roots_obj.try_iter()? {
        roots.push(parse_root(&r?)?);
    }
    let budget_s = match field(opts, "budget_s")? {
        Some(v) => v.extract::<f64>()?,
        None => 0.03,
    };
    let max_entries = match field(opts, "max_entries")? {
        Some(v) => v.extract::<i64>()?.max(0) as usize,
        None => 100_000,
    };
    let threads = match field(opts, "threads")? {
        Some(v) if !v.is_none() => v.extract::<i64>()?.clamp(0, 256) as usize,
        _ => 0,
    };
    Ok(Opts {
        roots,
        budget_s,
        max_entries,
        threads,
    })
}

struct InTask<'py> {
    obj: Bound<'py, PyAny>,
    ref_obj: Bound<'py, PyAny>,
    path_obj: Bound<'py, PyAny>,
    ref_: Ref,
    path: NativePath,
    root: u32,
}

fn native_path(path: &Bound<'_, PyAny>) -> PyResult<NativePath> {
    let s: OsString = path.extract()?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        Ok(s.encode_wide().collect())
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(s.into_vec().into_boxed_slice())
    }
}

/// The Python str of a native path (os.path.join's result for a found directory).
fn py_path<'py>(py: Python<'py>, path: &[common::Unit]) -> PyResult<Bound<'py, PyAny>> {
    // SAFETY: valid pointer/length; both calls return a new reference or NULL with an exception.
    unsafe {
        #[cfg(windows)]
        let p = pyo3::ffi::PyUnicode_FromWideChar(
            path.as_ptr().cast(),
            path.len() as pyo3::ffi::Py_ssize_t,
        );
        #[cfg(unix)]
        let p = pyo3::ffi::PyUnicode_DecodeFSDefaultAndSize(
            path.as_ptr().cast(),
            path.len() as pyo3::ffi::Py_ssize_t,
        );
        Bound::from_owned_ptr_or_err(py, p)
    }
}

/// `name.lower()` of a raw extension, as worker.ext_of does it (non-ASCII through Python itself).
fn lower_ext<'py>(py: Python<'py>, raw: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    if raw.is_ascii() {
        // SAFETY: ASCII is UTF-8.
        let s = unsafe { std::str::from_utf8_unchecked(raw) }.to_ascii_lowercase();
        return Ok(PyString::new(py, &s).into_any());
    }
    // SAFETY: valid pointer/length and a static error-handler name.
    let decoded = unsafe {
        #[cfg(windows)]
        let p = pyo3::ffi::PyUnicode_DecodeUTF8(
            raw.as_ptr().cast(),
            raw.len() as pyo3::ffi::Py_ssize_t,
            c"surrogatepass".as_ptr(),
        );
        #[cfg(unix)]
        let p = pyo3::ffi::PyUnicode_DecodeFSDefaultAndSize(
            raw.as_ptr().cast(),
            raw.len() as pyo3::ffi::Py_ssize_t,
        );
        Bound::from_owned_ptr_or_err(py, p)?
    };
    decoded.call_method0("lower")
}

fn parse_tasks<'py>(tasks: &Bound<'py, PyAny>, n_roots: usize) -> PyResult<Vec<InTask<'py>>> {
    let mut out = Vec::new();
    for t in tasks.try_iter()? {
        let obj = t?;
        let ref_obj = obj.get_item(0)?;
        let path_obj = obj.get_item(1)?;
        let root: i64 = obj.get_item(2)?.extract()?;
        if root < 0 || root as usize >= n_roots {
            return Err(PyValueError::new_err(format!(
                "task root index {root} out of range"
            )));
        }
        let (a, b, c): (i64, i64, i64) = ref_obj.extract()?;
        if !path_obj.is_instance_of::<PyString>() {
            return Err(PyTypeError::new_err("task path must be a str"));
        }
        let path = native_path(&path_obj)?;
        out.push(InTask {
            obj,
            ref_obj,
            path_obj,
            ref_: [a, b, c],
            path,
            root: root as u32,
        });
    }
    Ok(out)
}

fn ref_tuple<'py>(py: Python<'py>, r: Ref) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, r)
}

/// See the module docs; same fields as worker.SliceResult.
#[pyfunction]
fn scan_slice<'py>(
    py: Python<'py>,
    tasks: &Bound<'py, PyAny>,
    opts: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    let started = Instant::now();
    let opts = parse_opts(opts)?;
    let tasks = parse_tasks(tasks, opts.roots.len())?;
    let threads = if opts.threads == 0 {
        walk::default_threads()
    } else {
        opts.threads
    };
    let pool = walk::pool(threads).map_err(PyRuntimeError::new_err)?;
    let deadline = if opts.budget_s.is_nan() || opts.budget_s <= 0.0 {
        Some(started)
    } else {
        Duration::try_from_secs_f64(opts.budget_s)
            .ok()
            .and_then(|d| started.checked_add(d))
    };
    let ctx = Ctx {
        root_state: opts.roots.iter().map(|_| sys::RootState::new()).collect(),
        roots: opts.roots,
        deadline,
        max_entries: opts.max_entries,
        listed: AtomicUsize::new(0),
        entries: AtomicUsize::new(0),
        arenas: (0..pool.current_num_threads().max(1))
            .map(|_| Mutex::new(Arena::default()))
            .collect(),
    };
    let input_refs: Vec<Ref> = tasks.iter().map(|t| t.ref_).collect();
    let nodes: Vec<Node> = tasks
        .iter()
        .map(|t| Node::new(t.path.clone(), t.root, false))
        .collect();

    let arenas: Vec<Arena> = py.detach(|| {
        walk::walk(&pool, &nodes, &ctx);
        ctx.arenas
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
            .collect()
    });
    let flat = py.detach(|| walk::flatten(&nodes, &input_refs, &arenas));

    // extension table in first-appearance order of the lowercased extension (worker._Acc.ext_id)
    let ext_table = PyList::new(py, [""])?;
    let ext_index = PyDict::new(py);
    ext_index.set_item("", 0)?;
    let mut final_of_raw: Vec<u16> = vec![0];
    let mut ascii_cache: HashMap<Vec<u8>, u16> = HashMap::new();
    for raw in flat.raw_exts.iter().skip(1) {
        let lowered = if raw.is_ascii() {
            Some(raw.to_ascii_lowercase())
        } else {
            None
        };
        if let Some(id) = lowered.as_ref().and_then(|l| ascii_cache.get(l)) {
            final_of_raw.push(*id);
            continue;
        }
        let s = lower_ext(py, raw)?;
        let id = match ext_index.get_item(&s)? {
            Some(v) => v.extract::<u16>()?,
            None => {
                let id = ext_table.len();
                if id > u16::MAX as usize {
                    return Err(PyOverflowError::new_err(
                        "more than 65536 extensions in one slice",
                    ));
                }
                ext_table.append(&s)?;
                ext_index.set_item(&s, id)?;
                id as u16
            }
        };
        if let Some(l) = lowered {
            ascii_cache.insert(l, id);
        }
        final_of_raw.push(id);
    }
    let ext_ids: Vec<u16> = flat
        .raw_ext
        .iter()
        .map(|&r| final_of_raw[r as usize])
        .collect();

    let found_path = |o: &Origin<'_>| -> PyResult<Bound<'py, PyAny>> {
        match o {
            Origin::Input(i) => Ok(tasks[*i].path_obj.clone()),
            Origin::Found(node) => py_path(py, &node.path),
        }
    };
    let paths = PyList::empty(py);
    for o in &flat.listing_paths {
        paths.append(found_path(o)?)?;
    }
    let frontier = PyList::empty(py);
    for (r, o) in &flat.frontier {
        match (r, o) {
            (None, Origin::Input(i)) => frontier.append(&tasks[*i].obj)?,
            (Some(r), Origin::Found(node)) => {
                let t = PyTuple::new(
                    py,
                    [
                        ref_tuple(py, *r)?.into_any(),
                        py_path(py, &node.path)?,
                        node.root.into_pyobject(py)?.into_any(),
                    ],
                )?;
                frontier.append(t)?;
            }
            _ => return Err(PyRuntimeError::new_err("inconsistent frontier entry")),
        }
    }
    let errors = PyList::empty(py);
    for (er, o, msg) in &flat.errors {
        let r = match er {
            ErrRef::Input(i) => tasks[*i].ref_obj.clone(),
            ErrRef::Child(r) => ref_tuple(py, *r)?.into_any(),
        };
        let t = PyTuple::new(py, [r, found_path(o)?, PyString::new(py, msg).into_any()])?;
        errors.append(t)?;
    }

    let n_listings = flat.listing_paths.len();
    let walk::Flat {
        refs,
        bounds,
        names,
        name_lens,
        kind,
        flags,
        size,
        alloc,
        mtime,
        ctime,
        atime,
        ino,
        nlink,
        dev,
        listing_paths,
        raw_ext,
        raw_exts,
        frontier: flat_frontier,
        errors: flat_errors,
    } = flat;
    drop((listing_paths, raw_ext, raw_exts, flat_frontier, flat_errors));
    let cols = PyDict::new(py);
    cols.set_item("kind", PyArray1::from_vec(py, kind))?;
    cols.set_item("flags", PyArray1::from_vec(py, flags))?;
    cols.set_item("size", PyArray1::from_vec(py, size))?;
    cols.set_item("alloc", PyArray1::from_vec(py, alloc))?;
    cols.set_item("mtime", PyArray1::from_vec(py, mtime))?;
    cols.set_item("ctime", PyArray1::from_vec(py, ctime))?;
    cols.set_item("atime", PyArray1::from_vec(py, atime))?;
    cols.set_item("ino", PyArray1::from_vec(py, ino))?;
    cols.set_item("nlink", PyArray1::from_vec(py, nlink))?;
    cols.set_item("dev", PyArray1::from_vec(py, dev))?;

    let out = PyDict::new(py);
    out.set_item(
        "refs",
        PyArray1::from_vec(py, refs).reshape([n_listings, 3])?,
    )?;
    out.set_item("paths", paths)?;
    out.set_item("bounds", PyArray1::from_vec(py, bounds))?;
    out.set_item("name_blob", PyBytes::new(py, &names))?;
    out.set_item("name_lens", PyArray1::from_vec(py, name_lens))?;
    out.set_item("cols", cols)?;
    out.set_item("ext_table", ext_table)?;
    out.set_item("ext_ids", PyArray1::from_vec(py, ext_ids))?;
    out.set_item("frontier", frontier)?;
    out.set_item("errors", errors)?;
    drop(names);
    // freeing a big walked tree takes milliseconds: let a pool thread do it after we return
    pool.spawn(move || drop((nodes, arenas)));
    out.set_item("elapsed", started.elapsed().as_secs_f64())?;
    Ok(out)
}

/// How POSIX listings get their attributes on macOS: "on" (getattrlistbulk, checked against
/// fstatat), "off" (turned off: a difference, or SPACEKNIFE_NO_BULK=1), "untested" (nothing listed
/// yet); "unsupported" elsewhere (readdir / getdents64 + fstatat, NtQueryDirectoryFile on Windows).
#[pyfunction]
fn bulk_listing() -> String {
    #[cfg(target_os = "macos")]
    {
        posix::bulk_state()
    }
    #[cfg(not(target_os = "macos"))]
    {
        "unsupported".to_owned()
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(scan_slice, m)?)?;
    m.add_function(wrap_pyfunction!(bulk_listing, m)?)?;
    Ok(())
}
