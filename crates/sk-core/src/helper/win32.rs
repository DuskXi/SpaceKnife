//! Win32 plumbing for the helper (Python `helper/_win32.py`): overlapped named pipes, DACLs, admin
//! rights, UAC launch, volume facts. Also the small handle / string helpers `disks` and `io` use.
//!
//! Everything blocking takes a timeout; [`Pipe`] I/O can be aborted from any thread.

#![allow(clippy::upper_case_acronyms)]

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::{null, null_mut};
use std::time::Duration;

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_CANCELLED, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser, WinBuiltinAdministratorsSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceW, GetDriveTypeW, GetVolumeInformationW, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, QueryDosDeviceW, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_REJECT_REMOTE_CLIENTS, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, GetProcessId, INFINITE, OpenProcessToken, SetEvent,
    TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{SHELLEXECUTEINFOW, ShellExecuteExW};

use super::messages::Scalars;
use crate::i18n::t;

const PIPE_BUFFER: u32 = 1 << 20;
const MAX_IO_CHUNK: usize = 32 << 20;
const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;

// --------------------------------------------------------------------------- small helpers

/// `s` as a NUL-terminated UTF-16 string.
pub(crate) fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// The UTF-16 string up to the first NUL (or the whole buffer), lossily.
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end]).to_string_lossy().into_owned()
}

/// A NUL-terminated UTF-16 string at `p`.
///
/// # Safety
/// `p` must point to a NUL-terminated UTF-16 string (or be null).
pub(crate) unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: the caller promises a NUL-terminated string.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        from_wide(std::slice::from_raw_parts(p, n))
    }
}

pub(crate) fn valid(h: HANDLE) -> bool {
    !h.is_null() && h != INVALID_HANDLE_VALUE
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetLastError() }
}

fn win_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// A handle closed on drop.
pub(crate) struct Handle(pub HANDLE);

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// `CreateFileW(path, access, share, OPEN_EXISTING, flags)`; None when it cannot be opened.
    pub(crate) fn open(path: &str, access: u32, share: u32, flags: u32) -> Option<Handle> {
        let w = wide(path);
        // SAFETY: valid NUL-terminated path; no security attributes or template.
        let h = unsafe { CreateFileW(w.as_ptr(), access, share, null(), OPEN_EXISTING, flags, null_mut()) };
        valid(h).then_some(Handle(h))
    }

    /// `DeviceIoControl` with `input`, into a buffer of `out_size` bytes; the bytes returned.
    pub(crate) fn ioctl(&self, code: u32, input: &[u8], out_size: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; out_size];
        let mut got = 0u32;
        // SAFETY: buffers are valid for the given sizes; synchronous call (no OVERLAPPED).
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                if input.is_empty() { null() } else { input.as_ptr().cast() },
                input.len() as u32,
                out.as_mut_ptr().cast(),
                out_size as u32,
                &mut got,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        out.truncate(got as usize);
        Ok(out)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

fn millis(timeout: Option<Duration>) -> u32 {
    match timeout {
        None => INFINITE,
        Some(d) => d.as_millis().min(u128::from(INFINITE - 1)) as u32,
    }
}

/// `n` random bytes from the system's generator.
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    // SAFETY: valid buffer; ProcessPrng always succeeds (Windows 10+).
    unsafe { windows_sys::Win32::Security::Cryptography::ProcessPrng(buf.as_mut_ptr(), n) };
    buf
}

// --------------------------------------------------------------------------- identity & DACLs

/// String SID (S-1-5-21-…) of the user this process runs as.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: plain out-parameter calls on our own process token.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Handle(token);
        let mut size = 0u32;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut size);
        let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), size, &mut size) == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text: *mut u16 = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let out = from_wide_ptr(text);
        LocalFree(text.cast());
        Ok(out)
    }
}

/// Protected DACL: full access for the current user and BUILTIN\Administrators, nobody else.
///
/// The Administrators ACE only matches *elevated* tokens (a UAC-filtered token carries the group
/// as deny-only), so it lets an over-the-shoulder elevation (another admin account typed into
/// the UAC prompt) reach the pipe and token file without opening them to anyone who could not
/// read them anyway.
pub fn owner_only_sddl() -> io::Result<String> {
    Ok(format!("D:P(A;;GA;;;{})(A;;GA;;;BA)", current_user_sid()?))
}

