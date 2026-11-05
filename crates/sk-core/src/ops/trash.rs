//! Moving to the trash, restoring from it, deleting permanently (docs/manage.md §2.5).
//!
//! **A request to move something to the trash never becomes a permanent deletion.** On Windows
//! the shell silently destroys what does not fit its recycle bin (too large, path too long, the
//! volume set to "delete immediately", no bin at all) when no confirmation may be shown. So,
//! before an item is touched: the volume must have a bin (local fixed volume with a GUID,
//! `NukeOnDelete` off, no `NoRecycleFiles` policy), the item must fit the bin's capacity (with
//! headroom) and its paths must stay short enough inside the bin; otherwise the item gets
//! `no_trash` and is left alone. During the operation the shell's own announcement is the second
//! guard: a deletion it starts without "recycle" is refused (`ops::win`), which aborts the
//! operation before anything is destroyed; the rest is retried without the refused item.
//!
//! Linux follows the freedesktop.org Trash specification: the home trash
//! (`$XDG_DATA_HOME/Trash`) for files on its file system, else `$topdir/.Trash/$uid` (an admin
//! trash with the sticky bit) or `$topdir/.Trash-$uid`; an `info/<name>.trashinfo` is written
//! first (`O_EXCL` picks a free name), then the item is renamed into `files/` — a rename, never
//! a copy. macOS uses `NSFileManager trashItemAtURL` (best effort: not tested on a Mac).
//!
//! `trash_ref` (for [`restore`]) is the item's info file: `$I…` in `$Recycle.Bin\<SID>` on
//! Windows, the `.trashinfo` on Linux; on macOS a JSON pair of the trashed and original path.
//! Restoring never overwrites: an occupied original path fails with `exists`.
//!
//! Every function here is synchronous and may take long: run it on a worker thread. `progress`
//! is called once per item with its final result; `cancel` is polled between items (inside a
//! folder too, for permanent deletion).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

pub use sk_protocol::ops::OpCode;

use super::locks::find_lockers;
use super::names;
#[cfg_attr(target_os = "macos", allow(unused_imports))]
use super::paths::{protected_reason, resolve_lenient};
use super::text::{fill, trash_text as text};

/// One item's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpResult {
    pub path: String,
    pub ok: bool,
    pub code: OpCode,
    /// for the user, in the current language
    pub message: String,
    /// move_to_trash: what [`restore`] takes to put it back
    pub trash_ref: String,
}

impl OpResult {
    pub fn new(path: &str, code: OpCode, message: &str, trash_ref: &str) -> OpResult {
        let message = if message.is_empty() {
            default_message(code)
        } else {
            message.to_string()
        };
        OpResult {
            path: path.to_string(),
            ok: code == OpCode::Ok,
            code,
            message,
            trash_ref: trash_ref.to_string(),
        }
    }

    /// The protocol model (`OpItemResult`).
    pub fn to_protocol(&self) -> sk_protocol::ops::OpItemResult {
        sk_protocol::ops::OpItemResult {
            path: self.path.clone(),
            ok: self.ok,
            code: self.code,
            message: self.message.clone(),
        }
    }
}

fn default_message(code: OpCode) -> String {
    let key = code.as_str();
    let t = text(key);
    if t.is_empty() {
        key.to_string()
    } else {
        t.to_string()
    }
}

fn result(path: &str, code: OpCode) -> OpResult {
    OpResult::new(path, code, "", "")
}

fn result_msg(path: &str, code: OpCode, message: &str) -> OpResult {
    OpResult::new(path, code, message, "")
}

/// What moving something on a volume to the trash means there (UI-W5-39/40).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrashStatus {
    pub available: bool,
    /// why not (for the user)
    pub reason: String,
    /// where trashed items of this volume go
    pub location: String,
    /// bytes in it now (-1: unknown)
    pub used: i64,
    /// Windows: the bin's capacity on the volume (0: none or unknown)
    pub capacity: i64,
    /// trashed items stay on the volume: the space returns only when emptied
    pub same_volume: bool,
}

impl TrashStatus {
    fn unavailable(reason: &str, same_volume: bool) -> TrashStatus {
        TrashStatus {
            available: false,
            reason: reason.to_string(),
            location: String::new(),
            used: -1,
            capacity: 0,
            same_volume,
        }
    }
}

/// Longest path the shell's recycle engine handles (MAX_PATH less the NUL).
pub const WIN_MAX_PATH: usize = 259;
/// An item must stay below this share of the bin's capacity ...
pub const CAPACITY_SHARE: f64 = 0.9;
/// ... and one shell operation below this share (in case the engine sums a batch).
pub const BATCH_SHARE: f64 = 0.5;
/// A folder whose scanned size is below this share is not measured again.
pub const TRUST_SHARE: f64 = 0.25;
/// Items per shell operation (progress and cancel granularity).
pub const BATCH_ITEMS: usize = 64;
/// Entries walked to tell a Linux trash's size (-1 beyond).
pub const TRASH_USAGE_LIMIT: usize = 100_000;

/// Test seams: a volume's recycle-bin settings, and the longest path the checks allow.
#[doc(hidden)]
pub mod seams {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use parking_lot::Mutex;

    /// `(guid, total bytes)` → (nuke, max bytes); None: the real registry.
    pub type BinSettingsFn = fn(&str, u64) -> (bool, u64);
    pub static BIN_SETTINGS: Mutex<Option<BinSettingsFn>> = Mutex::new(None);
    pub static MAX_PATH: AtomicUsize = AtomicUsize::new(super::WIN_MAX_PATH);

    pub fn max_path() -> usize {
        MAX_PATH.load(Ordering::Relaxed)
    }
}

fn strerror(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// An I/O error as a result.
pub fn os_error(path: &str, e: &io::Error) -> OpResult {
    if e.kind() == io::ErrorKind::NotFound {
        return result(path, OpCode::Missing);
    }
    let raw = e.raw_os_error();
    if cfg!(windows) && matches!(raw, Some(32) | Some(33)) {
        return result_msg(path, OpCode::Locked, text("locked_unknown"));
    }
    #[cfg(unix)]
    let denied_errno = matches!(
        raw,
        Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EROFS)
    );
    #[cfg(not(unix))]
    let denied_errno = false;
    if e.kind() == io::ErrorKind::PermissionDenied || denied_errno {
        return result(path, OpCode::Denied);
    }
    result_msg(
        path,
        OpCode::Error,
        &fill(text("error"), &[("detail", strerror(e).into())]),
    )
}

fn lexists(p: impl AsRef<Path>) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

// --------------------------------------------------------------------------- checks

/// Whether `path` may open an entry other than the one its names say. The file system's stored
/// name of what it opens is compared with the scan `root`'s own followed by the path's names
/// below it; where they differ, a folder on the way that holds the path's name and another name
/// equal to it but for case decides. A folder that cannot be listed to tell counts as holding a
/// pair; nothing counts when the stored name is unknown (always, off Windows). `root_names`: the
/// roots' stored names, kept across the calls of one run.
pub fn reaches_another(
    path: &str,
    root: &str,
    root_names: &mut HashMap<String, Option<String>>,
) -> bool {
    let Some(below) = names::names_below(path, root) else {
        return false;
    };
    let Some(stored) = names::final_name(path) else {
        return false;
    };
    let root_name = root_names
        .entry(root.to_string())
        .or_insert_with(|| names::final_name(root))
        .clone();
    if names::task_is(&stored, root_name.as_deref(), &below) {
        return false;
    }
    let mut folder = PathBuf::from(root);
    for name in &below {
        let entries: Vec<String> =
            match std::fs::read_dir(names::long_path(&folder.to_string_lossy())) {
                Ok(it) => it
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect(),
                Err(_) => return true,
            };
        let upper = name.to_uppercase();
        if entries.contains(name)
            && entries
                .iter()
                .any(|e| e != name && e.to_uppercase() == upper)
        {
            return true;
        }
        folder.push(name);
    }
    false
}

