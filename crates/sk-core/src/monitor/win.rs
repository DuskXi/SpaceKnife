//! Windows watcher: one recursive `ReadDirectoryChangesExW` per scan root (docs/monitor.md §2.1).
//!
//! A folder handle opened for overlapped I/O and one worker thread that keeps a read pending and
//! waits on its event (or on the stop event). Every completed read becomes one batch for the sink;
//! the next read is issued before the batch is decoded, so the kernel keeps buffering meanwhile.
//! The extended information class (Windows 10 1709+) carries each file's size, allocation and
//! last write time, so `modified` changes arrive with numbers; volumes without it fall back to
//! `ReadDirectoryChangesW` (`modified` without numbers, `is_dir` unknown).
//!
//! Lost events are never silent: a read that completes with no bytes (the kernel's buffer
//! overflowed) becomes `overflow` for the root and watching goes on. When the root goes away the
//! watcher reports `failed`, emits `overflow` for the root and ends. A watch that never armed only
//! reports `failed`: nothing was watched, so nothing was lost; the caller falls back to polling.
//!
//! [`NotifyDecoder`] (buffers → changes) is plain code, tested on every platform.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{Change, ChangeKind};

pub const FILE_ACTION_ADDED: u32 = 1;
pub const FILE_ACTION_REMOVED: u32 = 2;
pub const FILE_ACTION_MODIFIED: u32 = 3;
pub const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
pub const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FA_PACKED: u32 = 0x200 | 0x800; // sparse | compressed: the real allocation is not in the event
const FA_RECALL: u32 = 0x40000 | 0x400000; // cloud placeholders

/// `FILE_NOTIFY_EXTENDED_INFORMATION` up to `FileName`.
pub const EXT_HEADER: usize = 84;
/// `FILE_NOTIFY_INFORMATION` up to `FileName`.
pub const STD_HEADER: usize = 12;
const EPOCH_SECS: i64 = 11_644_473_600;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn i64_at(b: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// FILETIME (100 ns since 1601) → Unix seconds; 0 (unknown) stays 0, nothing below 0.
pub fn unix_of_filetime(ft: i64) -> i64 {
    if ft <= 0 {
        0
    } else {
        (ft / 10_000_000 - EPOCH_SECS).max(0)
    }
}

/// Notification buffers → changes. One per watcher: it keeps a rename's old name when the new
/// name has not arrived yet (it comes with the next read).
#[derive(Clone, Debug)]
pub struct NotifyDecoder {
    /// the root (names in the buffers are relative to it)
    pub root: PathBuf,
    /// FILE_NOTIFY_EXTENDED_INFORMATION records (else FILE_NOTIFY_INFORMATION)
    pub extended: bool,
    /// allocations that are not a multiple of it are resident in the MFT: 0
    pub cluster: i64,
    /// RENAMED_OLD_NAME waiting for RENAMED_NEW_NAME
    pub held: Option<(PathBuf, Option<bool>)>,
}

impl NotifyDecoder {
    pub fn new(root: &Path) -> NotifyDecoder {
        NotifyDecoder {
            root: root.to_path_buf(),
            extended: true,
            cluster: 0,
            held: None,
        }
    }

    /// The held old name as `deleted` (its new name never came: the name is gone anyway).
    pub fn flush(&mut self) -> Vec<Change> {
        match self.held.take() {
            Some((path, is_dir)) => vec![Change::new(ChangeKind::Deleted, path, is_dir)],
            None => Vec::new(),
        }
    }

    fn join(&self, units: &[u16]) -> PathBuf {
        #[cfg(windows)]
        let rel: std::ffi::OsString = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(units)
        };
        #[cfg(not(windows))]
        let rel: std::ffi::OsString = String::from_utf16_lossy(units).replace('\\', "/").into();
        self.root.join(rel)
    }

    pub fn decode(&mut self, data: &[u8]) -> Vec<Change> {
        let mut out: Vec<Change> = Vec::new();
        let head = if self.extended {
            EXT_HEADER
        } else {
            STD_HEADER
        };
        let end = data.len();
        let mut off = 0usize;
        let mut modified = 0;
        while off + head <= end {
            let nxt = u32_at(data, off) as usize;
            let action = u32_at(data, off + 4);
            let (ft, alloc, size, attrs, nlen) = if self.extended {
                (
                    i64_at(data, off + 16),
                    i64_at(data, off + 40),
                    i64_at(data, off + 48),
                    u32_at(data, off + 56),
                    u32_at(data, off + 80) as usize,
                )
            } else {
                (0, 0, 0, 0, u32_at(data, off + 8) as usize)
            };
            let start = off + head;
            if start + nlen > end {
                break; // torn record: nothing after it is readable
            }
            let units: Vec<u16> = data[start..start + nlen]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            let path = self.join(&units);
            let is_dir = self
                .extended
                .then_some(attrs & FILE_ATTRIBUTE_DIRECTORY != 0);
            if self.held.is_some() && action != FILE_ACTION_RENAMED_NEW_NAME {
                out.extend(self.flush());
            }
            match action {
                FILE_ACTION_ADDED => out.push(Change::new(ChangeKind::Created, path, is_dir)),
                FILE_ACTION_REMOVED => out.push(Change::new(ChangeKind::Deleted, path, is_dir)),
                FILE_ACTION_MODIFIED => {
                    if !self.extended {
                        out.push(Change::new(ChangeKind::Modified, path, None));
                        modified += 1;
                    } else if is_dir != Some(true) {
                        // a folder's own times changed: its entries tell the rest
                        out.push(Change::modified(
                            path,
                            size.max(0),
                            self.alloc(alloc, attrs),
                            unix_of_filetime(ft),
                        ));
                        modified += 1;
                    }
                }
                FILE_ACTION_RENAMED_OLD_NAME => self.held = Some((path, is_dir)),
                FILE_ACTION_RENAMED_NEW_NAME => {
                    let (old, old_dir) = match self.held.take() {
                        Some((p, d)) => (Some(p), d),
                        None => (None, None),
                    };
                    out.push(Change::renamed(
                        path,
                        old,
                        if is_dir.is_none() { old_dir } else { is_dir },
                    ));
                }
                _ => {}
            }
            if nxt == 0 {
                break;
            }
            off += nxt;
        }
        if modified > 1 {
            // repeated modified for one path: the last one has the newest numbers
            let mut last: HashMap<PathBuf, usize> = HashMap::new();
            for (i, c) in out.iter().enumerate() {
                if c.kind == ChangeKind::Modified {
                    last.insert(c.path.clone(), i);
                }
            }
            out = out
                .into_iter()
                .enumerate()
                .filter(|(i, c)| c.kind != ChangeKind::Modified || last[&c.path] == *i)
                .map(|(_, c)| c)
                .collect();
        }
        out
    }

    fn alloc(&self, alloc: i64, attrs: u32) -> Option<i64> {
        if attrs & FA_PACKED != 0 {
            None // AllocatedLength is not what a compressed or sparse file occupies
        } else if attrs & FA_RECALL != 0 || (self.cluster > 0 && alloc % self.cluster != 0) {
            // a cloud placeholder counts as 0, like the scan does; resident data lives in the MFT
            // record and takes no clusters
            Some(0)
        } else {
            Some(alloc.max(0))
        }
    }
}

