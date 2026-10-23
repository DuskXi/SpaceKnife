"""``spaceknife plugin new``: a plugin skeleton that works as it is.

Every part (``--with report,command,…``) adds a commented example to ``plugin.toml``, ``plugin.py``
and ``tests/test_plugin.py``; the result passes ``plugin check --strict`` and its own tests before
anything is changed (``packages/backend/tests/test_plugin_cli.py`` builds every part). Edit from
there: docs/plugin-dev.md explains each part under the same name.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from pathlib import Path

from spaceknife_protocol.plugins import valid_plugin_id

DEFAULT = ("report", "command")


@dataclass(frozen=True, slots=True)
class Part:
    """One ``--with`` part: what it adds to each file and the permissions it needs."""

    toml: str = ""
    imports: tuple[str, ...] = ()
    code: str = ""
    test: str = ""
    permissions: tuple[str, ...] = ()
    files: dict[str, str] = field(default_factory=dict)
    needs_code: bool = True
    doc: str = ""  # the section of docs/plugin-dev.md


PARTS: dict[str, Part] = {
    "method": Part(
        imports=("Context", "Params", "Payload"),
        code='''
class HelloParams(Params):
    name: str = "world"


class HelloResult(Payload):
    text: str


@plugin.method
async def hello(ctx: Context, p: HelloParams) -> HelloResult:
    """Greets someone (a method: plugin.%ID%.hello, for panels and commands)."""
    return HelloResult(text=ctx.t("你好，{name}", "Hello, {name}", name=p.name))
''',
        test="""
async def test_hello(host: MockHost) -> None:
    host.set_lang("en")
    assert (await host.call("hello", name="SpaceKnife"))["text"] == "Hello, SpaceKnife"
""",
        doc="kind-method",
    ),
    "report": Part(
        toml="""
# A report needs no declaration here: @plugin.report in plugin.py declares it.
""",
        imports=("Context", "Report", "ReportContext", "T", "r"),
        code='''
@plugin.report(T("最大的文件", "Largest files"), icon="gauge", refresh=["scan"])
async def largest(ctx: Context, c: ReportContext) -> Report:
    """The largest files of the scan, as a table (opens as a tab)."""
    min_size = %MIN_SIZE%
    rows = ctx.tree.query(kind="file", min_size=min_size, columns=("bytes", "path"), sort="-bytes", limit=50)
    total = int(rows["bytes"].sum())
    return Report(
        title=T("最大的文件", "Largest files"),
        blocks=[
            r.stats([(T("文件", "Files"), len(rows), "count"), (T("合计", "Total"), total, "size")]),
            r.table(
                [r.col("bytes", T("占用", "Size"), "size"), r.col("path", T("位置", "Location"), "path")],
                rows=rows,
            ),
        ],
    )
''',
        test="""
async def test_largest(host: MockHost) -> None:
    report = await host.report("largest")
    assert "big.iso" in host.text(report)  # host.text: the report as plain text
""",
        permissions=("tree.read",),
        doc="kind-report",
    ),
    "command": Part(
        imports=("ActionContext", "Context", "T", "Toast"),
        code='''
@plugin.command("greet", T("问好", "Say hello"), icon="info", menus=["node"])
async def greet(ctx: Context, a: ActionContext) -> Toast:
    """A command: in the palette and in the right-click menu of a node (menus=["node"])."""
    return Toast(text=T("你好", "Hello"), tone="ok")
''',
        test="""
async def test_greet(host: MockHost) -> None:
    toast = await host.call("greet", node=host.find("notes.txt"))
    assert toast["tone"] == "ok"
""",
        doc="kind-command",
    ),
    "setting": Part(
        toml="""
[[settings]]                  # 设置 › 插件 shows it; read it with ctx.settings["min_size"]
name = "min_size"
title = { zh = "最小大小", en = "Minimum size" }
description = { zh = "只列出不小于它的文件。", en = "Only files at least this large." }
control = "number"
default = 1048576
unit = "B"
""",
        test="""
