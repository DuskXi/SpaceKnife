// Hover card (UI-MAIN-29…31): beside the hovered block, never covering it; flips to stay inside.
// Where a row's box can be most of the drawing (a large sector), the card follows the pointer.

import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";

import type { DupeNodeInfo, FilterVocab, TreeNodeResult } from "@/protocol/generated";
import { call } from "@/state/actions";
import { dupesOf, focusGroup, keepOptions, ruleLabel } from "@/state/dupes";
import { activeTab, get, setting, useApp } from "@/state/store";
import { fillOf, labelColors } from "@/treemap/paint";
import { Flag, Kind } from "@/visual/frame";
import { pointerBox } from "@/visual/Host";
import { rowTitle } from "@/visual/rowTitle";
import { type Box, HIT_ROOT, type HoverInfo, type VisualFrame } from "@/visual/types";
import { classColors } from "@/lib/classes";
import { fmtAgo, fmtDateTime, fmtInt, fmtPct, fmtSize, middlePath, sizeParts } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon, Kbd } from "@/ui/Icon";

const DELAY = 300;

/** A translated text with React nodes in its {placeholders}: the word order is the language's. */
function withNodes(text: string, nodes: Record<string, React.ReactNode>): React.ReactNode[] {
  return text.split(/\{(\w+)\}/).map((part, i) => (i % 2 ? <Fragment key={i}>{nodes[part]}</Fragment> : part));
}

/** The same target: the card stays (and only moves when it follows the pointer). */
export const sameTarget = (a: HoverInfo | null, b: HoverInfo | null) => !!a && !!b && a.viewId === b.viewId && a.frame === b.frame && a.target === b.target;

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