/// Whether Win32 rewrites a name of `path` (Windows): one ending in a dot or a space loses it,
/// so the path names another item (`odd.` becomes `odd`) or none.
pub fn win32_trims(path: &str) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let rest = if path.as_bytes().get(1) == Some(&b':') {
        &path[2..]
    } else {
        path
    };
    rest.split(['\\', '/'])
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .any(|n| n.ends_with('.') || n.ends_with(' '))
}

/// Per index, the reason an item is skipped: reached by its path only as another entry, gone,
/// protected, held open (§2.5 re-check).
fn precheck(
    paths: &[String],
    os: &[PathBuf],
    check_locks: bool,
    roots: &HashMap<String, String>,
) -> HashMap<usize, OpResult> {
    let mut skipped = HashMap::new();
    let mut alive = Vec::new();
    let mut root_names = HashMap::new();
    for (i, p) in paths.iter().enumerate() {
        if win32_trims(p) {
            // before lexists: that would look at the other item
            skipped.insert(
                i,
                result_msg(p, OpCode::AmbiguousName, text("trailing_name")),
            );
            continue;
        }
        if !lexists(&os[i]) {
            skipped.insert(i, result(p, OpCode::Missing));
            continue;
        }
        if let Some(reason) = protected_reason(p) {
            skipped.insert(i, result_msg(p, OpCode::Protected, &reason));
            continue;
        }
        if let Some(root) = roots.get(p)
            && reaches_another(p, root, &mut root_names)
        {
            skipped.insert(i, result(p, OpCode::AmbiguousName));
            continue;
        }
        alive.push(i);
    }
    if check_locks && !alive.is_empty() {
        let asked: Vec<String> = alive.iter().map(|&i| paths[i].clone()).collect();
        let lockers = find_lockers(&asked);
        for i in alive {
            if let Some(who) = lockers.get(&paths[i])
                && !who.is_empty()
            {
                let names = who.join(text("list_sep"));
                skipped.insert(
                    i,
                    result_msg(
                        &paths[i],
                        OpCode::Locked,
                        &fill(text("locked"), &[("names", names.into())]),
                    ),
                );
            }
        }
    }
    skipped
}

/// `(items done, items in total, the one just done)`.
pub type ProgressFn<'a> = &'a mut (dyn FnMut(usize, usize, &OpResult) + Send);
/// True: stop.
pub type CancelFn<'a> = &'a (dyn Fn() -> bool + Sync);

/// Options of [`move_to_trash`] and [`delete_permanently`].
pub struct OpOptions<'a> {
    pub progress: Option<ProgressFn<'a>>,
    pub cancel: Option<CancelFn<'a>>,
    /// bytes per path (keyed by the same strings) as last scanned, e.g. the basket's targets;
    /// folders well below the recycle bin's capacity are then not measured again
    pub sizes: Option<&'a HashMap<String, i64>>,
    /// skip items held open
    pub check_locks: bool,
    /// the scan root of each path (same keys); a path whose names below it open an entry
    /// differing only in case is left alone (`ambiguous_name`, [`reaches_another`])
    pub roots: Option<&'a HashMap<String, String>>,
}

impl Default for OpOptions<'_> {
    fn default() -> Self {
        OpOptions {
            progress: None,
            cancel: None,
            sizes: None,
            check_locks: true,
            roots: None,
        }
    }
}

/// Results in input order, progress as they become final.
struct Run<'a> {
    /// as shown (results, keys of the options' maps; lossy where a name is not Unicode)
    paths: Vec<String>,
    /// as given: what the file system is asked about (a name that is not valid Unicode, a lone
    /// UTF-16 surrogate on Windows or bytes that are not UTF-8 elsewhere, is kept exactly)
    os: Vec<PathBuf>,
    results: Vec<Option<OpResult>>,
    progress: Option<ProgressFn<'a>>,
    done: usize,
}

impl<'a> Run<'a> {
    fn new<P: AsRef<Path>>(given: &[P], progress: Option<ProgressFn<'a>>) -> Run<'a> {
        let os: Vec<PathBuf> = given.iter().map(|p| p.as_ref().to_path_buf()).collect();
        Run {
            paths: os
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            results: vec![None; os.len()],
            os,
            progress,
            done: 0,
        }
    }

    fn set(&mut self, i: usize, r: OpResult) {
        if self.results[i].is_some() {
            return;
        }
        self.done += 1;
        let total = self.paths.len();
        if let Some(p) = self.progress.as_mut() {
            // a broken observer must not stop the operation
            let done = self.done;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p(done, total, &r)))
                .is_err()
            {
                tracing::warn!("progress callback failed");
            }
        }
        self.results[i] = Some(r);
    }

    fn finish(mut self) -> Vec<OpResult> {
        for i in 0..self.paths.len() {
            if self.results[i].is_none() {
                let p = self.paths[i].clone();
                self.set(i, result(&p, OpCode::Cancelled));
            }
        }
        self.results.into_iter().flatten().collect()
    }
}

fn cancelled(cancel: Option<CancelFn<'_>>) -> bool {
    cancel.is_some_and(|c| c())
}

// --------------------------------------------------------------------------- public

/// Whether the volume of `path` has a trash to move things to (Windows: a recycle bin on a
/// local fixed volume that is not set to delete immediately).
pub fn trash_available(path: &str) -> bool {
    trash_status(path, false).available
}

/// The trash of the volume holding `path`; `usage`: also tell how much it holds now.
pub fn trash_status(path: &str, usage: bool) -> TrashStatus {
    let run = || -> io::Result<TrashStatus> {
        #[cfg(windows)]
        {
            win_status(path, usage)
        }
        #[cfg(target_os = "macos")]
        {
            let _ = usage;
            mac_status(path)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            fd_status(path, usage)
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (path, usage);
            Ok(TrashStatus::unavailable(text("unsupported"), false))
        }
    };
    run().unwrap_or_else(|e| {
        TrashStatus::unavailable(
            &fill(text("error"), &[("detail", strerror(&e).into())]),
            true,
        )
    })
}

