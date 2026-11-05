<!-- The view host of one view (visual/Host.tsx): GL shapes, labels and the breathing outline on three
     canvases, one WebGL2 context, the pointer, the viewport, the draw loops and what is drawn over
     any visualization (overlays.ts). The frame's wire layout picks the component (registry.ts): a
     zoom animates inside a component, a switch of visualization cross-fades two of them.

     Nothing the draw loops touch is reactive: the frames, the component instances and the overlay
     marks live in one plain object (`st`); the store's reads only trigger the watchers that hand them
     over. -->
<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import { tokens } from "@design/tokens";

import { useDevicePixelRatio, useReducedMotion } from "@/composables/useMedia";
import { classColors } from "@/lib/classes";
import { diffTiers, maxChange } from "@/lib/diff";
import { fmtDelta, fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { palette } from "@/lib/theme";
import { setViewSettings, viewCall } from "@/state/actions";
import { setting, useApp } from "@/state/store";
import { type ColorMode, rgba } from "@/treemap/paint";
import { acquireGl, type GlContext } from "@/visual/glContext";
import { pointerBox, scaled } from "@/visual/hit";
import { type HostHandle, hosts } from "@/visual/hosts";
import { HoverLight, hoverTarget } from "@/visual/hover";
import {
  type Breath,
  breathingRows,
  classFade,
  type ClassMark,
  classMarks,
  drawBreath,
  drawClassMark,
  drawFlash,
  drawTrail,
  drawTrash,
  type Flash,
  flashOf,
  type TrailMark,
  trailMark,
  type TrashMark,
  trashMarks,
  type TrashMode,
  trashMoves,
} from "@/visual/overlays";
import { componentFor } from "@/visual/registry";
import { breathGap, FPS_GAP, FrameLoop, frameGap, type Pace, type TransitionKind, transitionFor } from "@/visual/scheduler";
import { HIT_NONE, HIT_ROOT, type Hit, type HoverInfo, type PaintSettings, type VisualComponent, type VisualContext, type VisualFrame, type VisualGeometry, type VisualInstance } from "@/visual/types";

const props = defineProps<{ viewId: string }>();
const emit = defineEmits<{
  hover: [hover: HoverInfo | null];
  /** a row (or HIT_ROOT) was right-clicked; it is selected first */
  menu: [row: Hit, frame: VisualFrame, ev: MouseEvent];
}>();

/** One component instance and the frame it shows. */
interface Slot {
  component: VisualComponent;
  instance: VisualInstance;
  frame: VisualFrame;
}

interface Transition {
  kind: TransitionKind;
  /** "in": the row of the old frame that grows; "out": the row of the new frame it shrinks from */
  row: number;
  start: number;
  duration: number;
}

interface HostState {
  glc: GlContext | null;
  ctx: VisualContext | null;
  cur: Slot | null;
  /** the picture going away while a transition runs */
  old: Slot | null;
  anim: Transition | null;
  /** what cur was painted with (repainted after a lost context) */
  settings: PaintSettings | null;
  /** the row lit as hovered: worked out again for every picture by syncHover (visual/hover.ts) */
  light: HoverLight;
  /** where the pointer is over the picture (client px), null once it left */
  pointer: { x: number; y: number } | null;
  lastZoomKey: number | null;
  flash: Flash | null;
  trail: TrailMark[];
  breath: Breath[];
  /** what lies in a recycle bin (「回收站特效」) */
  trash: TrashMark[];
  /** the class lit from 「类别统计」 (UI-MAIN-65): its index (-1: none) and the node whose make-up
   * the panel shows, its rows, how far the veil has come in (0–1, kept while it fades out) and when */
  focusCls: number;
  focusNode: number;
  focus: ClassMark | null;
  focusColor: string;
  focusLevel: number;
  focusAt: number;
}

/** The badge on a recycle bin: what emptying it frees (macOS calls it the Trash). */
const binLabel = (bytes: number) =>
  window.sk.platform === "darwin" ? t("废纸篓 · 清空可释放 {size}", { size: fmtSize(bytes) }) : t("回收站 · 清空可释放 {size}", { size: fmtSize(bytes) });

/** The picture's ground in the theme on screen: what the GL canvas is cleared to. */
function ground(): readonly [number, number, number] {
  const [r, g, b] = rgba(palette().treemap.ground);
  return [r, g, b];
}
const ZOOM_MS = 250;
const SWAP_MS = tokens.motion.duration.swap;
const easeOut = (x: number) => 1 - (1 - x) ** 3;

function fit(canvas: HTMLCanvasElement, w: number, h: number): void {
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
}

const host = ref<HTMLDivElement | null>(null);
const glCanvas = ref<HTMLCanvasElement | null>(null);
const textCanvas = ref<HTMLCanvasElement | null>(null);
const breathCanvas = ref<HTMLCanvasElement | null>(null);
// plain, never reactive: read and written every display frame
const st: HostState = {
  glc: null,
  ctx: null,
  cur: null,
  old: null,
  anim: null,
  settings: null,
  light: new HoverLight(),
  pointer: null,
  lastZoomKey: null,
  flash: null,
  trail: [],
  breath: [],
  trash: [],
  focusCls: -1,
  focusNode: -1,
  focus: null,
  focusColor: "",
  focusLevel: 0,
  focusAt: 0,
};

const view = useApp((s) => s.views[props.viewId]);
const vocab = useApp((s) => s.vocab);
// 「主题」 (UI-W7-31 ①): the class colours and everything painted follow the theme on screen
const theme = useApp((s) => s.theme);
const classIds = computed(() => vocab.value?.classes.map((c) => c.id) ?? []);
const colors = computed(() => {
  void theme.value;
  return classColors(vocab.value);
});
const scanning = useApp((s) => {
  const v = s.views[props.viewId];
  const sc = v ? s.scans[v.state.scan_id] : undefined;
  return sc?.state === "scanning" || sc?.state === "queued";
});
// W7: 减少动画, 缩放动画时长, 光晕层数, 悬停高亮, 选中投影; unitEpoch redraws size labels
const reduced = useReducedMotion(useApp((s) => setting<string>(s, "appearance.reduce_motion", "system")));
const zoomMs = useApp((s) => setting(s, "treemap.zoom_ms", ZOOM_MS));
const haloLevels = useApp((s) => setting(s, "appearance.halo_levels", 3));
const hoverHighlight = useApp((s) => setting(s, "appearance.hover_highlight", true));
const selectionShadow = useApp((s) => setting(s, "appearance.selection_shadow", true));
const unitEpoch = useApp((s) => s.unitEpoch);
const fontScale = useApp((s) => setting(s, "treemap.font_scale", 100) / 100);
const levelContrast = useApp((s) => setting(s, "appearance.level_contrast", 10) / 10);
const borderContrast = useApp((s) => setting(s, "appearance.border_contrast", 10) / 10);
const trailOn = useApp((s) => setting(s, "appearance.mouse_trail", false));
const trashMode = useApp((s) => setting<string>(s, "appearance.trash_effect", "animated") as TrashMode);
const fpsGap = useApp((s) => FPS_GAP[setting<string>(s, "treemap.fps", "display")] ?? 0);
const dynaSmooth = useApp((s) => setting(s, "treemap.dynasmooth", true));
const dpr = useDevicePixelRatio();

/** What the draw loops read from the latest settings (computed reads: cached, cheap per frame). */
const live = () => ({ pace: { fpsGap: fpsGap.value, dynaSmooth: dynaSmooth.value } as Pace, reduced: reduced.value, trashMode: trashMode.value });

/** The lit class's rows in `frame` (UI-MAIN-65). Without a class the last rows stay while the
 * veil fades out (`fading`), and go with a new frame. */
function relight<F extends VisualFrame>(g: VisualGeometry<F>, frame: F, fading = false): void {
  if (st.focusCls >= 0) st.focus = classMarks(g, frame, st.focusCls, frame.rowOf(st.focusNode), frame.info.dpr);
  else if (!fading) st.focus = null;
}

// ---------------------------------------------------------------- draw loops

// the shapes and labels, then the trail and the change flashes over them
function drawMain(now: number): boolean {
  const { glc, cur } = st;
  const canvas = glCanvas.value;
  const tc = textCanvas.value;
  if (!glc || !cur || !canvas || !tc || glc.lost) return false;
  const W = cur.frame.info.width;
  const H = cur.frame.info.height;
  fit(canvas, W, H);
  fit(tc, W, H);
  const ctx = tc.getContext("2d")!;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, W, H);
  glc.begin(W, H, ground());
  const a = st.anim;
  if (a && st.old) {
    const k = Math.min(1, (now - a.start) / a.duration);
    const e = easeOut(k);
    if (a.kind === "swap") {
      st.old.instance.draw({ now, alpha: 1 - k, zoom: null });
      cur.instance.draw({ now, alpha: k, zoom: null });
    } else if (a.kind === "in") {
      // the old picture zooms into the row, the new one fades in at the end
      const fade = Math.max(0, (k - 0.55) / 0.45);
      st.old.instance.draw({ now, alpha: 1 - fade, zoom: { row: a.row, t: e } });
      cur.instance.draw({ now, alpha: fade, zoom: null });
    } else {
      // the new picture shrinks from the row that was the zoom root
      const appear = Math.min(1, k / 0.35);
      st.old.instance.draw({ now, alpha: 1 - appear, zoom: null });
      cur.instance.draw({ now, alpha: appear, zoom: { row: a.row, t: 1 - e } });
    }
    if (k < 1) return true;
    st.anim = null;
    st.old.instance.dispose();
    st.old = null;
    queueMicrotask(() => syncHover("picture")); // the block under the pointer, in the new picture
    return drawMain(now);
  }
  const dprNow = cur.frame.info.dpr;
  let more = cur.instance.draw({ now, alpha: 1, zoom: null });
  if (st.trail.length) {
    st.trail = drawTrail(ctx, st.trail, now, dprNow);
    more ||= st.trail.length > 0;
  }
  if (st.flash) {
    if (drawFlash(ctx, st.flash, now, reduced.value, dprNow)) more = true;
    else st.flash = null;
  }
  return more;
}
// the veil of the class lit from 「类别统计」 coming in or going
const focusFading = () => st.focus !== null && st.focusLevel !== (st.focusCls >= 0 ? 1 : 0);
// on a canvas of their own: the class lit from 「类别统计」, what lies in a recycle bin, and the
// folders still being scanned breathing over it (UI-TM-42)
function drawBreathing(now: number): boolean {
  const bc = breathCanvas.value;
  const cur = st.cur;
  if (!bc || !cur) return false;
  const { reduced: still, trashMode: mode } = live();
  if (st.focus) {
    st.focusLevel = classFade(st.focusLevel, st.focusCls >= 0, now - st.focusAt, still);
    st.focusAt = now;
    if (st.focusLevel === 0 && st.focusCls < 0) st.focus = null;
  }
  if (!st.breath.length && !st.trash.length && !st.focus) {
    fit(bc, 0, 0); // nothing over the view: no full-size layer kept (8–12 B a pixel)
    return false;
  }
  const W = cur.frame.info.width;
  const H = cur.frame.info.height;
  fit(bc, W, H);
  const ctx = bc.getContext("2d")!;
  ctx.clearRect(0, 0, W, H);
  const moving = trashMoves(st.trash, mode, still);
  if (!st.anim) {
    if (st.focus) drawClassMark(ctx, st.focus, st.focusColor, st.focusLevel, W, H, cur.frame.info.dpr);
    drawTrash(ctx, st.trash, now, moving, cur.frame.info.dpr, binLabel);
    drawBreath(ctx, st.breath, now, still, cur.frame.info.dpr);
  }
  return (st.breath.length > 0 && !still) || moving || focusFading() || st.anim !== null; // after a transition: draw again
}
// the bin's light and dashes move at 30 frames a second at most
function ambientGap(): number {
  const { pace, reduced: still, trashMode: mode } = live();
  if (st.breath.length) return breathGap(pace, true, still);
  if (focusFading()) return pace.fpsGap;
  return trashMoves(st.trash, mode, still) ? Math.max(pace.fpsGap, FPS_GAP["30"]!) : 0;
}
const loops = {
  main: new FrameLoop(drawMain, () => frameGap(live().pace, { user: st.anim !== null || st.trail.length > 0, ambient: st.flash !== null })),
  breath: new FrameLoop(drawBreathing, ambientGap),
};

