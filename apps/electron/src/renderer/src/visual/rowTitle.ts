// What a row (or the zoom root) is called: aggregates, free / unknown space and folders waiting for
// the scan have texts of their own instead of a name. One place for the labels, the hover card, the
// context menu and the crumbs.

import { t } from "@/lib/i18n";

import { Kind } from "./frame";
import { HIT_ROOT, type Hit, type VisualFrame } from "./types";

export interface RowTitle {
  /** the name or what stands for it; null when the frame carries no name for the row */
  line1: string | null;
  /** text that replaces the size line; null = the size */
  line2: string | null;
}

const NONE: RowTitle = { line1: null, line2: null };

export function rowTitle(f: VisualFrame, row: Hit, o: { scanning: boolean; formatInt: (n: number) => string }): RowTitle {
  if (row === HIT_ROOT) return rootTitle(f, o.formatInt);
  if (row < 0 || row >= f.count) return NONE;
  const kind = f.kind[row]!;
  const name = f.names[row] || null;
  switch (kind) {
    case Kind.SMALL:
      return { line1: t("{n} 个小项", { n: o.formatInt(f.count_[row]!) }), line2: null };
    case Kind.MORE:
      return { line1: t("+{n} 项", { n: o.formatInt(f.count_[row]!) }), line2: null };
    case Kind.FREE:
      return { line1: t("可用空间"), line2: null };
    case Kind.UNKNOWN:
      return { line1: o.scanning ? t("未知空间 · 尚未扫描") : t("未知空间"), line2: null };
    case Kind.PENDING:
      return { line1: name, line2: t("等待扫描…") };
    default:
      return { line1: name, line2: null };
  }
}

/** The zoom root: named by the last crumb ('small:N' / 'more:N' for an aggregate zoomed into).
 * `short`: the scanned folder by its own name, not its whole path (where the label is narrow). */
export function rootTitle(f: VisualFrame, formatInt: (n: number) => string, short = false): RowTitle {
  const crumbs = f.info.breadcrumb;
  const last = crumbs[crumbs.length - 1];
  const root = f.info.root;
  if (!last) return NONE;
  const label = last.label;
  if (label.startsWith("small:")) return { line1: t("{n} 个小项", { n: formatInt(Number(label.slice(6))) }), line2: null };
  if (label.startsWith("more:")) return { line1: t("+{n} 项", { n: formatInt(Number(label.slice(5))) }), line2: null };
  // the first crumb is the scanned path itself, the others are folder names
  const whole = crumbs.length === 1 && !short;
  const name = whole ? label.replace(/[\\/]+$/, "") || label : (label.split(/[\\/]/).filter(Boolean).pop() ?? label);
  return { line1: name || null, line2: root?.kind === Kind.PENDING ? t("等待扫描…") : null };
}
