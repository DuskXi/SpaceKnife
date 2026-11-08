//! Win32 plumbing for file operations (port of `ops/_win.py`).
//!
//! * Restart Manager: which processes hold files open (`RmGetList`) and their friendly names.
//! * The shell's copy engine (`IFileOperation`) moving items to the recycle bin without any
//!   window: `FOF_NO_UI` plus `FOF_ALLOWUNDO`, never `FOF_WANTNUKEWARNING`. A progress sink
//!   refuses every deletion the engine announces without "recycle" (`PreDeleteItem` lacking
//!   `TSF_DELETE_RECYCLE_IF_POSSIBLE`: the item would be destroyed) and reports where each
//!   recycled item landed (`PostDeleteItem`'s newly created item: the `$R` file). The COM
//!   objects are driven through their vtables by hand (no `windows` crate).
//! * Recycle-bin facts: per-volume settings (HKCU `BitBucket`), the current user's SID, the bin's
//!   current usage. Nothing here shows a window or asks for elevation.

#![allow(clippy::upper_case_acronyms)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetFileInformationByHandleEx,
    GetLongPathNameW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows_sys::Win32::System::RestartManager::{
    RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources, RmStartSession,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Shell::{
    SHChangeNotify, SHCreateItemFromParsingName, SHQUERYRBINFO, SHQueryRecycleBinW,
};
use windows_sys::core::{GUID, HRESULT};

pub const S_OK: HRESULT = 0;
pub const E_ABORT: HRESULT = 0x8000_4004_u32 as i32;
pub const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
const ERROR_MORE_DATA: u32 = 234;

pub const DRIVE_REMOVABLE: u32 = 2;
pub const DRIVE_FIXED: u32 = 3;
pub const DRIVE_REMOTE: u32 = 4;

const TSF_DELETE_RECYCLE_IF_POSSIBLE: u32 = 0x80;
const FOF_SILENT: u32 = 0x4;
const FOF_NOCONFIRMATION: u32 = 0x10;
const FOF_ALLOWUNDO: u32 = 0x40;
const FOF_NOCONFIRMMKDIR: u32 = 0x200;
const FOF_NOERRORUI: u32 = 0x400;
const FOF_NORECURSEREPARSE: u32 = 0x8000;
const FOFX_RECYCLEONDELETE: u32 = 0x0008_0000;
/// No FOF_WANTNUKEWARNING: it would show a dialog (and overrides FOF_NOCONFIRMATION).
const RECYCLE_FLAGS: u32 = FOF_ALLOWUNDO
    | FOF_NOCONFIRMATION
    | FOF_NOERRORUI
    | FOF_SILENT
    | FOF_NOCONFIRMMKDIR
    | FOF_NORECURSEREPARSE
    | FOFX_RECYCLEONDELETE;
const SIGDN_FILESYSPATH: u32 = 0x8005_8000;
const RM_REGISTER_CHUNK: usize = 1000;

const CLSID_FILE_OPERATION: GUID = GUID::from_u128(0x3ad05575_8857_4850_9277_11b85bdb8e09);
const IID_IFILE_OPERATION: GUID = GUID::from_u128(0x947aab5f_0a5c_4c13_b4d6_4bf7836fc9f8);
const IID_IFILE_OPERATION_PROGRESS_SINK: GUID =
    GUID::from_u128(0x04b0f1a7_9490_44bc_96e1_4296a31252e2);
const IID_ISHELL_ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);

