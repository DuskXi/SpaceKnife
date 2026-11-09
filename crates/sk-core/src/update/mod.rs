//! Update checks against the project's GitHub releases (`update.*`, [`sk_protocol::update`]).
//!
//! The pure parts and the blocking I/O they need; the server side (schedule, sessions, the running
//! download) belongs to the server.
//!
//! - versions: semantic versions with an optional pre-release part and leading `v`; a tag that is
//!   not one is ignored. [`level`] says how far behind the app is (the first part that differs);
//! - the release to offer ([`choose`]): never a draft; pre-releases (a version like
//!   `0.4.0-beta.1`, or GitHub's flag from 1.0 on: every 0.x release carries it) only on the
//!   `preview` channel; the newest by version;
//! - the file for this kind of install ([`pick_asset`]) and its SHA-256 from the release's
//!   `SHA256SUMS*.txt` ([`parse_checksums`]);
//! - the state kept between runs (`update.json` in the data directory, [`UpdateState`]): the last
//!   check, its ETag, the trimmed release list, the versions whose card was closed, muted series;
//! - the two network calls: the release list ([`fetch_releases`], `If-None-Match`: a 304 reuses
//!   the list) and, on the user's click, the download ([`download`]), hashed while it streams.
//!
//! Nothing here tells anyone anything about the user: one GET of the public release list with a
//! `SpaceKnife/<version>` User-Agent and no token, and the file the user asked for. HTTPS goes
//! through rustls with the operating system's certificate store; proxies come from the
//! environment (`HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`).
//!
//! [`swap`] is the portable copy's `--apply-update` mode; [`rollback`] the note it leaves when it
//! kept the old version.

pub mod rollback;
pub mod swap;

use std::cmp::Ordering as CmpOrdering;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sk_protocol::update::{InstallKind, UpdateAsset, UpdateChannel, UpdateInfo, UpdateLevel};

use crate::i18n::t;
use crate::{paths, tr};

pub const RELEASES_URL: &str = "https://api.github.com/repos/DuskXi/SpaceKnife/releases";
pub const RELEASES_PAGE: &str = "https://github.com/DuskXi/SpaceKnife/releases";
/// Every network call: connecting, and waiting for the response.
pub const TIMEOUT: Duration = Duration::from_secs(15);
pub const CHECK_INTERVAL: TimeDelta = TimeDelta::hours(24);
/// After a failed check.
pub const RETRY_INTERVAL: TimeDelta = TimeDelta::hours(6);
/// Bytes of release notes kept (UTF-8).
pub const NOTES_MAX: usize = 16 * 1024;
/// The release list is a few hundred KB; anything far bigger is not one.
pub const LIST_MAX: u64 = 8 << 20;
pub const SUMS_MAX: u64 = 1 << 20;
pub const CHUNK: usize = 256 * 1024;
/// Download progress at most 4 times a second.
pub const PROGRESS_GAP: Duration = Duration::from_millis(250);
pub const KEEP_DISMISSED: usize = 20;
pub const KEEP_MUTED: usize = 20;
/// Releases kept for the timeline of the 「关于与更新」 page.
pub const HISTORY_KEEP: usize = 30;
pub const STATE_VERSION: i64 = 1;
const LOOPBACK: &[&str] = &["127.0.0.1", "localhost", "::1"];

// ---------------------------------------------------------------- errors

/// A check or download failed; [`UpdateError::message`] is in the current language each time it
/// is read.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateError {
    Offline {
        reason: String,
    },
    Timeout,
    Certificate {
        reason: String,
    },
    Rate,
    RateUntil {
        minutes: i64,
    },
    Http {
        code: u16,
    },
    BadList,
    Failed {
        reason: String,
    },
    Sums {
        reason: String,
    },
    DownloadNet {
        reason: String,
    },
    DownloadHttp {
        code: u16,
    },
    Incomplete {
        received: u64,
        expected: u64,
    },
    Mismatch,
    Write {
        path: PathBuf,
        reason: String,
    },
    DownloadFailed {
        reason: String,
    },
    /// The download was cancelled; its partial file is gone.
    Cancelled,
    /// The release lists no SHA-256 for the file: it is not downloaded (an unverified installer
    /// would run silently).
    NoChecksum,
}

impl UpdateError {
    /// The message id's key, as the Python backend named them (`offline`, `rate_until`, …).
    pub fn key(&self) -> &'static str {
        match self {
            UpdateError::Offline { .. } => "offline",
            UpdateError::Timeout => "timeout",
            UpdateError::Certificate { .. } => "certificate",
            UpdateError::Rate => "rate",
            UpdateError::RateUntil { .. } => "rate_until",
            UpdateError::Http { .. } => "http",
            UpdateError::BadList => "bad_list",
            UpdateError::Failed { .. } => "failed",
            UpdateError::Sums { .. } => "sums",
            UpdateError::DownloadNet { .. } => "download_net",
            UpdateError::DownloadHttp { .. } => "download_http",
            UpdateError::Incomplete { .. } => "incomplete",
            UpdateError::Mismatch => "mismatch",
            UpdateError::Write { .. } => "write",
            UpdateError::DownloadFailed { .. } => "download_failed",
            UpdateError::Cancelled => "cancelled",
            UpdateError::NoChecksum => "no_checksum",
        }
    }

    /// The message in the current language.
    pub fn message(&self) -> String {
        match self {
            UpdateError::Offline { reason } => {
                tr!("连不上 GitHub：{reason}", reason = reason.as_str())
            }
            UpdateError::Timeout => t("连接 GitHub 超时").to_owned(),
            UpdateError::Certificate { reason } => {
                tr!("无法验证 GitHub 的证书：{reason}", reason = reason.as_str())
            }
            UpdateError::Rate => t("GitHub 暂时限制了查询次数，请过一会儿再试").to_owned(),
            UpdateError::RateUntil { minutes } => {
                tr!(
                    "GitHub 暂时限制了查询次数，约 {minutes} 分钟后恢复",
                    minutes = *minutes
                )
            }
            UpdateError::Http { code } => tr!("GitHub 返回了错误：HTTP {code}", code = *code),
            UpdateError::BadList => t("GitHub 返回的发布列表无法识别").to_owned(),
            UpdateError::Failed { reason } => {
                tr!("检查更新失败：{reason}", reason = reason.as_str())
            }
            UpdateError::Sums { reason } => {
                tr!(
                    "取不到这个版本发布的校验值（{reason}），没有下载",
                    reason = reason.as_str()
                )
            }
            UpdateError::DownloadNet { reason } => {
                tr!("下载中断：{reason}", reason = reason.as_str())
            }
            UpdateError::DownloadHttp { code } => tr!("下载失败：HTTP {code}", code = *code),
            UpdateError::Incomplete { received, expected } => tr!(
                "下载不完整：收到 {received} 字节，应为 {expected} 字节",
                received = *received,
                expected = *expected
            ),
            UpdateError::Mismatch => t("下载的文件与发布的 SHA-256 校验值不符，已删除").to_owned(),
            UpdateError::Write { path, reason } => {
                tr!(
                    "写不了 {path}：{reason}",
                    path = path,
                    reason = reason.as_str()
                )
            }
            UpdateError::DownloadFailed { reason } => {
                tr!("下载失败：{reason}", reason = reason.as_str())
            }
            UpdateError::Cancelled => "cancelled".to_owned(),
            UpdateError::NoChecksum => {
                t("这个版本的发布没有列出这个文件的 SHA-256 校验值，没有下载").to_owned()
            }
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for UpdateError {}

// ---------------------------------------------------------------- versions

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[vV]?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z.-]+)?$",
    )
    .expect("a valid pattern")
});

