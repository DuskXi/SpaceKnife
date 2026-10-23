"""docs/plugin-api.md stays true: its generated tables are the code's, every object the SDK exports
is in it, and the links between it and plugin-dev.md lead somewhere."""

from __future__ import annotations

import re
from pathlib import Path

import gen_plugin_api as g
from spaceknife_sdk import docs

ROOT = Path(__file__).resolve().parents[2]
API = (ROOT / "docs" / "plugin-api.md").read_text(encoding="utf-8")
DEV = (ROOT / "docs" / "plugin-dev.md").read_text(encoding="utf-8")


def test_generated_blocks_are_current() -> None:
    assert g.main(["--check"]) == 0, "run: uv run python tools/gen_plugin_api.py"


def test_every_exported_object_is_listed() -> None:
    """A model or dataclass newly exported from spaceknife_sdk must get a table (or be machinery)."""
    import dataclasses

    from pydantic import BaseModel

    import spaceknife_sdk as sdk

    listed = {cls.__name__ for _g, cls, _w in g._models()}
    for name in sdk.__all__:
        obj = getattr(sdk, name)
        data = isinstance(obj, type) and (issubclass(obj, BaseModel) or dataclasses.is_dataclass(obj))
        if data and name not in g.NOT_DATA:
            assert name in listed, f"{name}: add it to gen_plugin_api._models()"
    for _g, cls, _w in g._models():
        for field, *_rest in g.model_rows(cls):
            assert f"| `{field}` |" in API, (cls.__name__, field)


def test_anchors_are_unique_and_links_lead_somewhere() -> None:
    api = [a for a, _t in docs.anchors(API)]
    dev = [a for a, _t in docs.anchors(DEV)]
    assert all(a.startswith("api-") for a in api)
    assert len(set(api)) == len(api)
    assert not set(api) & set(dev)  # one anchor names one section of either file
    for target in re.findall(r"\]\(plugin-api\.md#([a-z0-9_-]+)\)", DEV):
        assert target in api, target
    for target in re.findall(r"\]\(plugin-dev\.md#([a-z0-9_-]+)\)", API):
        assert target in dev, target
    for target in re.findall(r"\]\(#([a-z0-9_-]+)\)", API):
        assert target in api, target


def test_the_cli_reads_both() -> None:
    assert docs.path(docs.API) == ROOT / "docs" / "plugin-api.md"
    part = docs.find("api-cache")
    assert part is not None
    assert part.startswith('<a id="api-cache"></a>')
    assert "## 6. 缓存" in part
    assert (docs.find("kind-report") or "").startswith('<a id="kind-report"></a>')
    assert docs.find("no-such-anchor") is None


def test_the_icon_list_and_commands_are_the_apps() -> None:
    import json

    icons = json.loads((ROOT / "design" / "icons" / "icons.json").read_text(encoding="utf-8"))["icons"]
    assert all(f"`{name}`" in API for name in icons)
    assert "| `report.export` |" in API
    assert "| `view.rescan` |" in API


def test_the_sections_the_tools_name_exist() -> None:
    """Every ``plugin docs --section X`` a message of the tools suggests leads to a section."""
    sources = [
        ROOT / "packages" / "backend" / "src" / "spaceknife" / "plugins" / "check.py",
        ROOT / "packages" / "backend" / "src" / "spaceknife" / "cli_plugin.py",
        ROOT / "packages" / "sdk" / "src" / "spaceknife_sdk" / "scaffold.py",
    ]
    named = {
        m for f in sources for m in re.findall(r"--section ([a-z0-9_.-]+)", f.read_text(encoding="utf-8"))
    }
    assert named
    for anchor in named:
        assert docs.find(anchor) is not None, anchor
