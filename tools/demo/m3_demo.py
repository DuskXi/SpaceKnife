"""M3 demo: run the Electron app (v1) from an isolated copy of the repository.

Usage (from the copy, e.g. SpaceKnife-demos/M3):

    uv run python tools/demo/m3_demo.py [--home DIR] [--foreground]

The app keeps its settings, snapshots, tags and logs under ``--home`` (default: ``<copy>/.home``),
so the demo never touches the real user data or the development tree. Unless ``--foreground`` is
given, the window starts minimized in the taskbar instead of taking the focus.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "apps" / "electron"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--home", default=str(ROOT / ".home"), help="isolated data/config directory")
    ap.add_argument("--foreground", action="store_true", help="show the window normally")
    args = ap.parse_args()
    pnpm = shutil.which("pnpm") or shutil.which("pnpm.cmd")
    if pnpm is None:
        print("pnpm not found on PATH", file=sys.stderr)
        return 1
    home = Path(args.home).resolve()
    home.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "SPACEKNIFE_HOME": str(home), "PYTHONIOENCODING": "utf-8"}
    if not args.foreground:
        env["SPACEKNIFE_START_MINIMIZED"] = "1"
    if not (APP / "out" / "main" / "index.js").exists() and not (APP / "out" / "main" / "index.mjs").exists():
        subprocess.run([pnpm, "build"], cwd=APP, env=env, check=True)
    # electron-vite preview runs the built app with the real Electron binary
    return subprocess.call([pnpm, "start"], cwd=APP, env=env)


if __name__ == "__main__":
    raise SystemExit(main())
