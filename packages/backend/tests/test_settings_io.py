"""Settings export / import (SpaceKnife's file, SpaceSniffer's configuration) and the 「高级」 tools."""

from __future__ import annotations

import json
import xml.etree.ElementTree as ET
from collections.abc import Iterator
from pathlib import Path

import pytest

from spaceknife import classes
from spaceknife.classes import DEFAULT_CLASSES
from spaceknife.server import Server
from spaceknife.settings import DEFAULTS
from spaceknife.settings_io import ImportError_, read_plan
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_protocol import settings as ps
from spaceknife_protocol.base import Empty
from spaceknife_protocol.methods import (
    CLASSES_GET,
    SETTINGS_EXPORT,
    SETTINGS_GET,
    SETTINGS_IMPORT,
    SETTINGS_UPDATE,
    SYSTEM_CACHE_CLEAR,
    SYSTEM_MAINTENANCE,
    TAGS_CLEAR_SAVED,
)

DATA = Path(__file__).parent / "data"
SS_DEFAULT = DATA / "SpaceSnifferConfig.default.xml"  # written by SpaceSniffer 2.0.5.18 itself


@pytest.fixture(autouse=True)
def isolated(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    yield
    classes.set_active(DEFAULT_CLASSES)


def test_spacesniffer_plan() -> None:
    plan = read_plan(SS_DEFAULT, dict(DEFAULTS))
    assert plan.source == "spacesniffer"
    groups = {
        g.findtext("name"): (g.findtext("extensions") or "").split(";")
        for g in ET.parse(SS_DEFAULT).getroot().iter("fileGroup")
    }
    assert plan.classes is not None
    by_name = {c.name: c for c in plan.classes}
    for name, exts in groups.items():  # UI-W7-36: its 5 classes with its extension lists
        assert by_name[name].extensions == list(dict.fromkeys(exts))
        assert by_name[name].color == ""  # SpaceSniffer's default primaries give way to the theme
    assert by_name["Video/Movies"].id == "video" and "视频" in by_name["Video/Movies"].aliases
    assert plan.classes[-1].id == "other" and "文档" in by_name
    assert plan.values["appearance.default_color_mode"] == "flat"
    assert plan.values["treemap.initial_detail"] == 3
    assert "treemap.min_element" not in plan.values  # 10 in both
    assert any("zoomTimeMs" in s for s in plan.skipped)


def test_spacesniffer_custom_colour(tmp_path: Path) -> None:
    text = SS_DEFAULT.read_text(encoding="utf-8").replace(
        "<color>0x000000FF</color>", "<color>0x00336699</color>", 1
    )
    (tmp_path / "c.xml").write_text(text, encoding="utf-8")
    plan = read_plan(tmp_path / "c.xml", dict(DEFAULTS))
    assert plan.classes is not None
    assert next(c for c in plan.classes if c.name == "Video/Movies").color == "#996633"  # BGR -> RGB


def test_bad_files(tmp_path: Path) -> None:
    for name, body in (("a.json", "{nope"), ("b.xml", "<x/>"), ("c.txt", "hello")):
        (tmp_path / name).write_text(body, encoding="utf-8")
        with pytest.raises(ImportError_):
            read_plan(tmp_path / name, dict(DEFAULTS))


async def test_round_trip_and_spacesniffer_import(tmp_path: Path, client: Connection) -> None:
    await client.call(SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"treemap.min_element": 14}))
    out = tmp_path / "mine.json"
    await client.call(SETTINGS_EXPORT, ps.SettingsFileParams(path=str(out)))
    doc = json.loads(out.read_text(encoding="utf-8"))
    assert doc["format"] == "spaceknife-settings" and doc["values"] == {"treemap.min_element": 14}

    preview = await client.call(SETTINGS_IMPORT, ps.SettingsImportParams(path=str(SS_DEFAULT)))
    assert not preview.applied and preview.classes and preview.changes
    assert (await client.call(SETTINGS_GET, Empty())).values["treemap.min_element"] == 14  # preview only
    applied = await client.call(SETTINGS_IMPORT, ps.SettingsImportParams(path=str(SS_DEFAULT), apply=True))
    assert applied.applied
    state = await client.call(SETTINGS_GET, Empty())
    assert state.values["appearance.default_color_mode"] == "flat"
    assert state.values["treemap.min_element"] == 10
    assert (await client.call(CLASSES_GET, Empty())).custom

    # our own export puts everything back, the class table included
    back = await client.call(SETTINGS_IMPORT, ps.SettingsImportParams(path=str(out), apply=True))
    assert back.source == "spaceknife" and back.classes is not None
    state = await client.call(SETTINGS_GET, Empty())
    assert (
        state.values["treemap.min_element"] == 14 and state.values["appearance.default_color_mode"] == "class"
    )
    assert not (await client.call(CLASSES_GET, Empty())).custom
    with pytest.raises(RemoteError):
        await client.call(SETTINGS_IMPORT, ps.SettingsImportParams(path=str(tmp_path / "missing.json")))


async def test_maintenance(tmp_path: Path, server: Server, client: Connection) -> None:
    from spaceknife import paths

    tags = paths.data_dir() / "tags"
    tags.mkdir(parents=True)
    (tags / "root-1.json").write_text('{"version": 1, "tags": []}', encoding="utf-8")
    cache = paths.cache_dir() / "junk"
    cache.mkdir(parents=True)
    (cache / "x.bin").write_bytes(b"x" * 1000)
    info = await client.call(SYSTEM_MAINTENANCE, Empty())
    assert info.saved_tags == 1 and info.cache_bytes >= 1000 and not info.helper_running
    info = await client.call(TAGS_CLEAR_SAVED, Empty())
    assert info.saved_tags == 0 and not tags.exists()
    info = await client.call(SYSTEM_CACHE_CLEAR, Empty())
    assert info.cache_bytes == 0
