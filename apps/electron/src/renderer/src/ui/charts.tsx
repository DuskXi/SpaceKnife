// Small charts for summary pages (「本机磁盘」, its card on the start page): a ring and a bar meter
// for one share against a whole, a part-to-whole stacked bar, and a sparkline. Thin marks, a track
// one step off the surface, 2px gaps between segments; the values they show are written next to
// them as text too, and each mark's title says it on hover.

import type { ReactNode } from "react";

export type Tone = "accent" | "warn" | "muted";

const TONE: Record<Tone, string> = {
  accent: "var(--sk-color-accent-default)",
  warn: "var(--sk-color-status-warning)",
  muted: "var(--sk-color-text-tertiary)",
};

/** A ring meter: `share` (0-1) of the circle, from the top, clockwise; `children` in the middle. */
export function Ring({ share, size = 44, stroke = 5, tone = "accent", title, children }: { share: number; size?: number; stroke?: number; tone?: Tone; title?: string; children?: ReactNode }) {
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const s = Math.max(0, Math.min(1, share));
  return (
    <span className="ring" style={{ width: size, height: size }} title={title} role="img" aria-label={title}>
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--sk-color-line-subtle)" strokeWidth={stroke} />
        {s > 0 && (
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            stroke={TONE[tone]}
            strokeWidth={stroke}
            strokeLinecap="round"
            strokeDasharray={`${Math.max(0.01, s * c - (s < 1 ? stroke * 0.6 : 0))} ${c}`}
            transform={`rotate(-90 ${size / 2} ${size / 2})`}
          />
        )}
      </svg>
      {children != null && <span className="ring-label">{children}</span>}
    </span>
  );
}

/** A bar meter: `share` (0-1) of the track. */
export function Meter({ share, tone = "accent", title }: { share: number; tone?: Tone; title?: string }) {
  return (
    <span className="meter" title={title} role="img" aria-label={title}>
      <span style={{ width: `${Math.max(0, Math.min(1, share)) * 100}%`, background: TONE[tone] }} />
    </span>
  );
}

export interface Segment {
  key: string;
  share: number;
  color: string;
  title: string;
}

/** Part-to-whole: segments by share, 2px apart; what is left of the whole stays track. */
export function StackBar({ segments, height = 10, title }: { segments: Segment[]; height?: number; title?: string }) {
  return (
    <span className="stackbar" style={{ height }} role="img" aria-label={title}>
      {segments
        .filter((s) => s.share > 0)
        .map((s) => (
          <span key={s.key} style={{ flexBasis: `${s.share * 100}%`, background: s.color }} title={s.title} />
        ))}
    </span>
  );
}

/** A sparkline of `values` (oldest first), scaled to their own range; the last point marked. */
export function Sparkline({ values, width = 96, height = 26, title }: { values: number[]; width?: number; height?: number; title?: string }) {
  if (values.length < 2) return null;
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const pad = 3;
  const x = (i: number) => pad + (i * (width - 2 * pad)) / (values.length - 1);
  const y = (v: number) => (hi === lo ? height / 2 : height - pad - ((v - lo) * (height - 2 * pad)) / (hi - lo));
  const points = values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const last = values.length - 1;
  return (
    <svg className="sparkline" width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={title}>
      {title && <title>{title}</title>}
      <polyline points={points} fill="none" stroke="var(--sk-color-accent-default)" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
      <circle cx={x(last)} cy={y(values[last]!)} r={3} fill="var(--sk-color-accent-default)" stroke="var(--sk-color-bg-panel)" strokeWidth={2} />
    </svg>
  );
}
