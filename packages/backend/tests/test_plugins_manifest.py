"""Manifests and names, folder hashes, installing (folders and zips, safely), icons and actions."""

from __future__ import annotations

import os
import stat
import sys
import zipfile
from pathlib import Path

import pytest

from spaceknife.plugins import install as inst
from spaceknife.plugins import paths as ppaths
from spaceknife.plugins.icons import sanitize_svg
from spaceknife.plugins.store import hash_folder, read_record
from spaceknife_protocol.plugins import split_wire_name, valid_local_name, valid_plugin_id, wire_name
from spaceknife_sdk.actions import ActionError, normalize_action, resolve_action
from spaceknife_sdk.errors import ManifestError
from spaceknife_sdk.manifest import parse_manifest

from .plugin_helpers import fixture

BASE = 'id = "example.thing"\nversion = "0.1.0"\nname = { zh = "东西", en = "Thing" }\n'


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def test_names() -> None:
    assert valid_plugin_id("example.dupes") and valid_plugin_id("acme.zip-source")
    for bad in (
        "dupes",
        "Example.dupes",
        "example.",
        "a.b-",
        "view.thing",
        "plugin.x",
        "builtin.x",
        "x" * 40 + ".y",
    ):
        assert not valid_plugin_id(bad), bad
    assert valid_plugin_id("builtin.export", builtin=True)
    assert valid_local_name("min_size") and not valid_local_name("min__size") and not valid_local_name("a.b")
    assert wire_name("example.dupes", "find") == "plugin.example.dupes.find"
    assert split_wire_name("plugin.example.dupes.find") == ("example.dupes", "find")
    assert split_wire_name("plugin.list") is None and split_wire_name("view.open") is None


def test_manifest_errors_say_what_to_fix() -> None:
    m = parse_manifest(
        BASE + '[[commands]]\nname = "go"\ntitle = "走"\nkeys = ["Ctrl+G"]\naction = { cycle = "visual" }\n'
    )
    assert m.commands[0].action == {"kind": "cycle", "what": "visual", "step": 1}
    with pytest.raises(ManifestError) as err:
        parse_manifest(BASE + '[[commands]]\nnam = "go"\ntitle = "走"\n')
    issues = {i.code: i for i in err.value.issues}
    assert issues["unknown_field"].fix == "did you mean 'name'?" and issues["unknown_field"].line == 5
    with pytest.raises(ManifestError, match="net_hosts"):
        parse_manifest(BASE + 'permissions = ["net"]\n')
    with pytest.raises(ManifestError, match="reserved"):
        parse_manifest(BASE.replace("example.thing", "view.thing"))
    with pytest.raises(ManifestError, match="main-process"):
        parse_manifest(BASE + 'main = "main.js"\n')
    with pytest.raises(ManifestError, match="semver"):
        parse_manifest(BASE.replace('"0.1.0"', '"1"'))
    with pytest.raises(ManifestError, match="palette"):
        parse_manifest(
            BASE
            + '[[color_modes]]\nname = "x"\ntitle = "x"\npalette = ["muted"]\nby = "ext"\nmap = { mp4 = 3 }\n'
        )
    with pytest.raises(ManifestError, match="options"):
        parse_manifest(
            BASE + '[[settings]]\nname = "x"\ntitle = "x"\ndefault = "c"\noptions = [["a", "甲"]]\n'
        )


def test_actions() -> None:
    assert normalize_action({"open": "report:dupes"}) == {"kind": "open", "target": "report:dupes"}
    assert normalize_action({"setting": "x", "toggle": True}) == {
        "kind": "setting",
        "key": "x",
        "value": None,
        "toggle": True,
    }
    for bad in (
        {},
        {"open": "nothing"},
        {"url": "http://x"},
        {"node": "shred"},
        {"call": "a", "open": "b"},
        {"toggle": "nope"},
    ):
        with pytest.raises(ActionError):
            normalize_action(bad)
    a = resolve_action(normalize_action({"call": "find", "args": {"n": 1}}), "example.dupes")
    assert a.method == "plugin.example.dupes.find" and a.args == {"n": 1}  # type: ignore[union-attr]
    c = resolve_action(normalize_action({"cleanup": "extra"}), "example.dupes")
    assert c.cleanup == "example.dupes.extra" and c.method == "plugin.example.dupes.extra"  # type: ignore[union-attr]
    s = resolve_action(normalize_action({"setting": "min", "value": 3}), "example.dupes")
    assert s.key == "plugin.example.dupes.min"  # type: ignore[union-attr]
    assert (
        resolve_action(normalize_action({"command": "view.rescan"}), "example.dupes").command == "view.rescan"
    )  # type: ignore[union-attr]


def test_folder_hash(tmp_path: Path) -> None:
    folder = tmp_path / "p"
    folder.mkdir()
    (folder / "plugin.toml").write_text(BASE, encoding="utf-8")
    (folder / "__pycache__").mkdir()
    (folder / "__pycache__" / "x.pyc").write_bytes(b"ignored")
    cache: dict[str, object] = {}
    h1 = hash_folder(folder, cache)  # type: ignore[arg-type]
    (folder / "__pycache__" / "y.pyc").write_bytes(b"still ignored")
    assert hash_folder(folder) == h1 and h1.startswith("sha256:")
    (folder / "extra.py").write_text("x = 1\n", encoding="utf-8")
    h2 = hash_folder(folder, cache)  # type: ignore[arg-type]
    assert h2 != h1 and "extra.py" in cache
    (folder / "extra.py").unlink()
    assert hash_folder(folder, cache) == h1 and "extra.py" not in cache  # type: ignore[arg-type]