/// A semantic version; build metadata (`+…`) is dropped, it does not order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            pre: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Option<Version> {
        let m = VERSION_RE.captures(text.trim())?;
        let n = |i: usize| m.get(i)?.as_str().parse::<u64>().ok();
        Some(Version {
            major: n(1)?,
            minor: n(2)?,
            patch: n(3)?,
            pre: m.get(4).map_or_else(Vec::new, |p| {
                p.as_str().split('.').map(str::to_owned).collect()
            }),
        })
    }
}

fn cmp_pre_id(a: &str, b: &str) -> CmpOrdering {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    match (digits(a), digits(b)) {
        (true, true) => {
            let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
            a.len().cmp(&b.len()).then_with(|| a.cmp(b))
        }
        (true, false) => CmpOrdering::Less,
        (false, true) => CmpOrdering::Greater,
        (false, false) => a.cmp(b),
    }
}

impl Ord for Version {
    /// Semver precedence: a release after its pre-releases; numeric identifiers numerically and
    /// before alphanumeric ones; a longer list of equal identifiers after the shorter.
    fn cmp(&self, other: &Self) -> CmpOrdering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => CmpOrdering::Equal,
                (true, false) => CmpOrdering::Greater,
                (false, true) => CmpOrdering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let c = cmp_pre_id(a, b);
                        if c != CmpOrdering::Equal {
                            return c;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
            .then_with(|| self.pre.cmp(&other.pre))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

fn parse_opt(v: Option<&str>) -> Option<Version> {
    v.and_then(Version::parse)
}

/// How far behind `latest` the running `current` is: the first part that differs (a pre-release
/// of the same version counts as a patch); `none` when it is not newer.
pub fn level(current: Option<&str>, latest: Option<&str>) -> UpdateLevel {
    let (Some(cur), Some(new)) = (parse_opt(current), parse_opt(latest)) else {
        return UpdateLevel::None;
    };
    if cur >= new {
        UpdateLevel::None
    } else if new.major != cur.major {
        UpdateLevel::Major
    } else if new.minor != cur.minor {
        UpdateLevel::Minor
    } else {
        UpdateLevel::Patch
    }
}

/// The series of `latest` that the user can mute (`update.mute`): `"0.4"` (every 0.4.x) for a
/// minor update, `"1"` (every 1.x) for a major one; `None` for a patch release or no update.
pub fn series(current: Option<&str>, latest: Option<&str>) -> Option<String> {
    let new = parse_opt(latest)?;
    match level(current, latest) {
        UpdateLevel::Major => Some(new.major.to_string()),
        UpdateLevel::Minor => Some(format!("{}.{}", new.major, new.minor)),
        _ => None,
    }
}

/// Whether `current` is in series `name` or past it: muting it says nothing any more.
pub fn series_reached(name: &str, current: Option<&str>) -> bool {
    let parts: Result<Vec<u64>, _> = name.split('.').map(str::parse::<u64>).collect();
    let Ok(parts) = parts else {
        return true; // not a series: drop it
    };
    let Some(cur) = parse_opt(current) else {
        return false;
    };
    let mine = [cur.major, cur.minor];
    mine[..parts.len().min(2)] >= parts[..]
}

// ---------------------------------------------------------------- releases

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    pub url: String,
}

/// One GitHub release, trimmed to what the update card needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    /// Without the leading v
    pub version: String,
    pub tag: String,
    #[serde(default)]
    pub name: String,
    /// GitHub's flag
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    /// The release page
    pub url: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn parsed(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    /// Offered on the `preview` channel only: a pre-release version (`0.4.0-beta.1`), or one GitHub
    /// marks as a pre-release — except during 0.x, when every release is marked so (release.yml):
    /// there a version without a pre-release part is on the `stable` channel too.
    pub fn preview(&self) -> bool {
        match self.parsed() {
            None => true,
            Some(v) if !v.pre.is_empty() => true,
            Some(v) => self.prerelease && v.major > 0,
        }
    }
}

/// (scheme lower-cased, netloc, host lower-cased) of a URL.
fn split_url(url: &str) -> Option<(String, &str, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let netloc = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostport = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = if let Some(stripped) = hostport.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    Some((
        scheme.to_ascii_lowercase(),
        netloc,
        host.to_ascii_lowercase(),
    ))
}

/// Pages and files the app may open or fetch: on github.com — or, with a local test feed
/// ([`feed_url`]), next to it.
pub fn allowed_url(url: &str, feed: &str) -> bool {
    let Some((scheme, netloc, host)) = split_url(url) else {
        return false;
    };
    if scheme == "https" && host == "github.com" {
        return true;
    }
    feed != RELEASES_URL
        && split_url(feed).is_some_and(|(fs, fn_, _)| fs == scheme && fn_ == netloc)
}

/// At most `limit` UTF-8 bytes, cut at a line end when there is one in the second half.
pub fn cap_notes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut b = limit;
    while !text.is_char_boundary(b) {
        b -= 1;
    }
    let cut = &text[..b];
    let total = cut.chars().count();
    let kept = match cut.rfind('\n') {
        Some(end) if cut[..end].chars().count() > total / 2 => &cut[..end],
        _ => cut,
    };
    format!("{}\n…", kept.trim_end())
}

fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// GitHub's release list -> the releases worth offering, newest first: no drafts, no tags that are
/// not a version; files hosted anywhere else are dropped.
pub fn parse_releases(data: &Value, feed: &str) -> Result<Vec<Release>, String> {
    let Some(items) = data.as_array() else {
        return Err("not a release list".into());
    };
    let mut out: Vec<(Version, Release)> = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        if obj
            .get("draft")
            .is_some_and(|d| d.as_bool().unwrap_or(!d.is_null()))
        {
            continue;
        }
        let Some(tag) = obj.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(v) = Version::parse(tag) else {
            continue;
        };
        let mut assets = Vec::new();
        for a in obj
            .get("assets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(name), Some(url)) = (
                a.get("name").and_then(Value::as_str),
                a.get("browser_download_url").and_then(Value::as_str),
            ) else {
                continue;
            };
            if allowed_url(url, feed) {
                let size = a.get("size").and_then(Value::as_u64).unwrap_or(0);
                assets.push(Asset {
                    name: name.to_owned(),
                    size,
                    url: url.to_owned(),
                });
            }
        }
        let url = obj
            .get("html_url")
            .and_then(Value::as_str)
            .filter(|p| allowed_url(p, feed))
            .map_or_else(
                || format!("{RELEASES_PAGE}/tag/{}", quote(tag)),
                str::to_owned,
            );
        let published_at = obj
            .get("published_at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let release = Release {
            version: v.to_string(),
            tag: tag.to_owned(),
            name: obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            prerelease: obj
                .get("prerelease")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            published_at,
            url,
            notes: obj
                .get("body")
                .and_then(Value::as_str)
                .map_or_else(String::new, |b| cap_notes(b, NOTES_MAX)),
            assets,
        };
        out.push((v, release));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

/// The newest release of a channel: `stable` skips pre-releases, `preview` includes them.
pub fn choose(releases: &[Release], channel: UpdateChannel) -> Option<&Release> {
    let mut best: Option<(Version, &Release)> = None;
    for r in releases {
        let Some(v) = r.parsed() else { continue };
        if channel == UpdateChannel::Stable && r.preview() {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| *b < v) {
            best = Some((v, r));
        }
    }
    best.map(|(_, r)| r)
}

/// What the cache keeps: the newest [`HISTORY_KEEP`] releases that have a version, newest first.
pub fn trim(releases: &[Release]) -> Vec<Release> {
    let mut known: Vec<(Version, &Release)> = releases
        .iter()
        .filter_map(|r| r.parsed().map(|v| (v, r)))
        .collect();
    known.sort_by(|a, b| b.0.cmp(&a.0));
    known
        .into_iter()
        .take(HISTORY_KEEP)
        .map(|(_, r)| r.clone())
        .collect()
}

pub fn to_info(release: &Release, asset: Option<&Asset>, sha256: Option<&str>) -> UpdateInfo {
    UpdateInfo {
        version: release.version.clone(),
        tag: release.tag.clone(),
        name: if release.name.is_empty() {
            release.tag.clone()
        } else {
            release.name.clone()
        },
        published_at: release.published_at,
        prerelease: release.prerelease,
        url: release.url.clone(),
        notes: release.notes.clone(),
        asset: asset.map(|a| UpdateAsset {
            name: a.name.clone(),
            size: a.size,
            url: a.url.clone(),
            sha256: sha256.map(str::to_owned),
        }),
    }
}

// ---------------------------------------------------------------- the file for this install

/// The version as the packager wrote it (deb / rpm may change - to ~).
const V_SLOT: &str = "[0-9A-Za-z.+~_-]+";

pub fn arch(machine: &str) -> String {
    match machine.to_lowercase().as_str() {
        "amd64" | "x86_64" | "x64" => "x64".into(),
        "arm64" | "aarch64" => "arm64".into(),
        other => other.to_owned(),
    }
}

/// This platform as Python's `sys.platform` named it (`win32`, `linux`, `darwin`).
pub fn system() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

/// The processor's architecture as the release files name it (`x64`, `arm64`, …), read at run
/// time: an x64 build that Windows on ARM runs emulated (or Rosetta on an Apple silicon Mac) is on
/// an `arm64` processor, and a portable copy of it moves to the native build. The build's own
/// architecture where the system can't say.
pub fn machine() -> String {
    static MACHINE: LazyLock<String> =
        LazyLock::new(|| native_machine().unwrap_or_else(|| arch(std::env::consts::ARCH)));
    MACHINE.clone()
}

/// A Windows `IMAGE_FILE_MACHINE_*` value as the release files name it.
pub fn windows_machine_arch(image: u16) -> Option<&'static str> {
    match image {
        0x8664 => Some("x64"),
        0xAA64 => Some("arm64"),
        0x014C => Some("x86"),
        _ => None,
    }
}

#[cfg(windows)]
fn native_machine() -> Option<String> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // IsWow64Process2 (Windows 10 1709): the system's own processor, also from an emulated
    // process. Looked up at run time: a static import would keep older Windows 10 from starting
    // the program at all.
    type IsWow64Process2 = unsafe extern "system" fn(HANDLE, *mut u16, *mut u16) -> i32;
    let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    // SAFETY: plain Win32 calls; kernel32 is always loaded, the function pointer has the
    // documented signature, and both out-pointers are valid for the call.
    unsafe {
        let module = GetModuleHandleW(kernel32.as_ptr());
        if module.is_null() {
            return None;
        }
        let f = GetProcAddress(module, c"IsWow64Process2".as_ptr().cast())?;
        let f: IsWow64Process2 = std::mem::transmute(f);
        let (mut process, mut native) = (0u16, 0u16);
        if f(GetCurrentProcess(), &mut process, &mut native) == 0 {
            return None;
        }
        windows_machine_arch(native).map(str::to_owned)
    }
}