export function HoverCard({ hover, bounds }: { hover: HoverInfo | null; bounds: { w: number; h: number } }) {
  const vocab = useApp((s) => s.vocab);
  const scanId = useApp((s) => (hover ? s.views[hover.viewId]?.state.scan_id : undefined));
  const colorMode = useApp((s) => (hover ? s.views[hover.viewId]?.state.color_mode : "class")) ?? "class";
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
  const [parentAlloc, setParentAlloc] = useState(0);
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

  // the node's details, once per target (not per pointer move)
  const frame = shown?.frame ?? null;
  const target = shown?.target ?? null;
  // 「重复」 colouring: a copy hovered as long as its card needs lights its whole group (docs/dupes.md §10.3)
  const shownView = shown?.viewId ?? null;
  useEffect(() => {
    if (colorMode !== "dupes" || !frame || target === null || target < 0 || !frame.dupg || !scanId || !shownView) return;
    const g = frame.dupg[target] ?? 0;
    const status = dupesOf(scanId);
    if (g <= 0 || !status) return;
    const tab = activeTab(get());
    focusGroup(status.job_id, g - 1, tab.viewIds.includes(shownView) ? tab.viewIds : [shownView]);
    return () => focusGroup(null, null);
  }, [colorMode, frame, target, scanId, shownView]);
  useEffect(() => {
    setInfo(null);
    if (!frame || target === null || !scanId) return;
    const { kind, node } = hoverFacts(frame, target, null);
    if (kind >= Kind.SMALL) return;
    let alive = true;
    void call("tree.node", { scan_id: scanId, node })
      .then(async (r) => {
        if (!alive) return;
        setInfo(r);
        if (r.node.parent > 0) {
          const p = await call("tree.node", { scan_id: scanId, node: r.node.parent });
          if (alive) setParentAlloc(p.node.alloc);
        }
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [frame, target, scanId]);

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
  const fill = fillOf(facts.columns, facts.row, colorMode, vocab?.classes.map((c) => c.id) ?? [], undefined, classColors(vocab));
  const [fg] = labelColors(fill);
  const bg = `rgb(${fill.slice(0, 3).map((v) => Math.round(v * 255)).join(",")})`;
  const [num, unit] = sizeParts(facts.size);
  const cls = vocab?.classes[facts.cls];
  const dir = kind === Kind.FOLDER || kind === Kind.COLLAPSED || kind === Kind.PENDING;
  const named = rowTitle(f, shown.target, { scanning, formatInt: (n) => fmtInt(n) }).line1;
  const title = (kind < Kind.SMALL ? node?.name : null) ?? named ?? "";
  const folder = info ? info.path.slice(0, Math.max(0, info.path.length - info.node.name.length)).replace(/[\\/]+$/, "") : "";
  const parentName = folder.split(/[\\/]/).filter(Boolean).pop() ?? "";
  const flags = shown.target === HIT_ROOT ? (f.info.root?.flags ?? 0) : (f.flags[shown.target] ?? 0);

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
          {parentAlloc > 0 && node && (
            <span className="muted">{withNodes(t("占 {name} {share}"), { name: parentName, share: <span className="mono strong">{fmtPct(node.alloc, parentAlloc)}</span> })}</span>
          )}
        </div>
        {flags & Flag.TRASH ? <TrashLine bin={(flags & Flag.TRASH_BIN) !== 0} size={facts.size} /> : null}
        {info?.dupe ? <DupeLine d={info.dupe} drawn={frame && target !== null && target >= 0 ? drawnCopies(frame, target) : null} /> : null}
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

/** Rows of the frame drawing a copy of the row's group (the row itself included); null: no column. */
export function drawnCopies(f: VisualFrame, row: number): number | null {
  const g = f.dupg?.[row] ?? 0;
  if (!f.dupg || g === 0) return null;
  let n = 0;
  for (let i = 0; i < f.count; i++) if (f.dupg[i] === g) n++;
  return n;
}

/** Duplicate files (docs/dupes.md §10.4): only copies whose whole contents were read are called
 * identical; files still being checked make no claim. `drawn`: copies the picture shows. */
export function DupeLine({ d, drawn = null }: { d: DupeNodeInfo; drawn?: number | null }) {
  if (d.state === "pending") return <div className="hc-dupe">{t("同样大小的文件正在核对内容…")}</div>;
  if (d.state === "skipped") return <div className="hc-dupe muted">{t("没有比较：{reason}", { reason: d.reason })}</div>;
  if (d.state === "none") return d.redundant > 0 ? <div className="hc-dupe">{t("里面有多余的副本 {size}", { size: fmtSize(d.redundant) })}</div> : null;
  if (d.state === "shared") {
    const other = d.others[0]?.path ?? "";
    return <div className="hc-dupe">{t("和 {path} 是同一份数据（硬链接或块克隆），删了不腾空间", { path: middlePath(other, 48) })}</div>;
  }
  const others = d.copies - 1;
  const hidden = drawn !== null && drawn < d.copies ? d.copies - drawn : 0;
  return (
    <div className="hc-dupe">
      <span>{t("另有 {n} 份内容相同 · 删掉多余的能腾出 {size}", { n: fmtInt(others), size: fmtSize(d.reclaim) })}</span>
      {d.others.slice(0, 2).map((m) => (
        <span key={`${m.scan_id}:${m.path}`} className="mono">
          {middlePath(m.path, 56)}
        </span>
      ))}
      {d.other_volumes.length > 0 && <span>{t("其中有的在 {vols}", { vols: d.other_volumes.join(" · ") })}</span>}
      {hidden > 0 && <span className="muted">{t("另 {n} 份不在画面里（在折叠的文件夹或小项里）", { n: fmtInt(hidden) })}</span>}
      <span className="hc-dupe-role">{d.state === "extra" ? t("这一份是多余的") : whyText(d.why)}</span>
    </div>
  );
}

const whyText = (why: DupeNodeInfo["why"]): string =>
  why === "preferred"
    ? t("在优先保留的位置里")
    : why === "folder"
      ? t("和同一对文件夹里的其他组留在同一边")
      : why === "volume"
        ? t("每个卷至少留一份")
        : why === "needed"
          ? t("程序需要或受保护")
          : why === "manual"
            ? t("手动选择保留")
            : why === "pinned"
              ? t("只能保留（另有名字，或删除篮删不了它）")
              : t("按「{rule}」保留这一份", { rule: ruleLabel(keepOptions().rule) });
