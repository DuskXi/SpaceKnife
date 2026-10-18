// Details panel (UI-MAIN-32…47, G11): details / class statistics / largest files of the selection.

import { useEffect, useState } from "react";

import type { NodeInfo, TreeSummaryResult } from "@/protocol/generated";
import { basketAdd, cancellable, viewCall } from "@/state/actions";
import { set, toast, useApp } from "@/state/store";
import { Kind } from "@/treemap/frame";
import { fmtAgo, fmtInt, fmtPct, fmtShare, fmtSize, sizeParts } from "@/lib/format";
import { useThrottled } from "@/lib/throttle";
import { isRunning } from "@/shell/ScanProgress";
import { Icon, Kbd } from "@/ui/Icon";

type Page = "details" | "classes" | "largest";
const TAGS = [
  { color: "red", label: "红色标签 ▲", key: 1 },
  { color: "yellow", label: "黄色标签 ■", key: 2 },
  { color: "green", label: "绿色标签 ●", key: 3 },
  { color: "blue", label: "蓝色标签 ◆", key: 4 },
] as const;

export function Inspector({ viewId }: { viewId: string }) {
  const view = useApp((s) => s.views[viewId]);
  const vocab = useApp((s) => s.vocab);
  const [page, setPage] = useState<Page>("details");
  const [summary, setSummary] = useState<TreeSummaryResult | null>(null);
  const frame = view?.frame;
  const scanId = view?.state.scan_id;
  const selRow = frame?.info.selected_row ?? null;
  const selKind = selRow != null && frame ? frame.kind[selRow]! : null;
  // the selection, or the zoom root when nothing is selected (00 待定 6)
  const node = selRow != null && frame && selKind! < Kind.SMALL ? frame.node[selRow]! : (view?.state.zoom_node ?? null);
  const synthetic = selRow != null && frame && selKind! >= Kind.SMALL ? selRow : null;
  const seq = frame?.info.seq ?? 0;
  const running = useApp((s) => (scanId ? isRunning(s.scans[scanId]) : false));
  // every frame may change the totals, but a summary walks the whole subtree: while the scan runs
  // (four frames a second) refresh it once a second, and drop a request the next one replaces
  const tick = useThrottled(seq, running ? 1000 : 0);

  useEffect(() => {
    if (!scanId || node == null || node < 0) return;
    let alive = true;
    const top = page === "largest" ? 50 : 5;
    const job = cancellable("tree.summary", { scan_id: scanId, node, top });
    job.promise.then((r) => alive && r && setSummary(r)).catch(() => {});
    return () => {
      alive = false;
      job.cancel();
    };
  }, [scanId, node, page, tick]);

  const className = (id: string) => vocab?.classes.find((c) => c.id === id)?.name ?? id;

  return (
    <aside className="inspector" aria-label="详情面板">
      <div className="inspector-head">
        <div className="segtabs" role="tablist" aria-label="侧栏视图">
          {(["details", "classes", "largest"] as const).map((p) => (
            <button key={p} role="tab" aria-selected={page === p} className={page === p ? "on" : ""} onClick={() => setPage(p)}>
              {{ details: "详情", classes: "类别统计", largest: "最大文件" }[p]}
            </button>
          ))}
        </div>
        <div className="inspector-tools">
          <button className="icon-btn small" aria-label="更多" title="更多（M6 接入右键菜单）" onClick={() => toast("更多操作在 M6 接入")}>
            <Icon name="more" />
          </button>
          <button className="icon-btn small" aria-label="隐藏详情面板" title="隐藏详情面板" onClick={() => set({ inspectorHidden: true })}>
            <Icon name="sidebar" />
          </button>
        </div>
      </div>
      <div className="inspector-body">
        {synthetic != null && frame ? (
          <SyntheticDetails frameKind={frame.kind[synthetic]!} size={Number(frame.size[synthetic])} count={frame.count_[synthetic]!} />
        ) : !summary ? (
          <div className="muted">正在读取…</div>
        ) : page === "details" ? (
          <Details s={summary} viewId={viewId} className={className} />
        ) : page === "classes" ? (
          <ClassStats s={summary} className={className} />
        ) : (
          <TopList s={summary} viewId={viewId} className={className} />
        )}
      </div>
      {summary && synthetic == null && (
        <div className="inspector-actions">
          <ActionRow icon="reveal" label={revealLabel()} keys="Ctrl+E" disabled={!summary.on_disk} onClick={() => window.sk.shell.showItemInFolder(summary.path)} />
          <ActionRow
            icon="copy"
            label="复制路径"
            keys="Ctrl+C"
            onClick={() => {
              window.sk.shell.copyText(summary.path);
              toast("已复制");
            }}
          />
          <ActionRow icon="basket" danger label="加入删除篮" keys="Delete" disabled={!summary.on_disk} onClick={() => scanId && basketAdd(scanId, summary.node.id)} />
          {!summary.on_disk && <div className="muted small inspector-note">{NOT_ON_DISK}</div>}
        </div>
      )}
    </aside>
  );
}

