"""``spaceknife plugin …`` and what is behind it: the skeleton works as made (every part), check finds
what is wrong and says where, build writes the generated files, and a plugin goes from a folder to
installed, enabled and back through the command line alone.

The commands run as the user runs them (``python -m spaceknife plugin …``, a temporary data folder).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

from spaceknife.plugins.build import build
from spaceknife.plugins.check import check_plugin, panel_sdk
from spaceknife_sdk import scaffold
from spaceknife_sdk.schema import export_plugin_schema, prefix_of
from spaceknife_sdk.testing import MockHost, make_tree
from spaceknife_sdk.tsgen import generate_plugin

ROOT = Path(__file__).resolve().parents[3]
RUN = [sys.executable, "-X", "utf8", "-m", "spaceknife", "run"]

pytestmark = pytest.mark.timeout(240)


def _env(home: Path) -> dict[str, str]:
    env = dict(os.environ)
    env["SPACEKNIFE_HOME"] = str(home)
    env["SPACEKNIFE_LANGUAGE"] = "zh-CN"
    env.pop("SPACEKNIFE_PLUGIN_DEV", None)
    return env


def sk(
    home: Path, *args: str, cwd: Path | None = None, input: str | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-X", "utf8", "-m", "spaceknife", "plugin", *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=_env(home),
        cwd=cwd or home,
        input=input,
        timeout=180,
        check=False,
    )


def _pytest(folder: Path, home: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", str(folder / "tests")],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=_env(home),
        cwd=folder,
        timeout=180,
        check=False,
    )


def test_new_makes_a_plugin_that_passes_check_and_its_tests(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    made = sk(home, "new", "example.hello", "--dir", str(tmp_path / "example.hello"))
    assert made.returncode == 0, made.stderr
    folder = tmp_path / "example.hello"
    assert (folder / "generated" / "schema.json").is_file()  # built right away
    checked = sk(home, "check", str(folder), "--strict", "--json")
    assert checked.returncode == 0, checked.stdout + checked.stderr
    doc = json.loads(checked.stdout)
    assert doc["ok"] and doc["plugin"] == "example.hello"
    assert all(i["severity"] == "info" for i in doc["issues"])
    tests = _pytest(folder, home)
    assert tests.returncode == 0, tests.stdout + tests.stderr


def test_every_part_together(tmp_path: Path) -> None:
    folder = tmp_path / "example.all"
    scaffold.write(folder, "example.all", list(scaffold.PARTS))
    assert build(folder).ok
    result = check_plugin(folder)  # in a plugin process, as the app runs it
    assert result.ok(strict=True), [i.text() for i in result.issues]
    tests = _pytest(folder, tmp_path / "home")
    assert tests.returncode == 0, tests.stdout + tests.stderr
    # each part's test, the check (here and in a plugin process), the report following min_size
    assert f"{len(scaffold.PARTS) + 3} passed" in tests.stdout
    ruff = shutil.which("ruff")
    if ruff is not None:  # what it writes is clean code (the repository's style)
        fmt = subprocess.run(
            [ruff, "format", "--check", str(folder)], capture_output=True, text=True, cwd=ROOT
        )
        assert fmt.returncode == 0, fmt.stdout + fmt.stderr
        lint = subprocess.run(
            [ruff, "check", "--no-cache", str(folder)], capture_output=True, text=True, cwd=ROOT
        )
        assert lint.returncode == 0, lint.stdout + lint.stderr


@pytest.mark.parametrize("part", sorted(scaffold.PARTS))
def test_each_part_alone(part: str, tmp_path: Path) -> None:
    folder = tmp_path / f"example.p{part}"
    scaffold.write(folder, f"example.p{part}", [part])
    assert build(folder, isolated=False).ok
    result = check_plugin(folder, process=False, subprocess_describe=False)
    assert result.ok(strict=True), [i.text() for i in result.issues]


def test_a_chinese_title_keeps_an_english_name() -> None:
    toml = scaffold.render("example.dup-finder", [], title="重复文件")["plugin.toml"]
    assert 'name = { zh = "重复文件", en = "Dup Finder" }' in toml
    steps = scaffold.next_steps(Path("x"), ["export", "panel"])
    assert any("export:md" in s for s in steps)
    assert not any("report:largest" in s for s in steps)


def test_new_refuses_bad_ids_and_parts(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="not a plugin id"):
        scaffold.render("hello", [])
    with pytest.raises(ValueError, match="not a plugin id"):
        scaffold.render("view.mine", [])  # an app namespace
    with pytest.raises(ValueError, match="unknown part"):
        scaffold.render("example.x", ["reports"])
    assert scaffold.parts_of(["report,visualization", "colour"]) == ["report", "visual", "color"]


def test_check_says_what_is_wrong_and_where(tmp_path: Path) -> None:
    folder = tmp_path / "example.bad"
    scaffold.write(folder, "example.bad", ["report"])
    toml = (folder / "plugin.toml").read_text(encoding="utf-8")
    (folder / "plugin.toml").write_text(toml.replace('icon = "gauge"', 'icno = "gauge"'), encoding="utf-8")
    result = check_plugin(folder, contracts=False)
    [typo] = [i for i in result.issues if i.code == "unknown_field"]
    assert typo.file == "plugin.toml" and typo.line > 0 and "icon" in typo.fix
    (folder / "plugin.toml").write_text(toml, encoding="utf-8")
    code = (folder / "plugin.py").read_text(encoding="utf-8")
    (folder / "plugin.py").write_text(code + "\n\nimport json_typo_module  # noqa\n", encoding="utf-8")
    result = check_plugin(folder, contracts=False)
    codes = {i.code for i in result.errors()}
    assert {"unknown_import", "import"} <= codes
    [imp] = [i for i in result.issues if i.code == "import"]
    assert imp.file == "plugin.py" and imp.line > 0 and "ModuleNotFoundError" in imp.message
    (folder / "plugin.py").write_text(
        code.replace(
            "async def largest(ctx: Context, c: ReportContext) -> Report:",
            "async def largest(ctx: Context, c: ReportContext) -> Report:\n    ctx.files.read(0)",
        ),
        encoding="utf-8",
    )
    result = check_plugin(folder, contracts=False)
    [perm] = [i for i in result.errors() if i.code == "missing_permission"]
    assert perm.file == "plugin.py" and perm.line > 0 and '"files.read"' in perm.fix
    (folder / "plugin.py").write_text(
        code.replace("rows = ctx.tree.query(", "rows = 1 / 0\n    rows = ctx.tree.query("), encoding="utf-8"
    )
    result = check_plugin(folder, process=False)
    [crash] = [i for i in result.errors() if i.code == "report"]
    assert "ZeroDivisionError" in crash.message
    doc = result.as_dict()
    assert doc["issues"][0]["doc"].startswith("docs/plugin-dev.md#")


def test_build_writes_types_schema_and_the_panel_sdk(tmp_path: Path) -> None:
    folder = tmp_path / "example.web"
    scaffold.write(folder, "example.web", ["panel", "method"])
    first = build(folder)
    assert set(first.written) >= {"generated/schema.json", "web/sk-plugin.d.ts", "web/_sk/bridge.js"}
    assert build(folder).written == []  # nothing changed
    schema = json.loads((folder / "generated" / "schema.json").read_text(encoding="utf-8"))
    assert schema["plugin"] == "example.web" and set(schema["methods"]) == {"counts", "hello"}
    assert schema["methods"]["hello"]["params"] == "#/$defs/ExampleWebHelloParams"
    assert schema["methods"]["hello"]["doc"].startswith("Greets someone")
    dts = (folder / "web" / "sk-plugin.d.ts").read_text(encoding="utf-8")
    assert "  hello: { params: ExampleWebHelloParams; result: ExampleWebHelloResult };" in dts
    assert dts == generate_plugin(schema)
    (folder / "web" / "_sk" / "bridge.js").write_text("// old", encoding="utf-8")
    stale = check_plugin(folder, contracts=False)
    assert [i.file for i in stale.issues if i.code == "stale_generated"] == ["web/_sk/bridge.js"]
    packed = build(folder, zip=True)
    assert packed.zip is not None and packed.zip.name == "example.web-0.1.0.zip"
    names = zipfile.ZipFile(packed.zip).namelist()
    assert "example.web/plugin.toml" in names and not any("/dist/" in n for n in names)
    assert prefix_of("acme.zip-source") == "AcmeZipSource"


def test_the_panel_sdk_copy_is_the_apps() -> None:
    app = ROOT / "apps" / "electron" / "resources" / "plugin-sdk"
    for name, data in panel_sdk().items():
        assert (app / name).read_bytes() == data, (
            f"copy apps/electron/resources/plugin-sdk/{name} to packages/sdk/src/spaceknife_sdk/web/"
        )


def test_install_enable_disable_uninstall(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    folder = tmp_path / "src" / "example.hello"
    scaffold.write(folder, "example.hello", ["report", "command"])
    packed = build(folder, zip=True)
    assert packed.zip is not None
    got = sk(home, "install", str(packed.zip))
    assert got.returncode == 0, got.stderr
    listed = json.loads(sk(home, "list", "--json").stdout)
    states = {p["id"]: p["state"] for p in listed["plugins"]}
    assert states["example.hello"] == "needs_confirm" and states["builtin.export"] == "enabled"
    refused = sk(home, "enable", "example.hello", input="n\n")
    assert refused.returncode == 1 and "tree.read" in refused.stdout
    enabled = sk(home, "enable", "example.hello", "--yes")
    assert enabled.returncode == 0 and "enabled" in enabled.stdout, enabled.stdout + enabled.stderr
    again = sk(home, "install", str(folder))
    assert again.returncode == 1 and "--replace" in again.stderr
    assert sk(home, "disable", "example.hello").stdout.strip().endswith("disabled")
    assert sk(home, "disable", "builtin.export").returncode == 1  # shipped: always on
    gone = sk(home, "uninstall", "example.hello")
    assert gone.returncode == 0, gone.stderr
    listed = json.loads(sk(home, "list", "--json").stdout)
    assert "example.hello" not in {p["id"] for p in listed["plugins"]}


def test_call_render_schema_docs(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    folder = tmp_path / "example.hello"
    scaffold.write(folder, "example.hello", ["report", "command", "cleanup", "export"])
    tree = tmp_path / "tree"
    (tree / "logs").mkdir(parents=True)
    (tree / "big.iso").write_bytes(b"\0" * 200_000)
    (tree / "logs" / "app.log").write_text("x", encoding="utf-8")
    shown = sk(home, "render", str(folder), "report:largest", "--scan", str(tree), "--inprocess")
    assert shown.returncode == 0, shown.stderr
    assert shown.stdout.startswith("# 最大的文件") and "big.iso" in shown.stdout
    english = sk(
        home, "render", str(folder), "report:largest", "--scan", str(tree), "--lang", "en", "--format", "json"
    )
    assert json.loads(english.stdout)["title"] == "Largest files"
    cleanup = sk(home, "render", str(folder), "cleanup:logs", "--scan", str(tree))
    assert "app.log" in cleanup.stdout and "1 candidate(s)" in cleanup.stdout
    exported = sk(home, "render", str(folder), "export:md", "--scan", str(tree), "--inprocess")
    assert exported.stdout.splitlines()[0] == "| Size | Path |"
    called = sk(home, "call", str(folder), "greet", "{}")
    assert called.returncode == 0 and json.loads(called.stdout)["type"] == "toast"
    missing = sk(home, "call", str(folder), "nope")
    assert missing.returncode == 1 and "nope" in missing.stderr
    manifest = json.loads(sk(home, "schema", "--manifest").stdout)
    assert "commands" in manifest["properties"]
    section = sk(home, "docs", "--section", "kind-cleanup")
    assert section.returncode == 0 and section.stdout.startswith('<a id="kind-cleanup"></a>')
    assert "kind-report" in sk(home, "docs", "--list").stdout


async def test_a_source_reads_the_file_the_user_picked(tmp_path: Path) -> None:
    """In a plugin process: the URI the user chose is readable with files.read, outside any scan."""
    folder = tmp_path / "example.src"
    scaffold.write(folder, "example.src", ["source"])
    listing = tmp_path / "elsewhere" / "list.txt"
    listing.parent.mkdir()
    listing.write_text("100 a/b.txt\n5 c.txt\n", encoding="utf-8")
    async with MockHost.load(folder, process=True) as host:
        await host.source("list", str(listing))
        assert host.store.name(host.find("a/b.txt")) == "b.txt"


async def test_a_running_plugin_process_reads_folders_scanned_later(tmp_path: Path) -> None:
    """The audit hook follows the open scans: a folder scanned after the process started is readable."""
    folder = tmp_path / "example.read"
    scaffold.write(folder, "example.read", ["method"])
    code = (folder / "plugin.py").read_text(encoding="utf-8")
    (folder / "plugin.py").write_text(
        code
        + """

