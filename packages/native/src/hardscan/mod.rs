//! Parallel hard scan (docs/native.md 2.2): `scan_slice(tasks, opts)`, the GIL-free twin of
//! `spaceknife.scan.worker.scan_slice` with the same inputs and outputs.
//!
//! `opts` is a dict (or any object with these attributes, e.g. a worker.SliceOptions):
//!
//! | key | default | meaning |
//! |---|---|---|
//! | `roots` | required | per root: dict/object with `dev`, `cluster_size` (4096), `resident_max` (0), `cross_mounts` (False), `skip` and `protect` (folders not entered: as mount points, as not accessible), `path` (the root as given), `verify_names` (False; Windows: tasks must be stored under their names below `path`, as on Btrfs) and `listed_alloc` (""; Windows: "NTFS", "ReFS" or "Btrfs" takes files' size on disk from their listing entries as that file system counts it, win.rs `listed_alloc`; "" estimates it from the size) |
//! | `budget_s` | 0.03 | time budget; directories not started by then go to the frontier |
//! | `max_entries` | 100000 | entry budget, checked like the time budget |
//! | `threads` | 0 | listing threads: 0 = the process's shared carriers, as many listings at once as the width pace.rs sets (the scanner's calls); N = a pool of N threads of the call's own (1: the calling thread) |
//! | `gate` | None | a `ListingGate` (slots.rs): a limit on listings in flight shared by every call that names it, and its pause (the backend's disk-load limit, one per disk) |
//! | `keep_access_time` | True | Windows and Linux: listing a folder does not update its last access time |
//! | `relative_open` | True | Windows: a found subdirectory is opened by name relative to its parent's handle, checked against the listed file id |
//! | `skip_final` | False | Windows (local NTFS, ReFS, Btrfs): no last directory query when the buffer before it had room for one more entry of any name (not in listings of reparse points; in each call the first 64 listings of a root that could leave it out, then one in 64, send it anyway, and one that returns entries turns it off for the root) |
//! | `dot_check` | False | Windows: a relative open is checked by the file id of its listing's "." entry (else by querying the handle) |
//! | `keep_where_updated` | True | Windows: `keep_access_time` only on volumes whose file system updates access times (the system's NtfsDisableLastAccessUpdate / RefsDisableLastAccessUpdate) |
//!
//! Test seams, not part of the interface: `_fold_strict` and `_extd_self_ids` (Windows: behave as
//! WinBtrfs does, win.rs RootState), `_verify_final` (Windows: send the last query `skip_final`
//! leaves out anyway and count what it returns, `_open_stats`), `_pinned_cap` (entries listed past the budget: below folders
//! opened by file id, and of folders whose name another entry shares; default max(2 × max_entries,
//! 10,000)). `ROOT_KEYS` and `OPT_KEYS` list the root and option keys this build knows.
//!
//! Unknown dict keys are rejected. The result is a dict with the fields of worker.SliceResult
//! (`SliceResult(**scan_slice(tasks, opts))`). Differences from the worker, all deliberate:
//!
//! - Directories are listed in parallel, so a *truncated* slice is not the worker's DFS prefix:
//!   it is some subtree of listed directories (still in preorder, references valid, frontier in
//!   preorder too). Budgets are checked before each directory, by every thread, so a slice can
//!   overshoot `max_entries` by up to one directory per thread. Untruncated slices are identical.
//! - Windows paths are opened in NT form: an empty FAT root is an empty listing rather than an
//!   error (names Win32 would rewrite, with a trailing dot or space, the worker lists through
//!   `\\?\` paths as well).
//! - POSIX: subdirectories found during the call are opened with O_NOFOLLOW (the worker would
//!   follow a directory that was swapped for a symlink in between).
//! - Windows: a folder whose name another entry of its listing shares in another case, which an
//!   open by name does not reach (WinBtrfs), is opened by its file id and listed; the worker, which
//!   cannot open by id, reports it as a listing error (case.rs, docs/native.md 2.2).

mod case;
mod common;
mod links;
mod pace;
#[cfg(unix)]
mod posix;
mod slots;
mod walk;
#[cfg(windows)]
mod win;

#[cfg(unix)]
use posix as sys;
#[cfg(windows)]
use win as sys;

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use numpy::{PyArray1, PyArrayMethods};
use pyo3::exceptions::{PyOverflowError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};