/// Move each path to the trash; one result per path, in order. Never deletes anything.
///
/// Paths are taken as the file system names them (`PathBuf` / `OsString`: names that are not
/// valid Unicode are reached exactly); results and the keys of `opts.sizes` / `opts.roots` are
/// their `to_string_lossy` forms.
pub fn move_to_trash<P: AsRef<Path>>(paths: &[P], opts: OpOptions<'_>) -> Vec<OpResult> {
    let empty_roots = HashMap::new();
    let empty_sizes = HashMap::new();
    let cancel = opts.cancel;
    let mut run = Run::new(paths, opts.progress);
    for (i, r) in precheck(
        &run.paths,
        &run.os,
        opts.check_locks,
        opts.roots.unwrap_or(&empty_roots),
    ) {
        run.set(i, r);
    }
    let todo: Vec<usize> = (0..paths.len())
        .filter(|&i| run.results[i].is_none())
        .collect();
    #[cfg(windows)]
    win_trash(&mut run, &todo, opts.sizes.unwrap_or(&empty_sizes), cancel);
    #[cfg(not(windows))]
    {
        let _ = opts.sizes.unwrap_or(&empty_sizes);
        for i in todo {
            if cancelled(cancel) {
                break;
            }
            let p = run.paths[i].clone();
            // macOS file systems store names as UTF-8: the shown path is the path
            #[cfg(target_os = "macos")]
            let r = mac_trash(&p);
            #[cfg(all(unix, not(target_os = "macos")))]
            let r = fd_trash(&p, &run.os[i].clone());
            #[cfg(not(unix))]
            let r = result_msg(&p, OpCode::NoTrash, text("unsupported"));
            run.set(i, r);
        }
    }
    run.finish()
}

/// Put trashed items back where they were (never over an existing item: `exists`).
pub fn restore(refs: &[String]) -> Vec<OpResult> {
    refs.iter()
        .map(|r| {
            #[cfg(windows)]
            let got = win_restore(r);
            #[cfg(target_os = "macos")]
            let got = mac_restore(r);
            #[cfg(all(unix, not(target_os = "macos")))]
            let got = fd_restore(r);
            #[cfg(not(any(windows, unix)))]
            let got: io::Result<OpResult> = Ok(result_msg(r, OpCode::Error, text("unsupported")));
            got.unwrap_or_else(|e| os_error(r, &e))
        })
        .collect()
}

/// Delete each path for good (folders with everything inside); links are removed, never
/// followed, and other file systems mounted inside a folder are not entered. `roots` as for
/// [`move_to_trash`] (paths as given, results and keys as shown).
pub fn delete_permanently<P: AsRef<Path>>(paths: &[P], opts: OpOptions<'_>) -> Vec<OpResult> {
    let empty_roots = HashMap::new();
    let cancel = opts.cancel;
    let mut run = Run::new(paths, opts.progress);
    for (i, r) in precheck(
        &run.paths,
        &run.os,
        opts.check_locks,
        opts.roots.unwrap_or(&empty_roots),
    ) {
        run.set(i, r);
    }
    for i in 0..paths.len() {
        if run.results[i].is_some() {
            continue;
        }
        if cancelled(cancel) {
            break;
        }
        let (shown, os) = (run.paths[i].clone(), run.os[i].clone());
        let r = delete_one(&shown, &os, cancel);
        run.set(i, r);
    }
    run.finish()
}

// --------------------------------------------------------------------------- permanent deletion

/// A symlink, or a Windows reparse point that is a name surrogate (junction, mount point).
fn is_link_like(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

fn unlink(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && cfg!(windows) => {
            let meta = std::fs::symlink_metadata(path)?;
            if meta.permissions().readonly() {
                // what Explorer deletes after asking
                let mut perm = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                std::fs::set_permissions(path, perm)?;
                std::fs::remove_file(path)
            } else if meta.is_dir() || meta.file_type().is_symlink() {
                std::fs::remove_dir(path) // a directory link
            } else {
                Err(e)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::IsADirectory => std::fs::remove_dir(path),
        Err(e) => Err(e),
    }
}

fn remove_link(path: &Path) -> io::Result<()> {
    unlink(path).or_else(|_| std::fs::remove_dir(path))
}

fn mount_inside() -> io::Error {
    io::Error::other(text("mount_inside"))
}

/// Delete a folder bottom-up; returns (first error, cancelled, anything deleted).
fn remove_tree(top: &Path, cancel: Option<CancelFn<'_>>) -> (Option<io::Error>, bool, bool) {
    let mut first: Option<io::Error> = None;
    let mut deleted = false;
    #[cfg(unix)]
    let dev = {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(top) {
            Ok(m) => m.dev(),
            Err(e) => return (Some(e), false, false),
        }
    };
    let mut stack: Vec<(PathBuf, bool)> = vec![(top.to_path_buf(), false)];
    while let Some((folder, listed)) = stack.pop() {
        if listed {
            match std::fs::remove_dir(&folder) {
                Ok(()) => deleted = true,
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
            continue;
        }
        if cancelled(cancel) {
            return (first, true, deleted);
        }
        stack.push((folder.clone(), true));
        let entries: Vec<PathBuf> = match std::fs::read_dir(&folder) {
            Ok(it) => it.flatten().map(|e| e.path()).collect(),
            Err(e) => {
                first.get_or_insert(e);
                continue;
            }
        };
        for entry in entries {
            let step = (|| -> io::Result<()> {
                let meta = std::fs::symlink_metadata(&entry)?;
                if is_link_like(&meta) {
                    // symlink, junction: the link only, never what it points to
                    if cfg!(windows) && super::paths::is_mount(&entry) {
                        return Err(mount_inside());
                    }
                    remove_link(&entry)?;
                    deleted = true;
                } else if meta.is_dir() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if meta.dev() != dev {
                            return Err(mount_inside());
                        }
                    }
                    stack.push((entry.clone(), false));
                } else {
                    unlink(&entry)?;
                    deleted = true;
                }
                Ok(())
            })();
            if let Err(e) = step {
                first.get_or_insert(e);
            }
        }
    }
    (first, false, deleted)
}

fn delete_one(path: &str, p: &Path, cancel: Option<CancelFn<'_>>) -> OpResult {
    let run = || -> io::Result<Option<OpResult>> {
        let meta = std::fs::symlink_metadata(p)?;
        if is_link_like(&meta) {
            remove_link(p)?;
        } else if meta.is_dir() {
            let (error, stopped, deleted) = remove_tree(p, cancel);
            if stopped {
                return Ok(Some(result_msg(
                    path,
                    OpCode::Cancelled,
                    if deleted {
                        text("cancelled_partly")
                    } else {
                        ""
                    },
                )));
            }
            if let Some(e) = error {
                if !deleted {
                    return Ok(Some(os_error(path, &e)));
                }
                return Ok(Some(result_msg(
                    path,
                    OpCode::Error,
                    &fill(text("partly"), &[("detail", strerror(&e).into())]),
                )));
            }
        } else {
            unlink(p)?;
        }
        Ok(None)
    };
    match run() {
        Ok(Some(r)) => r,
        Ok(None) => result_msg(path, OpCode::Ok, text("deleted")),
        Err(e) => os_error(path, &e),
    }
}

// --------------------------------------------------------------------------- Windows

/// `$I` record → (size, deletion time as unix seconds, original path).
///
/// Version 1 (Vista–8.1): 8-byte version, size, FILETIME, then the path in 260 UTF-16 units;
/// version 2 (Windows 10+): the same header, a 4-byte length in units, then the path.
pub fn parse_recycle_info(data: &[u8]) -> Result<(i64, f64, String), String> {
    if data.len() < 24 {
        return Err("record too short".into());
    }
    let q = |at: usize| i64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    let (version, size, filetime) = (q(0), q(8), q(16));
    let raw: &[u8] = match version {
        1 => &data[24..data.len().min(24 + 520)],
        2 => {
            if data.len() < 28 {
                return Err("record too short".into());
            }
            let units = i32::from_le_bytes(data[24..28].try_into().unwrap()).max(0) as usize;
            &data[28..data.len().min(28 + 2 * units)]
        }
        v => return Err(format!("unknown record version {v}")),
    };
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    let path = String::from_utf16_lossy(&units);
    if path.is_empty() {
        return Err("record has no path".into());
    }
    Ok((
        size,
        (filetime - 116_444_736_000_000_000) as f64 / 1e7,
        path,
    ))
}

/// `$R…` path in the bin → its `$I…` record.
#[cfg_attr(not(windows), allow(dead_code))]
fn info_of(recycled: &str) -> String {
    let p = Path::new(recycled);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rest: String = name.chars().skip(2).collect();
    p.with_file_name(format!("$I{rest}"))
        .to_string_lossy()
        .into_owned()
}

/// The result of a failed shell deletion.
pub fn hr_result(path: &str, hr: i32) -> OpResult {
    let value = hr as u32;
    let win32 = value >> 16 == 0x8007;
    let code = if win32 { value & 0xFFFF } else { value };
    if matches!(value, 0x8027_0027 | 0x8027_0028) || (win32 && matches!(code, 32 | 33)) {
        return result_msg(path, OpCode::Locked, text("locked_unknown"));
    }
    if matches!(value, 0x8027_0002 | 0x8027_0021 | 0x8027_0022 | 0x8027_003F)
        || (win32 && code == 5)
    {
        return result(path, OpCode::Denied);
    }
    if win32 && matches!(code, 2 | 3) {
        return result(path, OpCode::Missing);
    }
    if matches!(value, 0x8027_0036 | 0x8027_0037 | 0x8027_0038 | 0x8027_003A) {
        // the engine: cannot recycle
        return result_msg(path, OpCode::NoTrash, text("no_bin_volume"));
    }
    if matches!(value, 0x8027_0000 | 0x8027_0001 | 0x8000_4004) {
        return result(path, OpCode::Cancelled);
    }
    result_msg(
        path,
        OpCode::Error,
        &fill(
            text("error"),
            &[("detail", format!("0x{value:08X}").into())],
        ),
    )
}

#[cfg(windows)]
struct Bin {
    root: String,
    /// `$Recycle.Bin\<SID>` on it
    folder: String,
    capacity: u64,
}

/// The recycle bin for `path`, or why there is none.
#[cfg(windows)]
fn win_bin(path: &str) -> io::Result<Result<Bin, &'static str>> {
    use super::win;
    let root = win::volume_root(path)?;
    let kind = win::drive_type(&root);
    if kind == win::DRIVE_REMOTE {
        return Ok(Err(text("no_bin_remote")));
    }
    if kind == win::DRIVE_REMOVABLE {
        return Ok(Err(text("no_bin_removable")));
    }
    let guid = if kind == win::DRIVE_FIXED {
        win::volume_guid(&root)
    } else {
        String::new()
    };
    if guid.is_empty() {
        return Ok(Err(text("no_bin_volume")));
    }
    let total = win::disk_total(&root)?;
    let (nuke, max_bytes) = match *seams::BIN_SETTINGS.lock() {
        Some(f) => f(&guid, total),
        None => {
            let s = win::bin_settings(&guid, total);
            (s.nuke, s.max_bytes)
        }
    };
    if nuke || max_bytes == 0 {
        return Ok(Err(text("no_bin_nuke")));
    }
    let folder = Path::new(&root)
        .join("$Recycle.Bin")
        .join(win::user_sid()?)
        .to_string_lossy()
        .into_owned();
    Ok(Ok(Bin {
        root,
        folder,
        capacity: max_bytes,
    }))
}

