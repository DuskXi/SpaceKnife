// The contract between the view host and a visualization component (docs/visuals.md).
//
// A component draws one wire layout (drawlist.v1, flame.v1, polar.v1): its pure geometry answers
// hit tests, outlines and anchors for the host and the shell; its instances own the GL buffers and
// labels of one view. The host and the shell only see VisualFrame, the columns every layout shares;
// the geometry columns are the component's business. All coordinates are physical pixels of the
// canvas unless a name says CSS.

import type { FrameInfo } from "@/protocol/generated";
import type { Tiers } from "@/lib/diff";
import type { PalettePaint } from "@/plugins/colors";
import type { ColorMode } from "@/treemap/paint";

/** Version of this interface; a component built for another one is refused (visual/registry.ts). */
export const VISUAL_API = 1;

/** No row: background, a gap, or outside the drawing. */
export const HIT_NONE = -1;
/** The zoom root itself: the flame graph's root bar, the polar flame graph's centre disc. */
export const HIT_ROOT = -2;
/** A row of the frame, HIT_ROOT or HIT_NONE. */
export type Hit = number;

export interface Pt {
  x: number;
  y: number;
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** The columns every layout shares (docs/protocol.md §4.1), decoded by visual/frame.ts. */
export interface VisualFrame {
  info: FrameInfo;
  /** wire layout of the binary frame, e.g. "flame.v1" */
  layout: string;
  count: number;
  key: Uint32Array;
  node: Int32Array;
  parent: Int32Array;
  kind: Uint8Array;
  depth: Uint8Array;
  cls: Uint8Array;
  label: Uint8Array;
  size: BigInt64Array;
  count_: Uint32Array;
  flags: Uint16Array;
  tag: Uint8Array;
  tone: Uint8Array;
  /** top-level branch slot (0 neutral, 1–8); null from a server before visuals.v1 */
  branch: Uint8Array | null;
  /** compare views (W6): signed net change and status per row (0 same, 1 new, 2 deleted, 3 changed) */
  delta: BigInt64Array | null;
  dstat: Uint8Array | null;
  /** colors.v1: palette index per row of a plugin's colour mode (FrameInfo.palette); null otherwise */
  pal: Uint8Array | null;
  names: string[];
  /** the layout's geometry columns by name (also set on the frame itself, see RectFrame / PolarFrame) */
  geometry: Record<string, Int32Array>;
  /** row of a key, -1 when absent; built lazily */
  rowOf(key: number): number;
}

/** drawlist.v1 and flame.v1: integer rectangles. */
export interface RectFrame extends VisualFrame {
  x: Int32Array;
  y: Int32Array;
  w: Int32Array;
  h: Int32Array;
}

/** polar.v1: sectors, angles in PolarGeometry.unit per turn from 12 o'clock clockwise, radii from the centre. */
export interface PolarFrame extends VisualFrame {
  a0: Int32Array;
  a1: Int32Array;
  r0: Int32Array;
  r1: Int32Array;
}

/** Where outlines go: a CanvasRenderingContext2D or a Path2D (or a recorder in tests). */
export interface PathSink {
  moveTo(x: number, y: number): void;
  lineTo(x: number, y: number): void;
  arc(x: number, y: number, radius: number, startAngle: number, endAngle: number, counterclockwise?: boolean): void;
  closePath(): void;
  rect(x: number, y: number, w: number, h: number): void;
}

/** Spot for something placed on a row: its label, its tag marker, the compare badge / change badge. */
export type AnchorSlot = "label" | "tag" | "badge";

/** Pure geometry of a layout: no GL, no DOM, testable under node (visual/testing/conformance.ts). */
export interface VisualGeometry<F extends VisualFrame = VisualFrame> {
  /** the row under a point, HIT_ROOT on the zoom root's bar or disc, HIT_NONE elsewhere */
  hitTest(f: F, px: number, py: number): Hit;
  /** bounding box of a row or the root; null for HIT_NONE and for what this layout does not draw */
  bounds(f: F, hit: Hit): Box | null;
  /** a point for something placed on the row; null when it does not fit there. "label": the
   * label's centre; "tag": where the component's own paint puts its tag marker; "badge": the centre
   * of a badge (the host centres the static change badge on it, kept inside `bounds`) */
  anchor(f: F, hit: Hit, slot: AnchorSlot): Pt | null;
  /** room on a row: its long and short extent (breathing picks, minimum sizes, ordering) */
  box(f: F, hit: Hit): { major: number; minor: number };
  /** the row's outline moved `inset` px inward; false when there is nothing to outline */
  path(f: F, hit: Hit, inset: number, out: PathSink): boolean;
  /** whether a row is painted with a fill of its own (the compare colour scale counts only those) */
  filled(f: F, row: number): boolean;
}

/** What paint needs besides the frame; the host reads it from the settings once. */
export interface PaintSettings {
  colorMode: ColorMode;
  /** a plugin's colour mode: its palette (the frame's pal column indexes it); null for the built-in ones */
  palette?: PalettePaint | null;
  classIds: string[];
  /** hex colour per class index (W7 文件类别: user colours); the theme's by id when missing */
  classColors: string[];
  /** the scan is still running: unknown space says so */
  scanning: boolean;
  formatSize(bytes: number): string;
  formatInt(n: number): string;
  formatDelta(bytes: number): string;
  /** ancestors lit around the hovered / selected row (W7 外观 · 高亮光晕层数); Infinity = all */
  haloLevels: number;
  /** white inset on the hovered row (W7 外观 · 悬停高亮) */
  hoverHighlight: boolean;
  /** drop shadow under the selection (W7 外观 · 选中投影) */
  selectionShadow: boolean;
  /** 「元素字号」, 1 = 100 % */
  fontScale: number;
  /** 「层级对比」, 1 = the design */
  levelContrast: number;
  /** 「边框对比」, 1 = the design */
  borderContrast: number;
  /** reduced motion: no zoom or swap animation */
  reduced: boolean;
  /** compare views (W6): colour by change tier */
  diffTiers: Tiers | null;
}

/** One draw of an instance. */
export interface DrawPass {
  /** performance.now() of this display frame */
  now: number;
  /** opacity of the whole picture (cross-fades) */
  alpha: number;
  /** zoom animation: `row` of this frame grows to fill the canvas, t from 0 (as laid out) to 1 */
  zoom: { row: number; t: number } | null;
}

/** What the host lends a component instance: one WebGL2 context per host, shared by its instances. */
export interface VisualContext {
  canvas: HTMLCanvasElement;
  gl: WebGL2RenderingContext;
  /** the label layer over the GL canvas */
  text: CanvasRenderingContext2D;
  /** one object per key and context (programs, renderers); made again after a lost context */
  shared<T>(key: string, make: () => T): T;
}

/** The pictures of one view in one layout. */
export interface VisualInstance<F extends VisualFrame = VisualFrame> {
  /** paint a new frame (pure paint, then upload); buffers are reused */
  setFrame(f: F, s: PaintSettings): void;
  /** the hovered row changed */
  setHover(hit: Hit): void;
  /** draw into the GL canvas and the label layer; true while it needs another display frame */
  draw(p: DrawPass): boolean;
  /** free GL objects; the instance is not used again */
  dispose(): void;
  /** the context came back: make GL objects again (the host then calls setFrame with the last frame) */
  contextRestored(): void;
}

/** Hover card placement: beside the row's box, or next to the pointer (a large sector's box can be the whole disc). */
export type HoverPlacement = "beside" | "pointer";

/** A visualization's renderer side, registered per wire layout (visual/registry.ts). */
export interface VisualComponent<F extends VisualFrame = VisualFrame> {
  readonly layout: string;
  readonly api: typeof VISUAL_API;
  /** hello capability that announces this layout; null for the baseline drawlist.v1 */
  readonly capability: string | null;
  readonly hoverPlacement: HoverPlacement;
  readonly geometry: VisualGeometry<F>;
  create(ctx: VisualContext): VisualInstance<F>;
}

/** What the hover card is told about the row under the pointer. */
export interface HoverInfo {
  viewId: string;
  frame: VisualFrame;
  target: Hit;
  /** the row's bounds in CSS px relative to the host element: the card keeps clear of it */
  avoid: Box;
  /** the pointer in CSS px relative to the host element */
  pointer: Pt;
  placement: HoverPlacement;
}
