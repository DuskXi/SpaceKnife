"""Round 5 (fixes only): the authors' tools. ``plugin shot`` acts on what the canvas draws
(``rclick:node=a/b``, ``click:@x,y``) and types into the focused input (``type:``); a script that
throws says its error; ``--setting`` takes JSON (lists, objects), ``@file.json``, and JSON typed in
Windows PowerShell 5.1 (which drops the double quotes inside an argument); ``plugin new --help``
lists every part; ``check`` lets plugins import ``sys`` and ``platform``; the SDK exports
``LabeledAction`` and ``Block``; ``tree.file_ids`` needs ``files.read`` and looks only inside what
the plugin may read; ``plugin dev`` prints what the plugin says; the plugins page names every kind of
contribution; the docs say one thing about ``tree.walk``."""

from __future__ import annotations

import asyncio
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest
import typer
from typer.testing import CliRunner

from spaceknife import cli_plugin
from spaceknife.cliargs import as_typed, crt_split, json_value, needs_quotes, ps51_split
from spaceknife.devshot import Page, ScriptError, StepError, js_error, parse_step, run_step
from spaceknife.plugins.texts import KIND_ORDER
from spaceknife_sdk import scaffold
from spaceknife_sdk.imports import STDLIB, check_imports
from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(120)
ROOT = Path(__file__).resolve().parents[3]


# --------------------------------------------------------------------------- plugin shot


class FakePage(Page):
    """A page that answers ``where`` and records what is sent (no browser)."""

    def __init__(self, answers: dict[str, Any] | None = None) -> None:
        super().__init__(ws=None)
        self.sent: list[tuple[str, dict[str, Any]]] = []
        self.evals: list[str] = []
        self.answers = answers or {}

    async def send(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        timeout: float = 30.0,  # noqa: ASYNC109
        session: str | None = None,
    ) -> Any:
        self.sent.append((method, params or {}))
        return {}

    async def eval(self, expr: str, session: str | None = None) -> Any:
        self.evals.append(expr)
        for key, value in self.answers.items():
            if key in expr:
                if isinstance(value, Exception):
                    raise value
                return value
        return None


def test_canvas_steps_are_read_and_checked() -> None:
    assert parse_step("rclick:node=photos/2021") == (False, "rclick", "node=photos/2021")
    assert parse_step("dblclick:node=") == (False, "dblclick", "node=")
    assert parse_step("click:@120,300.5") == (False, "click", "@120,300.5")
    assert parse_step("type:hello world") == (False, "type", "hello world")
    assert parse_step("panel:type:30") == (True, "type", "30")
    for bad in ("click:@120", "hover:@a,b", "panel:click:node=a", "panel:rclick:@1,2"):
        with pytest.raises(StepError):
            parse_step(bad)


async def test_a_node_is_pressed_where_the_view_draws_it() -> None:
    page = FakePage({"__skDev.where": {"x": 210.5, "y": 120.0}})
    assert await run_step(page, "rclick:node=photos/2021") == ""
    assert 'window.__skDev.where("photos/2021")' in page.evals[-1]
    mouse = [p for m, p in page.sent if m == "Input.dispatchMouseEvent"]
    assert [(p["type"], p.get("button")) for p in mouse] == [
        ("mouseMoved", None),
        ("mousePressed", "right"),
        ("mouseReleased", "right"),
    ]
    assert all((p["x"], p["y"]) == (210.5, 120.0) for p in mouse)
    page = FakePage({"__skDev.where": {"x": 5, "y": 6}})
    await run_step(page, "dblclick:node=/")  # the zoom root: "" after the slashes
    assert 'where("")' in page.evals[-1]
    clicks = [
        p["clickCount"]
        for m, p in page.sent
        if m == "Input.dispatchMouseEvent" and p["type"] == "mousePressed"
    ]
    assert clicks == [1, 2]


