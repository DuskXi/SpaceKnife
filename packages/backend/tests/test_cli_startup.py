"""The command-line scan's start-up (docs/packaging.md, 命令行扫描的启动): what it imports, the
language read without the settings models."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from spaceknife.cli_scan import stored_language
from spaceknife.settings import DEFAULTS, SettingsStore

from .tree_fixture import build_tree

# loaded by other commands or by the app, never by `scan --no-snapshot` (each costs start-up time)
HEAVY = (
    "pydantic",
    "spaceknife_protocol",
    "spaceknife.settings",
    "spaceknife.io.governor",
    "spaceknife.io.registry",
    "spaceknife.store.snapshot",
    "spaceknife.export",
    "spaceknife.server",
    "rich.progress",
)


def test_scan_without_snapshot_leaves_the_heavy_modules_alone(tmp_path: Path) -> None:
    root = build_tree(tmp_path / "root").root
    code = (
        "import sys\n"
        "from typer.testing import CliRunner\n"
        "from spaceknife.cli import app\n"
        f"r = CliRunner().invoke(app, ['scan', {str(root)!r}, '--no-snapshot', '--top', '2'])\n"
        "assert r.exit_code == 0, r.output\n"
        "assert 'movie.mp4' in r.output, r.output\n"
        f"heavy = {HEAVY!r}\n"
        "print(','.join(sorted(m for m in sys.modules if m.startswith(heavy))))\n"
    )
    env = {**os.environ, "SPACEKNIFE_HOME": str(tmp_path / "home")}
    env.pop("SPACEKNIFE_LANGUAGE", None)  # read the (missing) settings file as a user's scan does
    out = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, encoding="utf-8", env=env, timeout=120
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "", f"imported at start-up: {out.stdout.strip()}"


@pytest.mark.parametrize(
    "content",
    [
        None,  # no settings file
        "",
        "{not json",
        "[]",
        '{"values": []}',
        '{"values": null}',
        "{}",
        '{"version": 1, "values": {}}',
        '{"values": {"general.language": "system"}}',
        '{"values": {"general.language": "en"}}',
        '{"values": {"general.language": "zh-CN", "general.hover_dates": false}}',
        '{"values": {"general.language": "en", "general.hover_dates": "no"}}',  # invalid file: defaults
        '{"values": {"general.language": "fr"}}',
        '{"values": {"general.language": 5}}',
        '{"version": "x", "values": {"general.language": "en"}}',
        '{"values": {"general.language": "system", "general.hover_dates": "no"}}',
    ],
)
def test_stored_language_reads_what_the_settings_store_reads(tmp_path: Path, content: str | None) -> None:
    file = tmp_path / "settings.json"
    if content is not None:
        file.write_text(content, encoding="utf-8")
    assert stored_language(file) == str(SettingsStore(file).get("general.language"))


def test_the_fast_language_path_assumes_the_default() -> None:
    assert DEFAULTS["general.language"] == "system"
