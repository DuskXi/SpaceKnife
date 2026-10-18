"""Render the app-icon candidates to PNG previews and a side-by-side overview.

    uv run python tools/render_app_icons.py

For every ``design/app-icon/candidates/<name>.svg`` (1024 px master) this writes
``<name>-16.png``, ``-32``, ``-48`` and ``-256`` next to it, rendered straight from the
master (no hand-hinted small variants), and ``overview.png``: every candidate at
256 / 48 / 32 / 16 px 1:1 plus 16 px blown up 4× (nearest neighbour), on a dark and a
light background.

Dev-only: needs ``resvg-py`` and ``pillow`` from the uv ``dev`` group.
"""

from __future__ import annotations

import io
import re
import sys
from pathlib import Path

import resvg_py
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
CANDIDATES = ROOT / "design" / "app-icon" / "candidates"
SIZES = (16, 32, 48, 256)

# Overview layout (px)
ZOOM = 4  # 16 px shown at 64 px, nearest neighbour
CELL_GAP = 36
PANEL_PAD = 32
ROW_PAD = 28
LABEL_W = 240
HEADER_H = 72
# window background of the app (color.bg.window) and a light window background
BACKGROUNDS = (("深色背景", (0x08, 0x09, 0x0B)), ("浅色背景", (0xF3, 0xF3, 0xF5)))
CAPTION_DARK = (0x8F, 0x95, 0xA2)  # color.text.tertiary
CAPTION_LIGHT = (0x5C, 0x61, 0x6C)
PAGE = (0x0E, 0x10, 0x13)  # color.bg.panel
TITLE = (0xF1, 0xF2, 0xF5)  # color.text.primary
SUBTITLE = (0xAE, 0xB3, 0xBE)  # color.text.secondary

_FONT_CANDIDATES = (
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/msyhbd.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
)


def _font(size: int) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    for path in _FONT_CANDIDATES:
        if Path(path).exists():
            return ImageFont.truetype(path, size)
    return ImageFont.load_default(size)


def render(svg: Path, size: int) -> Image.Image:
    png = resvg_py.svg_to_bytes(svg_path=str(svg), width=size, height=size)
    return Image.open(io.BytesIO(bytes(png))).convert("RGBA")


def title_of(svg: Path) -> str:
    """'A 刃面 Facet' from the <title> element, else the file stem."""
    match = re.search(r"<title>(?:[^<·]*·\s*)?([^<]+)</title>", svg.read_text(encoding="utf-8"))
    return match.group(1).strip() if match else svg.stem


def overview(svgs: list[Path], icons: dict[Path, dict[int, Image.Image]]) -> Image.Image:
    cells = [256, 48, 32, 16, 16 * ZOOM]
    panel_w = PANEL_PAD * 2 + sum(cells) + CELL_GAP * (len(cells) - 1)
    row_h = 256 + ROW_PAD * 2
    caption_h = 64
    width = LABEL_W + panel_w * len(BACKGROUNDS)
    height = HEADER_H + caption_h + row_h * len(svgs)
    page = Image.new("RGBA", (width, height), PAGE)
    draw = ImageDraw.Draw(page)
    draw.text((24, 22), "SpaceKnife 应用图标候选", font=_font(22), fill=TITLE)
    small = _font(13)
    label = _font(17)

    for p, (bg_name, bg) in enumerate(BACKGROUNDS):
        x0 = LABEL_W + p * panel_w
        draw.rectangle((x0, HEADER_H, x0 + panel_w - 1, height - 1), fill=bg)
        caption = CAPTION_DARK if sum(bg) < 384 else CAPTION_LIGHT
        draw.text((x0 + PANEL_PAD, HEADER_H + 12), bg_name, font=_font(15), fill=caption)
        x = x0 + PANEL_PAD
        for c, cell in enumerate(cells):
            text = f"16 ×{ZOOM}" if c == len(cells) - 1 else f"{cell}"
            draw.text((x, HEADER_H + 40), text, font=small, fill=caption)
            x += cell + CELL_GAP

    for r, svg in enumerate(svgs):
        y0 = HEADER_H + caption_h + r * row_h
        draw.text((24, y0 + ROW_PAD + 112), title_of(svg), font=label, fill=TITLE)
        for p in range(len(BACKGROUNDS)):
            x = LABEL_W + p * panel_w + PANEL_PAD
            for c, cell in enumerate(cells):
                if c == len(cells) - 1:
                    img = icons[svg][16].resize((cell, cell), Image.Resampling.NEAREST)
                else:
                    img = icons[svg][cell]
                # bottom-align so small sizes sit on a common baseline
                page.alpha_composite(img, (x, y0 + ROW_PAD + 256 - cell))
                x += cell + CELL_GAP
    return page.convert("RGB")


def main() -> int:
    svgs = sorted(CANDIDATES.glob("*.svg"), key=title_of)
    if not svgs:
        print(f"no candidates in {CANDIDATES}", file=sys.stderr)
        return 1
    icons: dict[Path, dict[int, Image.Image]] = {}
    for svg in svgs:
        icons[svg] = {}
        for size in SIZES:
            img = render(svg, size)
            img.save(svg.with_name(f"{svg.stem}-{size}.png"), optimize=True)
            icons[svg][size] = img
        print(f"{svg.relative_to(ROOT)}: {', '.join(str(s) for s in SIZES)} px")
    out = CANDIDATES / "overview.png"
    overview(svgs, icons).save(out, optimize=True)
    print(f"{out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
