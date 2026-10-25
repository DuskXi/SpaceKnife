"""A setting changed at any moment after a plugin is enabled reaches its process before the next
call (round 5's probe ``growthwatch-race``: rules written 5 ms after enabling were stored and
``settings.get`` had them, but the plugin's ``ctx.settings`` kept the old ones until the next
change).

Enabling a plugin with a ``start="startup"`` task starts its process at once. The process gets
the settings as they are when its start begins; a change while it starts was pushed to a runtime
that was not running yet, which drops it. Now the host sends whatever changed meanwhile (settings,
language, permissions, the scans' folders) as soon as the process runs, before any call."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest

from spaceknife.plugins.runtime.inprocess import InProcessRuntime
from spaceknife.plugins.runtime.process import ProcessRuntime
from spaceknife_sdk.testing import MockHost

pytestmark = pytest.mark.timeout(120)

PID = "t.startvalues"
TOML = f"""id = "{PID}"
version = "0.1.0"
name = {{ zh = "启动时的值", en = "Values at start" }}
entry = "plugin.py"
"""

CODE = """from __future__ import annotations

from typing import Any

from spaceknife_sdk import Context, Payload, Plugin, T

plugin = Plugin()
plugin.setting("word", T("词", "Word"), "old")
HEARD: list[Any] = []


class Now(Payload):
    word: str
    heard: list[Any]


@plugin.method
def now(ctx: Context) -> Now:
    return Now(word=str(ctx.settings.get("word")), heard=HEARD)


@plugin.on("settings")
def changed(ctx: Context, changed: list[str]) -> None:
    HEARD.append([changed, ctx.settings.get("word")])


@plugin.background("watch", T("盯着", "Watch"), start="startup")
def watch(ctx: Context) -> None:
    return None
"""

WORD = "new"


@pytest.fixture
def plugin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    folder = tmp_path / PID
    folder.mkdir()
    (folder / "plugin.toml").write_text(TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(CODE, encoding="utf-8")
    return folder


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_setting_changed_right_after_enabling_reaches_the_process(
    plugin: Path, process: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    starting = asyncio.Event()
    for cls in (ProcessRuntime, InProcessRuntime):
        real = cls.start

        async def slow(self: Any, real: Any = real) -> Any:
            starting.set()
            await asyncio.sleep(0.3)  # a packaged app's process takes this long to start, or more
            return await real(self)

        monkeypatch.setattr(cls, "start", slow)
    async with MockHost.load(plugin, process=process) as host:
        hp = host.server.plugins
        rec = hp.records[PID]
        await hp.disable(PID)
        starting.clear()
        info = await hp.enable(PID, rec.hash, sorted(rec.requested))
        assert info.state == "enabled"
        await asyncio.wait_for(starting.wait(), 10.0)  # its startup task starts the process now
        rt = hp.records[PID].runtime  # (enable reads the record again: a new one)
        assert rt is not None and rt.state != "running"
        # the settings page, at once (not MockHost.set_setting, which pushes the values itself)
        host.server.settings.update({f"plugin.{PID}.word": WORD})
        got = await host.call("now")
        assert got["word"] == WORD
        await host.settled()
        heard = (await host.call("now"))["heard"]
        assert all(h[1] == WORD for h in heard)  # a settings hook, if it ran, saw them too


@pytest.mark.parametrize("process", [False, True], ids=["inprocess", "process"])
async def test_a_setting_changed_at_once_after_enabling_reaches_the_process(
    plugin: Path, process: bool
) -> None:
    """The same without slowing anything down: enable, change, ask."""
    async with MockHost.load(plugin, process=process) as host:
        hp = host.server.plugins
        rec = hp.records[PID]
        for i in range(3):
            await hp.disable(PID)
            await hp.enable(PID, rec.hash, sorted(rec.requested))
            word = f"{WORD} {i}"
            host.server.settings.update({f"plugin.{PID}.word": word})
            assert (await host.call("now"))["word"] == word
