// Hover card (UI-MAIN-29…31): beside the hovered block, never covering it; flips to stay inside.
// Where a row's box can be most of the drawing (a large sector), the card follows the pointer.

import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";

import type { FilterVocab, TreeNodeResult } from "@/protocol/generated";
import { call } from "@/state/actions";
import { setting, useApp } from "@/state/store";
import { type ColorMode, fillOf, labelColors } from "@/treemap/paint";
import { Flag, Kind } from "@/visual/frame";
import { pointerBox, shareText, weightTotal } from "@/visual/Host";
import { sameTarget } from "@/visual/hover";
import { rowTitle } from "@/visual/rowTitle";
import { type Box, HIT_ROOT, type HoverInfo, type VisualFrame } from "@/visual/types";
import { classColors, classHex } from "@/lib/classes";
import { fmtAgo, fmtDateTime, fmtInt, fmtPct, fmtSize, sizeParts } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon, Kbd } from "@/ui/Icon";
import { palettePaint } from "@/plugins/colors";
import { HoverFields } from "@/plugins/fields";

const DELAY = 300;

/** A translated text with React nodes in its {placeholders}: the word order is the language's. */
function withNodes(text: string, nodes: Record<string, React.ReactNode>): React.ReactNode[] {
  return text.split(/\{(\w+)\}/).map((part, i) => (i % 2 ? <Fragment key={i}>{nodes[part]}</Fragment> : part));
}

/** What a card keeps clear of: the row's box, or a small box around the pointer. */
export const cardAvoid = (h: HoverInfo): Box => (h.placement === "pointer" ? pointerBox(h.pointer) : h.avoid);

/** What a hovered row (or the zoom root) is, for the cards: one row of the frame's columns. */
export function hoverFacts(f: VisualFrame, target: number, vocab: FilterVocab | null, ext?: string) {
  const root = target === HIT_ROOT ? f.info.root : null;
  if (root) {
    // the zoom root has no class column: a file root takes the class of its extension once known
    const cls = ext === undefined ? -1 : (vocab?.classes.findIndex((c) => c.extensions.includes(ext)) ?? -1);
    const one = { kind: Uint8Array.of(root.kind), cls: Uint8Array.of(cls < 0 ? 255 : cls), tone: Uint8Array.of(root.tone), branch: Uint8Array.of(root.branch), depth: Uint8Array.of(1), delta: null };
    return { kind: root.kind, node: root.node, key: root.key, size: root.size, cls, columns: one, row: 0 };
  }
  return { kind: f.kind[target]!, node: f.node[target]!, key: f.key[target]!, size: Number(f.size[target]), cls: f.cls[target]!, columns: f, row: target };
}

/** What 「占 <父文件夹名> N%」 divides by: the parent fetched for this very node (a card that moved to
 * another block, or a picture one level up, never divides by the last one's); null while it loads
 * and for a scanned root, which has no parent folder in the scan. */
export function shareBase(node: { parent: number } | undefined, parent: { id: number; alloc: number } | null): number | null {
  return node && parent && node.parent > 0 && parent.id === node.parent && parent.alloc > 0 ? parent.alloc : null;
}

