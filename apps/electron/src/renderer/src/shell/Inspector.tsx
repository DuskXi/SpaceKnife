// Details panel (UI-MAIN-32…47, G11): details / class statistics / largest files of the selection.

import { type CSSProperties, useEffect, useState } from "react";

import type { ClassStat, FolderClassInfo, NodeInfo, TreeSummaryResult } from "@/protocol/generated";
import { basketAdd, cancellable, editFilter, viewCall } from "@/state/actions";
import { patchView, readFlag, set, toast, useApp, writeFlag } from "@/state/store";
import { Kind } from "@/visual/frame";
import { fmtAgo, fmtCount, fmtInt, fmtPct, fmtShare, fmtSize, plural, sizeParts } from "@/lib/format";
import { summaryInterval, useThrottled } from "@/lib/throttle";
import { isRunning } from "@/shell/ScanProgress";
import { Icon, Kbd } from "@/ui/Icon";
import { classHexNow } from "@/lib/classes";
import { byLang, t } from "@/lib/i18n";
import { tagLabel } from "@/lib/tags";
import { inBrowser } from "@/lib/host";
import { runShortcut } from "@/state/commands";
import { InspectorFields } from "@/plugins/fields";
import { PluginIcon } from "@/plugins/icons";
import { InsightCards } from "@/plugins/insight";
import { InspectorPluginPage, inspectorPages } from "@/plugins/slots";

/** The built-in pages; a plugin's is "plugin:<its full id>" (slot "inspector"). */
type Page = "details" | "classes" | "largest";
const BUILTIN_PAGES: readonly string[] = ["details", "classes", "largest"];
// red ▲, yellow ■, green ●, blue ◆: the names come from tagLabel (W7 「标签」 · 显示名)
const TAGS = [
  { color: "red", key: 1 },
  { color: "yellow", key: 2 },
  { color: "green", key: 3 },
  { color: "blue", key: 4 },
] as const;
const pageLabel = (p: Page) => ({ details: t("详情"), classes: t("类别统计"), largest: t("最大文件") })[p];

