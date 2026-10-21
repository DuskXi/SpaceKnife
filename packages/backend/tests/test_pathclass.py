"""Folder classes (UI-W7-38): a recognised folder decides the class of the files in it."""

from __future__ import annotations

import os
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from spaceknife import pathclass
from spaceknife.classes import DEFAULT_CLASSES, ClassTable
from spaceknife.filter import FilterContext, evaluate
from spaceknife.layout.basis import NO_CLASS, Basis
from spaceknife.store import mutate
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore
from spaceknife.store.subtree import summarize
from spaceknife_protocol.classes import BUILTIN_CLASSES
from spaceknife_protocol.filter_syntax import parse

from .store_builder import build, ext_of, find

# the scanned volume: a drive on Windows, a folder elsewhere (both 3 characters with the separator)
C = "C:\\" if sys.platform == "win32" else "/c"


def at(*parts: str) -> str:
    """A path on that volume, as this system writes it."""
    return os.path.join(C, *parts)


@pytest.fixture(autouse=True)
def no_choices() -> Iterator[None]:
    pathclass.replace_folders([])
    yield
    pathclass.replace_folders([])


def classes_of(store: NodeStore, paths: list[str], table: ClassTable = DEFAULT_CLASSES) -> dict[str, str]:
    basis = Basis(store, table)
    ids = np.array([find(store, p) for p in paths], dtype=np.int64)
    return dict(zip(paths, (basis.class_ids[i] for i in basis.cls_of(ids).tolist()), strict=True))


def fresh(store: NodeStore, table: ClassTable = DEFAULT_CLASSES) -> np.ndarray:
    """The column worked out from scratch (to compare an updated one with)."""
    pathclass._entries.pop(store, None)
    return pathclass.column(store, table).copy()


def same_as_fresh(store: NodeStore) -> bool:
    """The kept column equals one worked out from scratch, on the live rows (a change leaves the
    old rows of a relisted folder GONE: outside every child range, never drawn or counted)."""
    kept = pathclass.column(store, DEFAULT_CLASSES).copy()
    live = (store.cols["flags"][: len(store)] & np.uint32(Flag.GONE)) == 0
    return bool(np.array_equal(kept[live], fresh(store)[live]))


GAMES: dict[str, Any] = {
    "Program Files (x86)": {
        "Steam": {
            "steam.exe": 10,
            "steamapps": {
                "common": {"Foo": {"Foo.exe": 1000, "intro.mp4": 5000, "data.pak": 3000, "package.json": 10}},
                "workshop": {"content": {"123": {"mod.bin": 40}}},
            },
        }
    },
    "Games": {
        "Bar": {"Bar.exe": 100, "Bar_Data": {"level0": 200, "music.ogg": 300}},
        "Baz": {"UnityPlayer.dll": 50, "Baz.exe": 10, "cg.mp4": 70},
        "Qux": {"Engine": {"x.dll": 5}, "Qux": {"Content": {"Paks": {"Qux.pak": 900}}}},
        "Godot": {"game.exe": 10, "game.pck": 500},
        "Blizz": {".build.info": 1, "Data": {"data.000": 60}},
        "NotAGame": {"tool.exe": 10, "readme.txt": 5},
    },
    "Epic Games": {"Launcher": {"Portal.exe": 10, "ui.dat": 5}, "Fortnite": {"pak.ucas": 80}},
    "miHoYo Launcher": {"games": {"Genshin Impact Game": {"YuanShen.exe": 10, "blob.blk": 700}}},
}


def test_game_installs_swallow_their_files() -> None:
    store = build(GAMES, root=C)
    got = classes_of(
        store,
        [
            "Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4",
            "Program Files (x86)/Steam/steamapps/common/Foo/data.pak",
            "Program Files (x86)/Steam/steamapps/common/Foo/package.json",  # the outer game wins
            "Program Files (x86)/Steam/steamapps/workshop/content/123/mod.bin",
            "Program Files (x86)/Steam/steam.exe",  # the launcher itself: software
            "Games/Bar/Bar_Data/music.ogg",  # Unity: Bar.exe beside Bar_Data
            "Games/Baz/cg.mp4",  # Unity: UnityPlayer.dll
            "Games/Qux/Engine/x.dll",  # Unreal: the folder holding Engine and Qux/Content/Paks
            "Games/Godot/game.pck",
            "Games/Blizz/Data/data.000",
            "Games/NotAGame/readme.txt",
            "Epic Games/Fortnite/pak.ucas",
            "Epic Games/Launcher/ui.dat",  # the launcher's own folder is no game
            "miHoYo Launcher/games/Genshin Impact Game/blob.blk",
        ],
    )
    assert got == {
        "Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4": "game",
        "Program Files (x86)/Steam/steamapps/common/Foo/data.pak": "game",
        "Program Files (x86)/Steam/steamapps/common/Foo/package.json": "game",
        "Program Files (x86)/Steam/steamapps/workshop/content/123/mod.bin": "game",
        "Program Files (x86)/Steam/steam.exe": "exec",
        "Games/Bar/Bar_Data/music.ogg": "game",
        "Games/Baz/cg.mp4": "game",
        "Games/Qux/Engine/x.dll": "game",
        "Games/Godot/game.pck": "game",
        "Games/Blizz/Data/data.000": "game",
        "Games/NotAGame/readme.txt": "doc",
        "Epic Games/Fortnite/pak.ucas": "game",
        "Epic Games/Launcher/ui.dat": "other",
        "miHoYo Launcher/games/Genshin Impact Game/blob.blk": "game",
    }


