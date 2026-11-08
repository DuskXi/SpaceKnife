//! Folder classes: a recognised folder decides the class of the files in it (UI-W7-38; port of
//! `spaceknife.pathclass`).
//!
//! The extension says what a file *is*; some folders say more. Everything inside a game install is
//! the game's, whatever its extension, and everything in a project is the project's. The built-in
//! rules (any platform's, so a snapshot from another system reads the same):
//!
//! - **game** (class `game`): Steam libraries (`steamapps`); the game folders of launchers (Epic,
//!   GOG, Xbox, EA, Ubisoft, Riot, HoYoPlay / miHoYo, Hypergryph); builds of game engines (Unity:
//!   `UnityPlayer.dll`, or `<name>.exe` beside `<name>_Data`; Unreal: `<game>/Content/Paks`;
//!   Godot: a `.pck` beside an `.exe`; Blizzard: `.build.info`); games' folders in a home
//!   (`.minecraft`, `Documents/My Games`, `Documents/Paradox Interactive`, `Saved Games`);
//! - **project** (class `code`): a folder holding a project marker (`.git`, `package.json`,
//!   `Cargo.toml`, `*.sln` …), never inside software, caches or package stores, and never a
//!   volume, a home or a standard user folder;
//! - **models** (class `data`): `.ollama`, `.lmstudio`, `.cache/huggingface` and the like;
//! - **system** (class `exec`): `System Volume Information`, the MFT scan's `$NTFS` folder;
//! - **software**, a fallback (class `exec`): `Program Files`, `ProgramData`, `Windows` at the
//!   top of a volume, `/usr`, `/opt`, `/Applications`; a home's `AppData`, `Library`, `.local`,
//!   `.config`.
//!
//! A rule *swallows* its folder (every file in it takes the class), except a fallback: there only
//! files whose extension is in no class take it. An outer folder wins over the folders inside it,
//! a swallowing rule over a fallback. A rule whose class is not in the table is off. The user's
//! choices (`class_folders.json`, [`set_folder`]) win over the rules, the deepest one inside its
//! own folder; `""` means "by extension only".
//!
//! The result is one `u8` per node ([`FolderClasses::column`]): 0 nothing, 1–127 the swallowing
//! class's index + 1, 128–255 the fallback class's index + 128. It is computed in one pass over
//! the folders by depth and kept per store (the caller keeps one [`FolderClasses`] beside each
//! store), with two more bytes per node. Rows that a scan or a change adds take their parent's
//! value; only folders that gain or lose what made them a rule's folder are worked out again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use sk_protocol::classes::OTHER;
use sk_store::lookup::PathIndex;
use sk_store::subtree::subtree_ids;
use sk_store::{Flags, Kind, NodeId, Nodes};

use crate::classes::ClassTable;

const GAME: u8 = 1;
const PROJECT: u8 = 2;
const MODELS: u8 = 3;
const SYSTEM: u8 = 4;
const SOFTWARE: u8 = 5;

fn rule_class(rule: u8) -> &'static str {
    match rule {
        GAME => "game",
        PROJECT => "code",
        MODELS => "data",
        _ => "exec", // SYSTEM, SOFTWARE
    }
}

fn is_fallback(rule: u8) -> bool {
    rule == SOFTWARE
}

/// Column values 1..=127 swallow, 128..=255 fall back.
pub const SWALLOW_MAX: usize = 127;
pub const FALLBACK_BASE: u8 = 128;

fn t(s: &str) -> String {
    crate::i18n::t(s).to_owned()
}

// --------------------------------------------------------------------------- the names the rules know