use common::{Arena, NativePath, RootOpts, StateOpts};
use slots::{ListingGate, Slots};
use walk::{Ctx, ErrRef, Node, Origin, Ref};

const OPT_KEYS: [&str; 14] = [
    "roots",
    "budget_s",
    "max_entries",
    "threads",
    "gate",
    "keep_access_time",
    "relative_open",
    "skip_final",
    "dot_check",
    "keep_where_updated",
    "_verify_final",
    "_fold_strict",
    "_extd_self_ids",
    "_pinned_cap",
];
const ROOT_KEYS: [&str; 9] = [
    "dev",
    "cluster_size",
    "resident_max",
    "cross_mounts",
    "skip",
    "protect",
    "path",
    "verify_names",
    "listed_alloc",
];

struct Opts {
    roots: Vec<RootOpts>,
    budget_s: f64,
    max_entries: usize,
    threads: usize,
    gate: Option<Arc<Slots>>,
    keep_access_time: bool,
    relative_open: bool,
    skip_final: bool,
    dot_check: bool,
    keep_where_updated: bool,
    verify_final: bool,
    fold_strict: bool,
    extd_self_ids: bool,
    pinned_cap: Option<usize>,
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
    if let Some(v) = field(obj, "path")? {
        if !v.is_none() {
            r.path = native_path(&v)?;
        }
    }
    if let Some(v) = field(obj, "verify_names")? {
        r.verify_names = v.is_truthy()?;
    }
    if let Some(v) = field(obj, "listed_alloc")?
        && !v.is_none()
    {
        let name: String = v.extract()?;
        r.listed_alloc = common::listed::parse(&name)
            .ok_or_else(|| PyValueError::new_err(format!("unknown listed_alloc {name:?}")))?;
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
    let gate = match field(opts, "gate")? {
        Some(v) if !v.is_none() => Some(
            v.cast::<ListingGate>()
                .map_err(|_| PyTypeError::new_err("gate must be a ListingGate"))?
                .get()
                .slots
                .clone(),
        ),
        _ => None,
    };
    let switch = |key: &str, default: bool| -> PyResult<bool> {
        match field(opts, key)? {
            Some(v) if !v.is_none() => v.is_truthy(),
            _ => Ok(default),
        }
    };
    let seam = |key: &str| -> PyResult<bool> {
        match field(opts, key)? {
            Some(v) => v.is_truthy(),
            None => Ok(false),
        }
    };
    let pinned_cap = match field(opts, "_pinned_cap")? {
        Some(v) if !v.is_none() => Some(v.extract::<i64>()?.max(0) as usize),
        _ => None,
    };
    Ok(Opts {
        roots,
        budget_s,
        max_entries,
        threads,
        gate,
        keep_access_time: switch("keep_access_time", true)?,
        relative_open: switch("relative_open", true)?,
        skip_final: switch("skip_final", false)?,
        dot_check: switch("dot_check", false)?,
        keep_where_updated: switch("keep_where_updated", true)?,
        verify_final: seam("_verify_final")?,
        fold_strict: seam("_fold_strict")?,
        extd_self_ids: seam("_extd_self_ids")?,
        pinned_cap,
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
    // threads 0: the shared carriers, within the width; else a pool of the call's own
    let pace = (opts.threads == 0).then(pace::get);
    let pool = match pace {
        Some(p) => {
            for r in &opts.roots {
                p.note_root(&r.path);
            }
            walk::carriers(p.capacity())
                .or_else(|_| walk::carriers(p.widths.start))
                .or_else(|_| walk::pool(1)) // no threads to be had: the calling thread's
        }
        None => walk::pool(opts.threads),
    }
    .map_err(PyRuntimeError::new_err)?;
    let budget_positive = !(opts.budget_s.is_nan() || opts.budget_s <= 0.0);
    let deadline = if !budget_positive {
        Some(started)
    } else {
        Duration::try_from_secs_f64(opts.budget_s)
            .ok()
            .and_then(|d| started.checked_add(d))
    };
    let ctx = Ctx {
        root_state: opts
            .roots
            .iter()
            .map(|r| {
                sys::RootState::new(&StateOpts {
                    keep_access_time: opts.keep_access_time,
                    relative_open: opts.relative_open,
                    verify_names: r.verify_names,
                    skip_final: opts.skip_final,
                    verify_final: opts.verify_final,
                    dot_check: opts.dot_check,
                    keep_where_updated: opts.keep_where_updated,
                    fold_strict: opts.fold_strict,
                    extd_self_ids: opts.extd_self_ids,
                })
            })
            .collect(),
        roots: opts.roots,
        deadline,
        budget_positive,
        max_entries: opts.max_entries,
        extra_cap: opts
            .pinned_cap
            .unwrap_or_else(|| opts.max_entries.saturating_mul(2).max(10_000)),
        extra: AtomicUsize::new(0),
        listed: AtomicUsize::new(0),
        entries: AtomicUsize::new(0),
        arenas: (0..pool.current_num_threads().max(1))
            .map(|_| Mutex::new(Arena::default()))
            .collect(),
        width: pace.map(pace::Pace::slots),
        gate: opts.gate,
        pace,
        running: AtomicUsize::new(0),
        most_running: pool.current_num_threads().max(1),
    };
    let input_refs: Vec<Ref> = tasks.iter().map(|t| t.ref_).collect();
    let nodes: Vec<Node> = tasks
        .iter()
        .map(|t| Node::new(t.path.clone(), t.root, false))
        .collect();

    let listing = Instant::now();
    let arenas: Vec<Arena> = py.detach(|| {
        walk::walk(&pool, &nodes, &ctx);
        if let Some(p) = pace {
            p.after_call();
        }
        ctx.arenas
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
            .collect()
    });
    let flat = py.detach(|| walk::flatten(&nodes, &input_refs, &arenas));
    let listed_s = listing.elapsed().as_secs_f64(); // without parsing, nor building the result

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
    out.set_item("elapsed", listed_s)?;
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

/// How directories were opened since the last reset, process-wide (tests and measurements; Windows:
/// relative opens, opens by path, and why relative opens fell back). Empty on POSIX.
#[pyfunction]
#[pyo3(signature = (reset = false))]
fn _open_stats(py: Python<'_>, reset: bool) -> PyResult<Bound<'_, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in sys::open_stats(reset) {
        d.set_item(k, v)?;
    }
    Ok(d)
}

/// The listing width of calls on the shared carriers (pace.rs, docs/native.md 2.2): `cpus` (the
/// CPUs this process may use), `start` (the start width: as many calling threads as the scanner
/// should use), `least`, `cold`, `max`, `width` now, `threads` (the carriers' pool now), `mode`
/// ("adaptive", "fixed", "start"), and `low` / `high`, the narrowest and widest width since the
/// last reset (`reset`: start over from the width now, after reading them).
#[pyfunction]
#[pyo3(signature = (reset = false))]
fn listing_threads(py: Python<'_>, reset: bool) -> PyResult<Bound<'_, PyDict>> {
    let p = pace::get();
    let (low, high) = p.seen(reset);
    let d = PyDict::new(py);
    d.set_item("cpus", p.n_cpu)?;
    d.set_item("start", p.widths.start)?;
    d.set_item("least", p.widths.least)?;
    d.set_item("cold", p.widths.cold)?;
    d.set_item("max", p.widths.most)?;
    d.set_item("width", p.width())?;
    d.set_item("threads", p.capacity())?;
    let mode = match p.mode {
        pace::Mode::Adaptive => "adaptive",
        pace::Mode::Fixed => "fixed",
        pace::Mode::Start => "start",
    };
    d.set_item("mode", mode)?;
    d.set_item("low", low)?;
    d.set_item("high", high)?;
    Ok(d)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(scan_slice, m)?)?;
    m.add_function(wrap_pyfunction!(listing_threads, m)?)?;
    m.add_class::<ListingGate>()?;
    // what the backend may pass per root (older builds reject keys they do not know)
    m.add("ROOT_KEYS", PyTuple::new(m.py(), ROOT_KEYS)?)?;
    m.add("OPT_KEYS", PyTuple::new(m.py(), OPT_KEYS)?)?;
    m.add_function(wrap_pyfunction!(_open_stats, m)?)?;
    m.add_class::<links::LinkSet>()?;
    m.add_function(wrap_pyfunction!(bulk_listing, m)?)?;
    Ok(())
}
