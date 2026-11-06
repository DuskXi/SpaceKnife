import { describe, expect, it } from "vitest";

import { pointerBox } from "@/visual/hit";
import { hitOfKey } from "@/visual/hover";
import { encodeFrame, TREEMAP_ROWS } from "@/visual/testing/fixtures";
import { HIT_NONE, HIT_ROOT, type HoverInfo } from "@/visual/types";

import { cardAvoid, hoverFacts, placeCard, placeCompareCard, sameTarget, splitPlaceholders } from "./hoverCard";

const { frame } = encodeFrame("drawlist.v1", TREEMAP_ROWS);
const hover = (patch: Partial<HoverInfo> = {}): HoverInfo => ({ viewId: "v", frame, target: 0, avoid: { x: 10, y: 10, w: 50, h: 40 }, pointer: { x: 30, y: 30 }, placement: "beside", ...patch });

describe("hover card helpers", () => {
  it("knows the same target", () => {
    expect(sameTarget(hover(), hover({ pointer: { x: 1, y: 1 } }))).toBe(true);
    expect(sameTarget(hover(), hover({ target: 1 }))).toBe(false);
    expect(sameTarget(null, hover())).toBe(false);
  });

  it("keeps clear of the row, or of the pointer when it follows it", () => {
    expect(cardAvoid(hover())).toEqual({ x: 10, y: 10, w: 50, h: 40 });
    expect(cardAvoid(hover({ placement: "pointer" }))).toEqual(pointerBox({ x: 30, y: 30 }));
    expect(pointerBox({ x: 30, y: 30 }, 5)).toEqual({ x: 25, y: 25, w: 10, h: 10 });
  });

  it("reads a row's facts from the frame", () => {
    const f = hoverFacts(frame, 0, null);
    expect(f.key).toBe(frame.key[0]);
    expect(f.node).toBe(frame.node[0]);
    expect(f.row).toBe(0);
    expect(f.columns).toBe(frame);
  });

  it("finds a row by its key", () => {
    expect(hitOfKey(frame, frame.key[1]!)).toBe(1);
    expect(hitOfKey(frame, 0xdeadbeef)).toBe(frame.info.root?.key === 0xdeadbeef ? HIT_ROOT : HIT_NONE);
  });

  it("places the card right of the box, else left, else below", () => {
    const bounds = { w: 400, h: 300 };
    const card = { w: 100, h: 80 };
    expect(placeCard({ x: 10, y: 20, w: 50, h: 40 }, card, bounds)).toEqual({ left: 70, top: 20 });
    // no room on the right: left of it
    expect(placeCard({ x: 250, y: 20, w: 100, h: 40 }, card, bounds)).toEqual({ left: 140, top: 20 });
    // no room on either side: below
    expect(placeCard({ x: 50, y: 20, w: 300, h: 40 }, card, bounds)).toEqual({ left: 50, top: 70 });
    // always inside, with a margin
    expect(placeCard({ x: 10, y: 280, w: 20, h: 10 }, card, bounds)).toEqual({ left: 40, top: 216 });
  });

  it("places the compare card below right of the box's corner", () => {
    expect(placeCompareCard({ x: 10, y: 10, w: 100, h: 100 }, { w: 50, h: 50 }, { w: 400, h: 300 })).toEqual({ left: 46, top: 46 });
    expect(placeCompareCard({ x: 380, y: 280, w: 10, h: 10 }, { w: 50, h: 50 }, { w: 400, h: 300 })).toEqual({ left: 318, top: 242 });
  });

  it("splits a text at its placeholders", () => {
    expect(splitPlaceholders("占 {name} {share}")).toEqual([
      { text: "占 ", slot: false },
      { text: "name", slot: true },
      { text: " ", slot: false },
      { text: "share", slot: true },
      { text: "", slot: false },
    ]);
  });
});