/// SECURITY_ATTRIBUTES built from SDDL (the descriptor is freed on drop).
struct SecurityAttributes {
    sd: *mut c_void,
    sa: SECURITY_ATTRIBUTES,
}

impl SecurityAttributes {
    fn new(sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut sd: *mut c_void = null_mut();
        // SAFETY: valid SDDL string and out-pointer.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            sd,
            sa: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            },
        })
    }
}

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.sd) };
    }
}

/// Create `path` (must not exist) with [`owner_only_sddl`] and write `data`.
pub fn write_private_file(path: &std::path::Path, data: &[u8]) -> io::Result<()> {
    let sa = SecurityAttributes::new(&owner_only_sddl()?)?;
    let w = wide(path);
    // SAFETY: valid path and security attributes.
    let h = unsafe {
        CreateFileW(w.as_ptr(), GENERIC_WRITE, 0, &sa.sa, CREATE_NEW, FILE_ATTRIBUTE_TEMPORARY, null_mut())
    };
    if !valid(h) {
        return Err(io::Error::last_os_error());
    }
    let h = Handle(h);
    let mut written = 0u32;
    // SAFETY: valid buffer.
    let ok = unsafe { WriteFile(h.0, data.as_ptr(), data.len() as u32, &mut written, null_mut()) };
    if ok == 0 || written as usize != data.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether BUILTIN\Administrators is *enabled* in this process's token.
///
/// That is what raw volume access needs. Unlike `TokenElevation` it is also false for a
/// Safer/restricted token where the group is deny-only.
pub fn is_admin() -> bool {
    let mut sid = [0u8; 68]; // SECURITY_MAX_SID_SIZE
    let mut size = sid.len() as u32;
    let mut member = 0i32;
    // SAFETY: buffers sized as documented.
    unsafe {
        if CreateWellKnownSid(WinBuiltinAdministratorsSid, null_mut(), sid.as_mut_ptr().cast(), &mut size) == 0 {
            return false;
        }
        if CheckTokenMembership(null_mut(), sid.as_mut_ptr().cast(), &mut member) == 0 {
            return false;
        }
    }
    member != 0
}

// --------------------------------------------------------------------------- named pipes

struct PipeState {
    closed: bool,
    users: usize,
}

/// One end of a byte-mode named pipe opened for overlapped I/O.
///
/// `read` / `write` block the calling thread for at most `timeout` without progress; `close`
/// may be called from any thread and aborts pending I/O at once. An OVERLAPPED never outlives
/// its operation: abandoned I/O is cancelled and waited for before the call returns, and the
/// handle is closed only when no call is using it.
pub struct Pipe {
    handle: HANDLE,
    cancel: HANDLE,
    state: Mutex<PipeState>,
}

// SAFETY: the handles are kernel objects; all mutable state is behind the mutex.
unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

fn event() -> io::Result<HANDLE> {
    // SAFETY: manual-reset, initially unsignalled, unnamed.
    let h = unsafe { CreateEventW(null(), 1, 0, null()) };
    if h.is_null() { Err(io::Error::last_os_error()) } else { Ok(h) }
}

fn closed_or_error(err: u32) -> io::Error {
    if err == ERROR_BROKEN_PIPE || err == ERROR_NO_DATA || err == ERROR_PIPE_NOT_CONNECTED {
        io::Error::new(io::ErrorKind::BrokenPipe, t("管道已断开"))
    } else {
        win_error(err)
    }
}

impl Pipe {
    fn new(handle: HANDLE) -> io::Result<Self> {
        let cancel = match event() {
            Ok(e) => e,
            Err(e) => {
                // SAFETY: we own the handle.
                unsafe { CloseHandle(handle) };
                return Err(e);
            }
        };
        Ok(Self {
            handle,
            cancel,
            state: Mutex::new(PipeState { closed: false, users: 0 }),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    pub fn close(&self) {
        {
            let mut st = self.state.lock();
            if st.closed {
                return;
            }
            st.closed = true;
            // SAFETY: valid handles (released only when closed and unused).
            unsafe { SetEvent(self.cancel) };
            if st.users > 0 {
                unsafe { CancelIoEx(self.handle, null()) }; // the last user releases the handles
                return;
            }
        }
        self.release();
    }

    fn release(&self) {
        // SAFETY: called exactly once, when closed and no call uses the handles.
        unsafe {
            CloseHandle(self.handle);
            CloseHandle(self.cancel);
        }
    }

    fn enter(&self) -> io::Result<HANDLE> {
        let mut st = self.state.lock();
        if st.closed {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, t("管道已关闭")));
        }
        st.users += 1;
        Ok(self.handle)
    }

    fn leave(&self) {
        let release = {
            let mut st = self.state.lock();
            st.users -= 1;
            st.closed && st.users == 0
        };
        if release {
            self.release();
        }
    }

    /// Start one overlapped operation and wait for it; returns the byte count.
    fn run(&self, timeout: Option<Duration>, start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32) -> io::Result<usize> {
        let handle = self.enter()?;
        let result = (|| {
            let ev = event()?;
            let ev = Handle(ev);
            // SAFETY: a zeroed OVERLAPPED is valid; it lives on this frame until the operation ends.
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.hEvent = ev.0;
            if start(handle, &mut ov) == 0 {
                let err = last_error();
                if err == ERROR_PIPE_CONNECTED {
                    return Ok(0); // ConnectNamedPipe: the client was faster
                }
                if err != ERROR_IO_PENDING {
                    return Err(closed_or_error(err));
                }
            }
            let waits = [ev.0, self.cancel];
            // SAFETY: valid handles.
            let outcome = unsafe { WaitForMultipleObjects(2, waits.as_ptr(), 0, millis(timeout)) };
            if outcome != WAIT_OBJECT_0 {
                unsafe { CancelIoEx(handle, &ov) };
            }
            let mut count = 0u32;
            // SAFETY: waits for the operation to finish (bWait = TRUE): the OVERLAPPED is released after.
            if unsafe { GetOverlappedResult(handle, &ov, &mut count, 1) } != 0 {
                return Ok(count as usize); // completed (possibly racing the cancel): keep the data
            }
            let err = last_error();
            if err == ERROR_OPERATION_ABORTED {
                if outcome == WAIT_TIMEOUT {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, t("管道读写超时")));
                }
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, t("管道已关闭")));
            }
            Err(closed_or_error(err))
        })();
        self.leave();
        result
    }

    /// Server end: wait for the client to connect.
    pub fn accept(&self, timeout: Option<Duration>) -> io::Result<()> {
        // SAFETY: valid handle and OVERLAPPED.
        self.run(timeout, |h, ov| unsafe { ConnectNamedPipe(h, ov) }).map(|_| ())
    }

    /// Read at least one byte into `buf`; `UnexpectedEof` when the other end has closed.
    pub fn read(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<usize> {
        let size = buf.len().min(MAX_IO_CHUNK) as u32;
        let ptr = buf.as_mut_ptr();
        // SAFETY: `buf` outlives the operation (run waits for it to finish).
        let got = match self.run(timeout, |h, ov| unsafe { ReadFile(h, ptr, size, null_mut(), ov) }) {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, t("管道已断开")));
            }
            other => other?,
        };
        if got == 0 && size > 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, t("管道已断开")));
        }
        Ok(got)
    }

    pub fn write(&self, buf: &[u8], timeout: Option<Duration>) -> io::Result<usize> {
        let size = buf.len().min(MAX_IO_CHUNK) as u32;
        let ptr = buf.as_ptr();
        // SAFETY: `buf` outlives the operation.
        self.run(timeout, |h, ov| unsafe { WriteFile(h, ptr, size, null_mut(), ov) })
    }

    fn pid(&self, query: unsafe extern "system" fn(HANDLE, *mut u32) -> i32) -> io::Result<u32> {
        let h = self.enter()?;
        let mut pid = 0u32;
        // SAFETY: valid handle and out-pointer.
        let ok = unsafe { query(h, &mut pid) };
        let err = io::Error::last_os_error();
        self.leave();
        if ok == 0 { Err(err) } else { Ok(pid) }
    }

    pub fn client_pid(&self) -> io::Result<u32> {
        self.pid(GetNamedPipeClientProcessId)
    }

    pub fn server_pid(&self) -> io::Result<u32> {
        self.pid(GetNamedPipeServerProcessId)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.close();
    }
}