export function Inspector({ viewId }: { viewId: string }) {
  const view = useApp((s) => s.views[viewId]);
  const vocab = useApp((s) => s.vocab);
  const plugins = useApp(inspectorPages);
  const stored = useApp((s) => s.inspectorPage);
  // a plugin page whose plugin went away: back to the details
  const page = BUILTIN_PAGES.includes(stored) || plugins.some((p) => `plugin:${p.id}` === stored) ? stored : "details";
  const pluginPage = page.startsWith("plugin:") ? page.slice(7) : null;
  const setPage = (p: string) => set({ inspectorPage: p });
  const commands = useApp((s) => s.commands);
  const extra = commands.filter((c) => c.menus.includes("inspector"));
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
  // every frame may change the totals, but a summary walks the whole subtree: refresh it at most
  // once a second while the scan runs (four frames a second), and the less often the larger the
  // subtree (live monitoring of a busy disk changes something every few seconds); a request the
  // next one replaces is dropped
  const items = summary && summary.node.id === node ? summary.node.files + summary.node.dirs : 0;
  const tick = useThrottled(seq, summaryInterval(items, running));

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
    <aside className="inspector" aria-label={t("详情面板")}>
      <div className="inspector-head">
        <div className="segtabs" role="tablist" aria-label={t("侧栏视图")}>
          {(["details", "classes", "largest"] as const).map((p) => (
            <button key={p} role="tab" aria-selected={page === p} className={page === p ? "on" : ""} onClick={() => setPage(p)}>
              {pageLabel(p)}
            </button>
          ))}
          {plugins.map((p) => {
            const id = `plugin:${p.id}`;
            return (
              <button key={id} role="tab" aria-selected={page === id} aria-label={p.title} title={p.title} className={`plugin-tab${page === id ? " on" : ""}`} onClick={() => setPage(id)}>
                <PluginIcon plugin={p.plugin} name={p.icon} size={14} />
              </button>
            );
          })}
        </div>
        <div className="inspector-tools">
          <button
            className="icon-btn small"
            aria-label={t("更多操作")}
            title={t("更多操作（Shift+F10）")}
            onClick={() => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "F10", shiftKey: true, bubbles: true }))}
          >
            <Icon name="more" />
          </button>
          <button className="icon-btn small" aria-label={t("隐藏详情面板")} title={t("隐藏详情面板")} onClick={() => set({ inspectorHidden: true })}>
            <Icon name="sidebar" />
          </button>
        </div>
      </div>
      <div className="inspector-body">
        {pluginPage ? (
          <InspectorPluginPage id={pluginPage} viewId={viewId} />
        ) : synthetic != null && frame ? (
          <SyntheticDetails frameKind={frame.kind[synthetic]!} size={Number(frame.size[synthetic])} count={frame.count_[synthetic]!} />
        ) : !summary ? (
          <div className="muted">{t("正在读取…")}</div>
        ) : page === "details" ? (
          <>
            {scanId && <InsightCards scanId={scanId} node={summary.node.id} tick={tick} />}
            <Details s={summary} viewId={viewId} className={className} />
            {scanId && <InspectorFields scanId={scanId} viewId={viewId} node={summary.node.id} tick={tick} />}
          </>
        ) : page === "classes" ? (
          <ClassStats s={summary} viewId={viewId} className={className} />
        ) : (
          <TopList s={summary} viewId={viewId} className={className} />
        )}
      </div>
      {summary && synthetic == null && !pluginPage && (
        <div className="inspector-actions">
          {!inBrowser() && (
            <ActionRow icon="reveal" label={revealLabel()} keys="Ctrl+E" disabled={!summary.on_disk} onClick={() => window.sk.shell.showItemInFolder(summary.path)} />
          )}
          <ActionRow
            icon="copy"
            label={t("复制路径")}
            keys="Ctrl+C"
            onClick={() => {
              window.sk.shell.copyText(summary.path);
              toast(t("已复制"));
            }}
          />
          <ActionRow icon="basket" danger label={t("加入删除篮")} keys="Delete" disabled={!summary.on_disk} onClick={() => scanId && basketAdd(scanId, summary.node.id)} />
          {extra.map((c) => (
            // plugins' commands for the selection (menus "inspector"): they run where the palette would
            <button key={c.id} className="action-row" onClick={() => void runShortcut(c.id)} title={c.status || undefined}>
              <span className="muted">
                <PluginIcon plugin={c.plugin} name={c.icon} />
              </span>
              <span className="flex">{c.name}</span>
              {c.keys[0] && <Kbd>{c.keys[0]}</Kbd>}
            </button>
          ))}
          {!summary.on_disk && <div className="muted small inspector-note">{notOnDisk()}</div>}
        </div>
      )}
    </aside>
  );
}

export const revealLabel = () =>
  window.sk.platform === "darwin" ? t("在访达中显示") : window.sk.platform === "win32" ? t("在资源管理器中显示") : t("在文件管理器中显示");