#[cfg(windows)]
fn win_status(path: &str, usage: bool) -> io::Result<TrashStatus> {
    match win_bin(path)? {
        Err(reason) => Ok(TrashStatus::unavailable(reason, false)),
        Ok(bin) => Ok(TrashStatus {
            available: true,
            reason: String::new(),
            location: bin.folder.clone(),
            used: if usage {
                super::win::bin_usage(&bin.root).0
            } else {
                -1
            },
            capacity: bin.capacity as i64,
            same_volume: true,
        }),
    }
}

/// Path length in UTF-16 units (what MAX_PATH counts).
#[cfg_attr(not(windows), allow(dead_code))]
fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// (bytes, too long) of an item, walking a folder until it passes `limit`. "Too long": some
/// path inside would pass MAX_PATH where it is now, or once it is renamed to `$R??????`.
#[cfg(windows)]
fn measure(path: &Path, limit: u64, bin_folder: &str) -> io::Result<(u64, bool)> {
    let max = seams::max_path();
    let meta = std::fs::symlink_metadata(path)?;
    let shown = path.to_string_lossy();
    if !meta.is_dir() || is_link_like(&meta) {
        return Ok((meta.len(), units(&shown) > max));
    }
    let base = units(&shown);
    let ext = path
        .extension()
        .map(|e| units(&e.to_string_lossy()) + 1)
        .unwrap_or(0);
    let in_bin = units(bin_folder) + 1 + 8 + ext; // \$R + 6 characters + ext
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let Ok(it) = std::fs::read_dir(&folder) else {
            continue; // what cannot be read here cannot be moved either: the engine reports it
        };
        for entry in it.flatten() {
            let full = units(&entry.path().to_string_lossy());
            if full > max || in_bin + full - base > max {
                return Ok((total, true));
            }
            let Ok(m) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if m.is_dir() && !is_link_like(&m) {
                stack.push(entry.path());
            } else {
                total += m.len();
            }
            if total > limit {
                return Ok((total, false));
            }
        }
    }
    Ok((total, false))
}

#[cfg(windows)]
fn win_key(path: &str) -> String {
    super::win::path_key(path)
}

/// Recycled items the shell reported under another spelling of their path: matched through the
/// original path recorded in their `$I` file (index → `$R` path).
#[cfg(windows)]
fn match_unmatched(
    unmatched: &[(String, Option<String>)],
    where_: &HashMap<String, usize>,
) -> HashMap<usize, String> {
    let mut found = HashMap::new();
    for (reported, recycled) in unmatched {
        let Some(recycled) = recycled else { continue };
        let original = std::fs::read(info_of(recycled))
            .ok()
            .and_then(|d| parse_recycle_info(&d[..d.len().min(1 << 16)]).ok())
            .map(|r| r.2)
            .unwrap_or_else(|| reported.clone());
        let mut hit = None;
        for candidate in [&original, reported] {
            let at = where_.get(&win_key(candidate)).or_else(|| {
                let real = resolve_lenient(Path::new(candidate));
                where_.get(&win_key(&real.to_string_lossy()))
            });
            if let Some(&at) = at {
                hit = Some(at);
                break;
            }
        }
        match hit {
            Some(at) => {
                found.insert(at, recycled.clone());
            }
            None => tracing::error!(
                "recycle: {reported} landed in the bin as {recycled} but matches no request"
            ),
        }
    }
    found
}