const PROJECT_FILES: &[&str] = &[
    "package.json",
    "cargo.toml",
    "pyproject.toml",
    "setup.py",
    "go.mod",
    "cmakelists.txt",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "pom.xml",
    "composer.json",
    "gemfile",
    "pubspec.yaml",
    "package.swift",
    "mix.exs",
    "deno.json",
    "meson.build",
    "project.godot",
];
const PROJECT_ANY: &[&str] = &[".git", ".hg", ".svn"];
const PROJECT_EXTS: &[&str] = &[
    "sln",
    "csproj",
    "vcxproj",
    "fsproj",
    "vbproj",
    "uproject",
    "xcodeproj",
    "xcworkspace",
];
const GAME_FILES: &[(&str, &str)] = &[
    ("unityplayer.dll", "Unity 游戏"),
    (".build.info", "暴雪游戏"),
];
const CANON: &[(&str, &str)] = &[
    ("cargo.toml", "Cargo.toml"),
    ("cmakelists.txt", "CMakeLists.txt"),
    ("gemfile", "Gemfile"),
    ("package.swift", "Package.swift"),
];
const LAUNCHERS: &[(&str, &[&str])] = &[
    (
        "epic games",
        &["launcher", "directxredist", "epiconlineservices"],
    ),
    ("gog games", &[]),
    ("xboxgames", &["gamesave"]),
    ("ea games", &[]),
    ("riot games", &["riot client"]),
];
const LAUNCHER_HOMES: &[&str] = &[
    "gog galaxy",
    "ubisoft game launcher",
    "hoyoplay",
    "mihoyo launcher",
    "hypergryph launcher",
];
const STEAM: &str = "steamapps";
const MINECRAFT: &str = ".minecraft";
const SAVES_IN_DOCUMENTS: &[&str] = &["my games", "paradox interactive"];
const SAVES_IN_HOME: &[&str] = &["saved games"];
const MODELS_ANYWHERE: &[&str] = &[".ollama", ".lmstudio"];
const MODELS_IN_CACHE: &[&str] = &["huggingface", "lm-studio", "torch"];
const SYSTEM_AT_TOP: &[&str] = &["system volume information"];
const SOFTWARE_AT_TOP: &[&str] = &[
    "program files",
    "program files (x86)",
    "programdata",
    "windows",
    "applications",
    "usr",
    "opt",
    "library",
    "system",
];
const SOFTWARE_IN_HOME: &[&str] = &["appdata", "library", ".local", ".config"];
const NO_PROJECTS: &[&str] = &[
    "node_modules",
    "site-packages",
    "dist-packages",
    ".cargo",
    ".rustup",
    ".npm",
    ".gradle",
    ".m2",
    ".nuget",
    ".vscode",
    ".vscode-server",
    ".cursor",
    ".cache",
    ".conda",
    "anaconda3",
    "miniconda3",
    ".pyenv",
    "$recycle.bin",
    ".trash",
    "windowsapps",
    "steamapps",
];
const USER_FOLDERS: &[&str] = &[
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "videos",
    "music",
    "movies",
    "public",
    "dropbox",
    "icloud drive",
    "saved games",
    "favorites",
    "links",
    "contacts",
    "searches",
    "3d objects",
];
const HOME_PARENTS: &[&str] = &["users", "home"];
const HELPERS: &[&str] = &["games", "content", "paks", "engine", ".cache", "go", "pkg"];

struct Known {
    names: HashSet<&'static [u8]>,
    keys: HashSet<u32>,
    max_len: usize,
}

fn key(name: &[u8]) -> u32 {
    ((name.len() as u32) << 16)
        | ((name[0].to_ascii_lowercase() as u32) << 8)
        | name[name.len() - 1].to_ascii_lowercase() as u32
}

static KNOWN: LazyLock<Known> = LazyLock::new(|| {
    let mut names: HashSet<&'static [u8]> = HashSet::new();
    let lists: [&[&str]; 16] = [
        PROJECT_FILES,
        PROJECT_ANY,
        LAUNCHER_HOMES,
        SAVES_IN_DOCUMENTS,
        SAVES_IN_HOME,
        MODELS_ANYWHERE,
        MODELS_IN_CACHE,
        SYSTEM_AT_TOP,
        SOFTWARE_AT_TOP,
        SOFTWARE_IN_HOME,
        NO_PROJECTS,
        USER_FOLDERS,
        HOME_PARENTS,
        HELPERS,
        &[STEAM],
        &[MINECRAFT],
    ];
    for l in lists {
        names.extend(l.iter().map(|s| s.as_bytes()));
    }
    names.extend(GAME_FILES.iter().map(|(n, _)| n.as_bytes()));
    for (l, kids) in LAUNCHERS {
        names.insert(l.as_bytes());
        names.extend(kids.iter().map(|s| s.as_bytes()));
    }
    let keys = names.iter().map(|n| key(n)).collect();
    let max_len = names.iter().map(|n| n.len()).max().unwrap_or(0);
    Known {
        names,
        keys,
        max_len,
    }
});

static VOLUME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[a-z]:[\\/]?|/|[\\/]{2}[^\\/]+[\\/][^\\/]+[\\/]?)$").unwrap()
});
static HOME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[\\/])(?:users|home)[\\/][^\\/]+[\\/]?$").unwrap());

// --------------------------------------------------------------------------- the user's choices

struct Choices {
    /// normalised path -> (path as given, class id; "" = by extension)
    folders: HashMap<String, (String, String)>,
}

static CHOICES: LazyLock<Mutex<Choices>> = LazyLock::new(|| {
    Mutex::new(Choices {
        folders: HashMap::new(),
    })
});
static CHOICES_VERSION: AtomicU64 = AtomicU64::new(0);

fn norm(path: &str) -> String {
    let n = sk_store::names::normpath(path.as_bytes());
    String::from_utf8_lossy(&sk_store::names::normcase(&n)).into_owned()
}

/// The user's choices, `(path, class id)`, sorted by path.
pub fn folders() -> Vec<(String, String)> {
    let mut v: Vec<_> = CHOICES.lock().unwrap().folders.values().cloned().collect();
    v.sort();
    v
}

pub fn folders_version() -> u64 {
    CHOICES_VERSION.load(Ordering::Acquire)
}

/// Replaces every choice.
pub fn replace_folders(items: impl IntoIterator<Item = (String, String)>) {
    let mut c = CHOICES.lock().unwrap();
    c.folders = items
        .into_iter()
        .map(|(p, cid)| (norm(&p), (p, cid)))
        .collect();
    CHOICES_VERSION.fetch_add(1, Ordering::AcqRel);
}