HOME: dict[str, Any] = {
    "Users": {
        "lenny": {
            ".gitconfig": 1,
            "code": {
                "app": {
                    "package.json": 10,
                    "src": {"main.ts": 100},
                    "weights.pt": 9000,
                    "node_modules": {"lib": {"package.json": 5, "index.js": 50}},
                    "build": {"Game.exe": 10, "Game_Data": {"a.assets": 30}},  # a built game: the project's
                },
                "Cargo.toml": 1,  # code/ is a project too: the outer one wins, all the same class
            },
            "Downloads": {"package.json": 10, "movie.mp4": 100, "repo": {".git": {"HEAD": 1}, "x.bin": 4}},
            "Documents": {"My Games": {"Save1.sav": 20}, "notes.txt": 3},
            "AppData": {"Local": {"tool": {"package.json": 1, "blob": 10, "pic.png": 5}}},
            ".cache": {"huggingface": {"hub": {"blob1": 100}}, "pip": {"wheel": 7}},
            ".ollama": {"models": {"blobs": {"sha256-abc": 1000}}},
            ".minecraft": {"saves": {"level.dat": 9}},
        }
    },
    "System Volume Information": {"{snap}": 400},
    "Program Files": {"App": {"app.exe": 10, "res.xyz": 20, "pic.png": 30}},
    "Data": {"lib": {"setup.py": 1, "m.py": 2}},
}


def test_a_folder_has_a_class_only_when_swallowed() -> None:
    """The class hover highlight lights a folder only when everything in it is that class."""
    store = build(GAMES, root=C)
    basis = Basis(store, DEFAULT_CLASSES)
    paths = [
        "Program Files (x86)/Steam/steamapps/common/Foo",  # a game: all of it
        "Games/Bar/Bar_Data",  # inside a game
        "Program Files (x86)/Steam",  # software: only its unclassified files fall back
        "Games/NotAGame",
        "Games",
    ]
    ids = np.array([find(store, p) for p in paths], dtype=np.int64)
    got = basis.cls_of(ids).tolist()
    game = basis.class_ids.index("game")
    assert got == [game, game, NO_CLASS, NO_CLASS, NO_CLASS]


def test_projects_models_system_and_software() -> None:
    store = build(HOME, root=C)
    got = classes_of(
        store,
        [
            "Users/lenny/code/app/weights.pt",
            "Users/lenny/code/app/node_modules/lib/index.js",
            "Users/lenny/code/app/build/Game_Data/a.assets",
            "Users/lenny/Downloads/movie.mp4",  # Downloads holds a package.json: still no project
            "Users/lenny/Downloads/repo/x.bin",  # ... but a repository inside it is one
            "Users/lenny/.gitconfig",
            "Users/lenny/Documents/My Games/Save1.sav",
            "Users/lenny/Documents/notes.txt",
            "Users/lenny/AppData/Local/tool/blob",  # app data: only unclassified files
            "Users/lenny/AppData/Local/tool/pic.png",
            "Users/lenny/AppData/Local/tool/package.json",  # no project inside AppData
            "Users/lenny/.cache/huggingface/hub/blob1",
            "Users/lenny/.cache/pip/wheel",
            "Users/lenny/.ollama/models/blobs/sha256-abc",
            "Users/lenny/.minecraft/saves/level.dat",
            "System Volume Information/{snap}",
            "Program Files/App/res.xyz",
            "Program Files/App/pic.png",
            "Data/lib/m.py",
        ],
    )
    assert got == {
        "Users/lenny/code/app/weights.pt": "code",
        "Users/lenny/code/app/node_modules/lib/index.js": "code",
        "Users/lenny/code/app/build/Game_Data/a.assets": "code",
        "Users/lenny/Downloads/movie.mp4": "video",
        "Users/lenny/Downloads/repo/x.bin": "code",
        "Users/lenny/.gitconfig": "other",
        "Users/lenny/Documents/My Games/Save1.sav": "game",
        "Users/lenny/Documents/notes.txt": "doc",
        "Users/lenny/AppData/Local/tool/blob": "exec",
        "Users/lenny/AppData/Local/tool/pic.png": "image",
        "Users/lenny/AppData/Local/tool/package.json": "data",
        "Users/lenny/.cache/huggingface/hub/blob1": "data",
        "Users/lenny/.cache/pip/wheel": "other",
        "Users/lenny/.ollama/models/blobs/sha256-abc": "data",
        "Users/lenny/.minecraft/saves/level.dat": "game",
        "System Volume Information/{snap}": "exec",
        "Program Files/App/res.xyz": "exec",
        "Program Files/App/pic.png": "image",
        "Data/lib/m.py": "code",
    }


