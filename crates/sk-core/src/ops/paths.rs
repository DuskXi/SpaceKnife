//! Path forms for copying and the built-in list of protected paths (docs/manage.md §2.1).
//!
//! Every frontend copies paths through [`path_forms`], so all of them put the same text on the
//! clipboard. [`protected_reason`] is asked when an item is added to the delete basket and again
//! right before it is deleted or moved to the trash.
//!
//! The protection lists are per platform ([`platform_rules`]); [`match_rules`] applies one to a
//! path without touching the disk (Windows and POSIX path rules are implemented here, not taken
//! from the host), so every platform's list is tested on every platform. [`protected_reason`]
//! adds what needs the disk: mount points, and paths whose parent folders lead through a link
//! into a protected place.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use regex::Regex;
use sk_protocol::ops::PathForms;

use super::text::path_text;

/// Which platform's path rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum System {
    Windows,
    Linux,
    Darwin,
}

impl System {
    /// The system this runs on.
    pub fn this() -> System {
        if cfg!(windows) {
            System::Windows
        } else if cfg!(target_os = "macos") {
            System::Darwin
        } else {
            System::Linux
        }
    }
}

// --------------------------------------------------------------------------- path forms

/// `\\?\C:\a` → `C:\a` and `\\?\UNC\host\share` → `\\host\share`.
pub fn strip_verbatim(path: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if let Some(rest) = path
        .strip_prefix("\\\\?\\UNC\\")
        .or_else(|| path.strip_prefix("\\\\?\\unc\\"))
    {
        return Cow::Owned(format!("\\\\{rest}"));
    }
    if let Some(rest) = path.strip_prefix("\\\\?\\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return Cow::Borrowed(rest);
    }
    Cow::Borrowed(path)
}