/// Chooses a folder's class (`Some("")`: by extension only), or forgets the choice (None).
pub fn set_folder(path: &str, class_id: Option<&str>) {
    let mut items: HashMap<String, (String, String)> = CHOICES.lock().unwrap().folders.clone();
    match class_id {
        None => {
            items.remove(&norm(path));
        }
        Some(cid) => {
            items.insert(norm(path), (path.to_owned(), cid.to_owned()));
        }
    }
    replace_folders(items.into_values());
}

pub fn user_file() -> PathBuf {
    crate::paths::config_dir().join("class_folders.json")
}

/// The choices of a `class_folders.json` text; None when it is not one (not JSON, no `folders`
/// list). An entry without a text `path` and `class` is left out on its own: one bad entry
/// costs that entry, not the others.
fn parse_folders(text: &str) -> Option<Vec<(String, String)>> {
    let v = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let items = v.get("folders")?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|f| {
                Some((
                    f.get("path")?.as_str()?.to_owned(),
                    f.get("class")?.as_str()?.to_owned(),
                ))
            })
            .collect(),
    )
}

/// The choices in a `class_folders.json` (none when it is missing or unreadable).
pub fn load(path: &Path) -> Vec<(String, String)> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                "{}",
                crate::tr!(
                    "文件夹归类 {path} 读不出来，已忽略：{error}",
                    path = path.display().to_string(),
                    error = e.to_string()
                )
            );
            return Vec::new();
        }
    };
    parse_folders(&text).unwrap_or_else(|| {
        tracing::warn!(
            "{}",
            crate::tr!(
                "文件夹归类 {path} 读不出来，已忽略：{error}",
                path = path.display().to_string(),
                error = "invalid"
            )
        );
        Vec::new()
    })
}

pub fn load_user() {
    replace_folders(load(&user_file()));
}

