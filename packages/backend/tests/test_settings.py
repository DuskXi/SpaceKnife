"""Settings (W7): registry, validation, persistence, external edits, broadcast and live effects."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import get_args

import pytest

from spaceknife.layout import registry
from spaceknife.server import Server
from spaceknife.settings import (
    BY_KEY,
    DEFAULTS,
    REGISTRY,
    ConflictError,
    SettingsError,
    SettingsStore,
    validate,
)
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_client.views import ViewClient
from spaceknife_protocol import CAP_BINARY_FRAMES
from spaceknife_protocol import settings as ps
from spaceknife_protocol import view as pv
from spaceknife_protocol.base import Empty
from spaceknife_protocol.errors import ErrorCode
from spaceknife_protocol.methods import (
    EVENT_SETTINGS_CHANGED,
    EVENT_VIEW_FRAME,
    SETTINGS_GET,
    SETTINGS_RESET,
    SETTINGS_UPDATE,
    VIEW_OPEN,
)

from .conftest import CLIENT, TOKEN
from .test_view_server import scanned
from .test_view_visuals import FAKES

CATEGORIES = {"general", "appearance", "treemap", "scan", "classes", "tags", "keys", "advanced", "about"}


def test_registry_is_consistent() -> None:
    assert len(BY_KEY) == len(REGISTRY)
    assert validate(dict(DEFAULTS)) == DEFAULTS  # every default passes its own validation
    for s in REGISTRY:
        assert s.category in CATEGORIES and s.name and s.description, s.key
        if s.options and s.control == "segmented":
            assert s.default in [o[0] for o in s.options], s.key
        if s.min is not None:
            assert s.min <= s.default <= s.max, s.key  # type: ignore[operator]


def test_visualization_settings_match_the_protocol() -> None:
    assert [o[0] for o in BY_KEY["appearance.default_visual"].options] == list(pv.BUILTIN_VISUALS)
    modes = list(get_args(pv.ColorModeName))
    for key in ("appearance.default_color_mode", "appearance.partition_color_mode"):
        assert [o[0] for o in BY_KEY[key].options] == modes, key
    assert DEFAULTS["appearance.default_visual"] == "treemap"
    assert DEFAULTS["appearance.partition_color_mode"] == "branch"
    assert DEFAULTS["treemap.flame_origin"] == "bottom"  # classic flame graph: the root at the bottom
    with pytest.raises(SettingsError):
        validate({**DEFAULTS, "appearance.default_visual": "pie"})
    with pytest.raises(SettingsError):
        validate({**DEFAULTS, "treemap.flame_origin": "left"})


def test_update_validates_and_persists(tmp_path: Path) -> None:
    file = tmp_path / "settings.json"
    st = SettingsStore(file)
    seen: list[int] = []
    st.listeners["t"] = lambda s: seen.append(s.version)
    st.update({"scan.unit_base": "1000", "treemap.min_element": 16})
    assert st.unit_base() == 1000 and seen == [1]
    saved = json.loads(file.read_text(encoding="utf-8"))
    assert saved["values"] == {"scan.unit_base": "1000", "treemap.min_element": 16}  # only non-defaults
    assert SettingsStore(file).values == st.values
    with pytest.raises(SettingsError):
        st.update({"treemap.min_element": 99})  # out of range
    with pytest.raises(SettingsError):
        st.update({"scan.unit_base": "999"})  # not an option
    with pytest.raises(SettingsError):
        st.update({"appearance.accent": "red"})  # not a colour
    with pytest.raises(SettingsError):
        st.update({"nope": 1})
    with pytest.raises(SettingsError):
        st.update({"treemap.dynazoom": True})  # not implemented yet: read-only
    with pytest.raises(ConflictError):
        st.update({"treemap.min_element": 12}, version=0)
    st.update({"treemap.min_element": 12}, version=st.version)
    assert st.values["treemap.min_element"] == 12
    st.update({"treemap.min_element": 12})  # no change: no new version, no event
    assert seen == [1, 2]
    st.reset("treemap")
    assert st.values["treemap.min_element"] == DEFAULTS["treemap.min_element"]
    assert st.values["scan.unit_base"] == "1000"  # other pages untouched
    st.reset(None)
    assert st.values == DEFAULTS


def test_external_edits_are_picked_up(tmp_path: Path) -> None:
    file = tmp_path / "settings.json"
    st = SettingsStore(file)
    st.update({"general.hover_age": False})
    file.write_text(
        json.dumps({"version": 1, "values": {"general.hover_age": True, "tags.omit_ctrl": False}}),
        encoding="utf-8",
    )
    import os

    os.utime(file, (1, 1))  # a different mtime than the one the store wrote
    assert st.poll() and st.values["tags.omit_ctrl"] is False and st.error == ""
    file.write_text("{ broken", encoding="utf-8")
    os.utime(file, (2, 2))
    assert not st.poll() and st.error and st.values["tags.omit_ctrl"] is False  # last valid values stay
    file.write_text(json.dumps({"values": {"treemap.min_element": 1}}), encoding="utf-8")
    os.utime(file, (3, 3))
    assert not st.poll() and "最小元素尺寸" in st.error


def test_system_unit_base(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    import spaceknife.settings as mod

    st = SettingsStore(tmp_path / "s.json")
    monkeypatch.setattr(mod, "IS_MAC", False)
    monkeypatch.setattr(mod.sys, "platform", "linux")
    monkeypatch.setenv("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")
    assert st.unit_base() == 1000
    monkeypatch.setenv("XDG_CURRENT_DESKTOP", "KDE")
    assert st.unit_base() == 1024


async def test_settings_over_the_protocol(tmp_path: Path, server: Server, client: Connection) -> None:
    state = await client.call(SETTINGS_GET, Empty())
    assert state.values == DEFAULTS and {m.key for m in state.meta} == set(DEFAULTS)
    assert state.path.endswith("settings.json") and state.unit_base in (1000, 1024)
    events: list[ps.SettingsChangedEvent] = []
    client.on_event(EVENT_SETTINGS_CHANGED, events.append)
    state = await client.call(
        SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"scan.unit_base": "1000"}, version=state.version)
    )
    assert state.unit_base == 1000
    with pytest.raises(RemoteError) as err:
        await client.call(
            SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"scan.unit_base": "1000"}, version=0)
        )
    assert err.value.code == ErrorCode.SETTINGS_CONFLICT
    with pytest.raises(RemoteError) as err:
        await client.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"treemap.min_element": 0}))
    assert err.value.code == ErrorCode.INVALID_PARAMS
    await asyncio.sleep(0.05)
    assert events and events[-1].state.values["scan.unit_base"] == "1000"
    state = await client.call(SETTINGS_RESET, ps.SettingsResetParams(category="scan"))
    assert state.values["scan.unit_base"] == "system"


async def test_the_memory_settings_reach_the_mapping_policy(server: Server, client: Connection) -> None:
    from spaceknife.store import mapped

    assert mapped.POLICY.large == mapped.LARGE_BYTES and mapped.POLICY.fraction == 0.25  # the defaults
    await client.call(
        SETTINGS_UPDATE,
        ps.SettingsUpdateParams(values={"advanced.map_large_scans": False, "advanced.memory_budget": 40}),
    )
    assert mapped.POLICY.large is None and mapped.POLICY.fraction == 0.4
    await client.call(SETTINGS_RESET, ps.SettingsResetParams(category="advanced"))
    assert mapped.POLICY.large == mapped.LARGE_BYTES and mapped.POLICY.fraction == 0.25


async def test_new_views_use_the_settings_and_geometry_is_live(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    await client.call(
        SETTINGS_UPDATE,
        ps.SettingsUpdateParams(
            values={
                "treemap.initial_detail": 7,
                "appearance.default_color_mode": "time",
                "treemap.show_free": False,
            }
        ),
    )
    scan_id = await scanned(tmp_path, client)
    vc = ViewClient(client)
    update, _ = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    st = update.state
    assert (st.detail, st.color_mode, st.show_free, st.min_element) == (7, "time", False, 10)
    pushed: list[pv.ViewFrameEvent] = []
    client.on_event(EVENT_VIEW_FRAME, pushed.append)
    await client.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"treemap.min_element": 18}))
    for _ in range(50):
        if any(p.frame.reason == "settings" for p in pushed):
            break
        await asyncio.sleep(0.02)
    assert any(p.frame.reason == "settings" for p in pushed)
    assert server.views.views[st.view_id].view.min_element == 18


async def test_new_views_take_the_visualization_settings(
    tmp_path: Path, server: Server, client: Connection
) -> None:
    await client.call(
        SETTINGS_UPDATE,
        ps.SettingsUpdateParams(
            values={
                "appearance.default_visual": "flame",
                "appearance.default_color_mode": "branch",
                "appearance.partition_color_mode": "flat",
                "treemap.flame_origin": "top",
            }
        ),
    )
    scan_id = await scanned(tmp_path, client)
    for v in FAKES:
        registry.register(v)
    try:
        vc = ViewClient(client)
        update, _ = await vc.call(
            VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
        )
        st = update.state
        flame = "flame" in [v.id for v in registry.all_visuals()]  # registered by its own module
        assert (st.visual, st.color_mode) == (("flame", "flat") if flame else ("treemap", "branch"))
        view = server.views.views[st.view_id].view
        assert view.visual_options["flame"] == (("origin", "top"),)
        view.set_visual("test.rooted")  # a flame.v1 layout: 「火焰图和火焰-极坐标图默认配色」
        assert view.color_mode == "flat"
        view.set_visual("treemap")
        assert view.color_mode == "branch"
        # an older frontend: the treemap, and class colours instead of branch colours
        old = await Connection.open(server.url, TOKEN, CLIENT, capabilities=[CAP_BINARY_FRAMES])
        try:
            update, _ = await ViewClient(old).call(
                VIEW_OPEN, pv.ViewOpenParams(scan_id=scan_id, viewport=pv.Viewport(width=800, height=600))
            )
            assert (update.state.visual, update.state.color_mode) == ("treemap", "class")
        finally:
            await old.close()
        # the flame graph's direction applies to open views at once
        await client.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"treemap.flame_origin": "bottom"}))
        assert view.visual_options["flame"] == (("origin", "bottom"),)
    finally:
        for v in FAKES:
            registry.unregister(v.id)