async def test_a_point_is_pressed_as_it_is_and_a_node_not_drawn_says_why() -> None:
    page = FakePage()
    await run_step(page, "hover:@40,50")
    assert page.sent == [("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": 40.0, "y": 50.0})]
    assert page.evals == []  # nothing asked of the page
    not_drawn = ScriptError("Error: photos is not drawn in the view (in a folder not opened …)")
    with pytest.raises(StepError, match=r"^photos is not drawn in the view"):
        await run_step(FakePage({"__skDev.where": not_drawn}), "click:node=photos")
    with pytest.raises(StepError, match="older than this step"):
        await run_step(FakePage({"__skDev.where": "no hooks"}), "click:node=photos")


async def test_type_goes_to_the_focused_input() -> None:
    page = FakePage()
    await run_step(page, "type:a b, “c”")
    assert page.sent == [("Input.insertText", {"text": "a b, “c”"})]
    panel = FakePage({"activeElement": True})
    panel.frames["s1"] = "http://127.0.0.1:1/plugins/x.y/web/index.html"
    await run_step(panel, "panel:type:42")
    assert 'const text = "42"' in panel.evals[-1] and "execCommand" in panel.evals[-1]
    nothing = FakePage({"activeElement": False})
    nothing.frames["s1"] = "http://127.0.0.1:1/plugins/x.y/web/index.html"
    with pytest.raises(StepError, match="has the focus"):
        await run_step(nothing, "panel:type:42")


async def test_a_script_that_throws_says_its_error() -> None:
    reference = {
        "text": "Uncaught",
        "exception": {"description": "ReferenceError: foo is not defined\n    at <anonymous>:1:1"},
    }
    assert js_error(reference) == "ReferenceError: foo is not defined"
    assert js_error({"text": "Uncaught", "exception": {"value": "no canvas"}}) == 'Uncaught "no canvas"'
    assert js_error({"text": "Uncaught SyntaxError", "lineNumber": 0}) == "Uncaught SyntaxError (line 1)"
    page = FakePage({"foo()": ScriptError("ReferenceError: foo is not defined")})
    with pytest.raises(StepError, match="the script threw ReferenceError: foo is not defined"):
        await run_step(page, "eval:foo()")


# --------------------------------------------------------------------------- arguments as typed

PS51 = (
    '"C:\\py\\python.exe" C:\\venv\\Scripts\\spaceknife plugin shot . '
    '--setting "rules=[{"glob": "a b.log", "days": 3}]" --setting n=3 '
    '--step "eval:document.querySelector("canvas") !== null" --setting x=["a","b"] -o "C:\\My Shots\\a.png"'
)


def test_crt_split_reads_quotes_and_backslashes_as_the_c_runtime() -> None:
    assert crt_split('a "b c" d') == ["a", "b c", "d"]
    assert crt_split('"a\\"b" c\\\\"d e"') == ['a"b', "c\\d e"]
    assert crt_split('"say ""hi"""') == ['say "hi"']
    assert crt_split("C:\\a\\b x") == ["C:\\a\\b", "x"]


def test_arguments_windows_powershell_51_mangled_are_read_as_typed() -> None:
    received = crt_split(PS51)[2:]  # what the program got: the C runtime's reading
    assert "rules=[{glob: a" in received and "b.log, days: 3}]" in received  # split in two
    argv = ["spaceknife", *received]
    assert as_typed(argv, PS51)[1:] == [
        "plugin",
        "shot",
        ".",
        "--setting",
        'rules=[{"glob": "a b.log", "days": 3}]',
        "--setting",
        "n=3",
        "--step",
        'eval:document.querySelector("canvas") !== null',
        "--setting",
        'x=["a","b"]',
        "-o",
        "C:\\My Shots\\a.png",
    ]
    assert ps51_split('"a b" "c"') == ["a b", '"c"']  # PowerShell wraps only what has whitespace