def test_records() -> None:
    rec = read_record(fixture("t.decl"), dev=True)
    assert rec.id == "t.decl" and rec.state == "needs_confirm" and not rec.issues and not rec.has_code
    assert read_record(fixture("t.full"), dev=True).has_code


def _zip(path: Path, members: dict[str, bytes], *, modes: dict[str, int] | None = None) -> Path:
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in members.items():
            info = zipfile.ZipInfo(name)
            info.compress_type = zipfile.ZIP_DEFLATED
            if modes and name in modes:
                info.external_attr = modes[name] << 16
            z.writestr(info, data)
    return path


def test_install_from_a_folder_and_a_zip(tmp_path: Path) -> None:
    res = inst.install(fixture("t.decl"))
    assert (
        res.installed and res.id == "t.decl" and (ppaths.plugins_dir() / "t.decl" / "plugin.toml").is_file()
    )
    again = inst.install(fixture("t.decl"))
    assert again.conflict and not again.installed and again.existing_version == "0.1.0"
    replaced = inst.install(fixture("t.decl"), replace=True)
    assert replaced.installed
    manifest = (fixture("t.crash") / "plugin.toml").read_bytes()
    code = (fixture("t.crash") / "plugin.py").read_bytes()
    z = _zip(tmp_path / "crash.zip", {"t.crash/plugin.toml": manifest, "t.crash/plugin.py": code})
    res = inst.install(z)
    assert res.installed and (ppaths.plugins_dir() / "t.crash" / "plugin.py").read_bytes() == code
    assert not any((ppaths.plugins_dir() / ".incoming").iterdir())


@pytest.mark.parametrize(
    ("members", "why"),
    [
        ({"/abs/plugin.toml": b"x"}, "绝对路径"),
        ({"a/../../evil.txt": b"x"}, "外面"),
        ({"plugin.toml": b"x" * (2 << 20)}, "压缩比"),
        ({"readme.txt": b"x"}, "缺少"),
    ],
)
def test_unsafe_zips_are_refused(tmp_path: Path, members: dict[str, bytes], why: str) -> None:
    with pytest.raises(inst.InstallError, match=why):
        inst.install(_zip(tmp_path / "bad.zip", members))
    assert not (tmp_path / "evil.txt").exists()
    assert (
        not any(p.name != ".incoming" for p in ppaths.plugins_dir().iterdir())
        if ppaths.plugins_dir().exists()
        else True
    )


def test_links_in_a_zip_are_refused(tmp_path: Path) -> None:
    z = _zip(
        tmp_path / "link.zip",
        {"plugin.toml": BASE.encode(), "evil": b"/etc/passwd"},
        modes={"evil": stat.S_IFLNK | 0o777},
    )
    with pytest.raises(inst.InstallError, match="链接"):
        inst.install(z)


@pytest.mark.skipif(sys.platform == "win32", reason="symbolic links need extra rights on Windows")
def test_links_in_a_folder_are_refused(tmp_path: Path) -> None:
    folder = tmp_path / "p"
    folder.mkdir()
    (folder / "plugin.toml").write_text(BASE, encoding="utf-8")
    os.symlink("/etc/passwd", folder / "passwd")
    with pytest.raises(inst.InstallError, match="symbolic"):
        inst.install(folder)


def test_uninstall_moves_to_the_bin(monkeypatch: pytest.MonkeyPatch) -> None:
    trashed: list[str] = []
    monkeypatch.setattr(inst, "TRASH", lambda paths: trashed.extend(paths) or [])
    monkeypatch.setattr(inst, "REMOVE_OUTRIGHT", lambda _folder: False)  # as for a real data folder
    res = inst.install(fixture("t.decl"))
    assert res.folder is not None
    ppaths.data_dir("t.decl").mkdir(parents=True)
    inst.uninstall(res.folder, "t.decl", remove_data=True)
    assert trashed == [str(res.folder), str(ppaths.data_dir("t.decl"))]


def test_uninstall_in_a_temporary_data_folder_removes_outright(monkeypatch: pytest.MonkeyPatch) -> None:
    """A trial run's data folder (SPACEKNIFE_HOME under the temporary folder, as in tests): nothing
    goes to the user's recycle bin."""
    trashed: list[str] = []
    monkeypatch.setattr(inst, "TRASH", lambda paths: trashed.extend(paths) or [])
    res = inst.install(fixture("t.decl"))
    assert res.folder is not None and inst.REMOVE_OUTRIGHT(ppaths.plugins_dir())
    ppaths.data_dir("t.decl").mkdir(parents=True)
    inst.uninstall(res.folder, "t.decl", remove_data=True)
    assert trashed == [] and not res.folder.exists() and not ppaths.data_dir("t.decl").exists()


def test_icons_are_cleaned() -> None:
    ok = sanitize_svg(
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M1 1L5 5" stroke="currentColor"/></svg>'
    )
    assert ok.startswith("<svg") and 'stroke="currentColor"' in ok
    for bad in (
        '<svg viewBox="0 0 24 24"><script>alert(1)</script></svg>',
        '<svg viewBox="0 0 24 24"><path d="M1 1" onclick="x()"/></svg>',
        '<svg viewBox="0 0 24 24"><path d="M1 1" fill="red"/></svg>',
        '<svg viewBox="0 0 24 24"><image href="http://x"/></svg>',
        '<svg viewBox="0 0 32 32"><path d="M1 1"/></svg>',
        '<!DOCTYPE x [<!ENTITY e "y">]><svg viewBox="0 0 24 24"/>',
        '<svg viewBox="0 0 24 24"><path d="url(javascript:x)"/></svg>',
    ):
        with pytest.raises(ValueError, match=r"."):
            sanitize_svg(bad)
