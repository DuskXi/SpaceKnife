"""Round 4, settings and actions (docs/plugins.md 「第三轮之后的改进」): list and multi-line settings
with declared lengths (``check`` finds a default that is too long), commands greyed for the node a
menu is about (``palette.check_many``), actions with arguments (a cleanup's, a command's, an open
panel's), the ``basket`` hook, and the delete basket counting a file's hard links once where the
scan recorded no file ids."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Any

import pytest

from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

LONG_NOTE = "这是一段比六十四个字长得多的说明，" * 6  # 108 characters: more than a text's 64

TOML = f"""id = "t.r4act"
version = "0.1.0"
name = {{ zh = "第四轮动作", en = "Round four actions" }}
entry = "plugin.py"
permissions = ["tree.read", "basket.propose"]

[[settings]]
name = "rules"
title = {{ zh = "规则", en = "Rules" }}
default = [{{ name = "日志", glob = "*.log", days = 30 }}]
max_items = 3

[[settings.fields]]
name = "name"
title = {{ zh = "名字", en = "Name" }}
default = ""
max_length = 20

[[settings.fields]]
name = "glob"
title = {{ zh = "通配符", en = "Pattern" }}
default = "*"

[[settings.fields]]
name = "days"
title = {{ zh = "天数", en = "Days" }}
default = 30
min = 1
max = 3650

[[settings.fields]]
name = "on"
title = {{ zh = "启用", en = "On" }}
default = true

[[settings]]
name = "note"
title = {{ zh = "备注", en = "Note" }}
control = "textarea"
max_length = 600
default = "{LONG_NOTE}"
"""

CODE = """from __future__ import annotations

import fnmatch
from typing import Any

from spaceknife_sdk import ActionContext, Context, Payload, Plugin, Proposal, Report, ReportContext, T, r

plugin = Plugin()
SEEN = {"basket": 0}


class Seen(Payload):
    seen: dict[str, Any]


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen(seen={"basket": SEEN["basket"], "rules": ctx.settings["rules"], "note": len(ctx.settings["note"])})


@plugin.method
def set_rules(ctx: Context) -> Seen:
    ctx.settings.set("rules", [{"name": "临时", "glob": "*.tmp"}])
    return Seen(seen={"rules": ctx.settings["rules"]})


@plugin.on("basket")
def basket_changed(ctx: Context) -> None:
    SEEN["basket"] += 1


@plugin.command("only_files", T("只对文件", "Files only"), when=["file"], menus=["node", "inspector"])
async def only_files(ctx: Context, c: ActionContext) -> Seen:
    return Seen(seen={"node": c.node, "args": c.args})


@plugin.method
def why_not(ctx: Context, c: ActionContext) -> str:
    return "" if c.node is not None and ctx.tree.node(c.node).name.endswith(".log") else "not a log"


@plugin.command("logs_only", T("只对日志", "Logs only"), check="why_not", menus=["node"])
async def logs_only(ctx: Context, c: ActionContext) -> None:
    pass


@plugin.cleanup(T("按规则清理", "Clean up by a rule"))
def by_rule(ctx: Context, c: ReportContext) -> list[Proposal]:
    glob = str(c.args.get("glob", "*"))
    t = ctx.tree
    out = []
    for n in t.query("", kind="file", columns=()).ids:
        name = t.node(int(n)).name
        if fnmatch.fnmatch(name, glob):
            out.append(Proposal(node=int(n), reason=T("匹配 {g}", "Matches {g}").format(g=glob)))
    return out


plugin.command("clean_logs", T("清理日志", "Clean up logs"), action={"cleanup": "by_rule", "args": {"glob": "*.log"}})
plugin.command("open_rules", T("打开规则", "Open the rules"), action={"open": "report:rules", "args": {"rule": 1}})
plugin.command("export_here", T("导出这里", "Export this"), action={"command": "report.export", "args": {"format": "csv", "scope": "selection"}})


@plugin.report(T("规则", "Rules"), slot="dialog")
def rules(ctx: Context, c: ReportContext) -> Report:
    return Report(blocks=[r.md(f"rule {c.args.get('rule')}")])


@plugin.command("propose", T("放进删除篮", "Into the basket"))
async def propose(ctx: Context, c: ActionContext) -> None:
    t = ctx.tree
    ctx.basket.propose([t.find(t.root_paths[0] + "/a.log")], T("旧日志", "Old log"))
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / "t.r4act"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