#[cfg(target_os = "macos")]
fn native_machine() -> Option<String> {
    // hw.optional.arm64 is 1 on Apple silicon, also for a process Rosetta translates; Intel Macs
    // don't have it
    let mut value: i32 = 0;
    let mut size = std::mem::size_of::<i32>();
    // SAFETY: a valid NUL-terminated name and an out-buffer of the size given.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.optional.arm64".as_ptr(),
            (&mut value as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    Some(
        if rc == 0 && value == 1 {
            "arm64"
        } else {
            "x64"
        }
        .to_owned(),
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_machine() -> Option<String> {
    // SAFETY: uname fills the struct it is given.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return None;
    }
    // SAFETY: uname writes NUL-terminated strings.
    let machine = unsafe { std::ffi::CStr::from_ptr(name.machine.as_ptr()) };
    Some(arch(&machine.to_string_lossy()))
}

#[cfg(not(any(unix, windows)))]
fn native_machine() -> Option<String> {
    None
}

/// The program that hosts the interface. Some kinds of install are built from one shell only (the
/// rpm and the AppImage from the Electron shell; the Windows installer, the Windows and macOS
/// portable copies, the deb and the macOS disk images from Tauri), Linux has a portable copy of
/// each (the Electron one under 0.3.4's name), and an install updates to a file of its own shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shell {
    /// The Tauri app: the backend runs in its process
    Tauri,
    /// The Electron app: it runs `spaceknife-cli serve` and says so with `SPACEKNIFE_SHELL=electron`
    Electron,
    /// A value nobody publishes files for
    Unknown,
}

impl Shell {
    pub fn as_str(self) -> &'static str {
        match self {
            Shell::Tauri => "tauri",
            Shell::Electron => "electron",
            Shell::Unknown => "unknown",
        }
    }
}

/// The shell, as the launcher tells it (`SPACEKNIFE_SHELL`): unset is the Tauri app, whose
/// backend runs in its own process.
pub fn shell() -> Shell {
    shell_from(&std::env::var("SPACEKNIFE_SHELL").unwrap_or_default())
}

/// [`shell`] for a value of `SPACEKNIFE_SHELL`.
pub fn shell_from(value: &str) -> Shell {
    match value.trim().to_lowercase().as_str() {
        "" | "tauri" => Shell::Tauri,
        "electron" => Shell::Electron,
        _ => Shell::Unknown,
    }
}

/// The file names for a kind of install of a shell on a platform, `{v}` for the version, the one
/// to take first first; empty: nothing is published for it (a run from source, an unknown install,
/// a kind the shell is not built as, an architecture without builds). A portable copy moves to the
/// build of the processor it runs on — an x64 copy on Windows on ARM becomes the native one, and
/// stays on x64 for a release without an ARM build (Windows runs it emulated).
///
/// The names are 0.3.4's, and stay so for good: an installed 0.3.x picks the newest release by
/// its version first and only then looks for its file under exactly these names (v0.3.4
/// `update.py` asset_templates; tools/release.mjs `expected` publishes them, Release check runs
/// 0.3.4's own code against them). The one kind of install 0.3.4 did not have, the Tauri app's
/// portable copy for Linux, has a name no 0.3.4 pattern matches (`-tauri.tar.gz`).
pub fn asset_templates(
    kind: InstallKind,
    shell: Shell,
    system: &str,
    machine: &str,
) -> Vec<String> {
    let a = arch(machine);
    let x64 = a == "x64";
    let known = x64 || a == "arm64";
    match (kind, shell) {
        // Tauri's (the forked NSIS template, apps/desktop/src-tauri/windows, which takes over
        // 0.3.4's electron-builder installation), under 0.3.4's name; Windows on ARM runs it
        // emulated
        (InstallKind::Nsis, Shell::Tauri) => vec!["SpaceKnife-{v}-Setup-x64.exe".into()],
        (InstallKind::Portable, Shell::Tauri) if system == "win32" && a == "arm64" => {
            vec![
                "SpaceKnife-{v}-win-arm64-portable.zip".into(),
                "SpaceKnife-{v}-win-x64-portable.zip".into(),
            ]
        }
        (InstallKind::Portable, Shell::Tauri) if system == "win32" => {
            vec!["SpaceKnife-{v}-win-x64-portable.zip".into()]
        }
        // Linux: 0.3.4's name is the Electron shell's portable copy (0.3.4's copies on RHEL 9, which
        // has no WebKitGTK 4.1, keep working); the Tauri one has a name 0.3.4's pattern never matches
        (InstallKind::Portable, Shell::Electron) if system.starts_with("linux") && known => {
            vec![format!("SpaceKnife-{{v}}-linux-{a}-portable.tar.gz")]
        }
        (InstallKind::Portable, Shell::Tauri) if system.starts_with("linux") && known => {
            vec![format!("SpaceKnife-{{v}}-linux-{a}-tauri.tar.gz")]
        }
        (InstallKind::Portable, Shell::Tauri) if system == "darwin" && known => {
            vec![format!("SpaceKnife-{{v}}-mac-{a}-portable.zip")]
        }
        (InstallKind::Appimage, Shell::Electron) if x64 => {
            vec!["SpaceKnife-{v}-x86_64.AppImage".into()]
        }
        (InstallKind::Deb, Shell::Tauri) if x64 => vec!["spaceknife_{v}_amd64.deb".into()],
        (InstallKind::Rpm, Shell::Electron) if x64 => vec!["spaceknife-{v}.x86_64.rpm".into()],
        (InstallKind::Dmg, Shell::Tauri) if known => {
            vec![format!("SpaceKnife-{{v}}-mac-{a}.dmg")]
        }
        _ => Vec::new(),
    }
}

/// The first of [`asset_templates`], or `None`.
pub fn asset_template(
    kind: InstallKind,
    shell: Shell,
    system: &str,
    machine: &str,
) -> Option<String> {
    asset_templates(kind, shell, system, machine)
        .into_iter()
        .next()
}

fn template_regex(template: &str, slot: &str) -> Regex {
    let pattern = regex::escape(template).replace(r"\{v\}", slot);
    Regex::new(&format!("^(?:{pattern})$")).expect("a valid pattern")
}

