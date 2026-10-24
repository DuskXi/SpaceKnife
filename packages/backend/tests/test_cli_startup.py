"""The command-line scan's start-up and exit (docs/packaging.md, 命令行扫描的启动): what it imports, the
language read without the settings models, the largest files without a full sort."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

from spaceknife.cli_scan import stored_language
from spaceknife.settings import DEFAULTS, SettingsStore
from spaceknife.store.subtree import largest

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


def test_scan_exits_with_its_output_complete(tmp_path: Path) -> None:
    """main() ends the process right after the output (cli._end_now): everything is written, the
    exit status is kept, and an auto-saved snapshot is complete on disk."""
    facts = build_tree(tmp_path / "root")
    env = {**os.environ, "SPACEKNIFE_HOME": str(tmp_path / "home"), "COLUMNS": "120"}
    run = subprocess.run(
        [sys.executable, "-X", "utf8", "-m", "spaceknife", "scan", str(facts.root), "--top", "3"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        timeout=120,
    )
    assert run.returncode == 0, run.stderr
    assert "movie.mp4" in run.stdout[run.stdout.index("最大的文件") :]  # the last table is there
    from spaceknife.store import snapshot

    (snap,) = (tmp_path / "home").rglob("*.sks")
    store, header = snapshot.load(snap)
    assert header.node_count == len(store) and len(store) > 1
    bad = subprocess.run(
        [sys.executable, "-m", "spaceknife", "scan", str(tmp_path / "missing")],
        capture_output=True,
        env=env,
        timeout=120,
    )
    assert bad.returncode == 2
    args = ["scan", str(facts.root), "--workers", "2", "--no-snapshot", "--top", "3"]
    pooled = subprocess.run(  # the Python walker's worker processes: the usual exit waits for them
        [sys.executable, "-X", "utf8", "-m", "spaceknife", *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env={**env, "SPACEKNIFE_NO_NATIVE": "1"},
        timeout=120,
    )
    assert pooled.returncode == 0, pooled.stderr
    assert "movie.mp4" in pooled.stdout[pooled.stdout.index("最大的文件") :]


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


def test_largest_matches_a_stable_sort() -> None:
    rng = np.random.default_rng(7)
    ids = np.sort(rng.choice(10_000, size=3_000, replace=False)).astype(np.int64)
    for alloc in (rng.integers(0, 50, size=ids.size), np.zeros(ids.size, dtype=np.int64)):
        alloc = alloc.astype(np.int64)
        for k in (0, 1, 5, 17, 2_999, 3_000, 5_000):
            expected = ids[np.argsort(-alloc, kind="stable")[:k]]
            got, sizes = largest(ids, alloc, k)
            assert np.array_equal(got, expected), k
            assert np.array_equal(sizes, alloc[np.argsort(-alloc, kind="stable")[:k]])