#[cfg(windows)]
fn win_trash(
    run: &mut Run<'_>,
    todo: &[usize],
    sizes: &HashMap<String, i64>,
    cancel: Option<CancelFn<'_>>,
) {
    use super::fmt_size;
    use super::win;
    let max = seams::max_path();
    let mut bins: HashMap<String, Result<std::sync::Arc<Bin>, &'static str>> = HashMap::new();
    let mut ready: Vec<(usize, std::sync::Arc<Bin>, u64)> = Vec::new();
    for &i in todo {
        let path = run.paths[i].clone();
        let os = run.os[i].clone();
        let found = (|| -> io::Result<Result<std::sync::Arc<Bin>, &'static str>> {
            let root = win::volume_root(&path)?;
            if let Some(f) = bins.get(&root) {
                return Ok(f.clone());
            }
            let f = win_bin(&path)?.map(std::sync::Arc::new);
            bins.insert(root, f.clone());
            Ok(f)
        })();
        let bin = match found {
            Err(e) => {
                run.set(i, os_error(&path, &e));
                continue;
            }
            Ok(Err(reason)) => {
                run.set(i, result_msg(&path, OpCode::NoTrash, reason));
                continue;
            }
            Ok(Ok(bin)) => bin,
        };
        let limit = (bin.capacity as f64 * CAPACITY_SHARE) as u64;
        if units(&path) > max {
            run.set(i, result_msg(&path, OpCode::NoTrash, text("too_long")));
            continue;
        }
        let hint = sizes.get(&path).copied();
        let measured = (|| -> io::Result<(u64, bool)> {
            let meta = std::fs::symlink_metadata(&os)?;
            let is_dir = meta.is_dir() && !is_link_like(&meta);
            match hint {
                Some(h) if is_dir && (h as f64) <= bin.capacity as f64 * TRUST_SHARE => {
                    Ok((h.max(0) as u64, false)) // far from the limit; the engine guard covers the rest
                }
                _ => measure(&os, limit, &bin.folder),
            }
        })();
        match measured {
            Err(e) => run.set(i, os_error(&path, &e)),
            Ok((_, true)) => run.set(i, result_msg(&path, OpCode::NoTrash, text("too_long"))),
            Ok((size, false)) if size > limit => {
                let msg = fill(
                    text("too_big"),
                    &[
                        ("size", fmt_size(size).into()),
                        ("limit", fmt_size(bin.capacity).into()),
                    ],
                );
                run.set(i, result_msg(&path, OpCode::NoTrash, &msg));
            }
            Ok((size, false)) => ready.push((i, bin, size)),
        }
    }

    // consecutive items on one volume, bounded in count and bytes, per shell operation
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let (mut volume, mut used) = (String::new(), 0u64);
    for (i, bin, size) in &ready {
        if !current.is_empty()
            && (bin.root != volume
                || current.len() >= BATCH_ITEMS
                || (used + size) as f64 > bin.capacity as f64 * BATCH_SHARE)
        {
            batches.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(*i);
        volume = bin.root.clone();
        used += size;
    }
    if !current.is_empty() {
        batches.push(current);
    }

    for batch in batches {
        let mut pending = batch;
        while !pending.is_empty() && !cancelled(cancel) {
            let where_: HashMap<String, usize> = pending
                .iter()
                .map(|&i| (win_key(&run.paths[i]), i))
                .collect();
            let batch_paths: Vec<PathBuf> = pending.iter().map(|&i| run.os[i].clone()).collect();
            let outcome = {
                let run_ref = &mut *run;
                let pending_ref = &pending;
                // as each item lands
                let mut on_item = |k: usize, entry: &win::Recycled| {
                    if let Some(recycled) = &entry.recycled {
                        let at = pending_ref[k];
                        let p = run_ref.paths[at].clone();
                        run_ref.set(
                            at,
                            OpResult::new(&p, OpCode::Ok, text("trashed"), &info_of(recycled)),
                        );
                    }
                };
                win::recycle(&batch_paths, cancel, &mut on_item)
            };
            let outcome = match outcome {
                Ok(o) => o,
                Err(e) => {
                    for &i in &pending {
                        let p = run.paths[i].clone();
                        run.set(i, os_error(&p, &e));
                    }
                    break;
                }
            };
            let late = match_unmatched(&outcome.unmatched, &where_);
            let refused: Vec<String> = outcome.refused.iter().map(|p| win_key(p)).collect();
            let mut retry: Vec<usize> = Vec::new();
            for (&i, entry) in pending.iter().zip(&outcome.items) {
                let path = run.paths[i].clone();
                let key = win_key(&path);
                let recycled = entry.recycled.clone().or_else(|| late.get(&i).cloned());
                if let Some(r) = recycled {
                    run.set(
                        i,
                        OpResult::new(&path, OpCode::Ok, text("trashed"), &info_of(&r)),
                    );
                } else if entry.refused
                    || refused
                        .iter()
                        .any(|r| *r == key || r.starts_with(&format!("{key}\\")))
                {
                    run.set(i, result_msg(&path, OpCode::NoTrash, text("refused")));
                } else if outcome.cancelled && entry.hr < 0 {
                    continue; // finish() reports it cancelled
                } else if !entry.reached && !refused.is_empty() {
                    retry.push(i); // the operation stopped at a refused item before this one
                } else if entry.hr >= 0 && !lexists(&run.os[i]) {
                    run.set(i, result_msg(&path, OpCode::Error, text("destroyed")));
                    tracing::error!("recycle: {path} vanished without a recycle-bin entry");
                } else if entry.hr >= 0 {
                    run.set(i, result_msg(&path, OpCode::Error, text("not_moved")));
                } else {
                    run.set(i, hr_result(&path, entry.hr));
                }
            }
            if !retry.is_empty() && retry.len() < pending.len() {
                pending = retry; // something was settled: go on with the rest
            } else {
                for i in retry {
                    let p = run.paths[i].clone();
                    run.set(i, result_msg(&p, OpCode::Error, text("not_moved")));
                }
                pending = Vec::new();
            }
        }
        if cancelled(cancel) {
            return;
        }
    }
}

#[cfg(windows)]
fn win_restore(r: &str) -> io::Result<OpResult> {
    use super::paths::nt_normcase;
    use super::win;
    let info = super::paths::absolute(Path::new(r));
    let folder = info.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = info
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bin_dir = folder.parent().map(Path::to_path_buf).unwrap_or_default();
    let base = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let volume_ok = || -> bool {
        let Some(top) = bin_dir.parent() else {
            return false;
        };
        let top = top.to_string_lossy();
        let top = format!("{}\\", top.trim_end_matches('\\'));
        win::volume_root(&info.to_string_lossy())
            .map(|root| nt_normcase(&top) == nt_normcase(&root))
            .unwrap_or(false)
    };
    let valid = name.len() > 2
        && name[..2].eq_ignore_ascii_case("$I")
        && win::user_sid().is_ok_and(|sid| base(&folder).eq_ignore_ascii_case(&sid))
        && base(&bin_dir).eq_ignore_ascii_case("$recycle.bin")
        && volume_ok();
    if !valid {
        return Ok(result_msg(r, OpCode::Error, text("bad_ref")));
    }
    let original = match std::fs::read(&info) {
        Ok(d) => match parse_recycle_info(&d[..d.len().min(1 << 16)]) {
            Ok((_, _, p)) => p,
            Err(_) => return Ok(result_msg(r, OpCode::Error, text("bad_ref"))),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(result_msg(r, OpCode::Missing, text("gone_from_trash")));
        }
        Err(e) => return Err(e),
    };
    let payload = folder.join(format!("$R{}", &name[2..]));
    if std::fs::symlink_metadata(&payload).is_err() {
        return Ok(result_msg(
            &original,
            OpCode::Missing,
            text("gone_from_trash"),
        ));
    }
    if lexists(&original) {
        return Ok(result(&original, OpCode::Exists));
    }
    if let Some(parent) = Path::new(&original).parent() {
        std::fs::create_dir_all(parent)?;
    }
    // MoveFileEx without REPLACE_EXISTING: never overwrites
    if let Err(e) = rename_noreplace(&payload, Path::new(&original)) {
        if e.kind() == io::ErrorKind::AlreadyExists {
            return Ok(result(&original, OpCode::Exists));
        }
        return Ok(os_error(&original, &e));
    }
    if let Err(e) = std::fs::remove_file(&info) {
        // Explorer drops a record whose $R is gone; nothing is lost
        tracing::warn!("restore: could not remove {}: {e}", info.display());
    }
    let folder_now = std::fs::metadata(&original).is_ok_and(|m| m.is_dir());
    win::notify_changed(&original, folder_now);
    Ok(OpResult::new(&original, OpCode::Ok, text("restored"), r))
}

