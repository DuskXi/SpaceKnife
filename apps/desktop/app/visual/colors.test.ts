import { afterEach, describe, expect, it } from "vitest";

import { type ThemeName, themes } from "@design/tokens";

import { applyTheme } from "@/lib/theme";
import { rgba } from "@/treemap/paint";

import { branchFill, center, haloOverlay, ringFill } from "./colors";

afterEach(() => void applyTheme("dark"));

describe.each(["dark", "light"] as ThemeName[])("%s theme", (theme) => {
  const V = themes[theme].visual;
  const hue = (slot: number, tier: number) => (V.branch as unknown as Record<string, Record<string, string>>)[String(slot)]![String(tier)]!;
  const on = () => applyTheme(theme);

  describe("branchFill", () => {
    it("takes the slot's hue at the depth's tier", () => {
      on();
      for (let slot = 1; slot <= 8; slot++) for (let depth = 1; depth <= 4; depth++) expect(branchFill(slot, depth)).toEqual(rgba(hue(slot, depth)));
    });

    it("shares tier 4 below depth 4 and tier 1 for the root's depth", () => {
      on();
      expect(branchFill(3, 9)).toEqual(rgba(hue(3, 4)));
      expect(branchFill(3, 0)).toEqual(rgba(hue(3, 1)));
    });

    it("is neutral for slot 0 and for slots the palette does not have", () => {
      on();
      for (const slot of [0, 9, 255, -1]) expect(branchFill(slot, 2)).toEqual(rgba(V.branch.neutral));
    });
  });

  describe("ringFill", () => {
    it("steps with depth and stays at the fourth below", () => {
      on();
      expect([1, 2, 3, 4, 7].map(ringFill)).toEqual([V.ring[1], V.ring[2], V.ring[3], V.ring[4], V.ring[4]].map((h) => rgba(h)));
    });
  });

  describe("root and halo colours", () => {
    it("read the visual tokens, the root's the level-0 header's", () => {
      on();
      expect(center().bg).toEqual(rgba(V.center.bg));
      expect(V.center.bg).toBe(themes[theme].treemap.level[0].headerBg);
      expect([center().text, center().sub]).toEqual([V.center.text, V.center.sub]);
      expect(haloOverlay()).toEqual(rgba(V.haloOverlay));
    });
  });
});

it("changes with the theme", () => {
  applyTheme("dark");
  const dark = [branchFill(1, 1), ringFill(1), center().bg, haloOverlay()];
  applyTheme("light");
  const light = [branchFill(1, 1), ringFill(1), center().bg, haloOverlay()];
  for (let k = 0; k < dark.length; k++) expect(light[k]).not.toEqual(dark[k]);
  // the halo lightens in the dark theme and darkens in the light one, at the same strength
  expect(haloOverlay()).toEqual(rgba(themes.light.visual.haloOverlay));
  expect(haloOverlay()[0]).toBeLessThan(0.1);
  applyTheme("dark");
  expect(haloOverlay()[0]).toBe(1);
  expect(haloOverlay()[3]).toBeCloseTo(0x1a / 255);
});
