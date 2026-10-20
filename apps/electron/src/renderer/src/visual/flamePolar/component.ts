// The polar flame graph (polar.v1): the flame graph drawn in polar coordinates, the zoom root as the
// centre disc and each level a ring outward. Sectors come from sectors.ts, colours and labels from
// paint.ts and labels.ts, hits and outlines from geometry.ts.

import { drawTags } from "@/treemap/text";
import type { TagItem } from "@/treemap/paint";

import { HIT_NONE, VISUAL_API, type DrawPass, type Hit, type PaintSettings, type PolarFrame, type VisualComponent, type VisualContext, type VisualInstance } from "../types";
import { IDENTITY, polarGeometry, polarOf, zoomTransform } from "./geometry";
import { centreLabel, drawLabels, type PreparedLabel, prepareLabels } from "./labels";
import { paintFrame, paintOverlay } from "./paint";
import { SectorRenderer, type SectorPicture } from "./sectors";

/** Share of a zoom over which the labels fade out; they do not follow the transform. */
const LABEL_FADE = 0.3;

class FlamePolarInstance implements VisualInstance<PolarFrame> {
  private renderer: SectorRenderer;
  private frame: PolarFrame | null = null;
  private settings: PaintSettings | null = null;
  private hovered: Hit = HIT_NONE;
  private selected = -1;
  /** the disc and the rows; the halo, hover outline and selection over them */
  private picture: SectorPicture | null = null;
  private overlay: SectorPicture | null = null;
  private pictureData: Float32Array | null = null;
  private overlayData: Float32Array | null = null;
  private labels: PreparedLabel[] = [];
  private centre: PreparedLabel[] = [];
  private tags: TagItem[] = [];

  constructor(private ctx: VisualContext) {
    this.renderer = this.shared();
  }

  private shared(): SectorRenderer {
    const gl = this.ctx.gl;
    return this.ctx.shared("sector", () => new SectorRenderer(gl));
  }

  setFrame(f: PolarFrame, s: PaintSettings): void {
    // rows of another frame are other rows: the host hovers again after a new frame
    if (f !== this.frame) this.hovered = HIT_NONE;
    this.frame = f;
    this.settings = s;
    const painted = paintFrame(f, s, this.pictureData);
    this.pictureData = painted.instances;
    this.selected = painted.selected;
    this.picture = this.renderer.upload(painted.instances, painted.count, this.picture);
    this.labels = prepareLabels(this.ctx.text, painted.labels);
    this.tags = painted.tags;
    this.paintOverlay();
  }

  setHover(hit: Hit): void {
    if (hit === this.hovered) return;
    this.hovered = hit;
    this.paintOverlay();
  }

  /** Only the overlay and the centre disc's text change with the hover. */
  private paintOverlay(): void {
    const f = this.frame;
    const s = this.settings;
    if (!f || !s) return;
    const o = paintOverlay(f, s, this.hovered, this.selected, this.overlayData);
    this.overlayData = o.instances;
    this.overlay = this.renderer.upload(o.instances, o.count, this.overlay);
    const c = centreLabel(f, s, this.hovered);
    this.centre = c ? prepareLabels(this.ctx.text, [c]) : [];
  }

  draw(p: DrawPass): boolean {
    const f = this.frame;
    const g = f && polarOf(f);
    if (!f || !g || !this.picture) return false;
    // zoom: the row opens to the whole turn and moves into the centre disc's place
    const xf = p.zoom ? zoomTransform(f, p.zoom.row, p.zoom.t) : IDENTITY;
    this.renderer.use(f.info.width, f.info.height, g.cx, g.cy);
    this.renderer.draw(this.picture, xf, p.alpha);
    if (this.overlay) this.renderer.draw(this.overlay, xf, p.alpha);
    const text = p.alpha * (p.zoom ? Math.max(0, 1 - p.zoom.t / LABEL_FADE) : 1);
    if (text > 0) {
      drawLabels(this.ctx.text, this.labels, text);
      drawTags(this.ctx.text, this.tags, f.info.dpr, text);
      drawLabels(this.ctx.text, this.centre, text);
    }
    return false;
  }

  dispose(): void {
    this.renderer.dispose(this.picture);
    this.renderer.dispose(this.overlay);
    this.picture = null;
    this.overlay = null;
  }

  contextRestored(): void {
    // the old context's buffers are gone with it; the host calls setFrame again
    this.renderer = this.shared();
    this.picture = null;
    this.overlay = null;
  }
}

export const flamePolarComponent: VisualComponent<PolarFrame> = {
  layout: "polar.v1",
  api: VISUAL_API,
  capability: "layout.polar.v1",
  hoverPlacement: "pointer",
  geometry: polarGeometry,
  create: (ctx) => new FlamePolarInstance(ctx),
};
