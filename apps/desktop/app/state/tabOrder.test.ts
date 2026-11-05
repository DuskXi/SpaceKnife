import { describe, expect, it } from "vitest";

import { dropIndex, moveTo, placeTab, stepIndex } from "./tabOrder";

const tabs = ["a", "b", "c", "d"].map((id) => ({ id }));
const ids = (list: { id: string }[]) => list.map((x) => x.id).join("");

describe("moving a tab", () => {
  it("puts it after `index` other tabs", () => {
    expect(ids(moveTo(tabs, "a", 2))).toBe("bcad");
    expect(ids(moveTo(tabs, "d", 0))).toBe("dabc");
    expect(ids(moveTo(tabs, "b", 3))).toBe("acdb");
  });

  it("clamps the slot, keeps the list for an unknown tab and never changes the input", () => {
    expect(ids(moveTo(tabs, "c", 99))).toBe("abdc");
    expect(ids(moveTo(tabs, "c", -5))).toBe("cabd");
    expect(ids(moveTo(tabs, "x", 1))).toBe("abcd");
    expect(ids(tabs)).toBe("abcd");
  });
});

describe("where a dragged tab lands", () => {
  // four tabs of 100 px from x = 0
  const boxes = tabs.map((t, i) => ({ id: t.id, left: i * 100, width: 100 }));

  it("passes a neighbour once the pointer crosses its middle", () => {
    expect(dropIndex(boxes, "a", 40)).toBe(0);
    expect(dropIndex(boxes, "a", 151)).toBe(1); // past b's middle
    expect(dropIndex(boxes, "a", 149)).toBe(0);
    expect(dropIndex(boxes, "d", 10)).toBe(0);
    expect(dropIndex(boxes, "d", 260)).toBe(3); // past c's middle: stays last
    expect(dropIndex(boxes, "b", 900)).toBe(3);
  });
});

describe("Ctrl+Shift+PgUp / PgDn", () => {
  it("moves one slot, not past the ends", () => {
    expect(stepIndex(tabs, "b", -1)).toBe(0);
    expect(stepIndex(tabs, "b", 1)).toBe(2);
    expect(stepIndex(tabs, "a", -1)).toBeNull();
    expect(stepIndex(tabs, "d", 1)).toBeNull();
    expect(stepIndex(tabs, "x", 1)).toBeNull();
  });
});

describe("a scan or snapshot opened from a start tab", () => {
  const scan = { id: "s" };

  it("takes the start tab's place, or comes last without one", () => {
    expect(ids(placeTab(tabs, scan, "b")!)).toBe("ascd");
    expect(ids(placeTab(tabs, scan)!)).toBe("abcds");
    expect(ids(tabs)).toBe("abcd");
  });

  it("has nowhere to go once that start tab was closed while it loaded", () => {
    // showScan then frees the new view and the scan: nothing stays in the backend with no tab
    expect(placeTab(tabs, scan, "x")).toBeNull();
    expect(placeTab([], scan, "a")).toBeNull();
  });
});
