"""Folder classes: a recognised folder decides the class of the files in it (UI-W7-38).

The extension says what a file *is*; some folders say more. Everything inside a game install is the
game's, whatever its extension, and everything in a project is the project's. The built-in rules
(any platform's, so a snapshot from another system reads the same):

- **game** (class ``game``): Steam libraries (``steamapps``); the game folders of launchers (Epic,
  GOG, Xbox, EA, Ubisoft, Riot, HoYoPlay / miHoYo, Hypergryph); builds of game engines (Unity:
  ``UnityPlayer.dll``, or ``<name>.exe`` beside ``<name>_Data``; Unreal: ``<game>/Content/Paks``;
  Godot: a ``.pck`` beside an ``.exe``; Blizzard: ``.build.info``); games' folders in a home
  (``.minecraft``, ``Documents/My Games``, ``Documents/Paradox Interactive``, ``Saved Games``);
- **project** (class ``code``): a folder holding a project marker (``.git``, ``package.json``,
  ``Cargo.toml``, ``*.sln`` …), never inside software, caches or package stores, and never a volume,
  a home or a standard user folder;
- **models** (class ``data``): ``.ollama``, ``.lmstudio``, ``.cache/huggingface`` and the like;
- **system** (class ``exec``): ``System Volume Information``, the MFT scan's ``$NTFS`` folder;
- **software**, a fallback (class ``exec``): ``Program Files``, ``ProgramData``, ``Windows`` at the
  top of a volume, ``/usr``, ``/opt``, ``/Applications``; a home's ``AppData``, ``Library``,
  ``.local``, ``.config``.

A rule *swallows* its folder (every file in it takes the class), except a fallback: there only files
whose extension is in no class take it. An outer folder wins over the folders inside it (a game's
own ``package.json`` stays the game's), a swallowing rule over a fallback. A rule whose class is not
in the user's table is off. The user's choices (``class_folders.json``, :func:`set_folder`) win over
the rules, the deepest one inside its own folder; ``""`` means "by extension only".

The result is one ``uint8`` per node (:func:`column`): 0 nothing, 1–127 the swallowing class's
index + 1, 128–255 the fallback class's index + 128. It is computed in one pass over the folders by
depth and kept per store, with two more bytes per node: each folder's own rule value and whether
projects are blocked there. Rows that a scan or a change adds take their parent's value; only
folders that gain or lose what made them a rule's folder are worked out again (:meth:`_Entry.update`).
Names are found without decoding every one: a length / first byte / last byte key read from the
name heap picks the few candidates to decode.
"""

from __future__ import annotations

import json
import os
import re
import threading
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from pathlib import Path, PurePath
from weakref import WeakKeyDictionary

import numpy as np
from loguru import logger

from spaceknife_protocol.classes import OTHER

from .classes import ClassTable
from .i18n import Texts
from .store.lookup import PathIndex
from .store.nodes import Flag, Kind, NodeStore
from .store.subtree import subtree_ids

GAME, PROJECT, MODELS, SYSTEM, SOFTWARE = 1, 2, 3, 4, 5
RULE_CLASS = {GAME: "game", PROJECT: "code", MODELS: "data", SYSTEM: "exec", SOFTWARE: "exec"}
FALLBACK = frozenset({SOFTWARE})  # only files whose extension is in no class
SWALLOW_MAX = 127  # column values 1..127 swallow, 128..255 fall back
FALLBACK_BASE = 128

TEXT = Texts(
    {
        "steam": "Steam 游戏库",
        "launcher": "{name} 的游戏",
        "unity": "Unity 游戏",
        "unreal": "Unreal 游戏",
        "godot": "Godot 游戏",
        "blizzard": "暴雪游戏",
        "minecraft": "Minecraft",
        "saves": "游戏存档",
        "project": "项目（{marker}）",
        "models": "模型缓存",
        "system": "系统卷信息",
        "mft": "NTFS 元数据",
        "software": "已安装的软件",
        "appdata": "应用数据",
        "user": "你的设置",
        "bad_file": "文件夹归类 {path} 读不出来，已忽略：{error}",
    }
)

# --------------------------------------------------------------------------- the names the rules know