/// The Win32 error inside an `HRESULT_FROM_WIN32` value (else the HRESULT itself).
pub fn hresult_code(hr: HRESULT) -> u32 {
    let value = hr as u32;
    if value >> 16 == 0x8007 {
        value & 0xFFFF
    } else {
        value
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: a NUL-terminated string from the system.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

// --------------------------------------------------------------------------- volumes

/// Mount point of the volume holding `path` (`C:\`, `C:\mnt\disk\`, `\\host\share\`).
pub fn volume_root(path: &str) -> io::Result<String> {
    let abs = super::paths::absolute(Path::new(path));
    let w = wide_path(&abs);
    let mut buf = vec![0u16; 1024];
    // SAFETY: valid buffers.
    if unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let root = from_wide(&buf);
    Ok(if root.ends_with('\\') {
        root
    } else {
        root + "\\"
    })
}

/// `{GUID}` of a mounted volume, or "" (network shares, SUBST drives).
pub fn volume_guid(root: &str) -> String {
    let w = wide(root);
    let mut buf = vec![0u16; 128];
    // SAFETY: valid buffers.
    if unsafe { GetVolumeNameForVolumeMountPointW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
        == 0
    {
        return String::new();
    }
    let name = from_wide(&buf); // \\?\Volume{GUID}\
    match (name.find('{'), name.find('}')) {
        (Some(a), Some(b)) if a < b => name[a..=b].to_string(),
        _ => String::new(),
    }
}

pub fn drive_type(root: &str) -> u32 {
    let w = wide(root);
    // SAFETY: a NUL-terminated string.
    unsafe { GetDriveTypeW(w.as_ptr()) }
}

/// Total bytes of the volume at `root`.
pub fn disk_total(root: &str) -> io::Result<u64> {
    let w = wide(root);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: valid out pointers.
    if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(total)
}

/// What the shell does with an item deleted on one volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinSettings {
    /// NukeOnDelete or the NoRecycleFiles policy: deleted items are destroyed
    pub nuke: bool,
    /// the bin's capacity on the volume: a larger item is destroyed, not recycled
    pub max_bytes: u64,
    /// read from the registry (false: Windows' default for the volume size)
    pub configured: bool,
}

const BITBUCKET: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume";
const POLICIES: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";

fn dword(hive: HKEY, key: &str, name: &str) -> Option<u32> {
    let (k, n) = (wide(key), wide(name));
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: valid buffers; RRF_RT_REG_DWORD checks the type.
    let err = unsafe {
        RegGetValueW(
            hive,
            k.as_ptr(),
            n.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut value as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (err == 0).then_some(value)
}

/// Windows' default bin capacity: 10 % of the first 40 GB plus 5 % of the rest (MB granular).
pub fn default_bin_bytes(total: u64) -> u64 {
    let mb = total >> 20;
    let first = mb.min(40 * 1024);
    (first / 10 + (mb - first) / 20) << 20
}

/// The recycle-bin settings of the volume `{guid}` (`total` bytes): HKCU BitBucket, policies.
pub fn bin_settings(guid: &str, total: u64) -> BinSettings {
    let hives = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE];
    let nuke = hives
        .iter()
        .any(|&h| dword(h, POLICIES, "NoRecycleFiles") == Some(1));
    let key = format!("{BITBUCKET}\\{guid}");
    let nuke_value = dword(HKEY_CURRENT_USER, &key, "NukeOnDelete");
    let max_mb = dword(HKEY_CURRENT_USER, &key, "MaxCapacity");
    let configured = max_mb.is_some();
    let mut max_bytes = max_mb.map_or_else(|| default_bin_bytes(total), |m| (m as u64) << 20);
    for &h in &hives {
        if let Some(percent) = dword(h, POLICIES, "RecycleBinSize") {
            max_bytes = max_bytes.min(total * percent.min(100) as u64 / 100);
        }
    }
    BinSettings {
        nuke: nuke || nuke_value == Some(1),
        max_bytes,
        configured,
    }
}

/// (bytes, items) in the recycle bin of the volume at `root`; (-1, -1) when unknown.
pub fn bin_usage(root: &str) -> (i64, i64) {
    let w = wide(root);
    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        i64Size: 0,
        i64NumItems: 0,
    };
    // SAFETY: a sized struct.
    if unsafe { SHQueryRecycleBinW(w.as_ptr(), &mut info) } != S_OK {
        return (-1, -1);
    }
    (info.i64Size, info.i64NumItems)
}

/// String SID of the user this process runs as (the name of its folder in `$Recycle.Bin`).
pub fn user_sid() -> io::Result<String> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(s) = SID.get() {
        return Ok(s.clone());
    }
    let mut token: HANDLE = null_mut();
    // SAFETY: plain Win32 calls with valid buffers; the handle and the string are freed.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut need = 0u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut need);
            let mut buf = vec![0u64; (need as usize).max(64).div_ceil(8)];
            if GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * 8) as u32,
                &mut need,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let s = from_wide_ptr(text);
            LocalFree(text as *mut c_void);
            Ok(s)
        })();
        CloseHandle(token);
        let s = result?;
        Ok(SID.get_or_init(|| s).clone())
    }
}

