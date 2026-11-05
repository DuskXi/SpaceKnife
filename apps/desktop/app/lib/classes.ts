// File class colours (W7 「文件类别」): a class the user coloured uses that colour, the others the
// theme's colour for their id; user-made classes without a colour fall back to 「其他」. Each theme
// has its own (W7 待定 22): `color` for the dark one, `color_light` for the light one, "" for the
// theme's; a class of the user's has no token, so without a colour in one theme it takes the one of
// the other (a classes.json from before themes has the same colour in both, classes.py).

import type { ThemeName } from "@design/tokens";

import type { FilterClassInfo, FilterVocab } from "@/protocol/generated";
import { get } from "@/state/store";
import { labelColors, rgba } from "@/treemap/paint";

import { palette, themeName } from "./theme";

type Coloured = Pick<FilterClassInfo, "id" | "color" | "color_light">;

/** The theme's colour for a built-in class id, undefined for a class of the user's. */
export const themeColor = (id: string): string | undefined => (palette().treemap.class as Record<string, string>)[id];

/** The user's colour for a class in `theme`, "" when the theme's own applies. */
export function userColor(c: Coloured, theme: ThemeName = themeName()): string {
  const [own, other] = theme === "light" ? [c.color_light, c.color] : [c.color, c.color_light];
  return own || (themeColor(c.id) ? "" : other);
}

const hexOf = (c: Coloured | undefined, id: string): string => (c && userColor(c)) || themeColor(id) || palette().treemap.class.other;

/** Hex colour of a class in the theme on screen. */
export const colorOf = (c: Coloured): string => hexOf(c, c.id);

/** Hex colour of a class (treemap fills, swatches) in the theme on screen. */
export function classHex(vocab: FilterVocab | null | undefined, id: string): string {
  return hexOf(vocab?.classes.find((x) => x.id === id), id);
}

/** Hex colour per class index of a frame's `cls` column (the vocabulary's order). */
export function classColors(vocab: FilterVocab | null | undefined): string[] {
  return (vocab?.classes ?? []).map(colorOf);
}

/** classHex with the vocabulary in the store now (swatches in lists and panels). */
export const classHexNow = (id: string): string => classHex(get().vocab, id);

/** What goes on a class's colour (an icon on its swatch): white or near-black, as on its blocks (UI-TM-19). */
export const inkOn = (hex: string): string => labelColors(rgba(hex))[0];