class HeadParams(Params):
    path: str


@plugin.method
def head(ctx: Context, p: HeadParams) -> HelloResult:
    return HelloResult(text=ctx.files.read(p.path, 5).decode())
""",
        encoding="utf-8",
    )
    toml = (folder / "plugin.toml").read_text(encoding="utf-8")
    (folder / "plugin.toml").write_text(
        toml.replace("permissions = []", 'permissions = ["files.read"]'), encoding="utf-8"
    )
    later = tmp_path / "later"
    later.mkdir()
    (later / "x.txt").write_text("hello there", encoding="utf-8")
    async with MockHost.load(folder, process=True) as host:
        host.scan(make_tree({"a.txt": 1}))
        assert (await host.call("hello"))["text"]  # the process runs
        await host.scan_dir(later)
        assert (await host.call("head", path=str(later / "x.txt")))["text"] == "hello"


def test_schema_prefixes_and_nested_models() -> None:
    from spaceknife_sdk.plugin import Description

    desc = Description.model_validate(
        {
            "id": "acme.tools",
            "version": "1.0.0",
            "api": 1,
            "methods": [
                {
                    "name": "find",
                    "params": {
                        "title": "FindParams",
                        "type": "object",
                        "properties": {"n": {"type": "integer"}},
                    },
                    "result": {
                        "title": "FindResult",
                        "type": "object",
                        "properties": {"items": {"type": "array", "items": {"$ref": "#/$defs/Item"}}},
                        "$defs": {"Item": {"title": "Item", "type": "object", "properties": {}}},
                    },
                },
                {"name": "report_x", "kind": "report"},
            ],
        }
    )
    schema = export_plugin_schema(desc)
    assert set(schema["$defs"]) == {"AcmeToolsFindParams", "AcmeToolsFindResult", "AcmeToolsItem"}
    assert schema["$defs"]["AcmeToolsFindResult"]["properties"]["items"]["items"] == {
        "$ref": "#/$defs/AcmeToolsItem"
    }
    assert list(schema["methods"]) == ["find"]  # reports are the host's to call


def test_run_exports_with_an_enabled_plugins_format(tmp_path: Path) -> None:
    """``spaceknife run … export <plugin>.<format>``: the enabled plugins of the data folder are
    started without a window; ``run help`` lists their formats."""
    home = tmp_path / "home"
    home.mkdir()
    folder = tmp_path / "src" / "example.fmt"
    scaffold.write(folder, "example.fmt", ["export"])
    assert sk(home, "install", str(folder)).returncode == 0
    enabled = sk(home, "enable", "example.fmt", "--yes")
    assert enabled.returncode == 0, enabled.stdout + enabled.stderr
    data = tmp_path / "data"
    (data / "sub").mkdir(parents=True)
    (data / "a.txt").write_bytes(b"a" * 10)
    (data / "sub" / "b.txt").write_bytes(b"b" * 20)
    out = tmp_path / "out.md"
    run = subprocess.run(
        [*RUN, "scan", str(data), "export", "example.fmt.md", str(out), "autoclose"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=_env(home),
        cwd=home,
        timeout=180,
        check=False,
    )
    assert run.returncode == 0, run.stdout + run.stderr
    assert "b.txt" in out.read_text(encoding="utf-8")
    shown = subprocess.run(
        [*RUN, "help"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=_env(home),
        cwd=home,
        timeout=180,
        check=False,
    )
    assert "example.fmt.md" in shown.stdout, shown.stdout + shown.stderr


async def test_call_sees_the_snapshots_given_with_history(tmp_path: Path) -> None:
    """``--history FILE``: ctx.snapshots.list() in the temporary backend lists it (a plugin that
    reads history is tried from the command line); docs --api prints the API reference."""
    home = tmp_path / "home"
    home.mkdir()
    folder = tmp_path / "example.hist"
    folder.mkdir()
    (folder / "plugin.toml").write_text(
        'id = "example.hist"\nversion = "0.1.0"\nname = { zh = "历史", en = "History" }\n'
        'entry = "plugin.py"\npermissions = ["tree.read", "snapshots.read"]\n',
        encoding="utf-8",
    )
    (folder / "plugin.py").write_text(
        "from spaceknife_sdk import Context, Payload, Plugin\n\nplugin = Plugin()\n\n\n"
        "class Count(Payload):\n    n: int\n    files: list[int]\n\n\n"
        "@plugin.method\nasync def count(ctx: Context) -> Count:\n"
        "    snaps = ctx.snapshots.list()\n    return Count(n=len(snaps), files=[s.files for s in snaps])\n",
        encoding="utf-8",
    )
    async with MockHost.load(folder) as host:
        host.scan(make_tree({"a.txt": 1, "b": {"c.txt": 2}}, root=str(tmp_path / "tree")))
        saved = Path(host.snapshot())
        shutil.copyfile(saved, tmp_path / "one.sks")
    called = sk(home, "call", str(folder), "count", "--history", str(tmp_path / "one.sks"), "--inprocess")
    assert called.returncode == 0, called.stderr
    assert json.loads(called.stdout) == {"n": 1, "files": [2]}
    api = sk(home, "docs", "--api")
    assert api.returncode == 0 and api.stdout.startswith("# SpaceKnife 插件 API 参考")
    part = sk(home, "docs", "--section", "api-hooks")
    assert part.returncode == 0 and "`settings`" in part.stdout
