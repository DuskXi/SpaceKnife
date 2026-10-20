// The polar flame graph's instance against a recording renderer (no GL under node): what a frame, a
// hover, a draw, a zoom, a lost context and dispose each upload and draw.

import { describe, expect, it } from "vitest";

import type { PolarGeometry } from "@/protocol/generated";

import { polarScene } from "../testing/fixtures";
import { HIT_NONE, HIT_ROOT, type PaintSettings, type VisualContext } from "../types";
import { flamePolarComponent } from "./component";
import { IDENTITY, zoomTransform } from "./geometry";
import type { SectorPicture, SectorRenderer } from "./sectors";

const S: PaintSettings = {
  colorMode: "branch",
  classIds: ["other", "video", "doc", "image"],
  classColors: [],
  scanning: false,
  formatSize: (b) => `${b} B`,
  formatInt: (n) => String(n),
  formatDelta: (b) => `${b} B`,
  haloLevels: 3,
  hoverHighlight: true,
  selectionShadow: true,
  fontScale: 1,
  levelContrast: 1,
  borderContrast: 1,
  reduced: false,
  diffTiers: null,
};

function harness() {
  const log: string[] = [];
  let next = 0;
  const renderer = {
    upload(_data: Float32Array, count: number, previous?: SectorPicture | null): SectorPicture {
      const p = previous ?? ({ vao: {}, buffer: {}, count: 0, capacity: 0, id: next++ } as unknown as SectorPicture);
      log.push(`upload ${(p as unknown as { id: number }).id} ${count}${previous ? " reuse" : ""}`);
      p.count = count;
      return p;
    },
    use: (w: number, h: number, cx: number, cy: number) => log.push(`use ${w} ${h} ${cx} ${cy}`),
    draw: (p: SectorPicture, xf: readonly number[], alpha: number) => log.push(`draw ${(p as unknown as { id: number }).id} ${xf.map((v) => +v.toFixed(4)).join(",")} ${alpha}`),
    dispose: (p: SectorPicture | null) => log.push(`dispose ${p ? (p as unknown as { id: number }).id : "null"}`),
  } as unknown as SectorRenderer;
  let shared = 0;
  const text = {
    font: "",
    fillStyle: "",
    globalAlpha: 1,
    textAlign: "start",
    textBaseline: "alphabetic",
    measureText: (s: string) => ({ width: 6 * s.length }),
    save() {},
    restore() {},
    translate() {},
    rotate() {},
    beginPath() {},
    roundRect() {},
    moveTo() {},
    lineTo() {},
    arc() {},
    closePath() {},
    fill() {},
    stroke() {},
    fillText: (s: string) => log.push(`text ${s}`),
  } as unknown as CanvasRenderingContext2D;
  const ctx: VisualContext = {
    canvas: {} as HTMLCanvasElement,
    gl: {} as WebGL2RenderingContext,
    text,
    shared: <T,>(key: string) => {
      shared++;
      log.push(`shared ${key}`);
      return renderer as unknown as T;
    },
  };
  return { log, ctx, sharedCalls: () => shared };
}

