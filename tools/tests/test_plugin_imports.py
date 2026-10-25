"""packaging/plugin_imports.py: every module plugin check lets a plugin import goes into the frozen
backend, and the build imports each one in a plugin process of it (docs/packaging.md
「插件能 import 的模块」). The frozen half runs in tools/build_package.py; here the list, its agreement
with plugin check, and the probe end to end against the development backend."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

from spaceknife_sdk.imports import STDLIB, THIRD_PARTY, check_imports

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "packaging" / "plugin_imports.py"


def load() -> ModuleType:
    spec = importlib.util.spec_from_file_location("plugin_imports", SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def listed() -> list[str]:
    """``--list`` in a process of its own, as the spec runs it (it imports every module)."""
    out = subprocess.run(
        [sys.executable, str(SCRIPT), "--list"], capture_output=True, text=True, check=True, timeout=240
    )
    return out.stdout.split()


def test_the_list_is_the_allow_list_and_the_public_submodules_under_it(listed: list[str]) -> None:
    names = set(listed)
    assert (STDLIB | THIRD_PARTY) - {"__future__"} <= names  # sys and platform too
    # submodules plugin check lets through (it looks at the first part only) that the app itself
    # never imports: the frozen backend of 0.3.3 had none of these
    assert {"html.parser", "json.tool", "numpy.lib.user_array", "numpy.linalg.lapack_lite"} <= names
    assert {
        "xml.dom.minidom",
        "email.mime.text",
        "sqlite3.dump",
        "concurrent.futures",
        "pydantic.v1",
    } <= names
    assert ("asyncio.windows_events" in names) == (sys.platform == "win32")  # what imports here
    assert ("asyncio.unix_events" in names) == (sys.platform != "win32")
    for name in names:
        parts = name.split(".")
        assert not any(p.startswith("_") for p in parts[1:]), name
        assert not {"tests", "test"} & set(parts), name
        assert not name.startswith(("numpy.testing", "numpy.f2py", "pydantic.mypy")), name


def test_plugin_check_allows_every_import_of_the_probe(tmp_path: Path, listed: list[str]) -> None:
    """The list and plugin check agree: a listed module check refused would show here."""
    load().write_probe(tmp_path, listed)
    source = (tmp_path / "plugin.py").read_text(encoding="utf-8")
    assert source.count("\n        import ") == len(listed)
    assert check_imports(tmp_path) == []


def test_what_the_app_leaves_out_is_allowed_and_exists() -> None:
    """NOT_FOR_PLUGINS names modules check lets through (else they need no entry) and that exist
    (an entry for a module gone from numpy or pydantic is stale)."""
    pi = load()
    for name in pi.NOT_FOR_PLUGINS:
        assert importlib.util.find_spec(name) is not None, name
        assert name.split(".")[0] in THIRD_PARTY | STDLIB


def test_compare_names_what_only_the_frozen_backend_lacks() -> None:
    pi = load()
    modules = ["a", "b", "c"]
    dev = {"imported": 2, "missing": {"c": "x"}, "failed": {"zoneinfo": "no zones"}, "notes": {}}
    frozen = {
        "imported": 1,
        "missing": {"b": "ModuleNotFoundError: No module named 'b'", "c": "x"},
        "failed": {"zoneinfo": "no zones", "sqlite3.iterdump": "ModuleNotFoundError: sqlite3.dump"},
        "notes": {},
    }
    problems = pi.compare(dev, frozen, modules)
    assert problems == [
        "module b is not in the frozen backend (ModuleNotFoundError: No module named 'b')",
        "sqlite3.iterdump: works with the development interpreter, frozen ModuleNotFoundError: sqlite3.dump",
    ]
    assert pi.compare(dev, dev, modules) == []


def test_the_probe_passes_with_the_development_backend() -> None:
    """End to end: plugin check and plugin call (a plugin process) with ``python -m spaceknife`` on
    both sides; tools/build_package.py does the same with the frozen backend on one side."""
    out = subprocess.run(
        [sys.executable, str(SCRIPT), "--development"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=280,
        check=False,
    )
    assert out.returncode == 0, out.stdout + out.stderr
    assert "modules plugins may import: all in the frozen backend" in out.stdout


def test_the_spec_bundles_the_list_and_the_build_checks_it() -> None:
    spec = (ROOT / "packaging" / "spaceknife-backend.spec").read_text(encoding="utf-8")
    assert '"/plugin_imports.py", "--list"' in spec
    assert "hidden +=" in spec
    build = (ROOT / "tools" / "build_package.py").read_text(encoding="utf-8")
    assert 'str(ROOT / "packaging" / "plugin_imports.py"), str(backend_exe())' in build