export const revealLabel = () =>
  window.sk.platform === "darwin" ? "在访达中显示" : window.sk.platform === "win32" ? "在资源管理器中显示" : "在文件管理器中显示";

/** Items under the MFT pseudo folders have no folder of their own to open or delete from. */
export const NOT_ON_DISK = "NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有可以打开或删除的对应位置";

function ActionRow(props: { icon: Parameters<typeof Icon>[0]["name"]; label: string; keys: string; danger?: boolean; disabled?: boolean; onClick: () => void }) {
  return (
    <button className="action-row" onClick={props.onClick} disabled={props.disabled}>
      <span className={props.danger ? "danger" : "muted"}>
        <Icon name={props.icon} />
      </span>
      <span className="flex">{props.label}</span>
      <Kbd>{props.keys}</Kbd>
    </button>
  );
}

function Details({ s, viewId, className }: { s: TreeSummaryResult; viewId: string; className: (id: string) => string }) {
  const n = s.node;
  const dir = n.kind === "dir";
  const [num, unit] = sizeParts(n.alloc);
  const sep = s.path.includes("\\") ? "\\" : "/";
  const cut = s.path.lastIndexOf(sep, s.path.length - 2);
  const vol = s.volume;
  const used = vol ? vol.total - vol.free : 0;
  const cls = !dir ? s.classes[0]?.class_id : undefined;
  return (
    <>
      <div className="ins-title">
        <div className="ins-icon" style={cls ? { background: `var(--sk-treemap-class-${cls})`, color: "#0A0B0D" } : undefined}>
          <Icon name={dir ? "folder" : "file"} size={18} />
        </div>
        <div className="ins-name">
          <h2>{n.name.split(/[\\/]/).filter(Boolean).pop() ?? n.name}</h2>
          <div className="ins-path mono">
            {cut > 0 ? s.path.slice(0, cut + 1) : ""}
            <span>{cut > 0 ? s.path.slice(cut + 1) : s.path}</span>
          </div>
        </div>
      </div>
      <div className="ins-size">
        <div className="big-num">
          <span className="num">{num}</span>
          <span className="unit">{unit}</span>
        </div>
        <div className="ins-stats" title={`精确 ${fmtInt(n.alloc)} 字节（逻辑 ${fmtInt(n.size)} 字节）`}>
          {dir ? `${fmtInt(n.files)} 个文件 · ${fmtInt(n.dirs)} 个文件夹 · 修改于 ${fmtAgo(s.latest_mtime)}` : `${className(cls ?? "other")} · 修改于 ${fmtAgo(n.mtime)}`}
        </div>
      </div>
      <section className="ins-section">
        <h3>占比</h3>
        {s.parent && <ShareBar label={`占 ${s.parent.name.split(/[\\/]/).filter(Boolean).pop() ?? s.parent.name}`} part={n.alloc} whole={s.parent.alloc} />}
        {vol && used > 0 && <ShareBar label={`占 ${vol.mount_point.replace(/[\\/]+$/, "")} 已用`} part={n.alloc} whole={used} />}
      </section>
      {dir && s.top_files.length > 0 && (
        <section className="ins-section">
          <h3>最大的子项</h3>
          <div className="top-list">
            {s.top_files.slice(0, 5).map((t) => (
              <TopRow key={t.node.id} node={t.node} rel={t.rel_parent} tag={t.tag ?? null} whole={n.alloc} viewId={viewId} />
            ))}
          </div>
        </section>
      )}
      <section className="ins-section">
        <div className="row-between">
          <h3>标签</h3>
          <span className="muted small">Ctrl+1–4 · Ctrl+0 清除</span>
        </div>
        <div className="tag-grid" role="group" aria-label="颜色标签">
          {TAGS.map((t) => (
            <button
              key={t.color}
              aria-label={`${t.label}（Ctrl+${t.key}）`}
              aria-pressed={s.tag === t.color}
              className={`tag-btn${s.tag === t.color ? " on" : ""}`}
              onClick={() => void viewCall(viewId, "view.tag", { color: s.tag === t.color ? null : t.color })}
            >
              <span className={`tag-shape ${t.color}`} />
              <span className="mono muted">{t.key}</span>
            </button>
          ))}
        </div>
      </section>
    </>
  );
}