/// Writes the choices atomically; none removes the file. A file there that cannot be read (so
/// [`load`] found no choices in it) is moved aside first, never written over or removed.
pub fn save(path: &Path) -> std::io::Result<()> {
    crate::userfile::keep_if_unreadable(path, |text| parse_folders(text).is_some())?;
    let items = folders();
    if items.is_empty() {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => return Ok(()),
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let doc = serde_json::json!({
        "version": 1,
        "folders": items.iter().map(|(p, c)| serde_json::json!({"path": p, "class": c})).collect::<Vec<_>>(),
    });
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

// --------------------------------------------------------------------------- reading names

fn is_gone(nodes: &Nodes, i: NodeId) -> bool {
    nodes.flags(i).intersects(Flags::GONE)
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// A node's own name, case-folded; a scan root is stored under its whole path.
fn base(nodes: &Nodes, node: NodeId) -> String {
    let name = nodes.name(node);
    if nodes.parent(node) == 0 {
        let t = name.trim_end_matches(['\\', '/']);
        let last = t.rsplit(['\\', '/']).next().unwrap_or("");
        return lower(if last.is_empty() { &name } else { last });
    }
    lower(&name)
}

/// Nodes in `[a, b)` whose (ASCII-lower-cased) name the rules know, with that name; also names
/// starting with `$ntfs` right below a scan root (the MFT scan's folder, in the UI language).
fn names_among(
    nodes: &Nodes,
    a: NodeId,
    b: NodeId,
    roots: &HashSet<NodeId>,
) -> BTreeMap<NodeId, String> {
    let known = &*KNOWN;
    let mut out = BTreeMap::new();
    let mut buf = Vec::with_capacity(known.max_len);
    for i in a..b {
        let name = nodes.name_bytes(i);
        if name.is_empty() {
            continue;
        }
        if name.len() <= known.max_len && known.keys.contains(&key(name)) {
            buf.clear();
            buf.extend(name.iter().map(u8::to_ascii_lowercase));
            if known.names.contains(buf.as_slice()) {
                out.insert(i, String::from_utf8_lossy(&buf).into_owned());
                continue;
            }
        }
        if name[0] == b'$' && nodes.parent(i) >= 0 && roots.contains(&(nodes.parent(i) as NodeId)) {
            let low = lower(&nodes.name(i));
            if low.starts_with("$ntfs") {
                out.insert(i, low);
            }
        }
    }
    out
}

// --------------------------------------------------------------------------- recognising folders

#[derive(Clone, Debug)]
struct Rule {
    rule: u8,
    reason: String,
    /// a rule only while something inside says so (markers, engine files)
    evidence: bool,
}

#[derive(Default)]
struct Found {
    roots: BTreeMap<NodeId, Rule>,
    evidence: HashMap<NodeId, NodeId>,
    blocks: Vec<NodeId>,
}

impl Found {
    fn add(
        &mut self,
        nodes: &Nodes,
        folder: i64,
        rule: u8,
        reason: String,
        marker: Option<NodeId>,
    ) {
        let n = nodes.len() as i64;
        if !(0 < folder && folder < n) || nodes.kind(folder as NodeId) != Kind::Dir {
            return;
        }
        let folder = folder as NodeId;
        let replace = match self.roots.get(&folder) {
            None => true,
            Some(old) => is_fallback(old.rule) && !is_fallback(rule),
        };
        if replace {
            self.roots.insert(
                folder,
                Rule {
                    rule,
                    reason,
                    evidence: marker.is_some(),
                },
            );
        }
        if let Some(m) = marker {
            self.evidence.insert(m, folder);
        }
    }
}

fn root_set(nodes: &Nodes) -> HashSet<NodeId> {
    nodes.roots().collect()
}

/// Rule folders that rows `[a, b)` make: by their own names, or as markers in a folder.
fn detect(nodes: &Nodes, a: NodeId, b: NodeId) -> Found {
    let n = nodes.len() as i64;
    let roots = root_set(nodes);
    let mut found = Found::default();
    let names: Vec<(NodeId, String)> = names_among(nodes, a, b, &roots)
        .into_iter()
        .filter(|(i, _)| !is_gone(nodes, *i))
        .collect();
    let name_of = |i: i64| {
        if 0 < i && i < n {
            base(nodes, i as NodeId)
        } else {
            String::new()
        }
    };
    let par = |i: i64| {
        if 0 < i && i < n {
            nodes.parent(i as NodeId) as i64
        } else {
            0
        }
    };
    let at_top = |i: i64| {
        let p = par(i);
        p > 0 && roots.contains(&(p as NodeId)) && VOLUME.is_match(&nodes.name(p as NodeId))
    };
    let is_home = |i: i64| {
        if i > 0 && roots.contains(&(i as NodeId)) {
            return HOME.is_match(&nodes.name(i as NodeId));
        }
        let p = par(i);
        p > 0 && HOME_PARENTS.contains(&name_of(p).as_str())
    };
    for (i, nm) in names {
        let i64_ = i as i64;
        let p = par(i64_);
        let folder = nodes.kind(i) == Kind::Dir;
        let nm = nm.as_str();
        if PROJECT_ANY.contains(&nm) || (PROJECT_FILES.contains(&nm) && !folder) {
            let marker = CANON.iter().find(|c| c.0 == nm).map_or(nm, |c| c.1);
            found.add(
                nodes,
                p,
                PROJECT,
                crate::tr!("项目（{marker}）", marker = marker),
                Some(i),
            );
        } else if let Some((_, reason)) = GAME_FILES.iter().find(|g| g.0 == nm).filter(|_| !folder)
        {
            found.add(nodes, p, GAME, t(reason), Some(i));
        }
        if !folder {
            continue;
        }
        if NO_PROJECTS.contains(&nm) || (nm == "pkg" && name_of(p) == "go") {
            found.blocks.push(i);
        }
        if nm == STEAM {
            found.add(nodes, i64_, GAME, t("Steam 游戏库"), None);
        } else if nm == MINECRAFT {
            found.add(nodes, i64_, GAME, t("Minecraft"), None);
        } else if MODELS_ANYWHERE.contains(&nm)
            || (MODELS_IN_CACHE.contains(&nm) && name_of(p) == ".cache")
        {
            found.add(nodes, i64_, MODELS, t("模型缓存"), None);
        } else if (SAVES_IN_DOCUMENTS.contains(&nm) && name_of(p) == "documents")
            || (SAVES_IN_HOME.contains(&nm) && is_home(p))
        {
            found.add(nodes, i64_, GAME, t("游戏存档"), None);
        } else if SYSTEM_AT_TOP.contains(&nm) && at_top(i64_) {
            found.add(nodes, i64_, SYSTEM, t("系统卷信息"), None);
        } else if nm.starts_with("$ntfs") && p > 0 && roots.contains(&(p as NodeId)) {
            found.add(nodes, i64_, SYSTEM, t("NTFS 元数据"), None);
        } else if SOFTWARE_AT_TOP.contains(&nm) && at_top(i64_) {
            found.add(nodes, i64_, SOFTWARE, t("已安装的软件"), None);
            found.blocks.push(i);
        } else if SOFTWARE_IN_HOME.contains(&nm) && is_home(p) {
            found.add(nodes, i64_, SOFTWARE, t("应用数据"), None);
            found.blocks.push(i);
        }
        if nm == "paks" && name_of(p) == "content" {
            // Unreal: <game>/<project>/Content/Paks
            let proj = par(p);
            let top = par(proj);
            let engine = top > 0
                && nodes
                    .children(top as NodeId)
                    .any(|k| name_of(k as i64) == "engine");
            found.add(
                nodes,
                if engine { top } else { proj },
                GAME,
                t("Unreal 游戏"),
                Some(i),
            );
        }
        let launcher = LAUNCHERS.iter().find(|l| l.0 == nm);
        let holder =
            launcher.is_some() || (nm == "games" && LAUNCHER_HOMES.contains(&name_of(p).as_str()));
        if holder {
            // a launcher's folder of games: each child folder is a game
            let skip: &[&str] = launcher.map_or(&[], |l| l.1);
            let who = if nm == "games" {
                nodes.name(p as NodeId).into_owned()
            } else {
                nodes.name(i).into_owned()
            };
            let label = crate::tr!("{name} 的游戏", name = who);
            let kids: Vec<NodeId> = nodes.children(i).collect();
            for k in kids {
                if nodes.kind(k) == Kind::Dir
                    && !is_gone(nodes, k)
                    && !skip.contains(&name_of(k as i64).as_str())
                {
                    found.add(nodes, k as i64, GAME, label.clone(), None);
                }
            }
        }
    }
    let (exe, other) = engines(nodes, a, b, &mut found);
    let mut both: Vec<NodeId> = exe.intersection(&other).copied().collect();
    both.sort_unstable();
    for folder in both {
        engine_folder(nodes, folder, &mut found);
    }
    found
}

/// Project files by extension (`*.sln` …) among rows `[a, b)`; and the folders that may hold an
/// engine build: those with an `.exe` and those with a `<name>_Data` folder or a `.pck`.
fn engines(
    nodes: &Nodes,
    a: NodeId,
    b: NodeId,
    found: &mut Found,
) -> (HashSet<NodeId>, HashSet<NodeId>) {
    let table = nodes.ext_table();
    let project: HashSet<u16> = table
        .iter()
        .enumerate()
        .filter(|(_, e)| PROJECT_EXTS.contains(&e.as_ref()))
        .map(|(i, _)| i as u16)
        .collect();
    let exe_id = table
        .iter()
        .position(|e| e.as_ref() == "exe")
        .map(|i| i as u16);
    let pck_id = table
        .iter()
        .position(|e| e.as_ref() == "pck")
        .map(|i| i as u16);
    let mut exe = HashSet::new();
    let mut other = HashSet::new();
    let exts = nodes.exts();
    for i in a..b {
        if is_gone(nodes, i) {
            continue;
        }
        let e = exts[i as usize];
        let p = nodes.parent(i);
        if p < 0 {
            continue;
        }
        if e != 0 && project.contains(&e) {
            found.add(
                nodes,
                p as i64,
                PROJECT,
                crate::tr!("项目（{marker}）", marker = nodes.name(i).into_owned()),
                Some(i),
            );
        }
        if Some(e) == exe_id && e != 0 {
            exe.insert(p as NodeId);
        }
        if Some(e) == pck_id && e != 0 {
            other.insert(p as NodeId);
        }
        if nodes.kind(i) == Kind::Dir && nodes.name_len(i) > 5 {
            let nm = nodes.name_bytes(i);
            if nm[nm.len() - 5..].eq_ignore_ascii_case(b"_data") {
                other.insert(p as NodeId);
            }
        }
    }
    (exe, other)
}

fn engine_folder(nodes: &Nodes, folder: NodeId, found: &mut Found) {
    let mut exes = HashSet::new();
    let mut data: Vec<(String, NodeId)> = Vec::new();
    let mut pck = None;
    for k in nodes.children(folder) {
        if is_gone(nodes, k) {
            continue;
        }
        let low = lower(&nodes.name(k));
        if nodes.kind(k) == Kind::Dir {
            if let Some(stem) = low.strip_suffix("_data") {
                data.push((stem.to_owned(), k));
            }
        } else if let Some(stem) = low.strip_suffix(".exe") {
            exes.insert(stem.to_owned());
        } else if low.ends_with(".pck") {
            pck = Some(k);
        }
    }
    if let Some((_, d)) = data.iter().find(|(stem, _)| exes.contains(stem)) {
        found.add(nodes, folder as i64, GAME, t("Unity 游戏"), Some(*d));
    } else if let Some(p) = pck.filter(|_| !exes.is_empty()) {
        found.add(nodes, folder as i64, GAME, t("Godot 游戏"), Some(p));
    }
}

/// A project may start at `folder` as far as the folder itself goes: not a volume, not a home or
/// the folder holding homes, not a standard user folder right in a home.
fn project_guard(nodes: &Nodes, folder: NodeId, roots: &HashSet<NodeId>) -> bool {
    if roots.contains(&folder) {
        let path = nodes.name(folder);
        return !VOLUME.is_match(&path) && !HOME.is_match(&path);
    }
    let p = nodes.parent(folder);
    let own = base(nodes, folder);
    let pname = if p > 0 {
        base(nodes, p as NodeId)
    } else {
        String::new()
    };
    if HOME_PARENTS.contains(&pname.as_str()) || HOME_PARENTS.contains(&own.as_str()) {
        return false;
    }
    if p > 0 && roots.contains(&(p as NodeId)) {
        let joined = format!(
            "{}/{}",
            nodes.name(p as NodeId).trim_end_matches(['\\', '/']),
            nodes.name(folder)
        );
        if HOME.is_match(&joined) {
            return false;
        }
    }
    let home = (p > 0 && roots.contains(&(p as NodeId)) && HOME.is_match(&nodes.name(p as NodeId)))
        || (p > 0
            && nodes.parent(p as NodeId) > 0
            && HOME_PARENTS.contains(&base(nodes, nodes.parent(p as NodeId) as NodeId).as_str()));
    !(home && (USER_FOLDERS.contains(&own.as_str()) || own.starts_with("onedrive")))
}

// --------------------------------------------------------------------------- the column

/// Rule -> column value under `table` (a rule whose class the table lacks is off).
fn encode(table: &ClassTable) -> HashMap<u8, u8> {
    let mut out = HashMap::new();
    for rule in [GAME, PROJECT, MODELS, SYSTEM, SOFTWARE] {
        let cid = rule_class(rule);
        if cid == OTHER {
            continue;
        }
        if let Some(idx) = table.index_of(cid).filter(|&i| i < SWALLOW_MAX) {
            out.insert(
                rule,
                if is_fallback(rule) {
                    FALLBACK_BASE + idx as u8
                } else {
                    idx as u8 + 1
                },
            );
        }
    }
    out
}

/// Column value of a user's choice: swallowed by `cid`; 0 for "by extension only".
fn value_of(table: &ClassTable, cid: &str) -> u8 {
    if cid.is_empty() || cid == OTHER {
        return 0;
    }
    match table.index_of(cid) {
        Some(i) if i < SWALLOW_MAX => i as u8 + 1,
        _ => 0,
    }
}

/// Folders grouped by depth, shallow first.
fn levels(nodes: &Nodes, dirs: &[NodeId]) -> Vec<Vec<NodeId>> {
    let mut order: Vec<NodeId> = dirs.to_vec();
    order.sort_by_key(|&d| nodes.depth(d));
    let mut out: Vec<Vec<NodeId>> = Vec::new();
    let mut last = None;
    for d in order {
        let depth = nodes.depth(d);
        if last != Some(depth) {
            out.push(Vec::new());
            last = Some(depth);
        }
        out.last_mut().unwrap().push(d);
    }
    out
}

#[derive(Default)]
struct Entry {
    key: Option<(usize, u64)>,
    version: u64,
    n: usize,
    col: Arc<Vec<u8>>,
    own: Vec<u8>,
    blocked: Vec<bool>,
    blocks: Vec<bool>,
    roots: HashMap<NodeId, Rule>,
    evidence: HashMap<NodeId, NodeId>,
    user: HashMap<NodeId, String>,
}

fn parent_of(nodes: &Nodes, i: NodeId) -> usize {
    nodes.parent(i).max(0) as usize
}

impl Entry {
    fn grow(&mut self, n: usize) {
        if self.own.len() >= n {
            return;
        }
        let cap = n.max(self.own.len() * 5 / 4 + 4096);
        Arc::make_mut(&mut self.col).resize(cap, 0);
        self.own.resize(cap, 0);
        self.blocked.resize(cap, false);
        self.blocks.resize(cap, false);
    }

    /// Folders `dirs`, outer before inner: an inherited swallow wins, then the folder's own rule,
    /// then an inherited fallback; projects stay blocked below a blocking folder.
    fn propagate(&mut self, nodes: &Nodes, dirs: &[NodeId]) {
        let col = Arc::make_mut(&mut self.col);
        for level in levels(nodes, dirs) {
            for d in level {
                let (i, up) = (d as usize, parent_of(nodes, d));
                let inh = col[up];
                let mine = self.own[i];
                col[i] = if inh > 0 && inh < FALLBACK_BASE {
                    inh
                } else if mine > 0 {
                    mine
                } else {
                    inh
                };
                self.blocked[i] = self.blocked[up] || self.blocks[i];
            }
        }
    }

    /// Rows other than folders take their folder's value.
    fn fill(&mut self, nodes: &Nodes, ids: impl Iterator<Item = NodeId>) {
        let col = Arc::make_mut(&mut self.col);
        for i in ids {
            if nodes.kind(i) != Kind::Dir {
                let p = nodes.parent(i);
                col[i as usize] = if p >= 0 { col[p as usize] } else { 0 };
            }
        }
    }

    /// Keep what was found; projects only where allowed. Returns the folders that changed.
    fn take(&mut self, nodes: &Nodes, found: Found, table: &ClassTable) -> Vec<NodeId> {
        let codes = encode(table);
        let roots = root_set(nodes);
        for &f in &found.blocks {
            self.blocks[f as usize] = true;
            self.blocked[f as usize] = true;
        }
        let mut changed = Vec::new();
        for (folder, r) in found.roots {
            if r.rule == PROJECT
                && (self.blocked_above(nodes, folder) || !project_guard(nodes, folder, &roots))
            {
                continue;
            }
            let value = codes.get(&r.rule).copied().unwrap_or(0);
            let old_swallows = self
                .roots
                .get(&folder)
                .is_some_and(|o| !is_fallback(o.rule));
            if value == 0 || (old_swallows && is_fallback(r.rule)) {
                continue;
            }
            self.roots.insert(folder, r);
            if self.own[folder as usize] != value {
                self.own[folder as usize] = value;
                changed.push(folder);
            }
        }
        for (marker, folder) in found.evidence {
            if self.roots.contains_key(&folder) {
                self.evidence.insert(marker, folder);
            }
        }
        changed
    }

    fn blocked_above(&self, nodes: &Nodes, folder: NodeId) -> bool {
        if self.blocked[folder as usize] {
            return true;
        }
        let mut p = nodes.parent(folder);
        let mut steps = 0;
        while p > 0 && steps <= nodes.len() {
            if self.blocks[p as usize] {
                return true;
            }
            p = nodes.parent(p as NodeId);
            steps += 1;
        }
        false
    }

    fn build(&mut self, nodes: &Nodes, table: &ClassTable, n: usize, version: u64) {
        self.col = Arc::new(vec![0; n]);
        self.own = vec![0; n];
        self.blocked = vec![false; n];
        self.blocks = vec![false; n];
        self.roots.clear();
        self.evidence.clear();
        let found = detect(nodes, 1, n as NodeId);
        let dirs: Vec<NodeId> = (1..n as NodeId)
            .filter(|&i| nodes.kind(i) == Kind::Dir && !is_gone(nodes, i))
            .collect();
        // blocking first (projects are kept only where nothing above blocks them), then the rules
        for &f in &found.blocks {
            self.blocks[f as usize] = true;
        }
        for level in levels(nodes, &dirs) {
            for d in level {
                self.blocked[d as usize] =
                    self.blocked[parent_of(nodes, d)] || self.blocks[d as usize];
            }
        }
        self.take(nodes, found, table);
        self.propagate(nodes, &dirs);
        self.fill(nodes, 1..n as NodeId);
        self.apply_user(nodes, table, n);
        self.n = n;
        self.version = version;
    }

    fn update(&mut self, nodes: &Nodes, table: &ClassTable, n: usize, version: u64) {
        self.grow(n);
        let mut dirty: HashSet<NodeId> = HashSet::new();
        let gone_roots: Vec<NodeId> = self
            .roots
            .keys()
            .copied()
            .filter(|&f| is_gone(nodes, f))
            .collect();
        for f in gone_roots {
            self.roots.remove(&f); // relisted: found again below
            self.own[f as usize] = 0;
        }
        let mut alive: HashMap<NodeId, usize> = HashMap::new();
        let codes = encode(table);
        let mut markers: Vec<(NodeId, NodeId)> =
            self.evidence.iter().map(|(&m, &f)| (m, f)).collect();
        markers.sort_unstable();
        for (marker, folder) in markers {
            if is_gone(nodes, marker) {
                self.evidence.remove(&marker);
                continue;
            }
            let now = nodes.parent(marker);
            if now >= 0 && now as NodeId != folder {
                // the folder's row moved (relisted): the rule moves with it
                let now = now as NodeId;
                self.evidence.insert(marker, now);
                if let Some(rule) = self.roots.remove(&folder)
                    && now > 0
                {
                    self.own[now as usize] = codes.get(&rule.rule).copied().unwrap_or(0);
                    self.roots.insert(now, rule);
                    if (now as usize) < self.n {
                        dirty.insert(now);
                    }
                }
            }
            *alive.entry(self.evidence[&marker]).or_default() += 1;
        }
        let lost: Vec<NodeId> = self
            .roots
            .iter()
            .filter(|(f, r)| r.evidence && !alive.contains_key(f))
            .map(|(&f, _)| f)
            .collect();
        for f in lost {
            self.roots.remove(&f);
            self.own[f as usize] = 0;
            dirty.insert(f);
        }
        let old_n = self.n;
        if n > old_n {
            let found = detect(nodes, old_n as NodeId, n as NodeId);
            for &f in &found.blocks {
                self.blocks[f as usize] = true;
            }
            let changed = self.take(nodes, found, table);
            dirty.extend(changed.into_iter().filter(|&f| (f as usize) < old_n));
            let new_dirs: Vec<NodeId> = (old_n as NodeId..n as NodeId)
                .filter(|&i| nodes.kind(i) == Kind::Dir && !is_gone(nodes, i))
                .collect();
            self.propagate(nodes, &new_dirs);
            self.fill(nodes, old_n as NodeId..n as NodeId);
        }
        for folder in outermost(nodes, &dirty) {
            if is_gone(nodes, folder) {
                continue;
            }
            let below = below(nodes, folder, n);
            let mut dirs = vec![folder];
            dirs.extend(
                below
                    .iter()
                    .copied()
                    .filter(|&i| nodes.kind(i) == Kind::Dir),
            );
            self.propagate(nodes, &dirs);
            self.fill(nodes, below.into_iter());
        }
        if !self.user.is_empty() || (n > old_n && !CHOICES.lock().unwrap().folders.is_empty()) {
            self.apply_user(nodes, table, n);
        }
        self.n = n;
        self.version = version;
    }

    fn apply_user(&mut self, nodes: &Nodes, table: &ClassTable, n: usize) {
        self.user.clear();
        let chosen = folders();
        if chosen.is_empty() {
            return;
        }
        let mut index = PathIndex::new();
        let mut located = Vec::new();
        for (path, cid) in chosen {
            if let (Some(node), _) = index.locate(nodes, path.as_bytes())
                && (node as usize) < n
                && nodes.kind(node) == Kind::Dir
            {
                located.push((nodes.depth(node), node, cid));
            }
        }
        located.sort();
        let col = Arc::make_mut(&mut self.col);
        for (_, node, cid) in located {
            let value = value_of(table, &cid);
            col[node as usize] = value;
            for b in below(nodes, node, n) {
                col[b as usize] = value;
            }
            self.user.insert(node, cid);
        }
    }
}

/// Subtree ids of rows below `n` (a scan may have folded more since).
fn below(nodes: &Nodes, folder: NodeId, n: usize) -> Vec<NodeId> {
    let mut ids = subtree_ids(nodes, folder).unwrap_or_default();
    ids.retain(|&i| (i as usize) < n && i != folder);
    ids
}

/// The folders of a set with no ancestor in it.
fn outermost(nodes: &Nodes, set: &HashSet<NodeId>) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = set
        .iter()
        .copied()
        .filter(|&f| {
            let mut p = nodes.parent(f);
            let mut steps = 0;
            while p > 0 && !set.contains(&(p as NodeId)) && steps <= nodes.len() {
                p = nodes.parent(p as NodeId);
                steps += 1;
            }
            p <= 0
        })
        .collect();
    out.sort_unstable();
    out
}

/// Why a node takes a folder class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason {
    pub class_id: String,
    pub reason: String,
    /// the folder that decided
    pub folder: NodeId,
    /// the user's choice
    pub user: bool,
    /// only files whose extension is in no class take it
    pub fallback: bool,
}

impl Reason {
    pub fn to_protocol(&self, nodes: &Nodes) -> sk_protocol::scan::FolderClassInfo {
        sk_protocol::scan::FolderClassInfo {
            class_id: self.class_id.clone(),
            reason: self.reason.clone(),
            folder: nodes.path_lossy(self.folder),
            user: self.user,
            fallback: self.fallback,
        }
    }
}

/// The folder-class column of one store, kept up to date as the store grows and changes. Keep one
/// beside each store (the Python module kept them in a weak dictionary per store); every call
/// takes the store's current view (`&Nodes` under a reader guard).
#[derive(Default)]
pub struct FolderClasses {
    entry: Mutex<Entry>,
}

impl FolderClasses {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry<'a>(
        &'a self,
        nodes: &Nodes,
        table: &Arc<ClassTable>,
    ) -> std::sync::MutexGuard<'a, Entry> {
        let mut e = self.entry.lock().unwrap_or_else(|p| p.into_inner());
        let key = (Arc::as_ptr(table) as usize, folders_version());
        let version = nodes.version();
        let n = nodes.len();
        if e.key != Some(key) || n < e.n {
            e.build(nodes, table, n, version);
            e.key = Some(key);
        } else if e.version != version || n > e.n {
            e.update(nodes, table, n, version);
        }
        e
    }

    /// One `u8` per node of the view (see the module docs): the first `nodes.len()` values. The
    /// vector is shared (copy-on-write): holding it never blocks the next update.
    pub fn column(&self, nodes: &Nodes, table: &Arc<ClassTable>) -> Arc<Vec<u8>> {
        let e = self.entry(nodes, table);
        e.col.clone()
    }

    /// Why `node` (a folder, or a file's folder) takes a folder class, if it does.
    pub fn explain(&self, nodes: &Nodes, node: NodeId, table: &Arc<ClassTable>) -> Option<Reason> {
        let e = self.entry(nodes, table);
        let node = if nodes.kind(node) != Kind::Dir {
            nodes.parent(node).max(0) as NodeId
        } else {
            node
        };
        let mut chain = Vec::new();
        let mut p = node as i64;
        while p > 0 && chain.len() <= nodes.len() {
            chain.push(p as NodeId);
            p = nodes.parent(p as NodeId) as i64;
        }
        for &f in &chain {
            // the deepest choice of the user wins
            if let Some(cid) = e.user.get(&f) {
                return (!cid.is_empty() && table.index_of(cid).is_some()).then(|| Reason {
                    class_id: cid.clone(),
                    reason: t("你的设置"),
                    folder: f,
                    user: true,
                    fallback: false,
                });
            }
        }
        let mut pick: Option<(NodeId, &Rule)> = None;
        for &f in chain.iter().rev() {
            // the outermost swallowing rule; else the nearest fallback
            if let Some(r) = e.roots.get(&f) {
                pick = Some((f, r));
                if !is_fallback(r.rule) {
                    break;
                }
            }
        }
        let (folder, r) = pick?;
        let cid = rule_class(r.rule);
        table.index_of(cid)?;
        Some(Reason {
            class_id: cid.to_owned(),
            reason: r.reason.clone(),
            folder,
            user: false,
            fallback: is_fallback(r.rule),
        })
    }
}

/// Class index of a row: `base` (by extension) under the column's folder classes.
#[inline]
pub fn resolve(col: u8, base: u8, other: u8) -> u8 {
    if col > 0 && col < FALLBACK_BASE {
        col - 1
    } else if col >= FALLBACK_BASE && base == other {
        col - FALLBACK_BASE
    } else {
        base
    }
}

/// [`resolve`] over slices (`out[i]` for `col[i]`, `base[i]`).
pub fn resolve_all(col: &[u8], base: &[u8], other: u8, out: &mut [u8]) {
    for ((o, &c), &b) in out.iter_mut().zip(col).zip(base) {
        *o = resolve(c, b, other);
    }
}