/// Encodes notification records (tests of the decoder, on every platform).
pub fn encode_records(records: &[(u32, &str, u32, i64, i64, i64)], extended: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, &(action, name, attrs, size, alloc, ft)) in records.iter().enumerate() {
        let units: Vec<u16> = name.encode_utf16().collect();
        let head = if extended { EXT_HEADER } else { STD_HEADER };
        let len = (head + units.len() * 2 + 7) & !7;
        let at = out.len();
        out.resize(at + len, 0);
        let next = if k + 1 == records.len() {
            0
        } else {
            len as u32
        };
        out[at..at + 4].copy_from_slice(&next.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&action.to_le_bytes());
        let name_at = if extended {
            out[at + 16..at + 24].copy_from_slice(&ft.to_le_bytes());
            out[at + 40..at + 48].copy_from_slice(&alloc.to_le_bytes());
            out[at + 48..at + 56].copy_from_slice(&size.to_le_bytes());
            out[at + 56..at + 60].copy_from_slice(&attrs.to_le_bytes());
            out[at + 80..at + 84].copy_from_slice(&((units.len() * 2) as u32).to_le_bytes());
            at + 84
        } else {
            out[at + 8..at + 12].copy_from_slice(&((units.len() * 2) as u32).to_le_bytes());
            at + 12
        };
        for (i, u) in units.iter().enumerate() {
            out[name_at + 2 * i..name_at + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
    }
    out
}

#[cfg(windows)]
pub use imp::{RdcwWatcher, is_network_path};

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_INCOMPLETE, ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileStandardInfo,
        GetDiskFreeSpaceW, GetDriveTypeW, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
        GetVolumePathNameW, OPEN_EXISTING, ReadDirectoryChangesExW, ReadDirectoryChangesW,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    };

    use super::super::poll::{Emitter, Worker};
    use super::super::{Change, Sink, WatchState, WatchStatus, Watcher, t};
    use super::NotifyDecoder;

    // file name, folder name, attributes, size, last write, creation
    const NOTIFY_FILTER: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x40;
    const EXTENDED_CLASS: i32 = 2; // ReadDirectoryNotifyExtendedInformation
    const UNSUPPORTED: [u32; 4] = [1, 50, 87, 124]; // invalid function, not supported, parameter, level
    const DRIVE_REMOTE: u32 = 4;
    const LOCAL_BUFFER: usize = 1 << 20;
    const NETWORK_BUFFER: usize = 64 << 10; // a change notification over SMB is capped at 64 KiB
    const MIN_BUFFER: usize = 256;
    const HOLD_RENAME: Duration = Duration::from_millis(50);
    const CHECK_LOCAL: Duration = Duration::from_secs(1);
    const CHECK_NETWORK: Duration = Duration::from_secs(5);
    const CANCEL_WAIT_MS: u32 = 2000;

    fn wide(p: &Path) -> Vec<u16> {
        long_path(p)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn long_path(p: &Path) -> PathBuf {
        let s = p.as_os_str().to_string_lossy();
        if s.len() < 240 || s.starts_with("\\\\?\\") {
            return p.to_path_buf();
        }
        if let Some(rest) = s.strip_prefix("\\\\") {
            return PathBuf::from(format!("\\\\?\\UNC\\{rest}"));
        }
        PathBuf::from(format!("\\\\?\\{s}"))
    }

    fn message(err: u32) -> String {
        let e = std::io::Error::from_raw_os_error(err as i32);
        let m = crate::volume::os_message(&e);
        if m.is_empty() {
            crate::tr!("错误 {code}", code = err)
        } else {
            m
        }
    }

    fn volume_path(p: &Path) -> Vec<u16> {
        let w = wide(p);
        let mut buf = vec![0u16; 1024];
        // SAFETY: buffers of the stated sizes.
        if unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), 1024) } != 0 && buf[0] != 0 {
            return buf;
        }
        Vec::new()
    }

    /// UNC paths and folders on a network drive.
    pub fn is_network_path(path: &Path) -> bool {
        let p = path.to_string_lossy().replace('/', "\\");
        if p.to_uppercase().starts_with("\\\\?\\UNC\\") {
            return true;
        }
        if p.starts_with("\\\\") && !p.starts_with("\\\\?\\") && !p.starts_with("\\\\.\\") {
            return true;
        }
        let v = volume_path(path);
        // SAFETY: NUL-terminated.
        !v.is_empty() && unsafe { GetDriveTypeW(v.as_ptr()) } == DRIVE_REMOTE
    }

    fn cluster_size(path: &Path) -> i64 {
        let v = volume_path(path);
        let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
        // SAFETY: NUL-terminated root; out pointers to locals.
        if !v.is_empty()
            && unsafe { GetDiskFreeSpaceW(v.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) }
                != 0
        {
            return spc as i64 * bps as i64;
        }
        0
    }

    fn final_path(h: HANDLE) -> String {
        let mut size = 1024u32;
        for _ in 0..2 {
            let mut buf = vec![0u16; size as usize];
            // SAFETY: buffer of `size` units.
            let got = unsafe { GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), size, 0) };
            if got == 0 {
                return String::new();
            }
            if got < size {
                return String::from_utf16_lossy(&buf[..got as usize]);
            }
            size = got + 1;
        }
        String::new()
    }

    struct Shared {
        root: PathBuf,
        emitter: Emitter,
        state: Mutex<(WatchState, String)>,
        armed: AtomicBool,
        stop_event: AtomicUsize,
        handle: AtomicUsize,
        extended: AtomicBool,
        buffer: AtomicUsize,
        done: AtomicBool,
    }

    impl Shared {
        fn stopping(&self) -> bool {
            self.emitter.stopping.load(Ordering::SeqCst)
        }

        fn fail(&self, detail: String, decoder: &mut NotifyDecoder) {
            if self.stopping() {
                return; // stopping: an aborted read is not a failure
            }
            *self.state.lock().unwrap() = (WatchState::Failed, detail.clone());
            tracing::warn!(
                "{}",
                crate::tr!(
                    "文件监视失败：{path}：{detail}",
                    path = self.root.display().to_string(),
                    detail = detail
                )
            );
            if self.armed.load(Ordering::SeqCst) {
                let mut batch = decoder.flush();
                batch.push(Change::overflow(self.root.clone()));
                self.emitter.emit(batch);
            }
        }
    }

    /// Recursive watch of `root` with `ReadDirectoryChangesExW` (see the module docs).
    ///
    /// `buffer_size`: bytes per read; by default 1 MiB on local volumes and 64 KiB on network
    /// paths. `status()` turns `running` once the first read is pending.
    pub struct RdcwWatcher {
        shared: Arc<Shared>,
        buffer_size: Option<usize>,
        worker: Worker,
        /// tests clear it before start() to get the plain-call fallback
        pub use_extended: AtomicBool,
    }

    impl RdcwWatcher {
        pub fn new(root: &Path, buffer_size: Option<usize>) -> RdcwWatcher {
            RdcwWatcher {
                shared: Arc::new(Shared {
                    root: root.to_path_buf(),
                    emitter: Emitter::new(),
                    state: Mutex::new((WatchState::Starting, String::new())),
                    armed: AtomicBool::new(false),
                    stop_event: AtomicUsize::new(0),
                    handle: AtomicUsize::new(0),
                    extended: AtomicBool::new(true),
                    buffer: AtomicUsize::new(
                        buffer_size.map_or(0, |b| b.max(MIN_BUFFER).div_ceil(8) * 8),
                    ),
                    done: AtomicBool::new(false),
                }),
                buffer_size: buffer_size.map(|b| b.max(MIN_BUFFER).div_ceil(8) * 8),
                worker: Worker::new(),
                use_extended: AtomicBool::new(true),
            }
        }

        /// Bytes per read (after start: what the volume took; 0 before start without an explicit size).
        pub fn buffer_size(&self) -> usize {
            self.shared.buffer.load(Ordering::SeqCst)
        }

        /// Whether the watcher thread has ended.
        pub fn finished(&self) -> bool {
            self.shared.done.load(Ordering::SeqCst)
        }

        /// Whether reads use the extended information class (after start: what the volume took).
        pub fn extended(&self) -> bool {
            self.shared.extended.load(Ordering::SeqCst)
        }
    }

    struct Reader {
        sh: Arc<Shared>,
        handle: HANDLE,
        io_event: HANDLE,
        stop_event: HANDLE,
        ov: Box<OVERLAPPED>,
        buf: Vec<u64>, // 8-byte aligned
        size: usize,
        extended: bool,
        pending: bool,
        completed: u64,
        attempts: Vec<(bool, usize)>,
        decoder: NotifyDecoder,
        final_path: String,
        check_every: Duration,
    }

    impl Reader {
        fn open(&mut self, explicit: Option<usize>, ext_ok: bool) -> bool {
            let w = wide(&self.sh.root);
            // SAFETY: NUL-terminated path.
            let h = unsafe {
                CreateFileW(
                    w.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if h == INVALID_HANDLE_VALUE || h.is_null() {
                // SAFETY: plain call.
                let err = unsafe { GetLastError() };
                self.sh.fail(
                    crate::tr!("无法监视此文件夹：{reason}", reason = message(err)),
                    &mut self.decoder,
                );
                return false;
            }
            self.handle = h;
            self.sh.handle.store(h as usize, Ordering::SeqCst);
            let network = is_network_path(&self.sh.root);
            let size = explicit.unwrap_or(if network {
                NETWORK_BUFFER
            } else {
                LOCAL_BUFFER
            });
            let sizes = if explicit.is_some() || size <= NETWORK_BUFFER {
                vec![size]
            } else {
                vec![size, NETWORK_BUFFER]
            };
            let exts: &[bool] = if ext_ok { &[true, false] } else { &[false] };
            self.attempts = exts
                .iter()
                .flat_map(|&e| sizes.iter().map(move |&s| (e, s)))
                .collect();
            self.check_every = if network { CHECK_NETWORK } else { CHECK_LOCAL };
            self.decoder.cluster = cluster_size(&self.sh.root);
            self.final_path = final_path(h);
            !self.sh.stopping()
        }

        /// Issue a read with the next (extended, buffer size) the volume accepts.
        fn arm(&mut self) -> bool {
            let mut err = 50u32;
            while !self.attempts.is_empty() {
                let (extended, size) = self.attempts.remove(0);
                if self.buf.len() * 8 != size {
                    self.buf = vec![0u64; size / 8];
                }
                self.size = size;
                self.extended = extended;
                self.decoder.extended = extended;
                err = self.issue();
                if !UNSUPPORTED.contains(&err) {
                    break;
                }
            }
            if err != 0 {
                if !self.sh.stopping() {
                    let detail = if UNSUPPORTED.contains(&err) {
                        t("此文件系统不支持变化通知")
                    } else {
                        crate::tr!("无法监视此文件夹：{reason}", reason = message(err))
                    };
                    self.sh.fail(detail, &mut self.decoder);
                }
                return false;
            }
            self.sh.armed.store(true, Ordering::SeqCst);
            self.sh.extended.store(self.extended, Ordering::SeqCst);
            self.sh.buffer.store(self.size, Ordering::SeqCst);
            if !self.sh.stopping() {
                let detail = if self.extended {
                    String::new()
                } else {
                    t("此卷不提供扩展变化信息，文件大小变化需重新列出目录")
                };
                *self.sh.state.lock().unwrap() = (WatchState::Running, detail);
            }
            tracing::debug!(
                "watching {} ({}, buffer {} KiB)",
                self.sh.root.display(),
                if self.extended {
                    "ReadDirectoryChangesExW"
                } else {
                    "ReadDirectoryChangesW"
                },
                self.size >> 10
            );
            true
        }

        /// Start the next read; 0 or the Win32 error.
        fn issue(&mut self) -> u32 {
            if self.sh.stopping() || self.handle.is_null() {
                return ERROR_OPERATION_ABORTED;
            }
            // SAFETY: our event; the OVERLAPPED and buffer live in this Reader until the read ends.
            unsafe { ResetEvent(self.io_event) };
            *self.ov = unsafe { std::mem::zeroed() };
            self.ov.hEvent = self.io_event;
            let ok = unsafe {
                if self.extended {
                    ReadDirectoryChangesExW(
                        self.handle,
                        self.buf.as_mut_ptr().cast(),
                        self.size as u32,
                        1,
                        NOTIFY_FILTER,
                        std::ptr::null_mut(),
                        &mut *self.ov,
                        None,
                        EXTENDED_CLASS,
                    )
                } else {
                    ReadDirectoryChangesW(
                        self.handle,
                        self.buf.as_mut_ptr().cast(),
                        self.size as u32,
                        1,
                        NOTIFY_FILTER,
                        std::ptr::null_mut(),
                        &mut *self.ov,
                        None,
                    )
                }
            };
            if ok == 0 {
                // SAFETY: plain call.
                let e = unsafe { GetLastError() };
                return if e == 0 { 1 } else { e };
            }
            self.pending = true;
            0
        }

        fn run(&mut self) {
            let handles = [self.io_event, self.stop_event];
            let mut next_check = Instant::now() + self.check_every;
            while !self.sh.stopping() {
                let wait = if self.decoder.held.is_some() {
                    HOLD_RENAME
                } else {
                    next_check.saturating_duration_since(Instant::now())
                };
                // SAFETY: two valid event handles.
                let r = unsafe {
                    WaitForMultipleObjects(2, handles.as_ptr(), 0, wait.as_millis() as u32)
                };
                if self.sh.stopping() || r == WAIT_OBJECT_0 + 1 {
                    return;
                }
                if r == WAIT_TIMEOUT {
                    let b = self.decoder.flush();
                    self.sh.emitter.emit(b);
                } else if r != WAIT_OBJECT_0 {
                    // SAFETY: plain call.
                    let e = unsafe { GetLastError() };
                    self.sh.fail(
                        crate::tr!("监视意外中断：{reason}", reason = message(e)),
                        &mut self.decoder,
                    );
                    return;
                } else if !self.completion() {
                    return;
                }
                if Instant::now() >= next_check {
                    next_check = Instant::now() + self.check_every;
                    let problem = self.root_problem(false);
                    if !problem.is_empty() {
                        self.sh.fail(problem, &mut self.decoder);
                        return;
                    }
                }
            }
        }

        /// Reap the finished read, issue the next one, hand the batch over; false: watch ended.
        fn completion(&mut self) -> bool {
            let mut got = 0u32;
            // SAFETY: our handle and OVERLAPPED.
            let ok = unsafe { GetOverlappedResult(self.handle, &*self.ov, &mut got, 0) };
            let err = if ok != 0 {
                0
            } else {
                unsafe { GetLastError() }
            };
            if err != ERROR_IO_INCOMPLETE {
                self.pending = false;
            }
            if self.sh.stopping() {
                return false;
            }
            if UNSUPPORTED.contains(&err) && self.completed == 0 && !self.attempts.is_empty() {
                // refused only once the request reached the volume (a share): the next fallback
                if !self.arm() {
                    return false;
                }
                let mut b = self.decoder.flush();
                b.push(Change::overflow(self.sh.root.clone()));
                self.sh.emitter.emit(b);
                return true;
            }
            let overflow = err == ERROR_NOTIFY_ENUM_DIR || (err == 0 && got == 0);
            if err != 0 && !overflow {
                let f = self.failure(err);
                self.sh.fail(f, &mut self.decoder);
                return false;
            }
            let n = (got as usize).min(self.size);
            // SAFETY: the buffer holds `size` bytes, `n` of them written by the completed read.
            let data: Vec<u8> =
                unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), n) }.to_vec();
            self.completed += 1;
            let reissue = self.issue(); // keep a read pending while this batch is decoded
            let batch = if overflow {
                tracing::debug!("change buffer overflow: {}", self.sh.root.display());
                let mut b = self.decoder.flush();
                b.push(Change::overflow(self.sh.root.clone()));
                b
            } else {
                self.decoder.decode(&data)
            };
            self.sh.emitter.emit(batch);
            if reissue != 0 && !self.sh.stopping() {
                let f = self.failure(reissue);
                self.sh.fail(f, &mut self.decoder);
                return false;
            }
            !self.sh.stopping()
        }

        fn failure(&self, err: u32) -> String {
            let p = self.root_problem(true);
            if !p.is_empty() {
                return p;
            }
            if UNSUPPORTED.contains(&err) {
                return t("此文件系统不支持变化通知");
            }
            crate::tr!("监视中断：{reason}", reason = message(err))
        }

        /// Whether the root is gone: deleted (delete pending, or POSIX-style) or moved.
        fn root_problem(&self, lookup: bool) -> String {
            if self.handle.is_null() {
                return String::new();
            }
            // SAFETY: zeroed POD out struct of the stated size.
            let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    self.handle,
                    FileStandardInfo,
                    (&mut info as *mut FILE_STANDARD_INFO).cast(),
                    std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
                )
            };
            if ok != 0 && info.DeletePending {
                return t("监视的文件夹已被删除");
            }
            let now = final_path(self.handle);
            if !self.final_path.is_empty()
                && !now.is_empty()
                && now.to_lowercase() != self.final_path.to_lowercase()
            {
                // a POSIX-style delete moves the still-open folder to \$Extend\$Deleted
                return if now.to_lowercase().contains("\\$extend\\$deleted\\") {
                    t("监视的文件夹已被删除")
                } else {
                    t("监视的文件夹已被移动或改名")
                };
            }
            if lookup && !self.sh.root.is_dir() {
                return t("监视的文件夹已被删除");
            }
            String::new()
        }

        fn close(&mut self) {
            self.sh.handle.store(0, Ordering::SeqCst);
            if !self.handle.is_null() && self.pending {
                // SAFETY: our handle and OVERLAPPED.
                unsafe { CancelIoEx(self.handle, &*self.ov) };
                // the buffer and OVERLAPPED must outlive the read: wait for the cancellation
                if unsafe { WaitForSingleObject(self.io_event, CANCEL_WAIT_MS) } != WAIT_OBJECT_0 {
                    tracing::warn!(
                        "{}",
                        crate::tr!(
                            "文件监视的读取未能取消，保留其缓冲区：{path}",
                            path = self.sh.root.display().to_string()
                        )
                    );
                    std::mem::forget(std::mem::take(&mut self.buf));
                    std::mem::forget(std::mem::replace(
                        &mut self.ov,
                        Box::new(unsafe { std::mem::zeroed() }),
                    ));
                }
            }
            if !self.handle.is_null() {
                // SAFETY: closing what we opened.
                unsafe { CloseHandle(self.handle) };
                self.handle = std::ptr::null_mut();
            }
            self.pending = false;
            let mut st = self.sh.state.lock().unwrap();
            if matches!(st.0, WatchState::Starting | WatchState::Running) {
                *st = (WatchState::Stopped, String::new());
            }
        }
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            self.close();
            // SAFETY: closing our events (the stop event is cleared from the shared state first).
            self.sh.stop_event.store(0, Ordering::SeqCst);
            unsafe {
                CloseHandle(self.io_event);
                CloseHandle(self.stop_event);
            }
        }
    }

    impl Watcher for RdcwWatcher {
        fn name(&self) -> &'static str {
            "rdcw"
        }

        fn start(&self, sink: Sink) -> std::io::Result<()> {
            if self.worker.started() || self.shared.stopping() {
                return Err(std::io::Error::other(
                    "RdcwWatcher.start() may only be called once, before stop()",
                ));
            }
            // SAFETY: plain calls; handles are closed by the Reader.
            let io_event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            let stop_event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if io_event.is_null() || stop_event.is_null() {
                let e = std::io::Error::last_os_error();
                unsafe {
                    if !io_event.is_null() {
                        CloseHandle(io_event);
                    }
                    if !stop_event.is_null() {
                        CloseHandle(stop_event);
                    }
                }
                return Err(e);
            }
            self.shared
                .stop_event
                .store(stop_event as usize, Ordering::SeqCst);
            *self.shared.emitter.sink.lock().unwrap() = Some(sink);
            let sh = self.shared.clone();
            let explicit = self.buffer_size;
            let ext_ok = self.use_extended.load(Ordering::SeqCst);
            let (io_u, stop_u) = (io_event as usize, stop_event as usize);
            self.worker
                .spawn(&format!("rdcw {}", self.shared.root.display()), move || {
                    let mut r = Reader {
                        decoder: NotifyDecoder::new(&sh.root),
                        sh,
                        handle: std::ptr::null_mut(),
                        io_event: io_u as HANDLE,
                        stop_event: stop_u as HANDLE,
                        // SAFETY: an all-zero OVERLAPPED is valid.
                        ov: Box::new(unsafe { std::mem::zeroed() }),
                        buf: Vec::new(),
                        size: 0,
                        extended: ext_ok,
                        pending: false,
                        completed: 0,
                        attempts: Vec::new(),
                        final_path: String::new(),
                        check_every: CHECK_LOCAL,
                    };
                    let body = std::panic::AssertUnwindSafe(|| {
                        if r.open(explicit, ext_ok) && r.arm() {
                            r.run();
                        }
                    });
                    if std::panic::catch_unwind(body).is_err() {
                        let sh = r.sh.clone();
                        tracing::error!(
                            "{}",
                            crate::tr!(
                                "文件监视线程异常：{path}",
                                path = sh.root.display().to_string()
                            )
                        );
                        sh.fail(
                            crate::tr!("监视意外中断：{reason}", reason = "panic"),
                            &mut r.decoder,
                        );
                    }
                    let sh = r.sh.clone();
                    drop(r);
                    sh.done.store(true, Ordering::SeqCst);
                })
        }

        fn add_dirs(&self, _dirs: Vec<PathBuf>, _mtimes: Option<Vec<u32>>, _since_ns: i64) {
            // recursive watch: every folder below the root is covered already (from the scan's start)
        }

        fn stop(&self) {
            self.shared.emitter.stopping.store(true, Ordering::SeqCst);
            let ev = self.shared.stop_event.load(Ordering::SeqCst);
            if ev != 0 {
                // SAFETY: the event lives until the reader drops (which clears the slot first).
                unsafe { SetEvent(ev as HANDLE) };
            }
            self.worker.finish(Duration::from_millis(1000));
            let mut st = self.shared.state.lock().unwrap();
            if st.0 != WatchState::Failed {
                *st = (WatchState::Stopped, String::new());
            }
        }

        fn status(&self) -> WatchStatus {
            let (state, detail) = self.shared.state.lock().unwrap().clone();
            WatchStatus {
                watched_dirs: if state == WatchState::Running { -1 } else { 0 },
                polled_dirs: 0,
                overflows: self.shared.emitter.overflows.load(Ordering::Relaxed),
                events: self.shared.emitter.events.load(Ordering::Relaxed),
                state,
                detail,
            }
        }
    }

    impl Drop for RdcwWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\w")
        } else {
            PathBuf::from("/w")
        }
    }

    #[test]
    fn decode_actions_and_numbers() {
        let mut d = NotifyDecoder::new(&root());
        d.cluster = 4096;
        let ft = (EPOCH_SECS + 1_700_000_000) * 10_000_000;
        let buf = encode_records(
            &[
                (FILE_ACTION_ADDED, "new.txt", 0x20, 0, 0, ft),
                (FILE_ACTION_ADDED, "dir", FILE_ATTRIBUTE_DIRECTORY, 0, 0, ft),
                (FILE_ACTION_MODIFIED, "big.bin", 0x20, 5000, 8192, ft),
                (FILE_ACTION_MODIFIED, "tiny.txt", 0x20, 10, 16, ft), // resident: no clusters
                (FILE_ACTION_MODIFIED, "packed.bin", 0x800, 10, 4096, ft),
                (FILE_ACTION_MODIFIED, "cloud.docx", 0x400000, 10, 4096, ft),
                (
                    FILE_ACTION_MODIFIED,
                    "dir",
                    FILE_ATTRIBUTE_DIRECTORY,
                    0,
                    0,
                    ft,
                ),
                (FILE_ACTION_REMOVED, "gone.txt", 0x20, 0, 0, 0),
            ],
            true,
        );
        let got = d.decode(&buf);
        let r = root();
        assert_eq!(got.len(), 7, "{got:?}");
        assert_eq!(
            got[0],
            Change::new(ChangeKind::Created, r.join("new.txt"), Some(false))
        );
        assert_eq!(
            got[1],
            Change::new(ChangeKind::Created, r.join("dir"), Some(true))
        );
        assert_eq!(
            got[2],
            Change::modified(r.join("big.bin"), 5000, Some(8192), 1_700_000_000)
        );
        assert_eq!(got[3].alloc, Some(0));
        assert_eq!(got[4].alloc, None);
        assert_eq!(got[5].alloc, Some(0));
        assert_eq!(
            got[6],
            Change::new(ChangeKind::Deleted, r.join("gone.txt"), Some(false))
        );
    }

    #[test]
    fn decode_keeps_the_last_modified_per_path() {
        let mut d = NotifyDecoder::new(&root());
        let buf = encode_records(
            &[
                (FILE_ACTION_MODIFIED, "a", 0x20, 1, 0, 0),
                (FILE_ACTION_MODIFIED, "b", 0x20, 2, 0, 0),
                (FILE_ACTION_MODIFIED, "a", 0x20, 3, 0, 0),
            ],
            true,
        );
        let got = d.decode(&buf);
        assert_eq!(
            got.iter()
                .map(|c| (c.path.clone(), c.size))
                .collect::<Vec<_>>(),
            vec![(root().join("b"), Some(2)), (root().join("a"), Some(3))]
        );
    }

    #[test]
    fn decode_renames_across_reads() {
        let mut d = NotifyDecoder::new(&root());
        let got = d.decode(&encode_records(
            &[(FILE_ACTION_RENAMED_OLD_NAME, "old", 0x10, 0, 0, 0)],
            true,
        ));
        assert!(got.is_empty());
        let got = d.decode(&encode_records(
            &[(FILE_ACTION_RENAMED_NEW_NAME, "new", 0x10, 0, 0, 0)],
            true,
        ));
        assert_eq!(
            got,
            vec![Change::renamed(
                root().join("new"),
                Some(root().join("old")),
                Some(true)
            )]
        );
        // an old name whose new name never comes is a deletion
        d.decode(&encode_records(
            &[(FILE_ACTION_RENAMED_OLD_NAME, "x", 0x20, 0, 0, 0)],
            true,
        ));
        assert_eq!(
            d.flush(),
            vec![Change::new(
                ChangeKind::Deleted,
                root().join("x"),
                Some(false)
            )]
        );
        // ... or is followed by another action
        let got = d.decode(&encode_records(
            &[
                (FILE_ACTION_RENAMED_OLD_NAME, "y", 0x20, 0, 0, 0),
                (FILE_ACTION_ADDED, "z", 0x20, 0, 0, 0),
            ],
            true,
        ));
        assert_eq!(
            got[0],
            Change::new(ChangeKind::Deleted, root().join("y"), Some(false))
        );
        assert_eq!(got[1].kind, ChangeKind::Created);
    }

    #[test]
    fn decode_plain_records_and_names() {
        let mut d = NotifyDecoder::new(&root());
        d.extended = false;
        let buf = encode_records(
            &[
                (FILE_ACTION_MODIFIED, "a b\\c.txt", 0, 0, 0, 0),
                (FILE_ACTION_ADDED, "中文.txt", 0, 0, 0, 0),
            ],
            false,
        );
        let got = d.decode(&buf);
        assert_eq!(got[0].kind, ChangeKind::Modified);
        assert_eq!(got[0].size, None);
        assert_eq!(got[0].is_dir, None);
        assert!(got[0].path.ends_with("c.txt"));
        assert_eq!(
            got[1],
            Change::new(ChangeKind::Created, root().join("中文.txt"), None)
        );
        // a torn buffer ends the batch
        assert!(d.decode(&buf[..10]).is_empty());
    }
}