/// The release's file for this kind of install of `shell`: the exact name for its version, else
/// one whose version part the packager spelled differently. `None` when there is none. `system`
/// defaults to this platform, `machine` to the processor this runs on ([`machine`]).
pub fn pick_asset<'a>(
    release: &'a Release,
    kind: InstallKind,
    shell: Shell,
    system: Option<&str>,
    machine: Option<&str>,
) -> Option<&'a Asset> {
    let native;
    let machine = match machine {
        Some(m) => m,
        None => {
            native = self::machine();
            &native
        }
    };
    let templates = asset_templates(kind, shell, system.unwrap_or(self::system()), machine);
    templates.iter().find_map(|template| {
        let exact = template.replace("{v}", &release.version);
        let pattern = template_regex(template, V_SLOT);
        let found: Vec<&Asset> = release
            .assets
            .iter()
            .filter(|a| pattern.is_match(&a.name))
            .collect();
        found
            .iter()
            .find(|a| a.name == exact)
            .or(found.first())
            .copied()
    })
}

// ---------------------------------------------------------------- checksums

static SUMS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^SHA256SUMS[0-9A-Za-z._-]*\.txt$").expect("a valid pattern"));

pub fn is_checksum_file(name: &str) -> bool {
    SUMS_RE.is_match(name)
}

/// `sha256sum` / `shasum -a 256` output -> (file name, lowercase hex). `*` (binary mode) and
/// leading folders are dropped; lines that are not a checksum are skipped.
pub fn parse_checksums(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((hex, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let name = rest.trim().trim_start_matches('*').replace('\\', "/");
        let name = name.rsplit('/').next().unwrap_or("").to_owned();
        if !name.is_empty() {
            out.retain(|(n, _)| *n != name);
            out.push((name, hex.to_lowercase()));
        }
    }
    out
}

/// The checksum of `name` in `sha256sum` output.
pub fn checksum_of(text: &str, name: &str) -> Option<String> {
    parse_checksums(text)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, h)| h)
}

/// The release's checksum files, the likeliest to list `asset` first: SHA256SUMS-mac-arm64.txt
/// for a file with "mac" and "arm64" in its name, then SHA256SUMS.txt, then the rest.
pub fn checksum_files<'a>(release: &'a Release, asset: &Asset) -> Vec<&'a Asset> {
    let name = asset.name.to_lowercase();
    let rank = |a: &Asset| -> u8 {
        let lower = a.name.to_lowercase();
        let middle = lower.get(10..lower.len().saturating_sub(4)).unwrap_or("");
        let words: Vec<&str> = middle
            .split(['-', '_', '.'])
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            1
        } else if words.iter().all(|w| name.contains(w)) {
            0
        } else {
            2
        }
    };
    let mut out: Vec<&Asset> = release
        .assets
        .iter()
        .filter(|a| is_checksum_file(&a.name))
        .collect();
    out.sort_by_key(|a| rank(a));
    out
}

// ---------------------------------------------------------------- environment

/// The release list: GitHub's — or `SPACEKNIFE_UPDATE_FEED` when it is on this machine (tests and
/// demos serve a release list of their own; nothing else is ever contacted).
pub fn feed_url() -> String {
    feed_url_from(&std::env::var("SPACEKNIFE_UPDATE_FEED").unwrap_or_default())
}

/// [`feed_url`] for a value of `SPACEKNIFE_UPDATE_FEED`.
pub fn feed_url_from(value: &str) -> String {
    let value = value.trim();
    if !value.is_empty() {
        if let Some((scheme, _, host)) = split_url(value)
            && (scheme == "http" || scheme == "https")
            && LOOPBACK.contains(&host.as_str())
        {
            return value.to_owned();
        }
        tracing::warn!(
            "SPACEKNIFE_UPDATE_FEED ignored: only a loopback address is allowed ({value})"
        );
    }
    RELEASES_URL.to_owned()
}

/// The running app's version as the launcher tells it (`SPACEKNIFE_APP_VERSION`).
pub fn app_version() -> Option<String> {
    std::env::var("SPACEKNIFE_APP_VERSION")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// How this copy was installed, as the launcher tells it: unset is a run from source.
pub fn install_kind() -> InstallKind {
    install_kind_from(&std::env::var("SPACEKNIFE_INSTALL_KIND").unwrap_or_default())
}

/// [`install_kind`] for a value of `SPACEKNIFE_INSTALL_KIND`.
pub fn install_kind_from(value: &str) -> InstallKind {
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        return InstallKind::Dev;
    }
    InstallKind::from_str(&value).unwrap_or(InstallKind::Unknown)
}

/// Where update files go: `updates` in the data directory, not the user's download folder.
pub fn updates_dir() -> PathBuf {
    paths::data_dir().join("updates")
}

/// The names in the updates folder: the release files, and what the app writes while a portable
/// copy is replaced (the new files unpacked, the old ones until the swap is done, the plan and its
/// log; the scripts of earlier test builds).
static UPDATE_NAMES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // the names Tauri's bundler gives its files, which test builds downloaded: cleaned up
        "SpaceKnife_{v}_x64-setup.exe",
        "SpaceKnife_{v}_amd64.AppImage",
        "SpaceKnife_{v}_amd64.deb",
        "SpaceKnife-{v}-1.x86_64.rpm",
        "SpaceKnife_{v}_aarch64.dmg",
        "SpaceKnife_{v}_x64.dmg",
        // the release files (asset_templates)
        "SpaceKnife-{v}-Setup-x64.exe",
        "SpaceKnife-{v}-win-x64-portable.zip",
        "SpaceKnife-{v}-win-arm64-portable.zip",
        "SpaceKnife-{v}-linux-x64-portable.tar.gz",
        "SpaceKnife-{v}-linux-arm64-portable.tar.gz",
        "SpaceKnife-{v}-linux-x64-tauri.tar.gz",
        "SpaceKnife-{v}-linux-arm64-tauri.tar.gz",
        "SpaceKnife-{v}-mac-arm64-portable.zip",
        "SpaceKnife-{v}-mac-x64-portable.zip",
        "SpaceKnife-{v}-x86_64.AppImage",
        "spaceknife_{v}_amd64.deb",
        "spaceknife-{v}.x86_64.rpm",
        "SpaceKnife-{v}-mac-arm64.dmg",
        "SpaceKnife-{v}-mac-x64.dmg",
        "staging-{v}",
        "previous-{v}",
        "update-{v}.json",
        "update-{v}.ps1",
        "update-{v}.sh",
        "update-{v}.log",
        // the note of an update that was undone (rollback.rs): gone once that version runs
        "rollback-{v}.json",
    ]
    .iter()
    .map(|n| template_regex(n, &format!("(?P<v>{V_SLOT})")))
    .collect()
});

