"""Render a self-contained HTML preview of the design tokens and icons.

Usage::

    uv run python tools/preview_design.py <output.html>

Shows the A2 chrome palette, the treemap ``carbon`` palette with label contrast ratios and a small
chrome sample in each theme (dark and light side by side, each in a ``data-theme`` section of the
generated tokens.css), then the type scale and icons — a visual check that the token pipeline is
right.
"""

from __future__ import annotations

import html
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gen_design as g

THEME_TITLES = {"dark": "深色", "light": "浅色"}
CLASS_NAMES = {
    "video": "视频",
    "image": "图片",
    "audio": "音频",
    "archive": "压缩包/镜像",
    "exec": "程序/系统",
    "doc": "文档",
    "game": "游戏",
    "code": "代码/开发",
    "data": "数据/模型",
    "other": "其他",
}


def _lum(hex_color: str) -> float:
    h = hex_color.lstrip("#")[:6]
    vals = [int(h[i : i + 2], 16) / 255 for i in (0, 2, 4)]
    lin = [v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4 for v in vals]
    return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2]


def contrast(a: str, b: str) -> float:
    la, lb = _lum(a), _lum(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def best_ink(bg: str, t: dict[str, g.Token]) -> tuple[str, float]:
    """UI-TM-19: the theme's white or near-black label, whichever contrasts more."""
    light, dark = str(t["treemap.label.light"].value), str(t["treemap.label.dark"].value)
    cl, cd = contrast(light, bg), contrast(dark, bg)
    return (light, cl) if cl >= cd else (dark, cd)


def _icon(icons: dict[str, str], name: str) -> str:
    return (
        "<svg width='16' height='16' viewBox='0 0 16 16' fill='none' stroke='currentColor' stroke-width='1.5' "
        f"stroke-linecap='round' stroke-linejoin='round'>{icons[name]}</svg>"
    )


def theme_section(tokens: list[g.Token], icons: dict[str, str]) -> str:
    """One theme's palette: the colours come from its data-theme block, the ratios from its tokens."""
    t = {tok.dotted: tok for tok in tokens}
    esc = html.escape

    def swatches(prefix: str, against: str | None = None) -> str:
        cards = []
        for tok in tokens:
            if tok.type != "color" or not tok.dotted.startswith(prefix):
                continue
            value = str(tok.value)
            note = f"<span class='ratio'>{contrast(value, against):.1f}:1</span>" if against else ""
            desc = f"<small>{esc(tok.description or '')}</small>" if tok.description else ""
            cards.append(
                f"<div class='sw'><div class='chip' style='background:{value}'></div>"
                f"<div><code>{esc(tok.dotted.removeprefix(prefix))}</code> {note}<br><b>{value}</b>{desc}</div></div>"
            )
        return "<div class='grid'>" + "".join(cards) + "</div>"

    blocks = []
    for c, label in CLASS_NAMES.items():
        bg = str(t[f"treemap.class.{c}"].value)
        ink, ratio = best_ink(bg, t)
        blocks.append(
            f"<div class='tm' style='background:{bg};color:{ink}'><b>{label}</b>"
            f"<span class='mono'>61.2 GB</span><small>{bg} · 文字 {ratio:.1f}:1</small></div>"
        )
    stripes = []
    for kind, label in (("free", "可用空间"), ("unknown", "未知空间"), ("pending", "等待扫描…")):
        a, b = t[f"treemap.{kind}.stripeA"].value, t[f"treemap.{kind}.stripeB"].value
        border = t[f"treemap.{kind}.border"].value
        text = t[f"treemap.{kind}.text"].value
        stripes.append(
            f"<div class='tm' style='background:repeating-linear-gradient(135deg,{a} 0 7px,{b} 7px 14px);"
            f"border:1px {'solid' if kind == 'free' else 'dashed'} {border};color:{text}'><b>{label}</b>"
            f"<span class='mono'>182.4 GB</span><small>文字 {contrast(str(text), str(a)):.1f}:1</small></div>"
        )
    diff = (
        "".join(
            f"<div class='step' style='background:{tok.value}'></div>"
            for tok in tokens
            if tok.dotted.startswith("treemap.diff.grow")
        )
        + "<span style='width:12px'></span>"
        + "".join(
            f"<div class='step' style='background:{tok.value}'></div>"
            for tok in tokens
            if tok.dotted.startswith("treemap.diff.shrink")
        )
    )
    branch = "".join(
        "<div class='steps'>"
        + "".join(
            f"<div class='step' style='background:{t[f'visual.branch.{slot}.{tier}'].value}'></div>"
            for tier in range(1, 5)
        )
        + "</div>"
        for slot in range(1, 9)
    )
    icon = lambda n: _icon(icons, n)  # noqa: E731
    chrome = f"""
    <div class='win'>
      <div class='titlebar'>{icon("logo")}<b>SpaceKnife</b>
        <div class='tab on'>{icon("drive")} C: 系统 {icon("close")}</div><div class='tab'>{icon("drive")} D: 数据</div>
        <div class='spacer'></div>{icon("windowMinimize")}{icon("windowMaximize")}{icon("windowClose")}</div>
      <div class='toolbar'>{icon("back")}{icon("forward")}{icon("up")}{icon("home")}
        <span class='crumb'>C: 系统</span>
        <div class='filter'>{icon("filter")} <span>过滤  例如 *.mp4; &gt;1gb; :class:视频</span><kbd>/</kbd></div>
        <div class='spacer'></div><span class='seg'>{icon("layers")} 层级 5</span>{icon("command")}</div>
      <div class='statusbar'>{icon("bolt")} MFT 快速扫描 · 1,284,635 项 · 4.8 秒 <i></i>
        <div class='spacer'></div> 可用 182.4 GB / 961 GB</div>
    </div>"""
    gap = "<div style='height:10px'></div>"
    return f"""
<h2>外框样例（A2）</h2>{chrome}
<h2>外框色板 · 文字色对 bg.panel 的对比度</h2>{swatches("color.text.", str(t["color.bg.panel"].value))}
<h2>外框色板 · 背景、线条、强调、状态、标签</h2>{swatches("color.bg.")}{gap}{swatches("color.line.")}
{gap}{swatches("color.accent.")}{gap}{swatches("color.status.")}{gap}{swatches("color.tag.")}
<h2>树图 carbon · 文件类别（文字自动取对比度更高的黑或白）</h2><div class="tms">{"".join(blocks)}</div>
<h2>树图 carbon · 可用 / 未知 / 等待扫描</h2><div class="tms">{"".join(stripes)}</div>
<h2>树图 carbon · 层级标题栏</h2>{swatches("treemap.level.")}
<h2>快照对比色阶（增长 4 档 · 减少 3 档）</h2><div class="steps">{diff}</div>
<h2>「顶层分支」配色（8 个槽位 × 4 档）</h2><div class="branch">{branch}</div>
"""


def build(themes: dict[str, list[g.Token]], icons: dict[str, str], meta: dict[str, object]) -> str:
    css = g.emit_css(themes)
    esc = html.escape
    tokens = themes[g.BASE_THEME]
    sections = "".join(
        f"<section data-theme='{name}'><h1>{THEME_TITLES.get(name, name)}</h1>{theme_section(theme, icons)}</section>"
        for name, theme in themes.items()
    )
    sizes = "".join(
        f"<div class='type' style='font-size:{tok.value}px'><code>{tok.path[-1]} · {tok.value}px</code>"
        f" C: 系统 · <span class='mono'>778.6 GB</span> SpaceKnife 你好</div>"
        for tok in tokens
        if tok.dotted.startswith("font.size.")
    )
    icon_cells = "".join(
        f"<div class='ic'><svg width='20' height='20' viewBox='{meta['viewBox']}' fill='none' stroke='currentColor' "
        f"stroke-width='{meta['strokeWidth']}' stroke-linecap='round' stroke-linejoin='round'>{svg}</svg>"
        f"<code>{esc(name)}</code></div>"
        for name, svg in icons.items()
    )
    return f"""<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>SpaceKnife 设计 token 预览</title>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600&family=Geist+Mono:wght@400;500&family=Noto+Sans+SC:wght@400;500;600&display=swap">
<style>
{css}
* {{ box-sizing: border-box; }}
body {{ margin: 0; background: var(--sk-color-bg-window); color: var(--sk-color-text-primary);
  font-family: var(--sk-font-family-ui); font-size: var(--sk-font-size-body); line-height: var(--sk-font-line-height-body); }}
main {{ padding: 32px 24px 64px; }}
.themes {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(560px, 1fr)); gap: 24px; }}
section {{ min-width: 0; padding: 20px; border-radius: var(--sk-radius-lg); background: var(--sk-color-bg-window);
  color: var(--sk-color-text-primary); border: 1px solid var(--sk-color-line-strong); }}
h1 {{ font-size: var(--sk-font-size-heading); margin: 0 0 4px; }}
h2 {{ font-size: var(--sk-font-size-title); margin: 36px 0 12px; color: var(--sk-color-text-secondary); }}
p.lead {{ color: var(--sk-color-text-tertiary); margin: 0 0 8px; }}
code, .mono {{ font-family: var(--sk-font-family-mono); font-variant-numeric: tabular-nums; }}
small {{ display: block; color: var(--sk-color-text-tertiary); font-size: var(--sk-font-size-small); }}
.grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr)); gap: 10px; }}
.sw {{ display: flex; gap: 10px; align-items: flex-start; padding: 10px; background: var(--sk-color-bg-panel);
  border: 1px solid var(--sk-color-line-subtle); border-radius: var(--sk-radius-lg); font-size: var(--sk-font-size-label); }}
.chip {{ width: 36px; height: 36px; border-radius: var(--sk-radius-md); border: 1px solid var(--sk-color-line-strong); flex: none; }}
.ratio {{ color: var(--sk-color-status-success); font-family: var(--sk-font-family-mono); }}
.tms {{ display: flex; flex-wrap: wrap; gap: var(--sk-treemap-geometry-gap); padding: 6px; background: var(--sk-treemap-ground); border-radius: var(--sk-radius-lg); }}
.tm {{ width: 150px; height: 92px; padding: 6px; border-radius: var(--sk-treemap-geometry-file-radius); display: flex; flex-direction: column; gap: 2px; font-size: var(--sk-font-size-small); }}
.tm small {{ color: inherit; opacity: .86; margin-top: auto; }}
.tm .mono {{ opacity: .86; }}
.steps {{ display: flex; gap: 4px; align-items: center; }}
.branch {{ display: grid; gap: 4px; padding: 6px; background: var(--sk-treemap-ground); border-radius: var(--sk-radius-lg); }}
.step {{ width: 48px; height: 28px; border-radius: 3px; }}
.type {{ padding: 6px 0; border-bottom: 1px solid var(--sk-color-line-subtle); }}
.type code {{ display: inline-block; width: 150px; color: var(--sk-color-text-tertiary); font-size: 11px; }}
.icons {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(118px, 1fr)); gap: 8px; }}
.ic {{ display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 12px 6px; background: var(--sk-color-bg-panel);
  border: 1px solid var(--sk-color-line-subtle); border-radius: var(--sk-radius-lg); color: var(--sk-color-text-secondary); }}
.ic code {{ font-size: 10px; color: var(--sk-color-text-tertiary); }}
.win {{ border: 1px solid var(--sk-color-line-strong); border-radius: var(--sk-radius-lg); overflow: hidden; }}
.win > div {{ display: flex; align-items: center; gap: 10px; padding: 0 12px; color: var(--sk-color-text-secondary); }}
.titlebar {{ height: var(--sk-layout-titlebar); background: var(--sk-color-bg-window); border-bottom: 1px solid var(--sk-color-line-subtle); }}
.titlebar svg:first-child {{ color: var(--sk-color-accent-default); }}
.tab {{ display: flex; gap: 6px; align-items: center; height: 30px; padding: 0 10px; border-radius: 8px 8px 0 0; align-self: flex-end; }}
.tab.on {{ background: var(--sk-color-bg-chrome); color: var(--sk-color-text-primary); }}
.toolbar {{ height: var(--sk-layout-toolbar); background: var(--sk-color-bg-chrome); border-bottom: 1px solid var(--sk-color-line-subtle); }}
.crumb {{ color: var(--sk-color-text-primary); font-weight: 600; }}
.filter {{ display: flex; align-items: center; gap: 8px; height: 32px; padding: 0 10px; flex: 1; max-width: 420px;
  background: var(--sk-color-bg-raised); border: 1px solid var(--sk-color-line-strong); border-radius: var(--sk-radius-md); color: var(--sk-color-text-tertiary); }}
.filter span {{ flex: 1; }}
kbd {{ font-family: var(--sk-font-family-mono); font-size: 11px; padding: 0 5px; border: 1px solid var(--sk-color-line-strong); border-radius: 4px; }}
.seg {{ display: flex; gap: 6px; align-items: center; height: 32px; padding: 0 10px; border: 1px solid var(--sk-color-line-strong); border-radius: var(--sk-radius-md); }}
.statusbar {{ height: var(--sk-layout-statusbar); background: var(--sk-color-bg-chrome); font-size: var(--sk-font-size-label); font-variant-numeric: tabular-nums; }}
.statusbar svg:first-child {{ color: var(--sk-color-accent-default); }}
.statusbar i {{ width: 1px; height: 14px; background: var(--sk-color-line-subtle); }}
.spacer {{ flex: 1; }}
</style></head>
<body><main>
<h1>SpaceKnife 设计 token 预览</h1>
<p class="lead">本页所有颜色、尺寸、字体都来自 tools/gen_design.py 生成的 tokens.css（每个主题一节，各在自己的 data-theme 下），图标来自 icons.json。</p>
<div class="themes">{sections}</div>
<h2>字号</h2>{sizes}
<h2>图标（{len(icons)} 个）</h2><div class="icons">{icon_cells}</div>
</main></body></html>
"""


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: preview_design.py <output.html>", file=sys.stderr)
        return 2
    meta, icons = g.load_icons()
    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(build(g.load_themes(), icons, meta), encoding="utf-8")
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
