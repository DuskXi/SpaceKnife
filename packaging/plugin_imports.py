"""Every module a plugin may import is in the frozen backend (docs/packaging.md 「插件能 import 的模块」).

    python packaging/plugin_imports.py --list                 the modules, one per line (the spec)
    python packaging/plugin_imports.py dist/backend/spaceknife-backend/spaceknife-backend[.exe]
    python packaging/plugin_imports.py --development          the check against python -m spaceknife

``plugin check`` holds a plugin to an allow-list (``spaceknife_sdk.imports``: ``STDLIB`` and
``THIRD_PARTY``) and looks only at the first part of a name: ``import xml.dom.minidom`` and
``from numpy import fft`` pass as well. PyInstaller, though, only bundles what it finds imported.
:func:`allowed_modules` is what a plugin may therefore import: each listed name and its public
submodules (no part starting with ``_``) that import in this interpreter (``asyncio.unix_events``
does not on Windows), except :data:`NOT_FOR_PLUGINS` (test code, build tools, mypy plugins: the app
ships no pytest, Fortran compiler or mypy). ``packaging/spaceknife-backend.spec`` bundles them.

The check (run by ``tools/build_package.py`` after the frozen backend's smoke test) writes a plugin
that imports every one of them, one ``import`` statement each, and uses the extension modules and
data behind a few (sqlite3's ``iterdump``, the codecs, lzma, time zones, numpy's linear algebra …).
Through the frozen backend, ``plugin check`` must refuse none of these imports, and ``plugin call``
runs the probe in a plugin process: whatever works there with the development interpreter
(``python -m spaceknife plugin call``, the same probe) must work frozen too. Exit status 1 names
what is missing: add it to the spec.
"""

from __future__ import annotations

import importlib
import io
import json
import os
import pkgutil
import subprocess
import sys
import tempfile
import warnings
from collections.abc import Sequence
from pathlib import Path
from typing import Any

TIMEOUT = 180
PROBE_ID = "packaging.imports"
# allowed by plugin check (the first part of the name is listed) but not something plugin code
# uses at run time; left out of the bundle on purpose. A name covers its submodules.
NOT_FOR_PLUGINS = {
    "numpy.conftest": "numpy's own pytest configuration",
    "numpy.testing": "test helpers (pytest, unittest): a plugin's tests run outside the app",
    "numpy.ma.testutils": "test helpers",
    "numpy.f2py": "builds Fortran extensions: a build tool",
    "numpy.typing.mypy_plugin": "a mypy plugin",
    "pydantic.mypy": "a mypy plugin (the spec excludes it: it would bring mypy along)",
    "pydantic.v1.mypy": "a mypy plugin",
}
TEST_PARTS = frozenset({"tests", "test"})  # a package's own test suite (numpy.linalg.tests …)


def _skipped(name: str) -> bool:
    parts = name.split(".")
    if any(p.startswith("_") for p in parts[1:]) or TEST_PARTS.intersection(parts[1:]):
        return True
    return any(name == n or name.startswith(n + ".") for n in NOT_FOR_PLUGINS)


def _public(top: str) -> list[str]:
    """``top`` and its public submodules that import here (packages are walked as they import)."""
    try:
        mod = importlib.import_module(top)
    except Exception:  # not on this platform
        return []
    found = [top]
    path = getattr(mod, "__path__", None)
    for info in pkgutil.iter_modules(path or [], top + "."):
        if _skipped(info.name):
            continue
        try:
            importlib.import_module(info.name)
        except Exception:  # not on this platform (asyncio.unix_events on Windows …)
            continue
        found += _public(info.name) if info.ispkg else [info.name]
    return found


def allowed_modules() -> list[str]:
    """What a plugin may import (see the module docstring), sorted."""
    from spaceknife_sdk.imports import STDLIB, THIRD_PARTY

    with warnings.catch_warnings():
        warnings.simplefilter("ignore")  # numpy.core, numpy.matlib: deprecated, still importable
        names: set[str] = set()
        for top in (STDLIB | THIRD_PARTY) - {"__future__"}:
            names.update(_public(top))
    return sorted(names)