TYPED = [
    "plugin",
    "shot",
    "C:\\My Plugins\\x",
    'rules=[{"glob": "a b.log", "days": 3}]',
    'rules="just text"',  # whitespace only inside its quotes: passed as it is
    'eval:f("a b")',
    'j={"k": "v" }',
    'x=["a","b"]',
    '"quoted"',
    "type:two words",
    "",
]


def ps51_join(args: list[str]) -> str:
    """What Windows PowerShell 5.1 writes for these arguments (its NeedQuotes, no escaping)."""
    return " ".join(f'"{a}"' if needs_quotes(a) or not a else a for a in args)


def test_what_powershell_51_writes_is_read_back() -> None:
    line = '"C:\\py\\python.exe" -m spaceknife ' + ps51_join(TYPED)
    assert ps51_split(line)[3:] == [a if a else '""' for a in TYPED[:-1]] + ['""']
    received = crt_split(line)[3:]
    assert received != TYPED  # the C runtime's reading lost quotes and split arguments
    assert as_typed(["spaceknife", *received], line)[1:-1] == TYPED[:-1]


def test_arguments_quoted_correctly_are_left_alone() -> None:
    escaped = '"C:\\py\\python.exe" -m spaceknife plugin call . m --setting "rules=[{\\"glob\\": \\"a b\\"}]"'
    argv = ["spaceknife", *crt_split(escaped)[3:]]
    assert argv[-1] == 'rules=[{"glob": "a b"}]'
    assert as_typed(argv, escaped) is argv  # \" anywhere: someone escaped, as they should
    plain = "python -m spaceknife plugin check ."
    argv = ["spaceknife", "plugin", "check", "."]
    assert as_typed(argv, plain) is argv
    assert as_typed(["spaceknife", "other"], PS51) == ["spaceknife", "other"]  # not this line
    assert as_typed(["spaceknife", "x"], None if sys.platform == "win32" else "") == ["spaceknife", "x"]