def test_min_size_has_a_default(host: MockHost) -> None:
    host.set_setting("min_size", 1)  # a test may change it
""",
        needs_code=False,
        doc="kind-setting",
    ),
    "cleanup": Part(
        imports=("Context", "Proposal", "ReportContext", "T"),
        code='''
@plugin.cleanup(T("清理日志文件", "Clean up log files"), name="logs")
async def logs(ctx: Context, c: ReportContext) -> list[Proposal]:
    """Candidates for the delete basket: the user reviews them first, then only the basket deletes."""
    rows = ctx.tree.query("*.log", kind="file", columns=("path",))
    return [Proposal(node=int(i), reason=T("日志文件", "A log file")) for i in rows.ids]
''',
        test="""
async def test_logs(host: MockHost) -> None:
    proposed = await host.cleanup("logs")
    assert [p.node for p in proposed] == [host.find("logs/app.log")]
""",
        permissions=("tree.read",),
        doc="kind-cleanup",
    ),
    "panel": Part(
        toml="""
[[panels]]                    # a web page in a sandboxed frame; it talks to the app through sk
name = "main"
title = { zh = "面板", en = "Panel" }
icon = "gauge"
entry = "web/index.html"
slot = "tab"
""",
        imports=("Context", "Payload"),
        code='''
class Counts(Payload):
    files: int
    folders: int


@plugin.method
async def counts(ctx: Context) -> Counts:
    """What the panel shows (web/app.js calls it: sk.call("counts"))."""
    kind = ctx.tree.col("kind")
    return Counts(files=int((kind == 0).sum()), folders=int((kind == 1).sum()))
''',
        test="""
async def test_counts(host: MockHost) -> None:
    got = await host.call("counts")
    assert got["files"] == 5
    assert got["folders"] >= 2
""",
        permissions=("tree.read",),
        files={
            "web/index.html": """<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <link rel="stylesheet" href="./_sk/base.css" />
  </head>
  <body>
    <h2 id="title"></h2>
    <p class="card"><span id="files">…</span> · <span id="folders">…</span></p>
    <script type="module" src="./app.js"></script>
  </body>
</html>
""",
            "web/app.js": """// The panel. sk is the app's bridge (docs/plugin-dev.md 「Web 面板」);
// its types for this plugin are in sk-plugin.d.ts (plugin build writes it).
import sk from "./_sk/bridge.js";

const t = (zh, en) => (sk.lang.startsWith("zh") ? zh : en);

async function show() {
  document.getElementById("title").textContent = t("文件和文件夹", "Files and folders");
  const c = await sk.call("counts");
  document.getElementById("files").textContent = `${c.files} ${t("个文件", "files")}`;
  document.getElementById("folders").textContent = `${c.folders} ${t("个文件夹", "folders")}`;
}

sk.ready.then(show);
sk.onContext(show); // another scan or selection
""",
            "web/fixtures.json": """{
  "lang": "zh-CN",
  "calls": { "counts": { "files": 5, "folders": 3 } }
}
""",
        },
        doc="kind-panel",
    ),
    "visual": Part(
        toml="""
[[visualizations]]            # the flame graph sized by file counts instead of bytes
name = "by_count"
title = { zh = "按文件数的火焰图", en = "Flame graph by file count" }
base = "flame"
size = "files"
""",
        test="""
async def test_by_count(host: MockHost) -> None:
    frame = await host.visual("by_count")  # checked against the row contract
    assert len(frame.draw) > 0
""",
        needs_code=False,
        doc="kind-visualization",
    ),
    "color": Part(
        toml="""
[[color_modes]]               # colours by modification time; palette entry 0 is the neutral one
name = "age"
title = { zh = "按修改时间", en = "By age" }
by = "age"
bands = ["30d", "1y"]
palette = ["muted", "ok", "warn", "danger"]
legend = [
  { zh = "其他", en = "Other" },
  { zh = "一个月内", en = "This month" },
  { zh = "一年内", en = "This year" },
  { zh = "更早", en = "Older" },
]
""",
        test="""
