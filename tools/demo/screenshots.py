"""README screenshots from mock data: nothing from the real disks, nothing shown on screen.

    uv run python tools/demo/screenshots.py --home C:\\Users\\Public\\sk-screens --out docs/images

Writes the two synthetic snapshots of tools/demo/mock_snapshot.py into ``--home`` (an isolated
SPACEKNIFE_HOME; pick a neutral folder: paths under it can show up in toasts), serves the built
renderer with a backend for that home (tools/demo/m3_web.py), and runs tools/demo/screenshots.cjs
in the repo's Electron with an off-screen window: treemap with a hover card, a filter, the command
palette and a snapshot comparison. ``--lang en`` captures the English interface as well
(``*-en.png``). ``--video`` records the short demo instead (tools/demo/demo.cjs: hover, zoom into a
folder, the flame graph and the polar flame graph, a typed filter, the palette, a comparison) and encodes it with ffmpeg into
``demo[-en].mp4`` (H.264) and ``demo[-en].gif``. ``--views`` takes the three visualizations of the
same volume instead (treemap, flame graph, polar flame graph; details panel hidden, 1.25× sharp) for
the website's pages and writes them as WebP to ``site/src/assets/visualizations`` (``treemap.webp``,
``flame.webp``, ``polar.webp``, ``*-en.webp``). Needs ``pnpm build`` in apps/electron first.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ELECTRON = ROOT / "apps" / "electron" / "node_modules" / "electron" / "dist"
VIEWS_OUT = ROOT / "site" / "src" / "assets" / "visualizations"
# --views: a 1440 × 760 window (CSS pixels; the toolbar needs the width), drawn at 1.25 pixels per
# CSS pixel, at the detail level a new view starts with; each can be overridden from the environment
VIEWS_SIZE = {"SHOT_WIDTH": "1440", "SHOT_HEIGHT": "760", "SHOT_ZOOM": "1.25", "SHOT_LEVEL": "5"}
WEBP_QUALITY = 90


def electron_exe() -> Path:
    if sys.platform == "win32":
        return ELECTRON / "electron.exe"
    if sys.platform == "darwin":
        return ELECTRON / "Electron.app" / "Contents" / "MacOS" / "Electron"
    return ELECTRON / "electron"


def capture(home: Path, out: Path, lang: str, snap: Path, video: bool = False, views: bool = False) -> int:
    env = {**os.environ, "SPACEKNIFE_LANGUAGE": lang}
    url_file = home / "url.txt"
    url_file.unlink(missing_ok=True)
    server = subprocess.Popen(
        [sys.executable, str(ROOT / "tools" / "demo" / "m3_web.py"), "--home", str(home)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        deadline = time.monotonic() + 60
        while not url_file.exists():
            if server.poll() is not None or time.monotonic() > deadline:
                print("the web server did not start", file=sys.stderr)
                return 1
            time.sleep(0.3)
        url = url_file.read_text(encoding="utf-8").strip() + "&platform=win32"
        script = "demo.cjs" if video else "screenshots.cjs"
        argv = [str(electron_exe()), str(ROOT / "tools" / "demo" / script)]
        shot_env = {
            **os.environ,
            "SHOT_URL": url,
            "SHOT_SNAPSHOT": str(snap),
            "SHOT_OUT": str(out),
            "TZ": "UTC",
        }
        shot_env["SHOT_LOG"] = str(home / "screenshots.log")
        frames = home / f"frames-{lang}"
        if video:
            shutil.rmtree(frames, ignore_errors=True)
            shot_env["SHOT_FRAMES"] = str(frames)
        pngs = home / f"views-{lang}"
        if views:
            shutil.rmtree(pngs, ignore_errors=True)
            shot_env |= {"SHOT_SET": "views", "SHOT_OUT": str(pngs)}
            shot_env |= {k: os.environ.get(k, v) for k, v in VIEWS_SIZE.items()}
        if lang == "en":
            shot_env |= {
                "SHOT_SUFFIX": "-en",
                "SHOT_COMPARE_QUERY": "compare",
                "SHOT_FILTER": ":class:Video;>1gb",
            }
        code = subprocess.run(argv, check=False, timeout=300, env=shot_env).returncode
        if video and code == 0:
            code = encode(frames, out, "-en" if lang == "en" else "")
        if views and code == 0:
            code = to_webp(pngs, out)
        return code
    finally:
        server.terminate()
        server.wait(timeout=30)


def to_webp(pngs: Path, out: Path) -> int:
    """The captured PNGs as WebP, near lossless (the website makes its own sizes and formats from them)."""
    from PIL import Image

    files = sorted(pngs.glob("*.png"))
    if len(files) != 3:
        print(f"expected 3 pictures in {pngs}, found {len(files)}", file=sys.stderr)
        return 1
    out.mkdir(parents=True, exist_ok=True)
    for png in files:
        webp = out / f"{png.stem}.webp"
        with Image.open(png) as im:
            im.convert("RGB").save(webp, "WEBP", quality=WEBP_QUALITY, method=6)
        print(f"{webp.name}  {webp.stat().st_size / 1024:.0f} KB")
    return 0


def encode(frames: Path, out: Path, suffix: str) -> int:
    """MP4 (1280 px, H.264, plays everywhere) and GIF (880 px, 12 fps, optimised palette) of the frames."""
    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg is None:
        print("ffmpeg is not on PATH", file=sys.stderr)
        return 1
    out.mkdir(parents=True, exist_ok=True)
    src = ["-framerate", "20", "-i", str(frames / "f%05d.jpg")]
    mp4 = out / f"demo{suffix}.mp4"
    gif = out / f"demo{suffix}.gif"
    run = [ffmpeg, "-y", "-loglevel", "error", *src]
    subprocess.run(
        [
            *run,
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",  # H.264 with yuv420p needs even sizes
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            str(mp4),
        ],
        check=True,
    )
    palette = "fps=12,scale=880:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=200:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle"
    subprocess.run([*run, "-vf", palette, str(gif)], check=True)
    for f in (mp4, gif):
        print(f"{f.name}  {f.stat().st_size / 2**20:.1f} MB")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--home", required=True, type=Path, help="an isolated, neutral folder (wiped first)")
    ap.add_argument("--out", type=Path, help="default: docs/images (--views: site/src/assets/visualizations)")
    ap.add_argument("--lang", choices=["zh-CN", "en", "both"], default="zh-CN")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument(
        "--video", action="store_true", help="record the demo (GIF + MP4) instead of screenshots"
    )
    mode.add_argument("--views", action="store_true", help="the three visualizations (WebP) for the website")
    args = ap.parse_args()
    args.out = args.out or (VIEWS_OUT if args.views else ROOT / "docs" / "images")
    if not (ROOT / "apps" / "electron" / "out" / "renderer" / "index.html").exists():
        print("run `pnpm build` in apps/electron first", file=sys.stderr)
        return 1
    shutil.rmtree(args.home, ignore_errors=True)
    out = subprocess.run(
        [sys.executable, str(ROOT / "tools" / "demo" / "mock_snapshot.py"), "--home", str(args.home)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    newest = Path(next(p for p in reversed(out) if p.endswith(".sks")))
    langs = ["zh-CN", "en"] if args.lang == "both" else [args.lang]
    code = 0
    for lang in langs:
        code |= capture(args.home, args.out.resolve(), lang, newest, video=args.video, views=args.views)
    return code


if __name__ == "__main__":
    sys.exit(main())