/// Tell Explorer windows that `path` appeared (SHChangeNotify; no window, never blocks).
pub fn notify_changed(path: &str, folder: bool) {
    const SHCNE_CREATE: i32 = 0x0000_0002;
    const SHCNE_MKDIR: i32 = 0x0000_0008;
    const SHCNE_UPDATEDIR: i32 = 0x0000_1000;
    const SHCNF_PATHW: u32 = 0x0005;
    let p = wide(path);
    let parent = Path::new(path)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let d = wide(&parent);
    // SAFETY: NUL-terminated strings.
    unsafe {
        SHChangeNotify(
            if folder { SHCNE_MKDIR } else { SHCNE_CREATE },
            SHCNF_PATHW,
            p.as_ptr() as *const c_void,
            null(),
        );
        SHChangeNotify(
            SHCNE_UPDATEDIR,
            SHCNF_PATHW,
            d.as_ptr() as *const c_void,
            null(),
        );
    }
}

/// The stored name of what `path` opens: its path below the volume's root as the file system
/// stores it (normalised, `FileNormalizedNameInfo`, else `FileNameInfo`), or None when it cannot
/// be read. Opened with no access rights and without following a reparse point at the end.
// TODO(scan): use the scan worker's `final_name` once it is ported.
pub fn final_name(path: &str) -> Option<String> {
    const SHARE_ALL: u32 = 7;
    const OPEN_EXISTING: u32 = 3;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_NORMALIZED_NAME_INFO: i32 = 24;
    const FILE_NAME_INFO: i32 = 2;
    let w = wide(&long_path(path));
    // SAFETY: plain Win32 calls; the handle is closed.
    unsafe {
        let h = CreateFileW(
            w.as_ptr(),
            0,
            SHARE_ALL,
            null(),
            OPEN_EXISTING,
            BACKUP_SEMANTICS | OPEN_REPARSE_POINT,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return None;
        }
        // exactly 64 KiB: WinBtrfs refuses a buffer of a few bytes more with ERROR_MORE_DATA
        let mut buf = vec![0u32; 65536 / 4];
        let ok = GetFileInformationByHandleEx(
            h,
            FILE_NORMALIZED_NAME_INFO,
            buf.as_mut_ptr() as *mut c_void,
            65536,
        ) != 0
            || GetFileInformationByHandleEx(
                h,
                FILE_NAME_INFO,
                buf.as_mut_ptr() as *mut c_void,
                65536,
            ) != 0;
        CloseHandle(h);
        if !ok {
            return None;
        }
        let n = buf[0] as usize / 2;
        let units = std::slice::from_raw_parts((buf.as_ptr() as *const u16).add(2), n.min(32766));
        Some(String::from_utf16_lossy(units))
    }
}

/// `path` as Win32 calls must get it: with the `\\?\` prefix when it is long, or when a name in
/// it ends in a dot or space (Win32 trims those).
pub fn long_path(path: &str) -> String {
    if path.starts_with("\\\\?\\") {
        return path.to_string();
    }
    if path.encode_utf16().count() < 240
        && !(path.contains(".\\")
            || path.contains(" \\")
            || path.ends_with('.')
            || path.ends_with(' '))
    {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix("\\\\") {
        return format!("\\\\?\\UNC\\{rest}");
    }
    format!("\\\\?\\{path}")
}

// --------------------------------------------------------------------------- Restart Manager

fn image_name(pid: u32) -> String {
    // SAFETY: plain Win32 calls; the handle is closed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) != 0;
        CloseHandle(h);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or("").to_string()
    }
}

fn rm_error(what: &str, err: u32) -> io::Error {
    io::Error::new(
        io::Error::from_raw_os_error(err as i32).kind(),
        format!("{what} failed ({err})"),
    )
}