async def test_age_colours(host: MockHost) -> None:
    frame = await host.color("age")
    assert frame.pal is not None
    assert int(frame.pal.max()) <= 3
""",
        needs_code=False,
        doc="kind-color_mode",
    ),
    "export": Part(
        imports=("ExportInput", "T"),
        code='''
@plugin.export_format(
    "md",
    T("Markdown 表格", "Markdown table"),
    extension="md",
    media_type="text/markdown",
    options=[{"name": "limit", "title": T("最多几行", "At most"), "default": 1000, "min": 1}],
)
def md(x: ExportInput) -> Iterator[str]:
    """Every file below ``x.root`` (0: everything), the largest first; ``x.match``: the filtered
    ones; ``x.options``: what the export dialog's fields say."""
    under = x.root or None
    limit = int(x.options.get("limit", 1000))
    rows = x.tree.query(kind="file", under=under, columns=("bytes", "path"), sort="-bytes", limit=100_000)
    yield "| Size | Path |\\n|---:|---|\\n"
    shown = 0
    for node, size, path in zip(rows.ids.tolist(), rows["bytes"].tolist(), rows.paths, strict=True):
        if x.match is None or x.match[node]:
            yield f"| {size} | {path} |\\n"
            shown += 1
            if shown >= limit:
                break
''',
        test="""
async def test_md_export(host: MockHost) -> None:
    out = await host.export("md")
    assert isinstance(out, str)
    assert out.splitlines()[2].endswith("big.iso |")
""",
        permissions=("tree.read",),
        doc="kind-export_format",
    ),
    "source": Part(
        imports=("Context", "Listing", "T"),
        code='''
@plugin.scan_source("list", T("文件清单", "File list"), pick="file", extensions=[".txt"])
def file_list(ctx: Context, uri: str, out: Listing) -> None:
    """Scans a text file of "<size> <path>" lines as if it were a folder (a plain def may block)."""
    for line in Path(ctx.files.open_path(uri)).read_text(encoding="utf-8").splitlines():
        size, _, path = line.strip().partition(" ")
        if path:
            out.file(path, size=int(size))
    out.root_name(Path(uri).stem)
''',
        test="""
async def test_file_list(host: MockHost, tmp_path: Path) -> None:
    listing = tmp_path / "list.txt"
    listing.write_text("100 a/b.txt\\n5 c.txt\\n", encoding="utf-8")
    await host.source("list", str(listing))
    assert host.store.name(host.find("a/b.txt")) == "b.txt"
""",
        permissions=("files.read",),
        files={"fixtures/list.txt": "100 docs/a.txt\n5 b.txt\n"},
        doc="kind-scan_source",
    ),
    "set": Part(
        imports=("Context", "T", "TreeView"),
        code='''
@plugin.node_set("big", T("大文件", "Big files"))
async def big(ctx: Context, tree: TreeView) -> np.ndarray:
    """A word for the filter box: ":set:big" matches these files."""
    return tree.query(kind="file", min_size=1 << 20, columns=()).ids
''',
        test="""
async def test_big_set(host: MockHost) -> None:
    assert await host.filter(":set:big") == ["big.iso"]
""",
        permissions=("tree.read",),
        doc="kind-node_set",
    ),
    "field": Part(
        imports=("Context", "FieldValue", "Node", "T"),
        code='''
@plugin.node_field(T("名字长度", "Name length"), slots=["inspector", "hover"])
async def name_length(ctx: Context, node: Node) -> FieldValue | None:
    """A line of the details panel (and the hover card) for the selected node."""
    return FieldValue(value=len(node.name), unit="count")
