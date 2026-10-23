"""The reference plugins pass ``plugin check --strict``: the shipped ones (builtin.export,
builtin.insights) and the examples (examples/plugins/, whose own tests pytest collects from there)."""

from __future__ import annotations

from pathlib import Path

import pytest

from spaceknife.plugins import paths as ppaths
from spaceknife.plugins.check import check_plugin

ROOT = Path(__file__).resolve().parents[3]
EXAMPLES = sorted(p for p in (ROOT / "examples" / "plugins").iterdir() if (p / "plugin.toml").is_file())
SHIPPED = sorted(p for p in ppaths.builtin_dir().iterdir() if (p / "plugin.toml").is_file())

pytestmark = pytest.mark.timeout(240)


def test_the_examples_are_there() -> None:
    assert [p.name for p in EXAMPLES] == ["example.devpack", "example.dupes", "example.panel"]
    assert [p.name for p in SHIPPED] == ["builtin.export", "builtin.insights"]
    for folder in EXAMPLES:
        assert (folder / "tests" / "test_plugin.py").is_file()


@pytest.mark.parametrize("folder", EXAMPLES + SHIPPED, ids=lambda p: p.name)
def test_check_strict(folder: Path) -> None:
    result = check_plugin(folder)  # code in a plugin process, as the app runs it
    assert result.ok(strict=True), [i.text() for i in result.issues]
    assert result.description is not None and result.plugin == folder.name


def test_the_examples_use_only_the_public_sdk() -> None:
    """No host internals: what a third-party plugin can write (the shipped ones may use the host)."""
    for folder in EXAMPLES:
        for py in folder.rglob("*.py"):
            if "tests" in py.relative_to(folder).parts:
                continue
            text = py.read_text(encoding="utf-8")
            assert "import spaceknife." not in text and "from spaceknife." not in text, py
            assert "_runtime" not in text and "spaceknife_sdk._" not in text, py