/// `(pid, friendly name)` of every process using any of `files` (one Restart Manager session).
/// Fails when the Restart Manager cannot be used (no session available, bad names).
pub fn rm_lockers(files: &[String]) -> io::Result<Vec<(u32, String)>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut session = 0u32;
    let mut key = [0u16; 33];
    // SAFETY: valid buffers; the session is ended.
    let err = unsafe { RmStartSession(&mut session, 0, key.as_mut_ptr()) };
    if err != 0 {
        return Err(rm_error("RmStartSession", err));
    }
    let result = (|| {
        for chunk in files.chunks(RM_REGISTER_CHUNK) {
            let names: Vec<Vec<u16>> = chunk.iter().map(|f| wide(f)).collect();
            let ptrs: Vec<*const u16> = names.iter().map(|n| n.as_ptr()).collect();
            // SAFETY: an array of NUL-terminated strings.
            let err = unsafe {
                RmRegisterResources(
                    session,
                    ptrs.len() as u32,
                    ptrs.as_ptr(),
                    0,
                    null(),
                    0,
                    null(),
                )
            };
            if err != 0 {
                return Err(rm_error("RmRegisterResources", err));
            }
        }
        let mut needed = 0u32;
        for _ in 0..8 {
            // the list may grow between the calls
            let mut buf: Vec<RM_PROCESS_INFO> =
                vec![RM_PROCESS_INFO::default(); needed.max(1) as usize];
            let mut count = needed;
            let mut reasons = 0u32;
            // SAFETY: `count` entries of room.
            let err = unsafe {
                RmGetList(
                    session,
                    &mut needed,
                    &mut count,
                    buf.as_mut_ptr(),
                    &mut reasons,
                )
            };
            if err == 0 {
                let mut out = Vec::new();
                for info in &buf[..count as usize] {
                    let pid = info.Process.dwProcessId;
                    let mut name = from_wide(&info.strAppName);
                    if name.is_empty() {
                        name = from_wide(&info.strServiceShortName);
                    }
                    if name.is_empty() {
                        name = image_name(pid);
                    }
                    if name.is_empty() {
                        name = format!("PID {pid}");
                    }
                    out.push((pid, name));
                }
                return Ok(out);
            }
            if err != ERROR_MORE_DATA {
                return Err(rm_error("RmGetList", err));
            }
        }
        Err(io::Error::other("RmGetList kept growing"))
    })();
    // SAFETY: the session started above.
    unsafe { RmEndSession(session) };
    result
}

// --------------------------------------------------------------------------- COM by hand

type Unknown = *mut c_void;

/// Method `index` of the COM object `obj`, as a function pointer of type `F`.
///
/// # Safety
/// `obj` is a live COM object whose vtable has a method of type `F` at `index`.
unsafe fn method<F: Copy>(obj: Unknown, index: usize) -> F {
    unsafe {
        let vtbl = *(obj as *const *const usize);
        let f = *vtbl.add(index);
        std::mem::transmute_copy::<usize, F>(&f)
    }
}

unsafe fn release(obj: Unknown) {
    if !obj.is_null() {
        unsafe {
            let f: unsafe extern "system" fn(Unknown) -> u32 = method(obj, 2);
            f(obj);
        }
    }
}

/// File-system path of an IShellItem (borrowed pointer), or None.
unsafe fn item_path(item: Unknown) -> Option<String> {
    if item.is_null() {
        return None;
    }
    unsafe {
        let get_display_name: unsafe extern "system" fn(Unknown, u32, *mut *mut u16) -> HRESULT =
            method(item, 5);
        let mut out: *mut u16 = null_mut();
        if get_display_name(item, SIGDN_FILESYSPATH, &mut out) < 0 || out.is_null() {
            return None;
        }
        let s = from_wide_ptr(out);
        CoTaskMemFree(out as *const c_void);
        Some(s)
    }
}

/// [`item_path`] kept exactly, as a trash reference ([`super::trash::ref_of`]): the new item's
/// name can hold a lone surrogate (the bin keeps the original extension).
unsafe fn item_ref(item: Unknown) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    if item.is_null() {
        return None;
    }
    unsafe {
        let get_display_name: unsafe extern "system" fn(Unknown, u32, *mut *mut u16) -> HRESULT =
            method(item, 5);
        let mut out: *mut u16 = null_mut();
        if get_display_name(item, SIGDN_FILESYSPATH, &mut out) < 0 || out.is_null() {
            return None;
        }
        let mut n = 0;
        while *out.add(n) != 0 {
            n += 1;
        }
        let os = std::ffi::OsString::from_wide(std::slice::from_raw_parts(out, n));
        CoTaskMemFree(out as *const c_void);
        Some(super::trash::ref_of(Path::new(&os)))
    }
}

