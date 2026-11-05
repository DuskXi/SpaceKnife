import { describe, expect, it } from "vitest";

import { HoverLight, hitOfKey, hoverTarget, sameTarget, targetKey } from "./hover";
import { rectScene } from "./testing/fixtures";
import { treemapGeometry as g } from "./treemap/geometry";
import { HIT_NONE, type Hit, type RectFrame } from "./types";

/** An instance as the components behave: a new frame drops the lit row. */
class FakeInstance {
  frame: RectFrame | null = null;
  lit: Hit = HIT_NONE;
  setFrame(f: RectFrame): void {
    if (f !== this.frame) this.lit = HIT_NONE;
    this.frame = f;
  }
  setHover(hit: Hit): void {
    this.lit = hit;
  }
}

type Pointer = { x: number; y: number } | null;

/** What the host does with a picture: the instance gets it, then the row is worked out again. */
function show(inst: FakeInstance, light: HoverLight, f: RectFrame, pointer: Pointer, key: number | null, moving = false): void {
  inst.setFrame(f);
  light.dropped();
  light.set(inst, hoverTarget({ frame: f, moving, pointer: pointer ? g.hitTest(f, pointer.x, pointer.y) : null, key }));
}

const picture = (seq: number) => rectScene({ info: { seq } }).frame;
const AT_ROW_3 = { x: 110, y: 50 }; // 中文.txt inside sub (treemap geometry test)

describe("hovered row", () => {
  it("stays on the block under a still pointer through new pictures (a file change redraws the view)", () => {
    const inst = new FakeInstance();
    const light = new HoverLight();
    show(inst, light, picture(1), AT_ROW_3, null);
    expect(inst.lit).toBe(3);
    for (let seq = 2; seq < 6; seq++) {
      show(inst, light, picture(seq), AT_ROW_3, null);
      expect(inst.lit).toBe(3);
    }
  });

  it("stays on a row lit from outside (result list) through new pictures", () => {
    const inst = new FakeInstance();
    const light = new HoverLight();
    const f1 = picture(1);
    const key = f1.key[1]!;
    show(inst, light, f1, null, key);
    expect(inst.lit).toBe(1);
    show(inst, light, picture(2), null, key);
    expect(inst.lit).toBe(1);
  });

  it("follows the pointer over the picture, not an outside key left behind", () => {
    const f = picture(1);
    expect(hoverTarget({ frame: f, moving: false, pointer: 3, key: f.key[1]! })).toBe(3);
    expect(hoverTarget({ frame: f, moving: false, pointer: HIT_NONE, key: f.key[1]! })).toBe(HIT_NONE);
    expect(hoverTarget({ frame: f, moving: false, pointer: null, key: f.key[1]! })).toBe(1);
  });

  it("lights nothing during a transition and the row again at its end", () => {
    const inst = new FakeInstance();
    const light = new HoverLight();
    const f = picture(1);
    show(inst, light, f, AT_ROW_3, null, true);
    expect(inst.lit).toBe(HIT_NONE);
    light.set(inst, hoverTarget({ frame: f, moving: false, pointer: g.hitTest(f, AT_ROW_3.x, AT_ROW_3.y), key: null }));
    expect(inst.lit).toBe(3);
  });

  it("lights nothing for a key the picture does not show, or without a picture", () => {
    const f = picture(1);
    expect(hitOfKey(f, 987_654_321)).toBe(HIT_NONE);
    expect(hoverTarget({ frame: f, moving: false, pointer: null, key: 987_654_321 })).toBe(HIT_NONE);
    expect(hoverTarget({ frame: null, moving: false, pointer: 3, key: null })).toBe(HIT_NONE);
  });

  it("reaches the instance once per change, and again after the instance dropped its row", () => {
    const inst = new FakeInstance();
    const light = new HoverLight();
    expect(light.set(inst, 2)).toBe(true);
    expect(light.set(inst, 2)).toBe(false);
    inst.lit = HIT_NONE; // a new frame
    light.dropped();
    expect(light.set(inst, 2)).toBe(true);
    expect(inst.lit).toBe(2);
    expect(light.row).toBe(2);
  });
});

describe("hover card target", () => {
  const card = (f: RectFrame, target: Hit, viewId = "v1") => ({ viewId, frame: f, target, avoid: { x: 0, y: 0, w: 1, h: 1 }, pointer: { x: 0, y: 0 }, placement: "beside" as const });

  it("is the same block in a new picture of the view, so the card stays and keeps its details", () => {
    const a = picture(1);
    const b = picture(2);
    expect(a).not.toBe(b);
    expect(sameTarget(card(a, 3), card(b, 3))).toBe(true);
    expect(targetKey(card(a, 3))).toBe(a.key[3]);
  });

  it("is another target for another block or another view", () => {
    const a = picture(1);
    expect(sameTarget(card(a, 3), card(a, 1))).toBe(false);
    expect(sameTarget(card(a, 3), card(a, 3, "v2"))).toBe(false);
    expect(sameTarget(null, card(a, 3))).toBe(false);
  });
});
