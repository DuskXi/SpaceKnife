// When and how the view host draws: which transition a new frame gets, the 「动画帧率」 cap and
// DynaSmooth, a requestAnimationFrame loop per canvas layer, and the media queries drawing depends
// on (reduced motion, the device pixel ratio) followed when they change.

import { useEffect, useState } from "react";

import { HIT_NONE, type VisualFrame } from "./types";

/** 「动画帧率」: least ms between two display frames that are drawn. */
export const FPS_GAP: Readonly<Record<string, number>> = { display: 0, "60": 1000 / 61, "30": 1000 / 31 };

export interface Pace {
  /** FPS_GAP of the 「动画帧率」 setting */
  fpsGap: number;
  /** 「DynaSmooth」 */
  dynaSmooth: boolean;
}

/** Least ms between two frames of the main layer. What the user does (a zoom, a swap, the trail)
 * gets the full rate; under DynaSmooth change flashes run at 30 fps; nothing moving: no cap, a new
 * frame or a hover draws at once. */
export function frameGap(p: Pace, moving: { user: boolean; ambient: boolean }): number {
  if (!moving.user && !moving.ambient) return 0;
  return moving.user || !p.dynaSmooth ? p.fpsGap : Math.max(p.fpsGap, FPS_GAP["30"]!);
}

/** Least ms between two breaths of the scanning outline (UI-TM-42): at most 30 fps under DynaSmooth;
 * no cap for a single still drawing (nothing breathing, or reduced motion). */
export function breathGap(p: Pace, breathing: boolean, reduced: boolean): number {
  if (!breathing || reduced) return 0;
  return p.dynaSmooth ? Math.max(p.fpsGap, FPS_GAP["30"]!) : p.fpsGap;
}

export type TransitionKind = "in" | "out" | "swap";

/** How a new frame replaces the one on screen. A zoom grows a row of the old frame to the whole
 * canvas ("in": `row` of `prev`); back / up / home shrink the new frame from the row that was the
 * zoom root ("out": `row` of `next`; forward grows like a zoom); another visualization cross-fades.
 * null: the new frame simply replaces the old one. */
export function transitionFor(
  prev: VisualFrame,
  next: VisualFrame,
  reason: string,
  zoomKey: number | null,
  prevZoom: number | null,
): { kind: TransitionKind; row: number } | null {
  if (reason === "visual" || prev.info.visual !== next.info.visual || prev.layout !== next.layout) return { kind: "swap", row: HIT_NONE };
  if (reason === "zoom" && zoomKey !== null) {
    const row = prev.rowOf(zoomKey);
    return row >= 0 ? { kind: "in", row } : null;
  }
  if ((reason === "back" || reason === "up" || reason === "home" || reason === "forward") && prevZoom !== null) {
    const row = next.rowOf(prevZoom);
    if (row < 0) return null;
    if (reason !== "forward") return { kind: "out", row };
    const back = prev.rowOf(zoomKey ?? -1);
    return back >= 0 ? { kind: "in", row: back } : null;
  }
  return null;
}

/** A requestAnimationFrame loop: run() draws now, or at a later display frame while gap() says it
 * is too early; it goes on as long as draw() returns true. */
export class FrameLoop {
  private raf = 0;
  private last = 0;

  constructor(
    private readonly draw: (now: number) => boolean,
    private readonly gap: () => number,
  ) {}

  readonly run = (): void => {
    cancelAnimationFrame(this.raf);
    this.raf = 0;
    const now = performance.now();
    const gap = this.gap();
    if (gap && now - this.last < gap) {
      this.raf = requestAnimationFrame(this.run); // 「动画帧率」: skip this display frame
      return;
    }
    this.last = now;
    if (this.draw(now)) this.raf = requestAnimationFrame(this.run);
  };

  stop(): void {
    cancelAnimationFrame(this.raf);
    this.raf = 0;
  }
}

const media = (query: string): MediaQueryList | null => (typeof window !== "undefined" && window.matchMedia ? window.matchMedia(query) : null);

/** 「减少动画」: on, off, or the system's setting, followed when the system's changes. */
export function useReducedMotion(setting: string): boolean {
  const [system, setSystem] = useState(() => media("(prefers-reduced-motion: reduce)")?.matches ?? false);
  useEffect(() => {
    const mq = media("(prefers-reduced-motion: reduce)");
    if (!mq) return;
    const on = () => setSystem(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return setting === "on" || (setting === "system" && system);
}

/** window.devicePixelRatio, followed when it changes without a resize (another monitor, zoom). */
export function useDevicePixelRatio(): number {
  const [dpr, setDpr] = useState(() => (typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1));
  useEffect(() => {
    let mq: MediaQueryList | null = null;
    const watch = () => {
      // a query for the current ratio stops matching when the ratio changes
      mq = media(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
      mq?.addEventListener("change", on);
    };
    const on = () => {
      mq?.removeEventListener("change", on);
      setDpr(window.devicePixelRatio || 1);
      watch();
    };
    watch();
    return () => mq?.removeEventListener("change", on);
  }, []);
  return dpr;
}