# ------------------------------------------------------------------------------------------- probe

PROBE_TOML = f"""\
# Written by packaging/plugin_imports.py: imports every module plugin check allows (the check of
# the frozen backend, docs/packaging.md). Not a plugin to install.
id = "{PROBE_ID}"
version = "1.0.0"
name = {{ zh = "打包检查：插件能 import 的模块", en = "Packaging check: modules plugins may import" }}
description = {{ zh = "逐个 import 插件检查允许的模块，再用一用它们背后的扩展模块和数据。", \
en = "Imports every module plugin check allows, then uses the extension modules and data behind a few." }}
author = "SpaceKnife packaging"
icon = "gauge"
entry = "plugin.py"
permissions = []
"""

# Each line: a name and an expression using modules the probe imports first; what it gives is
# printed as a note (a failure is recorded). They need extension modules, compiled data or
# lazily imported modules that an import alone does not show.
USES = {
    "sqlite3": "sqlite3.connect(':memory:').execute('select sqlite_version()').fetchone()[0]",
    "sqlite3.iterdump": "len(list(_db().iterdump()))",
    "zoneinfo": "zoneinfo.ZoneInfo('UTC').utcoffset(None) is not None "
    "and len(zoneinfo.available_timezones())",
    "zoneinfo.named": "str(datetime.datetime(2026, 7, 1, tzinfo=zoneinfo.ZoneInfo('Europe/Berlin'))"
    ".utcoffset())",
    "hashlib": "sorted(hashlib.new(a, b'x').name for a in "
    "('md5', 'sha1', 'sha256', 'sha512', 'blake2b', 'sha3_256'))",
    "hmac": "hmac.new(b'k', b'x', 'sha256').hexdigest()[:8]",
    "compression": "[m.decompress(m.compress(b'x' * 100)) == b'x' * 100 for m in (zlib, gzip, bz2, lzma)]",
    "zipfile": "_zip()",
    "tarfile": "_tar()",
    "codecs": "[codecs.lookup(c).name for c in ('gbk', 'gb18030', 'big5', 'shift_jis', 'euc_kr', 'cp1252', "
    "'utf-16', 'idna', 'punycode', 'unicode_escape', 'base64', 'hex', 'rot13', 'zlib', 'bz2')]",
    "unicodedata": "unicodedata.name('中') + ' ' + unicodedata.unidata_version",
    "decimal": "str(decimal.Decimal(1) / decimal.Decimal(7))[:8]",
    "locale": "locale.getlocale()",
    "platform": "platform.platform()",
    "platform.uname": "list(platform.uname())[:1] + [platform.machine(), platform.python_version()]",
    "uuid": "uuid.uuid4().version",
    "secrets": "len(secrets.token_hex(8))",
    "xml": "xml.etree.ElementTree.fromstring('<a><b/></a>')[0].tag + "
    "xml.dom.minidom.parseString('<c/>').documentElement.tagName",
    "xml.sax": "xml.sax.parseString(b'<d/>', xml.sax.ContentHandler()) is None",
    "email": "email.message_from_string('Subject: x\\n\\nbody')['Subject']",
    "html": "html.unescape('&amp;') + str(html.parser.HTMLParser().feed('<p>x</p>'))",
    "csv": "list(csv.reader(['a,b']))",
    "json": "json.loads(json.dumps({'k': '中'}, ensure_ascii=False))['k']",
    "tomllib": "tomllib.loads('a = 1')['a']",
    "asyncio": "asyncio.run(asyncio.sleep(0, 'ok'))",
    "concurrent": "list(concurrent.futures.ThreadPoolExecutor(2).map(abs, [-1, -2]))",
    "urllib": "urllib.parse.urlsplit('https://x/y?q=1').query + urllib.parse.quote('a b')",
    "array": "array.array('q', [1, 2]).itemsize",
    "struct": "struct.calcsize('<qd')",
    "binascii": "binascii.crc32(b'x')",
    "statistics": "statistics.median([1, 2, 3])",
    "datetime": "datetime.datetime.now(datetime.UTC).tzname()",
    "numpy": "float(numpy.linalg.inv(numpy.eye(3) * 2).sum()) "
    "+ float(abs(numpy.fft.fft([1, 0, 0, 0])).sum())",
    "numpy.random": "numpy.random.default_rng(1).integers(0, 10, 3).tolist()",
    "pydantic": "pydantic.TypeAdapter(list[int]).validate_python(['1', 2])",
    "pydantic.v1": "pydantic.v1.parse_obj_as(int, '3')",
}

