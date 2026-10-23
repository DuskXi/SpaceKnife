// The 「重复文件」 drawer (docs/dupes.md §10.5): the plan, the search's funnel and progress, the
// space each volume gets back, the keep rules, the verified groups (largest gain first) or the
// folders holding the most extra copies, and 「把多余的副本加入删除篮」. It follows the tab's scan,
// like the delete basket, and comes after it in the side column.

import { useEffect } from "react";

import type { DupeGroup, DupeMember, DupesFolderInfo, DupesPlan, DupesStatus } from "@/protocol/generated";
import {
  applyKeep,
  cancelSearch,
  cannotSearch,
  clearSearch,
  focusGroup,
  folderLabel,
  isRunning,
  keepOptions,
  keepThis,
  LIT_MAX,
  loadCoverage,
  loadFolders,
  loadGroups,
  loadPlan,
  type DupesLoadLevel,
  type DupesSort,
  type KeepRule,
  pauseSearch,
  PAGE,
  revealCopy,
  ruleLabel,
  setCross,
  setDupes,
  setListing,
  setLoad,
  startSearch,
  toBasket,
  useDupes,
} from "@/state/dupes";
import { activeTab, patchView, setting, useApp } from "@/state/store";
import { fmtAgo, fmtInt, fmtSize, middlePath, plural, sizeParts } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const LOADS: DupesLoadLevel[] = ["idle", "quiet", "balanced", "max"];
const loadLabel = (l: DupesLoadLevel): string => ({ idle: t("空闲"), quiet: t("安静"), balanced: t("均衡"), max: t("最大") })[l];
const RULES: KeepRule[] = ["oldest", "newest", "shortest"];
const SORTS: DupesSort[] = ["reclaim", "size", "copies", "path"];
const sortLabel = (s: DupesSort): string => ({ reclaim: t("可腾出"), size: t("单份大小"), copies: t("份数"), path: t("路径") })[s];

export const stateLabel = (s: DupesStatus): string =>
  ({
    planning: t("准备中"),
    identifying: t("核对身份"),
    probing: t("读头尾"),
    verifying: t("核对内容"),
    waiting_scan: t("等 {disk} 的扫描结束", { disk: s.waiting_for }),
    paused: t("已暂停"),
    done: t("完成"),
    cancelled: t("已停止"),
    failed: t("出错：{error}", { error: s.error }),
  })[s.state];

export const roleLabel = (role: DupeMember["role"]): string =>
  ({
    keep: t("保留"),
    extra: t("多余"),
    alias: t("同一个文件"),
    clone: t("同一份数据"),
    needed: t("程序需要"),
    other_volume: t("在别的卷上"),
    pinned: t("只能保留"),
  })[role];