/// `urllib.parse.quote(data, safe="/")`: everything but unreserved characters and `/` as `%XX`.
pub fn quote_path(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 3);
    for &b in data {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The four copy formats of `path` (an absolute path of `system`, default: this one).
///
/// Windows: `raw` `C:\a b`, `quoted` `"C:\a b"`, `posix` the WSL form `/mnt/c/a b` (drive letter
/// lower-cased under `/mnt/`), `uri` `file:///C:/a%20b` (UNC `\\host\share\x` → `//host/share/x`
/// and `file://host/share/x`). POSIX: `quoted` is `'…'` with each `'` written as `'\''`; `posix`
/// is the path. `on_disk` is always true here (the server clears it for pseudo folders).
pub fn path_forms(path: &str, system: Option<System>) -> PathForms {
    path_forms_bytes(path.as_bytes(), system)
}

/// [`path_forms`] of a path as the store keeps names (WTF-8 on Windows, raw bytes on POSIX): the
/// URI percent-encodes those bytes (a lone surrogate as its WTF-8 bytes, an undecodable POSIX
/// byte as itself); the text forms show such bytes as U+FFFD.
pub fn path_forms_bytes(raw: &[u8], system: Option<System>) -> PathForms {
    let text = String::from_utf8_lossy(raw).into_owned();
    if system.unwrap_or_else(System::this) == System::Windows {
        let plain = strip_verbatim(&text);
        // the same on the bytes (the prefixes are ASCII)
        let mut data: Vec<u8> =
            if raw.starts_with(b"\\\\?\\UNC\\") || raw.starts_with(b"\\\\?\\unc\\") {
                [b"\\\\".as_slice(), &raw[8..]].concat()
            } else if raw.starts_with(b"\\\\?\\") && raw.get(5) == Some(&b':') {
                raw[4..].to_vec()
            } else {
                raw.to_vec()
            };
        for b in data.iter_mut() {
            if *b == b'\\' {
                *b = b'/';
            }
        }
        let slashed = plain.replace('\\', "/");
        let drive = slashed.as_bytes().get(1) == Some(&b':');
        let uri = if drive {
            format!("file:///{}{}", &slashed[..2], quote_path(&data[2..]))
        } else if slashed.starts_with("//") {
            format!("file:{}", quote_path(&data))
        } else {
            let start = data.iter().take_while(|&&b| b == b'/').count();
            format!("file:///{}", quote_path(&data[start..]))
        };
        let posix = if drive {
            format!("/mnt/{}{}", slashed[..1].to_lowercase(), &slashed[2..])
        } else {
            slashed
        };
        return PathForms {
            raw: text.clone(),
            quoted: format!("\"{text}\""),
            posix,
            uri,
            on_disk: true,
        };
    }
    let quoted = format!("'{}'", text.replace('\'', "'\\''"));
    PathForms {
        raw: text.clone(),
        quoted,
        posix: text,
        uri: format!("file://{}", quote_path(raw)),
        on_disk: true,
    }
}

// --------------------------------------------------------------------------- path rules per system

fn nt_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// `ntpath.splitroot`: (drive, root, rest).
fn nt_splitroot(p: &str) -> (&str, &str, &str) {
    let b = p.as_bytes();
    if b.len() >= 2 && nt_sep(b[0] as char) && nt_sep(b[1] as char) {
        // UNC: \\server\share
        let rest = &p[2..];
        let i = rest.find(nt_sep).map_or(p.len(), |i| i + 2);
        let after = if i < p.len() { &p[i + 1..] } else { "" };
        let j = after.find(nt_sep).map_or(p.len(), |j| i + 1 + j);
        if j >= p.len() {
            return (p, "", "");
        }
        return (&p[..j], &p[j..j + 1], &p[j + 1..]);
    }
    if b.len() >= 2 && b[1] == b':' {
        let drive = &p[..2];
        if b.len() > 2 && nt_sep(b[2] as char) {
            return (drive, &p[2..3], &p[3..]);
        }
        return (drive, "", &p[2..]);
    }
    if !b.is_empty() && nt_sep(b[0] as char) {
        return ("", &p[..1], &p[1..]);
    }
    ("", "", p)
}

/// `ntpath.normpath` (on any host).
pub fn nt_normpath(path: &str) -> String {
    let p = path.replace('/', "\\");
    let (drive, root, rest) = nt_splitroot(&p);
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if root.is_empty() {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let mut out = format!("{drive}{root}{}", parts.join("\\"));
    if out.is_empty() {
        out.push('.');
    }
    out
}

/// `ntpath.normcase`.
pub fn nt_normcase(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// `ntpath.dirname`.
pub fn nt_dirname(p: &str) -> String {
    let (drive, root, rest) = nt_splitroot(p);
    let i = rest.rfind(nt_sep).map_or(0, |i| i + 1);
    let head = rest[..i].trim_end_matches(nt_sep);
    format!("{drive}{root}{head}")
}

/// `ntpath.join(base, part)` for plain relative parts.
fn nt_join(base: &str, part: &str) -> String {
    if base.is_empty() || base.ends_with(nt_sep) || base.ends_with(':') {
        format!("{base}{part}")
    } else {
        format!("{base}\\{part}")
    }
}

/// `posixpath.normpath` (on any host).
pub fn posix_normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let lead = path.len() - path.trim_start_matches('/').len();
    let initial = match lead {
        0 => "",
        2 => "//",
        _ => "/",
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if initial.is_empty() {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let out = format!("{initial}{}", parts.join("/"));
    if out.is_empty() { ".".into() } else { out }
}

/// `posixpath.dirname`.
pub fn posix_dirname(p: &str) -> String {
    let i = p.rfind('/').map_or(0, |i| i + 1);
    let head = &p[..i];
    if !head.is_empty() && !head.chars().all(|c| c == '/') {
        head.trim_end_matches('/').to_string()
    } else {
        head.to_string()
    }
}

fn posix_join(base: &str, part: &str) -> String {
    if base.is_empty() || base.ends_with('/') {
        format!("{base}{part}")
    } else {
        format!("{base}/{part}")
    }
}

// --------------------------------------------------------------------------- protection rules

/// One platform's protected paths. Keys are normalised ([`Rules::key`]); values are reason keys
/// (texts: [`reason_text`]).
#[derive(Clone, Debug)]
pub struct Rules {
    pub system: System,
    /// the path itself
    pub exact: HashMap<String, &'static str>,
    /// the path and everything below it
    pub tree: HashMap<String, &'static str>,
    /// below a tree rule: what is *inside* these may go
    pub allow: HashSet<String>,
    /// each direct child (homes in /home)
    pub children: HashMap<String, &'static str>,
    /// Windows: entries of any volume root ...
    pub root_names: HashMap<String, &'static str>,
    /// ... and everything below them
    pub root_trees: HashMap<String, &'static str>,
    /// any component matching (and below it)
    pub names: Vec<(Regex, &'static str)>,
}

impl Rules {
    fn empty(system: System) -> Rules {
        Rules {
            system,
            exact: HashMap::new(),
            tree: HashMap::new(),
            allow: HashSet::new(),
            children: HashMap::new(),
            root_names: HashMap::new(),
            root_trees: HashMap::new(),
            names: Vec::new(),
        }
    }

    pub fn windows(&self) -> bool {
        self.system == System::Windows
    }

    /// A path as the tables key it.
    pub fn key(&self, path: &str) -> String {
        if self.windows() {
            nt_normcase(&nt_normpath(&strip_verbatim(path)))
        } else {
            posix_normpath(path)
        }
    }

    pub fn parent(&self, key: &str) -> String {
        if self.windows() {
            nt_dirname(key)
        } else {
            posix_dirname(key)
        }
    }

    /// Every reason key the rules use.
    pub fn reasons(&self) -> HashSet<&'static str> {
        let mut out: HashSet<&'static str> = HashSet::new();
        for m in [
            &self.exact,
            &self.tree,
            &self.children,
            &self.root_names,
            &self.root_trees,
        ] {
            out.extend(m.values().copied());
        }
        out.extend(self.names.iter().map(|(_, r)| *r));
        out
    }

    fn put(&mut self, which: Table, reason: &'static str, paths: &[String]) {
        for p in paths.iter().filter(|p| !p.is_empty()) {
            let k = self.key(p);
            let table = match which {
                Table::Exact => &mut self.exact,
                Table::Tree => &mut self.tree,
                Table::Children => &mut self.children,
            };
            table.entry(k).or_insert(reason);
        }
    }
}

#[derive(Clone, Copy)]
enum Table {
    Exact,
    Tree,
    Children,
}

const WIN_PROFILE: &[&[&str]] = &[
    &["AppData"],
    &["AppData", "Local"],
    &["AppData", "LocalLow"],
    &["AppData", "Roaming"],
];
const POSIX_PROFILE: &[&[&str]] = &[
    &[".config"],
    &[".local"],
    &[".local", "share"],
    &[".local", "state"],
];
const MAC_PROFILE: &[&[&str]] = &[
    &["Library"],
    &["Library", "Application Support"],
    &["Library", "Mobile Documents"],
];
const LINUX_SYSTEM: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/libx32", "/proc", "/run",
    "/sbin", "/sys", "/usr", "/efi",
];
const LINUX_SHARED: &[&str] = &[
    "/var",
    "/var/lib",
    "/var/log",
    "/var/cache",
    "/var/tmp",
    "/tmp",
    "/opt",
    "/srv",
    "/mnt",
    "/media",
    "/snap",
    "/lost+found",
];
const MAC_SYSTEM: &[&str] = &[
    "/System",
    "/bin",
    "/sbin",
    "/usr",
    "/dev",
    "/etc",
    "/private/etc",
    "/private/var/db",
    "/private/var/vm",
    "/Library/Apple",
    "/cores",
];
const MAC_SHARED: &[&str] = &[
    "/Library",
    "/Volumes",
    "/private",
    "/private/var",
    "/private/tmp",
    "/var",
    "/tmp",
    "/opt",
];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The built-in protected paths of `system` for a user with `home`.
///
/// `env`: environment of a Windows system (`SystemRoot`, `ProgramFiles` …; defaults filled in) or
/// of a POSIX one (`XDG_DATA_HOME`). `app_dirs`: SpaceKnife's data, config and cache folders;
/// `program_dirs`: where the running program lives.
pub fn platform_rules(
    system: System,
    home: &str,
    env: &HashMap<String, String>,
    app_dirs: &[String],
    program_dirs: &[String],
) -> Rules {
    let get = |k: &str| env.get(k).cloned().unwrap_or_default();
    let get_or = |k: &str, d: String| env.get(k).cloned().unwrap_or(d);
    let mut r = Rules::empty(system);
    if system == System::Windows {
        let drive = get_or("SystemDrive", "C:".into());
        let sysroot = [get("SystemRoot"), get("windir")]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or_else(|| format!("{drive}\\Windows"));
        r.put(Table::Tree, "system", std::slice::from_ref(&sysroot));
        r.allow.insert(r.key(&format!("{sysroot}\\Temp")));
        r.put(
            Table::Exact,
            "program_root",
            &[
                get_or("ProgramFiles", format!("{drive}\\Program Files")),
                get_or("ProgramFiles(x86)", format!("{drive}\\Program Files (x86)")),
                get("ProgramW6432"),
            ],
        );
        r.put(
            Table::Exact,
            "data_root",
            &[get_or("ProgramData", format!("{drive}\\ProgramData"))],
        );
        let users = if home.is_empty() {
            format!("{drive}\\Users")
        } else {
            nt_dirname(home)
        };
        r.put(Table::Exact, "users_root", std::slice::from_ref(&users));
        r.put(Table::Children, "home", &[users]);
        r.put(Table::Exact, "home", &[home.to_string()]);
        let profile: Vec<String> = WIN_PROFILE
            .iter()
            .map(|parts| {
                parts
                    .iter()
                    .fold(home.to_string(), |acc, p| nt_join(&acc, p))
            })
            .collect();
        r.put(Table::Exact, "profile", &profile);
        let boot: Vec<String> = ["Recovery", "Boot", "EFI", "$WinREAgent"]
            .iter()
            .map(|n| format!("{drive}\\{n}"))
            .collect();
        r.put(Table::Tree, "boot", &boot);
        r.put(
            Table::Exact,
            "boot",
            &[format!("{drive}\\bootmgr"), format!("{drive}\\BOOTNXT")],
        );
        for n in ["pagefile.sys", "hiberfil.sys", "swapfile.sys"] {
            r.root_names.insert(n.into(), "system_file");
        }
        r.root_trees.insert("$recycle.bin".into(), "trash");
        r.root_trees
            .insert("system volume information".into(), "system");
    } else {
        if system == System::Linux {
            r.put(Table::Tree, "system", &strings(LINUX_SYSTEM));
            r.put(
                Table::Tree,
                "package_db",
                &strings(&[
                    "/var/lib/dpkg",
                    "/var/lib/rpm",
                    "/var/lib/dnf",
                    "/var/lib/pacman",
                ]),
            );
            for a in ["/run/media", "/run/user", "/run/mount"] {
                let k = r.key(a);
                r.allow.insert(k);
            }
            r.put(Table::Exact, "shared_root", &strings(LINUX_SHARED));
            r.put(Table::Exact, "users_root", &strings(&["/home"]));
            r.put(Table::Children, "home", &strings(&["/home"]));
            r.put(Table::Exact, "home", &strings(&["/root"]));
        } else {
            r.put(Table::Tree, "system", &strings(MAC_SYSTEM));
            r.put(
                Table::Exact,
                "program_root",
                &strings(&["/Applications", "/System/Applications"]),
            );
            r.put(Table::Exact, "shared_root", &strings(MAC_SHARED));
            r.put(Table::Exact, "users_root", &strings(&["/Users"]));
            r.put(Table::Children, "home", &strings(&["/Users"]));
            let profile: Vec<String> = MAC_PROFILE
                .iter()
                .map(|parts| {
                    parts
                        .iter()
                        .fold(home.to_string(), |acc, p| posix_join(&acc, p))
                })
                .collect();
            r.put(Table::Exact, "profile", &profile);
        }
        r.put(Table::Exact, "home", &[home.to_string()]);
        let profile: Vec<String> = POSIX_PROFILE
            .iter()
            .map(|parts| {
                parts
                    .iter()
                    .fold(home.to_string(), |acc, p| posix_join(&acc, p))
            })
            .collect();
        r.put(Table::Exact, "profile", &profile);
        let xdg = get("XDG_DATA_HOME");
        let data_home = if xdg.starts_with('/') {
            xdg
        } else {
            posix_join(&posix_join(home, ".local"), "share")
        };
        r.put(
            Table::Tree,
            "trash",
            &[
                posix_join(&data_home, "Trash"),
                posix_join(&posix_join(&posix_join(home, ".local"), "share"), "Trash"),
            ],
        );
        r.names = vec![
            // freedesktop $topdir/.Trash, .Trash-$uid; macOS ~/.Trash
            (Regex::new(r"^\.Trash(-\d+)?$").unwrap(), "trash"),
            // macOS volumes
            (Regex::new(r"^\.Trashes$").unwrap(), "trash"),
        ];
    }
    r.put(Table::Tree, "app", app_dirs);
    r.put(Table::Tree, "program", program_dirs);
    r
}

/// Why `path` is protected by `rules` (a reason key), or None. Pure: never touches the disk.
pub fn match_rules(path: &str, rules: &Rules) -> Option<&'static str> {
    let key = rules.key(path);
    let parts: Vec<&str> = if rules.windows() {
        let (_, root, rest) = nt_splitroot(&key);
        let rest_all = format!("{root}{rest}");
        if rest_all.trim_matches('\\').is_empty() {
            return Some("root");
        }
        rest.split('\\').filter(|p| !p.is_empty()).collect()
    } else {
        if key == "/" || key == "//" {
            return Some("root");
        }
        key.split('/').filter(|p| !p.is_empty()).collect()
    };
    if let Some(hit) = rules.exact.get(&key) {
        return Some(hit);
    }
    if let Some(hit) = rules.children.get(&rules.parent(&key)) {
        return Some(hit);
    }
    if rules.windows() {
        if parts.len() == 1
            && let Some(hit) = rules.root_names.get(parts[0])
        {
            return Some(hit);
        }
        if let Some(first) = parts.first()
            && let Some(hit) = rules.root_trees.get(*first)
        {
            return Some(hit);
        }
    }
    for (pattern, reason) in &rules.names {
        if parts.iter().any(|p| pattern.is_match(p)) {
            return Some(reason);
        }
    }
    // the nearest enclosing tree rule, unless an allowed folder lies between it and the path
    let mut node = key.clone();
    let mut allowed = false;
    loop {
        if node != key && rules.allow.contains(&node) {
            allowed = true;
        }
        if let Some(hit) = rules.tree.get(&node) {
            return if allowed { None } else { Some(hit) };
        }
        let up = rules.parent(&node);
        if up == node {
            return None;
        }
        node = up;
    }
}

/// The text of a reason key, in the current language.
pub fn reason_text(key: &str) -> &'static str {
    path_text(key)
}

// --------------------------------------------------------------------------- this system

const ENV_KEYS: &[&str] = &[
    "SystemDrive",
    "SystemRoot",
    "windir",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    "XDG_DATA_HOME",
];

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// SpaceKnife's own folders (data, config, cache), both as named and resolved like Python's
/// `Path.resolve()` (resolved again only when they move, i.e. with `SPACEKNIFE_HOME`).
///
/// Both forms: when a folder is a link (a symlinked `SPACEKNIFE_HOME`, or the per-package
/// redirection Windows applies to `AppData` for packaged apps) its contents are reached through
/// the resolved form, but the folder itself is the link — [`protected_key`] resolves only the
/// parents of a path, so the link would otherwise not be recognised as the app's folder.
fn app_dirs() -> Vec<String> {
    type Resolved = Option<(Vec<PathBuf>, Vec<String>)>;
    static CACHE: LazyLock<Mutex<Resolved>> = LazyLock::new(|| Mutex::new(None));
    let raw = vec![
        crate::paths::data_dir(),
        crate::paths::config_dir(),
        crate::paths::cache_dir(),
    ];
    let mut slot = CACHE.lock();
    if let Some((k, v)) = slot.as_ref()
        && *k == raw
    {
        return v.clone();
    }
    let resolved = app_dir_forms(&raw);
    *slot = Some((raw, resolved.clone()));
    resolved
}

/// Each folder resolved (links followed) and as named (absolute), without repeats.
pub fn app_dir_forms(dirs: &[PathBuf]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(dirs.len() * 2);
    for d in dirs {
        for form in [path_string(&resolve_lenient(d)), path_string(&absolute(d))] {
            if !out.contains(&form) {
                out.push(form);
            }
        }
    }
    out
}

/// Where the running program lives.
fn program_dirs() -> Vec<String> {
    static DIRS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| {
        let mut out: Vec<String> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            let exe = resolve_lenient(&exe);
            if let Some(dir) = exe.parent() {
                out.push(path_string(dir));
            }
        }
        out
    })
    .clone()
}