// ---------------------------------------------------------------- the context

let releaseContext: (() => void) | null = null;

function acquire(): boolean {
  const canvas = glCanvas.value;
  const text = textCanvas.value?.getContext("2d");
  if (!canvas || !text) return false;
  let glc: GlContext;
  try {
    glc = acquireGl(canvas);
  } catch (err) {
    console.error(err);
    return false;
  }
  st.glc = glc;
  st.ctx = { canvas, gl: glc.gl, text, shared: (key, make) => glc.shared(key, make) };
  const lost = (ev: Event) => {
    ev.preventDefault(); // ask for it back
    loops.main.stop();
  };
  // the GL objects went with the context: the instances make theirs again, the last frame is painted again
  const restored = () => {
    glc.restored();
    st.old?.instance.contextRestored();
    st.old?.instance.dispose();
    st.old = null;
    st.anim = null;
    if (st.cur) {
      st.cur.instance.contextRestored();
      if (st.settings) st.cur.instance.setFrame(st.cur.frame, st.settings);
    }
    loops.main.run();
    syncHover("picture"); // a transition cut short never reaches its end
  };
  canvas.addEventListener("webglcontextlost", lost);
  canvas.addEventListener("webglcontextrestored", restored);
  releaseContext = () => {
    canvas.removeEventListener("webglcontextlost", lost);
    canvas.removeEventListener("webglcontextrestored", restored);
    loops.main.stop();
    loops.breath.stop();
    st.old?.instance.dispose();
    st.cur?.instance.dispose();
    st.old = null;
    st.cur = null;
    st.anim = null;
    st.ctx = null;
    st.glc = null;
    glc.release();
    // the 2D layers' backings go now, not at the next major GC (a remount sizes them again)
    for (const c of [textCanvas.value, breathCanvas.value]) if (c) fit(c, 0, 0);
  };
  return true;
}