/// The version of a file or folder in the updates folder, from its name; `None` for any other
/// name. A deb or rpm package may spell a pre-release's `-` as `~`.
pub fn update_file_version(name: &str) -> Option<Version> {
    UPDATE_NAMES
        .iter()
        .find_map(|re| re.captures(name))
        .and_then(|m| Version::parse(&m["v"].replace('~', "-")))
}

/// Remove what the updates folder no longer needs (blocking): partial downloads, and everything of
/// the running version or an older one. The file of a newer release stays, and so does any name
/// this module did not write. Returns the names removed.
pub fn clean_updates(folder: &Path, current: Option<&str>) -> Vec<String> {
    clean_updates_with(folder, current, &remove_entry).removed
}

/// What a cleaning of the updates folder did: the names it removed, and the ones it could not
/// remove (yet): right after an update the old version's files may still be held by its last
/// processes as they close, or by a virus scanner looking at the moved files. [`clean_again`]
/// tries those later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub removed: Vec<String>,
    pub left: Vec<String>,
}

/// Remove a file, a link or a whole folder.
pub fn remove_entry(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// [`clean_updates`] with its removing done by `remove` (tests).
pub fn clean_updates_with(
    folder: &Path,
    current: Option<&str>,
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
) -> Cleaned {
    clean_names(folder, current, None, remove)
}

/// Try again to remove what an earlier cleaning left, each name only while it is still stale;
/// nothing else in the folder is touched (a download that started meanwhile stays), and no
/// partial download.
pub fn clean_again(folder: &Path, current: Option<&str>, names: &[String]) -> Cleaned {
    clean_again_with(folder, current, names, &remove_entry)
}

/// [`clean_again`] with its removing done by `remove` (tests).
pub fn clean_again_with(
    folder: &Path,
    current: Option<&str>,
    names: &[String],
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
) -> Cleaned {
    clean_names(folder, current, Some(names), remove)
}

fn clean_names(
    folder: &Path,
    current: Option<&str>,
    only: Option<&[String]>,
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
) -> Cleaned {
    let now = parse_opt(current);
    let mut out = Cleaned::default();
    let Ok(rd) = std::fs::read_dir(folder) else {
        return out;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        // a partial download is not tried again: by then a new download may write the same name
        if only.is_some_and(|names| !names.contains(&name) || name.ends_with(".part")) {
            continue;
        }
        let mut stale = name.ends_with(".part");
        if !stale && let Some(now) = &now {
            stale = update_file_version(&name).is_some_and(|v| v <= *now);
        }
        if !stale {
            continue;
        }
        let path = entry.path();
        // a replaced portable copy's files: removed only while they are SpaceKnife's own (the swap
        // moves nothing else; one of another program, or an older one, may have)
        if name.starts_with("previous-")
            && let Some(other) = not_spaceknifes(&path, std::env::consts::OS)
        {
            tracing::warn!(
                "{} holds what is not SpaceKnife's ({other}): left as it is",
                path.display()
            );
            continue;
        }
        match remove(&path) {
            Ok(()) => out.removed.push(name),
            Err(e) => {
                tracing::warn!("could not remove {} (yet): {e}", path.display());
                out.left.push(name);
            }
        }
    }
    out
}

/// The first top-level name in a `previous-<v>` folder that is neither a name of SpaceKnife's own
/// portable layouts on system `os` (swap::own_names) nor a system file, or None when everything
/// there is SpaceKnife's (a folder that can't be read, or a file in its place: None, it goes).
pub fn not_spaceknifes(folder: &Path, os: &str) -> Option<String> {
    if !std::fs::symlink_metadata(folder).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    let fold = |s: &str| {
        if os == "windows" {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let own: Vec<String> = swap::own_names(os).iter().map(|n| fold(n)).collect();
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .find(|n| !own.contains(&fold(n)) && !swap::system_file(n))
}

// ---------------------------------------------------------------- persistent state

fn default_feed() -> String {
    RELEASES_URL.to_owned()
}
fn state_version() -> i64 {
    STATE_VERSION
}

/// `update.json` in the data directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateState {
    #[serde(default = "state_version")]
    pub version: i64,
    #[serde(default = "default_feed")]
    pub feed: String,
    /// The last successful check
    #[serde(default)]
    pub checked_at: Option<DateTime<Utc>>,
    /// The last failed one, when it came after
    #[serde(default)]
    pub failed_at: Option<DateTime<Utc>>,
    /// Of `releases`
    #[serde(default)]
    pub etag: Option<String>,
    /// Trimmed ([`trim`])
    #[serde(default)]
    pub releases: Vec<Release>,
    /// [`HISTORY_KEEP`] when `releases` was trimmed: a cache of a version that kept fewer is
    /// fetched again whole, without its ETag
    #[serde(default)]
    pub kept: usize,
    /// Versions whose card / dialog was closed
    #[serde(default)]
    pub dismissed: Vec<String>,
    /// Series whose prompts are off ([`series`])
    #[serde(default)]
    pub muted: Vec<String>,
}

impl Default for UpdateState {
    fn default() -> Self {
        UpdateState::for_feed(RELEASES_URL)
    }
}

impl UpdateState {
    pub fn for_feed(feed: &str) -> UpdateState {
        UpdateState {
            version: STATE_VERSION,
            feed: feed.to_owned(),
            checked_at: None,
            failed_at: None,
            etag: None,
            releases: Vec::new(),
            kept: 0,
            dismissed: Vec::new(),
            muted: Vec::new(),
        }
    }
}

pub fn state_file() -> PathBuf {
    paths::data_dir().join("update.json")
}

/// The saved state; a missing or broken file is a fresh start, a cache of another feed is
/// dropped (the closed cards and the muted series are kept).
pub fn load_state(file: &Path, feed: &str) -> UpdateState {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "update state {} is unreadable, starting afresh: {e}",
                    file.display()
                );
            }
            return UpdateState::for_feed(feed);
        }
    };
    let state: UpdateState = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "update state {} is unreadable, starting afresh: {e}",
                file.display()
            );
            return UpdateState::for_feed(feed);
        }
    };
    if state.feed != feed {
        return UpdateState {
            dismissed: state.dismissed,
            muted: state.muted,
            ..UpdateState::for_feed(feed)
        };
    }
    state
}

