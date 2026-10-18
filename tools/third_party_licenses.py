"""Collect the licenses shipped with the installers (M8) into dist/licenses/.

    uv run python tools/third_party_licenses.py      # tools/build_package.py runs it

``LICENSE.txt``
    SpaceKnife itself (MIT).
``THIRD-PARTY-backend.txt``
    The Python runtime and packages frozen into the backend: name, version, license and the
    license files each package ships (from the build environment's metadata).
``THIRD-PARTY-native.txt``
    The Rust crates linked into ``spaceknife_native`` (``cargo metadata``, normal dependencies
    for this platform; license files from each crate's source).
``THIRD-PARTY-ui.txt``
    The JavaScript bundled into the window (``rollup-plugin-license`` writes
    ``apps/electron/out/renderer/THIRD-PARTY-LICENSES.txt``, so ``electron-vite build`` runs
    first) and the fonts (OFL-1.1).

Electron ships its own ``LICENSE.electron.txt`` and ``LICENSES.chromium.html`` next to the
executable. Identical license texts are printed once, with the packages that share them.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from collections import defaultdict
from importlib import metadata
from pathlib import Path

from packaging.requirements import Requirement
from packaging.utils import canonicalize_name

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "dist" / "licenses"
ELECTRON = ROOT / "apps" / "electron"
NATIVE = ROOT / "packages" / "native" / "Cargo.toml"
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|authors)", re.IGNORECASE)
RULE = "=" * 78


def _render(title: str, packages: list[tuple[str, str, str, list[str]]]) -> str:
    """packages: (name, version, license id, [license texts])."""
    lines = [title, RULE, ""]
    for name, version, lic, _ in packages:
        lines.append(f"{name} {version} — {lic or 'see license text'}")
    by_text: dict[str, list[str]] = defaultdict(list)
    missing: list[str] = []
    for name, version, lic, texts in packages:
        if not texts:
            missing.append(f"{name} {version} ({lic})")
        for text in texts:
            by_text[text.strip()].append(f"{name} {version}")
    for text, users in by_text.items():
        lines += ["", RULE, "Used by: " + ", ".join(users), RULE, "", text]
    if missing:
        lines += [
            "",
            RULE,
            "These packages ship no license file; the standard text of the named license applies:",
        ]
        lines += [f"  {m}" for m in missing]
    return "\n".join(lines) + "\n"


def python_packages() -> list[tuple[str, str, str, list[str]]]:
    """The runtime dependencies of spaceknife-backend, recursively (markers evaluated here)."""
    seen: dict[str, metadata.Distribution] = {}
    todo = ["spaceknife-backend"]
    while todo:
        name = canonicalize_name(todo.pop())
        if name in seen:
            continue
        try:
            dist = metadata.distribution(name)
        except metadata.PackageNotFoundError:
            continue
        seen[name] = dist
        for spec in dist.requires or []:
            req = Requirement(spec)
            if req.marker is None or req.marker.evaluate({"extra": ""}):
                todo.append(req.name)
    out = []
    python = Path(sys.base_prefix)
    for candidate in (
        python / "LICENSE.txt",
        python / "lib" / f"python{sys.version_info[0]}.{sys.version_info[1]}" / "LICENSE.txt",
    ):
        if candidate.exists():
            out.append(
                (
                    "Python",
                    sys.version.split()[0],
                    "PSF-2.0",
                    [candidate.read_text(encoding="utf-8", errors="replace")],
                )
            )
            break
    for key, dist in sorted(seen.items()):
        if key.startswith("spaceknife"):
            continue
        meta = dist.metadata
        lic = meta.get("License-Expression") or ""
        if not lic:
            classifiers = [
                c.split("::")[-1].strip()
                for c in meta.get_all("Classifier") or []
                if c.startswith("License ::")
            ]
            short = (meta.get("License") or "").strip()
            lic = ", ".join(classifiers) or (short if short and "\n" not in short and len(short) < 80 else "")
        texts = []
        for f in dist.files or []:
            if ".dist-info" in str(f) and LICENSE_FILE.match(Path(str(f)).name):
                path = Path(str(dist.locate_file(f)))
                if path.is_file():
                    texts.append(path.read_text(encoding="utf-8", errors="replace"))
        out.append((meta["Name"], dist.version, lic, texts))
    return out


def rust_crates() -> list[tuple[str, str, str, list[str]]]:
    """Crates reachable from spaceknife-native through normal dependencies on this platform."""
    host = next(
        line.split()[1]
        for line in subprocess.run(
            ["rustc", "-vV"], capture_output=True, text=True, check=True
        ).stdout.splitlines()
        if line.startswith("host:")
    )
    doc = json.loads(
        subprocess.run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--filter-platform",
                host,
                "--manifest-path",
                str(NATIVE),
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=True,
        ).stdout
    )
    packages = {p["id"]: p for p in doc["packages"]}
    nodes = {n["id"]: n for n in doc["resolve"]["nodes"]}
    root = doc["resolve"]["root"]
    reach: set[str] = set()
    todo = [root]
    while todo:
        pid = todo.pop()
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in reach:
                reach.add(dep["pkg"])
                todo.append(dep["pkg"])
    out = []
    for pid in sorted(reach, key=lambda i: (packages[i]["name"], packages[i]["version"])):
        pkg = packages[pid]
        folder = Path(pkg["manifest_path"]).parent
        texts = [
            f.read_text(encoding="utf-8", errors="replace")
            for f in sorted(folder.iterdir())
            if f.is_file() and LICENSE_FILE.match(f.name)
        ]
        out.append((pkg["name"], pkg["version"], pkg.get("license") or "", texts))
    return out


def ui_licenses() -> str:
    bundled = ELECTRON / "out" / "renderer" / "THIRD-PARTY-LICENSES.txt"
    if not bundled.exists():
        raise SystemExit(f"{bundled} is missing: run `pnpm build` in apps/electron first")
    parts = ["JavaScript bundled into the SpaceKnife window", RULE, "", bundled.read_text(encoding="utf-8")]
    fonts = sorted((ELECTRON / "node_modules" / "@fontsource-variable").glob("*/LICENSE"))
    for f in fonts:
        version = json.loads((f.parent / "package.json").read_text(encoding="utf-8"))["version"]
        parts += [
            "",
            RULE,
            f"Font: @fontsource-variable/{f.parent.name} {version}",
            RULE,
            "",
            f.read_text(encoding="utf-8"),
        ]
    return "\n".join(parts)


def main() -> int:
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    shutil.copyfile(ROOT / "LICENSE", OUT / "LICENSE.txt")
    py = python_packages()
    (OUT / "THIRD-PARTY-backend.txt").write_text(
        _render("Python runtime and packages in the SpaceKnife backend", py), encoding="utf-8", newline="\n"
    )
    rs = rust_crates()
    (OUT / "THIRD-PARTY-native.txt").write_text(
        _render("Rust crates in spaceknife_native", rs), encoding="utf-8", newline="\n"
    )
    (OUT / "THIRD-PARTY-ui.txt").write_text(ui_licenses(), encoding="utf-8", newline="\n")
    print(f"{OUT.relative_to(ROOT)}: {len(py)} Python packages, {len(rs)} Rust crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
