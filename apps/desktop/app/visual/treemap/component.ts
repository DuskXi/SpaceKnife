// The treemap as a visualization component (drawlist.v1): treemap/paint.ts turns a frame into GPU
// instances and labels, treemap/gl.ts draws them; a zoom grows one block to the whole canvas.

import { GlRenderer, type Picture } from "@/treemap/gl";
import { paint, type TagItem } from "@/treemap/paint";
import { drawTags, drawTexts, type PreparedText, prepareTexts } from "@/treemap/text";

import { type DrawPass, HIT_NONE, type Hit, type PaintSettings, type RectFrame, VISUAL_API, type VisualComponent, type VisualContext, type VisualInstance } from "../types";
import { treemapGeometry } from "./geometry";

/** The rectangle renderer of a context, shared with the flame graph. */
const rendererOf = (ctx: VisualContext) => ctx.shared("rect", () => new GlRenderer(ctx.canvas));

/** The affine map that takes a block to the whole canvas, `t` of the way from identity. */
function zoomTransform(f: RectFrame, row: number, t: number): { sx: number; sy: number; tx: number; ty: number } {
  const W = f.info.width;
  const H = f.info.height;
  const w = Math.max(1, f.w[row]!);
  const h = Math.max(1, f.h[row]!);
  const sx = W / w;
  const sy = H / h;
  return { sx: 1 + (sx - 1) * t, sy: 1 + (sy - 1) * t, tx: -f.x[row]! * sx * t, ty: -f.y[row]! * sy * t };
}

class TreemapInstance implements VisualInstance<RectFrame> {
  private renderer: GlRenderer;
  private picture: Picture | null = null;
  private scratch: Float32Array | undefined; // the instance array, refilled by every repaint
  private texts: PreparedText[] = [];
  private tags: TagItem[] = [];
  private frame: RectFrame | null = null;
  private settings: PaintSettings | null = null;
  private hovered: Hit = HIT_NONE;

  constructor(private readonly ctx: VisualContext) {
    this.renderer = rendererOf(ctx);
  }

  setFrame(f: RectFrame, s: PaintSettings): void {
    if (f !== this.frame) this.hovered = HIT_NONE;
    this.frame = f;
    this.settings = s;
    this.repaint();
  }

  /** The whole frame is painted again: the hover inset and the halo headers are part of it. */
  setHover(hit: Hit): void {
    if (hit === this.hovered) return;
    this.hovered = hit;
    this.repaint();
  }

  private repaint(): void {
    const f = this.frame;
    const s = this.settings;
    if (!f || !s) return;
    const painted = paint(f, {
      dpr: f.info.dpr,
      colorMode: s.colorMode,
      classIds: s.classIds,
      classColors: s.classColors,
      hovered: this.hovered >= 0 ? this.hovered : -1,
      scanning: s.scanning,
      formatSize: s.formatSize,
      formatInt: s.formatInt,
      haloLevels: s.haloLevels,
      hoverHighlight: s.hoverHighlight,
      selectionShadow: s.selectionShadow,
      fontScale: s.fontScale,
      levelContrast: s.levelContrast,
      borderContrast: s.borderContrast,
      diff: s.diffTiers && f.delta ? { tiers: s.diffTiers, formatDelta: s.formatDelta } : undefined,
    }, this.scratch);
    this.scratch = painted.instances;
    this.picture = this.renderer.upload(painted.instances, painted.count, this.picture);
    this.texts = prepareTexts(this.ctx.text, painted.texts);
    this.tags = painted.tags;
  }

  draw(p: DrawPass): boolean {
    const f = this.frame;
    if (!f || !this.picture) return false;
    const m = p.zoom ? zoomTransform(f, p.zoom.row, p.zoom.t) : { sx: 1, sy: 1, tx: 0, ty: 0 };
    this.renderer.use(f.info.width, f.info.height);
    this.renderer.draw(this.picture, m.sx, m.sy, m.tx, m.ty, p.alpha);
    // the labels grow with their blocks during a zoom
    const ctx = this.ctx.text;
    ctx.setTransform(m.sx, 0, 0, m.sy, m.tx, m.ty);
    drawTexts(ctx, this.texts, p.alpha);
    drawTags(ctx, this.tags, f.info.dpr, p.alpha);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    return false;
  }

  dispose(): void {
    this.renderer.dispose(this.picture);
    this.picture = null;
    this.frame = null;
    this.scratch = undefined;
  }

  contextRestored(): void {
    // the old buffers went with the lost context; the host paints the last frame again
    this.picture = null;
    this.renderer = rendererOf(this.ctx);
  }
}

export const treemapComponent: VisualComponent<RectFrame> = {
  layout: "drawlist.v1",
  api: VISUAL_API,
  capability: null,
  hoverPlacement: "beside",
  geometry: treemapGeometry,
  create: (ctx) => new TreemapInstance(ctx),
};