PROBE_HEAD = '''\
"""Written by packaging/plugin_imports.py (the check of the frozen backend, docs/packaging.md):
imports every module plugin check allows, one statement each, then uses a few."""

# ruff: noqa
from __future__ import annotations

from spaceknife_sdk import Context, Payload, Plugin

plugin = Plugin()


class Result(Payload):
    imported: int
    missing: dict[str, str]  # module: why it did not import
    failed: dict[str, str]  # USES name: what it raised
    notes: dict[str, str]  # USES name: what it gave


def _why(exc: BaseException) -> str:
    return f"{type(exc).__name__}: {exc}"[:300]


def _db():
    import sqlite3

    db = sqlite3.connect(":memory:")
    db.execute("create table t (a)")
    db.execute("insert into t values ('x')")
    return db


def _zip():
    import io
    import zipfile

    buf = io.BytesIO()
    for method in (zipfile.ZIP_DEFLATED, zipfile.ZIP_BZIP2, zipfile.ZIP_LZMA):
        with zipfile.ZipFile(buf, "w", method) as z:
            z.writestr("a.txt", "x" * 100)
        with zipfile.ZipFile(buf) as z:
            assert z.read("a.txt") == b"x" * 100
        buf.seek(0)
        buf.truncate()
    return "ok"


def _tar():
    import io
    import tarfile

    for mode in ("gz", "bz2", "xz"):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:" + mode) as t:
            info = tarfile.TarInfo("a.txt")
            info.size = 3
            t.addfile(info, io.BytesIO(b"abc"))
        buf.seek(0)
        with tarfile.open(fileobj=buf, mode="r:" + mode) as t:
            assert t.getnames() == ["a.txt"]
    return "ok"


def run() -> dict:
    import warnings

    warnings.simplefilter("ignore")
    missing: dict[str, str] = {}
    imported = 0
'''

PROBE_TAIL = """
    failed: dict[str, str] = {}
    notes: dict[str, str] = {}
    scope = {**globals(), **locals()}  # one namespace: generator expressions see the modules too
    for name, use in USES.items():
        try:
            notes[name] = repr(eval(use, scope))[:200]
        except BaseException as exc:
            failed[name] = _why(exc)
    return {"imported": imported, "missing": missing, "failed": failed, "notes": notes}


@plugin.method(timeout=120)
def probe(ctx: Context) -> Result:
    return Result(**run())
"""


def probe_source(modules: Sequence[str]) -> str:
    lines = [PROBE_HEAD]
    for m in modules:
        lines += [
            "    try:",
            f"        import {m}",
            "        imported += 1",
            "    except BaseException as exc:",
            f"        missing[{m!r}] = _why(exc)",
        ]
    lines += [f"    USES = {USES!r}", PROBE_TAIL]
    return "\n".join(lines)


def write_probe(folder: Path, modules: Sequence[str]) -> None:
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "plugin.toml").write_text(PROBE_TOML, encoding="utf-8")
    (folder / "plugin.py").write_text(probe_source(modules), encoding="utf-8")


# ------------------------------------------------------------------------------------------- check


def _run(argv: Sequence[str], env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(argv),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        timeout=TIMEOUT,
    )