type RulesKey = (String, Vec<(String, String)>, Vec<String>, Vec<String>);
type CachedRules = Option<(RulesKey, Arc<Rules>)>;
static RULES: LazyLock<Mutex<CachedRules>> = LazyLock::new(|| Mutex::new(None));

/// The rules of this system and user (rebuilt when the environment or app folders change).
pub fn current_rules() -> Arc<Rules> {
    let env: Vec<(String, String)> = ENV_KEYS
        .iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| (k.to_string(), v))
        })
        .collect();
    let key: RulesKey = (
        path_string(&crate::paths::home_dir()),
        env,
        app_dirs(),
        program_dirs(),
    );
    let mut slot = RULES.lock();
    if let Some((k, rules)) = slot.as_ref()
        && *k == key
    {
        return rules.clone();
    }
    let env_map: HashMap<String, String> = key.1.iter().cloned().collect();
    let rules = Arc::new(platform_rules(
        System::this(),
        &key.0,
        &env_map,
        &key.2,
        &key.3,
    ));
    *slot = Some((key, rules.clone()));
    rules
}

/// A path with its links resolved where it exists, without the `\\?\` prefix Windows adds;
/// unchanged where it cannot be resolved (Python's non-strict `realpath`).
pub fn resolve_lenient(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(real) => match real.to_str() {
            Some(s) if strip_verbatim(s).len() != s.len() => {
                PathBuf::from(strip_verbatim(s).into_owned())
            }
            _ => real,
        },
        Err(_) => {
            // resolve the part that exists, keep the rest
            if let (Some(parent), Some(name)) = (p.parent(), p.file_name())
                && !parent.as_os_str().is_empty()
                && parent != p
            {
                return resolve_lenient(parent).join(name);
            }
            p.to_path_buf()
        }
    }
}

