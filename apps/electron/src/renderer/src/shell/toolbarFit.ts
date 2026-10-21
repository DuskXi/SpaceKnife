// The toolbar in a narrow window (UI-MAIN-18, UI-SPEC open item 21): nothing may be cut off down to
// the smallest window (1024 px). The filter box shrinks first (its CSS min-width); after that the
// toolbar folds groups away one level at a time, measuring after each step, until it fits.
// Navigation, the breadcrumb, split, the command palette and the delete basket always stay.

import { type RefObject, useLayoutEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";

/** What is folded away at a collapse level. Each level keeps the ones before it. */
export interface Collapse {
  /** 1: the free / unknown space and recycle bin chips show only their swatch */
  chipSwatches: boolean;
  /** 2: the coloring segment (in a comparison: the changes shown) moves into the 「更多」 menu */
  coloringInMenu: boolean;
  /** 3: the chips move into the 「更多」 menu */
  chipsInMenu: boolean;
  /** 4: the palette button loses its Ctrl+K key cap, the stepper the word 「层级」, 「退出对比」 its text */
  compact: boolean;
  /** 5: the visualization switch moves into the 「更多」 menu */
  visualInMenu: boolean;
}

export const MAX_LEVEL = 5;

export function collapse(level: number): Collapse {
  return {
    chipSwatches: level >= 1,
    coloringInMenu: level >= 2,
    chipsInMenu: level >= 3,
    compact: level >= 4,
    visualInMenu: level >= 5,
  };
}

/** How narrow a part of the toolbar may get, for parts without a CSS min-width: the breadcrumb keeps
 * this much of itself (all of it when it is shorter) before the toolbar folds groups away. */
export const FIT_MIN_ATTR = "data-fit-min";

/** The width the toolbar needs to show what it shows now without cutting anything off: the parts that
 * don't shrink at their width, the ones that do at their minimum (CSS min-width, or `data-fit-min`). */
export function neededWidth(bar: HTMLElement): number {
  const style = getComputedStyle(bar);
  const gap = parseFloat(style.columnGap) || 0;
  let width = (parseFloat(style.paddingLeft) || 0) + (parseFloat(style.paddingRight) || 0);
  let parts = 0;
  for (const el of Array.from(bar.children) as HTMLElement[]) {
    const s = getComputedStyle(el);
    if (s.display === "none" || s.position === "absolute" || s.position === "fixed") continue;
    parts++;
    const fitMin = el.getAttribute(FIT_MIN_ATTR);
    if (fitMin != null) width += Math.min(naturalWidth(el), Number(fitMin));
    else if (s.flexShrink !== "0") width += parseFloat(s.minWidth) || 0;
    else width += el.getBoundingClientRect().width;
    // the right-hand group's auto margin is free space, not room it needs
    if (!el.classList.contains("toolbar-right")) width += (parseFloat(s.marginLeft) || 0) + (parseFloat(s.marginRight) || 0);
  }
  return width + gap * Math.max(0, parts - 1);
}

/** The width an element takes when it gets all the room it wants (its parts shrink with it, so its
 * scrollWidth doesn't tell). Measured by sizing it to max-content for a moment. */
function naturalWidth(el: HTMLElement): number {
  const { flex, width, maxWidth } = el.style;
  el.style.flex = "none";
  el.style.width = "max-content";
  el.style.maxWidth = "none";
  const natural = el.getBoundingClientRect().width;
  el.style.flex = flex;
  el.style.width = width;
  el.style.maxWidth = maxWidth;
  return natural;
}

/** The collapse level the toolbar needs now. `content` names what it shows (tab, language, path,
 * chips…): when it changes, and when the window gets wider or narrower, the level starts over at 0
 * and climbs while the toolbar doesn't fit. That happens before the browser paints, so no frame
 * shows a clipped toolbar. A part that grows on its own (a label loading) only makes it climb. */
export function useCollapseLevel(bar: RefObject<HTMLElement | null>, content: string): number {
  const [fit, setFit] = useState({ level: 0, tick: 0 });
  const shown = useRef(content);
  useLayoutEffect(() => {
    const el = bar.current;
    if (!el) return;
    if (shown.current !== content) {
      shown.current = content;
      if (fit.level !== 0) {
        setFit((f) => ({ level: 0, tick: f.tick + 1 }));
        return;
      }
    }
    if (fit.level < MAX_LEVEL && neededWidth(el) > el.clientWidth + 0.5) setFit((f) => ({ level: f.level + 1, tick: f.tick }));
  });
  useLayoutEffect(() => {
    const el = bar.current;
    if (!el) return;
    let width = el.clientWidth;
    const recheck = () => setFit((f) => ({ ...f, tick: f.tick + 1 }));
    const observer = new ResizeObserver((entries) => {
      if (entries.some((e) => e.target === el) && el.clientWidth !== width) {
        width = el.clientWidth;
        flushSync(() => setFit((f) => ({ level: 0, tick: f.tick + 1 })));
      } else recheck();
    });
    observer.observe(el);
    for (const child of Array.from(el.children)) observer.observe(child);
    // web fonts arriving change every label's width
    void document.fonts?.ready.then(() => setFit((f) => ({ level: 0, tick: f.tick + 1 })));
    return () => observer.disconnect();
  }, [bar, content]);
  return fit.level;
}
