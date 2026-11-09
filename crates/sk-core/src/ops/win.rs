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
//!   current usage (from its `$I` records). Nothing here shows a window or asks for elevation.
//! * Moving into the bin by this process itself where the bin is the user's own folder: a `$I`
//!   record and a rename to `$R` ([`recycle_into`]), what the shell does without walking anything.

#![allow(clippy::upper_case_acronyms)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

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
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCreateItemFromParsingName};
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

/// (bytes, items) in this user's recycle bin on the volume at `root`, as its `$I` records tell
/// (each holds its item's size): the bin's folder listed, each record read once (kept by its
/// path, length and time: a record never changes), the items themselves never walked.
/// `SHQueryRecycleBin` walked every folder in the bin each time it was asked (60 000 operations
/// for a bin holding one folder of 33 000 files), and the drawer asks after every change. (0, 0)
/// without a bin folder; (-1, -1) when it cannot be read.
pub fn bin_usage(root: &str) -> (i64, i64) {
    use std::time::SystemTime;
    type Known = HashMap<PathBuf, (u64, Option<SystemTime>, i64)>;
    static RECORDS: std::sync::Mutex<Option<Known>> = std::sync::Mutex::new(None);
    let _phase = super::probe::phase("bin_usage");
    let Ok(sid) = user_sid() else {
        return (-1, -1);
    };
    let folder = Path::new(root).join("$Recycle.Bin").join(sid);
    super::probe::call(super::probe::Call::List);
    let listing = match std::fs::read_dir(&folder) {
        Ok(it) => it,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (0, 0),
        Err(_) => return (-1, -1),
    };
    let mut known = RECORDS.lock().unwrap_or_else(|p| p.into_inner());
    let known = known.get_or_insert_with(HashMap::new);
    if known.len() > 200_000 {
        known.clear();
    }
    let (mut bytes, mut items) = (0i64, 0i64);
    for entry in listing.flatten() {
        let name = entry.file_name();
        let named = name.to_string_lossy();
        if !named.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("$I")) {
            continue;
        }
        // the listing's own facts (no call of its own on Windows)
        let Ok(meta) = entry.metadata() else { continue };
        let (len, when) = (meta.len(), meta.modified().ok());
        let path = entry.path();
        let size = match known.get(&path) {
            Some(&(l, w, s)) if l == len && w == when => s,
            _ => {
                super::probe::call(super::probe::Call::Open);
                let Ok(data) = std::fs::read(&path) else {
                    continue;
                };
                let Ok((s, _, _)) =
                    super::trash::parse_recycle_record(&data[..data.len().min(1 << 16)])
                else {
                    continue;
                };
                known.insert(path, (len, when, s));
                s
            }
        };
        bytes += size.max(0);
        items += 1;
    }
    (bytes, items)
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

// --------------------------------------------------------------------------- the bin, directly

/// The current user's recycle-bin folder on the volume mounted at `root`, when this process may
/// put items into it itself: `$Recycle.Bin` and `$Recycle.Bin\<SID>` both real folders (neither a
/// reparse point), the latter owned by this user (what the shell made it), the former by this
/// user or the system (SYSTEM, Administrators, TrustedInstaller: what Windows and the shell make
/// it). A `$Recycle.Bin` another user made (on a data volume any user may create folders on)
/// lets that user swap this user's folder for a link between this check and the renames: such a
/// volume is left to the shell. None leaves the volume to the shell: it makes the folder when
/// there is none, and knows what to do with one it did not make (FAT has no owners at all).
pub fn own_bin(root: &str) -> Option<PathBuf> {
    use std::os::windows::fs::MetadataExt;
    const DIRECTORY: u32 = 0x10;
    const REPARSE_POINT: u32 = 0x400;
    /// SYSTEM, Administrators, TrustedInstaller
    const TRUSTED: [&str; 3] = [
        "S-1-5-18",
        "S-1-5-32-544",
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
    ];
    let sid = user_sid().ok()?;
    let top = Path::new(root).join("$Recycle.Bin");
    let mine = top.join(&sid);
    for p in [&top, &mine] {
        super::probe::call(super::probe::Call::Stat);
        let attrs = std::fs::symlink_metadata(p).ok()?.file_attributes();
        if attrs & DIRECTORY == 0 || attrs & REPARSE_POINT != 0 {
            return None;
        }
    }
    let top_owner = owner_of(&top)?;
    if !(top_owner.eq_ignore_ascii_case(&sid) || TRUSTED.contains(&top_owner.as_str())) {
        return None;
    }
    owner_of(&mine)?.eq_ignore_ascii_case(&sid).then_some(mine)
}