// a hidden window (minimized, or covered: Chromium reports both) keeps no canvas memory: the GL
// context is given up (WebViews never sleep WebGL drawing buffers) and the 2D layers shrink to
// nothing; when it shows again, the context comes back and the last frame is painted again
let lose: WEBGL_lose_context | null = null;
function onVisibility(): void {
  const glc = st.glc;
  if (document.hidden) {
    if (!glc || glc.lost) return;
    lose = glc.gl.getExtension("WEBGL_lose_context");
    if (!lose) return;
    loops.main.stop();
    loops.breath.stop();
    lose.loseContext(); // the host's lost handler keeps it restorable (preventDefault)
    for (const c of [textCanvas.value, breathCanvas.value]) if (c) fit(c, 0, 0);
  } else if (lose) {
    const back = lose;
    lose = null;
    back.restoreContext(); // webglcontextrestored: the instances make their objects, the frame is painted
    loops.breath.run();
  }
}

// ---------------------------------------------------------------- frames

const frame = computed(() => view.value?.frame ?? null);
const colorMode = computed(() => (view.value?.state.color_mode ?? "class") as ColorMode);

/** A new frame, or a colour-mode / paint-setting change: hand it to the component. */
function applyFrame(): void {
  const f = frame.value;
  if (!f || !st.ctx) return;
  const component = componentFor(f.layout);
  if (!component) return;
  const g = component.geometry;
  const mode = trashMode.value;
  const still = reduced.value;
  const settings: PaintSettings = {
    colorMode: colorMode.value,
    classIds: classIds.value,
    classColors: colors.value,
    scanning: scanning.value,
    formatSize: (b) => fmtSize(b),
    formatInt: (n) => fmtInt(n),
    formatDelta: (b) => fmtDelta(b),
    haloLevels: haloLevels.value,
    hoverHighlight: hoverHighlight.value,
    selectionShadow: selectionShadow.value,
    fontScale: fontScale.value,
    levelContrast: levelContrast.value,
    borderContrast: borderContrast.value,
    reduced: still,
    // the compare colour scale counts the rows this visualization fills
    diffTiers: f.delta ? diffTiers(maxChange(f.delta, f.kind, (row) => g.filled(f, row))).tiers : null,
  };
  if (st.cur && f === st.cur.frame) {
    // same frame, new colour mode or paint setting: repaint in place; not while the frame of
    // another visualization is on its way (the colour mode already is that one's)
    if (view.value?.state.visual === f.info.visual) {
      st.settings = settings;
      st.cur.instance.setFrame(st.cur.frame, settings);
    }
    if (!st.anim) loops.main.run();
    st.trash = mode === "off" ? [] : trashMarks(g, f, f.info.dpr);
    relight(g, f);
    loops.breath.run(); // 「减少动画」 or 「回收站特效」 may have changed: move again or hold still
    return;
  }
  st.light.dropped(); // the instance gets another frame, or a new instance starts: no row lit
  const zoomKey = view.value?.state.zoom_key ?? null;
  const prevZoom = st.lastZoomKey;
  st.lastZoomKey = zoomKey;
  const prev = st.cur;
  const sameSize = !!prev && prev.frame.info.width === f.info.width && prev.frame.info.height === f.info.height;
  const tr = prev && sameSize ? transitionFor(prev.frame, f, f.info.reason, zoomKey, prevZoom) : null;
  const duration = !tr || still ? 0 : tr.kind === "swap" ? SWAP_MS : zoomMs.value;
  const anim: Transition | null = tr && duration > 0 ? { ...tr, start: performance.now(), duration } : null;
  if (st.old) st.old.instance.dispose();
  st.old = null;
  if (prev && !anim && prev.component === component) {
    // the same visualization without an animation: its instance and buffers go on
    prev.instance.setFrame(f, settings);
    st.cur = { ...prev, frame: f };
  } else {
    const instance = component.create(st.ctx);
    instance.setFrame(f, settings);
    if (anim) st.old = prev;
    else prev?.instance.dispose();
    st.cur = { component, instance, frame: f };
  }
  st.anim = anim;
  st.settings = settings;
  st.flash = flashOf(g, f, f.info.flash ?? [], performance.now());
  loops.main.run();
  st.breath = breathingRows(g, f, f.info.dpr);
  st.trash = mode === "off" ? [] : trashMarks(g, f, f.info.dpr);
  relight(g, f);
  loops.breath.run();
  syncHover("picture");
}
watch(
  [frame, colorMode, unitEpoch, theme, haloLevels, hoverHighlight, selectionShadow, fontScale, levelContrast, borderContrast, colors, scanning, reduced, trashMode],
  applyFrame,
);