''',
        test="""
async def test_name_length(host: MockHost) -> None:
    fields = await host.fields(node=host.find("notes.txt"))
    assert [f.value for f in fields] == [9]
""",
        permissions=("tree.read",),
        doc="kind-node_field",
    ),
    "insight": Part(
        imports=("Context", "Insight", "Node", "T"),
        code='''
@plugin.insight
async def backups(ctx: Context, node: Node) -> Insight | None:
    """The insight card of a node: what it is and whether it can go."""
    if node.is_file and node.name.endswith(".bak"):
        return Insight(
            title=T("备份文件", "A backup"),
            reason=T("编辑器留下的备份", "Left by an editor"),
            cleanup="review",
        )
    return None
''',
        test="""
async def test_backup_insight(host: MockHost) -> None:
    cards = await host.insights(node=host.find("old.bak"))
    assert [c.cleanup for c in cards if c.plugin == host.plugin_id] == ["review"]
""",
        permissions=("tree.read",),
        doc="kind-insight",
    ),
    "hook": Part(
        imports=("Context", "ScanInfo", "T"),
        code='''
@plugin.on("scan_done")
async def scanned(ctx: Context, scan: ScanInfo) -> None:
    """Runs when a scan finishes (also: start, stop, scan_closed, settings, lang)."""
    ctx.toast(T("扫描完成：{items} 项", "Scanned {items} items").format(items=scan.items), tone="ok")
