// The treemap of one view: WebGL shapes + Canvas2D labels, hover / select / zoom, zoom animation.

import { useEffect, useMemo, useRef } from "react";

import { tokens } from "@design/tokens";

import { setViewSettings, viewCall } from "@/state/actions";
import { setting, useApp } from "@/state/store";
import { fmtInt, fmtSize } from "@/lib/format";

import { type DrawFrame, hitTest } from "./frame";
import { GlRenderer, type Picture } from "./gl";
import { type ColorMode, paint, rgba } from "./paint";
import { drawTags, drawTexts, type PreparedText, prepareTexts } from "./text";

export interface HoverInfo {
  viewId: string;
  frame: DrawFrame;
  row: number;
  /** block rectangle in CSS px relative to the treemap element */
  rect: { x: number; y: number; w: number; h: number };
}

interface Props {
  viewId: string;
  onHover?: (hover: HoverInfo | null) => void;
  onContextMenu?: (row: number, frame: DrawFrame, ev: React.MouseEvent) => void;
}

interface Layer {
  frame: DrawFrame;
  picture: Picture;
  texts: PreparedText[];
  tags: ReturnType<typeof paint>["tags"];
}

interface Anim {
  start: number;
  duration: number;
  /** old picture zooms from identity into `from` (zoom in) */
  mode: "in" | "out";
  rect: { x: number; y: number; w: number; h: number };
}

const GROUND = rgba(tokens.treemap.ground);
const ZOOM_MS = 250;
const easeOut = (t: number) => 1 - (1 - t) ** 3;