/// The string SID of the owner of the file or folder `path`.
fn owner_of(path: &Path) -> Option<String> {
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    let w = wide_path(path);
    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: a NUL-terminated path, out pointers; the descriptor is freed below, `owner` points
    // into it.
    unsafe {
        let err = GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        );
        if err != 0 || owner.is_null() {
            if !sd.is_null() {
                LocalFree(sd);
            }
            return None;
        }
        let mut text: *mut u16 = null_mut();
        let named = ConvertSidToStringSidW(owner, &mut text) != 0;
        let owner_sid = if named {
            from_wide_ptr(text)
        } else {
            String::new()
        };
        if !text.is_null() {
            LocalFree(text as *mut c_void);
        }
        LocalFree(sd);
        named.then_some(owner_sid)
    }
}

/// Now, as a FILETIME (100 ns since 1601).
pub fn filetime_now() -> i64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: a valid out pointer.
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) as i64
}

/// A `$I` record as Windows 10 and later write it (version 2): the version, the item's size, when
/// it was deleted (FILETIME), the original path's length in UTF-16 units with its NUL, then the
/// path and the NUL ([`super::trash::parse_recycle_record`] reads it back).
pub fn recycle_record(size: u64, deleted: i64, original: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + 2 * (original.len() + 1));
    out.extend_from_slice(&2i64.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&deleted.to_le_bytes());
    out.extend_from_slice(&((original.len() + 1) as i32).to_le_bytes());
    for u in original.iter().chain(std::iter::once(&0u16)) {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Six characters as the shell picks them for `$R` / `$I` names (letters and digits).
fn bin_name() -> String {
    use std::sync::atomic::AtomicU64;
    static STATE: AtomicU64 = AtomicU64::new(0);
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut x = STATE
        .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        .wrapping_add(seed)
        .wrapping_add(u64::from(std::process::id()) << 32);
    (0..6)
        .map(|_| {
            // splitmix64
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            CHARS[(z % CHARS.len() as u64) as usize] as char
        })
        .collect()
}

/// Move `item` (a file, a folder or a link, not followed) into the bin folder `bin` (on the same
/// volume, [`own_bin`]) as the shell does: its `$I…` record is written first under a name no
/// entry has, then the item is renamed to the `$R…` beside it — a rename only: never replacing
/// anything, never copying (another volume fails). `size` goes into the record. Returns the `$R`
/// path; on failure the record is removed again and the item is where it was.
pub fn recycle_into(item: &Path, bin: &Path, size: u64) -> io::Result<PathBuf> {
    use std::io::Write;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let abs = super::paths::absolute(item);
    // recorded as the shell records it: 8.3 short names (`RUNNER~1`) expanded; a name that is
    // not Unicode exactly as it is
    let original: Vec<u16> = match (abs.to_str(), long_names_of(&abs)) {
        (Some(_), Some(long)) => long.encode_utf16().collect(),
        _ => abs.as_os_str().encode_wide().collect(),
    };
    let record = recycle_record(size, filetime_now(), &original);
    // the shell keeps the extension (a folder's too); a name stays within 255 units
    let ext: Vec<u16> = item
        .extension()
        .map(|e| {
            std::iter::once(b'.' as u16)
                .chain(e.encode_wide())
                .collect::<Vec<u16>>()
        })
        .filter(|e| e.len() <= 240)
        .unwrap_or_default();
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..16 {
        let stem = bin_name();
        let named = |prefix: &str| -> PathBuf {
            let units: Vec<u16> = prefix
                .encode_utf16()
                .chain(stem.encode_utf16())
                .chain(ext.iter().copied())
                .collect();
            bin.join(std::ffi::OsString::from_wide(&units))
        };
        let (info, payload) = (named("$I"), named("$R"));
        super::probe::call(super::probe::Call::Write);
        let record_phase = super::probe::phase("direct.record");
        let mut fh = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = fh.write_all(&record) {
            drop(fh);
            let _ = std::fs::remove_file(&info);
            return Err(e);
        }
        drop(fh);
        drop(record_phase);
        super::probe::call(super::probe::Call::Rename);
        let _rename_phase = super::probe::phase("direct.rename");
        match super::trash::rename_noreplace(item, &payload) {
            Ok(()) => return Ok(payload),
            Err(e) => {
                let _ = std::fs::remove_file(&info);
                if e.kind() == io::ErrorKind::AlreadyExists {
                    last = e; // a `$R` left without its record: another name
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(last)
}

/// Tell Explorer windows what [`recycle_into`] did: the folders the originals were in, and the
/// bin folders, changed (once each: a notice per item cost 0.7 ms each, the shell resolving its
/// path); the bin's icon may be full now. Never blocks, shows nothing.
pub fn notify_recycled(originals: &[String], bins: &[PathBuf]) {
    const SHCNE_UPDATEDIR: i32 = 0x0000_1000;
    const SHCNF_PATHW: u32 = 0x0005;
    let mut folders: Vec<PathBuf> = originals
        .iter()
        .filter_map(|p| Path::new(p).parent().map(Path::to_path_buf))
        .chain(bins.iter().cloned())
        .collect();
    folders.sort();
    folders.dedup();
    for f in &folders {
        let w = wide_path(f);
        // SAFETY: a NUL-terminated string.
        unsafe {
            SHChangeNotify(
                SHCNE_UPDATEDIR,
                SHCNF_PATHW,
                w.as_ptr() as *const c_void,
                null(),
            )
        };
    }
    update_bin_icon();
}

/// `SHUpdateRecycleBinIcon` (shell32; not in the bindings): the desktop's bin shows full.
fn update_bin_icon() {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let module = wide("shell32.dll");
    // SAFETY: shell32 is loaded (SHChangeNotify links it); the export takes nothing.
    unsafe {
        let h = GetModuleHandleW(module.as_ptr());
        if h.is_null() {
            return;
        }
        if let Some(f) = GetProcAddress(h, c"SHUpdateRecycleBinIcon".as_ptr().cast()) {
            let f: unsafe extern "system" fn() = std::mem::transmute(f);
            f();
        }
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
    super::probe::call(super::probe::Call::Open);
    super::probe::call(super::probe::Call::Query);
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

/// The full Win32 path of the program process `pid` runs, when this process may ask.
fn image_path(pid: u32) -> Option<String> {
    // SAFETY: plain Win32 calls; the handle is closed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 4096];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) != 0;
        CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..size as usize]))
    }
}

fn image_name(pid: u32) -> String {
    image_path(pid)
        .map(|full| full.rsplit(['\\', '/']).next().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// The programs running now, by their full paths (every process this one may ask about; no
/// disk I/O). A folder holding one renames although the program runs from it: its image is
/// mapped, no handle is open.
pub fn running_images() -> Vec<String> {
    use windows_sys::Win32::System::ProcessStatus::EnumProcesses;
    let mut pids = vec![0u32; 2048];
    loop {
        let mut got = 0u32;
        let bytes = (pids.len() * 4) as u32;
        // SAFETY: a buffer of `bytes` bytes, an out pointer.
        if unsafe { EnumProcesses(pids.as_mut_ptr(), bytes, &mut got) } == 0 {
            return Vec::new();
        }
        if got < bytes || pids.len() >= 1 << 20 {
            pids.truncate(got as usize / 4);
            break;
        }
        pids = vec![0u32; pids.len() * 2];
    }
    let mut out: Vec<String> = pids
        .into_iter()
        .filter(|&p| p != 0)
        .filter_map(image_path)
        .collect();
    out.sort();
    out.dedup();
    out
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
    long_names_of(Path::new(path))
}

/// [`long_names`] of the exact path: a name that is not Unicode (a lone surrogate) is looked up
/// as it is, not as the U+FFFD its text shows (no such file: no long names). The result is text,
/// as the shell's reports are read.
pub fn long_names_of(exact: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let shown = exact.to_string_lossy();
    let path: &str = &shown;
    if !path.contains('~') {
        return None;
    }
    // the exact units, behind the `\\?\` (or `\\?\UNC\`) prefix long_path puts before the text
    let units: Vec<u16> = exact.as_os_str().encode_wide().collect();
    let prefixed = long_path(path);
    let mut w: Vec<u16> = if prefixed == path {
        units
    } else if let Some(unc) = prefixed.strip_prefix("\\\\?\\UNC\\")
        && path.starts_with("\\\\")
        && path.len() == unc.len() + 2
    {
        "\\\\?\\UNC\\"
            .encode_utf16()
            .chain(units[2..].iter().copied())
            .collect()
    } else {
        "\\\\?\\".encode_utf16().chain(units).collect()
    };
    w.push(0);
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

/// The progress sink: a COM object whose first field is its vtable. It lives on the heap and counts
/// its references: ours ([`SinkRef`]) and those the engine takes (Advise, QueryInterface); the
/// last Release frees it.
#[repr(C)]
struct Sink<'a> {
    vtbl: *const SinkVtbl,
    refs: AtomicU32,
    /// the caller's callbacks: None once our reference is gone, so an engine that holds the sink
    /// longer reaches none of them
    cancel: Option<&'a (dyn Fn() -> bool + Sync)>,
    on_item: Option<OnItem<'a>>,
    cancelled: bool,
    items: Vec<Recycled>,
    index: HashMap<String, usize>,
    refused_other: Vec<String>,
    unmatched: Vec<(String, Option<String>)>,
}

/// What [`recycle`] calls right after each item was recycled.
type OnItem<'a> = &'a mut (dyn FnMut(usize, &Recycled) + Send);

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
            sink_add_ref(this); // the pointer handed out is a reference of its own
            return S_OK;
        }
        *ppv = null_mut();
        E_NOINTERFACE
    }
}
unsafe extern "system" fn sink_add_ref(this: P) -> u32 {
    // SAFETY: `this` is a live sink: whoever calls holds a reference to it.
    unsafe {
        (*(this as *const Sink<'_>))
            .refs
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }
}

unsafe extern "system" fn sink_release(this: P) -> u32 {
    // SAFETY: as in sink_add_ref; the reference given back is the caller's, so once none is left
    // nobody can reach the sink and it is freed (it was made by SinkRef::new, a Box).
    unsafe {
        let left = (*(this as *const Sink<'_>))
            .refs
            .fetch_sub(1, Ordering::AcqRel)
            - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut Sink<'_>));
        }
        left
    }
}

/// Our reference to a [`Sink`] (made with one: ours). Dropping it detaches the caller's
/// callbacks, then gives the reference back: the sink goes when the engine has let go too.
struct SinkRef<'a>(std::ptr::NonNull<Sink<'a>>);

impl<'a> SinkRef<'a> {
    fn new(
        cancel: Option<&'a (dyn Fn() -> bool + Sync)>,
        on_item: OnItem<'a>,
        items: Vec<Recycled>,
        index: HashMap<String, usize>,
    ) -> Self {
        let sink = Box::new(Sink {
            vtbl: &SINK_VTBL,
            refs: AtomicU32::new(1),
            cancel,
            on_item: Some(on_item),
            cancelled: false,
            items,
            index,
            refused_other: Vec::new(),
            unmatched: Vec::new(),
        });
        SinkRef(std::ptr::NonNull::from(Box::leak(sink)))
    }

    /// The sink as the engine takes it (an IFileOperationProgressSink).
    fn com(&self) -> P {
        self.0.as_ptr().cast()
    }

    /// # Safety
    /// Not while one of the sink's methods runs (the engine calls them on this thread, from
    /// inside its own calls).
    unsafe fn get(&mut self) -> &mut Sink<'a> {
        // SAFETY: our reference keeps it alive; the caller rules out other users.
        unsafe { self.0.as_mut() }
    }
}

impl Drop for SinkRef<'_> {
    fn drop(&mut self) {
        let s = self.0.as_ptr();
        // SAFETY: alive until our reference goes, below; no sink method runs now.
        unsafe {
            (*s).cancel = None;
            (*s).on_item = None;
            sink_release(s.cast());
        }
    }
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
                if let Some(on_item) = sink.on_item.as_mut() {
                    on_item(i, &snapshot);
                }
            }
            Some(_) => {}
        }
    }));
    S_OK
}

