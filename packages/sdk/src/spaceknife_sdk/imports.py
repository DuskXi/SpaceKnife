"""What a plugin may import: the frozen app only carries the modules PyInstaller found, so plugins are
held to this list (``packaging/spaceknife-backend.spec`` bundles it) even where more would work."""

from __future__ import annotations

import ast
from collections.abc import Iterable
from pathlib import Path

from .errors import Issue

STDLIB = frozenset(
    {
        "json",
        "re",
        "math",
        "statistics",
        "collections",
        "itertools",
        "functools",
        "dataclasses",
        "datetime",
        "time",
        "zoneinfo",
        "hashlib",
        "hmac",
        "base64",
        "binascii",
        "struct",
        "zlib",
        "gzip",
        "bz2",
        "lzma",
        "zipfile",
        "tarfile",
        "csv",
        "io",
        "pathlib",
        "fnmatch",
        "glob",
        "typing",
        "enum",
        "decimal",
        "fractions",
        "heapq",
        "bisect",
        "textwrap",
        "unicodedata",
        "uuid",
        "tomllib",
        "html",
        "xml",
        "urllib",
        "string",
        "difflib",
        "operator",
        "contextlib",
        "asyncio",
        "concurrent",
        "threading",
        "queue",
        "random",
        "secrets",
        "sqlite3",
        "email",
        "os",
        "abc",
        "copy",
        "array",
        "codecs",
        "locale",
        "pprint",
        "warnings",
        "weakref",
        "types",
        "numbers",
        "shlex",
        "__future__",
    }
)
THIRD_PARTY = frozenset({"spaceknife_sdk", "spaceknife_protocol", "numpy", "pydantic", "pydantic_core"})
# the SDK's ctx.net / ctx.exec instead; native code is never allowed
FORBIDDEN = frozenset(
    {"ctypes", "cffi", "socket", "subprocess", "multiprocessing", "_ctypes", "ssl", "select"}
)


def check_imports(folder: Path, files: Iterable[Path] | None = None) -> list[Issue]:
    """Imports outside the list (``lib/`` and the plugin's own modules are fine)."""
    own = {p.stem for p in folder.glob("*.py")} | {
        p.name for p in folder.iterdir() if (p / "__init__.py").is_file()
    }
    lib = folder / "lib"
    if lib.is_dir():
        own |= {p.stem for p in lib.glob("*.py")} | {p.name for p in lib.iterdir() if p.is_dir()}
    issues: list[Issue] = []
    targets = (
        list(files)
        if files is not None
        else [
            p
            for p in folder.rglob("*.py")
            if "lib" not in p.relative_to(folder).parts[:1] and "tests" not in p.parts
        ]
    )
    for path in targets:
        rel = path.relative_to(folder).as_posix()
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=rel)
        except (SyntaxError, UnicodeDecodeError) as exc:
            issues.append(Issue("syntax", str(exc), file=rel, line=getattr(exc, "lineno", 0) or 0))
            continue
        for node in ast.walk(tree):
            names: list[str] = []
            if isinstance(node, ast.Import):
                names = [a.name for a in node.names]
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                names = [node.module]
            for name in names:
                top = name.split(".")[0]
                if top in FORBIDDEN:
                    issues.append(
                        Issue(
                            "forbidden_import",
                            f"{name} is not available to plugins",
                            file=rel,
                            line=getattr(node, "lineno", 0),
                            fix="use ctx.net.fetch / ctx.exec, which check the permissions",
                        )
                    )
                elif top not in STDLIB and top not in THIRD_PARTY and top not in own:
                    issues.append(
                        Issue(
                            "unknown_import",
                            f"{name}: not in the frozen app",
                            file=rel,
                            line=getattr(node, "lineno", 0),
                            fix="put pure-Python dependencies in lib/",
                            severity="warning" if top.startswith("_") else "error",
                        )
                    )
    return issues


__all__ = ["FORBIDDEN", "STDLIB", "THIRD_PARTY", "check_imports"]
