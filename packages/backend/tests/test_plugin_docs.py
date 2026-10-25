"""docs/plugin-dev.md stays true: its examples run, every field of every kind is documented, every
anchor the tools point at exists, and it stays short enough to read in one go."""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from spaceknife.plugins.check import ANCHORS, check_plugin
from spaceknife_sdk import docs, scaffold
from spaceknife_sdk.manifest import KINDS, Manifest

ROOT = Path(__file__).resolve().parents[3]
DOC = ROOT / "docs" / "plugin-dev.md"
TEXT = DOC.read_text(encoding="utf-8")
BLOCK = re.compile(r"^```(\S+)(?: (\S+))?\n(.*?)^```", re.M | re.S)

HEADER_TOML = """id = "docs.examples"
version = "0.1.0"
name = { zh = "文档里的例子", en = "The docs' examples" }
entry = "plugin.py"
permissions = ["tree.read", "files.read", "snapshots.read", "basket.propose"]
"""
HEADER_PY = """from __future__ import annotations

import asyncio
import zipfile
from collections.abc import AsyncIterator, Iterator
from pathlib import Path, PurePath

import numpy as np

from spaceknife_sdk import *  # noqa: F403

plugin = Plugin()  # noqa: F405
"""
# the section of each kind (its anchor) and the spec model whose fields it must list
SECTIONS = {
    "command": "kind-command",
    "keybinding": "kind-keybinding",
    "setting": "kind-setting",
    "report": "kind-report",
    "node_field": "kind-node_field",
    "insight": "kind-insight",
    "panel": "kind-panel",
    "visualization": "kind-visualization",
    "color_mode": "kind-color_mode",
    "export_format": "kind-export_format",
    "export_template": "kind-export_template",
    "insight_rules": "kind-insight_rules",
    "file_class": "kind-file_class",
    "folder_rule": "kind-file_class",
    "node_set": "kind-node_set",
    "filter_preset": "kind-filter_preset",
    "cleanup": "kind-cleanup",
    "scan_source": "kind-scan_source",
    "watcher": "kind-scan_source",
    "change_annotator": "kind-change_annotator",
    "hook": "kind-hook",
    "background": "kind-background",
    "icon": "icons",
}


def _blocks(tag: str) -> list[str]:
    return [m.group(3) for m in BLOCK.finditer(TEXT) if m.group(2) == tag]


def test_the_examples_make_a_working_plugin(tmp_path: Path) -> None:
    folder = tmp_path / "docs.examples"
    (folder / "web").mkdir(parents=True)
    (folder / "templates").mkdir()
    (folder / "insights").mkdir()
    (folder / "web" / "index.html").write_text("<!doctype html><title>board</title>", encoding="utf-8")
    tomls, pys = _blocks("ex"), []
    pys = [m.group(3) for m in BLOCK.finditer(TEXT) if m.group(1) == "python" and m.group(2) == "ex"]
    tomls = [m.group(3) for m in BLOCK.finditer(TEXT) if m.group(1) == "toml" and m.group(2) == "ex"]
    assert len(pys) >= 15 and len(tomls) >= 10
    (folder / "plugin.toml").write_text(HEADER_TOML + "\n" + "\n".join(tomls), encoding="utf-8")
    (folder / "plugin.py").write_text(HEADER_PY + "\n\n" + "\n\n".join(pys), encoding="utf-8")
    (folder / "templates" / "top.sktpl").write_text(_blocks("ex-template")[0], encoding="utf-8")
    (folder / "insights" / "docs.toml").write_text(_blocks("ex-rules")[0], encoding="utf-8")
    result = check_plugin(folder, process=False, subprocess_describe=False)
    errors = [i.text() for i in result.errors()]
    assert errors == []
    assert result.description is not None
    kinds = {c["kind"] for c in result.description.contributions}
    assert {"command", "keybinding", "setting", "report", "node_field", "insight", "panel"} <= kinds
    assert {"visualization", "color_mode", "export_format", "export_template", "file_class"} <= kinds
    assert {"folder_rule", "node_set", "filter_preset", "cleanup", "scan_source", "watcher"} <= kinds
    assert {"change_annotator", "hook"} <= kinds


@pytest.mark.parametrize("kind", sorted(SECTIONS))
def test_every_field_is_documented(kind: str) -> None:
    section = docs.section(TEXT, SECTIONS[kind])
    assert section is not None, SECTIONS[kind]
    model = KINDS[kind][0]
    for name, info in model.model_fields.items():
        field = info.alias or name
        assert f"`{field}`" in section or (kind == "icon" and field in ("name", "file")), (kind, field)


def test_every_manifest_field_is_documented() -> None:
    section = docs.section(TEXT, "manifest")
    assert section is not None
    tables = {t for _kind, (_m, t) in KINDS.items() if t}
    for name, info in Manifest.model_fields.items():
        if (info.alias or name) in tables:
            continue  # the contributions' tables: section 2 and 3
        assert f"`{info.alias or name}`" in section, name


def test_the_anchors_the_tools_name_exist() -> None:
    have = {a for a, _title in docs.anchors(TEXT)}
    wanted = set(ANCHORS.values()) | {p.doc for p in scaffold.PARTS.values()} | set(SECTIONS.values())
    assert wanted - have == set()
    assert len(have) == len(docs.anchors(TEXT))  # no anchor twice


def test_it_is_short() -> None:
    lines = TEXT.splitlines()
    assert len(lines) <= 1000
    quick = docs.section(TEXT, "quickstart")
    assert quick is not None and len(quick.splitlines()) <= 100


def test_the_cli_finds_it() -> None:
    assert docs.path() == DOC
    part = docs.section(TEXT, "kind-report")
    assert part is not None and part.startswith('<a id="kind-report"></a>') and "#### 报告" in part
    assert '<a id="kind-node_field"></a>' not in part  # stops at the next section


QUICK = (ROOT / "docs" / "plugin-quick.md").read_text(encoding="utf-8")


def test_the_quick_reference_is_one_short_page() -> None:
    # what an author reads first (plugin docs prints it): a fraction of the guide
    assert len(QUICK.encode("utf-8")) < 12_000
    assert len(QUICK.encode("utf-8")) * 5 < len(TEXT.encode("utf-8"))


def test_the_quick_reference_points_at_what_exists() -> None:
    from spaceknife_sdk import report
    from spaceknife_sdk.testing import MockHost

    for anchor in set(re.findall(r"`((?:kind|api|quick)-[a-z_]+)`", QUICK)):
        assert docs.find(anchor) is not None, anchor
    for name in set(re.findall(r"\bhost\.([a-z_]+)\b", QUICK)):
        assert hasattr(MockHost, name), name
    for name in set(re.findall(r"\br\.([a-z_]+)\(", QUICK)):
        assert hasattr(report, name), name
    parts = re.search(r"`--with` 的部分[^`]*`([^`]+)`", QUICK)
    assert parts is not None
    assert set(parts.group(1).split()) == set(scaffold.PARTS)
