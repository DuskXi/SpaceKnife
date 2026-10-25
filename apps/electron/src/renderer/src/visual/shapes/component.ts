// A plugin's own layout (shapes.v1): the frame's picture and the pointer's overlay as two instance
// buffers of gl.ts's shape program, plus the label layer. The host lends the GL context and drives
// zoom and cross-fade timing through draw(). A frame the plugin could not lay out says why.

import { palette } from "@/lib/theme";
import type { TagItem } from "@/treemap/paint";
import { drawTags, drawTexts, type PreparedText, prepareTexts } from "@/treemap/text";

import { type DrawPass, HIT_NONE, type Hit, type PaintSettings, type ShapesFrame, VISUAL_API, type VisualComponent, type VisualContext, type VisualInstance } from "../types";
import { shapesGeometry, shapesOf } from "./geometry";
import { ShapeRenderer, type ShapePicture } from "./gl";
import { moveTags, moveTexts, paintShapes, paintShapesOverlay, ShapeInstances, zoomXform } from "./paint";

/** The context's shape renderer. */
const renderer = (ctx: VisualContext): ShapeRenderer => ctx.shared("shapes", () => new ShapeRenderer(ctx.canvas));

const FONT_UI = '"Geist", "Geist Variable", "Noto Sans SC", "Noto Sans SC Variable", system-ui, sans-serif';

class ShapesInstance implements VisualInstance<ShapesFrame> {
  private ctx: VisualContext;
  private gl: ShapeRenderer;
  private main: ShapePicture | null = null;
  private over: ShapePicture | null = null;
  private mainBuf = new ShapeInstances();
  private overBuf = new ShapeInstances(16);
  private frame: ShapesFrame | null = null;
  private settings: PaintSettings | null = null;
  private hover: Hit = HIT_NONE;
  private texts: PreparedText[] = [];
  private tags: TagItem[] = [];

  constructor(ctx: VisualContext) {
    this.ctx = ctx;
    this.gl = renderer(ctx);
  }

  setFrame(f: ShapesFrame, s: PaintSettings): void {
    if (this.frame?.info.seq !== f.info.seq) this.hover = HIT_NONE;
    this.frame = f;
    this.settings = s;
    const p = paintShapes(f, s, this.mainBuf);
    this.main = this.gl.upload(p.instances, p.count, this.main);
    const message = shapesOf(f)?.message;
    if (message && !f.count) {
      // nothing drawn: why, in the middle of the canvas
      const dpr = f.info.dpr;
      p.texts.push({
        x: f.info.width / 2,
        y: f.info.height / 2,
        text: message,
        font: `500 ${12 * s.fontScale * dpr}px ${FONT_UI}`,
        color: palette().color.text.secondary,
        maxWidth: f.info.width - 32 * dpr,
        alpha: 1,
        align: "center",
      });
    }
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
    const p = paintShapesOverlay(this.frame, this.settings, this.hover, this.overBuf);
    this.over = this.gl.upload(p.instances, p.count, this.over);
  }

  draw(p: DrawPass): boolean {
    const f = this.frame;
    if (!f || !this.settings || !this.main || p.alpha <= 0) return false;
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
    this.gl = renderer(this.ctx);
    this.main = null;
    this.over = null;
    if (!this.frame) return;
    this.main = this.gl.upload(this.mainBuf.data, this.mainBuf.count);
    this.over = this.gl.upload(this.overBuf.data, this.overBuf.count);
  }
}

export const shapesComponent: VisualComponent<ShapesFrame> = {
  layout: "shapes.v1",
  api: VISUAL_API,
  capability: "layout.shapes.v1",
  hoverPlacement: "beside",
  geometry: shapesGeometry,
  create: (ctx) => new ShapesInstance(ctx),
};