/// What the engine did with one item handed to [`recycle`].
#[derive(Clone, Debug)]
pub struct Recycled {
    pub path: String,
    /// HRESULT of the deletion (0 = done); E_ABORT when refused or cancelled
    pub hr: HRESULT,
    /// the item's new path in the bin (`$R…`, a trash reference: [`super::trash::ref_of`]),
    /// None if not recycled
    pub recycled: Option<String>,
    /// the engine wanted to destroy it (no recycle): refused, untouched
    pub refused: bool,
    /// the engine got to this item at all
    pub reached: bool,
}

/// One shell operation: an entry per path, the operation's HRESULT and what was refused.
#[derive(Clone, Debug)]
pub struct RecycleRun {
    pub items: Vec<Recycled>,
    pub hr: HRESULT,
    pub cancelled: bool,
    /// paths the engine wanted to destroy (any level)
    pub refused: Vec<String>,
    /// (path, new path) of deletions reported for paths not queued
    pub unmatched: Vec<(String, Option<String>)>,
}

/// `path` with its 8.3 short names (`RUNNER~1`) expanded to the long ones, as the shell reports
/// items; None when it has none, or it does not exist.
pub fn long_names(path: &str) -> Option<String> {
    if !path.contains('~') {
        return None;
    }
    let w = wide(&long_path(path));
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: w is NUL-terminated; buf holds buf.len() units.
        let n =
            unsafe { GetLongPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            let got = String::from_utf16_lossy(&buf[..n]);
            let got = match got.strip_prefix("\\\\?\\") {
                Some(rest) if !path.starts_with("\\\\?\\") => match rest.strip_prefix("UNC\\") {
                    Some(unc) => format!("\\\\{unc}"),
                    None => rest.to_string(),
                },
                _ => got,
            };
            return (got != path).then_some(got);
        }
        buf.resize(n, 0);
    }
}

pub(crate) fn path_key(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    super::paths::nt_normcase(&super::paths::nt_normpath(path))
}

/// The progress sink: a COM object whose first field is its vtable.
#[repr(C)]
struct Sink<'a> {
    vtbl: *const SinkVtbl,
    cancel: Option<&'a (dyn Fn() -> bool + Sync)>,
    on_item: &'a mut (dyn FnMut(usize, &Recycled) + Send),
    cancelled: bool,
    items: Vec<Recycled>,
    index: HashMap<String, usize>,
    refused_other: Vec<String>,
    unmatched: Vec<(String, Option<String>)>,
}

type P = *mut c_void;
type W = *const u16;

#[repr(C)]
struct SinkVtbl {
    query_interface: unsafe extern "system" fn(P, *const GUID, *mut P) -> HRESULT,
    add_ref: unsafe extern "system" fn(P) -> u32,
    release: unsafe extern "system" fn(P) -> u32,
    start_operations: unsafe extern "system" fn(P) -> HRESULT,
    finish_operations: unsafe extern "system" fn(P, HRESULT) -> HRESULT,
    pre_rename_item: unsafe extern "system" fn(P, u32, P, W) -> HRESULT,
    post_rename_item: unsafe extern "system" fn(P, u32, P, W, HRESULT, P) -> HRESULT,
    pre_move_item: unsafe extern "system" fn(P, u32, P, P, W) -> HRESULT,
    post_move_item: unsafe extern "system" fn(P, u32, P, P, W, HRESULT, P) -> HRESULT,
    pre_copy_item: unsafe extern "system" fn(P, u32, P, P, W) -> HRESULT,
    post_copy_item: unsafe extern "system" fn(P, u32, P, P, W, HRESULT, P) -> HRESULT,
    pre_delete_item: unsafe extern "system" fn(P, u32, P) -> HRESULT,
    post_delete_item: unsafe extern "system" fn(P, u32, P, HRESULT, P) -> HRESULT,
    pre_new_item: unsafe extern "system" fn(P, u32, P, W) -> HRESULT,
    post_new_item: unsafe extern "system" fn(P, u32, P, W, W, u32, HRESULT, P) -> HRESULT,
    update_progress: unsafe extern "system" fn(P, u32, u32) -> HRESULT,
    reset_timer: unsafe extern "system" fn(P) -> HRESULT,
    pause_timer: unsafe extern "system" fn(P) -> HRESULT,
    resume_timer: unsafe extern "system" fn(P) -> HRESULT,
}