// the class the pointer is on in 「类别统计」 (UI-MAIN-65): its rows lit in the picture, everything
// else under a veil; the filter stays as it is
const focusCls = computed(() => {
  const hc = view.value?.hoverClass ?? null;
  return hc == null ? -1 : classIds.value.indexOf(hc.id);
});
const focusNode = computed(() => view.value?.hoverClass?.node ?? -1);
function applyFocus(): void {
  st.focusCls = focusCls.value;
  st.focusNode = focusNode.value;
  st.focusColor = focusCls.value >= 0 ? (colors.value[focusCls.value] ?? "") : st.focusColor;
  const cur = st.cur;
  const component = cur ? componentFor(cur.frame.layout) : null;
  if (cur && component) relight(component.geometry, cur.frame, true);
  st.focusAt = performance.now();
  loops.breath.run();
}
watch([focusCls, focusNode, colors], applyFocus);

// hover driven from outside (W3 result list): that row lit like a pointer hover (new pictures
// light it again where the frame is taken in)
const hoverKey = computed(() => view.value?.hoverKey ?? null);
watch(hoverKey, () => syncHover("key"));

// viewport: tell the backend (≥ 10 updates a second while resizing, UI-TM-05); a new device pixel
// ratio (another monitor) is sent again even when the size stays
watch(
  [host, () => props.viewId, dpr] as const,
  ([el, viewId], _old, onCleanup) => {
    if (!el || typeof ResizeObserver === "undefined") return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let last = 0;
    const send = () => {
      const rect = el.getBoundingClientRect();
      if (rect.width < 10 || rect.height < 10) return;
      last = performance.now();
      void setViewSettings(viewId, { viewport: { width: rect.width, height: rect.height, dpr: window.devicePixelRatio || 1 } });
    };
    const ro = new ResizeObserver(() => {
      if (timer) clearTimeout(timer);
      if (performance.now() - last > 100) send();
      timer = setTimeout(send, 60);
    });
    ro.observe(el);
    onCleanup(() => {
      ro.disconnect();
      if (timer) clearTimeout(timer);
    });
  },
  { flush: "post" },
);

