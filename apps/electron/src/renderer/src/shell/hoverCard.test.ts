// The hover card's 「占 <父文件夹名> N%」 (UI-MAIN-30): divided by the parent fetched for the node on
// the card, never by the one before (round 4: 920 % on a scanned root after going up a level).

import { describe, expect, it } from "vitest";

import { shareBase } from "./HoverCard";

describe("shareBase", () => {
  const file = { parent: 7, alloc: 3 };

  it("divides by the parent fetched for this node", () => {
    expect(shareBase(file, { id: 7, alloc: 12 })).toBe(12);
  });

  it("writes no share while the node's own parent loads, or with the last card's", () => {
    expect(shareBase(file, null)).toBeNull();
    expect(shareBase(file, { id: 4, alloc: 1 })).toBeNull(); // the folder hovered before
    expect(shareBase(undefined, { id: 7, alloc: 12 })).toBeNull();
  });

  it("writes none for a scanned root (its parent is not in the scan) or an empty parent", () => {
    // the scanned folder hovered after going up: the card before it had a far smaller parent
    const scanned = { parent: 0, alloc: 38_600_000 };
    expect(shareBase(scanned, { id: 0, alloc: 4_200_000 })).toBeNull();
    expect(shareBase(file, { id: 7, alloc: 0 })).toBeNull();
  });
});