export function DupesDrawer() {
  const tab = useApp((s) => activeTab(s));
  const scanId = tab.scanId ?? "";
  // re-rendered when the scan finishes or is a snapshot
  const scanState = useApp((s) => (scanId ? `${s.scans[scanId]?.state}|${s.scans[scanId]?.method}` : ""));
  const status = useDupes((s) => (scanId ? (s.byScan[scanId] ?? null) : null));
  const plan = useDupes((s) => (scanId ? (s.plan[scanId] ?? null) : null));
  const view = useDupes((s) => s.tab);
  const sort = useDupes((s) => s.sort);
  const jobId = status?.job_id ?? null;
  const why = scanState ? cannotSearch(scanId) : cannotSearch(null);
  useEffect(() => {
    if (!jobId) {
      if (scanId) void loadPlan(scanId);
      return;
    }
    void loadGroups(jobId, "reset");
  }, [jobId, scanId, sort]);
  useEffect(() => {
    if (jobId && view === "folders") void loadFolders(jobId, scanId);
  }, [jobId, view, scanId]);
  const limited = status && status.node != null && (status.mode === "against" || status.mode === "inside") ? status : null;
  const asked = useDupes((s) => (jobId ? (s.coverage[jobId] ?? null) : null));
  useEffect(() => {
    if (limited && !isRunning(limited)) void loadCoverage(limited);
  }, [limited?.job_id, limited?.state]); // eslint-disable-line react-hooks/exhaustive-deps
  const covered = limited ?? (status && asked ? status : null);

  return (
    <aside className="basket-drawer dupes-drawer" aria-label={t("重复文件")}>
      <div className="bk-summary">
        <div className="bk-title">
          <span className="dp-icon">
            <Icon name="dupes" />
          </span>
          <strong>{t("重复文件")}</strong>
          <span className="flex" />
          {status && !isRunning(status) && (
            <button className="ghost small" title={t("清除这次查找的结果和配色")} onClick={() => void clearSearch(status.job_id)}>
              {t("清除结果")}
            </button>
          )}
          <button className="icon-btn" title={t("关闭")} aria-label={t("关闭重复文件")} onClick={() => setDupes({ open: false })}>
            <Icon name="close" />
          </button>
        </div>
        {status ? <Scope s={status} /> : null}
        {status ? <Summary s={status} scanId={scanId} /> : why ? <div className="muted small dp-why">{why}</div> : <PlanView plan={plan} scanId={scanId} />}
        {covered && <Coverage s={covered} />}
      </div>
      {status && (
        <>
          <KeepRules s={status} />
          <div className="bk-listhead dp-tabs" role="tablist">
            <button role="tab" aria-selected={view === "groups"} className={view === "groups" ? "on" : ""} onClick={() => setDupes({ tab: "groups" })}>
              {t("组")}
            </button>
            <button role="tab" aria-selected={view === "folders"} className={view === "folders" ? "on" : ""} onClick={() => setDupes({ tab: "folders" })}>
              {t("文件夹")}
            </button>
            <span className="flex" />
            {view === "groups" && <ListOptions s={status} />}
          </div>
          <LitNote viewIds={tab.viewIds} />
          {view === "groups" ? <GroupList s={status} scanId={scanId} viewIds={tab.viewIds} focused={tab.viewIds[tab.focusedPane]} /> : <FolderList viewId={tab.viewIds[tab.focusedPane]} />}
          <Actions s={status} />
        </>
      )}
    </aside>
  );
}

/** What was compared: the whole scan, one folder, one file, or every open scan; and the lower bound. */
function Scope({ s }: { s: DupesStatus }) {
  const scans = useApp((x) => x.scans);
  const names = s.scan_ids.map((id) => scans[id]?.roots?.[0]?.path ?? id);
  const what =
    s.mode === "against"
      ? t("这个文件夹在别处有没有")
      : s.mode === "inside"
        ? t("这个文件夹里")
        : s.mode === "file"
          ? t("和一个文件大小相同的文件")
          : s.cross_volume
            ? t("跨盘：{list}", { list: names.join(" · ") })
            : t("整个扫描");
  return (
    <div className="muted small dp-scope">
      {what} · {t("不小于 {size}", { size: fmtSize(s.min_size) })}
    </div>
  );
}

function PlanView({ plan, scanId }: { plan: DupesPlan | null; scanId: string }) {
  const cross = useApp((s) => setting(s, "dupes.cross_volume", false));
  const scans = useApp((s) => Object.values(s.scans).filter((x) => x.state === "done" && x.source === "scan").length);
  return (
    <div className="dp-plan">
      {plan ? (
        <>
          <div className="dp-big">
            <span className="mono">{fmtInt(plan.candidates)}</span> <span className="bk-unit">{t("个文件和别的文件大小相同")}</span>
          </div>
          <div className="muted small">
            {t("先读每个文件的头尾，共约 {probe}；只有几份还一样时才读完整个文件。大小独一无二的 {n} 个文件不读。", {
              probe: fmtSize(plan.probe_bytes),
              n: fmtInt(plan.excluded.unique ?? 0),
            })}
          </div>
          {plan.cross_extra && <div className="muted small">{t("跨盘多出 {n} 个候选，{size}", { n: fmtInt(plan.cross_extra.files ?? 0), size: fmtSize(plan.cross_extra.bytes ?? 0) })}</div>}
          {plan.last && (
            <div className="muted small dp-last">
              {t("上次的结果（{ago}）：{g} 组，可腾出 {size} · 这次先只核对文件有没有变过", {
                ago: fmtAgo(Date.parse(plan.last.at) / 1000),
                g: fmtInt(plan.last.groups),
                size: fmtSize(plan.last.reclaim),
              })}
            </div>
          )}
          <Excluded counts={plan.excluded} />
        </>
      ) : (
        <div className="muted small">{t("正在估计要读多少…")}</div>
      )}
      <label className="dp-check" title={scans < 2 ? t("至少要有两个打开的、扫完的扫描") : undefined}>
        <input type="checkbox" checked={cross} disabled={scans < 2 && !cross} onChange={(e) => void setCross(scanId, e.target.checked)} />
        <span>{t("跨盘比较：把所有打开的扫描放在一起比")}</span>
      </label>
      {cross && <div className="muted small">{t("跨盘的副本常常是有意的备份：每个卷默认至少留一份，别的卷上的副本不预选")}</div>}
      <button className="primary dp-start" disabled={!scanId || !plan} onClick={() => void startSearch(scanId)}>
        <Icon name="search" />
        {t("开始查找")}
      </button>
    </div>
  );
}

