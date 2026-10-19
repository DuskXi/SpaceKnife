"""Build the installers (M8): freeze the backend, build the Electron app, package it.

    uv sync --locked --group native --group package
    uv run python tools/build_package.py                  # this platform's installers
    uv run python tools/build_package.py --skip-backend   # reuse dist/backend (Electron changes only)
    uv run python tools/build_package.py --targets deb    # Linux: some targets only

Windows → dist/electron/SpaceKnife-<version>-Setup-x64.exe (NSIS, unsigned) and win-unpacked/
Linux   → dist/electron/SpaceKnife-<version>-x86_64.AppImage, spaceknife_<version>_amd64.deb,
          spaceknife-<version>.x86_64.rpm (needs ``rpmbuild`` on PATH) and linux-unpacked/
macOS   → dist/electron/SpaceKnife-<version>-mac-<arch>.dmg for this Mac's architecture (arm64 or
          x64; ad-hoc signed, not notarized) and mac[-arm64]/SpaceKnife.app

Steps: a release build of the native extension (maturin, with the build machine's paths remapped:
no home folder or user name in the binaries), PyInstaller (packaging/spaceknife-backend.spec →
dist/backend/spaceknife-backend), the frozen backend's smoke test (packaging/smoke_backend.py), on
Linux the glibc floor of every ELF in the bundle (docs/PLAN.md: RHEL 9 / Ubuntu 22.04, glibc 2.34),
``electron-vite build``, the license files (tools/third_party_licenses.py → dist/licenses),
``electron-builder`` (apps/electron/electron-builder.yml), a check that no file of the
packaged app contains the build machine's home path, and on macOS a check of the app's ad-hoc
signature. The native extension is required: an installer without it would scan at a fraction of
the speed. See docs/packaging.md.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import time
import zipfile
from collections.abc import Iterator
from pathlib import Path

from rich.console import Console

ROOT = Path(__file__).resolve().parent.parent
ELECTRON = ROOT / "apps" / "electron"
DIST = ROOT / "dist"
BACKEND_DIR = DIST / "backend" / "spaceknife-backend"
NATIVE_RELEASE = ROOT / "build" / "native-release"  # spaceknife_native/ of the release wheel
GLIBC_FLOOR = (2, 34)  # RHEL 9; Ubuntu 22.04 has 2.35
LINUX_TARGETS = ("AppImage", "deb", "rpm")

console = Console()


def step(title: str) -> float:
    console.print(f"[bold]▸ {title}[/bold]")
    return time.monotonic()


def done(t0: float, note: str = "") -> None:
    console.print(f"  [green]ok[/green] {time.monotonic() - t0:.1f} s{f' · {note}' if note else ''}")


def run(argv: list[str], cwd: Path = ROOT, env: dict[str, str] | None = None) -> None:
    exe = shutil.which(argv[0]) or argv[0]  # pnpm is pnpm.cmd on Windows
    subprocess.run([exe, *argv[1:]], cwd=cwd, check=True, env=env)


def backend_exe() -> Path:
    return BACKEND_DIR / ("spaceknife-backend.exe" if sys.platform == "win32" else "spaceknife-backend")


def native_release() -> None:
    """The native extension as shipped: a release wheel whose panic messages and debug-info paths
    name /cargo, /rustup and /spaceknife instead of the build machine's folders (Cargo's
    trim-paths is not stable yet), unpacked into NATIVE_RELEASE for PyInstaller."""
    t0 = step("native extension (release build, build paths remapped)")
    hint = "run `uv sync --locked --group native --group package`"
    for module in ("PyInstaller", "maturin"):
        if importlib.util.find_spec(module) is None:
            raise SystemExit(f"no {module}: {hint}")
    home = Path.home()
    flags = [
        f"--remap-path-prefix={os.environ.get('CARGO_HOME') or home / '.cargo'}=/cargo",
        f"--remap-path-prefix={os.environ.get('RUSTUP_HOME') or home / '.rustup'}=/rustup",
        f"--remap-path-prefix={ROOT}=/spaceknife",
    ]
    if sys.platform == "win32":
        flags.append("-Clink-arg=/PDBALTPATH:%_PDB%")  # the PDB's file name only, not its full path
    env = {**os.environ, "CARGO_ENCODED_RUSTFLAGS": "\x1f".join(flags)}
    wheels = ROOT / "build" / "native-wheel"
    shutil.rmtree(wheels, ignore_errors=True)
    run(
        [
            sys.executable,
            "-m",
            "maturin",
            "build",
            "--release",
            "--manifest-path",
            str(ROOT / "packages" / "native" / "Cargo.toml"),
            "--out",
            str(wheels),
            "--interpreter",
            sys.executable,
            "--target-dir",
            str(ROOT / "build" / "native-target"),
            "--auditwheel",
            "skip",
        ],
        env=env,
    )
    (wheel,) = wheels.glob("*.whl")
    shutil.rmtree(NATIVE_RELEASE, ignore_errors=True)
    with zipfile.ZipFile(wheel) as zf:
        zf.extractall(NATIVE_RELEASE, [n for n in zf.namelist() if n.startswith("spaceknife_native/")])
    done(t0, wheel.name)


def freeze_backend() -> None:
    native_release()
    t0 = step("frozen backend (PyInstaller)")
    # the release build of the native extension comes first on the path (the spec reads it too)
    env = {**os.environ, "PYTHONPATH": str(NATIVE_RELEASE), "SPACEKNIFE_NATIVE_RELEASE": str(NATIVE_RELEASE)}
    run(
        [
            sys.executable,
            "-m",
            "PyInstaller",
            "--noconfirm",
            "--clean",
            "--log-level",
            "WARN",
            "--distpath",
            str(DIST / "backend"),
            "--workpath",
            str(ROOT / "build" / "pyinstaller"),
            str(ROOT / "packaging" / "spaceknife-backend.spec"),
        ],
        env=env,
    )
    size = sum(f.stat().st_size for f in BACKEND_DIR.rglob("*") if f.is_file())
    done(t0, f"{size / 2**20:.0f} MB")
    t0 = step("frozen backend smoke test")
    run([sys.executable, str(ROOT / "packaging" / "smoke_backend.py"), str(backend_exe())])
    done(t0)


GLIBC = re.compile(rb"GLIBC_(\d+)\.(\d+)")


def check_glibc() -> None:
    """Every ELF of the bundle may only need glibc symbols up to GLIBC_FLOOR (conservative: any
    version string in the file counts)."""
    t0 = step(f"glibc floor {GLIBC_FLOOR[0]}.{GLIBC_FLOOR[1]}")
    worst: tuple[int, int] = (0, 0)
    bad: list[str] = []
    for path in BACKEND_DIR.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        with path.open("rb") as f:
            if f.read(4) != b"\x7fELF":
                continue
        found = {(int(a), int(b)) for a, b in GLIBC.findall(path.read_bytes())}
        if not found:
            continue
        top = max(found)
        worst = max(worst, top)
        if top > GLIBC_FLOOR:
            bad.append(f"{path.relative_to(BACKEND_DIR)} needs GLIBC_{top[0]}.{top[1]}")
    if bad:
        raise SystemExit("glibc too new for RHEL 9:\n  " + "\n  ".join(bad))
    done(t0, f"highest GLIBC_{worst[0]}.{worst[1]}")


def home_path_patterns() -> list[re.Pattern[bytes]]:
    """The build machine's home folder as it may appear in a file: both slash styles, JS / JSON
    escaped, UTF-8 and UTF-16 (Windows resources), any case (Windows paths)."""
    home = str(Path.home()).rstrip("\\/")
    variants = {home, home.replace("\\", "/"), home.replace("\\", "\\\\")}
    out = []
    for v in variants:
        # not followed by another name character: /home/user must not match /home/username
        out.append(re.compile(re.escape(v.encode()) + rb"(?![A-Za-z0-9_])", re.IGNORECASE))
        out.append(re.compile(re.escape(v.encode("utf-16-le")) + rb"(?![A-Za-z0-9_]\x00)", re.IGNORECASE))
    return out


def _contents(path: Path) -> Iterator[tuple[str, bytes]]:
    """A file's bytes, and the members of the compressed archives a raw scan cannot see: zip
    files (PyInstaller's base_library.zip) and the frozen backend's own archive (the Python
    modules, marshalled, in its embedded PYZ)."""
    data = path.read_bytes()
    yield "", data
    if data[:4] == b"PK\x03\x04":
        with zipfile.ZipFile(path) as zf:
            for name in zf.namelist():
                yield name, zf.read(name)
    elif path.name in ("spaceknife-backend", "spaceknife-backend.exe"):
        from PyInstaller.archive.readers import CArchiveReader  # type: ignore[import-untyped]

        pkg = CArchiveReader(str(path))
        for name, entry in pkg.toc.items():
            if entry[-1] == "z":  # PYZ: the pure-Python modules
                pyz = pkg.open_embedded_archive(name)
                for module in pyz.toc:
                    raw = pyz.extract(module, raw=True)
                    if raw:
                        yield f"{name}/{module}", raw
            else:
                member = pkg.extract(name)
                if member:
                    yield name, member


# GitHub-hosted runners: a home folder that names no person. Wheels from PyPI that were built on
# such runners (pydantic-core, zstandard, …) contain it too, so it cannot be told apart from ours.
RUNNER_HOMES = {"/Users/runner", "/home/runner"}


def check_no_build_paths(root: Path) -> None:
    """No file of the packaged app may contain the build machine's home path (user name),
    including inside zip files and the frozen backend's module archive."""
    t0 = step(f"no build-machine paths in {root.relative_to(ROOT)}")
    if os.environ.get("GITHUB_ACTIONS") == "true" and str(Path.home()) in RUNNER_HOMES:
        done(t0, f"skipped: {Path.home()} is the GitHub runner's home, not a user's")
        return
    patterns = home_path_patterns()
    bad: list[str] = []
    files = members = 0
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        files += 1
        for member, data in _contents(path):
            members += bool(member)
            hits = sum(len(p.findall(data)) for p in patterns)
            if hits:
                where = f"{path.relative_to(root)}{' › ' + member if member else ''}"
                bad.append(f"{where}: {hits}")
    if bad:
        raise SystemExit("the build machine's home path is in the package:\n  " + "\n  ".join(bad))
    done(t0, f"{files} files, {members} archive members")


def mac_arm64() -> bool:
    return sys.platform == "darwin" and os.uname().machine == "arm64"


def unpacked_app() -> Path:
    """electron-builder's unpacked app: the folder the installer is made from."""
    if sys.platform == "win32":
        return DIST / "electron" / "win-unpacked"
    if sys.platform == "darwin":
        return DIST / "electron" / ("mac-arm64" if mac_arm64() else "mac") / "SpaceKnife.app"
    return DIST / "electron" / "linux-unpacked"


def check_mac_signature(app: Path) -> None:
    """The whole bundle carries a valid ad-hoc signature (electron-builder.yml: identity "-"): an
    Apple Silicon Mac runs no unsigned code, and a broken signature makes Gatekeeper call the app
    "damaged" instead of offering "Open Anyway"."""
    t0 = step("ad-hoc signature of the app")
    run(["codesign", "--verify", "--deep", "--strict", str(app)])
    info = subprocess.run(["codesign", "-dv", str(app)], capture_output=True, text=True, check=True).stderr
    if "Signature=adhoc" not in info:
        raise SystemExit(f"{app.name} is not ad-hoc signed:\n{info}")
    done(t0, "valid, ad-hoc")


def build_electron(targets: list[str]) -> None:
    t0 = step("Electron app (electron-vite build)")
    run(["pnpm", "-s", "build"], cwd=ELECTRON)
    done(t0)
    t0 = step("licenses (dist/licenses)")
    run([sys.executable, str(ROOT / "tools" / "third_party_licenses.py")])
    done(t0)
    if not backend_exe().exists():
        raise SystemExit(f"no frozen backend at {BACKEND_DIR}: run without --skip-backend")
    arch = "--x64"
    if sys.platform == "win32":
        platform = ["--win", "nsis"]
    elif sys.platform.startswith("linux"):
        if "rpm" in targets and shutil.which("rpmbuild") is None:
            raise SystemExit("rpm needs rpmbuild on PATH (or leave it out: --targets AppImage deb)")
        platform = ["--linux", *targets]
    elif sys.platform == "darwin":
        # the frozen backend runs on this Mac's architecture only, so the app is built for it too
        platform = ["--mac", "dmg"]
        arch = "--arm64" if mac_arm64() else "--x64"
    else:
        raise SystemExit(f"no installers for {sys.platform}")
    t0 = step(f"installers (electron-builder {' '.join(platform[1:])} {arch[2:]})")
    run(["pnpm", "exec", "electron-builder", *platform, arch, "--publish", "never"], cwd=ELECTRON)
    done(t0)
    check_no_build_paths(unpacked_app())
    if sys.platform == "darwin":
        check_mac_signature(unpacked_app())


def main() -> int:
    for stream in (sys.stdout, sys.stderr):  # a GBK console on Chinese Windows
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--skip-backend", action="store_true", help="reuse dist/backend/spaceknife-backend")
    ap.add_argument("--targets", nargs="+", choices=LINUX_TARGETS, default=list(LINUX_TARGETS), help="Linux")
    args = ap.parse_args()
    if not args.skip_backend:
        freeze_backend()
        check_no_build_paths(BACKEND_DIR)
    if sys.platform.startswith("linux"):
        check_glibc()
    build_electron(args.targets)
    out = DIST / "electron"
    for f in sorted(out.iterdir()):
        if f.is_file() and f.suffix in {".exe", ".AppImage", ".deb", ".rpm", ".dmg"}:
            console.print(f"  {f.relative_to(ROOT)}  [dim]{f.stat().st_size / 2**20:.0f} MB[/dim]")
    return 0


if __name__ == "__main__":
    sys.exit(main())
