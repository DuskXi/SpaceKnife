// Hover card (UI-MAIN-29…31): beside the hovered block, never covering it; flips to stay inside.

import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type { TreeNodeResult } from "@/protocol/generated";
import { call } from "@/state/actions";
import { setting, useApp } from "@/state/store";
import { Kind } from "@/treemap/frame";
import type { HoverInfo } from "@/treemap/Treemap";
import { fillOf, labelColors } from "@/treemap/paint";
import { classColors } from "@/lib/classes";
import { fmtAgo, fmtDateTime, fmtInt, fmtPct, sizeParts } from "@/lib/format";
import { Icon, Kbd } from "@/ui/Icon";

const DELAY = 300;

export function HoverCard({ hover, bounds }: { hover: HoverInfo | null; bounds: { w: number; h: number } }) {
  const vocab = useApp((s) => s.vocab);
  const scanId = useApp((s) => (hover ? s.views[hover.viewId]?.state.scan_id : undefined));
  const colorMode = useApp((s) => (hover ? s.views[hover.viewId]?.state.color_mode : "class")) ?? "class";
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
    if (keepOpen && shown) {
      setShown(hover); // moving between blocks: the open card switches in place
      return;
    }
    setShown(null);
    const t = setTimeout(() => setShown(hover), DELAY);
    return () => clearTimeout(t);
  }, [hover, shown, keepOpen]);

  useEffect(() => {
    setInfo(null);
    if (!shown || !scanId) return;
    const kind = shown.frame.kind[shown.row]!;
    if (kind >= Kind.SMALL) return;
    let alive = true;
    const node = shown.frame.node[shown.row]!;
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
  }, [shown, scanId]);

  // right of the block, top-aligned; else left; else below / above (UI-MAIN-29)
  useLayoutEffect(() => {
    if (!shown || !card.current) return;
    const r = shown.rect;
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
  const row = shown.row;
  const kind = f.kind[row]!;
  const fill = fillOf(f, row, colorMode as "class", vocab?.classes.map((c) => c.id) ?? [], undefined, classColors(vocab));
  const [fg] = labelColors(fill);
  const bg = `rgb(${fill.slice(0, 3).map((v) => Math.round(v * 255)).join(",")})`;
  const [num, unit] = sizeParts(Number(f.size[row]));
  const cls = vocab?.classes[f.cls[row]!];
  const node = info?.node;
  const dir = kind === Kind.FOLDER || kind === Kind.COLLAPSED || kind === Kind.PENDING;
  const title =
    kind === Kind.SMALL
      ? `${fmtInt(f.count_[row]!)} 个小项`
      : kind === Kind.MORE
        ? `+${fmtInt(f.count_[row]!)} 项`
        : kind === Kind.FREE
          ? "可用空间"
          : kind === Kind.UNKNOWN
            ? "未知空间"
            : (node?.name ?? f.names[row] ?? "");
  const folder = info ? info.path.slice(0, Math.max(0, info.path.length - info.node.name.length)).replace(/[\\/]+$/, "") : "";
  const parentName = folder.split(/[\\/]/).filter(Boolean).pop() ?? "";

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
            <span className="muted">
              占 {parentName} <span className="mono strong">{fmtPct(node.alloc, parentAlloc)}</span>
            </span>
          )}
        </div>
        {node && (
          <div className="hc-class muted">
            {dir ? (
              <span>
                {fmtInt(node.files)} 个文件 · {fmtInt(node.dirs)} 个文件夹
              </span>
            ) : (
              <>
                <span className="swatch" style={{ background: bg }} />
                <span>
                  {cls?.name ?? "其他"}
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
              ["创建", node.ctime],
              ["修改", node.mtime],
              ["访问", node.atime],
            ] as const
          ).map(([label, t]) => (
            <div key={label} className="hc-time">
              <span className="muted">{label}</span>
              {showAge && <span>{fmtAgo(t)}</span>}
              <span className="mono muted">{fmtDateTime(t)}</span>
            </div>
          ))}
        </div>
      )}
      <div className="hc-hint">
        <span>双击放大</span>
        <span>·</span>
        <span>右键更多</span>
        <span>·</span>
        <Kbd>Space</Kbd>
        <span>预览</span>
      </div>
    </div>
  );
}