_PROJECT_FILES = frozenset(
    {
        "package.json", "cargo.toml", "pyproject.toml", "setup.py", "go.mod", "cmakelists.txt",
        "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "pom.xml",
        "composer.json", "gemfile", "pubspec.yaml", "package.swift", "mix.exs", "deno.json", "meson.build",
        "project.godot",
    }
)  # fmt: skip
_PROJECT_ANY = frozenset({".git", ".hg", ".svn"})  # a folder, or a file (git worktrees, submodules)
_PROJECT_EXTS = frozenset(
    {"sln", "csproj", "vcxproj", "fsproj", "vbproj", "uproject", "xcodeproj", "xcworkspace"}
)
_GAME_FILES = {"unityplayer.dll": "unity", ".build.info": "blizzard"}
# how markers are usually spelt (the reason a folder is a project)
_CANON = {
    "cargo.toml": "Cargo.toml", "cmakelists.txt": "CMakeLists.txt", "gemfile": "Gemfile",
    "package.swift": "Package.swift",
}  # fmt: skip
# launcher folders whose child folders are games, and the children that are the launcher itself
_LAUNCHERS = {
    "epic games": frozenset({"launcher", "directxredist", "epiconlineservices"}),
    "gog games": frozenset(),
    "xboxgames": frozenset({"gamesave"}),
    "ea games": frozenset(),
    "riot games": frozenset({"riot client"}),
}
_LAUNCHER_HOMES = frozenset(
    {"gog galaxy", "ubisoft game launcher", "hoyoplay", "mihoyo launcher", "hypergryph launcher"}
)
_STEAM = "steamapps"
_MINECRAFT = ".minecraft"
_SAVES_IN_DOCUMENTS = frozenset({"my games", "paradox interactive"})
_SAVES_IN_HOME = frozenset({"saved games"})
_MODELS_ANYWHERE = frozenset({".ollama", ".lmstudio"})
_MODELS_IN_CACHE = frozenset({"huggingface", "lm-studio", "torch"})
_SYSTEM_AT_TOP = frozenset({"system volume information"})
_SOFTWARE_AT_TOP = frozenset(
    {
        "program files",
        "program files (x86)",
        "programdata",
        "windows",
        "applications",
        "usr",
        "opt",
        "library",
    }
    | {"system"}
)
_SOFTWARE_IN_HOME = {"appdata": "appdata", "library": "appdata", ".local": "appdata", ".config": "appdata"}
# no project starts inside these (software, caches, package stores, the bin)
_NO_PROJECTS = frozenset(
    {
        "node_modules", "site-packages", "dist-packages", ".cargo", ".rustup", ".npm", ".gradle", ".m2",
        ".nuget", ".vscode", ".vscode-server", ".cursor", ".cache", ".conda", "anaconda3", "miniconda3",
        ".pyenv", "$recycle.bin", ".trash", "windowsapps", "steamapps",
    }
)  # fmt: skip
_USER_FOLDERS = frozenset(
    {
        "desktop", "documents", "downloads", "pictures", "videos", "music", "movies", "public", "dropbox",
        "icloud drive", "saved games", "favorites", "links", "contacts", "searches", "3d objects",
    }
)  # fmt: skip
_HOME_PARENTS = frozenset({"users", "home"})
_HELPERS = frozenset({"games", "content", "paks", "engine", ".cache", "go", "pkg"})

_NAMES: frozenset[str] = frozenset(
    {
        *_PROJECT_FILES, *_PROJECT_ANY, *_GAME_FILES, *_LAUNCHERS, *_LAUNCHER_HOMES, _STEAM, _MINECRAFT,
        *_SAVES_IN_DOCUMENTS, *_SAVES_IN_HOME, *_MODELS_ANYWHERE, *_MODELS_IN_CACHE, *_SYSTEM_AT_TOP,
        *_SOFTWARE_AT_TOP, *_SOFTWARE_IN_HOME, *_NO_PROJECTS, *_USER_FOLDERS, *_HOME_PARENTS, *_HELPERS,
        *(c for kids in _LAUNCHERS.values() for c in kids),
    }
)  # fmt: skip
_LOWER = np.arange(256, dtype=np.uint8)
_LOWER[65:91] += 32


def _key(name: bytes) -> int:
    return (len(name) << 16) | (int(_LOWER[name[0]]) << 8) | int(_LOWER[name[-1]])


_KEYS = np.array(sorted({_key(nm.encode()) for nm in _NAMES}), dtype=np.int64)
_MAX_LEN = max(len(nm.encode()) for nm in _NAMES)
# the names by byte length, as fixed-width byte strings for one vectorised comparison per length
_BY_LENGTH: dict[int, np.ndarray] = {}
for _nm in _NAMES:
    _b = _nm.encode()
    _BY_LENGTH.setdefault(len(_b), np.zeros(0, dtype=f"S{len(_b)}"))
    _BY_LENGTH[len(_b)] = np.append(_BY_LENGTH[len(_b)], np.array([_b], dtype=f"S{len(_b)}"))