TREE = make_tree(
    {"a.log": b"a" * 5000, "b.txt": b"b" * 3000, "sub": {"c.log": b"c" * 2000, "d.bin": b"d" * 100}}
)


# --------------------------------------------------------------------------- gap 3: settings


def test_list_and_textarea_declarations_are_checked() -> None:
    from spaceknife_sdk.manifest import TEXT_MAX, SettingSpec

    s = SettingSpec(
        name="rules",
        title="规则",
        default=[{"glob": "*.log"}],
        fields=[
            {"name": "glob", "title": "通配符", "default": "*"},
            {"name": "days", "title": "天数", "default": 7},
        ],
    )
    assert s.control == "list" and s.items_limit == 50 and s.fields[1].control == "number"
    assert SettingSpec(name="n", title="备注", default="", control="textarea").text_limit == 4096
    assert SettingSpec(name="n", title="名字", default="").text_limit == TEXT_MAX
    bad: list[tuple[dict[str, Any], str]] = [
        ({"default": "x" * 100}, "longer than max_length"),  # a text default over 64: was let through
        ({"default": "x" * 100, "max_length": 50}, "declare max_length = 100"),
        ({"default": [], "control": "list"}, "a list needs fields"),
        (
            {"default": [{"nope": 1}], "fields": [{"name": "a", "title": "a", "default": ""}]},
            "no field 'nope'",
        ),
        (
            {"default": [{"a": 99}], "fields": [{"name": "a", "title": "a", "default": 1, "max": 10}]},
            "outside",
        ),
        (
            {"default": [{}, {}], "max_items": 1, "fields": [{"name": "a", "title": "a", "default": ""}]},
            "max_items",
        ),
        (
            {
                "default": [],
                "fields": [
                    {
                        "name": "a",
                        "title": "a",
                        "default": [],
                        "fields": [{"name": "b", "title": "b", "default": 1}],
                    }
                ],
            },
            "can't hold lists",
        ),
        ({"default": 3, "max_length": 10}, "max_length is for text"),
    ]
    for kw, why in bad:
        with pytest.raises(ValueError, match=why):
            SettingSpec(name="x", title="x", **kw)


def test_check_reports_a_default_longer_than_its_setting_holds(tmp_path: Path) -> None:
    from spaceknife.plugins.check import check_plugin

    folder = tmp_path / "t.long"
    folder.mkdir()
    (folder / "plugin.toml").write_text(
        'id = "t.long"\nversion = "0.1.0"\nname = "Long"\n\n[[settings]]\nname = "rules"\ntitle = "规则"\n'
        f'default = "{"很长的规则" * 30}"\n',
        encoding="utf-8",
    )
    result = check_plugin(folder, contracts=False)
    assert any(
        "longer than max_length" in i.text() and "max_length = 150" in i.text() for i in result.errors()
    )
    (folder / "plugin.toml").write_text(
        (folder / "plugin.toml").read_text(encoding="utf-8") + 'control = "textarea"\n', encoding="utf-8"
    )
    assert check_plugin(folder, contracts=False).ok()


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_list_and_textarea_settings_end_to_end(plugin: Path, process: bool) -> None:
    from spaceknife import settings as settings_mod
    from spaceknife.settings import SettingsError

    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        await host.settled()
        meta = {m.key: m for m in settings_mod.meta() if m.plugin == "t.r4act"}
        rules, note = meta["plugin.t.r4act.rules"], meta["plugin.t.r4act.note"]
        assert rules.control == "list" and rules.max_items == 3
        assert [f.name for f in rules.fields] == ["name", "glob", "days", "on"]
        assert [f.control for f in rules.fields] == ["text", "text", "number", "switch"]
        assert rules.fields[0].max_length == 20 and rules.fields[2].max == 3650
        assert note.control == "textarea" and note.max_length == 600
        # the plugin sees the defaults, missing fields filled in
        seen = (await host.call("seen"))["seen"]
        assert seen["rules"] == [{"name": "日志", "glob": "*.log", "days": 30, "on": True}]
        assert seen["note"] == len(LONG_NOTE)
        # a valid list goes through; the plugin has it on its next call
        store = host.server.settings
        store.update(
            {
                "plugin.t.r4act.rules": [
                    {"name": "a", "glob": "*.bak", "days": 7},
                    {"glob": "*.old", "on": False},
                ]
            }
        )
        assert store.get_value("plugin.t.r4act.rules")[1] == {
            "name": "",
            "glob": "*.old",
            "days": 30,
            "on": False,
        }
        await host.set_setting("note", "第一行\n第二行")
        seen = (await host.call("seen"))["seen"]
        assert [x["glob"] for x in seen["rules"]] == ["*.bak", "*.old"] and seen["note"] == 7
        # what does not fit is refused, the value stays
        for bad, why in [
            ([{"glob": "*"}] * 4, "最多 3 项"),
            ([{"colour": "red"}], "没有「colour」这一栏"),
            ([{"days": 0}], "第 1 项"),
            ([{"days": "7"}], "天数"),
            ([{"name": "n" * 21}], "名字"),
            ("*.log", "应是一个列表"),
        ]:
            with pytest.raises(SettingsError, match=why):
                store.update({"plugin.t.r4act.rules": bad})
        with pytest.raises(SettingsError):
            store.update({"plugin.t.r4act.note": "x" * 601})
        assert len(store.get_value("plugin.t.r4act.rules")) == 2
        # the plugin changes its own list
        assert (await host.call("set_rules"))["seen"]["rules"][0]["glob"] == "*.tmp"
        assert store.get_value("plugin.t.r4act.rules") == [
            {"name": "临时", "glob": "*.tmp", "days": 30, "on": True}
        ]


