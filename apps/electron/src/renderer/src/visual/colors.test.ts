import { describe, expect, it } from "vitest";

import { tokens } from "@design/tokens";

import { rgba } from "@/treemap/paint";

import { branchFill, CENTER, HALO_OVERLAY, ringFill } from "./colors";

const V = tokens.visual;
const hue = (slot: number, tier: number) => (V.branch as unknown as Record<string, Record<string, string>>)[String(slot)]![String(tier)]!;

describe("branchFill", () => {
  it("takes the slot's hue at the depth's tier", () => {
    for (let slot = 1; slot <= 8; slot++) for (let depth = 1; depth <= 4; depth++) expect(branchFill(slot, depth)).toEqual(rgba(hue(slot, depth)));
  });

  it("shares tier 4 below depth 4 and tier 1 for the root's depth", () => {
    expect(branchFill(3, 9)).toEqual(rgba(hue(3, 4)));
    expect(branchFill(3, 0)).toEqual(rgba(hue(3, 1)));
  });

  it("is neutral for slot 0 and for slots the palette does not have", () => {
    for (const slot of [0, 9, 255, -1]) expect(branchFill(slot, 2)).toEqual(rgba(V.branch.neutral));
  });
});

describe("ringFill", () => {
  it("steps with depth and stays at the fourth below", () => {
    expect([1, 2, 3, 4, 7].map(ringFill)).toEqual([V.ring[1], V.ring[2], V.ring[3], V.ring[4], V.ring[4]].map((h) => rgba(h)));
  });
});

describe("root and halo colours", () => {
  it("read the visual tokens", () => {
    expect(CENTER.bg).toEqual(rgba(V.center.bg));
    expect([CENTER.text, CENTER.sub]).toEqual([V.center.text, V.center.sub]);
    expect(HALO_OVERLAY).toEqual(rgba(V.haloOverlay));
    expect(HALO_OVERLAY[3]).toBeCloseTo(0x1a / 255);
  });
});
