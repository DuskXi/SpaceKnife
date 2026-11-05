// The flame graph's renderer side (flame.v1): the frame's picture and the pointer's overlay as two
// instance buffers of treemap/gl.ts's rounded-rectangle program, plus the label layer. The host
// lends the GL context and drives zoom and cross-fade timing through draw().

import { GlRenderer, type Picture } from "@/treemap/gl";
import type { TagItem } from "@/treemap/paint";
import { drawTags, drawTexts, type PreparedText, prepareTexts } from "@/treemap/text";

import { type DrawPass, HIT_NONE, type Hit, type PaintSettings, type RectFrame, VISUAL_API, type VisualComponent, type VisualContext, type VisualInstance } from "../types";
import { flameGeometry } from "./geometry";
import { Instances, moveTags, moveTexts, paintFlame, paintFlameOverlay, zoomXform } from "./paint";

/** The context's rectangle renderer, shared with the treemap's instances. */
const rects = (ctx: VisualContext): GlRenderer => ctx.shared("rect", () => new GlRenderer(ctx.canvas));

class FlameInstance implements VisualInstance<RectFrame> {
  private ctx: VisualContext;
  private gl: GlRenderer;
  private main: Picture | null = null;
  private over: Picture | null = null;
  // CPU-side instances: kept between paints and only ever grown, a hover never reallocates
  private mainBuf = new Instances();
  private overBuf = new Instances(16);
  private frame: RectFrame | null = null;
  private settings: PaintSettings | null = null;
  private hover: Hit = HIT_NONE;
  private texts: PreparedText[] = [];
  private tags: TagItem[] = [];

  constructor(ctx: VisualContext) {
    this.ctx = ctx;
    this.gl = rects(ctx);
  }

  setFrame(f: RectFrame, s: PaintSettings): void {
    // a new frame numbers its rows anew: the host hovers again; the same frame (new settings) keeps it
    if (this.frame?.info.seq !== f.info.seq) this.hover = HIT_NONE;
    this.frame = f;
    this.settings = s;
    const p = paintFlame(f, s, this.mainBuf);
    this.main = this.gl.upload(p.instances, p.count, this.main);
    // truncated once per frame (UI-TM-17); draws and animation frames reuse it
    this.texts = prepareTexts(this.ctx.text, p.texts);
    this.tags = p.tags;
    this.paintOverlay();
  }

  setHover(hit: Hit): void {
    if (hit === this.hover) return;
    this.hover = hit;
    this.paintOverlay();
  }

  private paintOverlay(): void {
    if (!this.frame || !this.settings) return;
    const p = paintFlameOverlay(this.frame, this.settings, this.hover, this.overBuf);
    this.over = this.gl.upload(p.instances, p.count, this.over);
  }

  draw(p: DrawPass): boolean {
    const f = this.frame;
    if (!f || !this.settings || !this.main || p.alpha <= 0) return false;
    // zoom: the bars stretch along x and slide to the root band; labels only move with their anchors
    const m = zoomXform(f, p.zoom, this.settings.reduced);
    this.gl.use(f.info.width, f.info.height);
    const [sx, sy, tx, ty] = m ? [m.sx, m.sy, m.tx, m.ty] : [1, 1, 0, 0];
    this.gl.draw(this.main, sx, sy, tx, ty, p.alpha);
    if (this.over) this.gl.draw(this.over, sx, sy, tx, ty, p.alpha);
    drawTexts(this.ctx.text, m ? moveTexts(this.texts, m) : this.texts, p.alpha);
    drawTags(this.ctx.text, m ? moveTags(this.tags, m) : this.tags, f.info.dpr, p.alpha);
    return false;
  }

  dispose(): void {
    this.gl.dispose(this.main);
    this.gl.dispose(this.over);
    this.main = null;
    this.over = null;
    this.frame = null;
    this.settings = null;
  }

  contextRestored(): void {
    // the old context's objects are gone: upload the kept instances again into the new renderer
    this.gl = rects(this.ctx);
    this.main = null;
    this.over = null;
    if (!this.frame) return;
    this.main = this.gl.upload(this.mainBuf.data, this.mainBuf.count);
    this.over = this.gl.upload(this.overBuf.data, this.overBuf.count);
  }
}

export const flameComponent: VisualComponent<RectFrame> = {
  layout: "flame.v1",
  api: VISUAL_API,
  capability: "layout.flame.v1",
  hoverPlacement: "beside",
  geometry: flameGeometry,
  create: (ctx) => new FlameInstance(ctx),
};