describe("flamePolarComponent instances", () => {
  const { frame: f } = polarScene();
  const g = f.info.geometry as PolarGeometry;

  it("uploads the picture and the overlay for a frame, only the overlay for a hover", () => {
    const h = harness();
    const inst = flamePolarComponent.create(h.ctx);
    expect(h.log).toEqual(["shared sector"]);
    h.log.length = 0;
    inst.setFrame(f, S);
    expect(h.log).toEqual([`upload 0 ${f.count + 1}`, expect.stringMatching(/^upload 1 \d+$/)]);
    h.log.length = 0;
    inst.setHover(f.rowOf(6));
    expect(h.log).toEqual([expect.stringMatching(/^upload 1 \d+ reuse$/)]);
    h.log.length = 0;
    inst.setHover(f.rowOf(6));
    inst.setHover(f.rowOf(6));
    expect(h.log).toEqual([]);
  });

  it("draws the picture, the overlay and the labels, the hovered row named in the centre", () => {
    const h = harness();
    const inst = flamePolarComponent.create(h.ctx);
    inst.setFrame(f, S);
    inst.setHover(f.rowOf(2));
    h.log.length = 0;
    expect(inst.draw({ now: 0, alpha: 1, zoom: null })).toBe(false);
    expect(h.log.slice(0, 3)).toEqual([`use ${f.info.width} ${f.info.height} ${g.cx} ${g.cy}`, `draw 0 ${IDENTITY.join(",")} 1`, `draw 1 ${IDENTITY.join(",")} 1`]);
    const texts = h.log.filter((l) => l.startsWith("text "));
    expect(texts).toContain("text a.mp4");
    expect(texts.slice(-3)).toEqual(["text a.mp4", "text 400 B", "text 占 40%"]);
    h.log.length = 0;
    inst.setHover(HIT_ROOT);
    inst.draw({ now: 0, alpha: 1, zoom: null });
    expect(h.log.filter((l) => l.startsWith("text ")).slice(-2)).toEqual(["text root", "text 1000 B"]);
  });

  it("zooms by the polar transform and lets the labels go in the first 30 %", () => {
    const h = harness();
    const inst = flamePolarComponent.create(h.ctx);
    inst.setFrame(f, S);
    const sub = f.rowOf(3);
    h.log.length = 0;
    inst.draw({ now: 0, alpha: 0.6, zoom: { row: sub, t: 0.5 } });
    const xf = zoomTransform(f, sub, 0.5).map((v) => +v.toFixed(4)).join(",");
    expect(h.log).toEqual([expect.stringMatching(/^use /), `draw 0 ${xf} 0.6`, `draw 1 ${xf} 0.6`]);
    h.log.length = 0;
    inst.draw({ now: 0, alpha: 1, zoom: { row: sub, t: 0.1 } });
    expect(h.log.some((l) => l.startsWith("text "))).toBe(true);
  });

  it("forgets the hover with a new frame, keeps it with new settings", () => {
    const h = harness();
    const inst = flamePolarComponent.create(h.ctx);
    inst.setFrame(f, S);
    inst.setHover(f.rowOf(2));
    inst.setFrame(f, { ...S, colorMode: "class" });
    h.log.length = 0;
    inst.draw({ now: 0, alpha: 1, zoom: null });
    expect(h.log.filter((l) => l.startsWith("text ")).slice(-1)).toEqual(["text 占 40%"]);
    const again = polarScene().frame;
    inst.setFrame(again, S);
    h.log.length = 0;
    inst.draw({ now: 0, alpha: 1, zoom: null });
    expect(h.log.filter((l) => l.startsWith("text ")).slice(-2)).toEqual(["text root", "text 1000 B"]);
    h.log.length = 0;
    inst.setHover(HIT_NONE);
    expect(h.log).toEqual([]);
  });

  it("makes its buffers again after a lost context and frees them on dispose", () => {
    const h = harness();
    const inst = flamePolarComponent.create(h.ctx);
    inst.setFrame(f, S);
    inst.contextRestored();
    expect(h.sharedCalls()).toBe(2);
    h.log.length = 0;
    expect(inst.draw({ now: 0, alpha: 1, zoom: null })).toBe(false);
    expect(h.log).toEqual([]); // nothing to draw until the host paints the frame again
    inst.setFrame(f, S);
    expect(h.log.filter((l) => l.startsWith("upload")).every((l) => !l.endsWith("reuse"))).toBe(true);
    h.log.length = 0;
    inst.dispose();
    expect(h.log).toEqual(["dispose 2", "dispose 3"]);
    h.log.length = 0;
    inst.dispose();
    expect(h.log).toEqual(["dispose null", "dispose null"]);
  });
});
