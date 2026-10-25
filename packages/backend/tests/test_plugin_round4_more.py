"""Round 4, the rest of core agent 5's group (docs/plugins.md 「第三轮之后的改进」): insight rules that
look at a folder's neighbours and contents and say how a thing comes back (gap 12), a code insight
about a folder above the node, the folders a plugin's folder rule decided and the details panel
naming that rule; a field answer invalidated while it was worked out is asked again, nested texts
follow each language (gap 19); a query that is cut short says so."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.r4ins"
version = "0.1.0"
name = { zh = "第四轮洞察", en = "Round four insights" }
entry = "plugin.py"
permissions = ["tree.read"]

[[folder_rules]]
name = "nm"
glob = "**/node_modules"
class = "data"
beside = ["package.json"]
"""

RULES = """version = 1

[[rule]]
id = "r4-node-modules"
kind = "build"
title = "项目的依赖"
cleanup = "safe"
reason = "项目装的依赖包。"
regenerate = "在项目里运行 npm install 会装回来"
names = ["node_modules"]
beside = ["package.json"]
priority = 3
i18n.en.title = "A project's dependencies"
i18n.en.reason = "The packages a project installed."
i18n.en.regenerate = "npm install in the project puts them back"

[[rule]]
id = "r4-venv"
kind = "build"
title = "虚拟环境"
cleanup = "safe"
reason = "Python 虚拟环境。"
paths = ["**/*"]
node = "dir"
contains = ["pyvenv.cfg"]
priority = 3
"""

CODE = """from __future__ import annotations

import asyncio
import time

from collections.abc import Iterator

import numpy as np

from spaceknife_sdk import ColorInput, Context, ExportInput, FieldValue, Insight, Node, Payload, Plugin, T
from spaceknife_sdk.manifest import SettingSpec

plugin = Plugin()
CALLS = {"field": 0, "late": 0}


@plugin.color_mode("late", T("迟到", "Late"), palette=["muted", "ok"])
def late(c: ColorInput) -> np.ndarray:
    CALLS["late"] += 1
    if CALLS["late"] == 1:
        time.sleep(0.6)  # as a first frame that maps the tree's columns first
    return np.ones(len(c.ids), dtype=np.uint8)


@plugin.insight(globs=["**/proj/src/*"])
def about_project(ctx: Context, node: Node) -> Insight | None:
    return Insight(
        title=T("项目的源代码", "The project's source"),
        reason=T("这个文件属于项目 proj。", "This file belongs to the project proj."),
        regenerate=T("从版本库重新取", "Check it out again"),
        node=ctx.tree.node(node.parent).parent,  # src is in proj: the card is about proj
    )


@plugin.node_field(T("慢字段", "Slow field"))
async def slow(ctx: Context, node: Node) -> FieldValue:
    CALLS["field"] += 1
    n = CALLS["field"]
    await asyncio.sleep(0.3)
    return FieldValue(value=f"answer {n}")


class Folders(Payload):
    names: list[str]
    truncated: bool
    total: int


@plugin.method
def folders(ctx: Context) -> Folders:
    ids = ctx.tree.rule_folders("nm")
    rows = ctx.tree.query(kind="file", limit=2)
    return Folders(names=[ctx.tree.rel_path(int(i)) for i in ids], truncated=rows.truncated, total=rows.total)


@plugin.export_format(
    "cols",
    T("列", "Columns"),
    extension="txt",
    options=[
        SettingSpec(
            name="cols",
            title=T("列", "Columns"),
            default="name",
            control="multi",
            options=[("name", T("名字", "Name")), ("size", T("大小", "Size"))],
        ),
        SettingSpec(name="depth", title=T("层数", "Levels"), default=1, min=1, max=5),
    ],
)
def cols(x: ExportInput) -> Iterator[str]:
    yield f"{x.options.get('cols')}|{x.options.get('depth')}"


@plugin.method
def query_all(ctx: Context) -> Folders:
    rows = ctx.tree.query(kind="file")  # no limit: the default one
    return Folders(names=[], truncated=rows.truncated, total=rows.total)
"""

