"""What the second trial of plugin authors found missing (plugins-round2 gaps): texts made at a
module's top level follow the language, commands know the selected node's kind and where the user
is, MockHost runs commands and asks whether they can run, hooks are checked, multi-choice and
drop-down settings, export options that follow a setting, a visualization's own colour mode."""

from __future__ import annotations

from pathlib import Path

import pytest

from spaceknife_sdk import ErrorCode, RequestError
from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(240)

TOML = """id = "t.round3"
version = "0.1.0"
name = { zh = "第三轮", en = "Round three" }
entry = "plugin.py"
permissions = ["tree.read", "clipboard", "files.read", "basket.propose"]

[[settings]]
name = "cols"
title = { zh = "列", en = "Columns" }
control = "multi"
default = "name,size"
options = [["name", { zh = "名字", en = "Name" }], ["size", { zh = "大小", en = "Size" }], ["path", { zh = "位置", en = "Location" }]]

[[settings]]
name = "style"
title = { zh = "样式", en = "Style" }
default = "a"
options = [["a", "A"], ["b", "B"], ["c", "C"], ["d", "D"], ["e", "E"]]

[[settings]]
name = "days"
title = { zh = "天数", en = "Days" }
default = 7
unit = { zh = "天", en = "days" }
"""

CODE = """from __future__ import annotations

from typing import Any

from spaceknife_sdk import ActionContext, Context, ExportInput, Payload, Plugin, T, r
from spaceknife_sdk.manifest import SettingSpec

plugin = Plugin()

# made once, at import: still written in the language of each call
COLUMNS = [r.col("name", T("名字", "Name"), "text"), r.col("size", T("大小", "Size"), "size")]
HELLO = T("你好", "Hello")


class Seen(Payload):
    seen: dict[str, Any]


@plugin.method
async def columns(ctx: Context) -> Seen:
    t = r.table(COLUMNS, rows=[{"name": HELLO, "size": 1}])
    return Seen(seen={"table": t.model_dump(mode="json")})


@plugin.command("only_files", T("只对文件", "Files only"), when=["file"])
async def only_files(ctx: Context, c: ActionContext) -> Seen:
    return Seen(seen={"node": c.node, "zoom": c.zoom_node, "filter": c.filter, "args": c.args})


@plugin.command("only_dirs", T("只对文件夹", "Folders only"), when=["dir"])
async def only_dirs(ctx: Context, c: ActionContext) -> Seen:
    return Seen(seen={})


@plugin.command("only_logs", T("只对日志", "Logs only"), when=["ext:log,txt"])
async def only_logs(ctx: Context, c: ActionContext) -> Seen:
    return Seen(seen={})


@plugin.method
def why_not(ctx: Context, c: ActionContext) -> str:
    if c.node is None:
        return "nothing selected"
    return "" if ctx.tree.node(c.node).name != "b.txt" else "not b.txt"


@plugin.command("checked", T("要检查", "Checked"), check="why_not")
async def checked(ctx: Context, c: ActionContext) -> Seen:
    return Seen(seen={})


@plugin.command("copy", T("复制", "Copy"))
async def copy(ctx: Context, c: ActionContext) -> None:
    ctx.clipboard.write_text(T("复制了", "copied"))


@plugin.method
def hashes(ctx: Context) -> Seen:
    t = ctx.tree
    ids = [t.find(p) for p in t.root_paths] + [t.find(t.root_paths[0] + "/" + n) for n in ("a.log", "b.txt")]
    out = ctx.files.hash_many([i for i in ids if i is not None], limit=4)
    try:
        ctx.files.read("/etc/hostname" if "/" in t.root_paths[0] else "C:/Windows/win.ini")
        outside = "read"
    except Exception as exc:
        outside = type(exc).__name__
    return Seen(seen={"hashes": out, "outside": outside})


@plugin.command("bin_them", T("放进删除篮", "Into the basket"))
async def bin_them(ctx: Context, c: ActionContext) -> None:
    t = ctx.tree
    a, b = t.find(t.root_paths[0] + "/a.log"), t.find(t.root_paths[0] + "/b.txt")
    ctx.basket.propose([(a, T("甲", "A"))])
    ctx.basket.propose([(b, T("乙", "B"))])  # a second time in the same call: still one message


@plugin.command("take_back", T("拿回", "Take back"))
async def take_back(ctx: Context, c: ActionContext) -> Seen:
    t = ctx.tree
    return Seen(seen={"removed": ctx.basket.withdraw([t.find(t.root_paths[0] + "/a.log")])})


@plugin.method
def numbers(ctx: Context) -> Seen:
    return Seen(seen={"size": ctx.fmt.size(1536000), "count": ctx.fmt.count(12345), "t": ctx.t("{n:size}", "{n:size}", n=2048)})


@plugin.on("scan_done")
async def done(ctx: Context, scan: Any) -> None:
    ctx.state("seen").set({"scan": scan.scan_id})


@plugin.state
class Seen2(Payload):
    scan: str = ""


@plugin.export_format(
    "list",
    T("列表", "List"),
    extension="txt",
    options=[
        SettingSpec(
            name="cols",
            title=T("列", "Columns"),
            default="name",
            control="multi",
            options=[("name", T("名字", "Name")), ("size", T("大小", "Size")), ("path", T("位置", "Location"))],
            default_from="cols",
            group=T("内容", "Contents"),
        )
    ],
)
def listing(x: ExportInput) -> Any:
    yield x.options["cols"] + "\\n"


@plugin.visualization("weights", T("权重", "Weights"), base="treemap", size="code", default_color="tint")
def weights(v: Any) -> Any:
    import numpy as np

    return np.ones(len(v.tree), dtype=np.float64)


@plugin.color_mode("tint", T("着色", "Tint"), palette=["muted", "accent"])
def tint(c: Any) -> Any:
    import numpy as np

    return np.ones(len(c.ids), dtype=np.uint8)
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    folder = tmp_path / "t.round3"
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    code = CODE.replace('ctx.state("seen").set({"scan": scan.scan_id})', "pass")
    (folder / "plugin.py").write_text(code, encoding="utf-8")
    return folder


TREE = make_tree({"a.log": 10, "b.txt": 20, "c.bin": 5, "sub": {"d.log": 1}})


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_texts_made_at_import_follow_the_language(plugin: Path, process: bool) -> None:
    async with MockHost.load(plugin, process=process) as host:
        host.scan(TREE)
        zh = (await host.call("columns"))["seen"]["table"]
        assert [c["label"] for c in zh["columns"]] == ["名字", "大小"]
        assert zh["rows"][0]["cells"]["name"] == "你好"
        await host.set_lang("en")
        en = (await host.call("columns"))["seen"]["table"]
        assert [c["label"] for c in en["columns"]] == ["Name", "Size"]
        assert en["rows"][0]["cells"]["name"] == "Hello"


async def test_commands_know_the_kind_of_what_is_selected(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        a_log, b_txt, c_bin, sub = (host.find(x) for x in ("a.log", "b.txt", "c.bin", "sub"))
        assert await host.command_available("only_files", node=a_log) is None
        assert await host.command_available("only_files", node=sub) is not None
        assert await host.command_available("only_files") is not None  # nothing selected
        assert await host.command_available("only_dirs", node=sub) is None
        assert await host.command_available("only_dirs", node=a_log) is not None
        assert await host.command_available("only_logs", node=b_txt) is None
        assert await host.command_available("only_logs", node=c_bin) is not None
        # check = "<method>" gets the ActionContext of the selection
        assert await host.command_available("checked", node=a_log) is None
        assert await host.command_available("checked", node=b_txt) == "not b.txt"
        assert await host.command_available("checked") == "nothing selected"


async def test_a_command_gets_where_the_user_is(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        sub = host.find("sub")
        seen = (await host.command("only_files", node=sub, zoom_node=sub, filter="*.log", extra=1))["seen"]
        assert seen == {"node": sub, "zoom": sub, "filter": "*.log", "args": {"extra": 1}}
        await host.command("copy", node=sub)
        assert host.clipboard == ["复制了"]
        with pytest.raises(KeyError, match="no command"):
            await host.command("nope")


async def test_hooks_are_checked_and_raise(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        await host.hook("scan_done")  # the scan in front by default
        with pytest.raises(ValueError, match="unknown hook event"):
            await host.hook("scan_finished")
        with pytest.raises(TypeError, match="takes"):
            await host.hook("settings", scan={})


async def test_failed_calls_raise_a_public_error(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        with pytest.raises(RequestError) as err:
            await host.call("nope")
        assert err.value.code == ErrorCode.METHOD_NOT_FOUND


async def test_multi_select_and_unit_settings(plugin: Path) -> None:
    from spaceknife import settings as settings_mod
    from spaceknife.settings import SettingsError

    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        await host.settled()
        meta = {m.key: m for m in settings_mod.meta() if m.plugin == "t.round3"}
        assert meta["plugin.t.round3.cols"].control == "multi"
        assert meta["plugin.t.round3.style"].control == "select"  # more than four options
        assert meta["plugin.t.round3.days"].unit == "天"
        # chosen values in the options' order; anything else is refused
        host.server.settings.update({"plugin.t.round3.cols": "path,name"})
        assert host.server.settings.get("plugin.t.round3.cols") == "name,path"
        with pytest.raises(SettingsError):
            host.server.settings.update({"plugin.t.round3.cols": "name,colour"})
        # an export option that follows the setting, under its group heading
        assert (await host.export_text("list")) == "name,path\n"
        from spaceknife.extensions import EXTENSIONS
        from spaceknife.server.export_handlers import _option

        c = next(c for c in EXTENSIONS.of("export_format") if c.owner == "t.round3")
        opt = _option(c.spec.options[0], "zh-CN", "t.round3", host.server)
        assert (opt.default, opt.group, opt.control) == ("name,path", "内容", "multi")


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_files_many_at_once_and_outside(plugin: Path, process: bool) -> None:
    from spaceknife_sdk import FileNotAllowed

    assert issubclass(FileNotAllowed, PermissionError)
    tree = make_tree({"a.log": b"aaaa-and-more", "b.txt": b"bbbb"})
    async with MockHost.load(plugin, process=process) as host:
        host.scan(tree)
        seen = (await host.call("hashes"))["seen"]
        root, a, b = seen["hashes"]
        assert root is None  # a folder: not read
        assert a is not None and b is not None and a != b
        assert seen["outside"] == "FileNotAllowed"


async def test_one_basket_message_a_call_and_taking_back(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(make_tree({"a.log": 10, "b.txt": 20}))
        await host.command("bin_them")
        assert {i.reason for i in host.basket} == {"甲", "乙"}
        assert len([t for t in host.toasts if "删除篮" in t]) == 1
        removed = (await host.command("take_back"))["seen"]["removed"]
        assert removed == 1 and [i.reason for i in host.basket] == ["乙"]


async def test_numbers_as_the_interface_writes_them(plugin: Path) -> None:
    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        seen = (await host.call("numbers"))["seen"]
        assert seen["count"] == "12,345"
        assert seen["size"] in ("1.5 MB", "1.5 MB")  # 1024 or 1000: the user's unit base
        assert seen["t"] in ("2.0 KB", "2.0 KB")


async def test_a_visualization_starts_with_its_own_colour_mode(plugin: Path) -> None:
    from spaceknife.layout import registry

    async with MockHost.load(plugin) as host:
        host.scan(TREE)
        await host.settled()
        assert registry.get("t.round3.weights").default_color == "t.round3.tint"
        frame = await host.visual("weights")
        # shares are of the zoom root's weight (the frame's total), so the rows never pass 100%
        assert frame.size_unit == "weight" and frame.root.size >= int(frame.draw.size.max()) > 0
