// Which row a view host lights as hovered.
//
// A row numbers one frame only: every new picture (a file change, a scan's progress, a zoom) has
// its own rows. So the host never carries a row from one picture to the next. It keeps what stays
// the same between pictures — where the pointer is over the picture, and a key lit from outside
// (a hover in the result list) — and works the row out again, in this one place, whenever the
// picture or either of them changes. (Two paths used to write the row, one from the pointer and
// one from the outside key; on every new frame the second cleared what the first had just lit.)

import { HIT_NONE, HIT_ROOT, type Hit, type HoverInfo, type VisualFrame } from "./types";

/** The row of a key, or the zoom root when that is the key (result list hover, context menu). */
export function hitOfKey(f: VisualFrame, key: number): Hit {
  const row = f.rowOf(key);
  return row >= 0 ? row : f.info.root?.key === key ? HIT_ROOT : HIT_NONE;
}

export interface HoverInputs {
  /** the picture on screen (null: none yet) */
  frame: VisualFrame | null;
  /** a transition runs: nothing is lit until it ends (its end works the row out again) */
  moving: boolean;
  /** the row under the pointer, worked out in `frame`, while the pointer is over the picture; null once it left */
  pointer: Hit | null;
  /** a key lit from outside the picture (null: none) */
  key: number | null;
}

/** The row to light in `frame`: the one under the pointer while it is over the picture, else the
 * outside key's row. */
export function hoverTarget(i: HoverInputs): Hit {
  if (!i.frame || i.moving) return HIT_NONE;
  if (i.pointer !== null) return i.pointer;
  return i.key === null ? HIT_NONE : hitOfKey(i.frame, i.key);
}

/** What the instance on screen shows lit, so that a change reaches it once. An instance drops its
 * row with every new frame and starts with none: call `dropped()` then. */
export class HoverLight {
  private shown: Hit = HIT_NONE;

  get row(): Hit {
    return this.shown;
  }

  dropped(): void {
    this.shown = HIT_NONE;
  }

  /** Light `hit` on `instance`; true when that changed what it shows (it needs drawing). */
  set(instance: { setHover(hit: Hit): void } | null | undefined, hit: Hit): boolean {
    if (hit === this.shown) return false;
    this.shown = hit;
    instance?.setHover(hit);
    return true;
  }
}

/** What a hover card is about, the same in every picture of the view: the row's key. */
export const targetKey = (h: HoverInfo): number => (h.target === HIT_ROOT ? (h.frame.info.root?.key ?? -1) : (h.frame.key[h.target] ?? -1));

/** The same target: the card stays (and only moves when it follows the pointer), also when a new
 * picture of the view comes in (a file change, a scan's progress). */
export const sameTarget = (a: HoverInfo | null, b: HoverInfo | null): boolean => !!a && !!b && a.viewId === b.viewId && targetKey(a) === targetKey(b);