export function Treemap({ viewId, onHover, onContextMenu }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const glCanvas = useRef<HTMLCanvasElement>(null);
  const textCanvas = useRef<HTMLCanvasElement>(null);
  const r = useRef<{
    gl: GlRenderer | null;
    cur: Layer | null;
    old: Layer | null;
    anim: Anim | null;
    hovered: number;
    raf: number;
    lastZoomKey: number | null;
    lastSeq: number;
  }>({ gl: null, cur: null, old: null, anim: null, hovered: -1, raf: 0, lastZoomKey: null, lastSeq: -1 });

  const view = useApp((s) => s.views[viewId]);
  const vocab = useApp((s) => s.vocab);
  const classIds = useMemo(() => vocab?.classes.map((c) => c.id) ?? [], [vocab]);
  const scanning = useApp((s) => {
    const v = s.views[viewId];
    const st = v ? s.scans[v.state.scan_id] : undefined;
    return st?.state === "scanning" || st?.state === "queued";
  });
  // W7: 减少动画, 缩放动画时长, 光晕层数, 悬停高亮, 选中投影; unitEpoch redraws size labels
  const motion = useApp((s) => setting<string>(s, "appearance.reduce_motion", "system"));
  const zoomMs = useApp((s) => setting(s, "treemap.zoom_ms", ZOOM_MS));
  const haloLevels = useApp((s) => setting(s, "appearance.halo_levels", 3));
  const hoverHighlight = useApp((s) => setting(s, "appearance.hover_highlight", true));
  const selectionShadow = useApp((s) => setting(s, "appearance.selection_shadow", true));
  const unitEpoch = useApp((s) => s.unitEpoch);
  const reduced = motion === "on" || (motion === "system" && typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches);

  // ---------------------------------------------------------------- render loop

  const render = () => {
    const st = r.current;
    const canvas = glCanvas.current;
    const tc = textCanvas.current;
    if (!st.gl || !st.cur || !canvas || !tc) return;
    const W = st.cur.frame.info.width;
    const H = st.cur.frame.info.height;
    if (canvas.width !== W || canvas.height !== H) {
      canvas.width = W;
      canvas.height = H;
      tc.width = W;
      tc.height = H;
    }
    const ctx = tc.getContext("2d")!;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, W, H);
    st.gl.begin(W, H, [GROUND[0], GROUND[1], GROUND[2]]);
    const dpr = st.cur.frame.info.dpr;
    const a = st.anim;
    if (a && st.old) {
      const t = Math.min(1, (performance.now() - a.start) / a.duration);
      const e = easeOut(t);
      // the transform that maps rect -> whole canvas, interpolated from identity
      const full = { sx: W / a.rect.w, sy: H / a.rect.h, tx: -a.rect.x * (W / a.rect.w), ty: -a.rect.y * (H / a.rect.h) };
      const lerp = (k: number) => ({ sx: 1 + (full.sx - 1) * k, sy: 1 + (full.sy - 1) * k, tx: full.tx * k, ty: full.ty * k });
      const fade = Math.max(0, (t - 0.55) / 0.45);
      if (a.mode === "in") {
        const m = lerp(e);
        st.gl.draw(st.old.picture, m.sx, m.sy, m.tx, m.ty, 1 - fade);
        st.gl.draw(st.cur.picture, 1, 1, 0, 0, fade);
        ctx.setTransform(m.sx, 0, 0, m.sy, m.tx, m.ty);
        drawTexts(ctx, st.old.texts, 1 - fade);
        drawTags(ctx, st.old.tags, dpr, 1 - fade);
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        drawTexts(ctx, st.cur.texts, fade);
        drawTags(ctx, st.cur.tags, dpr, fade);
      } else {
        const m = lerp(1 - e);
        const appear = Math.min(1, t / 0.35);
        st.gl.draw(st.old.picture, 1, 1, 0, 0, 1 - appear);
        st.gl.draw(st.cur.picture, m.sx, m.sy, m.tx, m.ty, appear);
        ctx.setTransform(m.sx, 0, 0, m.sy, m.tx, m.ty);
        drawTexts(ctx, st.cur.texts, appear);
        drawTags(ctx, st.cur.tags, dpr, appear);
      }
      if (t < 1) {
        st.raf = requestAnimationFrame(render);
        return;
      }
      st.anim = null;
      st.gl.dispose(st.old.picture);
      st.old = null;
      render();
      return;
    }
    st.gl.draw(st.cur.picture);
    drawTexts(ctx, st.cur.texts);
    drawTags(ctx, st.cur.tags, dpr);
  };

  const repaint = (frame: DrawFrame, colorMode: ColorMode, keepPicture?: Picture | null) => {
    const st = r.current;
    if (!st.gl || !textCanvas.current) return null;
    const painted = paint(frame, {
      dpr: frame.info.dpr,
      colorMode,
      classIds,
      hovered: st.hovered,
      scanning,
      formatSize: (b) => fmtSize(b),
      formatInt: (n) => fmtInt(n),
      haloLevels,
      hoverHighlight,
      selectionShadow,
    });
    const picture = st.gl.upload(painted.instances, painted.count, keepPicture);
    const ctx = textCanvas.current.getContext("2d")!;
    return { frame, picture, texts: prepareTexts(ctx, painted.texts), tags: painted.tags } satisfies Layer;
  };

  // ---------------------------------------------------------------- setup

  useEffect(() => {
    if (!glCanvas.current) return;
    try {
      r.current.gl = new GlRenderer(glCanvas.current);
    } catch (err) {
      console.error(err);
    }
    return () => cancelAnimationFrame(r.current.raf);
  }, []);

  // new frame, or a colour-mode / hover change
  const frame = view?.frame ?? null;
  const colorMode = (view?.state.color_mode ?? "class") as ColorMode;
  useEffect(() => {
    const st = r.current;
    if (!frame || !st.gl) return;
    const isNew = frame.info.seq !== st.lastSeq;
    st.lastSeq = frame.info.seq;
    if (!isNew) {
      // same frame, new hover or colour mode: repaint in place
      if (st.cur) st.cur = repaint(frame, colorMode, st.cur.picture) ?? st.cur;
      if (!st.anim) render();
      return;
    }
    st.hovered = -1;
    const next = repaint(frame, colorMode);
    if (!next) return;
    const reason = frame.info.reason;
    const zoomKey = view?.state.zoom_key ?? null;
    const prevZoom = st.lastZoomKey;
    st.lastZoomKey = zoomKey;
    const duration = reduced ? 0 : zoomMs;
    let anim: Anim | null = null;
    if (st.cur && duration > 0 && st.cur.frame.info.width === frame.info.width && st.cur.frame.info.height === frame.info.height) {
      const prev = st.cur.frame;
      if (reason === "zoom" && zoomKey !== null) {
        const row = prev.rowOf(zoomKey);
        if (row >= 0) anim = { start: performance.now(), duration, mode: "in", rect: rectOf(prev, row) };
      } else if ((reason === "back" || reason === "up" || reason === "home" || reason === "forward") && prevZoom !== null) {
        const row = frame.rowOf(prevZoom);
        if (row >= 0) anim = { start: performance.now(), duration, mode: reason === "forward" ? "in" : "out", rect: rectOf(frame, row) };
        if (anim && anim.mode === "in") {
          const back = prev.rowOf(zoomKey ?? -1);
          anim = back >= 0 ? { ...anim, rect: rectOf(prev, back) } : null;
        }
      }
    }
    if (st.old) st.gl.dispose(st.old.picture);
    st.old = anim ? st.cur : null;
    if (!anim && st.cur) st.gl.dispose(st.cur.picture);
    st.cur = next;
    st.anim = anim;
    cancelAnimationFrame(st.raf);
    render();
    onHover?.(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [frame, colorMode, unitEpoch, haloLevels, hoverHighlight, selectionShadow]);

  // hover driven from outside (W3 result list): highlight that block like a pointer hover
  const hoverKey = view?.hoverKey ?? null;
  useEffect(() => {
    const st = r.current;
    if (!st.cur || st.anim || !st.gl) return;
    const row = hoverKey == null ? -1 : st.cur.frame.rowOf(hoverKey);
    if (row === st.hovered) return;
    st.hovered = row;
    st.cur = repaint(st.cur.frame, colorMode, st.cur.picture) ?? st.cur;
    render();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hoverKey, frame]);

  // viewport: tell the backend (≥ 10 updates a second while resizing, UI-TM-05)
  useEffect(() => {
    const el = host.current;
    if (!el) return;
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
    return () => {
      ro.disconnect();
      if (timer) clearTimeout(timer);
    };
  }, [viewId]);

  // ---------------------------------------------------------------- pointer

  const toCanvas = (ev: React.MouseEvent) => {
    const el = glCanvas.current!;
    const rect = el.getBoundingClientRect();
    const f = r.current.cur?.frame;
    if (!f) return null;
    return { px: (ev.clientX - rect.left) * (f.info.width / rect.width), py: (ev.clientY - rect.top) * (f.info.height / rect.height), f, rect };
  };

  const onMove = (ev: React.MouseEvent) => {
    const st = r.current;
    if (st.anim) return;
    const p = toCanvas(ev);
    if (!p) return;
    const row = hitTest(p.f, p.px, p.py);
    if (row === st.hovered) return;
    st.hovered = row;
    if (st.cur) st.cur = repaint(st.cur.frame, colorMode, st.cur.picture) ?? st.cur;
    render();
    if (row < 0) onHover?.(null);
    else {
      const k = p.rect.width / p.f.info.width;
      onHover?.({
        viewId,
        frame: p.f,
        row,
        rect: { x: p.f.x[row]! * k, y: p.f.y[row]! * k, w: p.f.w[row]! * k, h: p.f.h[row]! * k },
      });
    }
  };

  const onLeave = () => {
    const st = r.current;
    if (st.hovered >= 0) {
      st.hovered = -1;
      if (st.cur) st.cur = repaint(st.cur.frame, colorMode, st.cur.picture) ?? st.cur;
      render();
    }
    onHover?.(null);
  };

  const onClick = (ev: React.MouseEvent) => {
    const p = toCanvas(ev);
    if (!p) return;
    const row = hitTest(p.f, p.px, p.py);
    void viewCall(viewId, "view.select", { key: row >= 0 ? p.f.key[row]! : null });
  };

  const onDouble = (ev: React.MouseEvent) => {
    const p = toCanvas(ev);
    if (!p) return;
    const row = hitTest(p.f, p.px, p.py);
    if (row >= 0) void viewCall(viewId, "view.zoom", { key: p.f.key[row]! });
  };

  const onMenu = (ev: React.MouseEvent) => {
    ev.preventDefault();
    const p = toCanvas(ev);
    if (!p) return;
    const row = hitTest(p.f, p.px, p.py);
    if (row < 0) return;
    void viewCall(viewId, "view.select", { key: p.f.key[row]! });
    onContextMenu?.(row, p.f, ev);
  };

  return (
    <div ref={host} className="treemap" onMouseMove={onMove} onMouseLeave={onLeave} onClick={onClick} onDoubleClick={onDouble} onContextMenu={onMenu}>
      <canvas ref={glCanvas} className="treemap-layer" />
      <canvas ref={textCanvas} className="treemap-layer" />
      {frame && frame.count === 0 && <div className="treemap-empty">这里没有任何占用空间的内容</div>}
    </div>
  );
}

function rectOf(f: DrawFrame, row: number) {
  return { x: f.x[row]!, y: f.y[row]!, w: Math.max(1, f.w[row]!), h: Math.max(1, f.h[row]!) };
}