# --------------------------------------------------------------------------- gap 6: greyed commands


async def test_commands_are_checked_for_the_node_a_menu_is_about(plugin: Path) -> None:
    from spaceknife_protocol import palette as pp
    from spaceknife_protocol import view as pv

    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        await host._ready()
        assert host.scan_id is not None
        vhost = host.server.views.open(
            host.session, pv.ViewOpenParams(scan_id=host.scan_id, viewport=pv.Viewport(width=600, height=400))
        )
        view = vhost.view
        view.selected = host.find("a.log")  # the selection is a file …
        route = host.server.router.get("palette.check_many")
        assert route is not None
        ids = ["t.r4act.only_files", "t.r4act.logs_only"]

        async def states(node: int | None) -> dict[str, tuple[bool, str]]:
            ctx = pp.PaletteContext(tab="scan", scan_id=host.scan_id, view_id=view.id, node=node)
            got = await route.handler(host.session, pp.PaletteCheckManyParams(ids=ids, context=ctx))
            return {c.id: (c.enabled, c.status) for c in got.commands}

        # … but the menu was opened on a folder: the file-only command is greyed, with the reason
        on_dir = await states(host.find("sub"))
        assert on_dir["t.r4act.only_files"] == (False, "只用于文件")
        assert on_dir["t.r4act.logs_only"] == (False, "not a log")  # its check method, for that node
        on_txt = await states(host.find("b.txt"))
        assert on_txt["t.r4act.only_files"][0] and on_txt["t.r4act.logs_only"] == (False, "not a log")
        assert all(enabled for enabled, _ in (await states(host.find("sub/c.log"))).values())
        assert all(enabled for enabled, _ in (await states(None)).values())  # no node: the selection
        host.server.views.close(view.id)


# --------------------------------------------------------------------------- gap 10: arguments


def test_actions_take_arguments() -> None:
    from spaceknife_sdk.actions import ActionError, normalize_action, resolve_action

    a = normalize_action({"cleanup": "by_rule", "args": {"glob": "*.log"}})
    assert a == {"kind": "cleanup", "cleanup": "by_rule", "args": {"glob": "*.log"}}
    wired = resolve_action(a, "a.b")
    assert (
        wired.kind == "cleanup" and wired.method == "plugin.a.b.by_rule" and wired.args == {"glob": "*.log"}
    )
    with pytest.raises(ActionError, match="unknown keys"):
        normalize_action({"cleanup": "by_rule", "params": {}})