// what the shell asks about a row: its box, where its menu goes (context menu, Shift+F10)
watch(
  [host, () => props.viewId] as const,
  ([el, viewId], _old, onCleanup) => {
    if (!el) return;
    const where = (hit: Hit) => {
      const cur = st.cur;
      const canvas = glCanvas.value;
      const b = cur && canvas ? cur.component.geometry.bounds(cur.frame, hit) : null;
      if (!cur || !canvas || !b) return null;
      const rect = canvas.getBoundingClientRect();
      return { cur, b, rect, k: rect.width / cur.frame.info.width };
    };
    const handle: HostHandle = {
      element: el,
      boundsCss(hit) {
        const w = where(hit);
        return w ? scaled(w.b, w.k) : null;
      },
      menuAnchor(hit) {
        const w = where(hit);
        if (!w) return null;
        const { cur, b, rect, k } = w;
        // beside a block: near its top (a tall folder's menu stays by its title); at a sector:
        // its label point
        const a =
          cur.component.hoverPlacement === "beside"
            ? { x: b.x + b.w / 2, y: b.y + Math.min(b.h / 2, 24 * cur.frame.info.dpr) }
            : (cur.component.geometry.anchor(cur.frame, hit, "label") ?? { x: b.x + b.w / 2, y: b.y + b.h / 2 });
        const at = { x: rect.left + a.x * k, y: rect.top + a.y * k };
        // a sector's box can be the whole disc: such components keep clear of the label point only
        const s = scaled(b, k);
        const avoid = cur.component.hoverPlacement === "pointer" ? pointerBox(at) : { ...s, x: rect.left + s.x, y: rect.top + s.y };
        return { ...at, avoid };
      },
    };
    hosts.set(viewId, handle);
    onCleanup(() => {
      if (hosts.get(viewId) === handle) hosts.delete(viewId);
    });
  },
  { flush: "post" },
);