/// Rename that fails with `AlreadyExists` instead of replacing `dst`.
///
/// Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` never replaces. Linux:
/// `renameat2(RENAME_NOREPLACE)`; macOS: `renamex_np(RENAME_EXCL)`; where neither is available,
/// a check right before `rename`.
pub fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let (s, d) = (wide_os(src), wide_os(dst));
        // SAFETY: NUL-terminated strings; flags 0: no replace, no copy across volumes
        if unsafe { MoveFileExW(s.as_ptr(), d.as_ptr(), 0) } == 0 {
            let e = io::Error::last_os_error();
            if matches!(e.raw_os_error(), Some(80) | Some(183)) {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, e));
            }
            return Err(e);
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        match native_noreplace(src, dst) {
            Some(r) => r,
            None => {
                if std::fs::symlink_metadata(dst).is_ok() {
                    return Err(io::Error::from_raw_os_error(libc::EEXIST));
                }
                std::fs::rename(src, dst)
            }
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        if std::fs::symlink_metadata(dst).is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        std::fs::rename(src, dst)
    }
}

#[cfg(windows)]
fn wide_os(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let s = p.to_string_lossy();
    let long = names::long_path(&s);
    if long.len() != s.len() {
        return std::ffi::OsStr::new(&long)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
    }
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A no-replace rename: Some(result), or None when this file system cannot.
#[cfg(unix)]
fn native_noreplace(src: &Path, dst: &Path) -> Option<io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let s = CString::new(src.as_os_str().as_bytes()).ok()?;
    let d = CString::new(dst.as_os_str().as_bytes()).ok()?;
    #[cfg(target_os = "macos")]
    let rc = {
        unsafe extern "C" {
            fn renamex_np(
                from: *const libc::c_char,
                to: *const libc::c_char,
                flags: libc::c_uint,
            ) -> libc::c_int;
        }
        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        // SAFETY: NUL-terminated strings.
        unsafe { renamex_np(s.as_ptr(), d.as_ptr(), RENAME_EXCL) as libc::c_long }
    };
    #[cfg(target_os = "linux")]
    let rc = {
        const RENAME_NOREPLACE: libc::c_uint = 1;
        // SAFETY: NUL-terminated strings; AT_FDCWD for both.
        unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                s.as_ptr(),
                libc::AT_FDCWD,
                d.as_ptr(),
                RENAME_NOREPLACE,
            )
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let rc: libc::c_long = {
        let _ = (&s, &d);
        return None;
    };
    if rc == 0 {
        return Some(Ok(()));
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP) => None,
        _ => Some(Err(e)),
    }
}

// --------------------------------------------------------------------------- Linux (freedesktop)

/// `$XDG_DATA_HOME/Trash` (`~/.local/share/Trash` when unset or not absolute).
pub fn home_trash() -> PathBuf {
    let xdg = std::env::var("XDG_DATA_HOME").unwrap_or_default();
    let base = if Path::new(&xdg).is_absolute() && !xdg.is_empty() {
        PathBuf::from(xdg)
    } else {
        crate::paths::home_dir().join(".local").join("share")
    };
    base.join("Trash")
}

/// `Path=` of a `.trashinfo` (decoded; may be relative to the trash's topdir).
pub fn parse_trashinfo(text: &str) -> Result<Vec<u8>, String> {
    let mut section = "";
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line;
        } else if section == "[Trash Info]"
            && let Some(v) = line.strip_prefix("Path=")
        {
            return Ok(unquote(v));
        }
    }
    Err("no Path in [Trash Info]".into())
}

