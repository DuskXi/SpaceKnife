// Colour modes as the renderer offers them (docs/plugins.md 「color_mode」): the four built-in ones,
// then the enabled plugins' (extensions.get color_modes, colors.v1), for the toolbar, the C key and
// 「配色：…」. A plugin's mode paints from the frame's u1 `pal` column through its palette; the
// palette's entries are resolved here: a semantic name, a design token, class:<id> (the user's
// class colour), #RRGGBB, or {light, dark} (the app is dark: the dark one).

import { tokens } from "@design/tokens";

import type { ColorModeInfo, ExtensionsState } from "@/protocol/generated";
import { t } from "@/lib/i18n";

/** One entry of a palette: a semantic name, a token, class:<id>, #RRGGBB, or one per theme. */
export type PaletteEntry = ColorModeInfo["palette"][number];

export const BUILTIN_COLORS = ["class", "flat", "time", "branch"] as const;
export type BuiltinColorMode = (typeof BUILTIN_COLORS)[number];

export const isBuiltinColor = (id: string): id is BuiltinColorMode => (BUILTIN_COLORS as readonly string[]).includes(id);

/** The toolbar's short names of the built-in modes. */
export const builtinColorLabel = (m: BuiltinColorMode): string => ({ class: t("类别"), flat: t("扁平"), time: t("时间"), branch: t("分支") })[m];

export interface ColorChoice {
  id: string;
  title: string;
  /** null: built in */
  plugin: string | null;
  info: ColorModeInfo | null;
  /** why it can't be picked for this view (display text); null when it can */
  reason: string | null;
}

/** Every colour mode for a view drawn in `layout` (null: not known yet), built-in ones first. A
 * plugin's mode that does not apply to this layout stays listed with its reason. */
export function colorChoices(ext: ExtensionsState | null | undefined, layout: string | null): ColorChoice[] {
  const out: ColorChoice[] = BUILTIN_COLORS.map((id) => ({ id, title: builtinColorLabel(id), plugin: null, info: null, reason: null }));
  for (const m of ext?.color_modes ?? []) {
    if (!m.plugin) continue;
    const fits = !m.layouts.length || layout === null || m.layouts.includes(layout);
    out.push({ id: m.id, title: m.title, plugin: m.plugin, info: m, reason: fits ? null : t("这种配色不适用于当前可视化") });
  }
  return out;
}

/** The C key: the next (dir 1) or previous usable mode after `current`, wrapping around. */
export function nextColorMode(choices: readonly ColorChoice[], current: string, dir: 1 | -1 = 1): string {
  const usable = choices.filter((c) => c.reason === null);
  if (!usable.length) return current;
  const at = usable.findIndex((c) => c.id === current);
  if (at < 0) return usable[dir === 1 ? 0 : usable.length - 1]!.id;
  return usable[(at + dir + usable.length) % usable.length]!.id;
}

const SEMANTIC: Record<string, string> = {
  accent: tokens.color.accent.default,
  ok: tokens.color.status.success,
  warn: tokens.color.status.warning,
  danger: tokens.color.status.danger,
  muted: tokens.treemap.class.other,
};

const HEX = /^#[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?$/;

/** A token by its dotted path ("visual.branch.3.2", "treemap.class.video"); null when it is not a colour. */
function token(path: string): string | null {
  let node: unknown = tokens;
  for (const part of path.split(".")) {
    if (!node || typeof node !== "object" || !(part in node)) return null;
    node = (node as Record<string, unknown>)[part];
  }
  return typeof node === "string" && HEX.test(node) ? node : null;
}

/** One palette entry as a hex colour; the neutral colour for anything it can't read. */
export function resolveColor(entry: PaletteEntry | string | null | undefined, classHex: (id: string) => string): string {
  if (!entry) return SEMANTIC.muted!;
  if (typeof entry === "object") return HEX.test(entry.dark) ? entry.dark : HEX.test(entry.light) ? entry.light : SEMANTIC.muted!;
  if (HEX.test(entry)) return entry;
  if (entry in SEMANTIC) return SEMANTIC[entry]!;
  if (entry.startsWith("class:")) return classHex(entry.slice(6));
  return token(entry) ?? SEMANTIC.muted!;
}

/** A tone of a report block or field (accent ok warn danger muted, or a colour like a palette's). */
export function toneColor(tone: string | null | undefined, classHex: (id: string) => string): string | null {
  if (!tone) return null;
  return resolveColor(tone, classHex);
}

/** A plugin mode's palette as hex colours, index 0 the neutral one; at most 16 entries. */
export function paletteHex(info: ColorModeInfo, classHex: (id: string) => string): string[] {
  const list = info.palette.slice(0, 16).map((e) => resolveColor(e, classHex));
  return list.length ? list : [SEMANTIC.muted!];
}

/** What the painters need for a plugin's mode (null for the built-in ones). */
export interface PalettePaint {
  /** hex per palette index */
  colors: string[];
  /** open folders in the palette's colours too (else their level / ring tones) */
  folders: boolean;
}

export function palettePaint(id: string, ext: ExtensionsState | null | undefined, classHex: (id: string) => string): PalettePaint | null {
  if (isBuiltinColor(id)) return null;
  const info = ext?.color_modes.find((m) => m.id === id);
  if (!info) return { colors: [SEMANTIC.muted!], folders: false };
  return { colors: paletteHex(info, classHex), folders: info.folders === "palette" };
}

/** A mode's legend: (colour, text) per palette entry that has a text. */
export function legendOf(info: ColorModeInfo, classHex: (id: string) => string): { color: string; text: string }[] {
  const colors = paletteHex(info, classHex);
  return info.legend.map((text, i) => ({ color: colors[i] ?? colors[0]!, text })).filter((x) => x.text);
}