''',
        test="""
async def test_scan_done_hook(host: MockHost) -> None:
    await host.hook("scan_done", scan={"scan_id": host.scan_id, "items": 7})
    assert host.toasts[-1] in ("扫描完成：7 项", "Scanned 7 items")
""",
        doc="kind-hook",
    ),
}
ALIASES = {
    "visualization": "visual",
    "colour": "color",
    "color_mode": "color",
    "node_set": "set",
    "node_field": "field",
}


def parts_of(names: Iterable[str]) -> list[str]:
    out: list[str] = []
    for raw in names:
        for name in (n.strip().lower() for n in raw.split(",")):
            if not name:
                continue
            name = ALIASES.get(name, name)
            if name not in PARTS:
                raise ValueError(f"unknown part {name!r}: one of {', '.join(PARTS)}")
            if name not in out:
                out.append(name)
    return out


def _title_of(plugin_id: str) -> str:
    last = plugin_id.rsplit(".", 1)[-1]
    return " ".join(w.capitalize() for w in re.split(r"[-_]", last) if w) or plugin_id


TREE = [
    '"big.iso": 4 << 20,',
    '"notes.txt": (2000, "2020-01-01"),',
    '"old.bak": 300,',
    '"logs": {"app.log": 1200},',
    '"docs": {"readme.md": 50},',
]


def render(plugin_id: str, parts: Sequence[str], *, title: str | None = None) -> dict[str, str]:
    """The files of a new plugin (relative path -> text)."""
    if not valid_plugin_id(plugin_id):
        raise ValueError(
            f"{plugin_id!r} is not a plugin id: two or more dot-separated lowercase parts, e.g. "
            "example.hello (and not starting with builtin, sk, spaceknife, plugin or an app namespace)"
        )
    chosen = parts_of(parts) if parts else list(DEFAULT)
    items = [PARTS[p] for p in chosen]
    name = title or _title_of(plugin_id)
    code = any(p.needs_code for p in items)
    perms = sorted({perm for p in items for perm in p.permissions})
    min_size = '"min_size"' if "setting" in chosen else ""
    toml = [
        f"# {plugin_id}: made by `spaceknife plugin new` (docs/plugin-dev.md). Parts: {', '.join(chosen)}.",
        f'id = "{plugin_id}"',
        'version = "0.1.0"',
        f'name = {{ zh = "{name}", en = "{name}" }}',
        f'description = {{ zh = "{name}（由 plugin new 生成）。", en = "{name} (made by plugin new)." }}',
    ]
    if code:
        toml.append('entry = "plugin.py"')
    toml.append(f"permissions = [{', '.join(repr(p).replace(chr(39), chr(34)) for p in perms)}]")
    if "report" in chosen:
        toml.append("""
[[commands]]                  # in the palette (Ctrl+K); keys = ["Ctrl+Alt+L"] would bind a key
name = "open_largest"
title = { zh = "最大的文件", en = "Largest files" }
icon = "gauge"
when = ["done"]               # only once a scan has finished
action = { open = "report:largest" }""")
    for p in items:
        if p.toml:
            toml.append(p.toml.rstrip())
    files: dict[str, str] = {"plugin.toml": "\n".join(toml).rstrip() + "\n"}
    if code:
        sdk = sorted({"Plugin", *(i for p in items for i in p.imports)})
        head = [
            f'"""{name}: a SpaceKnife plugin (docs/plugin-dev.md)."""',
            "",
            "from __future__ import annotations",
            "",
        ]
        std = []
        if "export" in chosen:
            std.append("from collections.abc import Iterator")
        if "source" in chosen:
            std.append("from pathlib import Path")
        if std:
            head += [*std, ""]
        if {"export", "set"} & set(chosen):
            head += ["import numpy as np", ""]
        names = ", ".join(sdk)
        if len(names) + 26 <= 110:
            head.append(f"from spaceknife_sdk import {names}")
        else:
            head += ["from spaceknife_sdk import (", *(f"    {n}," for n in sdk), ")"]
        head += ["", "plugin = Plugin()"]
        blocks = [p.code.replace("%ID%", plugin_id).strip("\n") for p in items if p.code]
        size_expr = 'ctx.settings["min_size"]' if min_size else "1  # bytes"
        body = "\n\n\n".join(blocks).replace("%MIN_SIZE%", size_expr)
        files["plugin.py"] = "\n".join(head) + "\n\n\n" + body + "\n"
    tests = [
        f'"""Tests of {plugin_id}: uv run pytest tests (MockHost runs the plugin without the app)."""',
        "",
        "from __future__ import annotations",
        "",
    ]
    tests += ["from collections.abc import AsyncIterator"]
    if "source" in chosen:
        tests += ["from pathlib import Path"]
    tests += [
        "",
        "import pytest",
        "",
        "from spaceknife_sdk.testing import MockHost, make_tree",
        "",
        "",
        "@pytest.fixture",
        "async def host() -> AsyncIterator[MockHost]:",
        '    """The plugin in this folder, with a small synthetic tree scanned."""',
        "    async with MockHost.load(__file__) as h:",
        "        h.scan(",
        "            make_tree(",
        "                {",
        *(f"                    {line}" for line in TREE),
        "                }",
        "            )",
        "        )",
        "        yield h",
        "",
        "",
        "async def test_plugin_check(host: MockHost) -> None:",
        "    await host.check(strict=True)  # what `spaceknife plugin check` checks",
    ]
    for p in items:
        if p.test:
            tests += ["", "", p.test.strip("\n")]
    files["tests/test_plugin.py"] = "\n".join(tests) + "\n"
    for p in items:
        files.update(p.files)
    files[".gitignore"] = "__pycache__/\ndist/\n"
    # for `pytest tests` in this folder (inside the SpaceKnife repository its own settings apply)
    files["pytest.ini"] = "[pytest]\nasyncio_mode = auto\nasyncio_default_fixture_loop_scope = function\n"
    return files


def write(folder: Path, plugin_id: str, parts: Sequence[str], *, title: str | None = None) -> list[Path]:
    """Create the plugin in ``folder`` (it must not exist or be empty); returns the files written."""
    files = render(plugin_id, parts, title=title)
    if folder.exists() and any(folder.iterdir()):
        raise FileExistsError(f"{folder} exists and is not empty")
    out = []
    for rel, text in files.items():
        path = folder / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")
        out.append(path)
    return out


__all__ = ["DEFAULT", "PARTS", "Part", "parts_of", "render", "write"]
