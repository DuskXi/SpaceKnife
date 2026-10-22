"""Plugins over the socket: management (confirm, hash pinning, enable / disable), plugin methods with
JSON and binary results, events, shared states and streams, and sessions without plugins.v1."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest

from spaceknife.plugins import paths as ppaths
from spaceknife.server import Server, ServerConfig
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import CAP_BINARY_FRAMES, CAP_SUBSCRIPTIONS, CAP_VISUALS
from spaceknife_protocol import plugins as pp
from spaceknife_protocol import settings as ps
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_EXTENSIONS,
    EXTENSIONS_GET,
    PALETTE_COMMANDS,
    PLUGIN_DESCRIBE,
    PLUGIN_DISABLE,
    PLUGIN_ENABLE,
    PLUGIN_LIST,
    SETTINGS_GET,
    SETTINGS_UPDATE,
    SUBSCRIBE,
)
from spaceknife_protocol.streams import SubscribeParams

from .plugin_helpers import CLIENT, TOKEN, connect, copy_plugin, plugin_server

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


async def _server() -> Server:
    srv = Server(ServerConfig(token=TOKEN, hello_timeout=0.5, ping_interval=None))
    await srv.start()
    return srv


async def test_confirm_enable_change_and_disable() -> None:
    copy_plugin("t.full", ppaths.plugins_dir())
    srv = await _server()
    try:
        async with connect(srv) as conn:
            changed: list[pp.ExtensionsChangedEvent] = []
            conn.on_event(EVENT_EXTENSIONS, changed.append)
            listed = {p.id: p for p in (await conn.call(PLUGIN_LIST, Empty())).plugins}
            assert listed["t.full"].state == "needs_confirm" and listed["t.full"].hash.startswith("sha256:")
            desc = await conn.call(PLUGIN_DESCRIBE, pp.PluginIdParams(id="t.full"))
            assert [p.id for p in desc.plugin.permissions] == ["tree.read", "files.read", "basket.propose"]
            assert (
                desc.unsigned and "command: 大文件报告" in desc.summary
            )  # declared in plugin.toml (the code has not run)
            with pytest.raises(RemoteError) as err:
                await conn.call(
                    PLUGIN_ENABLE,
                    pp.PluginEnableParams(
                        id="t.full", hash="sha256:nope", grant=["tree.read", "files.read", "basket.propose"]
                    ),
                )
            assert err.value.code == ErrorCode.INVALID_PARAMS
            with pytest.raises(RemoteError):
                await conn.call(
                    PLUGIN_ENABLE,
                    pp.PluginEnableParams(id="t.full", hash=desc.plugin.hash, grant=["tree.read"]),
                )
            # nothing of it exists before it is enabled
            assert "t.full.big" not in {c.id for c in (await conn.call(PALETTE_COMMANDS, Empty())).commands}
            info = await conn.call(
                PLUGIN_ENABLE,
                pp.PluginEnableParams(
                    id="t.full", hash=desc.plugin.hash, grant=["tree.read", "files.read", "basket.propose"]
                ),
            )
            assert info.state == "enabled" and info.contributions["command"] == 2
            cmds = {c.id: c for c in (await conn.call(PALETTE_COMMANDS, Empty())).commands}
            assert cmds["t.full.big"].plugin == "t.full" and cmds["t.full.big"].action is not None
            assert cmds["t.full.big"].keys == ["Ctrl+Alt+B"]
            ext = await conn.call(EXTENSIONS_GET, Empty())
            assert [r.id for r in ext.reports] == ["t.full.big"] and ext.reports[
                0
            ].method == "plugin.t.full.big"
            assert {f.id for f in ext.node_fields} == {"t.full.name_len", "t.full.ext"}
            assert [s.id for s in ext.sources] == ["t.full.zip"] and [c.id for c in ext.cleanups] == [
                "t.full.logs"
            ]
            for _ in range(50):
                if changed:
                    break
                await asyncio.sleep(0.02)
            assert changed and "command" in changed[-1].kinds
            # its setting: changed, kept while the plugin is off, back when it is on again
            await conn.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"plugin.t.full.min_size": 7}))
            await conn.call(PLUGIN_DISABLE, pp.PluginIdParams(id="t.full"))
            state = await conn.call(SETTINGS_GET, Empty())
            assert "plugin.t.full.min_size" not in state.values
            saved = json.loads(srv.settings.file.read_text(encoding="utf-8"))["values"]
            assert saved["plugin.t.full.min_size"] == 7
            with pytest.raises(RemoteError) as err:
                await conn.call_raw("plugin.t.full.add", {"a": 1})
            assert err.value.code in (ErrorCode.PLUGIN_DISABLED, ErrorCode.METHOD_NOT_FOUND)
            info = await conn.call(
                PLUGIN_ENABLE,
                pp.PluginEnableParams(
                    id="t.full", hash=desc.plugin.hash, grant=["tree.read", "files.read", "basket.propose"]
                ),
            )
            assert (await conn.call(SETTINGS_GET, Empty())).values["plugin.t.full.min_size"] == 7
    finally:
        await srv.close()
    # its files change: it is "changed" until confirmed again
    (ppaths.plugins_dir() / "t.full" / "plugin.py").write_text(
        (ppaths.plugins_dir() / "t.full" / "plugin.py").read_text(encoding="utf-8") + "\n# changed\n",
        encoding="utf-8",
    )
    srv = await _server()
    try:
        assert srv.plugins.records["t.full"].state == "changed"
        assert not [c for c in srv.plugins.records["t.full"].keys]
        async with connect(srv) as conn:
            desc = await conn.call(PLUGIN_DESCRIBE, pp.PluginIdParams(id="t.full"))
            info = await conn.call(
                PLUGIN_ENABLE,
                pp.PluginEnableParams(
                    id="t.full", hash=desc.plugin.hash, grant=["tree.read", "files.read", "basket.propose"]
                ),
            )
            assert info.state == "enabled"
    finally:
        await srv.close()


@pytest.mark.parametrize("runtime", ["inprocess", "process"])
async def test_plugin_methods_events_states_and_streams(runtime: str) -> None:
    async with plugin_server("t.full", runtime=runtime) as srv, connect(srv) as conn:
        events: list[dict[str, Any]] = []
        conn.on_raw("plugin.t.full.progress", events.append)
        assert await conn.call_raw("plugin.t.full.add", {"a": 4, "b": 5}) == {"total": 9}
        for _ in range(50):
            if events:
                break
            await asyncio.sleep(0.02)
        assert events == [{"done": 1, "total": 1}]
        with pytest.raises(RemoteError) as err:
            await conn.call_raw("plugin.t.full.add", {"a": "x"})
        assert err.value.code == ErrorCode.INVALID_PARAMS
        with pytest.raises(RemoteError) as err:
            await conn.call_raw("plugin.t.full.nothing", {})
        assert err.value.code == ErrorCode.METHOD_NOT_FOUND
        # binary results come as a frame stamped with the request
        frames: list[Any] = []
        conn.on_binary(lambda header, cols: frames.append((header, cols)))
        assert (await conn.call_raw("plugin.t.full.columns", {}))["columns"] is True
        for _ in range(50):
            if frames:
                break
            await asyncio.sleep(0.02)
        header, cols = frames[0]
        assert header.stream == "plugin.t.full.columns" and header.request_id is not None
        assert cols["double"].tolist() == [0, 2, 4, 6, 8]
        # shared state: the current value, then every change
        states: list[dict[str, Any]] = []
        conn.on_raw("plugin.t.full.status", states.append)
        sub = (await conn.call(SUBSCRIBE, SubscribeParams(topic="plugin.t.full.status"))).sub
        await conn.call_raw("plugin.t.full.add", {"a": 1})
        for _ in range(100):
            if len(states) >= 2:
                break
            await asyncio.sleep(0.02)
        assert states[0]["sub"] == sub and states[0]["value"] == {"runs": 1}
        assert states[-1]["value"] == {"runs": 2}
        # a stream: items until it ends
        ticks: list[dict[str, Any]] = []
        conn.on_raw("plugin.t.full.ticks", ticks.append)
        await conn.call(SUBSCRIBE, SubscribeParams(topic="plugin.t.full.ticks"))
        for _ in range(100):
            if len(ticks) >= 3:
                break
            await asyncio.sleep(0.02)
        assert [t["value"]["done"] for t in ticks] == [0, 1, 2]


async def test_sessions_without_plugins() -> None:
    async with plugin_server("t.full") as srv:
        caps = [CAP_BINARY_FRAMES, CAP_SUBSCRIPTIONS, CAP_VISUALS]
        conn = await Connection.open(srv.url, TOKEN, CLIENT, capabilities=caps)
        try:
            with pytest.raises(RemoteError) as err:
                await conn.call(PLUGIN_LIST, Empty())
            assert err.value.code == ErrorCode.INVALID_REQUEST
            with pytest.raises(RemoteError) as err:
                await conn.call_raw("plugin.t.full.add", {"a": 1})
            assert err.value.code == ErrorCode.INVALID_REQUEST
            cmds = (await conn.call(PALETTE_COMMANDS, Empty())).commands
            assert all(c.plugin is None and c.action is None for c in cmds)
            assert "t.full.big" not in {c.id for c in cmds}
        finally:
            await conn.close()