static SINK_VTBL: SinkVtbl = SinkVtbl {
    query_interface: sink_qi,
    add_ref: sink_add_ref,
    release: sink_release,
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
    // is found under both (expanded from the exact path: a name that is not Unicode too)
    let index = shown
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            let long = long_names_of(&paths[i]).map(|l| (path_key(&l), i));
            std::iter::once((path_key(p), i)).chain(long)
        })
        .collect();
    let mut sink = SinkRef::new(cancel, on_item, items, index);
    // SAFETY: COM on this (fresh) thread; every interface pointer is released by its guard, the
    // apartment's guard goes last; the sink counts its references, so the engine may hold it as
    // long as it likes (it reaches the caller's callbacks only while `sink` lives).
    unsafe {
        let _apartment = Apartment(CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        ));
        let done = |hr: HRESULT, sink: &mut SinkRef<'_>| RecycleRun {
            items: std::mem::take(&mut sink.get().items),
            hr,
            cancelled: false,
            refused: Vec::new(),
            unmatched: Vec::new(),
        };
        let mut raw: Unknown = null_mut();
        let hr = CoCreateInstance(
            &CLSID_FILE_OPERATION,
            null_mut(),
            CLSCTX_ALL,
            &IID_IFILE_OPERATION,
            &mut raw,
        );
        if hr < 0 || raw.is_null() {
            return done(hr, &mut sink);
        }
        let op = ComPtr(raw);
        let set_flags: unsafe extern "system" fn(Unknown, u32) -> HRESULT = method(op.0, 5);
        let advise: unsafe extern "system" fn(Unknown, P, *mut u32) -> HRESULT = method(op.0, 3);
        let unadvise: unsafe extern "system" fn(Unknown, u32) -> HRESULT = method(op.0, 4);
        let delete_item: unsafe extern "system" fn(Unknown, Unknown, P) -> HRESULT =
            method(op.0, 18);
        let perform: unsafe extern "system" fn(Unknown) -> HRESULT = method(op.0, 21);
        let hr = set_flags(op.0, RECYCLE_FLAGS);
        if hr < 0 {
            return done(hr, &mut sink);
        }
        let mut cookie = 0u32;
        let hr = advise(op.0, sink.com(), &mut cookie);
        if hr < 0 {
            return done(hr, &mut sink);
        }
        let mut queued = 0;
        for (i, path) in paths.iter().enumerate() {
            // as given: a name with a lone surrogate is parsed exactly
            let w = wide_path(path);
            let mut raw: Unknown = null_mut();
            let hr =
                SHCreateItemFromParsingName(w.as_ptr(), null_mut(), &IID_ISHELL_ITEM, &mut raw);
            if hr < 0 || raw.is_null() {
                let s = sink.get();
                s.items[i].hr = hr;
                s.items[i].reached = true;
                continue;
            }
            let item = ComPtr(raw);
            let hr = delete_item(op.0, item.0, null_mut());
            drop(item);
            if hr < 0 {
                let s = sink.get();
                s.items[i].hr = hr;
                s.items[i].reached = true;
            } else {
                queued += 1;
            }
        }
        let result = if queued > 0 { perform(op.0) } else { S_OK };
        unadvise(op.0, cookie);
        drop(op);
        let s = sink.get();
        let mut refused: Vec<String> = s
            .items
            .iter()
            .filter(|e| e.refused)
            .map(|e| e.path.clone())
            .collect();
        refused.append(&mut s.refused_other);
        RecycleRun {
            items: std::mem::take(&mut s.items),
            hr: result,
            cancelled: s.cancelled,
            refused,
            unmatched: std::mem::take(&mut s.unmatched),
        }
    }
}