_SPAN = {k: np.arange(k, dtype=np.int64) for k in _BY_LENGTH}
_DOLLAR = ord("$")
CHUNK = 1 << 20  # rows looked at in one go (bounds the temporary arrays)
_DATA_SUFFIX = np.frombuffer(b"_data", dtype=np.uint8)

_VOLUME = re.compile(r"^(?:[a-z]:[\\/]?|/|[\\/]{2}[^\\/]+[\\/][^\\/]+[\\/]?)$", re.IGNORECASE)
_HOME = re.compile(r"(?:^|[\\/])(?:users|home)[\\/][^\\/]+[\\/]?$", re.IGNORECASE)

# --------------------------------------------------------------------------- the user's choices

_lock = threading.Lock()
_folders: dict[str, tuple[str, str]] = {}  # normalised path -> (path as given, class id; "" = by extension)
_version = 0


def user_file() -> Path:
    from .paths import config_dir

    return config_dir() / "class_folders.json"


def _norm(path: str) -> str:
    return os.path.normcase(os.path.normpath(path))


def folders() -> list[tuple[str, str]]:
    """The user's choices, ``(path, class id)``, sorted by path."""
    with _lock:
        return sorted(_folders.values())


def folders_version() -> int:
    return _version


def _replace(items: Iterable[tuple[str, str]]) -> None:
    global _version
    with _lock:
        _folders.clear()
        for path, cid in items:
            _folders[_norm(path)] = (path, cid)
        _version += 1


def load(path: Path) -> list[tuple[str, str]]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
        return [(str(f["path"]), str(f["class"])) for f in raw["folders"]]
    except FileNotFoundError:
        return []
    except (OSError, ValueError, KeyError, TypeError) as exc:
        logger.warning(TEXT["bad_file"].format(path=path, error=exc))
        return []


def load_user() -> None:
    _replace(load(user_file()))


def save(path: Path) -> None:
    """Write the choices atomically; none removes the file."""
    items = folders()
    if not items:
        path.unlink(missing_ok=True)
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    doc = {"version": 1, "folders": [{"path": p, "class": c} for p, c in items]}
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(doc, ensure_ascii=False, indent=1), encoding="utf-8")
    tmp.replace(path)


def set_folder(path: str, class_id: str | None) -> None:
    """Choose a folder's class (``""``: by extension only), or forget the choice (None)."""
    with _lock:
        items = dict(_folders)
    if class_id is None:
        items.pop(_norm(path), None)
    else:
        items[_norm(path)] = (path, class_id)
    _replace(items.values())


def replace_folders(items: Iterable[tuple[str, str]]) -> None:
    _replace(items)


# --------------------------------------------------------------------------- reading names


def _names_among(store: NodeStore, a: int, b: int, roots: set[int]) -> dict[int, str]:
    """Nodes in ``[a, b)`` whose case-folded name the rules know, with that name; also names
    starting with ``$`` right below a scan root (the MFT scan's folders follow the UI language)."""
    c = store.cols
    lengths, parent = c["name_len"], c["parent"]
    out: dict[int, str] = {}
    root_ids = np.fromiter(roots, dtype=np.int64, count=len(roots))
    lo = a
    while lo < b:
        stop, run, sep = store.names_run(lo, min(b, lo + CHUNK))
        if stop <= lo:
            break
        ln = lengths[lo:stop].astype(np.int64)
        s = sep.astype(np.int64)
        ok = (ln > 0) & (ln <= _MAX_LEN)
        first = np.zeros(len(ln), dtype=np.int64)
        last = np.zeros(len(ln), dtype=np.int64)
        first[ok] = _LOWER[run[s[ok] + 1]]
        last[ok] = _LOWER[run[s[ok] + ln[ok]]]
        cand = np.flatnonzero(ok & np.isin((ln << 16) | (first << 8) | last, _KEYS))
        for k in np.unique(ln[cand]).tolist():  # exact names, compared in bulk per length
            sel = cand[ln[cand] == k]
            grid = _LOWER[run[(s[sel] + 1)[:, None] + _SPAN[k]]]
            names = np.ascontiguousarray(grid).view(f"S{k}").ravel()
            hit = np.isin(names, _BY_LENGTH[k])
            for i, name in zip(sel[hit].tolist(), names[hit].tolist(), strict=True):
                out[lo + i] = name.decode()
        for i in np.flatnonzero((first == _DOLLAR) & np.isin(parent[lo:stop], root_ids)).tolist():
            name = store.name(lo + i).casefold()  # the MFT scan's folders, in the UI language
            if name.startswith("$ntfs"):
                out[lo + i] = name
        lo = stop
    return out


