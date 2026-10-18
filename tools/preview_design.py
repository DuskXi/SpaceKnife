"""Render a self-contained HTML preview of the design tokens and icons.

Usage::

    uv run python tools/preview_design.py <output.html>

Shows the A2 chrome palette, the treemap ``carbon`` palette with label contrast ratios, type
scale, icons and a small chrome sample — a visual check that the token pipeline is right.
"""

from __future__ import annotations

import html
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gen_design as g


def _lum(hex_color: str) -> float:
    h = hex_color.lstrip("#")[:6]
    vals = [int(h[i : i + 2], 16) / 255 for i in (0, 2, 4)]
    lin = [v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4 for v in vals]
    return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2]


def contrast(a: str, b: str) -> float:
    la, lb = _lum(a), _lum(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def best_ink(bg: str) -> tuple[str, float]:
    light, dark = "#FFFFFF", "#0A0B0D"
    cl, cd = contrast(light, bg), contrast(dark, bg)
    return (light, cl) if cl >= cd else (dark, cd)


def build(tokens: list[g.Token], icons: dict[str, str], meta: dict[str, object]) -> str:
    t = {tok.dotted: tok for tok in tokens}
    css = g.emit_css(tokens)
    esc = html.escape

    def swatches(prefix: str, against: str | None = None) -> str:
        cards = []
        for tok in tokens:
            if tok.type != "color" or not tok.dotted.startswith(prefix):
                continue
            value = str(tok.value)
            note = ""
            if against:
                note = f"<span class='ratio'>{contrast(value, against):.1f}:1</span>"
            desc = f"<small>{esc(tok.description or '')}</small>" if tok.description else ""
            cards.append(
                f"<div class='sw'><div class='chip' style='background:{value}'></div>"
                f"<div><code>{esc(tok.dotted.removeprefix(prefix))}</code> {note}<br><b>{value}</b>{desc}</div></div>"
            )
        return "<div class='grid'>" + "".join(cards) + "</div>"

    classes = ["video", "image", "audio", "archive", "exec", "doc", "other"]
    names = {
        "video": "视频",
        "image": "图片",
        "audio": "音频",
        "archive": "压缩包/镜像",
        "exec": "程序/系统",
        "doc": "文档",
        "other": "其他",
    }
    blocks = []
    for c in classes:
        bg = str(t[f"treemap.class.{c}"].value)
        ink, ratio = best_ink(bg)
        blocks.append(
            f"<div class='tm' style='background:{bg};color:{ink}'><b>{names[c]}</b>"
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
    icon = lambda n: (  # noqa: E731
        f"<svg width='16' height='16' viewBox='0 0 16 16' fill='none' stroke='currentColor' stroke-width='1.5' "
        f"stroke-linecap='round' stroke-linejoin='round'>{icons[n]}</svg>"
    )
    chrome = f"""
    <div class='win'>
      <div class='titlebar'>{icon("logo")}<b>SpaceKnife</b>
        <div class='tab on'>{icon("drive")} C: 系统 {icon("close")}</div><div class='tab'>{icon("drive")} D: 数据</div>
        <div class='spacer'></div>{icon("windowMinimize")}{icon("windowMaximize")}{icon("windowClose")}</div>
      <div class='toolbar'>{icon("back")}{icon("forward")}{icon("up")}{icon("home")}
        <span class='crumb'>C: 系统</span>
        <div class='filter'>{icon("filter")} <span>过滤  例如 *.mp4; &gt;1gb; :class:视频</span><kbd>/</kbd></div>
        <div class='spacer'></div><span class='seg'>{icon("layers")} 层级 5</span>{icon("split")}{icon("command")}{icon("basket")}</div>
      <div class='statusbar'>{icon("bolt")} MFT 快速扫描 · 1,284,635 项 · 4.8 秒 <i></i>{icon("pulse")} 实时监控中 <i></i>
        {icon("gauge")} 磁盘负载 均衡 ≤70% · 当前 8% <div class='spacer'></div> 可用 182.4 GB / 961 GB</div>
    </div>"""
    return f"""<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>SpaceKnife 设计 token 预览</title>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600&family=Geist+Mono:wght@400;500&family=Noto+Sans+SC:wght@400;500;600&display=swap">
<style>
{css}
* {{ box-sizing: border-box; }}
body {{ margin: 0; background: var(--sk-color-bg-window); color: var(--sk-color-text-primary);
  font-family: var(--sk-font-family-ui); font-size: var(--sk-font-size-body); line-height: var(--sk-font-line-height-body); }}
main {{ max-width: 1240px; margin: 0 auto; padding: 32px 24px 64px; }}
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
.tm {{ width: 168px; height: 92px; padding: 6px; border-radius: var(--sk-treemap-geometry-file-radius); display: flex; flex-direction: column; gap: 2px; font-size: var(--sk-font-size-small); }}
.tm small {{ color: inherit; opacity: .86; margin-top: auto; }}
.tm .mono {{ opacity: .86; }}
.steps {{ display: flex; gap: 4px; align-items: center; }}
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
<p class="lead">M0 生成物检查：本页所有颜色、尺寸、字体都来自 tools/gen_design.py 生成的 tokens.css，图标来自 icons.json。</p>
<h2>外框样例（A2）</h2>{chrome}
<h2>外框色板 · 文字色对 bg.panel 的对比度</h2>{swatches("color.text.", str(t["color.bg.panel"].value))}
<h2>外框色板 · 背景、线条、强调、状态、标签</h2>{swatches("color.bg.")}{"<div style='height:10px'></div>"}{swatches("color.line.")}
{"<div style='height:10px'></div>"}{swatches("color.accent.")}{"<div style='height:10px'></div>"}{swatches("color.status.")}{"<div style='height:10px'></div>"}{swatches("color.tag.")}
<h2>树图 carbon · 文件类别（文字自动取对比度更高的黑或白）</h2><div class="tms">{"".join(blocks)}</div>
<h2>树图 carbon · 可用 / 未知 / 等待扫描</h2><div class="tms">{"".join(stripes)}</div>
<h2>树图 carbon · 层级标题栏</h2>{swatches("treemap.level.")}
<h2>快照对比色阶（增长 4 档 · 减少 3 档）</h2><div class="steps">{diff}</div>
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
    out.write_text(build(g.load_tokens(), icons, meta), encoding="utf-8")
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