def test_a_home_or_volume_is_never_a_project() -> None:
    store = build(
        {"Users": {"lenny": {".git": {"HEAD": 1}, "photo.jpg": 5}}, ".git": {"HEAD": 1}, "a.mp3": 3}, root=C
    )
    got = classes_of(store, ["Users/lenny/photo.jpg", "a.mp3"])
    assert got == {"Users/lenny/photo.jpg": "image", "a.mp3": "audio"}
    # a scan of the home folder itself: its .git makes no project either
    home = build({".git": {"HEAD": 1}, "photo.jpg": 5}, root="C:\\Users\\lenny")
    assert classes_of(home, ["photo.jpg"]) == {"photo.jpg": "image"}
    # a scan of a project folder: the root is the project
    proj = build({"Cargo.toml": 1, "big.mp4": 5}, root="C:\\work\\proj")
    assert classes_of(proj, ["big.mp4"]) == {"big.mp4": "code"}


def test_a_rule_is_off_when_its_class_is_gone() -> None:
    store = build(GAMES, root=C)
    no_game = ClassTable(tuple(c for c in BUILTIN_CLASSES if c.id != "game"))
    path = "Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4"
    assert classes_of(store, [path], no_game) == {path: "video"}


def test_the_users_choices_win(tmp_path: Path) -> None:
    store = build(GAMES, root=C)
    foo = "Program Files (x86)/Steam/steamapps/common/Foo"
    pathclass.set_folder(at("Games", "NotAGame"), "game")
    pathclass.set_folder(at("Program Files (x86)", "Steam", "steamapps", "common", "Foo"), "")  # by extension
    got = classes_of(store, ["Games/NotAGame/readme.txt", f"{foo}/intro.mp4", f"{foo}/data.pak"])
    assert got == {
        "Games/NotAGame/readme.txt": "game",
        f"{foo}/intro.mp4": "video",
        f"{foo}/data.pak": "other",
    }
    why = pathclass.explain(store, find(store, "Games/NotAGame/readme.txt"), DEFAULT_CLASSES)
    assert why is not None and why.user and why.class_id == "game"
    assert pathclass.explain(store, find(store, f"{foo}/intro.mp4"), DEFAULT_CLASSES) is None
    # saved and read back; forgetting a choice brings the rule back
    path = tmp_path / "class_folders.json"
    pathclass.save(path)
    pathclass.set_folder(at("Games", "NotAGame"), None)
    pathclass.set_folder(at("Program Files (x86)", "Steam", "steamapps", "common", "Foo"), None)
    assert classes_of(store, [f"{foo}/intro.mp4"]) == {f"{foo}/intro.mp4": "game"}
    pathclass.replace_folders(pathclass.load(path))
    assert len(pathclass.folders()) == 2
    pathclass.replace_folders([])
    pathclass.save(path)
    assert not path.exists()


def test_reasons() -> None:
    store = build({**GAMES, **HOME}, root=C)

    def why(path: str) -> tuple[str, str, bool]:
        r = pathclass.explain(store, find(store, path), DEFAULT_CLASSES)
        assert r is not None
        return r.class_id, store.path(r.folder).replace("\\", "/"), r.fallback

    def folder(*parts: str) -> str:
        return at(*parts).replace("\\", "/")

    assert why("Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4") == (
        "game",
        folder("Program Files (x86)", "Steam", "steamapps"),
        False,
    )
    assert why("Users/lenny/code/app/src/main.ts") == ("code", folder("Users", "lenny", "code"), False)
    assert why("Program Files/App/res.xyz") == ("exec", folder("Program Files"), True)
    assert pathclass.explain(store, find(store, "Users/lenny/Documents/notes.txt"), DEFAULT_CLASSES) is None