/// The single instance of a new local-only pipe, accessible to this user (and elevated admins).
pub fn create_pipe_server(name: &str) -> io::Result<Pipe> {
    let sa = SecurityAttributes::new(&owner_only_sddl()?)?;
    let w = wide(name);
    // SAFETY: valid name and security attributes.
    let h = unsafe {
        CreateNamedPipeW(
            w.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS, // byte type, byte read mode, blocking (all 0) + local only
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            &sa.sa,
        )
    };
    if !valid(h) {
        return Err(io::Error::last_os_error());
    }
    Pipe::new(h)
}

/// Client end (the helper). Identification-level only: the server can never impersonate us.
pub fn connect_pipe(name: &str, timeout: Duration) -> io::Result<Pipe> {
    let w = wide(name);
    let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    let open = || {
        // SAFETY: valid name.
        unsafe { CreateFileW(w.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, flags, null_mut()) }
    };
    let mut h = open();
    if !valid(h) && last_error() == ERROR_PIPE_BUSY {
        // SAFETY: valid name.
        unsafe { WaitNamedPipeW(w.as_ptr(), millis(Some(timeout))) };
        h = open();
    }
    if !valid(h) {
        return Err(io::Error::last_os_error());
    }
    Pipe::new(h)
}

// --------------------------------------------------------------------------- processes

