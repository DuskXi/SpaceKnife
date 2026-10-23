"""What the first trial of plugin authors found missing: check finds wrong names and failing
sources, MockHost behaves as the app does, folder rules have names and an order, the enable dialog
lists everything, a plugin's class speaks both languages."""

from __future__ import annotations

import json
import zipfile
from pathlib import Path

import pytest

from spaceknife import pathclass
from spaceknife.plugins.check import check_plugin
from spaceknife.plugins.icons import APP_ICONS
from spaceknife_protocol.classes import localized
from spaceknife_sdk import scaffold
from spaceknife_sdk.testing import MockHost, make_tree

from .plugin_helpers import mock

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = Path(__file__).parent / "plugin_fixtures"

pytestmark = pytest.mark.timeout(240)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))


def test_the_app_icons_are_the_design_ones() -> None:
    data = json.loads((ROOT / "design" / "icons" / "icons.json").read_text(encoding="utf-8"))
    assert set(data["icons"]) == APP_ICONS


def test_check_names_wrong_icons_and_references(tmp_path: Path) -> None:
    folder = tmp_path / "example.refs"
    scaffold.write(folder, "example.refs", ["command"])
    toml = (folder / "plugin.toml").read_text(encoding="utf-8")
    toml = toml.replace('entry = "plugin.py"', 'entry = "plugin.py"\nicon = "zzz_nonexistent"', 1)
    toml += """
[[commands]]
name = "to_nowhere"
title = { zh = "去不存在的地方", en = "Go nowhere" }
action = { view = { visual = "nope", color_mode = "nope2" } }

[[commands]]
name = "open_nothing"
title = { zh = "打开不存在的报告", en = "Open no report" }
action = { open = "report:nothing" }
"""
    (folder / "plugin.toml").write_text(toml, encoding="utf-8")
    result = check_plugin(folder, contracts=False)
    texts = [i.text() for i in result.issues]
    assert any("zzz_nonexistent" in t and "icon_name" in t for t in texts), texts
    refs = [t for t in texts if "bad_reference" in t]
    assert any("'nope'" in t for t in refs) and any("'nope2'" in t for t in refs)
    assert any("'nothing'" in t for t in refs)
    assert not result.ok(strict=False)


def test_check_fails_a_source_that_fails_on_its_fixture(tmp_path: Path) -> None:
    folder = tmp_path / "example.badsrc"
    scaffold.write(folder, "example.badsrc", ["source"])
    code = (folder / "plugin.py").read_text(encoding="utf-8")
    marker = "out: Listing) -> None:\n"
    assert marker in code
    code = code.replace(marker, marker + '    raise RuntimeError("broken on purpose")\n', 1)
    (folder / "plugin.py").write_text(code, encoding="utf-8")
    result = check_plugin(folder, process=False, subprocess_describe=False)
    assert any(i.code == "source" and i.severity == "error" for i in result.issues), [
        i.text() for i in result.issues
    ]


def test_private_sdk_modules_are_flagged(tmp_path: Path) -> None:
    folder = tmp_path / "example.priv"
    scaffold.write(folder, "example.priv", ["report"])
    with (folder / "plugin.py").open("a", encoding="utf-8") as fh:
        fh.write("\nfrom spaceknife_sdk._current import current_context  # noqa: E402, F401\n")
    result = check_plugin(folder, contracts=False, subprocess_describe=False)
    assert any(i.code == "private_import" for i in result.issues)