def _ending_with(store: NodeStore, ids: np.ndarray, suffix: np.ndarray) -> np.ndarray:
    """Those of ``ids`` (sorted) whose name ends with ``suffix`` (ASCII, lower case), any case."""
    if not len(ids):
        return ids
    c = store.cols
    keep: list[np.ndarray] = []
    k = len(suffix)
    lo, hi = int(ids[0]), int(ids[-1]) + 1
    while lo < hi:
        stop, run, sep = store.names_run(lo, hi)
        if stop <= lo:
            break
        sel = ids[(ids >= lo) & (ids < stop)]
        ln = c["name_len"][sel].astype(np.int64)
        sel, ln = sel[ln >= k], ln[ln >= k]
        if len(sel):
            end = sep.astype(np.int64)[sel - lo] + ln  # the last byte of each name
            match = np.ones(len(sel), dtype=bool)
            for j in range(k):
                match &= _LOWER[run[end - (k - 1 - j)]] == suffix[j]
            keep.append(sel[match])
        lo = stop
    return np.concatenate(keep) if keep else np.zeros(0, dtype=np.int64)


def _base(store: NodeStore, node: int) -> str:
    """A node's own name, case-folded; a scan root is stored under its whole path."""
    name = store.name(node)
    if int(store.cols["parent"][node]) == 0:
        name = PurePath(name.replace("\\", "/")).name or name
    return name.casefold()


# --------------------------------------------------------------------------- recognising folders


@dataclass(slots=True)
class _Rule:
    rule: int
    reason: str
    evidence: bool  # a rule only while something inside says so (markers, engine files)


@dataclass(slots=True)
class _Found:
    roots: dict[int, _Rule] = field(default_factory=dict)  # folder -> its rule
    evidence: dict[int, int] = field(default_factory=dict)  # marker node -> the folder it makes
    blocks: list[int] = field(default_factory=list)  # folders below which no project starts


def _detect(store: NodeStore, a: int, b: int) -> _Found:
    """Rule folders that rows ``[a, b)`` make: by their own names, or as markers in a folder."""
    n = len(store)
    c = store.cols
    kind, parent, flags = c["kind"], c["parent"], c["flags"]
    gone = np.uint32(Flag.GONE)
    roots = {int(r) for r in store.roots()}
    found = _Found()
    names = {i: nm for i, nm in _names_among(store, a, b, roots).items() if not flags[i] & gone}

    def name_of(i: int) -> str:
        return _base(store, i) if 0 < i < n else ""

    def par(i: int) -> int:
        return int(parent[i]) if 0 < i < n else 0

    def at_top(i: int) -> bool:
        p = par(i)
        return p in roots and bool(_VOLUME.match(store.name(p)))

    def is_home(i: int) -> bool:
        if i in roots:
            return bool(_HOME.search(store.name(i)))
        p = par(i)
        return p > 0 and name_of(p) in _HOME_PARENTS

    def add(folder: int, rule: int, reason: str, marker: int | None = None) -> None:
        if not 0 < folder < n or kind[folder] != Kind.DIR:
            return
        old = found.roots.get(folder)
        if old is None or (old.rule in FALLBACK and rule not in FALLBACK):
            found.roots[folder] = _Rule(rule, reason, marker is not None)
        if marker is not None:
            found.evidence[marker] = folder

    project = TEXT["project"]
    for i, nm in names.items():
        p = par(i)
        folder = kind[i] == Kind.DIR
        if nm in _PROJECT_ANY or (nm in _PROJECT_FILES and not folder):
            add(p, PROJECT, project.format(marker=_CANON.get(nm, nm)), i)
        elif nm in _GAME_FILES and not folder:
            add(p, GAME, TEXT[_GAME_FILES[nm]], i)
        if not folder:
            continue
        if nm in _NO_PROJECTS or (nm == "pkg" and name_of(p) == "go"):
            found.blocks.append(i)
        if nm == _STEAM:
            add(i, GAME, TEXT["steam"])
        elif nm == _MINECRAFT:
            add(i, GAME, TEXT["minecraft"])
        elif nm in _MODELS_ANYWHERE or (nm in _MODELS_IN_CACHE and name_of(p) == ".cache"):
            add(i, MODELS, TEXT["models"])
        elif (nm in _SAVES_IN_DOCUMENTS and name_of(p) == "documents") or (
            nm in _SAVES_IN_HOME and is_home(p)
        ):
            add(i, GAME, TEXT["saves"])
        elif nm in _SYSTEM_AT_TOP and at_top(i):
            add(i, SYSTEM, TEXT["system"])
        elif nm.startswith("$ntfs") and p in roots:
            add(i, SYSTEM, TEXT["mft"])
        elif nm in _SOFTWARE_AT_TOP and at_top(i):
            add(i, SOFTWARE, TEXT["software"])
            found.blocks.append(i)
        elif nm in _SOFTWARE_IN_HOME and is_home(p):
            add(i, SOFTWARE, TEXT[_SOFTWARE_IN_HOME[nm]])
            found.blocks.append(i)
        if nm == "paks" and name_of(p) == "content":  # Unreal: <game>/<project>/Content/Paks
            proj = par(p)
            top = par(proj)
            engine = top > 0 and any(name_of(k) == "engine" for k in store.children(top))
            add(top if engine else proj, GAME, TEXT["unreal"], i)
        holder = nm in _LAUNCHERS or (nm == "games" and name_of(p) in _LAUNCHER_HOMES)
        if holder:  # a launcher's folder of games: each child folder is a game
            skip = _LAUNCHERS.get(nm, frozenset())
            label = TEXT["launcher"].format(name=store.name(p) if nm == "games" else store.name(i))
            for k in store.children(i):
                if kind[k] == Kind.DIR and not flags[k] & gone and name_of(k) not in skip:
                    add(k, GAME, label)
    exe: set[int] = set()
    other: set[int] = set()
    for lo in range(a, b, CHUNK):
        _engines(store, lo, min(b, lo + CHUNK), add, exe, other)
    for folder in sorted(exe & other):
        _engine_folder(store, int(folder), add)
    return found