/// A process we started through a raw Win32 handle (ShellExecuteEx).
pub struct ProcessHandle {
    handle: Option<Handle>,
    code: Option<i32>,
    pid: u32,
}

impl ProcessHandle {
    /// Takes ownership of `handle`.
    ///
    /// # Safety
    /// `handle` must be a valid process handle that nothing else closes.
    pub unsafe fn new(handle: HANDLE) -> Self {
        // SAFETY: valid process handle.
        let pid = unsafe { GetProcessId(handle) };
        Self {
            handle: Some(Handle(handle)),
            code: None,
            pid,
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn poll(&mut self) -> Option<i32> {
        if self.code.is_some() {
            return self.code;
        }
        let h = self.handle.as_ref()?;
        // SAFETY: valid handle.
        if unsafe { WaitForSingleObject(h.0, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: valid handle.
        unsafe { GetExitCodeProcess(h.0, &mut code) };
        self.code = Some(code as i32);
        self.code
    }

    pub fn wait(&mut self, timeout: Duration) -> Option<i32> {
        if let Some(h) = &self.handle {
            // SAFETY: valid handle.
            unsafe { WaitForSingleObject(h.0, millis(Some(timeout))) };
        }
        self.poll()
    }

    pub fn terminate(&mut self) {
        if self.poll().is_none()
            && let Some(h) = &self.handle
        {
            // SAFETY: valid handle.
            unsafe { TerminateProcess(h.0, 1) };
        }
    }

    pub fn close(&mut self) {
        if self.handle.is_some() {
            self.poll();
            self.handle = None;
        }
    }
}

/// What `ShellExecuteExW` is called with (Python `launch.ShellExecuteParams`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellExecuteParams {
    pub verb: String,
    pub file: String,
    pub parameters: String,
    pub directory: Option<String>,
    pub show: i32,
    pub mask: u32,
}

/// ShellExecuteExW on the calling thread (blocks while the UAC prompt is open). The error is
/// `ERROR_CANCELLED` (1223) when the user declines the prompt.
pub fn shell_execute(params: &ShellExecuteParams) -> io::Result<ProcessHandle> {
    let verb = wide(&params.verb);
    let file = wide(&params.file);
    let parameters = wide(&params.parameters);
    let directory = params.directory.as_ref().map(wide);
    // SAFETY: COM initialisation of this thread, balanced below.
    let com = unsafe { CoInitializeEx(null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
    let result = (|| {
        // SAFETY: zeroed SHELLEXECUTEINFOW is valid; the strings outlive the call.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = params.mask;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.lpDirectory = directory.as_ref().map_or(null(), |d| d.as_ptr());
        info.nShow = params.show;
        // SAFETY: valid structure.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let err = last_error();
            if err == ERROR_CANCELLED {
                return Err(io::Error::from_raw_os_error(ERROR_CANCELLED as i32));
            }
            return Err(win_error(err));
        }
        if !valid(info.hProcess) {
            return Err(io::Error::other(t("ShellExecuteExW 没有返回进程句柄")));
        }
        // SAFETY: a fresh process handle we own.
        Ok(unsafe { ProcessHandle::new(info.hProcess) })
    })();
    if com >= 0 {
        // SAFETY: balances the successful CoInitializeEx (S_OK / S_FALSE).
        unsafe { CoUninitialize() };
    }
    result
}

// --------------------------------------------------------------------------- volumes

/// Unprivileged facts about `X:`: drive type, DOS device target, filesystem, geometry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeDetails {
    pub drive_type: u32,
    pub device: String,
    pub fs_type: String,
    pub label: String,
    pub serial: String,
    pub bytes_per_sector: u64,
    pub cluster_size: u64,
}

impl VolumeDetails {
    pub fn scalars(&self) -> Scalars {
        let mut m = Scalars::new();
        m.insert("drive_type".into(), self.drive_type.into());
        m.insert("device".into(), self.device.clone().into());
        m.insert("fs_type".into(), self.fs_type.clone().into());
        m.insert("label".into(), self.label.clone().into());
        m.insert("serial".into(), self.serial.clone().into());
        m.insert("bytes_per_sector".into(), self.bytes_per_sector.into());
        m.insert("cluster_size".into(), self.cluster_size.into());
        m
    }
}

pub fn volume_details(volume: &str) -> VolumeDetails {
    let upper = volume.to_uppercase();
    let root = wide(format!("{upper}\\"));
    let dos = wide(&upper);
    let mut target = vec![0u16; 1024];
    let mut fs = vec![0u16; 261];
    let mut label = vec![0u16; 261];
    let (mut serial, mut maxlen, mut fsflags) = (0u32, 0u32, 0u32);
    let (mut spc, mut bps, mut nfree, mut nclusters) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: buffers sized as passed.
    unsafe {
        let device = if QueryDosDeviceW(dos.as_ptr(), target.as_mut_ptr(), 1024) != 0 {
            from_wide(&target)
        } else {
            String::new()
        };
        let ok = GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            261,
            &mut serial,
            &mut maxlen,
            &mut fsflags,
            fs.as_mut_ptr(),
            261,
        ) != 0;
        let geometry = GetDiskFreeSpaceW(root.as_ptr(), &mut spc, &mut bps, &mut nfree, &mut nclusters) != 0;
        VolumeDetails {
            drive_type: GetDriveTypeW(root.as_ptr()),
            device,
            fs_type: if ok { from_wide(&fs) } else { String::new() },
            label: if ok { from_wide(&label) } else { String::new() },
            serial: if ok { format!("{serial:08X}") } else { String::new() },
            bytes_per_sector: if geometry { u64::from(bps) } else { 0 },
            cluster_size: if geometry { u64::from(spc) * u64::from(bps) } else { 0 },
        }
    }
}

/// FSCTL_GET_NTFS_VOLUME_DATA on `\\.\X:` opened read-only (needs administrator rights).
pub fn ntfs_volume_data(volume: &str) -> io::Result<Scalars> {
    let path = format!("\\\\.\\{}", volume.to_uppercase());
    let w = wide(&path);
    // SAFETY: valid path.
    let h = unsafe {
        CreateFileW(w.as_ptr(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut())
    };
    if !valid(h) {
        return Err(io::Error::last_os_error());
    }
    let h = Handle(h);
    let raw = h.ioctl(FSCTL_GET_NTFS_VOLUME_DATA, &[], 96)?;
    if raw.len() < 96 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short NTFS_VOLUME_DATA_BUFFER"));
    }
    let i64_at = |o: usize| i64::from_le_bytes(raw[o..o + 8].try_into().expect("8 bytes"));
    let u32_at = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().expect("4 bytes"));
    // NTFS_VOLUME_DATA_BUFFER: serial 0, sectors 8, total clusters 16, free 24, reserved 32,
    // bytes/sector 40, bytes/cluster 44, bytes/record 48, clusters/record 52, MFT valid length 56,
    // MFT start LCN 64, MFT2 start 72, MFT zone 80 / 88
    let mut m = Scalars::new();
    m.insert("record_size".into(), u32_at(48).into());
    m.insert("total_clusters".into(), i64_at(16).into());
    m.insert("free_clusters".into(), i64_at(24).into());
    m.insert("mft_start_lcn".into(), i64_at(64).into());
    m.insert("mft_valid_data_length".into(), i64_at(56).into());
    Ok(m)
}