def test_filters_and_totals_follow_the_folder_classes() -> None:
    store = build(GAMES, root=C)
    ctx = FilterContext(now=0, classes=DEFAULT_CLASSES)

    def matched(text: str) -> set[str]:
        nf = parse(text).normal
        assert nf is not None
        res = evaluate(store, nf, ctx)
        return {store.path(int(i)).replace("\\", "/")[3:] for i in np.flatnonzero(res.match)}

    assert "Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4" in matched(":class:game")
    assert "Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4" not in matched(":class:video")
    assert matched(":class:other") == {"Epic Games/Launcher/ui.dat"}
    root = next(iter(store.roots()))
    basis = Basis(store, DEFAULT_CLASSES)
    s = summarize(store, root, basis.cls_of, len(basis.class_ids), 3, basis.ext_cls_of)
    assert s.class_parts is not None
    game, video = basis.class_ids.index("game"), basis.class_ids.index("video")
    assert s.class_parts[game, video] == 5000 + 70  # intro.mp4 and cg.mp4 were videos


# --------------------------------------------------------------------------- following changes


def relist(store: NodeStore, folder: int, children: dict[str, int | None]) -> None:
    """Relist ``folder`` with these children (None: a folder), as a change on disk would."""
    names = [n.encode() for n in children]
    k = len(names)
    entries = {col: np.zeros(k, dtype=np.int64) for col in ENTRY_COLUMNS}
    for i, (_name, size) in enumerate(children.items()):
        entries["kind"][i] = Kind.DIR if size is None else Kind.FILE
        entries["size"][i] = entries["alloc"][i] = size or 0
    ext = np.array([store.ext_id(ext_of(n)) for n in children], dtype=np.uint16)
    mutate.relist(
        store, folder, b"".join(names), np.array([len(n) for n in names], dtype=np.uint16), entries, ext
    )


def test_changes_update_only_what_they_touch() -> None:
    store = build(
        {"Users": {"lenny": {"work": {"a": {"x.bin": 5}, "b": {"y.bin": 6}}}}, "Games": {"G": {"G.exe": 1}}},
        root=C,
    )
    a = find(store, "Users/lenny/work/a")
    assert classes_of(store, ["Users/lenny/work/a/x.bin"])["Users/lenny/work/a/x.bin"] == "other"
    entry = pathclass._entries[store]
    # a marker appears: the folder becomes a project
    relist(store, a, {"x.bin": 5, "pyproject.toml": 1})
    pathclass.column(store, DEFAULT_CLASSES)
    assert (
        pathclass._entries[store] is entry and entry.key[1] == pathclass.folders_version()
    )  # updated, not rebuilt
    assert same_as_fresh(store)
    assert classes_of(store, ["Users/lenny/work/a/x.bin"])["Users/lenny/work/a/x.bin"] == "code"
    # a Unity build appears inside an existing folder
    g = find(store, "Games/G")
    relist(store, g, {"G.exe": 1, "G_Data": None, "clip.mp4": 9})
    assert same_as_fresh(store)
    assert classes_of(store, ["Games/G/clip.mp4"])["Games/G/clip.mp4"] == "game"
    # the marker goes away again: back to the extensions
    a = find(store, "Users/lenny/work/a")
    relist(store, a, {"x.bin": 5})
    assert same_as_fresh(store)
    assert classes_of(store, ["Users/lenny/work/a/x.bin"])["Users/lenny/work/a/x.bin"] == "other"
    # the parent of a project is relisted (the project's row moves): still a project
    b = find(store, "Users/lenny/work/b")
    relist(store, b, {"y.bin": 6, ".git": None})
    work = find(store, "Users/lenny/work")
    relist(store, work, {"a": None, "b": None, "new.txt": 1})
    assert same_as_fresh(store)


def test_a_scan_in_progress() -> None:
    spec = {"Users": {"lenny": {"p": {"Cargo.toml": 1, "src": {"lib.rs": 3}}, "q": {"v.mp4": 9}}}}
    store = build(spec, root=C, pending=("Users/lenny/p/src",))
    pathclass.column(store, DEFAULT_CLASSES)  # looked at before the scan is done
    src = find(store, "Users/lenny/p/src")
    names = [b"lib.rs", b"big.bin"]
    entries = {k: np.zeros(2, dtype=np.int64) for k in ENTRY_COLUMNS}
    entries["kind"][:] = Kind.FILE
    entries["size"][:] = entries["alloc"][:] = [3, 7]
    exts = np.array([store.ext_id("rs"), store.ext_id("bin")], dtype=np.uint16)
    store.ingest_names(src, names, entries, exts)
    assert same_as_fresh(store)
    assert classes_of(store, ["Users/lenny/p/src/big.bin"])["Users/lenny/p/src/big.bin"] == "code"