def _engines(
    store: NodeStore,
    a: int,
    b: int,
    add: Callable[[int, int, str, int | None], None],
    exe: set[int],
    other: set[int],
) -> None:
    """Project files by extension (``*.sln`` …) among rows ``[a, b)``; and the folders that may hold
    an engine build: those with an ``.exe`` (into ``exe``) and those with a ``<name>_Data`` folder or
    a ``.pck`` (into ``other``). Unity: ``<name>.exe`` beside ``<name>_Data``; Godot: ``.pck`` beside
    ``.exe`` (checked by :func:`_engine_folder` for folders in both)."""
    c = store.cols
    kind, parent, flags = c["kind"][a:b], c["parent"], c["flags"][a:b]
    live = (flags & np.uint32(Flag.GONE)) == 0
    ext = c["ext"][a:b]
    table = store.ext_table
    ids = np.arange(a, b, dtype=np.int64)

    def with_ext(names: Iterable[str]) -> np.ndarray:
        want = [j for j, e in enumerate(table) if e in names]
        return ids[np.isin(ext, want) & live] if want else ids[:0]

    for i in with_ext(_PROJECT_EXTS).tolist():
        add(int(parent[i]), PROJECT, TEXT["project"].format(marker=store.name(i)), i)
    exe.update(parent[with_ext(("exe",))].tolist())
    dirs = ids[(kind == Kind.DIR) & live & (c["name_len"][a:b] > 5)]
    other.update(parent[_ending_with(store, dirs, _DATA_SUFFIX)].tolist())
    other.update(parent[with_ext(("pck",))].tolist())


def _engine_folder(store: NodeStore, folder: int, add: Callable[[int, int, str, int | None], None]) -> None:
    exes: set[str] = set()
    data: dict[str, int] = {}
    pck: int | None = None
    c = store.cols
    for k in store.children(folder):
        if c["flags"][k] & np.uint32(Flag.GONE):
            continue
        low = store.name(k).casefold()
        if c["kind"][k] == Kind.DIR:
            if low.endswith("_data"):
                data[low[:-5]] = k
        elif low.endswith(".exe"):
            exes.add(low[:-4])
        elif low.endswith(".pck"):
            pck = k
    unity = next((d for stem, d in data.items() if stem in exes), None)
    if unity is not None:
        add(folder, GAME, TEXT["unity"], unity)
    elif pck is not None and exes:
        add(folder, GAME, TEXT["godot"], pck)