def test_settings_take_json_a_file_or_text(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (tmp_path / "rules.json").write_text('[{"glob": "*.log", "days": 9}]', encoding="utf-8-sig")
    got = cli_plugin._settings(
        ["n=3", "on=true", 'rules=[{"glob": "*.tmp"}]', "title=two words", f"file=@{tmp_path / 'rules.json'}"]
    )
    assert got == {
        "n": 3,
        "on": True,
        "rules": [{"glob": "*.tmp"}],
        "title": "two words",
        "file": [{"glob": "*.log", "days": 9}],
    }
    assert cli_plugin._settings(["rules=[{glob: a"]) == {"rules": "[{glob: a"}  # said, kept as text
    assert "rules=@file.json" in capsys.readouterr().err
    for bad in ("nothing", f"f=@{tmp_path / 'missing.json'}"):
        with pytest.raises(typer.Exit):
            cli_plugin._settings([bad])
    (tmp_path / "bad.json").write_text("[1,", encoding="utf-8")
    with pytest.raises(ValueError, match="is not JSON"):
        json_value("f", f"@{tmp_path / 'bad.json'}")


def _echo_plugin(folder: Path) -> Path:
    scaffold.write(folder, "example.args", ["method", "setting"])
    code = (folder / "plugin.py").read_text(encoding="utf-8")
    code += '''

class Now(Payload):
    rules: list[dict[str, object]]


@plugin.method
async def settings_now(ctx: Context, p: Params) -> Now:
    """The rules setting as the plugin sees it."""
    return Now(rules=ctx.settings["rules"])
'''
    (folder / "plugin.py").write_text(code, encoding="utf-8")
    return folder


@pytest.mark.skipif(
    sys.platform != "win32" or not shutil.which("powershell"), reason="Windows PowerShell 5.1"
)
@pytest.mark.parametrize(
    "typed",
    [
        'rules=[{"glob": "a b.log", "days": 3}]',  # whitespace outside its quotes: PowerShell wraps it
        'rules=[{"glob":"a b.log","days":3}]',  # only inside them: passed as it is
    ],
)
def test_a_setting_typed_in_windows_powershell_51_arrives(tmp_path: Path, typed: str) -> None:
    folder = _echo_plugin(tmp_path / "example.args")
    script = f"& '{sys.executable}' -m spaceknife plugin call '{folder}' settings_now --inprocess --setting '{typed}'"
    env = {**os.environ, "SPACEKNIFE_HOME": str(tmp_path / "home"), "PYTHONIOENCODING": "utf-8"}
    run = subprocess.run(
        ["powershell", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True,
        timeout=110,
        env=env,
        check=False,
    )
    out = run.stdout.decode("utf-8", "replace")
    assert run.returncode == 0, out + run.stderr.decode("utf-8", "replace")
    assert json.loads(out[out.index("{") :]) == {"rules": [{"glob": "a b.log", "days": 3}]}


def test_plugin_call_takes_params_from_a_file(tmp_path: Path) -> None:
    (tmp_path / "p.json").write_text('{"name": "two words"}', encoding="utf-8")
    assert cli_plugin._json_arg(f"@{tmp_path / 'p.json'}", "params") == {"name": "two words"}
    with pytest.raises(typer.Exit):
        cli_plugin._json_arg("{name: x}", "params")


# --------------------------------------------------------------------------- plugin new, check, SDK


def test_new_help_lists_every_part() -> None:
    result = CliRunner().invoke(cli_plugin.plugin_app, ["new", "--help"], env={"COLUMNS": "400"})
    assert result.exit_code == 0
    text = " ".join(result.output.replace("│", " ").split())
    assert f"Parts, comma-separated: {' '.join(scaffold.PARTS)} (default: report,command)" in text
    assert {"changes", "task"} <= set(scaffold.PARTS)
    quick = (ROOT / "docs" / "plugin-quick.md").read_text(encoding="utf-8")
    assert f"`{' '.join(scaffold.PARTS)}`" in quick  # the quick reference: the same list


def test_check_allows_sys_and_platform_and_says_what_else_is_wrong(tmp_path: Path) -> None:
    (tmp_path / "plugin.py").write_text(
        "import platform\nimport sys\nimport sched\nimport requests\n", encoding="utf-8"
    )
    issues = {i.message.split(":")[0]: i for i in check_imports(tmp_path)}
    assert set(issues) == {"sched", "requests"}
    assert "a standard module plugins may not use" in issues["sched"].message
    assert "--section imports" in issues["sched"].fix
    assert "not in the packaged app" in issues["requests"].message and "lib/" in issues["requests"].fix
    # the docs list what check allows
    dev = (ROOT / "docs" / "plugin-dev.md").read_text(encoding="utf-8")
    listed = re.search(r"以及标准库的这些：`([^`]+)`", dev)
    assert listed is not None and set(listed.group(1).split()) == STDLIB - {"__future__"}


def test_the_sdk_exports_actions_and_blocks() -> None:
    import spaceknife_sdk as sdk
    from spaceknife_protocol import plugins as pp

    assert sdk.LabeledAction is pp.LabeledAction and sdk.Block is pp.Block
    assert isinstance(sdk.r.action("x", open="report:y"), sdk.LabeledAction)


FILE_IDS = """
from spaceknife_sdk import Context, Listing, Params, Payload, Plugin

plugin = Plugin()


class Ids(Payload):
    virtual: bool
    ids: list[list[int] | None]


@plugin.method
def ids(ctx: Context, p: Params) -> Ids:
    tree = ctx.tree
    found = tree.query(kind="file", columns=()).ids
    return Ids(virtual=tree.virtual, ids=[list(x) if x else None for x in tree.file_ids(found)])


@plugin.scan_source("list", "List", pick="file", extensions=[".txt"])
def file_list(ctx: Context, uri: str, out: Listing) -> None:
    out.file("a/b.txt", size=5)
"""


def _file_ids_plugin(folder: Path) -> Path:
    folder.mkdir()
    (folder / "plugin.toml").write_text(
        'id = "example.fileids"\nversion = "0.1.0"\nname = { zh = "文件号", en = "File ids" }\n'
        'entry = "plugin.py"\npermissions = ["tree.read", "files.read"]\n',
        encoding="utf-8",
    )
    (folder / "plugin.py").write_text(FILE_IDS, encoding="utf-8")
    return folder


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_file_ids_need_files_read_and_look_only_at_files_on_disk(tmp_path: Path, process: bool) -> None:
    folder = _file_ids_plugin(tmp_path / "example.fileids")
    tree = make_tree({"a.bin": b"one", "b.bin": b"two"})
    async with MockHost.load(folder, process=process) as host:
        host.scan(tree)
        got = await host.call("ids")
        assert not got["virtual"] and len(got["ids"]) == 2
        assert all(x is not None and len(x) == 3 and x[2] >= 1 for x in got["ids"])  # device, id, links
        listing = tmp_path / "list.txt"
        listing.write_text("5 a/b.txt\n", encoding="utf-8")
        await host.source("list", str(listing))
        got = await host.call("ids")
        assert got == {"virtual": True, "ids": [None]}  # a scan source's files are not on this disk
    async with MockHost.load(folder, process=process, grant=["tree.read"]) as host:
        host.scan(tree)
        with pytest.raises(Exception, match=r"files.read"):
            await host.call("ids")


# --------------------------------------------------------------------------- plugin dev, the page


async def test_plugin_dev_prints_what_the_plugin_says(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.devui import _dev

    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("SPACEKNIFE_PLUGIN_DEV", "1")
    folder = tmp_path / "example.talk"
    scaffold.write(folder, "example.talk", ["changes"])  # its permissions: tree.read, notify
    code = (folder / "plugin.py").read_text(encoding="utf-8")
    code += """

@plugin.on("start")
def said(ctx: Context) -> None:
    ctx.log.info("started up")
    ctx.toast(T("你好", "Hello"), tone="ok")
    ctx.notify(T("看这里", "Look here"), T("正文", "The body"))
"""
    (folder / "plugin.py").write_text(code, encoding="utf-8")
    said: list[str] = []
    ready: list[dict[str, Any]] = []
    stop = asyncio.Event()
    task = asyncio.create_task(
        _dev(folder, "example.talk", None, 0, None, [], said.append, stop, ready.append)
    )
    try:
        loop = asyncio.get_running_loop()
        end = loop.time() + 30
        while not (any(s.startswith("notice:") for s in said) and any(s.startswith("toast") for s in said)):
            assert loop.time() < end, said
            await asyncio.sleep(0.05)
        assert "log info: started up" in said
        assert any(s.startswith("toast (ok): ") for s in said)
        assert any(s.startswith("notice: ") and " — " in s for s in said)
    finally:
        stop.set()
        await asyncio.wait_for(task, 30)


def test_the_plugins_page_names_every_kind_of_contribution() -> None:
    page = (ROOT / "apps" / "electron" / "src" / "renderer" / "src" / "pages" / "PluginsPage.tsx").read_text(
        encoding="utf-8"
    )
    body = page[page.index("function kindLabel(") :]
    body = body[: body.index("return names[kind]")]
    named = set(re.findall(r"^\s+([a-z_]+): t\(", body, re.M))
    assert set(KIND_ORDER) <= named, set(KIND_ORDER) - named


def test_the_docs_say_one_thing_about_walk() -> None:
    dev = (ROOT / "docs" / "plugin-dev.md").read_text(encoding="utf-8")
    api = (ROOT / "docs" / "plugin-api.md").read_text(encoding="utf-8")
    assert "先序" not in dev and "逐层" in dev and "逐层" in api