def call_probe(backend: Sequence[str], probe: Path, env: dict[str, str]) -> dict[str, Any]:
    """``plugin call <probe> probe`` through ``backend`` (a plugin process): the probe's result."""
    out = _run([*backend, "plugin", "call", str(probe), "probe"], env)
    if out.returncode != 0:
        raise SystemExit(
            f"FAIL {' '.join(backend)} plugin call → exit {out.returncode}\n{out.stdout}{out.stderr}"
        )
    start = out.stdout.find("{")
    result: dict[str, Any] = json.loads(out.stdout[start:]) if start >= 0 else {}
    if "missing" not in result:
        raise SystemExit(f"FAIL no result from plugin call:\n{out.stdout}{out.stderr}")
    return result


def refused_imports(backend: Sequence[str], probe: Path, env: dict[str, str]) -> list[str]:
    """What ``plugin check`` (through ``backend``) says about the probe's imports."""
    out = _run([*backend, "plugin", "check", str(probe), "--json", "--no-contracts"], env)
    start = out.stdout.find("{")
    if start < 0:
        return [f"plugin check gave no JSON (exit {out.returncode}): {out.stdout}{out.stderr}"]
    doc = json.loads(out.stdout[start:])
    codes = {"syntax", "forbidden_import", "unknown_import", "private_import", "import", "describe"}
    return [f"{i.get('code')}: {i.get('message')}" for i in doc.get("issues", []) if i.get("code") in codes]


def compare(dev: dict[str, Any], frozen: dict[str, Any], modules: Sequence[str]) -> list[str]:
    """What the frozen backend cannot do that the development interpreter can."""
    problems = []
    for m, why in sorted(frozen["missing"].items()):
        if m not in dev["missing"]:
            problems.append(f"module {m} is not in the frozen backend ({why})")
    for name, why in sorted(frozen["failed"].items()):
        if name not in dev["failed"]:
            problems.append(f"{name}: works with the development interpreter, frozen {why}")
    if frozen["imported"] != len(modules) - len(frozen["missing"]):
        problems.append(f"the probe imported {frozen['imported']} of {len(modules)} modules")
    return problems


def check(backend: Sequence[str], dev: Sequence[str] | None = None) -> int:
    """The check of the module docstring: ``backend`` is the frozen backend's command, ``dev`` the
    development one (``python -m spaceknife``). 0 when the frozen backend has everything."""
    dev = list(dev or [sys.executable, "-m", "spaceknife"])
    modules = allowed_modules()
    with tempfile.TemporaryDirectory(prefix="sk-plugin-imports-", ignore_cleanup_errors=True) as tmp:
        root = Path(tmp)
        probe = root / PROBE_ID
        write_probe(probe, modules)
        env = {
            **os.environ,
            "SPACEKNIFE_HOME": str(root / "home"),
            "NO_COLOR": "1",
            "PYTHONIOENCODING": "utf-8",
        }
        refused = refused_imports(backend, probe, env)
        base = call_probe(dev, probe, env)
        frozen = call_probe(backend, probe, env)
    problems = [f"plugin check refuses {r}" for r in refused] + compare(base, frozen, modules)
    print(f"{len(modules)} modules plugins may import; frozen: {frozen['imported']} imported")
    for name, value in frozen["notes"].items():
        print(f"  {name}: {value}")
    for name, why in sorted(base["failed"].items()):
        print(f"note: {name} fails with the development interpreter too: {why}")
    for p in problems:
        print(f"FAIL {p}")
    if problems:
        print("add what is missing to packaging/spaceknife-backend.spec (hiddenimports)")
        return 1
    print("modules plugins may import: all in the frozen backend")
    return 0


def main(argv: Sequence[str]) -> int:
    if isinstance(sys.stdout, io.TextIOWrapper):  # a console that cannot show every note (cp1252 …)
        sys.stdout.reconfigure(errors="backslashreplace")
    if list(argv) == ["--list"]:
        print("\n".join(allowed_modules()))
        return 0
    if list(argv) == ["--development"]:  # the probe against the development backend itself (tools/tests)
        return check([sys.executable, "-m", "spaceknife"])
    if len(argv) != 1:
        print(__doc__)
        return 2
    return check([str(Path(argv[0]).resolve())])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
