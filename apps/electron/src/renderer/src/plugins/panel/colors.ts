// sk.colors: the colours a view paints some nodes with, for a web panel that draws them its own way
// (docs/plugin-dev.md 「Web 面板」). The backend says what colours each node (colors.of: class,
// age band, branch slot, a plugin palette index); this turns that into the colours of the theme on
// screen with the same functions the visualizations paint with, and a legend of what is there.
// Pure: tested under node.

import type { ExtensionsState, FilterVocab, ViewColors } from "@/protocol/generated";
import { classColors } from "@/lib/classes";
import { t } from "@/lib/i18n";
import { fillOf, paletteFill, type RGBA } from "@/treemap/paint";
import { branchFill, ringFill } from "@/visual/colors";
import { Kind } from "@/visual/frame";

import { legendOf, palettePaint } from "../colors";

/** What sk.colors answers: a colour per node (#rrggbb, "" for what is not a node), the mode they
 * are of, and a legend of the colours among them (classes, age bands, a plugin's palette). */
export interface PanelColors {
  mode: string;
  colors: string[];
  legend: { label: string; color: string }[];
}

const hex = (c: RGBA): string =>
  "#" +
  [c[0], c[1], c[2]]
    .map((v) =>
      Math.round(Math.min(1, Math.max(0, v)) * 255)
        .toString(16)
        .padStart(2, "0"),
    )
    .join("");

/** The age bands as the time colour mode names them (tone 0 … 4). */
export const TONE_LABELS = () => [t("1 天内"), t("1 周内"), t("1 月内"), t("1 年内"), t("更早或未知")];

export function panelColors(r: ViewColors, vocab: FilterVocab | null | undefined, ext: ExtensionsState | null | undefined, classHex: (id: string) => string, timeHex: (tone: number) => string): PanelColors {
  const n = r.kind.length;
  const classIds = vocab?.classes.map((c) => c.id) ?? [];
  const colors = classColors(vocab);
  const pal = palettePaint(r.mode, ext, classHex);
  const frame = {
    kind: Uint8Array.from(r.kind, (k) => (k < 0 ? Kind.FILE : k)),
    cls: Uint8Array.from(r.cls),
    tone: Uint8Array.from(r.tone),
    branch: Uint8Array.from(r.branch),
    depth: Uint8Array.from(r.depth, (d) => Math.max(1, d)),
    pal: r.pal ? Uint8Array.from(r.pal) : null,
    delta: null,
  };
  const out: string[] = [];
  const seenCls = new Set<number>();
  const seenTone = new Set<number>();
  const seenPal = new Set<number>();
  for (let i = 0; i < n; i++) {
    if (r.kind[i]! < 0) {
      out.push("");
      continue;
    }
    const folder = frame.kind[i] !== Kind.FILE;
    let c: RGBA;
    // as the flame graphs and the plugins' layouts paint: folders on the level tones, files by the mode
    if (r.mode === "branch") c = branchFill(frame.branch[i]!, frame.depth[i]!);
    else if (folder) c = pal?.folders ? paletteFill(frame, i, pal) : ringFill(frame.depth[i]!);
    else c = fillOf(frame, i, r.mode, classIds, undefined, colors, pal);
    out.push(hex(c));
    if (!folder) {
      seenCls.add(frame.cls[i]!);
      seenTone.add(frame.tone[i]!);
    }
    if (frame.pal) seenPal.add(frame.pal[i]!);
  }
  const legend: PanelColors["legend"] = [];
  if (r.mode === "class") {
    for (const k of [...seenCls].sort((a, b) => a - b)) {
      const info = vocab?.classes[k];
      if (info) legend.push({ label: info.name, color: colors[k] ?? classHex(info.id) });
    }
  } else if (r.mode === "time") {
    const labels = TONE_LABELS();
    for (const k of [...seenTone].sort((a, b) => a - b)) legend.push({ label: labels[k] ?? "", color: timeHex(k) });
  } else if (r.palette) {
    const info = ext?.color_modes.find((m) => m.id === r.palette);
    if (info) {
      const entries = legendOf(info, classHex);
      info.legend.forEach((text, k) => {
        if (text && seenPal.has(k)) legend.push({ label: text, color: entries.find((e) => e.text === text)?.color ?? "" });
      });
    }
  }
  return { mode: r.mode, colors: out, legend };
}