export function HoverCard({ hover, bounds }: { hover: HoverInfo | null; bounds: { w: number; h: number } }) {
  const vocab = useApp((s) => s.vocab);
  const scanId = useApp((s) => (hover ? s.views[hover.viewId]?.state.scan_id : undefined));
  const colorMode = useApp((s) => (hover ? s.views[hover.viewId]?.state.color_mode : "class")) ?? "class";
  const ext = useApp((s) => s.extensions);
  const scanning = useApp((s) => {
    const st = scanId ? s.scans[scanId] : undefined;
    return st?.state === "scanning" || st?.state === "queued";
  });
  const [shown, setShown] = useState<HoverInfo | null>(null);
  // W7 常规 · 悬停提示
  const keepOpen = useApp((s) => setting(s, "general.hover_keep_open", true));
  const showDates = useApp((s) => setting(s, "general.hover_dates", true));
  const showAge = useApp((s) => setting(s, "general.hover_age", true));
  const [info, setInfo] = useState<TreeNodeResult | null>(null);
  const [parent, setParent] = useState<{ id: number; alloc: number } | null>(null);
  const card = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: 0, top: 0 });

  useEffect(() => {
    if (shown === hover) return;
    if (!hover) {
      setShown(null);
      return;
    }
    if ((keepOpen || sameTarget(shown, hover)) && shown) {
      setShown(hover); // moving between blocks (or with the pointer): the open card switches in place
      return;
    }
    setShown(null);
    const timer = setTimeout(() => setShown(hover), DELAY);
    return () => clearTimeout(timer);
  }, [hover, shown, keepOpen]);

  // the node's details, once per node (not per pointer move); a new picture asks again (its numbers
  // may have changed) and the card keeps what it shows until the answer comes
  const facts0 = shown ? hoverFacts(shown.frame, shown.target, null) : null;
  const detailNode = facts0 && facts0.kind < Kind.SMALL ? facts0.node : null;
  const seq = shown?.frame.info.seq ?? 0;
  useEffect(() => {
    if (detailNode === null || !scanId) {
      setInfo(null);
      setParent(null);
      return;
    }
    // the parent's size stays while the answer comes: shareBase uses it only for its own children
    setInfo((cur) => (cur && cur.node.id === detailNode ? cur : null));
    let alive = true;
    void call("tree.node", { scan_id: scanId, node: detailNode })
      .then(async (r) => {
        if (!alive) return;
        setInfo(r);
        if (r.node.parent > 0) {
          const p = await call("tree.node", { scan_id: scanId, node: r.node.parent });
          if (alive) setParent({ id: p.node.id, alloc: p.node.alloc });
        }
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [detailNode, scanId, seq]);

  // right of the block, top-aligned; else left; else below / above (UI-MAIN-29)
  useLayoutEffect(() => {
    if (!shown || !card.current) return;
    const r = cardAvoid(shown);
    const cw = card.current.offsetWidth;
    const ch = card.current.offsetHeight;
    let left = r.x + r.w + 10;
    let top = r.y;
    if (left + cw > bounds.w) left = r.x - 10 - cw;
    if (left < 0) {
      left = Math.min(Math.max(0, r.x), bounds.w - cw);
      top = r.y + r.h + 10;
      if (top + ch > bounds.h) top = r.y - 10 - ch;
    }
    top = Math.max(4, Math.min(top, bounds.h - ch - 4));
    left = Math.max(4, Math.min(left, bounds.w - cw - 4));
    setPos({ left, top });
  }, [shown, info, bounds.w, bounds.h]);

  if (!shown) return null;
  const f = shown.frame;
  const node = info?.node;
  const facts = hoverFacts(f, shown.target, vocab, node?.ext);
  const kind = facts.kind;
  // a plugin's colour mode (colors.v1): the swatch takes the row's palette colour, as the picture does
  const classes = classColors(vocab);
  const palette = palettePaint(colorMode, ext, (id) => classHex(vocab, id));
  const fill = fillOf(facts.columns, facts.row, colorMode as ColorMode, vocab?.classes.map((c) => c.id) ?? [], undefined, classes, palette);
  const [fg] = labelColors(fill);
  const bg = `rgb(${fill.slice(0, 3).map((v) => Math.round(v * 255)).join(",")})`;
  // a visualization sized by file count (size_unit "count"): the number of items, not bytes; by a
  // plugin's weights ("weight"): its share of the root, and the real size on disk below
  const weighted = f.info.size_unit === "weight";
  const [num, unit] =
    f.info.size_unit === "count" ? [fmtInt(facts.size), t("项")] : weighted ? [shareText(facts.size, weightTotal(f)), t("的权重")] : sizeParts(facts.size);
  const cls = vocab?.classes[facts.cls];
  const dir = kind === Kind.FOLDER || kind === Kind.COLLAPSED || kind === Kind.PENDING;
  const named = rowTitle(f, shown.target, { scanning, formatInt: (n) => fmtInt(n) }).line1;
  const title = (kind < Kind.SMALL ? node?.name : null) ?? named ?? "";
  const folder = info ? info.path.slice(0, Math.max(0, info.path.length - info.node.name.length)).replace(/[\\/]+$/, "") : "";
  const parentName = folder.split(/[\\/]/).filter(Boolean).pop() ?? "";
  const flags = shown.target === HIT_ROOT ? (f.info.root?.flags ?? 0) : (f.flags[shown.target] ?? 0);
  const base = shareBase(node, parent);

  return (
    <div ref={card} className="hovercard" role="tooltip" style={pos}>
      <div className="hc-body">
        <div className="hc-head">
          <div className="hc-icon" style={{ background: dir ? "var(--sk-color-bg-raised)" : bg, color: dir ? "var(--sk-color-text-primary)" : fg }}>
            <Icon name={dir ? "folder" : "file"} />
          </div>
          <div className="hc-name">
            <div className="hc-title">{title}</div>
            {folder && <div className="mono hc-path">{folder}</div>}
          </div>
        </div>
        <div className="row-between">
          <div className="big-num small">
            <span className="num">{num}</span>
            <span className="unit">{unit}</span>
          </div>
          {base !== null && node && (
            <span className="muted">{withNodes(t("占 {name} {share}"), { name: parentName, share: <span className="mono strong">{fmtPct(node.alloc, base)}</span> })}</span>
          )}
        </div>
        {/* a plugin's weights drew the area: what it takes on disk is still the real size */}
        {weighted && node && <div className="muted small">{t("磁盘上占 {size}", { size: fmtSize(node.alloc) })}</div>}
        {flags & Flag.TRASH && !weighted ? <TrashLine bin={(flags & Flag.TRASH_BIN) !== 0} size={facts.size} /> : null}
        {node && (
          <div className="hc-class muted">
            {dir ? (
              <span>{t("{files} 个文件 · {dirs} 个文件夹", { files: fmtInt(node.files), dirs: fmtInt(node.dirs) })}</span>
            ) : (
              <>
                <span className="swatch" style={{ background: bg }} />
                <span>
                  {cls?.name ?? t("其他")}
                  {node.ext ? ` · .${node.ext}` : ""}
                </span>
              </>
            )}
          </div>
        )}
      </div>
      {node && shown.target !== HIT_ROOT && <HoverFields scanId={scanId} node={node.id} />}
      {node && showDates && (
        <div className="hc-times">
          {(
            [
              ["ctime", t("创建"), node.ctime],
              ["mtime", t("修改"), node.mtime],
              ["atime", t("访问"), node.atime],
            ] as const
          ).map(([key, label, time]) => (
            <div key={key} className="hc-time">
              <span className="muted">{label}</span>
              {showAge && <span>{fmtAgo(time)}</span>}
              <span className="mono muted">{fmtDateTime(time)}</span>
            </div>
          ))}
        </div>
      )}
      <div className="hc-hint">
        <span>{t("双击放大")}</span>
        <span>·</span>
        <span>{t("右键更多")}</span>
        <span>·</span>
        <Kbd>Space</Kbd>
        <span>{t("预览")}</span>
      </div>
    </div>
  );
}

/** In a recycle bin (「回收站特效」): its space comes back only when the bin is emptied. */
function TrashLine({ bin, size }: { bin: boolean; size: number }) {
  const mac = window.sk.platform === "darwin";
  const text = bin
    ? mac
      ? t("废纸篓：清空后可释放 {size}", { size: fmtSize(size) })
      : t("回收站：清空后可释放 {size}", { size: fmtSize(size) })
    : mac
      ? t("在废纸篓里：清空废纸篓后才释放空间")
      : t("在回收站里：清空回收站后才释放空间");
  return (
    <div className="hc-trash">
      <Icon name="basket" size={12} />
      <span>{text}</span>
    </div>
  );
}