onMounted(() => {
  if (acquire()) {
    // what the watchers above did not hand over while there was no context yet
    applyFrame(); // lights the hovered row too (syncHover), the one lit from outside included
    applyFocus();
  }
  document.addEventListener("visibilitychange", onVisibility);
});
onBeforeUnmount(() => {
  document.removeEventListener("visibilitychange", onVisibility);
  releaseContext?.();
  releaseContext = null;
});

// ---------------------------------------------------------------- pointer

function toCanvasAt(clientX: number, clientY: number) {
  const cur = st.cur;
  const canvas = glCanvas.value;
  if (!cur || !canvas) return null;
  const rect = canvas.getBoundingClientRect();
  const f = cur.frame;
  const px = (clientX - rect.left) * (f.info.width / rect.width);
  const py = (clientY - rect.top) * (f.info.height / rect.height);
  return { cur, f, rect, hit: cur.component.geometry.hitTest(f, px, py) };
}
type At = NonNullable<ReturnType<typeof toCanvasAt>>;
const toCanvas = (ev: MouseEvent) => toCanvasAt(ev.clientX, ev.clientY);

function hoverInfo(p: At, clientX: number, clientY: number): HoverInfo {
  const pointer = { x: clientX - p.rect.left, y: clientY - p.rect.top };
  const b = p.cur.component.geometry.bounds(p.f, p.hit);
  return {
    viewId: props.viewId,
    frame: p.f,
    target: p.hit,
    avoid: b ? scaled(b, p.rect.width / p.f.info.width) : pointerBox(pointer),
    pointer,
    placement: p.cur.component.hoverPlacement,
  };
}

