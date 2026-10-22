"""Settings with plugins: namespaced keys, kept while their plugin is off, one bad value falling back
alone, unknown keys kept, options worked out from the registry."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from spaceknife.extensions import EXTENSIONS, Contribution
from spaceknife.settings import BY_KEY, SettingsError, SettingsStore, Spec, dynamic_options

PLUGIN_SPEC = Spec(
    key="plugin.example.unit.depth",
    category="plugins",
    group="单元",
    name="深度",
    description="",
    control="number",
    default=3,
    min=1,
    max=9,
    kind=int,
    plugin="example.unit",
)


@pytest.fixture
def plugin_setting() -> object:
    c = Contribution("setting", "example.unit.depth", "example.unit", None, PLUGIN_SPEC)
    EXTENSIONS.register(c)
    yield c
    EXTENSIONS.unregister_owner("example.unit")


def test_plugin_values_are_namespaced_and_kept(tmp_path: Path, plugin_setting: object) -> None:
    file = tmp_path / "settings.json"
    st = SettingsStore(file)
    assert st.get("plugin.example.unit.depth") == 3 and st.plugin_settings("example.unit") == {"depth": 3}
    st.update({"plugin.example.unit.depth": 7})
    with pytest.raises(SettingsError):
        st.update({"plugin.example.unit.depth": 99})
    assert json.loads(file.read_text(encoding="utf-8"))["values"] == {"plugin.example.unit.depth": 7}
    meta = {m.key: m for m in st.state().meta}["plugin.example.unit.depth"]
    assert meta.category == "plugins" and meta.plugin == "example.unit"
    # the plugin goes: its value stays in the file, and comes back with it
    EXTENSIONS.unregister_owner("example.unit")
    st.plugins_changed()
    assert "plugin.example.unit.depth" not in st.state().values
    st.update({"treemap.min_element": 16})
    assert json.loads(file.read_text(encoding="utf-8"))["values"]["plugin.example.unit.depth"] == 7
    EXTENSIONS.register(Contribution("setting", "example.unit.depth", "example.unit", None, PLUGIN_SPEC))
    st.plugins_changed()
    assert st.get("plugin.example.unit.depth") == 7
    assert SettingsStore(file).get("plugin.example.unit.depth") == 7


def test_one_bad_value_and_unknown_keys(tmp_path: Path) -> None:
    file = tmp_path / "settings.json"
    file.write_text(
        json.dumps(
            {"values": {"treemap.min_element": 999, "scan.unit_base": "1000", "someday.new_key": [1, 2]}}
        ),
        encoding="utf-8",
    )
    st = SettingsStore(file)
    assert st.get("scan.unit_base") == "1000"  # the good value stays
    assert st.get("treemap.min_element") == BY_KEY["treemap.min_element"].default and st.error
    st.update({"scan.unit_base": "1024"})
    assert json.loads(file.read_text(encoding="utf-8"))["values"]["someday.new_key"] == [1, 2]


def test_dynamic_options(tmp_path: Path) -> None:
    from dataclasses import replace

    from spaceknife.layout import registry
    from spaceknife.layout.api import ENGINE_API

    spec = BY_KEY["appearance.default_visual"]
    assert [o[0] for o in dynamic_options(spec)] == ["treemap", "flame", "flame_polar"]
    base = registry.get("flame")
    registry.register(
        replace(base, id="example.unit.files", plugin="example.unit", title_id="文件数", api=ENGINE_API)
    )
    try:
        assert [o[0] for o in dynamic_options(spec)][-1] == "example.unit.files"
        st = SettingsStore(tmp_path / "s.json")
        st.update({"appearance.default_visual": "example.unit.files"})
    finally:
        registry.unregister("example.unit.files")
    # gone: the stored value is kept, the default is in effect, and it is back with the plugin
    st2 = SettingsStore(tmp_path / "s.json")
    assert st2.get("appearance.default_visual") == "treemap"
    st2.update({"treemap.min_element": 16})
    assert (
        json.loads((tmp_path / "s.json").read_text(encoding="utf-8"))["values"]["appearance.default_visual"]
        == "example.unit.files"
    )