/// A COM interface pointer holding one reference, released when dropped.
struct ComPtr(Unknown);

impl Drop for ComPtr {
    fn drop(&mut self) {
        // SAFETY: a live object whose reference is ours (release skips null).
        unsafe { release(self.0) };
    }
}

/// `CoInitializeEx` on this thread (its HRESULT), balanced when dropped: S_OK and S_FALSE are
/// ours to undo, a failure (RPC_E_CHANGED_MODE) is not.
struct Apartment(HRESULT);

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // SAFETY: this thread's own successful CoInitializeEx.
            unsafe { CoUninitialize() };
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vtbl(com: P) -> &'static SinkVtbl {
        // SAFETY: a live sink, whose first field points at the static vtable.
        unsafe { &**(com as *const *const SinkVtbl) }
    }

    /// The engine's AddRef / Release count (the engine may hold the sink past our part in it),
    /// QueryInterface hands out a reference of its own, and an engine that still holds the sink
    /// once ours is gone reaches none of the caller's callbacks; the last Release frees it.
    #[test]
    fn the_progress_sink_counts_its_references() {
        let mut seen = Vec::new();
        let stop = || true;
        {
            let cancel: &(dyn Fn() -> bool + Sync) = &stop;
            let mut on_item = |i: usize, _: &Recycled| seen.push(i);
            let item = Recycled {
                path: String::new(),
                hr: E_ABORT,
                recycled: None,
                refused: false,
                reached: false,
            };
            // a null item has the path "": it is item 0
            let index = HashMap::from([(String::new(), 0usize)]);
            let sink = SinkRef::new(Some(cancel), &mut on_item, vec![item], index);
            let com = sink.com();
            let vt = vtbl(com);
            let recycle = TSF_DELETE_RECYCLE_IF_POSSIBLE;
            // SAFETY: the sink is alive while a reference to it is held, as COM has it.
            unsafe {
                assert_eq!((vt.add_ref)(com), 2, "the engine's reference (Advise)");
                let mut out: P = null_mut();
                let qi = (vt.query_interface)(com, &IID_IFILE_OPERATION_PROGRESS_SINK, &mut out);
                assert_eq!((qi, out), (S_OK, com));
                assert_eq!((vt.release)(out), 2, "QueryInterface counted one");
                // ours alive: the callbacks are reached
                assert_eq!((vt.pre_delete_item)(com, recycle, null_mut()), E_ABORT);
                assert_eq!(
                    (vt.post_delete_item)(com, 0, null_mut(), S_OK, null_mut()),
                    S_OK
                );
                drop(sink);
                // only the engine's left: the sink is still there, the callbacks are not
                assert_eq!((vt.pre_delete_item)(com, recycle, null_mut()), S_OK);
                assert_eq!(
                    (vt.post_delete_item)(com, 0, null_mut(), S_OK, null_mut()),
                    S_OK
                );
                assert_eq!((vt.release)(com), 0, "the last reference frees it");
            }
        }
        assert_eq!(seen, [0]);
    }
}