unsafe extern "system" fn sink_qi(this: P, riid: *const GUID, ppv: *mut P) -> HRESULT {
    unsafe {
        let want = &*riid;
        let same = |g: &GUID| {
            g.data1 == want.data1
                && g.data2 == want.data2
                && g.data3 == want.data3
                && g.data4 == want.data4
        };
        if same(&IID_IUNKNOWN) || same(&IID_IFILE_OPERATION_PROGRESS_SINK) {
            *ppv = this;
            return S_OK;
        }
        *ppv = null_mut();
        E_NOINTERFACE
    }
}
unsafe extern "system" fn sink_ref(_: P) -> u32 {
    1
}
unsafe extern "system" fn ok0(_: P) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_finish(_: P, _: HRESULT) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_pre_rename(_: P, _: u32, _: P, _: W) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_rename(_: P, _: u32, _: P, _: W, _: HRESULT, _: P) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_pre_move(_: P, _: u32, _: P, _: P, _: W) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_move(
    _: P,
    _: u32,
    _: P,
    _: P,
    _: W,
    _: HRESULT,
    _: P,
) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_pre_new(_: P, _: u32, _: P, _: W) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_new(
    _: P,
    _: u32,
    _: P,
    _: W,
    _: W,
    _: u32,
    _: HRESULT,
    _: P,
) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_progress(_: P, _: u32, _: u32) -> HRESULT {
    S_OK
}

unsafe extern "system" fn sink_pre_delete(this: P, flags: u32, item: P) -> HRESULT {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let sink = &mut *(this as *mut Sink<'_>);
        let path = item_path(item).unwrap_or_default();
        let at = sink.index.get(&path_key(&path)).copied();
        if let Some(i) = at {
            sink.items[i].reached = true;
        }
        if sink.cancel.is_some_and(|c| c()) {
            sink.cancelled = true;
            return E_ABORT;
        }
        if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE == 0 {
            // would be destroyed: never let it happen
            match at {
                Some(i) => sink.items[i].refused = true,
                None => sink.refused_other.push(path),
            }
            return E_ABORT;
        }
        S_OK
    }));
    run.unwrap_or(E_ABORT) // never let an error turn into "go ahead"
}

unsafe extern "system" fn sink_post_delete(
    this: P,
    _flags: u32,
    item: P,
    hr: HRESULT,
    created: P,
) -> HRESULT {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let sink = &mut *(this as *mut Sink<'_>);
        let path = item_path(item).unwrap_or_default();
        match sink.index.get(&path_key(&path)).copied() {
            None => sink.unmatched.push((path, item_ref(created))),
            Some(i) if !sink.items[i].refused => {
                let entry = &mut sink.items[i];
                entry.reached = true;
                entry.hr = hr;
                entry.recycled = item_ref(created);
                let snapshot = entry.clone();
                (sink.on_item)(i, &snapshot);
            }
            Some(_) => {}
        }
    }));
    S_OK
}

static SINK_VTBL: SinkVtbl = SinkVtbl {
    query_interface: sink_qi,
    add_ref: sink_ref,
    release: sink_ref,
    start_operations: ok0,
    finish_operations: ok_finish,
    pre_rename_item: ok_pre_rename,
    post_rename_item: ok_post_rename,
    pre_move_item: ok_pre_move,
    post_move_item: ok_post_move,
    pre_copy_item: ok_pre_move,
    post_copy_item: ok_post_move,
    pre_delete_item: sink_pre_delete,
    post_delete_item: sink_post_delete,
    pre_new_item: ok_pre_new,
    post_new_item: ok_post_new,
    update_progress: ok_progress,
    reset_timer: ok0,
    pause_timer: ok0,
    resume_timer: ok0,
};