/// Write `text` to `file` through `<file>.tmp` and a rename.
pub fn write_atomic(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = file.with_file_name(name);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

pub fn save_state(state: &UpdateState, file: &Path) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    state.serialize(&mut ser).map_err(std::io::Error::other)?;
    write_atomic(file, &String::from_utf8_lossy(&buf))
}

// ---------------------------------------------------------------- network

/// How long a small body (the release list, a checksum file) may take in all: a server that
/// trickles it byte by byte does not hold the check for ever.
const SMALL_BODY: u32 = 4;

/// `timeout` bounds connecting, waiting for the response and each read (a connection that stalls
/// mid-body fails after it, as each read did in the Python backend); `body`: a limit on the whole
/// body as well.
fn agent(url: &str, timeout: Duration, body: Option<Duration>) -> ureq::Agent {
    let loopback = split_url(url).is_some_and(|(_, _, h)| LOOPBACK.contains(&h.as_str()));
    let mut cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .timeout_per_read(Some(timeout))
        .timeout_recv_body(body)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        );
    if loopback {
        cfg = cfg.proxy(None);
    }
    cfg.build().into()
}

fn reason_of(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Io(io) => io.to_string(),
        other => other.to_string(),
    }
}

/// A failed connection -> its error; `offline_key`: true for the release list (`offline` /
/// `timeout`), false for a download (`download_net` with the reason).
fn network_error(e: &ureq::Error, make: impl Fn(String) -> UpdateError, list: bool) -> UpdateError {
    let timed_out = matches!(e, ureq::Error::Timeout(_))
        || matches!(e, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut);
    if timed_out {
        return if list {
            UpdateError::Timeout
        } else {
            make(t("连接 GitHub 超时").to_owned())
        };
    }
    match e {
        ureq::Error::Rustls(r) => UpdateError::Certificate {
            reason: r.to_string(),
        },
        ureq::Error::Tls(r) => UpdateError::Certificate {
            reason: (*r).to_owned(),
        },
        other => make(reason_of(other)),
    }
}

/// A read that timed out: the system's own error, or ureq's timeout wrapped into an I/O error by
/// the body reader.
fn timed_out(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::TimedOut
        || e.get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            .is_some_and(|u| {
                matches!(u, ureq::Error::Timeout(_))
                    || matches!(u, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut)
            })
}

fn io_error(e: &std::io::Error, make: impl Fn(String) -> UpdateError, list: bool) -> UpdateError {
    if timed_out(e) {
        return if list {
            UpdateError::Timeout
        } else {
            make(t("连接 GitHub 超时").to_owned())
        };
    }
    make(e.to_string())
}

type Response = ureq::http::Response<ureq::Body>;

fn get(
    url: &str,
    user_agent: &str,
    timeout: Duration,
    body: Option<Duration>,
    headers: &[(&str, &str)],
) -> Result<Response, ureq::Error> {
    let mut req = agent(url, timeout, body)
        .get(url)
        .header("User-Agent", user_agent);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.call()
}

fn header<'a>(resp: &'a Response, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

fn read_limited(resp: &mut Response, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    resp.body_mut()
        .as_reader()
        .take(limit)
        .read_to_end(&mut out)?;
    Ok(out)
}

fn rate_limit(resp: &mut Response) -> Option<UpdateError> {
    let code = resp.status().as_u16();
    if code != 403 && code != 429 {
        return None;
    }
    let remaining_zero = header(resp, "X-RateLimit-Remaining") == Some("0");
    let retry = header(resp, "Retry-After").is_some();
    let reset = header(resp, "X-RateLimit-Reset").map(str::to_owned);
    let body = read_limited(resp, 4096).unwrap_or_default();
    let limited = code == 429
        || remaining_zero
        || retry
        || String::from_utf8_lossy(&body)
            .to_lowercase()
            .contains("rate limit");
    if !limited {
        return None;
    }
    match reset.and_then(|r| r.trim().parse::<f64>().ok()) {
        None => Some(UpdateError::Rate),
        Some(reset) => {
            let now = Utc::now().timestamp_millis() as f64 / 1000.0;
            Some(UpdateError::RateUntil {
                minutes: (((reset - now) / 60.0).ceil() as i64).max(1),
            })
        }
    }
}

/// The release list (or `None`: 304, the cached list still holds) and its ETag.
#[derive(Clone, Debug, PartialEq)]
pub struct Fetched {
    pub releases: Option<Vec<Release>>,
    pub etag: Option<String>,
}

/// GET the release list (blocking: run it on a blocking thread). `etag`: the cached list's, sent
/// as `If-None-Match` — an unchanged list comes back as 304 and does not count against the limit.
pub fn fetch_releases(
    feed: &str,
    etag: Option<&str>,
    user_agent: &str,
    timeout: Duration,
) -> Result<Fetched, UpdateError> {
    let mut headers = vec![("Accept", "application/vnd.github+json")];
    if let Some(e) = etag {
        headers.push(("If-None-Match", e));
    }
    let offline = |reason| UpdateError::Offline { reason };
    let mut resp = get(
        feed,
        user_agent,
        timeout,
        Some(timeout * SMALL_BODY),
        &headers,
    )
    .map_err(|e| network_error(&e, offline, true))?;
    let code = resp.status().as_u16();
    if code == 304 {
        return Ok(Fetched {
            releases: None,
            etag: etag.map(str::to_owned),
        });
    }
    if code >= 400 {
        return Err(rate_limit(&mut resp).unwrap_or(UpdateError::Http { code }));
    }
    if !(200..300).contains(&code) {
        return Err(UpdateError::Http { code });
    }
    let new_etag = header(&resp, "ETag").map(str::to_owned);
    let body = read_limited(&mut resp, LIST_MAX + 1).map_err(|e| io_error(&e, offline, true))?;
    if body.len() as u64 > LIST_MAX {
        return Err(UpdateError::BadList);
    }
    let data: Value = serde_json::from_slice(&body).map_err(|_| UpdateError::BadList)?;
    let releases = parse_releases(&data, feed).map_err(|_| UpdateError::BadList)?;
    Ok(Fetched {
        releases: Some(releases),
        etag: new_etag,
    })
}