const REAL_TTL: Duration = Duration::from_secs(2); // a resolved parent is reused (one basket add)
const REAL_MAX: usize = 1024;
static REAL_PARENTS: LazyLock<Mutex<HashMap<PathBuf, (Instant, PathBuf)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn real_parent(parent: &Path) -> PathBuf {
    let now = Instant::now();
    if let Some((when, real)) = REAL_PARENTS.lock().get(parent)
        && now.duration_since(*when) < REAL_TTL
    {
        return real.clone();
    }
    let real = resolve_lenient(parent);
    let mut cache = REAL_PARENTS.lock();
    if cache.len() >= REAL_MAX {
        cache.clear();
    }
    cache.insert(parent.to_path_buf(), (now, real.clone()));
    real
}

/// An absolute form of `path` (without touching the disk beyond the current directory).
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` with its parent folders resolved (links followed), the last component kept.
pub fn resolved(path: &Path) -> PathBuf {
    let abs = absolute(path);
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => real_parent(parent).join(name),
        _ => resolve_lenient(&abs),
    }
}

/// Whether `path` is a mount point (only folders can be: files skip the volume lookup).
pub fn is_mount(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !is_folder_entry(&meta) {
        return false;
    }
    is_mount_dir(path)
}

/// A folder as the entry itself is (links not followed). On Windows a folder mount point is a
/// directory reparse point, which Rust reports as a symlink and not as a directory: the
/// directory attribute tells.
fn is_folder_entry(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        meta.is_dir()
    }
}

#[cfg(windows)]
fn is_mount_dir(path: &Path) -> bool {
    let abs = path_string(&absolute(path));
    let (_, _, rest) = nt_splitroot(&abs);
    if rest.trim_matches(nt_sep).is_empty() {
        return true; // a drive or share root
    }
    match super::win::volume_root(&abs) {
        Ok(root) => {
            nt_normcase(root.trim_end_matches(nt_sep)) == nt_normcase(abs.trim_end_matches(nt_sep))
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn is_mount_dir(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(me) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let Ok(up) = std::fs::symlink_metadata(path.join("..")) else {
        return false;
    };
    me.dev() != up.dev() || me.ino() == up.ino()
}

#[cfg(not(any(windows, unix)))]
fn is_mount_dir(_path: &Path) -> bool {
    false
}

/// Why `path` must not be deleted, as a reason key ([`reason_text`]), or None when it may be.
///
/// Volume roots and mount points, system folders, program and data roots, user homes
/// themselves, trash folders and SpaceKnife's own folders. Also checks the path with its parent
/// folders resolved, so a link into `/usr` or `C:\Windows` does not open a way around the list.
pub fn protected_key(path: &str) -> Option<&'static str> {
    let rules = current_rules();
    let mut reason = match_rules(path, &rules);
    if reason.is_none() {
        let real = path_string(&resolved(Path::new(path)));
        if rules.key(&real) != rules.key(path) {
            reason = match_rules(&real, &rules);
        }
    }
    if reason.is_none() && is_mount(Path::new(path)) {
        reason = Some("mount");
    }
    reason
}

/// [`protected_key`] as the text for the user (current language).
pub fn protected_reason(path: &str) -> Option<String> {
    protected_key(path).map(|k| path_text(k).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data folder that is a link (a symlinked SPACEKNIFE_HOME, Windows' AppData redirection
    /// for packaged apps): the link itself and everything below it are the app's.
    #[test]
    fn linked_app_folder_is_protected_as_named_and_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("snapshots")).unwrap();
        let link = tmp.path().join("SpaceKnife");
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .is_ok_and(|o| o.status.success());
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&real, &link).is_ok();
        #[cfg(not(any(windows, unix)))]
        let made = false;
        if !made {
            return;
        }
        let forms = app_dir_forms(std::slice::from_ref(&link));
        assert_eq!(forms.len(), 2, "{forms:?}");
        // a home elsewhere (its parent's children are homes)
        let home = if cfg!(windows) {
            r"Q:\Users\u"
        } else {
            "/nonexistent/home/u"
        }
        .to_string();
        let rules = platform_rules(System::this(), &home, &HashMap::new(), &forms, &[]);
        let link_s = path_string(&link);
        assert_eq!(match_rules(&link_s, &rules), Some("app"), "{forms:?}");
        let below = path_string(&resolved(&link.join("snapshots").join("c.sks")));
        assert_eq!(
            match_rules(&below, &rules),
            Some("app"),
            "{below} / {forms:?}"
        );
    }

    #[test]
    fn nt_helpers() {
        assert_eq!(nt_normpath("C:/a//b/./c/../d"), "C:\\a\\b\\d");
        assert_eq!(nt_normpath("C:"), "C:");
        assert_eq!(nt_normpath("\\\\nas\\share\\\\"), "\\\\nas\\share\\");
        assert_eq!(nt_dirname("c:\\windows"), "c:\\");
        assert_eq!(nt_dirname("c:\\"), "c:\\");
        assert_eq!(nt_dirname("\\\\nas\\share\\x"), "\\\\nas\\share\\");
        assert_eq!(
            posix_normpath("/home/u/./Downloads/../.config"),
            "/home/u/.config"
        );
        assert_eq!(posix_normpath("//x"), "//x");
        assert_eq!(posix_normpath("///x"), "/x");
        assert_eq!(posix_dirname("/home"), "/");
        assert_eq!(posix_dirname("/"), "/");
        assert_eq!(strip_verbatim("\\\\?\\UNC\\host\\share"), "\\\\host\\share");
        assert_eq!(strip_verbatim("\\\\?\\C:\\x"), "C:\\x");
    }
}
