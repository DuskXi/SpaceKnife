"""Build the installers (M8): freeze the backend, build the Electron app, package it.

    uv sync --locked --group native --group package
    uv run python tools/build_package.py                  # this platform's installers
    uv run python tools/build_package.py --skip-backend   # reuse dist/backend (Electron changes only)
    uv run python tools/build_package.py --targets deb    # Linux: some targets only

Windows → dist/electron/SpaceKnife-<version>-Setup-x64.exe (NSIS, unsigned) and win-unpacked/
Linux   → dist/electron/SpaceKnife-<version>-x86_64.AppImage, spaceknife_<version>_amd64.deb,
          spaceknife-<version>.x86_64.rpm (needs ``rpmbuild`` on PATH) and linux-unpacked/

Steps: PyInstaller (packaging/spaceknife-backend.spec → dist/backend/spaceknife-backend), the
frozen backend's smoke test (packaging/smoke_backend.py), on Linux the glibc floor of every ELF in
the bundle (docs/PLAN.md: RHEL 9 / Ubuntu 22.04, glibc 2.34), ``electron-vite build``,
``electron-builder`` (apps/electron/electron-builder.yml). The native extension is required:
an installer without it would scan at a fraction of the speed. See docs/packaging.md.
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

from rich.console import Console

ROOT = Path(__file__).resolve().parent.parent
ELECTRON = ROOT / "apps" / "electron"
DIST = ROOT / "dist"
BACKEND_DIR = DIST / "backend" / "spaceknife-backend"
GLIBC_FLOOR = (2, 34)  # RHEL 9; Ubuntu 22.04 has 2.35
LINUX_TARGETS = ("AppImage", "deb", "rpm")

console = Console()


def step(title: str) -> float:
    console.print(f"[bold]▸ {title}[/bold]")
    return time.monotonic()


def done(t0: float, note: str = "") -> None:
    console.print(f"  [green]ok[/green] {time.monotonic() - t0:.1f} s{f' · {note}' if note else ''}")


def run(argv: list[str], cwd: Path = ROOT) -> None:
    exe = shutil.which(argv[0]) or argv[0]  # pnpm is pnpm.cmd on Windows
    subprocess.run([exe, *argv[1:]], cwd=cwd, check=True)


def backend_exe() -> Path:
    return BACKEND_DIR / ("spaceknife-backend.exe" if sys.platform == "win32" else "spaceknife-backend")


def freeze_backend() -> None:
    t0 = step("frozen backend (PyInstaller)")
    hint = "run `uv sync --locked --group native --group package`"
    if importlib.util.find_spec("PyInstaller") is None:
        raise SystemExit(f"no PyInstaller: {hint}")
    try:  # the compiled module itself, not just the package folder
        importlib.import_module("spaceknife_native._native")
    except ImportError as exc:
        raise SystemExit(f"the native extension does not load ({exc}): {hint}") from exc
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
        ]
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


def build_electron(targets: list[str]) -> None:
    t0 = step("Electron app (electron-vite build)")
    run(["pnpm", "-s", "build"], cwd=ELECTRON)
    done(t0)
    if not backend_exe().exists():
        raise SystemExit(f"no frozen backend at {BACKEND_DIR}: run without --skip-backend")
    if sys.platform == "win32":
        platform = ["--win", "nsis"]
    elif sys.platform.startswith("linux"):
        if "rpm" in targets and shutil.which("rpmbuild") is None:
            raise SystemExit("rpm needs rpmbuild on PATH (or leave it out: --targets AppImage deb)")
        platform = ["--linux", *targets]
    else:
        raise SystemExit("installers are built on Windows and Linux only (macOS packaging is deferred)")
    t0 = step(f"installers (electron-builder {' '.join(platform[1:])})")
    run(["pnpm", "exec", "electron-builder", *platform, "--x64", "--publish", "never"], cwd=ELECTRON)
    done(t0)


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
    if sys.platform.startswith("linux"):
        check_glibc()
    build_electron(args.targets)
    out = DIST / "electron"
    for f in sorted(out.iterdir()):
        if f.is_file() and f.suffix in {".exe", ".AppImage", ".deb", ".rpm"}:
            console.print(f"  {f.relative_to(ROOT)}  [dim]{f.stat().st_size / 2**20:.0f} MB[/dim]")
    return 0


if __name__ == "__main__":
    sys.exit(main())