/** Why the hovered row is worked out again: a new picture (or the end of a transition, a context
 * back), the pointer moved or left, or the row lit from outside changed. */
type HoverWhy = "picture" | "move" | "leave" | "key";

// The hovered row and its card, in one place for every cause (visual/hover.ts): worked out from
// where the pointer is and the row lit from outside, for the picture on screen. So a new picture
// under a still pointer (a file change, a scan's progress) lights the block under it again and
// keeps its card up with this frame's numbers; nothing is lit while a transition runs (its end
// calls this again).
function syncHover(why: HoverWhy): void {
  const cur = st.cur;
  const at = st.pointer;
  const moving = st.anim !== null;
  const p = cur && at && !moving ? toCanvasAt(at.x, at.y) : null;
  const before = st.light.row;
  const hit = hoverTarget({ frame: cur?.frame ?? null, moving, pointer: p ? p.hit : null, key: hoverKey.value });
  if (cur && hit !== before) {
    if (why === "move" && trailOn.value && !reduced.value && before !== HIT_NONE) {
      const mark = trailMark(cur.component.geometry, cur.frame, before, performance.now());
      if (mark) st.trail.push(mark);
    }
    st.light.set(cur.instance, hit);
    loops.main.run();
  }
  // the card is the pointer's: up for its row, with a new picture's numbers, and moving along
  // with a pointer it follows
  if (!p || !at || p.hit === HIT_NONE) {
    if (why !== "move" || hit !== before) emit("hover", null);
  } else if (why !== "move" || hit !== before || p.cur.component.hoverPlacement === "pointer") {
    emit("hover", hoverInfo(p, at.x, at.y));
  }
}

function onMove(ev: MouseEvent): void {
  st.pointer = { x: ev.clientX, y: ev.clientY };
  syncHover("move");
}

function onLeave(): void {
  st.pointer = null;
  syncHover("leave");
}

// the zoom root's bar or disc: a click shows it in the details panel (no selection), a double
// click goes up a level, a right click opens its menu
function onClick(ev: MouseEvent): void {
  const p = toCanvas(ev);
  if (!p) return;
  void viewCall(props.viewId, "view.select", { key: p.hit >= 0 ? p.f.key[p.hit]! : null });
}

function onDouble(ev: MouseEvent): void {
  const p = toCanvas(ev);
  if (!p) return;
  if (p.hit >= 0) void viewCall(props.viewId, "view.zoom", { key: p.f.key[p.hit]! });
  else if (p.hit === HIT_ROOT) void viewCall(props.viewId, "view.up");
}

function onMenu(ev: MouseEvent): void {
  ev.preventDefault();
  const p = toCanvas(ev);
  if (!p || p.hit === HIT_NONE || (p.hit === HIT_ROOT && !p.f.info.root)) return;
  void viewCall(props.viewId, "view.select", { key: p.hit >= 0 ? p.f.key[p.hit]! : null });
  emit("menu", p.hit, p.f, ev);
}

const empty = computed(() => {
  const f = frame.value;
  return !!f && f.count === 0 && !(f.info.root && f.info.root.size > 0);
});
</script>

<template>
  <div ref="host" class="treemap" :data-view-id="viewId" @mousemove="onMove" @mouseleave="onLeave" @click="onClick" @dblclick="onDouble" @contextmenu="onMenu">
    <canvas ref="glCanvas" class="treemap-layer" />
    <canvas ref="textCanvas" class="treemap-layer" />
    <canvas ref="breathCanvas" class="treemap-layer" />
    <div v-if="empty" class="treemap-empty">{{ view?.state.compare ? t("两次之间没有变化") : t("这里没有任何占用空间的内容") }}</div>
  </div>
</template>