/// `urllib.parse.unquote_to_bytes`.
fn unquote(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `name` shortened (keeping its extension) so that it plus `extra` bytes fits NAME_MAX.
pub fn fit_name(name: &[u8], extra: usize) -> Vec<u8> {
    let limit = 255usize.saturating_sub(extra);
    if name.len() <= limit {
        return name.to_vec();
    }
    let dot = name.iter().rposition(|&c| c == b'.').filter(|&d| d > 0);
    let (stem, ext) = match dot {
        Some(d) => (&name[..d], &name[d..]),
        None => (name, &b""[..]),
    };
    if limit > ext.len() {
        let keep = limit - ext.len();
        let mut cut = stem[..keep.max(1).min(stem.len())].to_vec();
        // never end in the middle of a UTF-8 sequence
        while let Some(&last) = cut.last() {
            if std::str::from_utf8(&cut).is_ok() || last < 0x80 {
                break;
            }
            cut.pop();
        }
        cut.extend_from_slice(ext);
        cut
    } else {
        name[..limit].to_vec()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
use fd::{fd_restore, fd_status, fd_trash};

#[cfg(all(unix, not(target_os = "macos")))]
pub mod fd {
    //! The freedesktop.org trash (Linux).
    use std::ffi::OsStr;
    use std::io::{self, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use super::*;

    pub(crate) struct FdTrash {
        /// holds files/ and info/
        pub root: PathBuf,
        /// None for the home trash (absolute Path=); else Path= is relative to it
        pub topdir: Option<PathBuf>,
    }

    /// Test seams: where a path's device and mount top are, whether a trash folder is usable.
    #[doc(hidden)]
    pub mod seams {
        use parking_lot::Mutex;
        use std::path::Path;
        pub type ExistingDev = fn(&Path) -> std::io::Result<u64>;
        pub type MountTop = fn(&Path, u64) -> std::path::PathBuf;
        pub type UsableDir = fn(&Path, u32, bool) -> bool;
        pub static EXISTING_DEV: Mutex<Option<ExistingDev>> = Mutex::new(None);
        pub static MOUNT_TOP: Mutex<Option<MountTop>> = Mutex::new(None);
        pub static USABLE_DIR: Mutex<Option<UsableDir>> = Mutex::new(None);
    }

    fn existing_dev(path: &Path) -> io::Result<u64> {
        if let Some(f) = *seams::EXISTING_DEV.lock() {
            return f(path);
        }
        let mut p = path.to_path_buf();
        loop {
            match std::fs::metadata(&p) {
                Ok(m) => return Ok(m.dev()),
                Err(e) => match p.parent() {
                    Some(up) if up != p => p = up.to_path_buf(),
                    _ => return Err(e),
                },
            }
        }
    }

    /// The highest folder above `path` on the same file system (its mount point).
    fn mount_top(path: &Path, dev: u64) -> PathBuf {
        if let Some(f) = *seams::MOUNT_TOP.lock() {
            return f(path, dev);
        }
        let abs = super::super::paths::absolute(path);
        let mut cur = resolve_lenient(abs.parent().unwrap_or(Path::new("/")));
        loop {
            let Some(up) = cur.parent().map(Path::to_path_buf) else {
                return cur;
            };
            match std::fs::symlink_metadata(&up) {
                Ok(m) if m.dev() == dev => cur = up,
                _ => return cur,
            }
        }
    }

    fn uid() -> u32 {
        // SAFETY: always succeeds.
        unsafe { libc::getuid() }
    }

    /// A trash folder that is a real folder owned by `uid` (made with mode 0700 if `create`).
    fn usable_dir(path: &Path, uid: u32, create: bool) -> bool {
        if let Some(f) = *seams::USABLE_DIR.lock() {
            return f(path, uid, create);
        }
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !create {
                    let parent = path.parent().unwrap_or(Path::new("/"));
                    let c =
                        std::ffi::CString::new(parent.as_os_str().as_bytes()).unwrap_or_default();
                    // SAFETY: a NUL-terminated path.
                    return unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } == 0;
                }
                if std::fs::DirBuilder::new().mode(0o700).create(path).is_err() {
                    return false;
                }
                match std::fs::symlink_metadata(path) {
                    Ok(m) => m,
                    Err(_) => return false,
                }
            }
            Err(_) => return false,
        };
        meta.is_dir() && meta.uid() == uid
    }

    fn make_subdirs(root: &Path) -> io::Result<()> {
        for sub in ["files", "info"] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root.join(sub))?;
        }
        Ok(())
    }

    fn ready(trash: FdTrash, create: bool) -> Result<FdTrash, &'static str> {
        if create && make_subdirs(&trash.root).is_err() {
            return Err(text("no_trash_dir"));
        }
        Ok(trash)
    }

    /// The trash for `path` (freedesktop spec), or why there is none.
    pub(crate) fn find(path: &Path, create: bool) -> io::Result<Result<FdTrash, &'static str>> {
        let dev = std::fs::symlink_metadata(path)?.dev();
        let home = home_trash();
        if existing_dev(&home)? == dev {
            if create {
                make_subdirs(&home)?;
            }
            return Ok(Ok(FdTrash {
                root: home,
                topdir: None,
            }));
        }
        let uid = uid();
        let top = mount_top(path, dev);
        let admin = top.join(".Trash");
        if let Ok(m) = std::fs::symlink_metadata(&admin)
            && m.is_dir()
            && m.permissions().mode() & libc::S_ISVTX as u32 != 0
        {
            // not a link, sticky: usable
            let mine = admin.join(uid.to_string());
            if usable_dir(&mine, uid, create) {
                return Ok(ready(
                    FdTrash {
                        root: mine,
                        topdir: Some(top),
                    },
                    create,
                ));
            }
        }
        let mine = top.join(format!(".Trash-{uid}"));
        if usable_dir(&mine, uid, create) {
            return Ok(ready(
                FdTrash {
                    root: mine,
                    topdir: Some(top),
                },
                create,
            ));
        }
        Ok(Err(text("no_trash_dir")))
    }

    pub(crate) fn fd_status(path: &str, usage: bool) -> io::Result<TrashStatus> {
        match find(Path::new(path), false)? {
            Err(reason) => Ok(TrashStatus::unavailable(reason, false)),
            Ok(t) => Ok(TrashStatus {
                available: true,
                reason: String::new(),
                location: t.root.to_string_lossy().into_owned(),
                used: if usage {
                    tree_bytes(&t.root.join("files"))
                } else {
                    -1
                },
                capacity: 0,
                same_volume: true,
            }),
        }
    }

    fn tree_bytes(path: &Path) -> i64 {
        let (mut total, mut seen) = (0i64, 0usize);
        let mut stack = vec![path.to_path_buf()];
        while let Some(folder) = stack.pop() {
            let Ok(it) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in it.flatten() {
                seen += 1;
                if seen > TRASH_USAGE_LIMIT {
                    return -1;
                }
                let Ok(m) = std::fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if m.is_dir() {
                    stack.push(entry.path());
                }
                let blocks = m.blocks() as i64 * 512;
                total += if blocks > 0 { blocks } else { m.len() as i64 };
            }
        }
        total
    }

    fn relpath(path: &Path, base: &Path) -> PathBuf {
        let p: Vec<_> = path.components().collect();
        let b: Vec<_> = base.components().collect();
        let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let mut out = PathBuf::new();
        for _ in common..b.len() {
            out.push("..");
        }
        for c in &p[common..] {
            out.push(c);
        }
        out
    }

    /// `path` as shown (results), `os` as given (the file system's bytes).
    pub(crate) fn fd_trash(path: &str, os: &Path) -> OpResult {
        let found = match find(os, true) {
            Ok(f) => f,
            Err(e) => return os_error(path, &e),
        };
        let trash = match found {
            Ok(t) => t,
            Err(reason) => return result_msg(path, OpCode::NoTrash, reason),
        };
        let original = super::super::paths::absolute(os);
        let recorded = match &trash.topdir {
            Some(top) => {
                let real = resolve_lenient(original.parent().unwrap_or(Path::new("/")))
                    .join(original.file_name().unwrap_or_default());
                relpath(&real, top)
            }
            None => original.clone(),
        };
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            super::super::paths::quote_path(recorded.as_os_str().as_bytes()),
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
        );
        let base: Vec<u8> = original
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| b"item".to_vec());
        let dot = base.iter().rposition(|&c| c == b'.').filter(|&d| d > 0);
        let (stem, ext) = match dot {
            Some(d) => (&base[..d], &base[d..]),
            None => (&base[..], &b""[..]),
        };
        for n in 1..10_000 {
            let candidate = if n == 1 {
                base.clone()
            } else {
                [stem, format!(".{n}").as_bytes(), ext].concat()
            };
            let name = fit_name(&candidate, ".trashinfo".len());
            let mut info_name = name.clone();
            info_name.extend_from_slice(b".trashinfo");
            let info = trash.root.join("info").join(OsStr::from_bytes(&info_name));
            let target = trash.root.join("files").join(OsStr::from_bytes(&name));
            let mut fh = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&info)
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return os_error(path, &e),
            };
            if let Err(e) = fh.write_all(body.as_bytes()) {
                drop(fh);
                let _ = std::fs::remove_file(&info);
                return os_error(path, &e);
            }
            drop(fh);
            if std::fs::symlink_metadata(&target).is_ok() {
                // a left-over without its info file: pick another name
                let _ = std::fs::remove_file(&info);
                continue;
            }
            if let Err(e) = rename_noreplace(os, &target) {
                let _ = std::fs::remove_file(&info);
                match e.raw_os_error() {
                    Some(libc::EEXIST) => continue,
                    Some(libc::EXDEV) => {
                        return result_msg(path, OpCode::NoTrash, text("cross_mount"));
                    }
                    _ => return os_error(path, &e),
                }
            }
            return OpResult::new(path, OpCode::Ok, text("trashed"), &info.to_string_lossy());
        }
        result_msg(path, OpCode::Error, text("no_name"))
    }

    pub(crate) fn fd_restore(r: &str) -> io::Result<OpResult> {
        let info = super::super::paths::absolute(Path::new(r));
        let info_dir = info.parent().map(Path::to_path_buf).unwrap_or_default();
        let root = info_dir.parent().map(Path::to_path_buf).unwrap_or_default();
        let uid = uid();
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        if !info.to_string_lossy().ends_with(".trashinfo") || name(&info_dir) != "info" {
            return Ok(result_msg(r, OpCode::Error, text("bad_ref")));
        }
        let norm = |p: &Path| super::super::paths::posix_normpath(&p.to_string_lossy());
        let topdir: Option<PathBuf> = if norm(&root) == norm(&home_trash()) {
            None
        } else if name(&root) == format!(".Trash-{uid}") {
            root.parent().map(Path::to_path_buf)
        } else if name(&root) == uid.to_string()
            && root.parent().is_some_and(|p| name(p) == ".Trash")
        {
            root.parent().and_then(Path::parent).map(Path::to_path_buf)
        } else {
            return Ok(result_msg(r, OpCode::Error, text("bad_ref")));
        };
        let recorded = match std::fs::read(&info) {
            Ok(d) => match parse_trashinfo(&String::from_utf8_lossy(&d[..d.len().min(1 << 16)])) {
                Ok(p) => p,
                Err(_) => return Ok(result_msg(r, OpCode::Error, text("bad_ref"))),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(result_msg(r, OpCode::Missing, text("gone_from_trash")));
            }
            Err(e) => return Err(e),
        };
        let recorded = PathBuf::from(OsStr::from_bytes(&recorded));
        let original = if recorded.is_absolute() {
            recorded
        } else {
            topdir.unwrap_or_else(|| PathBuf::from("/")).join(recorded)
        };
        let original = PathBuf::from(norm(&original));
        let original_s = original.to_string_lossy().into_owned();
        let file = info
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        let payload = root
            .join("files")
            .join(OsStr::from_bytes(&file[..file.len() - ".trashinfo".len()]));
        if std::fs::symlink_metadata(&payload).is_err() {
            return Ok(result_msg(
                &original_s,
                OpCode::Missing,
                text("gone_from_trash"),
            ));
        }
        if std::fs::symlink_metadata(&original).is_ok() {
            return Ok(result(&original_s, OpCode::Exists));
        }
        if let Some(parent) = original.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = rename_noreplace(&payload, &original) {
            if e.raw_os_error() == Some(libc::EEXIST) {
                return Ok(result(&original_s, OpCode::Exists));
            }
            return Ok(os_error(&original_s, &e));
        }
        if let Err(e) = std::fs::remove_file(&info) {
            tracing::warn!("restore: could not remove {}: {e}", info.display());
        }
        Ok(OpResult::new(&original_s, OpCode::Ok, text("restored"), r))
    }
}