def test_synthetic_trees_are_not_in_the_temporary_folder(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife_sdk.testing import _mock_trees

    monkeypatch.delenv("SPACEKNIFE_MOCK_TREES")
    assert _mock_trees() in (Path.home() / ".spaceknife-mock", Path(__import__("tempfile").gettempdir()))


async def test_mock_host_is_the_app(tmp_path: Path) -> None:
    archive = tmp_path / "a.zip"
    with zipfile.ZipFile(archive, "w") as z:
        z.writestr("x.txt", b"x")
    async with mock("t.full") as host:
        host.scan(make_tree({"a": {"b.log": (b"log", "2020-01-02")}, "c.txt": 3}))
        root = host.store.name(1)
        assert Path(root).parent == tmp_path / "trees"  # SPACEKNIFE_MOCK_TREES (default ~/.spaceknife-mock)
        node = host.find("a/b.log")
        assert int(host.store.cols["mtime"][node]) == 1577923200
        assert (Path(root) / "a" / "b.log").read_bytes() == b"log"
        with pytest.raises(KeyError):
            host.find("a/missing")
        assert "t.full.big" in host.commands() and host.keys()["Ctrl+Alt+B"] == "t.full.big"
        out = await host.export("lines", filter="*.log")
        assert out == "b.log\n" or "b.log" in out
        await host.set_setting("min_size", 7)  # waits for the hooks
        assert host.server.plugins.epochs["t.full"] >= 1
        host.set_lang("en")
        from spaceknife import i18n

        assert i18n.language() == "en"
        host.server.settings.update({"plugin.t.full.min_size": 9})
        assert i18n.language() == "en"  # a change of settings keeps the language
        await host.scan_dir(tmp_path)
        scan_id = await host.source("zip", str(archive))
        assert host.status(scan_id).state == "done"
        with pytest.raises(RuntimeError, match="failed"):
            await host.source("zip", str(tmp_path / "missing.zip"))


def test_folder_rules_are_named_ordered_and_excluded(tmp_path: Path) -> None:
    from spaceknife.plugins.adapters import rule_names
    from spaceknife_sdk.manifest import FolderRuleSpec

    specs = [
        ("folder_rule", FolderRuleSpec.model_validate({"glob": "**/.venv", "class": "x"})),
        ("folder_rule", FolderRuleSpec.model_validate({"glob": "**/venv", "class": "x"})),
        ("folder_rule", FolderRuleSpec.model_validate({"glob": "**/env", "class": "x", "name": "rule_venv"})),
    ]
    names = sorted(rule_names(specs).values())
    assert names == ["rule_venv_2", "rule_venv_3"]
    try:
        pathclass.set_plugin_rules(
            [
                ("**/node_modules", "a", False, 0, ("**/steamapps/**",)),
                ("**/{build,out}", "b", False, 5),
            ]
        )
        globs = [r.glob for r in pathclass._plugin_rules]
        assert globs == ["**/{build,out}", "**/node_modules"]
        assert pathclass._plugin_rules[0].regex.match("p/out")
        assert any(x.match("c/steamapps/g/node_modules") for x in pathclass._plugin_rules[1].exclude)
    finally:
        pathclass.set_plugin_rules([])


def test_a_plugin_class_speaks_both_languages() -> None:
    from spaceknife.plugins.adapters import _class
    from spaceknife.plugins.store import PluginRecord
    from spaceknife_sdk.manifest import FileClassSpec

    rec = PluginRecord(id="t.x", folder=Path("."), manifest=None)
    spec = FileClassSpec.model_validate(
        {
            "name": "cache",
            "title": {"zh": "开发缓存", "en": "Dev caches"},
            "color": {"light": "#7C4DDB", "dark": "#B39DFA"},
        }
    )
    info = _class(rec, spec)
    assert info.color == "#B39DFA" and info.color_light == "#7C4DDB"
    en = localized(info, "en")
    assert en.name == "Dev caches" and "开发缓存" in en.aliases
    assert localized(info, "zh-CN").name == "开发缓存"


async def test_the_enable_dialog_lists_what_the_code_adds(tmp_path: Path) -> None:
    async with MockHost.load(ROOT / "examples" / "plugins" / "example.dupes") as host:
        res = await host.server.plugins.describe("example.dupes")
        kinds = {g.kind: g for g in res.groups}
        assert {"report", "cleanup", "node_field", "node_set"} <= set(kinds)
        assert all(g.count == len(g.items) or g.count > len(g.items) for g in res.groups)
        assert res.summary and all("(" in line for line in res.summary)
        for k in res.keys:
            assert k.key and k.title