/** Items under the MFT pseudo folders have no folder of their own to open or delete from. */
export const notOnDisk = () => t("NTFS 元数据与孤立文件只在 MFT 快速扫描里可见，没有可以打开或删除的对应位置");

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
  const settings = useApp((st) => st.settings); // tag display names (W7 标签)
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
        <div className="ins-icon" style={cls ? { background: classHexNow(cls), color: "#0A0B0D" } : undefined}>
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
        <div className="ins-stats" title={t("精确 {alloc} 字节（逻辑 {size} 字节）", { alloc: fmtInt(n.alloc), size: fmtInt(n.size) })}>
          {dir
            ? (() => {
                const fields = {
                  files: fmtInt(n.files),
                  dirs: fmtInt(n.dirs),
                  ago: fmtAgo(s.latest_mtime),
                };
                const text = t(
                  "{files} 个文件 · {dirs} 个文件夹 · 修改于 {ago}",
                  fields,
                );
                const one =
                  n.files === 1 && n.dirs === 1
                    ? "{files} file · {dirs} folder · modified {ago}"
                    : n.files === 1
                      ? "{files} file · {dirs} folders · modified {ago}"
                      : "{files} files · {dirs} folder · modified {ago}";

                return n.files === 1 || n.dirs === 1
                  ? plural(1, text, one, fields)
                  : text;
            })()
          : t("{cls} · 修改于 {ago}", {
              cls: className(cls ?? "other"),
              ago: fmtAgo(n.mtime),
            })}
        </div>
        {s.folder_class && <FolderClass info={s.folder_class} className={className} />}
      </div>
      <section className="ins-section">
        <h3>{t("占比")}</h3>
        {s.parent && <ShareBar label={t("占 {name}", { name: s.parent.name.split(/[\\/]/).filter(Boolean).pop() ?? s.parent.name })} part={n.alloc} whole={s.parent.alloc} />}
        {vol && used > 0 && <ShareBar label={t("占 {volume} 已用", { volume: vol.mount_point.replace(/[\\/]+$/, "") })} part={n.alloc} whole={used} />}
      </section>
      {dir && s.top_files.length > 0 && (
        <section className="ins-section">
          <h3>{t("最大的子项")}</h3>
          <div className="top-list">
            {s.top_files.slice(0, 5).map((f) => (
              <TopRow key={f.node.id} node={f.node} rel={f.rel_parent} tag={f.tag ?? null} cls={f.class_id} whole={n.alloc} viewId={viewId} />
            ))}
          </div>
        </section>
      )}
      <section className="ins-section">
        <div className="row-between">
          <h3>{t("标签")}</h3>
          <span className="muted small">{t("Ctrl+1–4 · Ctrl+0 清除")}</span>
        </div>
        <div className="tag-grid" role="group" aria-label={t("颜色标签")}>
          {TAGS.map((tg) => {
            const name = t("{tag}（Ctrl+{key}）", { tag: tagLabel(settings, tg.color), key: tg.key });
            return (
              <button
                key={tg.color}
                aria-label={name}
                title={name}
                aria-pressed={s.tag === tg.color}
                className={`tag-btn${s.tag === tg.color ? " on" : ""}`}
                onClick={() => void viewCall(viewId, "view.tag", { color: s.tag === tg.color ? null : tg.color })}
              >
                <span className={`tag-shape ${tg.color}`} />
                <span className="mono muted">{tg.key}</span>
              </button>
            );
          })}
        </div>
      </section>
    </>
  );
}