TREE = make_tree(
    {
        "proj": {
            "package.json": 90,
            "node_modules": {"x.js": 4000},
            "src": {"a.ts": 300},
            "env": {"pyvenv.cfg": 10, "lib": {"m.py": 100}},
        },
        "npm-global": {"node_modules": {"y.js": 5000}},  # no package.json beside it
    }
)


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / "t.r4ins"
    (folder / "insights").mkdir(parents=True)
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    (folder / "insights" / "rules.toml").write_text(RULES, encoding="utf-8")
    return folder


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_insight_rules_look_beside_and_inside(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        cards = await host.insights(node=host.find("proj/node_modules"))
        mine = [c for c in cards if c.rule == "r4-node-modules"]
        assert mine and mine[0].regenerate == "在项目里运行 npm install 会装回来"
        inner = await host.insights(node=host.find("proj/node_modules/x.js"))
        assert (
            inner[0].rule == "r4-node-modules"
            and inner[0].inside
            and inner[0].path == host.path("proj/node_modules")
        )
        # no package.json beside it: not this rule (the built-in one may still say something)
        assert all(
            c.rule != "r4-node-modules"
            for c in await host.insights(node=host.find("npm-global/node_modules"))
        )
        assert [c.rule for c in await host.insights(node=host.find("proj/env"))][:1] == ["r4-venv"]
        assert all(c.rule != "r4-venv" for c in await host.insights(node=host.find("proj/src")))
        await host.set_lang("en")
        cards = await host.insights(node=host.find("proj/node_modules"))
        assert next(c for c in cards if c.rule == "r4-node-modules").regenerate.startswith("npm install")


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_code_card_about_a_folder_above(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        cards = await host.insights(node=host.find("proj/src/a.ts"))
        card = next(c for c in cards if c.source == "plugin")
        assert card.path == host.path("proj") and card.inside and card.regenerate == "从版本库重新取"


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_the_folders_a_rule_decided_and_why_a_folder_is_classed(plugin: Path, process: bool) -> None:
    from spaceknife import pathclass
    from spaceknife.layout.basis import Basis

    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        got = await host.call("folders")
        assert got["names"] == ["proj/node_modules"]  # npm-global's has no package.json beside it
        assert got["truncated"] is True and got["total"] == 6
        why = pathclass.explain(host.store, host.find("proj/node_modules/x.js"), Basis(host.store).classes)
        assert why is not None and why.rule == "t.r4ins.nm" and why.class_id == "data"
        assert "**/node_modules" in why.reason


async def test_a_query_cut_short_by_the_default_limit_says_so(
    plugin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from loguru import logger

    import spaceknife_sdk.tree as sdk_tree

    monkeypatch.setattr(sdk_tree, "QUERY_LIMIT", 3)
    lines: list[str] = []
    sink = logger.add(lambda m: lines.append(str(m)), level="WARNING")
    try:
        async with MockHost.load(plugin) as host:
            host.scan(TREE)
            got = await host.call("query_all")
            assert got["truncated"] is True and got["total"] == 6
            await host.settled()
        assert any("matched 6 rows; only the first 3" in line for line in lines)
    finally:
        logger.remove(sink)


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_field_invalidated_while_it_is_worked_out_is_asked_again(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        node = host.find("proj/package.json")
        await host.settled()
        asked = asyncio.ensure_future(host.fields("inspector", node=node))
        await asyncio.sleep(0.15)
        host.server.plugins.invalidate("t.r4ins")  # ctx.invalidate / new settings, meanwhile
        fields = await asked
        # before: the stale answer was dropped and the details panel got nothing
        assert [f.value for f in fields if f.id.endswith("slow")] == ["answer 2"]


async def test_export_option_values_are_checked(plugin: Path) -> None:
    from spaceknife.server.rpc import RpcError

    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        assert await host.export_text("cols", options={"cols": "size,name", "depth": 3}) == "name,size|3"
        for bad, why in (
            ({"cols": "name,bogus"}, "bogus"),  # round 3: an unknown column silently gave an empty one
            ({"depth": 9}, "depth"),
            ({"colz": "name"}, "colz"),
        ):
            with pytest.raises(RpcError, match=why):
                await host.export_text("cols", options=bad)


async def test_a_colour_mode_that_missed_its_first_frame_is_drawn_again(
    plugin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.plugins import host as host_module

    # the app's 20 ms, made longer: a loaded test machine answers the quick second call in time
    monkeypatch.setattr(host_module, "COLOR_TIMEOUT", 0.3)
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        await host.settled()
        asked: list[str] = []

        async def repaint(mode: str) -> None:
            asked.append(mode)

        host.server.views.repaint_color = repaint  # what the views would do: draw again
        first = await host.color("late")
        assert first.pal is not None and not first.pal.any()  # the time missed: neutral
        await asyncio.sleep(1.2)
        assert asked == ["t.r4ins.late"]  # before: nothing asked again, it stayed neutral
        again = await host.color("late")
        assert again.pal is not None and again.pal.any()


def test_shot_steps_are_read_in_order_and_checked_before_anything_starts() -> None:
    from spaceknife.devshot import StepError, _key, _split_set, parse_step

    assert parse_step("click:text=应用") == (False, "click", "text=应用")
    assert parse_step("rclick:.treemap") == (False, "rclick", ".treemap")
    assert parse_step("panel:set:#days=30") == (True, "set", "#days=30")
    assert parse_step("eval:document.title") == (False, "eval", "document.title")
    assert _split_set('input[name="a=b"]=5') == ('input[name="a=b"]', "5")
    assert _split_set("#q=a=b") == ("#q", "a=b")
    assert _key("Control+K")[1:3] == (ord("K"), 2) and _key("Escape")[1] == 27 and _key("a")[3] == "a"
    for bad in ("tap:x", "click", "panel:select:a", "set:novalue", "wait:soon"):
        with pytest.raises(StepError):
            parse_step(bad)
    with pytest.raises(StepError):
        _key("Hyper+K")


def test_a_nested_text_is_written_in_each_language() -> None:
    from spaceknife_protocol.text import T

    t = T("{n} 个在{where}", "{n} in {where}").format(n=3, where=T("回收站", "the bin"))
    assert t.zh == "3 个在回收站" and t.en == "3 in the bin"
    assert T("{x:size}", "{x:size}").format(x=2048).en == "2.0 KB"