async def test_actions_with_arguments_end_to_end(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        # a cleanup action carries its args into ReportContext.args
        logs = await host.command("clean_logs")
        assert sorted(p.path.rsplit("/", 1)[-1].rsplit(os.sep, 1)[-1] for p in logs) == ["a.log", "c.log"]
        # args given when it runs (a panel's sk.host.command(id, args)) are added to the action's own
        bins = await host.command("clean_logs", glob="*.bin")
        assert [p.path.rsplit(os.sep, 1)[-1].rsplit("/", 1)[-1] for p in bins] == ["d.bin"]
        # open and command actions are returned with the args they carry
        opened = await host.command("open_rules", rule=2)
        assert opened["target"] == "report:t.r4act.rules" and opened["args"] == {"rule": 2}
        exported = await host.command("export_here")
        assert exported["command"] == "report.export" and exported["args"]["scope"] == "selection"
        # a report gets the args it was opened with
        assert "rule 2" in host.text(await host.report("rules", args={"rule": 2}))
        # a decorated command's call gets the node and the args
        seen = (await host.command("only_files", node=host.find("a.log"), extra=1))["seen"]
        assert seen == {"node": host.find("a.log"), "args": {"extra": 1}}


# --------------------------------------------------------------------------- gap 13: the basket hook


def test_hook_names_are_checked(tmp_path: Path) -> None:
    from spaceknife.plugins.check import check_plugin
    from spaceknife_sdk.manifest import HOOKS, HookSpec

    assert "basket" in HOOKS and HookSpec(name="b", event="basket").event == "basket"  # was refused
    folder = tmp_path / "t.hooks"
    folder.mkdir()
    (folder / "plugin.toml").write_text(
        'id = "t.hooks"\nversion = "0.1.0"\nname = "Hooks"\nentry = "plugin.py"\n', encoding="utf-8"
    )
    (folder / "plugin.py").write_text(
        "from spaceknife_sdk import Context, Plugin\n\nplugin = Plugin()\n\n\n"
        '@plugin.on("basket")\ndef ok(ctx: Context) -> None:\n    pass\n\n\n'
        '@plugin.on("baskets")\ndef typo(ctx: Context) -> None:\n    pass\n',
        encoding="utf-8",
    )
    result = check_plugin(folder, contracts=False)
    hooks = [i for i in result.issues if i.code == "unknown_hook"]
    assert len(hooks) == 1 and hooks[0].line == 11 and hooks[0].file == "plugin.py"
    assert "did you mean 'basket'" in hooks[0].fix
    # found from the code even when importing the plugin fails for another reason first
    text = (folder / "plugin.py").read_text(encoding="utf-8")
    (folder / "plugin.py").write_text("import nonexistent_module_xyz\n" + text, encoding="utf-8")
    result = check_plugin(folder, contracts=False)
    assert [i.line for i in result.issues if i.code == "unknown_hook"] == [12]


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_the_basket_hook_runs(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        await host.command("propose")
        assert [i.reason for i in host.basket] == ["旧日志"]

        async def heard(at_least: int) -> int:
            for _ in range(100):
                await asyncio.sleep(0.05)
                await host.settled()
                n = int((await host.call("seen"))["seen"]["basket"])
                if n >= at_least:
                    return n
            raise AssertionError("the basket hook did not run")

        first = await heard(1)
        host.server.ops.basket.clear()  # the user empties the basket
        await host.server.ops.broadcast()
        assert await heard(first + 1) > first


# --------------------------------------------------------------------------- gap 17: file ids


def test_basket_counts_hard_links_once_without_file_ids_from_the_scan(tmp_path: Path) -> None:
    """A hard scan on Windows records no file ids: the basket asks the system for them, so two names
    of one file free its space once (they showed twice before)."""
    from spaceknife.ops.basket import Basket

    from .store_builder import build, find

    root = tmp_path / "real"
    (root / "a").mkdir(parents=True)
    (root / "b").mkdir()
    (root / "a" / "x.dat").write_bytes(b"x" * 8192)
    try:
        os.link(root / "a" / "x.dat", root / "b" / "y.dat")
    except OSError as exc:
        pytest.skip(f"no hard links here: {exc}")
    store = build({"a": {"x.dat": 8192}, "b": {"y.dat": 8192}}, root=str(root), cluster=4096)
    x, y = find(store, "a/x.dat"), find(store, "b/y.dat")
    assert int(store.cols["ino"][x]) == 0 and int(store.cols["nlink"][x]) <= 1  # as such a scan leaves them
    basket = Basket(volume_of=lambda _root: "VOL")
    xid = basket.add(store, "s", [x]).added[0]
    item = basket.get(xid)
    assert item is not None and item.alloc == 0 and "硬链接" in item.note  # its other name stays
    basket.add(store, "s", [y])
    assert basket.freeable() == {"VOL": 8192}  # one file, not two
    folders = Basket(volume_of=lambda _root: "VOL")
    folders.add(store, "s", [find(store, "a"), find(store, "b")])
    assert folders.freeable() == {"VOL": 8192}
