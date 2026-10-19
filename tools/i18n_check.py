"""Checks the translations (M7.5): no hard-coded Chinese left, every text has an English entry.

    uv run python tools/i18n_check.py [--area backend|electron] [--file PATH ...] [--missing]

Backend (AST): Chinese text must be a message id — the first argument of ``_()`` / ``tr()``, a
value of a ``Texts({...})`` table, or an argument of a definition that is translated where it is
shown (settings ``S(...)``, palette ``C(...)``, …: :data:`REGISTERED`). Docstrings are skipped; a
line ending in ``# i18n: ignore`` too (data that only looks like text: aliases, synonyms).
Electron: see tools/i18n_scan_ts.mjs (needs Node and apps/electron/node_modules).

Every message id must have an entry in the English catalog of its side
(``packages/backend/src/spaceknife/locales/en/*.json``, ``apps/electron/src/renderer/src/locales/en/*.json``).
Exit status 1 when anything is wrong; ``--missing`` prints the ids without a translation as JSON
(a starting point for a catalog file).
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BACKEND = ROOT / "packages" / "backend" / "src" / "spaceknife"
ELECTRON = ROOT / "apps" / "electron" / "src"
BACKEND_CATALOG = BACKEND / "locales" / "en"
ELECTRON_CATALOG = ELECTRON / "renderer" / "src" / "locales" / "en"
CJK = re.compile(r"[\u3400-\u9fff\uff01-\uff60\u3000-\u303f]")
WRAPPERS = {"_", "tr"}
TABLES = {"Texts"}
# definitions whose texts are translated where they are shown (settings meta, palette, …)
REGISTERED = {"S", "C", "_c", "BuiltinInfo", "_Option"}
IGNORE = re.compile(r"#\s*i18n:\s*ignore\s*$")


@dataclass(frozen=True)
class Problem:
    file: str
    line: int
    text: str
    why: str

    def __str__(self) -> str:
        return f"{self.file}:{self.line}: {self.why}: {self.text!r}"


def _docstrings(tree: ast.AST) -> set[int]:
    ids: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            body = node.body
            if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant):
                ids.add(id(body[0].value))
    return ids


def _call_name(node: ast.AST | None) -> str | None:
    if isinstance(node, ast.Call):
        f = node.func
        if isinstance(f, ast.Name):
            return f.id
        if isinstance(f, ast.Attribute):
            return f.attr
    return None


def scan_python(paths: list[Path]) -> tuple[dict[str, set[str]], list[Problem]]:
    msgids: dict[str, set[str]] = {}
    problems: list[Problem] = []
    for path in paths:
        text = path.read_text(encoding="utf-8")
        if not CJK.search(text):
            continue
        tree = ast.parse(text)
        lines = text.splitlines()
        docs = _docstrings(tree)
        parents: dict[int, ast.AST] = {}
        for node in ast.walk(tree):
            for child in ast.iter_child_nodes(node):
                parents[id(child)] = node
        rel = path.relative_to(ROOT).as_posix()

        def ignored(node: ast.AST, lines: list[str] = lines) -> bool:
            line = lines[getattr(node, "lineno", 1) - 1] if getattr(node, "lineno", 0) else ""
            end = getattr(node, "end_lineno", None)
            last = lines[end - 1] if end else line
            return bool(IGNORE.search(line) or IGNORE.search(last))

        for node in ast.walk(tree):
            if isinstance(node, ast.JoinedStr):
                parts = "".join(
                    v.value for v in node.values if isinstance(v, ast.Constant) and isinstance(v.value, str)
                )
                if CJK.search(parts) and not ignored(node):
                    problems.append(
                        Problem(rel, node.lineno, parts[:80], 'f-string: use tr("…{name}…", name=…)')
                    )
                continue
            if not (isinstance(node, ast.Constant) and isinstance(node.value, str)):
                continue
            if id(node) in docs or not CJK.search(node.value):
                continue
            parent = parents.get(id(node))
            if isinstance(parent, ast.JoinedStr):
                continue  # reported with the f-string
            if (
                isinstance(parent, ast.Call)
                and parent.args
                and (
                    (_call_name(parent) in WRAPPERS and parent.args[0] is node)
                    # plural(count, "中文", "one in English", ...)
                    or (_call_name(parent) == "plural" and len(parent.args) > 1 and parent.args[1] is node)
                )
            ):
                msgids.setdefault(node.value, set()).add(rel)
                continue
            if (
                isinstance(parent, ast.Dict)
                and node in parent.values
                and _call_name(parents.get(id(parent))) in TABLES
            ):
                msgids.setdefault(node.value, set()).add(rel)
                continue
            if ignored(node):
                continue
            # an argument (or part of one) of a registered definition
            anc: ast.AST | None = parent
            registered = False
            while anc is not None and not isinstance(anc, (ast.stmt,)):
                if _call_name(anc) in REGISTERED:
                    registered = True
                    break
                anc = parents.get(id(anc))
            if registered:
                msgids.setdefault(node.value, set()).add(rel)
                continue
            problems.append(
                Problem(rel, node.lineno, node.value[:80], "Chinese text not passed to _() / tr()")
            )
    return msgids, problems


def scan_electron() -> tuple[dict[str, set[str]], list[Problem]] | None:
    node = shutil.which("node")
    if node is None or not (ELECTRON.parent / "node_modules").exists():
        return None
    out = subprocess.run(
        [node, str(ROOT / "tools" / "i18n_scan_ts.mjs"), str(ELECTRON)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
    ).stdout
    doc = json.loads(out)
    problems = [
        Problem("apps/electron/src/" + p["file"], p["line"], p["text"], p["why"]) for p in doc["problems"]
    ]
    ids = {k: {"apps/electron/src/" + f for f in v} for k, v in doc["msgids"].items()}
    return ids, problems


def load_catalog(folder: Path) -> tuple[dict[str, str], list[str]]:
    merged: dict[str, str] = {}
    conflicts: list[str] = []
    for f in sorted(folder.glob("*.json")):
        for k, v in json.loads(f.read_text(encoding="utf-8")).items():
            if k in merged and merged[k] != v:
                conflicts.append(f"{f.name}: {k!r} = {v!r}, elsewhere {merged[k]!r}")
            merged[k] = v
    return merged, conflicts


PLACEHOLDER = re.compile(r"\{(\w*)\}")


def check_catalog(msgids: dict[str, set[str]], catalog: dict[str, str], side: str) -> list[str]:
    out: list[str] = []
    for m in sorted(msgids):
        en = catalog.get(m)
        if not en:
            out.append(f"{side}: no English for {m!r}")
        elif sorted(PLACEHOLDER.findall(m)) != sorted(PLACEHOLDER.findall(en)):
            out.append(f"{side}: placeholders differ: {m!r} -> {en!r}")
        elif CJK.search(en):
            out.append(f"{side}: the English text still has Chinese: {m!r} -> {en!r}")
    return out


def backend_files() -> list[Path]:
    return [p for p in BACKEND.rglob("*.py") if "locales" not in p.parts]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--area", choices=["backend", "electron", "all"], default="all")
    ap.add_argument("--file", action="append", default=[], help="Only report problems in these files")
    ap.add_argument("--missing", action="store_true", help="Print message ids without English as JSON")
    args = ap.parse_args()
    only = {Path(f).resolve() for f in args.file}
    bad = 0
    missing: dict[str, str] = {}

    def keep(p: Problem) -> bool:
        return not only or (ROOT / p.file).resolve() in only

    if args.area in ("backend", "all"):
        ids, problems = scan_python(backend_files())
        ids = {k: v for k, v in ids.items() if not only or any((ROOT / f).resolve() in only for f in v)}
        catalog, conflicts = load_catalog(BACKEND_CATALOG)
        for p in filter(keep, problems):
            print(p)
            bad += 1
        for line in conflicts:
            print("backend catalog conflict:", line)
        for line in check_catalog(ids, catalog, "backend"):
            if not args.missing:
                print(line)
            bad += 1
        missing.update({m: "" for m in ids if not catalog.get(m)})
    if args.area in ("electron", "all"):
        found = scan_electron()
        if found is None:
            print("electron: skipped (needs node and apps/electron/node_modules)")
            if args.area == "electron":  # asked for by name (CI): not checking is a failure
                bad += 1
        else:
            ids, problems = found
            ids = {k: v for k, v in ids.items() if not only or any((ROOT / f).resolve() in only for f in v)}
            catalog, conflicts = load_catalog(ELECTRON_CATALOG)
            for p in filter(keep, problems):
                print(p)
                bad += 1
            for line in conflicts:
                print("electron catalog conflict:", line)
            for line in check_catalog(ids, catalog, "electron"):
                if not args.missing:
                    print(line)
                bad += 1
            missing.update({m: "" for m in ids if not catalog.get(m)})
    if args.missing:
        sys.stdout.write(json.dumps(dict(sorted(missing.items())), ensure_ascii=False, indent=1) + "\n")
    print(f"{bad} problem(s)", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