def _project_guard(store: NodeStore, folder: int, roots: set[int]) -> bool:
    """A project may start at ``folder`` as far as the folder itself goes: not a volume, not a home
    or the folder holding homes, not a standard user folder right in a home."""
    parent = store.cols["parent"]
    if folder in roots:
        path = store.name(folder)
        return not _VOLUME.match(path) and not _HOME.search(path)
    p = int(parent[folder])
    own = _base(store, folder)
    pname = _base(store, p) if p > 0 else ""
    if pname in _HOME_PARENTS or own in _HOME_PARENTS:  # a home, or Users / home
        return False
    if p in roots and _HOME.search(store.name(p).rstrip("\\/") + "/" + store.name(folder)):
        return False
    home = (p in roots and bool(_HOME.search(store.name(p)))) or (
        p > 0 and int(parent[p]) > 0 and _base(store, int(parent[p])) in _HOME_PARENTS
    )
    return not (home and (own in _USER_FOLDERS or own.startswith("onedrive")))


# --------------------------------------------------------------------------- the column


def _encode(table: ClassTable) -> dict[int, int]:
    """Rule -> column value under ``table`` (a rule whose class the table lacks is off)."""
    out: dict[int, int] = {}
    for rule, cid in RULE_CLASS.items():
        if cid == OTHER or cid not in table.by_id:
            continue
        idx = table.ids.index(cid)
        if idx < SWALLOW_MAX:
            out[rule] = FALLBACK_BASE + idx if rule in FALLBACK else idx + 1
    return out


def _value_of(table: ClassTable, cid: str) -> int:
    """Column value of a user's choice: swallowed by ``cid``; 0 for "by extension only"."""
    if not cid or cid == OTHER or cid not in table.by_id:
        return 0
    idx = table.ids.index(cid)
    return idx + 1 if idx < SWALLOW_MAX else 0


def _levels(store: NodeStore, dirs: np.ndarray) -> list[np.ndarray]:
    depth = store.cols["depth"]
    order = dirs[np.argsort(depth[dirs], kind="stable")]
    return np.split(order, np.flatnonzero(np.diff(depth[order])) + 1)


