"""More of what the first trial of plugin authors found missing: copying text to the clipboard, a
table's next pages, snapshots that list their totals, the roots and paths of a tree."""

from __future__ import annotations

from pathlib import Path

import pytest

from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.gaps"
version = "0.1.0"
name = { zh = "缺口", en = "Gaps" }
entry = "plugin.py"
permissions = ["tree.read", "snapshots.read", "clipboard"]
"""

CODE = """from __future__ import annotations

from typing import Any

from spaceknife_sdk import Context, Insight, Node, Payload, Plugin, Report, ReportContext, T, r

plugin = Plugin()


class Facts(Payload):
    facts: dict[str, Any]


@plugin.method
async def copy(ctx: Context) -> Facts:
    ctx.clipboard.write_text(T("复制的文字", "copied text"))
    return Facts(facts={})


@plugin.method
async def facts(ctx: Context) -> Facts:
    t = ctx.tree
    top = t.roots[0]
    kids = [int(k) for k in t.children(top)]
    snaps = ctx.snapshots.list()
    return Facts(
        facts={
            "roots": t.roots,
            "virtual_top": t.node(0).depth,
            "root_depth": t.node(top).depth,
            "child_depth": t.node(kids[0]).depth,
            "rel": sorted(t.rel_paths(kids)),
            "snap_files": [s.files for s in snaps],
            "snap_alloc": [s.bytes_alloc for s in snaps],
        }
    )


@plugin.node_field(T("文本", "Text"), name="txt", exts=["txt"], slots=["inspector", "hover"])
def txt(ctx: Context, node: Node) -> str:
    return node.name


@plugin.node_field(T("文件夹", "Folder"), name="folder", kinds=["dir"], globs=["**/a"])
def folder(ctx: Context, node: Node) -> str:
    return f"{node.name}:{ctx.slot}"


@plugin.insight(globs=["**/node_modules"])
async def deps(ctx: Context, node: Node) -> Insight | None:
    return Insight(title=T("依赖", "Dependencies"), reason=T("包管理器装的", "Installed by npm"), priority=5)


@plugin.report(T("分页", "Pages"), name="pages")
async def pages(ctx: Context, c: ReportContext) -> Report:
    start = int(c.cursor or 0)
    rows = [{"n": i} for i in range(start, min(start + 2, 5))]
    more = r.more(str(start + 2)) if start + 2 < 5 else None
    return Report(blocks=[r.table([r.col("n", T("号", "No."), "count")], rows=rows, more=more)])
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    folder = tmp_path / "t.gaps"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_text_goes_to_the_clipboard_of_who_asked(plugin: Path, process: bool) -> None:
    # round 2: in a plugin process the clipboard message was not on the wire, the host ended the
    # process for a protocol violation (MockHost ran in process only)
    async with MockHost.load(plugin / "plugin.py", process=process) as host:
        host.scan(make_tree({"a.txt": 1}))
        await host.call("copy")
        await host.call("copy")
        assert host.clipboard == ["复制的文字", "复制的文字"]
        rec = host.server.plugins.records["t.gaps"]
        assert rec.restarts == 0 and rec.state == "enabled"
    async with MockHost.load(plugin / "plugin.py", grant=["tree.read"], process=process) as host:
        host.scan(make_tree({"a.txt": 1}))
        with pytest.raises(Exception, match="clipboard"):
            await host.call("copy")
        assert host.clipboard == []


async def test_a_table_pages_through_its_report(plugin: Path) -> None:
    async with MockHost.load(plugin / "plugin.py") as host:
        host.scan(make_tree({"a.txt": 1}))
        seen: list[int] = []
        report = await host.report("pages")
        while True:
            table = report.blocks[0]
            seen += [int(row.cells["n"]) for row in table.rows]  # type: ignore[union-attr]
            more = table.more  # type: ignore[union-attr]
            if more is None:
                break
            assert more.method == "plugin.t.gaps.pages"  # what the frontend calls with the cursor
            report = await host.report("pages", cursor=more.cursor)
        assert seen == [0, 1, 2, 3, 4]


async def test_trees_and_snapshots_say_what_the_docs_say(plugin: Path) -> None:
    """docs/plugin-api.md: node 0 is the virtual top (depth 0), a root has depth 1; relative paths
    use '/'; a snapshot's header has its totals."""
    async with MockHost.load(plugin / "plugin.py") as host:
        host.scan(make_tree({"a": {"b.txt": 10, "c": {"d.bin": 20}}, "e.log": 5}))
        host.snapshot()
        facts = (await host.call("facts"))["facts"]
        assert facts["roots"] == [1] and facts["virtual_top"] == 0
        assert facts["root_depth"] == 1 and facts["child_depth"] == 2
        assert facts["rel"] == ["a", "e.log"]
        assert facts["snap_files"] == [3] and facts["snap_alloc"] == [35]


async def test_fields_are_asked_only_where_they_apply(plugin: Path) -> None:
    async with MockHost.load(plugin / "plugin.py") as host:
        host.scan(make_tree({"a": {"b.txt": 10, "c.bin": 3}, "d": {"e.TXT": 1}}))

        async def ids(path: str, slot: str = "inspector") -> dict[str, object]:
            return {f.id: f.value for f in await host.fields(slot, node=host.find(path))}

        assert await ids("a/b.txt") == {"t.gaps.txt": "b.txt"}
        assert await ids("d/e.TXT") == {"t.gaps.txt": "e.TXT"}  # extensions in any case
        assert await ids("a/c.bin") == {}
        assert await ids("a") == {"t.gaps.folder": "a:inspector"}  # a folder matching the glob
        assert await ids("d") == {}


def test_a_test_imports_the_plugins_own_code(plugin: Path) -> None:
    from spaceknife_sdk.testing import plugin_module

    mod = plugin_module(plugin / "tests" / "test_x.py")  # any path inside the plugin
    assert mod.txt.__name__ == "txt" and mod.plugin.manifest.id == "t.gaps"
    assert plugin_module(plugin) is mod  # imported once


async def test_insight_cards_come_highest_priority_first(plugin: Path) -> None:
    async with MockHost.load(plugin / "plugin.py") as host:
        host.scan(make_tree({"app": {"node_modules": {"x.js": 10}}}))
        cards = await host.insights(node=host.find("app/node_modules"))
        assert [c.source for c in cards][:1] == ["plugin"] and cards[0].priority == 5
        assert any(c.source == "rule" for c in cards)  # the built-in rule's card, after it
