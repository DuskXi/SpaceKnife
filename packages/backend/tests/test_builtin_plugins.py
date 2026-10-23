"""The shipped plugins (docs/plugins.md 「内置插件」): ``builtin.export`` and ``builtin.insights`` are
plugin folders declared with the public SDK, and everything they took over behaves as before.

The bytes the exports write are pinned by test_export.py and test_cli_run.py (unchanged by the
move); ``export.targets`` against the list the code before the move produced
(data/export_targets.golden.json, both languages); the rule order built-in → plugins → the user's.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path

import pytest

from spaceknife import i18n, insights
from spaceknife.export import FORMATS, BuiltinInfo, builtin_template, resolve
from spaceknife.extensions import EXTENSIONS
from spaceknife.insights import load_rules
from spaceknife.plugins import builtins
from spaceknife.plugins import paths as ppaths
from spaceknife_client.connection import RemoteError
from spaceknife_protocol import CAP_PLUGINS, KNOWN_CAPABILITIES
from spaceknife_protocol import plugins as pp
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import EXPORT_TARGETS, PLUGIN_DISABLE, PLUGIN_LIST
from spaceknife_sdk.manifest import load_manifest
from spaceknife_sdk.testing import MockHost, make_tree

from .plugin_helpers import connect, plugin_server

GOLDEN = Path(__file__).parent / "data" / "export_targets.golden.json"

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Iterator[None]:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    before = i18n.language()
    yield
    i18n.system_language.cache_clear()
    i18n.set_language(before)


def test_the_shipped_plugins_are_plugin_folders() -> None:
    loaded = builtins.ensure()
    assert sorted(loaded) == ["builtin.export", "builtin.insights"]
    assert builtins.failures() == {}
    for pid, item in loaded.items():
        assert item.folder == ppaths.builtin_dir() / pid
        manifest = load_manifest(item.folder)
        assert manifest.builtin and manifest.id == pid
    kinds = sorted(c.kind for c in EXTENSIONS.owned("builtin.export"))
    assert kinds == ["export_format"] * 3 + ["export_template"] * 5
    assert [c.id for c in EXTENSIONS.owned("builtin.insights")] == ["builtin.insights.builtin"]
    # the formats are the SDK's export_format declarations of plugin.py, called directly
    export = loaded["builtin.export"]
    assert sorted(n for k, n in export.description.handlers) == ["csv", "html", "json"]
    assert [f.plugin for f in FORMATS.values()] == ["builtin.export"] * 3


def test_names_resolve_as_before() -> None:
    assert list(FORMATS) == ["csv", "json", "html"]
    assert resolve("csv") is FORMATS["csv"] and resolve(" JSON ") is FORMATS["json"]
    assert resolve("builtin.export.html") is FORMATS["html"]  # the full id works too
    tpl = builtin_template("simple FILE list")
    assert tpl is not None and tpl.name == "Simple file list"
    assert resolve("builtin.export.simple_file_list") is tpl
    assert FORMATS["csv"].bom and FORMATS["csv"].extension == ".csv"
    assert FORMATS["json"].media_type == "application/json"


@pytest.mark.parametrize("lang", ["zh-CN", "en"])
async def test_export_targets_are_unchanged(lang: str, monkeypatch: pytest.MonkeyPatch) -> None:
    golden = json.loads(GOLDEN.read_text(encoding="utf-8"))[lang]
    monkeypatch.setenv("SPACEKNIFE_LANGUAGE", lang)
    i18n.system_language.cache_clear()  # the server takes its language from there
    async with plugin_server() as srv:
        without = [c for c in KNOWN_CAPABILITIES if c != CAP_PLUGINS]
        for caps in (without, list(KNOWN_CAPABILITIES)):
            async with connect(srv, caps) as conn:
                got = (await conn.call(EXPORT_TARGETS, Empty())).model_dump(mode="json")["targets"]
                assert got == golden


async def test_they_stay_when_a_server_closes_and_cannot_be_turned_off() -> None:
    async with plugin_server() as srv:
        listed = {p.id: p for p in srv.plugins.list_plugins().plugins}
        for pid in ("builtin.export", "builtin.insights"):
            assert listed[pid].builtin and listed[pid].state == "enabled"
        async with connect(srv) as conn:
            ids = [p.id for p in (await conn.call(PLUGIN_LIST, Empty())).plugins]
            assert ids[:2] == ["builtin.export", "builtin.insights"]
            with pytest.raises(RemoteError, match=r"内置插件不能停用|cannot"):
                await conn.call(PLUGIN_DISABLE, pp.PluginIdParams(id="builtin.export"))
        await srv.plugins.restart("builtin.export")  # nothing to restart: no runtime
        assert srv.plugins.records["builtin.export"].runtime is None
    assert resolve("csv") is FORMATS["csv"] and len(insights.active_rules()) > 0


async def test_a_plugins_rules_sit_between_the_builtin_and_the_users(tmp_path: Path) -> None:
    """Built-in → plugins → the user's folder: each later source overrides an earlier rule by id."""
    plugin = tmp_path / "t.rules"
    (plugin / "insights").mkdir(parents=True)
    (plugin / "plugin.toml").write_text(
        'id = "t.rules"\nversion = "0.1.0"\nname = { zh = "规则", en = "Rules" }\n', encoding="utf-8"
    )
    rule = '[[rule]]\nid = "node-modules"\nkind = "build"\ntitle = "{title}"\nreason = "r"\nnames = ["node_modules"]\n'
    (plugin / "insights" / "mine.toml").write_text(rule.format(title="插件的"), encoding="utf-8")
    user = tmp_path / "user"
    user.mkdir()
    async with MockHost.load(plugin) as host:
        host.scan(make_tree({"node_modules": {"a.js": 1}}))
        found = insights.match("/p/node_modules", "dir")
        assert found is not None and found.title == "插件的"  # the plugin's replaces the built-in rule
        (user / "10-mine.toml").write_text(rule.format(title="用户的"), encoding="utf-8")
        load_rules(user, host="linux")
        found = insights.match("/p/node_modules", "dir")
        assert found is not None and found.title == "用户的"  # the user's replaces the plugin's
        (user / "10-mine.toml").write_text(
            '[[rule]]\nid = "node-modules"\nenabled = false\n', encoding="utf-8"
        )
        load_rules(user, host="linux")
        assert insights.match("/p/node_modules", "dir") is None
    load_rules(None)
    found = insights.match("/p/node_modules", "dir")
    assert found is not None and found.rule == "node-modules" and found.title != "插件的"


def test_builtin_info_reads_in_both_languages() -> None:
    from spaceknife.export import BUILTIN_INFO

    info = BUILTIN_INFO["Grouped by folder"]
    assert isinstance(info, BuiltinInfo)
    i18n.set_language("en")
    assert info.title == "Grouped by folder" and info.description.startswith("One section per folder")
    i18n.set_language("zh-CN")
    assert info.title == "按文件夹分组" and info.title_id == "按文件夹分组"
