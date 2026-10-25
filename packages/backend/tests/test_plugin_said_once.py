"""What a plugin's errors and log lines say, and that they say it once (round 5's small items).

- A command's check that raises a plain exception made the command unavailable with only
  「检查出错：plugin error: ValueError」: the exception's own message went nowhere but the plugin's
  log. A call's error now carries it (on one line, at most ``ERROR_TEXT_MAX`` characters), and the
  check's reason reads 「检查出错：ValueError: …」 / "The check failed: ValueError: …".
- ``plugin dev`` printed each warning of the plugin twice: as ``log warning: …`` (it prints what
  the plugin logs) and again through the terminal's log sink; ``plugin call`` the same at the end
  (``log WARNING: …``). The terminal sinks now leave out the lines the command prints itself; the
  host's own warnings about the plugin still show, and the log file and the log panel keep all."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest
from loguru import logger

from spaceknife.log import on_terminal
from spaceknife.plugins import host as host_mod
from spaceknife.server.rpc import RpcError
from spaceknife_protocol import palette as ppal
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import PALETTE_CHECK_MANY
from spaceknife_sdk._runtime import ERROR_TEXT_MAX
from spaceknife_sdk.testing import MockHost, make_tree

pytestmark = pytest.mark.timeout(120)

PID = "t.saidonce"
TOML = f"""id = "{PID}"
version = "0.1.0"
name = {{ zh = "说一遍", en = "Said once" }}
entry = "plugin.py"
permissions = ["tree.read"]
"""

CODE = """from __future__ import annotations

from spaceknife_sdk import ActionContext, Context, Plugin, T

plugin = Plugin()


@plugin.method
def check_kaputt(ctx: Context, a: ActionContext) -> str | None:
    raise ValueError("kaputt:\\n  the index is gone")


@plugin.method
def check_long(ctx: Context, a: ActionContext) -> str | None:
    raise RuntimeError("x" * 5000)


@plugin.method
def fails(ctx: Context) -> None:
    raise KeyError("no such thing")


plugin.command("kaputt", T("坏的", "Broken"), check="check_kaputt", action={"open": "settings"})
plugin.command("long", T("长的", "Long"), check="check_long", action={"open": "settings"})
"""

DEV_TOML = """id = "example.once"
version = "0.1.0"
name = { zh = "一遍", en = "Once" }
entry = "plugin.py"
permissions = ["tree.read"]
"""

DEV_CODE = """from __future__ import annotations

from spaceknife_sdk import Context, Plugin, ScanInfo

plugin = Plugin()


@plugin.on("start")
def careful(ctx: Context) -> None:
    ctx.log.warning("careful: said once")


@plugin.on("scan_done")
def broken(ctx: Context, scan: ScanInfo) -> None:
    raise ValueError("a hook that fails")
"""


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_MOCK_TREES", str(tmp_path / "trees"))
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / PID
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_check_that_raises_says_its_own_message(
    plugin: Path, process: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_mod, "CHECK_TIMEOUT", 1.0)  # a loaded test machine: a check answers
    async with MockHost.load(plugin, process=process) as host:
        sid = host.scan(make_tree({"a": {"x.bin": 100}}))
        await host.settled()
        vhost = host.server.views.open(
            host.session, pv.ViewOpenParams(scan_id=sid, viewport=pv.Viewport(width=800, height=600))
        )
        context = ppal.PaletteContext(tab="scan", scan_id=sid, view_id=vhost.view.id, node=host.find("a"))
        many = host.server.router.get(PALETTE_CHECK_MANY.name)
        assert many is not None

        async def reasons() -> dict[str, str]:
            got = await many.handler(
                host.session,
                ppal.PaletteCheckManyParams(ids=[f"{PID}.kaputt", f"{PID}.long"], context=context),
            )
            assert not any(c.enabled for c in got.commands)
            return {c.id.rsplit(".", 1)[1]: str(c.status) for c in got.commands}

        said = await reasons()
        assert said["kaputt"] == "检查出错：ValueError: kaputt: the index is gone"
        assert said["long"].startswith("检查出错：RuntimeError: xxx") and said["long"].endswith("…")
        assert len(said["long"]) == len("检查出错：") + host_mod.CHECK_REASON_MAX
        await host.set_lang("en")
        said = await reasons()
        assert said["kaputt"] == "The check failed: ValueError: kaputt: the index is gone"

        # any call: the error carries the message too (bounded); the log has the traceback
        with pytest.raises(RpcError) as err:
            await host.call("fails")
        assert err.value.message == "plugin error: KeyError: 'no such thing'"
        assert ERROR_TEXT_MAX >= 200


async def test_plugin_dev_prints_each_log_line_of_the_plugin_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from spaceknife.devui import _dev

    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("SPACEKNIFE_PLUGIN_DEV", "1")
    folder = tmp_path / "example.once"
    folder.mkdir()
    (folder / "plugin.toml").write_text(DEV_TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(DEV_CODE, encoding="utf-8")
    data = tmp_path / "data"
    data.mkdir()
    (data / "a.txt").write_text("hi", encoding="utf-8")
    terminal: list[str] = []  # what the terminal's log sink (setup_logging) prints
    sink = logger.add(
        lambda m: terminal.append(str(m)), level="WARNING", format="{message}", filter=on_terminal
    )
    said: list[str] = []
    stop = asyncio.Event()
    task = asyncio.create_task(_dev(folder, "example.once", data, 0, None, [], said.append, stop, None))
    try:
        loop = asyncio.get_running_loop()
        end = loop.time() + 30
        while not (
            any("a hook that fails" in t for t in terminal) and "log warning: careful: said once" in said
        ):
            assert loop.time() < end, (said, terminal)
            await asyncio.sleep(0.05)
        await asyncio.sleep(0.3)
        assert said.count("log warning: careful: said once") == 1
        assert not [t for t in terminal if "careful: said once" in t]  # not again through the sink
        # the host's own warning about the plugin is not a line the plugin logged: it still shows
        assert [t for t in terminal if "a hook that fails" in t]
        stop.set()
        await asyncio.wait_for(task, 30)
        logger.bind(source="plugin:example.once", plugin_log=True).warning("after plugin dev")
        assert [t for t in terminal if "after plugin dev" in t]  # (the filter went with it)
    finally:
        stop.set()
        if not task.done():
            await asyncio.wait_for(task, 30)
        logger.remove(sink)


def test_plugin_call_prints_the_plugin_log_once() -> None:
    from spaceknife.cli_plugin import _plugin_logs

    terminal: list[str] = []
    sink = logger.add(
        lambda m: terminal.append(str(m)), level="WARNING", format="{message}", filter=on_terminal
    )
    try:
        with _plugin_logs("t.call") as lines:
            logger.bind(source="plugin:t.call").warning("from the plugin")
            logger.warning("hook x of t.call failed: why")
            logger.warning("something else")
        assert lines == ["WARNING: from the plugin", "WARNING: hook x of t.call failed: why"]
        assert [t.strip() for t in terminal] == ["something else"]
        logger.bind(source="plugin:t.call").warning("after the call")  # the terminal has it again
        assert [t.strip() for t in terminal] == ["something else", "after the call"]
    finally:
        logger.remove(sink)
