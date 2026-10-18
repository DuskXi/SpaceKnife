// File class colours (W7 「文件类别」): a class the user coloured uses that colour, the others the
// theme's colour for their id; user-made classes without a colour fall back to 「其他」.

import { tokens } from "@design/tokens";

import type { FilterVocab } from "@/protocol/generated";
import { get } from "@/state/store";

const THEME = tokens.treemap.class as Record<string, string>;

/** Hex colour of a class (treemap fills, swatches). */
export function classHex(vocab: FilterVocab | null | undefined, id: string): string {
  const c = vocab?.classes.find((x) => x.id === id);
  return c?.color || THEME[id] || THEME.other!;
}

/** Hex colour per class index of a frame's `cls` column (the vocabulary's order). */
export function classColors(vocab: FilterVocab | null | undefined): string[] {
  return (vocab?.classes ?? []).map((c) => c.color || THEME[c.id] || THEME.other!);
}

/** classHex with the vocabulary in the store now (swatches in lists and panels). */
export const classHexNow = (id: string): string => classHex(get().vocab, id);