// --------------------------------------------------------------------------- macOS (untested)

#[cfg(target_os = "macos")]
fn mac_status(path: &str) -> io::Result<TrashStatus> {
    let remote = crate::volume::volume_for(Path::new(path)).is_ok_and(|v| v.is_remote);
    if remote {
        return Ok(TrashStatus::unavailable(text("no_bin_remote"), false));
    }
    Ok(TrashStatus {
        available: true,
        reason: String::new(),
        location: crate::paths::home_dir()
            .join(".Trash")
            .to_string_lossy()
            .into_owned(),
        used: -1,
        capacity: 0,
        same_volume: true,
    })
}

/// `NSFileManager trashItemAtURL:resultingItemURL:error:`.
#[cfg(target_os = "macos")]
fn mac_trash(path: &str) -> OpResult {
    use objc2::rc::autoreleasepool;
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let manager = NSFileManager::defaultManager();
        let mut out: Option<objc2::rc::Retained<NSURL>> = None;
        // SAFETY: valid objects; the out pointer lives through the call.
        let done = unsafe { manager.trashItemAtURL_resultingItemURL_error(&url, Some(&mut out)) };
        match done {
            Err(err) => {
                let detail = err.localizedDescription().to_string();
                result_msg(
                    path,
                    OpCode::Error,
                    &fill(text("error"), &[("detail", detail.into())]),
                )
            }
            Ok(()) => {
                let landed = out
                    .and_then(|u| u.path())
                    .map(|p| p.to_string())
                    .unwrap_or_default();
                let original = super::paths::absolute(Path::new(path))
                    .to_string_lossy()
                    .into_owned();
                let r = serde_json::json!({"trashed": landed, "original": original}).to_string();
                OpResult::new(path, OpCode::Ok, text("trashed"), &r)
            }
        }
    })
}

#[cfg(target_os = "macos")]
fn mac_restore(r: &str) -> io::Result<OpResult> {
    let parsed: Option<(String, String)> = serde_json::from_str::<serde_json::Value>(r)
        .ok()
        .and_then(|v| {
            Some((
                v.get("trashed")?.as_str()?.to_string(),
                v.get("original")?.as_str()?.to_string(),
            ))
        });
    let Some((trashed, original)) = parsed else {
        return Ok(result_msg(r, OpCode::Error, text("bad_ref")));
    };
    let parts: Vec<&str> = trashed.split('/').collect();
    if !parts.contains(&".Trash") && !parts.contains(&".Trashes") {
        return Ok(result_msg(r, OpCode::Error, text("bad_ref")));
    }
    if !lexists(&trashed) {
        return Ok(result_msg(
            &original,
            OpCode::Missing,
            text("gone_from_trash"),
        ));
    }
    if lexists(&original) {
        return Ok(result(&original, OpCode::Exists));
    }
    if let Some(parent) = Path::new(&original).parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Err(e) = rename_noreplace(Path::new(&trashed), Path::new(&original)) {
        if e.raw_os_error() == Some(libc::EEXIST) {
            return Ok(result(&original, OpCode::Exists));
        }
        return Ok(os_error(&original, &e));
    }
    Ok(OpResult::new(&original, OpCode::Ok, text("restored"), r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hresults_map_to_codes() {
        let cases: &[(u32, OpCode)] = &[
            (0x8027_0027, OpCode::Locked),  // COPYENGINE_E_SHARING_VIOLATION_SRC
            (0x8007_0020, OpCode::Locked),  // HRESULT_FROM_WIN32(ERROR_SHARING_VIOLATION)
            (0x8007_0005, OpCode::Denied),  // E_ACCESSDENIED
            (0x8007_0002, OpCode::Missing), // ERROR_FILE_NOT_FOUND
            (0x8027_0037, OpCode::NoTrash), // COPYENGINE_E_RECYCLE_SIZE_TOO_BIG
            (0x8000_4004, OpCode::Cancelled),
            (0x8000_4005, OpCode::Error), // E_FAIL
        ];
        for &(hr, code) in cases {
            assert_eq!(hr_result("p", hr as i32).code, code, "{hr:#x}");
        }
    }

    #[test]
    fn names_fit() {
        assert_eq!(fit_name(b"a.txt", 10), b"a.txt");
        let long = [b"x".repeat(300), b".txt".to_vec()].concat();
        let fit = fit_name(&long, 10);
        assert_eq!(fit.len(), 245);
        assert!(fit.ends_with(b".txt"));
    }

    #[test]
    fn unquoting() {
        assert_eq!(unquote("/a%20b/%E6%96%87.txt"), "/a b/文.txt".as_bytes());
        assert_eq!(unquote("100%"), b"100%");
    }
}