class _Entry:
    """One store's column under one class table and one set of the user's choices."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.key: tuple[int, int] = (-1, -1)
        self.version = -1
        self.n = 0
        self.col = np.zeros(0, dtype=np.uint8)  # the column
        self.own = np.zeros(0, dtype=np.uint8)  # a rule folder's own value
        self.blocked = np.zeros(0, dtype=bool)  # no project starts here
        self.blocks = np.zeros(0, dtype=bool)  # ... because of this folder's own name
        self.roots: dict[int, _Rule] = {}
        self.evidence: dict[int, int] = {}
        self.user: dict[int, str] = {}  # folder -> the user's class there ("" = by extension)

    def _grow(self, n: int) -> None:
        if len(self.col) >= n:
            return
        cap = max(n, int(len(self.col) * 1.25) + 4096)
        for name in ("col", "own", "blocked", "blocks"):
            old = getattr(self, name)
            new = np.zeros(cap, dtype=old.dtype)
            new[: len(old)] = old
            setattr(self, name, new)

    def _propagate(self, store: NodeStore, dirs: np.ndarray) -> None:
        """Folders ``dirs``, outer before inner: an inherited swallow wins, then the folder's own
        rule, then an inherited fallback; projects stay blocked below a blocking folder."""
        parent = store.cols["parent"]
        for level in _levels(store, dirs):
            up = parent[level]
            inh = self.col[up]
            mine = self.own[level]
            swallow = (inh > 0) & (inh < FALLBACK_BASE)
            self.col[level] = np.where(swallow, inh, np.where(mine > 0, mine, inh))
            self.blocked[level] = self.blocked[up] | self.blocks[level]

    def _fill(self, store: NodeStore, ids: np.ndarray) -> None:
        """Rows other than folders take their folder's value."""
        rows = ids[store.cols["kind"][ids] != Kind.DIR]
        if len(rows):
            self.col[rows] = self.col[store.cols["parent"][rows]]

    def _take(self, store: NodeStore, found: _Found, table: ClassTable) -> list[int]:
        """Keep what was found; projects only where allowed. Returns the folders that changed."""
        codes = _encode(table)
        roots = {int(r) for r in store.roots()}
        for f in found.blocks:
            self.blocks[f] = True
            self.blocked[f] = True
        changed: list[int] = []
        for folder, r in found.roots.items():
            if r.rule == PROJECT and (
                self._blocked_above(store, folder) or not _project_guard(store, folder, roots)
            ):
                continue
            value = codes.get(r.rule, 0)
            old = self.roots.get(folder)
            if not value or (old is not None and old.rule not in FALLBACK and r.rule in FALLBACK):
                continue
            self.roots[folder] = r
            if self.own[folder] != value:
                self.own[folder] = value
                changed.append(folder)
        for marker, folder in found.evidence.items():
            if folder in self.roots:
                self.evidence[marker] = folder
        return changed

    def _blocked_above(self, store: NodeStore, folder: int) -> bool:
        """Projects are blocked at ``folder`` (its own name, or a blocking folder above it)."""
        if self.blocked[folder]:
            return True
        parent = store.cols["parent"]
        p = int(parent[folder])
        while p > 0:  # rows not propagated yet (a folder of this very batch)
            if self.blocks[p]:
                return True
            p = int(parent[p])
        return False

    def build(self, store: NodeStore, table: ClassTable, n: int, version: int) -> None:
        """The column of rows ``[0, n)`` from scratch, as of the store's ``version`` (read before ``n``)."""
        self.col = np.zeros(n, dtype=np.uint8)
        self.own = np.zeros(n, dtype=np.uint8)
        self.blocked = np.zeros(n, dtype=bool)
        self.blocks = np.zeros(n, dtype=bool)
        self.roots, self.evidence = {}, {}
        found = _detect(store, 1, n)
        c = store.cols
        live = (c["flags"][:n] & np.uint32(Flag.GONE)) == 0
        dirs = np.flatnonzero((c["kind"][:n] == Kind.DIR) & live)
        dirs = dirs[dirs > 0]
        # blocking first (projects are kept only where nothing above blocks them), then the rules
        for f in found.blocks:
            self.blocks[f] = True
        for level in _levels(store, dirs):
            self.blocked[level] = self.blocked[c["parent"][level]] | self.blocks[level]
        self._take(store, found, table)
        self._propagate(store, dirs)
        for lo in range(1, n, CHUNK):
            self._fill(store, np.arange(lo, min(n, lo + CHUNK), dtype=np.int64))
        self._apply_user(store, table, n)
        self.n, self.version = n, version

    def update(self, store: NodeStore, table: ClassTable, n: int, version: int) -> None:
        """Rows added since the last look up to ``n`` (a scan, a change relisting a folder), and rule
        folders that lost what made them so: only their subtrees are worked out again."""
        c = store.cols
        flags, parent = c["flags"], c["parent"]
        gone = np.uint32(Flag.GONE)
        self._grow(n)
        dirty: set[int] = set()
        for folder in [f for f in self.roots if flags[f] & gone]:  # relisted: found again below
            del self.roots[folder]
            self.own[folder] = 0
        alive: dict[int, int] = {}
        for marker, folder in list(self.evidence.items()):
            if flags[marker] & gone:
                del self.evidence[marker]
                continue
            now = int(parent[marker])
            if now != folder:  # the folder's row moved (relisted): the rule moves with it
                self.evidence[marker] = now
                rule = self.roots.pop(folder, None)
                if rule is not None and now > 0:
                    self.roots[now] = rule
                    self.own[now] = _encode(table).get(rule.rule, 0)
                    if now < self.n:
                        dirty.add(now)
            alive[self.evidence[marker]] = alive.get(self.evidence[marker], 0) + 1
        for folder in [f for f, r in self.roots.items() if r.evidence and not alive.get(f)]:
            del self.roots[folder]
            self.own[folder] = 0
            dirty.add(folder)
        if n > self.n:
            found = _detect(store, self.n, n)
            for f in found.blocks:
                self.blocks[f] = True
            changed = self._take(store, found, table)
            dirty.update(f for f in changed if f < self.n)
            ids = np.arange(self.n, n, dtype=np.int64)
            live = (flags[self.n : n] & gone) == 0
            self._propagate(store, ids[(c["kind"][self.n : n] == Kind.DIR) & live])
            self._fill(store, ids)
        for folder in _outermost(store, dirty):
            if flags[folder] & gone:
                continue
            below = _below(store, folder, n)
            dirs = np.concatenate([np.array([folder], dtype=np.int64), below[c["kind"][below] == Kind.DIR]])
            self._propagate(store, dirs)
            self._fill(store, below)
        if self.user or (n > self.n and _folders):  # a chosen folder may be among the new rows
            self._apply_user(store, table, n)
        self.n, self.version = n, version

    def _apply_user(self, store: NodeStore, table: ClassTable, n: int) -> None:
        self.user = {}
        chosen = folders()
        if not chosen:
            return
        index = PathIndex(store)
        located = []
        for path, cid in chosen:
            node, _deepest = index.locate(path)
            if node is not None and node < n and store.cols["kind"][node] == Kind.DIR:
                located.append((int(store.cols["depth"][node]), int(node), cid))
        for _depth, node, cid in sorted(located):
            value = _value_of(table, cid)
            self.col[node] = value
            below = _below(store, node, n)
            if len(below):
                self.col[below] = value
            self.user[node] = cid