function Excluded({ counts }: { counts: Record<string, number> }) {
  const parts = [
    counts.small ? t("小于下限 {n}", { n: fmtInt(counts.small) }) : "",
    counts.cloud ? t("云占位 {n}（读内容会触发下载）", { n: fmtInt(counts.cloud) }) : "",
    counts.system ? t("系统和程序文件夹 {n}", { n: fmtInt(counts.system) }) : "",
    counts.bins ? t("回收站 {n}", { n: fmtInt(counts.bins) }) : "",
    counts.network ? t("网络盘 {n}", { n: fmtInt(counts.network) }) : "",
  ].filter(Boolean);
  if (!parts.length) return null;
  return <div className="muted small dp-excluded">{t("没有比较：{list}", { list: parts.join(" · ") })}</div>;
}

function Summary({ s, scanId }: { s: DupesStatus; scanId: string }) {
  const running = isRunning(s);
  const [num, unit] = sizeParts(s.reclaim);
  const f = s.funnel;
  const pct = Math.round(100 * (s.progress ?? 0));
  const skipped = Object.values(s.skipped_by_reason).reduce((a, b) => a + b, 0);
  return (
    <>
      <div className="bk-free">
        <div className="muted small">
          <span className="bk-legend dp-legend" /> {t("可腾出")}
        </div>
        <div className="bk-big">
          <span className="mono">{num}</span> <span className="bk-unit">{unit}</span>
        </div>
        <div className="muted small">{plural(s.extra, t("{n} 份多余的副本 · {g} 组", { n: fmtInt(s.extra), g: fmtInt(s.groups) }), `1 extra copy · ${fmtInt(s.groups)} groups`)}</div>
      </div>
      {(s.ledger.length > 1 || s.ledger.some((v) => v.shared > 0)) && (
        <div className="dp-ledger" aria-label={t("按卷")}>
          {s.ledger.map((v) => (
            <div key={v.volume} className="dp-ledger-row">
              <span className="mono">{v.volume}</span>
              <span className="muted">{v.shared > 0 ? t("同一份数据 {size}（删了不腾空间）", { size: fmtSize(v.shared) }) : ""}</span>
              <span className="mono dp-reclaim">{fmtSize(v.reclaim)}</span>
            </div>
          ))}
        </div>
      )}
      {s.ledger
        .filter((v) => v.note)
        .map((v) => (
          <div key={`note-${v.volume}`} className="dp-warn small">
            <Icon name="info" size={13} /> {v.volume} · {v.note}
          </div>
        ))}
      <div className="dp-funnel small" aria-label={t("查找进度")}>
        <div className="dp-step">
          <span>{t("同样大小（不读盘）")}</span>
          <span className="mono">{fmtInt(f.candidates)}</span>
        </div>
        <div className="dp-step">
          <span>{t("同一个文件合并后")}</span>
          <span className="mono">{fmtInt(f.physical)}</span>
        </div>
        {f.aliases + f.clones > 0 && <div className="dp-step muted">{t("其中同一个文件的别名 {a} · 块克隆 {c}（不读内容）", { a: fmtInt(f.aliases), c: fmtInt(f.clones) })}</div>}
        <div className="dp-step">
          <span>{t("读了头尾")}</span>
          <span className="mono">{fmtInt(f.probed)}</span>
        </div>
        <div className="dp-step dp-strong">
          <span>{t("内容相同（已读完全文核对）")}</span>
          <span className="mono">{t("{g} 组 · {n} 个文件", { g: fmtInt(f.confirmed_groups), n: fmtInt(f.confirmed_files) })}</span>
        </div>
        <div className="muted">
          {t("已读 {read}（候选的 {pct}）· 缓存认出 {hits} 个", {
            read: fmtSize(s.bytes.read),
            pct: s.bytes.full_upper ? `${((100 * s.bytes.read) / s.bytes.full_upper).toFixed(1)}%` : "—",
            hits: fmtInt(s.bytes.cache_hits),
          })}
        </div>
        {skipped > 0 && (
          <div className="muted">
            {t("没有比较 {n} 个：{list}", {
              n: fmtInt(skipped),
              list: Object.entries(s.skipped_by_reason)
                .map(([k, v]) => `${skipLabel(k)} ${fmtInt(v)}`)
                .join(" · "),
            })}
          </div>
        )}
        {s.stale_groups > 0 && !running && (
          <div className="dp-warn">
            <Icon name="info" size={13} />
            {t("{n} 组因为文件变了退回「待核对」，不会被删；「再查一次」只读变过的文件", { n: fmtInt(s.stale_groups) })}
          </div>
        )}
        {s.atime_moved_by_others > 0 && (
          <div className="dp-warn">
            <Icon name="warning" size={13} />
            {t("读取后有 {n} 个文件的访问时间变了。SpaceKnife 自己的读取不改访问时间，多半是杀毒软件在打开时检查文件", { n: fmtInt(s.atime_moved_by_others) })}
          </div>
        )}
      </div>
      <div className="dp-run">
        <div className="dp-bar" role="progressbar" aria-label={t("查找进度")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
          <span style={{ width: `${running ? pct : 100}%` }} />
        </div>
        <div className="dp-run-row small">
          <span>{stateLabel(s)}</span>
          {running && <span className="mono">{pct}%</span>}
          {running && s.rate > 0 && <span className="muted mono">{t("{rate}/秒", { rate: fmtSize(s.rate) })}</span>}
          {running && s.eta_max_s != null && <span className="muted">{t("最多还要 {m} 分钟", { m: String(Math.max(1, Math.ceil(s.eta_max_s / 60))) })}</span>}
          <span className="flex" />
          {!running && s.mode === "all" && scanId && (
            <button className="ghost small dp-again" title={t("没变过的文件不再读内容")} onClick={() => void startSearch(scanId)}>
              <Icon name="refresh" size={13} />
              {t("再查一次")}
            </button>
          )}
          {running && (
            <>
              <button className="icon-btn" title={s.state === "paused" ? t("继续") : t("暂停")} aria-label={s.state === "paused" ? t("继续") : t("暂停")} onClick={() => void pauseSearch(s.job_id, s.state !== "paused")}>
                <Icon name={s.state === "paused" ? "play" : "pause"} />
              </button>
              <button className="icon-btn" title={t("停止（已核对的留下）")} aria-label={t("停止")} onClick={() => void cancelSearch(s.job_id)}>
                <Icon name="stop" />
              </button>
            </>
          )}
        </div>
        {running && (
          <div className="segmented small dp-levels" role="radiogroup" aria-label={t("读盘负载")}>
            {LOADS.map((l) => (
              <button key={l} role="radio" aria-checked={s.load === l} className={s.load === l ? "on" : ""} onClick={() => s.load !== l && void setLoad(s.job_id, l)}>
                {loadLabel(l)}
              </button>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

export const skipLabel = (k: string): string =>
  ({
    gone: t("已不在"),
    denied: t("没有权限"),
    not_file: t("不是普通文件"),
    cloud: t("云占位"),
    surrogate: t("链接"),
    ambiguous_name: t("只差大小写"),
    changed: t("核对期间变了"),
    in_use: t("被独占"),
    atime: t("保留不了访问时间"),
    read_error: t("读取出错"),
    unnameable: t("名字不能用"),
    stopped: t("没来得及核对"),
  })[k] ?? k;

/** 「这个文件夹在别处有没有」: a bar by bytes (§8.4) — for the user to judge, never a delete. */
export function Coverage({ s }: { s: DupesStatus }) {
  const cover = useDupes((x) => x.coverage[s.job_id] ?? null);
  if (!cover) return isRunning(s) ? <div className="muted small">{t("核对完后这里给出这个文件夹有多少在别处也有")}</div> : null;
  return <CoverageBar info={cover.info} label={folderLabel(cover.node)} onClose={s.mode === "all" ? () => setDupes({ coverage: dropKey(useDupes.getState().coverage, s.job_id) }) : undefined} />;
}

function dropKey<T>(o: Record<string, T>, key: string): Record<string, T> {
  const out = { ...o };
  delete out[key];
  return out;
}

export function CoverageBar({ info, label, onClose }: { info: DupesFolderInfo; label?: string; onClose?: () => void }) {
  const parts = [
    { id: "elsewhere", bytes: info.elsewhere_bytes, n: info.elsewhere, label: t("在别处有一份") },
    { id: "only", bytes: info.only_here_bytes, n: info.only_here, label: t("只在这里") },
    { id: "same", bytes: info.same_data_bytes, n: info.same_data, label: t("同一份数据") },
    { id: "unchecked", bytes: info.unchecked_bytes + info.small_bytes, n: info.unchecked + info.small, label: t("没核对") },
  ];
  const total = parts.reduce((a, p) => a + p.bytes, 0);
  return (
    <div className="dp-cover">
      <div className="dp-cover-head small">
        <strong>{t("这个文件夹在别处有没有")}</strong>
        {label && <span className="mono dp-path">{label}</span>}
        <span className="flex" />
        {onClose && (
          <button className="icon-btn small" title={t("关闭")} aria-label={t("关闭")} onClick={onClose}>
            <Icon name="close" size={12} />
          </button>
        )}
      </div>
      <div className="dp-cover-bar" role="img" aria-label={parts.map((p) => `${p.label} ${fmtSize(p.bytes)}`).join(" · ")}>
        {parts.map((p) => (p.bytes > 0 ? <span key={p.id} className={`dp-c-${p.id}`} style={{ flexGrow: total ? p.bytes / total : 0 }} /> : null))}
      </div>
      <div className="dp-cover-legend small">
        {parts.map((p) => (
          <span key={p.id}>
            <span className={`bk-legend dp-c-${p.id}`} />
            {p.label} <span className="mono">{fmtSize(p.bytes)}</span>
          </span>
        ))}
      </div>
      {info.small > 0 && <div className="muted small">{t("其中 {n} 个小于下限，没有核对", { n: fmtInt(info.small) })}</div>}
      {info.only_here_list.length > 0 && (
        <details className="dp-only small">
          <summary>{t("只在这里的最大的 {n} 个", { n: fmtInt(info.only_here_list.length) })}</summary>
          {info.only_here_list.slice(0, 50).map((x) => (
            <div key={x.node} className="dp-only-row">
              <span className="mono dp-path" title={x.path}>
                {middlePath(x.path, 44)}
              </span>
              <span className="mono">{fmtSize(x.alloc)}</span>
            </div>
          ))}
        </details>
      )}
      <div className="muted small">{t("删除整个文件夹不在这里做：照这条判断，或者把多余的副本加入删除篮")}</div>
    </div>
  );
}

function KeepRules({ s }: { s: DupesStatus }) {
  const rule = useApp((a) => setting(a, "dupes.keep_rule", "oldest")) as KeepRule;
  const folder = useApp((a) => setting(a, "dupes.folder_consistent", true));
  const perVolume = useApp((a) => setting(a, "dupes.cross_keep_per_volume", true));
  return (
    <div className="dp-keep small">
      <label>
        {t("保留")}
        <select className="bk-sort" value={rule} aria-label={t("保留哪一份")} onChange={(e) => void applyKeep(s.job_id, { rule: e.target.value as KeepRule })}>
          {RULES.map((r) => (
            <option key={r} value={r}>
              {ruleLabel(r)}
            </option>
          ))}
        </select>
      </label>
      <label className="dp-check" title={t("两个文件夹里有好几组副本时，保留的都在同一边：删完后另一边可以整个清空")}>
        <input type="checkbox" checked={folder} onChange={(e) => void applyKeep(s.job_id, { folderConsistent: e.target.checked })} />
        <span>{t("同一对文件夹留在同一边")}</span>
      </label>
      {s.cross_volume && (
        <label className="dp-check">
          <input type="checkbox" checked={perVolume} onChange={(e) => void applyKeep(s.job_id, { perVolume: e.target.checked })} />
          <span>{t("每个卷至少留一份")}</span>
        </label>
      )}
    </div>
  );
}

function ListOptions({ s }: { s: DupesStatus }) {
  const sort = useDupes((x) => x.sort);
  const crossOnly = useDupes((x) => x.crossOnly);
  const shared = useDupes((x) => x.includeShared);
  return (
    <>
      {s.cross_volume && (
        <label className="dp-check" title={t("只看副本在不同卷上的组")}>
          <input type="checkbox" checked={crossOnly} onChange={(e) => setListing({ crossOnly: e.target.checked }, s.job_id)} />
          <span>{t("只看跨盘")}</span>
        </label>
      )}
      <label className="dp-check" title={t("也列出同一个文件的几个名字和块克隆：它们不占双份空间，删了不腾空间")}>
        <input type="checkbox" checked={shared} onChange={(e) => setListing({ includeShared: e.target.checked }, s.job_id)} />
        <span>{t("同一份数据")}</span>
      </label>
      <select className="bk-sort" value={sort} aria-label={t("排序")} onChange={(e) => setListing({ sort: e.target.value as DupesSort }, s.job_id)}>
        {SORTS.map((x) => (
          <option key={x} value={x}>
            {sortLabel(x)}
          </option>
        ))}
      </select>
    </>
  );
}

/** More copies than are lit at once. */
function LitNote({ viewIds }: { viewIds: string[] }) {
  const truncated = useDupes((x) => viewIds.some((v) => x.located[v]?.truncated));
  return truncated ? <div className="muted small dp-litnote">{t("只点亮最大的 {n} 份", { n: fmtInt(LIT_MAX) })}</div> : null;
}

const NO_GROUPS: DupeGroup[] = [];

function GroupList({ s, scanId, viewIds, focused }: { s: DupesStatus; scanId: string; viewIds: string[]; focused: string | undefined }) {
  const groups = useDupes((x) => (x.groupsJob === s.job_id ? x.groups : NO_GROUPS));
  const total = useDupes((x) => (x.groupsJob === s.job_id ? x.groupsTotal : 0));
  const lit = useDupes((x) => (x.focus?.jobId === s.job_id ? x.focus.group : -1));
  return (
    <div className="bk-list dp-list" role="list" aria-label={t("内容相同的文件")} onMouseLeave={() => focusGroup(null, null)}>
      {groups.length === 0 && <div className="muted bk-empty">{isRunning(s) ? t("核对完的组会出现在这里，不必等全部做完") : t("没有找到内容相同的文件")}</div>}
      {groups.map((g) => (
        <GroupRow key={g.id} g={g} jobId={s.job_id} scanId={scanId} viewId={focused} lit={g.id === lit} onHover={() => focusGroup(s.job_id, g.id, viewIds)} />
      ))}
      {groups.length < total && (
        <button className="ghost dp-more" onClick={() => void loadGroups(s.job_id, "more")}>
          {t("再显示 {n} 组", { n: fmtInt(Math.min(PAGE, total - groups.length)) })}
        </button>
      )}
    </div>
  );
}

export function groupName(g: DupeGroup): string {
  const first = g.members[0];
  const name = first ? (first.path.split(/[\\/]/).pop() ?? first.path) : "";
  return g.names > 1 ? t("{name} 等 {n} 个名字", { name, n: fmtInt(g.names) }) : name;
}

export const keepWhyText = (why: DupeGroup["keep_why"], rule: KeepRule): string =>
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
              : t("按「{rule}」保留", { rule: ruleLabel(rule) });

export function GroupRow({ g, jobId, scanId, viewId, lit, onHover }: { g: DupeGroup; jobId: string; scanId: string; viewId: string | undefined; lit: boolean; onHover: () => void }) {
  const confirmed = g.state === "confirmed";
  return (
    <div className={`bk-row dp-group${lit ? " lit" : ""}`} role="listitem" onMouseEnter={onHover}>
      <div className="bk-row-main">
        <span className={`bk-icon dp-icon${confirmed ? "" : " shared"}`}>
          <Icon name="dupes" />
        </span>
        <div className="bk-text">
          <span className="bk-name">{groupName(g)}</span>
          <span className="bk-path">
            {t("{n} 份 · 每份 {size}", { n: fmtInt(g.copies), size: fmtSize(g.size) })}
            {g.cross ? ` · ${g.volumes.join(" · ")}` : ""}
          </span>
        </div>
        <span className="bk-size">
          <span className="mono dp-reclaim">{confirmed ? fmtSize(g.reclaim) : "0 B"}</span>
          <span className="bk-path">{t("可腾出")}</span>
        </span>
      </div>
      <div className="dp-members">
        <div className="dp-verdict small">
          {confirmed ? t("内容相同（已读完全文核对）") : t("同一份数据（硬链接或块克隆）：删了不腾空间")}
          {confirmed && g.keep_why ? ` · ${keepWhyText(g.keep_why, keepOptions().rule)}` : ""}
        </div>
        {g.members.map((m) => (
          <div key={`${m.scan_id}:${m.path}`} className={`dp-member ${m.role}`}>
            <span className={`dp-role ${m.role}`}>{roleLabel(m.role)}</span>
            <button className="mono dp-path" title={m.scan_id === scanId ? t("在图里找到它：{path}", { path: m.path }) : m.path} disabled={m.scan_id !== scanId} onClick={() => void revealCopy(viewId, m.node)}>
              {middlePath(m.path, 48)}
            </button>
            {m.role === "extra" && (
              <button className="ghost dp-keepbtn" title={t("保留这一份，这组别的副本里挑多余的")} onClick={() => void keepThis(jobId, g.id, m.path)}>
                {t("保留这一份")}
              </button>
            )}
          </div>
        ))}
        {g.total_members > g.members.length && <div className="muted small">{t("另 {n} 份", { n: fmtInt(g.total_members - g.members.length) })}</div>}
      </div>
    </div>
  );
}

function FolderList({ viewId }: { viewId: string | undefined }) {
  const folders = useDupes((x) => x.folders);
  const total = useDupes((x) => x.foldersTotal);
  const hover = (node: number | null) => viewId && patchView(viewId, { hoverKey: node });
  return (
    <div className="bk-list dp-list" role="list" aria-label={t("多余的副本最多的文件夹")} onMouseLeave={() => hover(null)}>
      {folders.length === 0 && <div className="muted bk-empty">{t("还没有文件夹里有多余的副本")}</div>}
      {folders.map((f) => (
        <button key={f.node} className="bk-row dp-folder" role="listitem" title={t("在图里找到它：{path}", { path: f.path })} onMouseEnter={() => hover(f.node)} onClick={() => void revealCopy(viewId, f.node)}>
          <div className="bk-row-main">
            <span className="bk-icon">
              <Icon name="folder" />
            </span>
            <div className="bk-text">
              <span className="bk-name mono">{middlePath(f.path, 44)}</span>
              <div className="dp-share" aria-hidden="true">
                <span style={{ width: `${Math.min(100, f.alloc ? (100 * f.redundant) / f.alloc : 0)}%` }} />
              </div>
            </div>
            <span className="bk-size">
              <span className="mono dp-reclaim">{fmtSize(f.redundant)}</span>
              <span className="bk-path">{t("多余的副本")}</span>
            </span>
          </div>
        </button>
      ))}
      {total > folders.length && <div className="muted small bk-empty">{t("另 {n} 个文件夹", { n: fmtInt(total - folders.length) })}</div>}
    </div>
  );
}

function Actions({ s }: { s: DupesStatus }) {
  return (
    <div className="bk-actions">
      <div className="muted small">{t("执行前会再核对每一份没有变过；保留的那份变了或也要删时，这一组跳过")}</div>
      <div className="bk-buttons">
        <button className="primary dp-commit" disabled={s.extra === 0} onClick={() => void toBasket(s.job_id)}>
          <Icon name="basket" />
          <span>{plural(s.extra, t("把多余的 {n} 份加入删除篮 · 可腾出 {size}", { n: fmtInt(s.extra), size: fmtSize(s.reclaim) }), `Put the extra copy in the delete basket · ${fmtSize(s.reclaim)} to free`)}</span>
        </button>
      </div>
    </div>
  );
}