/** The folder class the selection takes (UI-W7-38), and what decided it; changed from the menu. */
function FolderClass({ info, className }: { info: FolderClassInfo; className: (id: string) => string }) {
  const cls = className(info.class_id);
  const text = info.fallback
    ? t("其中没有类别的文件算作「{cls}」 · {reason}", { cls, reason: info.reason })
    : t("整个文件夹算作「{cls}」 · {reason}", { cls, reason: info.reason });
  return (
    <div className="ins-fclass" title={t("由 {folder} 决定。右键菜单的「归类」可以更改", { folder: info.folder })}>
      <span className="swatch" style={{ background: classHexNow(info.class_id) }} />
      <span className="ins-fclass-text">{text}</span>
    </div>
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

// `cls` comes from the backend: by extension, and by size for ts / mts
function TopRow(props: { node: NodeInfo; rel: string; tag: string | null; cls: string; whole: number; viewId: string }) {
  const { node, rel, tag, cls, whole, viewId } = props;
  return (
    <button className="top-row" onClick={() => void reveal(viewId, node.id)}>
      <span className="top-line">
        <span className="swatch" style={{ background: classHexNow(cls) }} />
        <span className="top-name">{node.name}</span>
        {tag && <span className={`tag-mini ${tag}`} role="img" aria-label={t("{tag} 标签", { tag })} />}
        <span className="flex" />
        <span className="mono">{fmtSize(node.alloc)}</span>
      </span>
      <span className="top-sub mono">
        <span>{rel}</span>
        <span>{fmtPct(node.alloc, whole)}</span>
      </span>
      <span className="top-bar">
        <span style={{ width: `${whole ? (node.alloc / whole) * 100 : 0}%`, background: classHexNow(cls) }} />
      </span>
    </button>
  );
}

/** Locate a block: select it, navigating (with history) until it is drawn (UI-MAIN-40, UI-W3-33). */
export async function reveal(viewId: string, key: number): Promise<void> {
  await viewCall(viewId, "view.reveal", { key });
}

// SpaceKnife's own classes are niche: listed only when they hold a real share of the scope, the
// rest behind 「显示全部类别」 (UI-MAIN-65; the composition panel's rule, without its folding).
const NICHE = new Set(["code", "data"]);
const NICHE_SHARE = 0.02;
const NICHE_BYTES = 8 * 2 ** 30;
const ALL_KEY = "sk.classStatsAll";

/** 「类别统计」 (UI-MAIN-65): the scope's make-up as one stacked bar, then a row per class. */
function ClassStats({ s, viewId, className }: { s: TreeSummaryResult; viewId: string; className: (id: string) => string }) {
  const [all, setAll] = useState(() => readFlag(ALL_KEY, false));
  const [hot, setHotRow] = useState<string | null>(null);
  // the class under the pointer lights up in the picture (UI-MAIN-65), only within this folder
  const node = s.node.id;
  const setHot = (id: string | null) => {
    setHotRow(id);
    patchView(viewId, { hoverClass: id === null ? null : { id, node } });
  };
  useEffect(() => () => patchView(viewId, { hoverClass: null }), [viewId, node]);
  const total = s.classes.reduce((a, c) => a + c.bytes, 0);
  const files = s.classes.reduce((a, c) => a + c.files, 0);
  const minor = (c: ClassStat) => NICHE.has(c.class_id) && c.bytes < NICHE_SHARE * total && c.bytes < NICHE_BYTES;
  const folded = s.classes.filter(minor);
  const rows = all ? s.classes : s.classes.filter((c) => !minor(c));
  // columns as wide as their longest value (mono: 1ch a character), never narrower than the heading
  const widest = (f: (c: ClassStat) => string) => Math.max(5, ...rows.map((c) => f(c).length));
  const cols = {
    "--cs-files": `${widest((c) => fmtInt(c.files))}ch`,
    "--cs-files-short": `${widest((c) => fmtCount(c.files))}ch`,
    "--cs-size": `${widest((c) => fmtSize(c.bytes))}ch`,
  } as CSSProperties;
  const name = s.node.name.split(/[\\/]/).filter(Boolean).pop() ?? s.node.name;
  const describe = (c: ClassStat) => {
    const about = t("{name}：{files} 个文件，{size}，占 {share}", { name: className(c.class_id), files: fmtInt(c.files), size: fmtSize(c.bytes), share: fmtShare(c.bytes, total) });
    if (!c.parts.length) return about;
    // a folder class took files over (「游戏 · 其中视频 3 GB」)
    const parts = c.parts.slice(0, 5).map((p) => t("{name} {size}", { name: className(p.class_id), size: fmtSize(p.bytes) }));
    return t("{about}。其中按扩展名本是：{parts}", { about, parts: parts.join(byLang("、", ", ")) });
  };
  const toggle = () => {
    writeFlag(ALL_KEY, !all);
    setAll(!all);
  };
  return (
    <section className="cstat" style={cols}>
      <header className="cstat-head">
        <h3 className="cstat-title" title={s.path}>
          {name}
        </h3>
        <span className="cstat-total mono">{fmtSize(total)}</span>
      </header>
      <div className="cstat-sub">{plural(files, t("按文件类别 · {n} 个文件", { n: fmtInt(files) }), "By file class · 1 file")}</div>
      <div className="cstat-stack" role="img" aria-label={s.classes.map(describe).join(" · ")}>
        {s.classes
          .filter((c) => c.bytes > 0)
          .map((c) => (
            <span
              key={c.class_id}
              className={hot && hot !== c.class_id ? "dim" : undefined}
              title={describe(c)}
              style={{ flexGrow: c.bytes, background: classHexNow(c.class_id) }}
              onMouseEnter={() => setHot(c.class_id)}
              onMouseLeave={() => setHot(null)}
            />
          ))}
      </div>
      <div className="cstat-cols" aria-hidden="true">
        <span />
        <span>{t("类别")}</span>
        <span className="cstat-files">{t("文件数")}</span>
        <span className="cstat-size">{t("大小")}</span>
        <span className="cstat-pct">{t("占比")}</span>
      </div>
      <ul className="cstat-list">
        {rows.map((c) => {
          const hex = classHexNow(c.class_id);
          return (
            <li key={c.class_id}>
              <button
                className={`cstat-row${hot === c.class_id ? " hot" : ""}${minor(c) ? " minor" : ""}`}
                title={t("{about}。点击按这一类过滤", { about: describe(c) })}
                onClick={() => void editFilter(viewId, `:class:${className(c.class_id)}`, null, true)}
                onMouseEnter={() => setHot(c.class_id)}
                onMouseLeave={() => setHot(null)}
                onFocus={() => setHot(c.class_id)}
                onBlur={() => setHot(null)}
              >
                <span className="swatch" style={{ background: hex }} />
                <span className="cstat-name">{className(c.class_id)}</span>
                <span className="cstat-files mono">
                  <span className="full">{fmtInt(c.files)}</span>
                  <span className="short">{fmtCount(c.files)}</span>
                </span>
                <span className="cstat-size mono">{fmtSize(c.bytes)}</span>
                <span className="cstat-pct mono">{fmtShare(c.bytes, total)}</span>
                <span className="cstat-bar">
                  <span style={{ width: `${total ? (c.bytes / total) * 100 : 0}%`, background: hex }} />
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      {folded.length > 0 && (
        <button className="cstat-more" onClick={toggle} aria-pressed={all}>
          {all
            ? t("只显示主要类别")
            : t("显示全部类别（另有 {n} 个小众类别，共 {size}）", { n: folded.length, size: fmtSize(folded.reduce((a, c) => a + c.bytes, 0)) })}
        </button>
      )}
    </section>
  );
}

function TopList({ s, viewId }: { s: TreeSummaryResult; viewId: string; className: (id: string) => string }) {
  return (
    <section className="ins-section">
      <h3>{plural(s.top_files.length, t("最大的 {n} 个文件", { n: s.top_files.length }), "1 largest file")}</h3>
      <div className="top-list">
        {s.top_files.map((f) => (
          <TopRow key={f.node.id} node={f.node} rel={f.rel_parent} tag={f.tag ?? null} cls={f.class_id} whole={s.node.alloc} viewId={viewId} />
        ))}
      </div>
    </section>
  );
}

function SyntheticDetails({ frameKind, size, count }: { frameKind: number; size: number; count: number }) {
  const [num, unit] = sizeParts(size);
  const text =
    frameKind === Kind.SMALL
      ? [plural(count, t("{n} 个小项", { n: fmtInt(count) }), "1 small item"), t("同一文件夹里单独画不下的小文件合并成一块。双击可以把它们逐个铺开。")]
      : frameKind === Kind.MORE
        ? [t("+{n} 项", { n: fmtInt(count) }), t("同级里画不下的条目合并成一块。双击可以把它们铺开。")]
        : frameKind === Kind.FREE
          ? [t("可用空间"), t("系统报告的卷可用空间。按 F 显示或隐藏。")]
          : [t("未知空间"), t("未知空间 = 系统报告的已用 − 已扫描合计，通常是无权限或系统保留的区域。以管理员身份扫描可以减少这部分（M4）。")];
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
