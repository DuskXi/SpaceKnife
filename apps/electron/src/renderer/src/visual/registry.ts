// Visualization components by wire layout. The built-in ones register in visual/builtins.ts; the
// host and the frame path only reach a component through here, never by importing it.

import { isKnownLayout } from "./frame";
import { VISUAL_API, type VisualComponent, type VisualFrame } from "./types";

/** Capability of a client that understands visualizations (FrameInfo.visual/layout/geometry/root, visual.list). */
export const CAP_VISUALS = "visuals.v1";

const components = new Map<string, VisualComponent>();

export function registerVisual<F extends VisualFrame>(c: VisualComponent<F>): void {
  if (c.api !== VISUAL_API) throw new Error(`visual component for ${c.layout}: api ${String(c.api)}, this renderer speaks ${VISUAL_API}`);
  if (!isKnownLayout(c.layout)) throw new Error(`visual component for ${c.layout}: no decoder for this layout`);
  if (components.has(c.layout)) throw new Error(`a visual component for ${c.layout} is already registered`);
  components.set(c.layout, c as unknown as VisualComponent);
}

export function componentFor(layout: string): VisualComponent | null {
  return components.get(layout) ?? null;
}

/** Layouts this renderer can draw, in registration order. */
export function registeredLayouts(): string[] {
  return [...components.keys()];
}

/** Capabilities to announce in hello: visuals.v1 and one per drawable layout beyond the baseline. */
export function visualCapabilities(): string[] {
  const caps = [CAP_VISUALS];
  for (const c of components.values()) if (c.capability && !caps.includes(c.capability)) caps.push(c.capability);
  return caps;
}