function ShareBar({ label, part, whole }: { label: string; part: number; whole: number }) {
  return (
    <div className="share">
      <div className="row-between small">
        <span className="muted">{label}</span>
        <span className="mono">
          <span className="strong">{fmtShare(part, whole)}</span> <span className="muted">/ {fmtSize(whole)}</span>
        </span>
      </div>
      <div className="bar">
        <div style={{ width: `${Math.min(100, whole ? (part / whole) * 100 : 0)}%` }} />
      </div>
    </div>
  );
}

function TopRow({ node, rel, tag, whole, viewId }: { node: NodeInfo; rel: string; tag: string | null; whole: number; viewId: string }) {
  const vocab = useApp((s) => s.vocab);
  const cls = vocab?.classes.find((c) => c.extensions.includes(node.ext))?.id ?? "other";
  return (
    <button className="top-row" onClick={() => void reveal(viewId, node.id)}>
      <span className="top-line">
        <span className="swatch" style={{ background: `var(--sk-treemap-class-${cls})` }} />
        <span className="top-name">{node.name}</span>
        {tag && <span className={`tag-mini ${tag}`} role="img" aria-label={`${tag} 标签`} />}
        <span className="flex" />
        <span className="mono">{fmtSize(node.alloc)}</span>
      </span>
      <span className="top-sub mono">
        <span>{rel}</span>
        <span>{fmtPct(node.alloc, whole)}</span>
      </span>
      <span className="top-bar">
        <span style={{ width: `${whole ? (node.alloc / whole) * 100 : 0}%`, background: `var(--sk-treemap-class-${cls})` }} />
      </span>
    </button>
  );
}

/** Locate a block: select it, navigating (with history) until it is drawn (UI-MAIN-40, UI-W3-33). */
export async function reveal(viewId: string, key: number): Promise<void> {
  await viewCall(viewId, "view.reveal", { key });
}

function ClassStats({ s, className }: { s: TreeSummaryResult; className: (id: string) => string }) {
  const total = s.classes.reduce((a, c) => a + c.bytes, 0);
  return (
    <section className="ins-section">
      <h3>{s.node.name.split(/[\\/]/).filter(Boolean).pop()} · 按文件类别</h3>
      {s.classes.map((c) => (
        <div key={c.class_id} className="class-row">
          <span className="swatch" style={{ background: `var(--sk-treemap-class-${c.class_id})` }} />
          <span className="flex">{className(c.class_id)}</span>
          <span className="mono muted">{fmtInt(c.files)} 个</span>
          <span className="mono">{fmtSize(c.bytes)}</span>
          <span className="mono muted share-pct">{fmtShare(c.bytes, total)}</span>
          <span className="class-bar">
            <span style={{ width: `${total ? (c.bytes / total) * 100 : 0}%`, background: `var(--sk-treemap-class-${c.class_id})` }} />
          </span>
        </div>
      ))}
    </section>
  );
}

function TopList({ s, viewId }: { s: TreeSummaryResult; viewId: string; className: (id: string) => string }) {
  return (
    <section className="ins-section">
      <h3>最大的 {s.top_files.length} 个文件</h3>
      <div className="top-list">
        {s.top_files.map((t) => (
          <TopRow key={t.node.id} node={t.node} rel={t.rel_parent} tag={t.tag ?? null} whole={s.node.alloc} viewId={viewId} />
        ))}
      </div>
    </section>
  );
}

function SyntheticDetails({ frameKind, size, count }: { frameKind: number; size: number; count: number }) {
  const [num, unit] = sizeParts(size);
  const text =
    frameKind === Kind.SMALL
      ? [`${fmtInt(count)} 个小项`, "同一文件夹里单独画不下的小文件合并成一块。双击可以把它们逐个铺开。"]
      : frameKind === Kind.MORE
        ? [`+${fmtInt(count)} 项`, "同级里画不下的条目合并成一块。双击可以把它们铺开。"]
        : frameKind === Kind.FREE
          ? ["可用空间", "系统报告的卷可用空间。按 F 显示或隐藏。"]
          : ["未知空间", "未知空间 = 系统报告的已用 − 已扫描合计，通常是无权限或系统保留的区域。以管理员身份扫描可以减少这部分（M4）。"];
  return (
    <>
      <div className="ins-name">
        <h2>{text[0]}</h2>
      </div>
      <div className="big-num">
        <span className="num">{num}</span>
        <span className="unit">{unit}</span>
      </div>
      <p className="muted">{text[1]}</p>
    </>
  );
}