/// The published SHA-256 of `asset` (blocking); `None` when the release publishes none for it.
/// [`UpdateError::Sums`] when a checksum file exists but cannot be read: an unverifiable download
/// is not started.
pub fn fetch_checksums(
    release: &Release,
    asset: &Asset,
    user_agent: &str,
    timeout: Duration,
) -> Result<Option<String>, UpdateError> {
    let sums_err = |reason| UpdateError::Sums { reason };
    for sums in checksum_files(release, asset) {
        let mut resp = get(
            &sums.url,
            user_agent,
            timeout,
            Some(timeout * SMALL_BODY),
            &[],
        )
        .map_err(|e| network_error(&e, sums_err, false))?;
        let code = resp.status().as_u16();
        if code >= 400 {
            return Err(UpdateError::Sums {
                reason: format!("HTTP {code}"),
            });
        }
        let body = read_limited(&mut resp, SUMS_MAX).map_err(|e| io_error(&e, sums_err, false))?;
        if let Some(found) = checksum_of(&String::from_utf8_lossy(&body), &asset.name) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// What a download is doing, for its progress callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Verifying,
}

fn file_sha256(path: &Path, cancel: &AtomicBool) -> Result<String, std::io::Error> {
    let mut f = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(hex(&digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Download `url` to `dest` (blocking): streamed to `<dest>.part`, hashed on the way, checked
/// against `sha256` and only then renamed. A `dest` that already has the right hash is kept as it
/// is. Returns `Some(true)`: the file matched its checksum. Without a published checksum
/// (`sha256` is `None`) nothing is fetched: [`UpdateError::NoChecksum`].
///
/// `timeout` bounds connecting, the response and each read: a connection that stalls fails
/// after it, and `cancel` takes effect within it. [`UpdateError::Cancelled`] once `cancel` is
/// set, another [`UpdateError`] otherwise; either way the partial file is gone, and a file whose
/// hash does not match is never kept.
#[allow(clippy::too_many_arguments)]
pub fn download(
    url: &str,
    dest: &Path,
    size: u64,
    sha256: Option<&str>,
    user_agent: &str,
    progress: &mut dyn FnMut(Phase, u64, u64),
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<Option<bool>, UpdateError> {
    let Some(exp) = sha256.map(str::to_lowercase) else {
        return Err(UpdateError::NoChecksum);
    };
    let write_err = |p: &Path, e: &std::io::Error| UpdateError::Write {
        path: p.to_owned(),
        reason: e.to_string(),
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| write_err(dest, &e))?;
    }
    if dest.is_file() {
        progress(Phase::Verifying, 0, size);
        match file_sha256(dest, cancel) {
            Ok(h) if h == exp => {
                progress(Phase::Verifying, size, size);
                return Ok(Some(true));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                return Err(UpdateError::Cancelled);
            }
            Err(e) => return Err(write_err(dest, &e)),
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    let part = part_path(dest);
    let result = (|| {
        let net = |reason| UpdateError::DownloadNet { reason };
        // no limit on the whole body: a large file on a slow line takes its time
        let mut resp =
            get(url, user_agent, timeout, None, &[]).map_err(|e| network_error(&e, net, false))?;
        let code = resp.status().as_u16();
        if code >= 400 {
            return Err(UpdateError::DownloadHttp { code });
        }
        let total = header(&resp, "Content-Length")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(size);
        let (received, digest) = stream(&mut resp, &part, total, progress, cancel)?;
        if total > 0 && received != total {
            return Err(UpdateError::Incomplete {
                received,
                expected: total,
            });
        }
        if digest != exp {
            return Err(UpdateError::Mismatch);
        }
        std::fs::rename(&part, dest).map_err(|e| write_err(dest, &e))
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        // cancelled while the connection was being made: that, not the error it ended with
        return Err(if cancel.load(Ordering::Relaxed) {
            UpdateError::Cancelled
        } else {
            e
        });
    }
    #[cfg(unix)]
    {
        if dest.extension().is_some_and(|e| e == "AppImage") {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(Some(true))
}

/// Copy the response into `part`: (bytes received, their SHA-256).
fn stream(
    resp: &mut Response,
    part: &Path,
    total: u64,
    progress: &mut dyn FnMut(Phase, u64, u64),
    cancel: &AtomicBool,
) -> Result<(u64, String), UpdateError> {
    let write_err = |e: &std::io::Error| UpdateError::Write {
        path: part.to_owned(),
        reason: e.to_string(),
    };
    let mut fh = File::create(part).map_err(|e| write_err(&e))?;
    let mut digest = Sha256::new();
    let mut received = 0u64;
    let mut last: Option<Instant> = None;
    let mut buf = vec![0u8; CHUNK];
    let mut reader = resp.body_mut().as_reader();
    progress(Phase::Downloading, 0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // cancelled while a read waited (a stalled connection): the read timeout ended it
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(UpdateError::Cancelled),
            Err(e) => {
                return Err(io_error(
                    &e,
                    |reason| UpdateError::DownloadNet { reason },
                    false,
                ));
            }
        };
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        fh.write_all(&buf[..n]).map_err(|e| write_err(&e))?;
        received += n as u64;
        let now = Instant::now();
        if last.is_none_or(|l| now - l >= PROGRESS_GAP) {
            last = Some(now);
            progress(Phase::Downloading, received, total);
        }
    }
    fh.flush().map_err(|e| write_err(&e))?;
    drop(fh);
    progress(Phase::Downloading, received, total);
    Ok((received, hex(&digest.finalize())))
}

#[cfg(test)]
mod tests;
