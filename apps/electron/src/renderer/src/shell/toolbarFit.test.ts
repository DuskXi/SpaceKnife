// The toolbar's narrow-window rules (toolbarFit.ts): the order groups fold away in, and the width the
// toolbar needs. The width is measured on stand-in elements (the tests run without a DOM).

import { afterEach, describe, expect, it, vi } from "vitest";

import { collapse, MAX_LEVEL, neededWidth } from "./toolbarFit";

describe("collapse levels", () => {
  it("fold the groups away in the spec's order, each level keeping the ones before it", () => {
    const order = ["chipSwatches", "coloringInMenu", "chipsInMenu", "compact", "visualInMenu"] as const;
    expect(MAX_LEVEL).toBe(order.length);
    for (let level = 0; level <= MAX_LEVEL; level++) {
      const c = collapse(level);
      expect(order.map((k) => c[k]), `level ${level}`).toEqual(order.map((_, i) => i < level));
    }
  });
});

interface Part {
  width: number;
  /** a part that shrinks: its CSS min-width */
  min?: string;
  /** data-fit-min, with the width it takes when it gets all the room it wants */
  fitMin?: number;
  natural?: number;
  margin?: number;
  right?: boolean;
  hidden?: boolean;
}

function bar(parts: Part[], { gap = 12, padding = 12 } = {}): HTMLElement {
  const styles = new Map<object, Record<string, string>>();
  const children = parts.map((p) => {
    const style: Record<string, string> = {};
    const el = {
      style,
      classList: { contains: (c: string) => c === "toolbar-right" && !!p.right },
      getAttribute: (a: string) => (a === "data-fit-min" && p.fitMin != null ? String(p.fitMin) : null),
      // sized to max-content (naturalWidth): the width it wants, otherwise its width now
      getBoundingClientRect: () => ({ width: style.width === "max-content" ? (p.natural ?? p.width) : p.width }),
    };
    styles.set(el, {
      display: p.hidden ? "none" : "flex",
      position: "static",
      flexShrink: p.min != null || p.fitMin != null ? "1" : "0",
      minWidth: p.min ?? "auto",
      marginLeft: `${p.margin ?? 0}px`,
      marginRight: "0px",
    });
    return el;
  });
  const root = { children };
  styles.set(root, { columnGap: `${gap}px`, paddingLeft: `${padding}px`, paddingRight: `${padding}px` });
  vi.stubGlobal("getComputedStyle", (el: object) => styles.get(el));
  return root as unknown as HTMLElement;
}

describe("neededWidth", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("adds fixed parts at their width, shrinking ones at their minimum, padding and gaps", () => {
    // nav 134, filter (min 220, now 300), right group 500 with an auto margin of 90 that is free space
    const el = bar([{ width: 134 }, { width: 300, min: "220px", margin: 4 }, { width: 500, right: true, margin: 90 }]);
    expect(neededWidth(el)).toBe(12 + 134 + 12 + 220 + 4 + 12 + 500 + 12);
  });

  it("keeps a breadcrumb at its data-fit-min, or whole when it is shorter", () => {
    const long = bar([{ width: 40, fitMin: 160, natural: 350 }]);
    expect(neededWidth(long)).toBe(12 + 160 + 12);
    const short = bar([{ width: 40, fitMin: 160, natural: 93 }]);
    expect(neededWidth(short)).toBe(12 + 93 + 12);
  });

  it("skips hidden parts, gaps included", () => {
    const el = bar([{ width: 100 }, { width: 50, hidden: true }, { width: 100 }], { gap: 10, padding: 0 });
    expect(neededWidth(el)).toBe(210);
  });
});