def _below(store: NodeStore, folder: int, n: int) -> np.ndarray:
    """:func:`subtree_ids` of the rows ``[0, n)`` the column is being worked out for: a scan thread
    may have folded more listings since (the next look takes those)."""
    ids = subtree_ids(store, folder)
    return ids[ids < n]


def _outermost(store: NodeStore, folders_: set[int]) -> list[int]:
    """The folders of a set with no ancestor in it."""
    parent = store.cols["parent"]
    out = []
    for f in folders_:
        p = int(parent[f])
        while p > 0 and p not in folders_:
            p = int(parent[p])
        if p <= 0:
            out.append(f)
    return out


_entries: WeakKeyDictionary[NodeStore, _Entry] = WeakKeyDictionary()
_entries_lock = threading.Lock()


def _entry(store: NodeStore, table: ClassTable) -> tuple[_Entry, np.ndarray]:
    """The store's entry, up to date, and its column (taken under the entry's lock).

    A scan folds listings on the event loop thread while compute threads read the store (without a
    lock: it only appends). So the version is read before the node count, and both are recorded as
    read: a listing that lands while the column is worked out leaves the entry behind the store, and
    the next look takes its rows. (Recording the version at the end marked them done without
    working them out: a column shorter than the nodes of a later walk, IndexError in a summary.)
    """
    with _entries_lock:
        e = _entries.get(store)
        if e is None:
            e = _entries[store] = _Entry()
    key = (id(table), folders_version())
    with e.lock:
        version = store.version
        n = len(store)
        if e.key != key or n < e.n:
            e.build(store, table, n, version)
            e.key = key
        elif e.version != version or n > e.n:
            e.update(store, table, n, version)
        return e, e.col[:n]


def column(store: NodeStore, table: ClassTable) -> np.ndarray:
    """One ``uint8`` per node of ``store`` (see the module doc), up to date with the store: it covers
    every node the store had when the call began (a scan only adds), so ids from an earlier walk."""
    return _entry(store, table)[1]


def resolve(col: np.ndarray, base: np.ndarray, other: int) -> np.ndarray:
    """Class index per row: ``base`` (by extension) under the column's folder classes."""
    swallow = (col > 0) & (col < FALLBACK_BASE)
    fallback = (col >= FALLBACK_BASE) & (base == other)
    out = np.where(swallow, col.astype(np.int64) - 1, base)
    out = np.where(fallback, col.astype(np.int64) - FALLBACK_BASE, out)
    return out.astype(base.dtype, copy=False)


@dataclass(frozen=True, slots=True)
class Reason:
    class_id: str
    reason: str
    folder: int  # the folder that decided
    user: bool  # the user's choice
    fallback: bool  # only files whose extension is in no class take it


def explain(store: NodeStore, node: int, table: ClassTable) -> Reason | None:
    """Why ``node`` (a folder, or a file's folder) takes a folder class, if it does."""
    e, _col = _entry(store, table)
    parent = store.cols["parent"]
    if store.cols["kind"][node] != Kind.DIR:
        node = int(parent[node])
    chain: list[int] = []
    p = node
    while p > 0:
        chain.append(p)
        p = int(parent[p])
    for f in chain:  # the deepest choice of the user wins
        if f in e.user:
            cid = e.user[f]
            return Reason(cid, TEXT["user"], f, True, False) if cid and cid in table.by_id else None
    pick: tuple[int, _Rule] | None = None
    for f in reversed(chain):  # the outermost swallowing rule; else the nearest fallback
        r = e.roots.get(f)
        if r is None:
            continue
        pick = (f, r)
        if r.rule not in FALLBACK:
            break
    if pick is None:
        return None
    folder, r = pick
    cid = RULE_CLASS[r.rule]
    if cid not in table.by_id:
        return None
    return Reason(cid, r.reason, folder, False, r.rule in FALLBACK)