fn recycle_sta(
    paths: &[PathBuf],
    cancel: Option<&(dyn Fn() -> bool + Sync)>,
    on_item: &mut (dyn FnMut(usize, &Recycled) + Send),
) -> RecycleRun {
    let shown: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let items: Vec<Recycled> = shown
        .iter()
        .map(|p| Recycled {
            path: p.clone(),
            hr: E_ABORT,
            recycled: None,
            refused: false,
            reached: false,
        })
        .collect();
    // the shell reports items by their long names: a request through a short name (`RUNNER~1`)
    // is found under both
    let index = shown
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            let long = long_names(p).map(|l| (path_key(&l), i));
            std::iter::once((path_key(p), i)).chain(long)
        })
        .collect();
    let mut sink = Box::new(Sink {
        vtbl: &SINK_VTBL,
        cancel,
        on_item,
        cancelled: false,
        items,
        index,
        refused_other: Vec::new(),
        unmatched: Vec::new(),
    });
    // SAFETY: COM on this (fresh) thread; every object is released; the sink outlives the
    // operation (it is unadvised before it is dropped).
    unsafe {
        let init = CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let run = (|| {
            let mut op: Unknown = null_mut();
            let hr = CoCreateInstance(
                &CLSID_FILE_OPERATION,
                null_mut(),
                CLSCTX_ALL,
                &IID_IFILE_OPERATION,
                &mut op,
            );
            let done = |hr: HRESULT, sink: &mut Sink<'_>| RecycleRun {
                items: std::mem::take(&mut sink.items),
                hr,
                cancelled: false,
                refused: Vec::new(),
                unmatched: Vec::new(),
            };
            if hr < 0 || op.is_null() {
                return done(hr, &mut sink);
            }
            let set_flags: unsafe extern "system" fn(Unknown, u32) -> HRESULT = method(op, 5);
            let advise: unsafe extern "system" fn(Unknown, P, *mut u32) -> HRESULT = method(op, 3);
            let unadvise: unsafe extern "system" fn(Unknown, u32) -> HRESULT = method(op, 4);
            let delete_item: unsafe extern "system" fn(Unknown, Unknown, P) -> HRESULT =
                method(op, 18);
            let perform: unsafe extern "system" fn(Unknown) -> HRESULT = method(op, 21);
            let hr = set_flags(op, RECYCLE_FLAGS);
            if hr < 0 {
                release(op);
                return done(hr, &mut sink);
            }
            let mut cookie = 0u32;
            let sink_ptr: *mut Sink<'_> = &mut *sink;
            let hr = advise(op, sink_ptr as P, &mut cookie);
            if hr < 0 {
                release(op);
                return done(hr, &mut sink);
            }
            let mut queued = 0;
            for (i, path) in paths.iter().enumerate() {
                // as given: a name with a lone surrogate is parsed exactly
                let w = wide_path(path);
                let mut item: Unknown = null_mut();
                let hr = SHCreateItemFromParsingName(
                    w.as_ptr(),
                    null_mut(),
                    &IID_ISHELL_ITEM,
                    &mut item,
                );
                if hr < 0 || item.is_null() {
                    let s = &mut *sink_ptr;
                    s.items[i].hr = hr;
                    s.items[i].reached = true;
                    continue;
                }
                let hr = delete_item(op, item, null_mut());
                release(item);
                if hr < 0 {
                    let s = &mut *sink_ptr;
                    s.items[i].hr = hr;
                    s.items[i].reached = true;
                } else {
                    queued += 1;
                }
            }
            let result = if queued > 0 { perform(op) } else { S_OK };
            unadvise(op, cookie);
            release(op);
            let sink = &mut *sink_ptr;
            let mut refused: Vec<String> = sink
                .items
                .iter()
                .filter(|e| e.refused)
                .map(|e| e.path.clone())
                .collect();
            refused.append(&mut sink.refused_other);
            RecycleRun {
                items: std::mem::take(&mut sink.items),
                hr: result,
                cancelled: sink.cancelled,
                refused,
                unmatched: std::mem::take(&mut sink.unmatched),
            }
        })();
        if init >= 0 {
            // S_OK / S_FALSE: balance it (RPC_E_CHANGED_MODE: not ours to undo)
            CoUninitialize();
        }
        run
    }
}

/// Move `paths` to the recycle bin in one shell operation, on a fresh STA thread.
///
/// Returns one [`Recycled`] per path (same order) and the operation's HRESULT. `on_item` is
/// called on that thread right after each item was recycled (with its index). `cancel` is
/// polled before each item: once it returns true the rest is not touched. A refused destruction
/// aborts the whole operation (what was not reached yet is untouched; the caller retries it
/// without the refused).
pub fn recycle(
    paths: &[PathBuf],
    cancel: Option<&(dyn Fn() -> bool + Sync)>,
    on_item: &mut (dyn FnMut(usize, &Recycled) + Send),
) -> io::Result<RecycleRun> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("spaceknife-recycle".into())
            .spawn_scoped(s, || recycle_sta(paths, cancel, on_item))?
            .join()
            .map_err(|_| io::Error::other("the recycle thread panicked"))
    })
}
