// The geometry of the small summary charts (components/ui/charts/*.vue; ui/charts.tsx in the Electron
// renderer): a ring and a bar meter for one share against a whole, a part-to-whole stacked bar and a
// sparkline. Colours are CSS variables, so the theme switches them without a repaint.

export type Tone = "accent" | "warn" | "muted";

export const TONE: Record<Tone, string> = {
  accent: "var(--sk-color-accent-default)",
  warn: "var(--sk-color-status-warning)",
  muted: "var(--sk-color-text-tertiary)",
};

export interface Segment {
  key: string;
  share: number;
  color: string;
  title: string;
}

/** A share kept to 0-1. */
export const clampShare = (share: number): number => Math.max(0, Math.min(1, share));

/** The ring's arc: radius, and the stroke-dasharray that draws `share` of the circle (a round cap
 * would overshoot by half the stroke at each end, so the arc is shortened unless it is whole). */
export function ringArc(share: number, size: number, stroke: number): { r: number; dash: string } {
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const s = clampShare(share);
  return { r, dash: `${Math.max(0.01, s * c - (s < 1 ? stroke * 0.6 : 0))} ${c}` };
}

/** A sparkline's polyline points and its last point, `values` scaled to their own range. */
export function sparkPoints(values: readonly number[], width: number, height: number): { points: string; last: { x: number; y: number } } | null {
  if (values.length < 2) return null;
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const pad = 3;
  const x = (i: number) => pad + (i * (width - 2 * pad)) / (values.length - 1);
  const y = (v: number) => (hi === lo ? height / 2 : height - pad - ((v - lo) * (height - 2 * pad)) / (hi - lo));
  const points = values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const n = values.length - 1;
  return { points, last: { x: x(n), y: y(values[n]!) } };
}
