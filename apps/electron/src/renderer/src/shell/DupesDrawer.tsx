// The 「重复文件」 drawer (docs/dupes.md §10.5): the plan (what it reads at worst, the scans and disks,
// the load level and optional caps), the search's funnel and progress, the space each volume gets
// back, the keep rules, the verified groups (largest gain first, filtered by name or by the folder
// in view) or the folders holding the most extra copies with the folder they repeat, and
// 「把多余的副本加入删除篮」. It follows the tab's scan, like the delete basket, and comes after it in
// the side column.

import { useEffect, useState } from "react";

import type { DupeGroup, DupeMember, DupesFolderInfo, DupesPlan, DupesStatus } from "@/protocol/generated";
import {
  applyKeep,
  cancelSearch,
  cannotSearch,
  clearSearch,
  crossChoices,
  focusGroup,
  folderLabel,
  isRunning,
  keepOptions,
  keepThis,
  LIT_MAX,
  litFolders,
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
  setLeftOut,
  setListing,
  setLoad,
  startSearch,
  stoppedEarly,
  toBasket,
  useDupes,
} from "@/state/dupes";
import { activeTab, setting, useApp } from "@/state/store";
import { distinctPaths, fitPath, fmtAgo, fmtInt, fmtSize, plural, sizeParts } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const LOADS: DupesLoadLevel[] = ["idle", "quiet", "balanced", "max"];
export const loadLabel = (l: DupesLoadLevel): string => ({ idle: t("空闲"), quiet: t("安静"), balanced: t("均衡"), max: t("最大") })[l];
const RULES: KeepRule[] = ["oldest", "newest", "shortest"];
const SORTS: DupesSort[] = ["reclaim", "size", "copies", "path"];
const sortLabel = (s: DupesSort): string => ({ reclaim: t("可腾出"), size: t("单份大小"), copies: t("份数"), path: t("路径") })[s];

export const mediaLabel = (m: string): string =>
  ({
    nvme_ssd: "NVMe SSD",
    sata_ssd: "SATA SSD",
    ssd: "SSD",
    hdd: "HDD",
    usb_hdd: "USB HDD",
    usb_ssd: "USB SSD",
    usb_flash: t("U 盘"),
    sd_card: t("SD 卡"),
    network: t("网络盘"),
    virtual: t("虚拟盘"),
  })[m] ?? "";

export const stateLabel = (s: DupesStatus): string => {
  if (s.state === "done" && s.stop_reason === "budget") return t("读到上限，停了");
  if (s.state === "done" && s.stop_reason === "time") return t("到时间上限，停了");
  return {
    planning: t("准备中"),
    identifying: t("核对身份"),
    probing: t("读头尾"),
    verifying: t("核对内容"),
    waiting_scan: t("等 {disk} 的扫描结束", { disk: s.waiting_for }),
    paused: t("已暂停"),
    done: t("完成"),
    cancelled: t("已停止"),
    failed: t("出错"),
  }[s.state];
};

export const roleLabel = (role: DupeMember["role"]): string =>
  ({
    keep: t("保留"),
    extra: t("多余"),
    alias: t("同一个文件"),
    clone: t("同一份数据"),
    needed: t("程序需要"),
    pinned: t("必须保留"),
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
  const focused = tab.viewIds[tab.focusedPane];

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
            <button className="ghost small nowrap" title={t("清除这次查找的结果和配色")} onClick={() => void clearSearch(status.job_id)}>
              {t("清除结果")}
            </button>
          )}
          <button className="icon-btn" title={t("关闭")} aria-label={t("关闭重复文件")} onClick={() => setDupes({ open: false })}>
            <Icon name="close" />
          </button>
        </div>
        {status ? <Scope s={status} /> : null}
        {status ? <Summary s={status} scanId={scanId} /> : why ? <div className="muted small dp-why">{why}</div> : <PlanView plan={plan} scanId={scanId} />}
        {covered && <Coverage s={covered} viewId={focused} />}
        {status && <DupesLegend compact />}
      </div>
      {status && (
        <>
          <KeepRules s={status} />
          <div className="bk-listhead dp-tabs" role="tablist">
            <button role="tab" aria-selected={view === "groups"} className={view === "groups" ? "on" : ""} onClick={() => setDupes({ tab: "groups" })}>
              {t("组")}
            </button>
            <button role="tab" aria-selected={view === "folders"} className={view === "folders" ? "on" : ""} onClick={() => setDupes({ tab: "folders" })}>
              {t("按文件夹")}
            </button>
            <span className="flex" />
            {view === "groups" && <SortPicker s={status} />}
          </div>
          {view === "groups" && <ListFilters s={status} />}
          <LitNote viewIds={tab.viewIds} />
          {view === "groups" ? <GroupList s={status} scanId={scanId} viewIds={tab.viewIds} focused={focused} /> : <FolderList s={status} scanId={scanId} viewId={focused} />}
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
      {what} · {t("不小于 {size}", { size: fmtSize(s.min_size) })} · {t("负载「{level}」", { level: loadLabel((s.load === "custom" ? "balanced" : s.load) as DupesLoadLevel) })}
    </div>
  );
}

/** The worst case of a search, in bytes and (from the last search's speed here) in time. */
export function worstCase(plan: DupesPlan): string {
  const rate = plan.last?.rate ?? 0;
  const media = [...new Set(plan.disks.map((d) => mediaLabel(d.media)).filter(Boolean))].join(" · ");
  const time = rate > 0 ? t("按上次的速度最多约 {m} 分钟", { m: fmtInt(Math.max(1, Math.ceil(plan.full_bytes_max / rate / 60))) }) : t("要多久看盘的速度");
  return t("最坏要读 {size}（{time}{media}）· 最多能腾出 {reclaim}", {
    size: fmtSize(plan.full_bytes_max),
    time,
    media: media ? ` · ${media}` : "",
    reclaim: fmtSize(plan.reclaim_max),
  });
}

function PlanView({ plan, scanId }: { plan: DupesPlan | null; scanId: string }) {
  const cross = useApp((s) => setting(s, "dupes.cross_volume", false));
  const error = useDupes((s) => s.planError[scanId] ?? "");
  const startLoad = useDupes((s) => s.startLoad);
  const capGb = useDupes((s) => s.capGb);
  const capMinutes = useDupes((s) => s.capMinutes);
  const settingLoad = useApp((s) => setting(s, "dupes.load", "quiet")) as DupesLoadLevel;
  const load = startLoad ?? (LOADS.includes(settingLoad) ? settingLoad : "quiet");
  // re-read when tabs open or close: the cross-volume choices are the open tabs' scans
  useApp((s) => s.tabs.map((x) => x.scanId ?? "").join("|"));
  const choices = crossChoices(scanId);
  return (
    <div className="dp-plan">
      {error ? (
        <div className="dp-warn small" role="alert">
          <Icon name="warning" size={13} />
          <span className="flex">{t("没能估计要读多少：{error}", { error })}</span>
          <button className="ghost small nowrap" onClick={() => void loadPlan(scanId)}>
            {t("重试")}
          </button>
        </div>
      ) : plan ? (
        <>
          <div className="dp-big">
            <span className="mono">{fmtInt(plan.candidates)}</span> <span className="bk-unit">{plural(plan.candidates, t("个文件和别的文件大小相同"), "file shares its size with another file")}</span>
          </div>
          <div className="small dp-worst">{worstCase(plan)}</div>
          <div className="muted small">
            {t("先读每个文件的头尾，共约 {probe}；只有几份头尾还一样时才读完整个文件。大小独一无二的 {n} 个文件不读。", {
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
          <Excluded counts={plan.excluded} minSize={plan.min_size} />
        </>
      ) : (
        <div className="muted small">{t("正在估计要读多少…")}</div>
      )}
      <label className="dp-check" title={choices.length < 2 ? t("至少要有两个打开的、扫完的扫描") : undefined}>
        <input type="checkbox" checked={cross} disabled={choices.length < 2 && !cross} onChange={(e) => void setCross(scanId, e.target.checked)} />
        <span>{t("跨盘比较：把打开的扫描放在一起比")}</span>
      </label>
      {cross && (
        <div className="dp-scans small" aria-label={t("要比较的扫描")}>
          {choices.map((c) => (
            <label key={c.scanId} className="dp-check">
              <input type="checkbox" checked={c.on} disabled={c.scanId === scanId} onChange={(e) => setLeftOut(c.scanId, scanId, !e.target.checked)} />
              <span className="mono dp-path" title={c.label}>
                {c.label}
              </span>
            </label>
          ))}
          <div className="muted">{t("跨盘的副本常常是有意的备份：每个卷默认至少留一份")}</div>
        </div>
      )}
      <div className="dp-opts small">
        <div className="segmented small dp-levels" role="radiogroup" aria-label={t("读盘负载")}>
          {LOADS.map((l) => (
            <button key={l} role="radio" aria-checked={load === l} className={load === l ? "on" : ""} onClick={() => setDupes({ startLoad: l })}>
              {loadLabel(l)}
            </button>
          ))}
        </div>
        <label className="dp-cap">
          {t("最多读")}
          <input type="number" min={0} step={1} value={capGb ?? ""} placeholder={t("不限")} aria-label={t("最多读多少 GB")} onChange={(e) => setDupes({ capGb: e.target.value === "" ? null : Math.max(0, Number(e.target.value)) })} />
          GB
        </label>
        <label className="dp-cap">
          {t("最多")}
          <input type="number" min={0} step={1} value={capMinutes ?? ""} placeholder={t("不限")} aria-label={t("最多多少分钟")} onChange={(e) => setDupes({ capMinutes: e.target.value === "" ? null : Math.max(0, Number(e.target.value)) })} />
          {t("分钟")}
        </label>
      </div>
      <button className="primary dp-start" disabled={!scanId || !plan} onClick={() => void startSearch(scanId)}>
        <Icon name="search" />
        {t("开始查找")}
      </button>
    </div>
  );
}

function Excluded({ counts, minSize }: { counts: Record<string, number>; minSize: number }) {
  const parts = [
    counts.small ? plural(counts.small, t("{n} 个文件小于下限 {size}", { n: fmtInt(counts.small), size: fmtSize(minSize) }), "1 file below the minimum of {size}", { size: fmtSize(minSize) }) : "",
    counts.cloud ? t("云占位 {n} 个（读内容会触发下载）", { n: fmtInt(counts.cloud) }) : "",
    counts.system ? t("系统和程序文件夹里的 {n} 个", { n: fmtInt(counts.system) }) : "",
    counts.bins ? t("回收站里的 {n} 个", { n: fmtInt(counts.bins) }) : "",
    counts.network ? t("网络盘上的 {n} 个", { n: fmtInt(counts.network) }) : "",
  ].filter(Boolean);
  if (!parts.length) return null;
  return <div className="muted small dp-excluded">{t("没有比较：{list}", { list: parts.join(" · ") })}</div>;
}

function Summary({ s, scanId }: { s: DupesStatus; scanId: string }) {
  const running = isRunning(s);
  const early = stoppedEarly(s);
  const [num, unit] = sizeParts(s.reclaim);
  const f = s.funnel;
  const pct = Math.round(100 * (s.progress ?? 0));
  const skipped = Object.entries(s.skipped_by_reason).filter(([k]) => k !== "stopped");
  const nSkipped = skipped.reduce((a, [, v]) => a + v, 0);
  return (
    <>
      <div className="bk-free">
        <div className="muted small">
          <span className="bk-legend dp-legend" /> {early ? t("至少可腾出") : t("可腾出")}
        </div>
        <div className="bk-big">
          <span className="mono">{num}</span> <span className="bk-unit">{unit}</span>
        </div>
        <div className="muted small">
          {plural(s.extra, t("{n} 份多余的副本", { n: fmtInt(s.extra) }), "1 extra copy")} · {plural(s.groups, t("{g} 组", { g: fmtInt(s.groups) }), "1 group")}
        </div>
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
      {s.state === "failed" && (
        <div className="dp-warn small" role="alert">
          <Icon name="warning" size={13} />
          <span>{s.error || t("查找中途出错，可以再查一次（详情见日志）")}</span>
        </div>
      )}
      {early && s.undecided > 0 && (
        <div className="dp-warn small">
          <Icon name="info" size={13} />
          <span>
            {plural(s.undecided, t("还有 {n} 个候选没核对，结果不完整；「再查一次」接着核对，核对过的不再读", { n: fmtInt(s.undecided) }), "1 candidate was not checked: the result is incomplete. “Search again” goes on (what was checked is not read again)")}
          </span>
        </div>
      )}
      <div className="dp-funnel small" aria-label={t("查找进度")}>
        <div className="dp-step">
          <span>{t("同样大小（不读盘）")}</span>
          <span className="mono">{fmtInt(f.candidates)}</span>
        </div>
        <div className="dp-step">
          <span>{t("同一个文件的几个名字算一份后")}</span>
          <span className="mono">{fmtInt(f.physical)}</span>
        </div>
        {f.aliases + f.clones > 0 && <div className="dp-step muted">{t("其中同一个文件的别名 {a} · 块克隆 {c}（不读内容）", { a: fmtInt(f.aliases), c: fmtInt(f.clones) })}</div>}
        {s.bytes.cache_hits > 0 && (
          <div className="dp-step">
            <span>{t("缓存认出（不读盘）")}</span>
            <span className="mono">{fmtInt(s.bytes.cache_hits)}</span>
          </div>
        )}
        <div className="dp-step">
          <span>{t("读了头尾")}</span>
          <span className="mono">{fmtInt(f.probed)}</span>
        </div>
        <div className="dp-step dp-strong">
          <span>{t("内容相同（已读完全文核对）")}</span>
          <span className="mono">{t("{g} 组 · {n} 个文件", { g: fmtInt(f.confirmed_groups), n: fmtInt(f.confirmed_files) })}</span>
        </div>
        <div className="muted">
          {t("已读 {read}（候选的 {pct}）", {
            read: fmtSize(s.bytes.read),
            pct: s.bytes.full_upper ? `${((100 * s.bytes.read) / s.bytes.full_upper).toFixed(1)}%` : "—",
          })}
        </div>
        {nSkipped > 0 && (
          <div className="muted">
            {t("没有比较 {n} 个：{list}", {
              n: fmtInt(nSkipped),
              list: skipped.map(([k, v]) => `${skipLabel(k)} ${fmtInt(v)}`).join(" · "),
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
        <div className={`dp-bar${early ? " stopped" : ""}`} role="progressbar" aria-label={t("查找进度")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={running || early ? pct : 100}>
          <span style={{ width: `${running || early ? pct : 100}%` }} />
        </div>
        <div className="dp-run-row small">
          <span className="nowrap">{stateLabel(s)}</span>
          {(running || early) && <span className="mono">{pct}%</span>}
          {running && s.rate > 0 && <span className="muted mono nowrap">{t("{rate}/秒", { rate: fmtSize(s.rate) })}</span>}
          {running && s.eta_max_s != null && <span className="muted nowrap">{t("最多还要 {m} 分钟", { m: String(Math.max(1, Math.ceil(s.eta_max_s / 60))) })}</span>}
          {running && s.rate <= 0 && s.state !== "paused" && s.state !== "waiting_scan" && <span className="muted nowrap">{t("速度估计中")}</span>}
          <span className="flex" />
          {!running && s.mode === "all" && scanId && (
            <button className="ghost small dp-again nowrap" title={t("没变过的文件不再读内容")} onClick={() => void startSearch(scanId)}>
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
    external_links: t("另有硬链接"),
    network_mount: t("在网络盘或 FUSE 上"),
  })[k] ?? k;

/** The 「重复」 colouring's key (docs/dupes.md §10.2): file states and folder tiers. */
export function DupesLegend({ compact = false }: { compact?: boolean }) {
  const [open, setOpen] = useState(!compact);
  const files: [string, string][] = [
    ["extra", t("多余的副本（可删）")],
    ["keep", t("保留的一份")],
    ["pending", t("待核对")],
    ["shared", t("同一份数据（删了不腾空间）")],
    ["none", t("没有重复，或没比较")],
  ];
  const tiers: [string, string][] = [
    ["1", t("有一点")],
    ["2", "≥ 10%"],
    ["3", "≥ 40%"],
    ["4", "≥ 80%"],
  ];
  return (
    <div className={`dp-key small${compact ? " compact" : ""}`} aria-label={t("「重复」配色的图例")}>
      <button className="dp-key-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon name={open ? "chevronDown" : "chevronRight"} size={12} />
        {t("图例")}
      </button>
      {open && (
        <div className="dp-key-body">
          {files.map(([id, label]) => (
            <span key={id} className="dp-key-item">
              <span className={`dp-swatch f-${id}`} />
              {label}
            </span>
          ))}
          <span className="dp-key-sub muted">{t("文件夹：多余副本占的比例")}</span>
          {tiers.map(([id, label]) => (
            <span key={id} className="dp-key-item">
              <span className={`dp-swatch t-${id}`} />
              {label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/** 「这个文件夹在别处有没有」: a bar by bytes (§8.4) — for the user to judge, never a delete. */
export function Coverage({ s, viewId }: { s: DupesStatus; viewId?: string }) {
  const cover = useDupes((x) => x.coverage[s.job_id] ?? null);
  if (!cover) return isRunning(s) ? <div className="muted small">{t("核对完后这里给出这个文件夹有多少在别处也有")}</div> : null;
  return <CoverageBar info={cover.info} label={folderLabel(cover.node)} viewId={viewId} onClose={s.mode === "all" ? () => setDupes({ coverage: dropKey(useDupes.getState().coverage, s.job_id) }) : undefined} />;
}

function dropKey<T>(o: Record<string, T>, key: string): Record<string, T> {
  const out = { ...o };
  delete out[key];
  return out;
}

export function CoverageBar({ info, label, onClose, viewId }: { info: DupesFolderInfo; label?: string; onClose?: () => void; viewId?: string }) {
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
        <div className="dp-cover-title">
          <strong>{t("这个文件夹在别处有没有？")}</strong>
          {label && <span className="mono dp-path">{label}</span>}
        </div>
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
      {info.elsewhere_list.length > 0 && (
        <div className="dp-where small">
          <span className="muted">{t("别处的副本主要在：")}</span>
          {info.elsewhere_list.map((w) => (
            <button
              key={`${w.scan_id}:${w.path}`}
              className="mono dp-path dp-where-row"
              title={t("在图里找到它：{path}", { path: w.path })}
              onMouseEnter={() => w.node > 0 && litFolders(w.scan_id, [w.node])}
              onMouseLeave={() => litFolders(null)}
              onClick={() => w.node > 0 && void revealCopy(viewId, w.node, w.scan_id)}
            >
              <span className="dp-path">{fitPath(w.path, 40)}</span>
              <span className="muted">{fmtSize(w.bytes)}</span>
            </button>
          ))}
        </div>
      )}
      {info.small > 0 && <div className="muted small">{plural(info.small, t("其中 {n} 个小于下限，没有核对", { n: fmtInt(info.small) }), "1 of them is below the minimum and was not checked")}</div>}
      {info.only_here_list.length > 0 && (
        <details className="dp-only small">
          <summary>{t("只在这里的最大的 {n} 个", { n: fmtInt(info.only_here_list.length) })}</summary>
          {info.only_here_list.slice(0, 50).map((x) => (
            <div key={x.node} className="dp-only-row">
              <span className="mono dp-path" title={x.path}>
                {fitPath(x.path, 44)}
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

function SortPicker({ s }: { s: DupesStatus }) {
  const sort = useDupes((x) => x.sort);
  return (
    <select className="bk-sort" value={sort} aria-label={t("排序")} onChange={(e) => setListing({ sort: e.target.value as DupesSort }, s.job_id)}>
      {SORTS.map((x) => (
        <option key={x} value={x}>
          {sortLabel(x)}
        </option>
      ))}
    </select>
  );
}

/** Second row of the list head: by name or path, the folder in view, across volumes only, the
 * same data too. */
function ListFilters({ s }: { s: DupesStatus }) {
  const crossOnly = useDupes((x) => x.crossOnly);
  const shared = useDupes((x) => x.includeShared);
  const under = useDupes((x) => x.underZoom);
  const query = useDupes((x) => x.query);
  const [text, setText] = useState(query);
  useEffect(() => {
    if (text === query) return;
    const timer = setTimeout(() => setListing({ query: text }, s.job_id), 250);
    return () => clearTimeout(timer);
  }, [text, query, s.job_id]);
  return (
    <div className="dp-filters small">
      <input className="dp-query" type="search" value={text} placeholder={t("按名字或路径筛选")} aria-label={t("按名字或路径筛选")} onChange={(e) => setText(e.target.value)} />
      <label className="dp-check nowrap" title={t("只看在当前放大的文件夹里有副本的组")}>
        <input type="checkbox" checked={under} onChange={(e) => setListing({ underZoom: e.target.checked }, s.job_id)} />
        <span>{t("只看当前文件夹")}</span>
      </label>
      {s.cross_volume && (
        <label className="dp-check nowrap" title={t("只看副本在不同卷上的组")}>
          <input type="checkbox" checked={crossOnly} onChange={(e) => setListing({ crossOnly: e.target.checked }, s.job_id)} />
          <span>{t("只看跨盘")}</span>
        </label>
      )}
      <label className="dp-check nowrap" title={t("也列出同一个文件的几个名字和块克隆：它们不占双份空间，删了不腾空间")}>
        <input type="checkbox" checked={shared} onChange={(e) => setListing({ includeShared: e.target.checked }, s.job_id)} />
        <span>{t("也列同一份数据")}</span>
      </label>
    </div>
  );
}

/** More copies than are lit at once. */
function LitNote({ viewIds }: { viewIds: string[] }) {
  const truncated = useDupes((x) => viewIds.some((v) => x.located[v]?.truncated));
  return truncated ? <div className="muted small dp-litnote">{t("只点亮最大的 {n} 份", { n: fmtInt(LIT_MAX) })}</div> : null;
}

const NO_GROUPS: DupeGroup[] = [];

/** What the empty list says: still running, stopped early, failed, filtered, or nothing found. */
export function emptyText(s: DupesStatus, filtered: boolean): string {
  if (isRunning(s)) return t("核对完的组会出现在这里，不必等全部做完");
  if (filtered) return t("没有符合筛选的组");
  if (s.state === "failed") return t("查找出错，没有找到完整的结果");
  if (stoppedEarly(s))
    return plural(s.undecided, t("查找已停止：{n} 个候选没核对，结果不完整", { n: fmtInt(s.undecided) }), "The search stopped: 1 candidate was not checked, the result is incomplete");
  return t("没有找到内容相同的文件");
}

function GroupList({ s, scanId, viewIds, focused }: { s: DupesStatus; scanId: string; viewIds: string[]; focused: string | undefined }) {
  const groups = useDupes((x) => (x.groupsJob === s.job_id ? x.groups : NO_GROUPS));
  const total = useDupes((x) => (x.groupsJob === s.job_id ? x.groupsTotal : 0));
  const lit = useDupes((x) => (x.focus?.jobId === s.job_id ? x.focus.group : -1));
  const filtered = useDupes((x) => x.query.trim() !== "" || x.underZoom || x.crossOnly);
  return (
    <div className="bk-list dp-list" role="list" aria-label={t("内容相同的文件")} onMouseLeave={() => focusGroup(null, null)}>
      {groups.length === 0 && <div className="muted bk-empty">{emptyText(s, filtered)}</div>}
      {groups.map((g) => (
        <GroupRow key={g.id} g={g} jobId={s.job_id} scanId={scanId} viewId={focused} lit={g.id === lit} onHover={() => focusGroup(s.job_id, g.id, viewIds)} />
      ))}
      {groups.length < total && (
        <button className="ghost dp-more" onClick={() => void loadGroups(s.job_id, "more")}>
          {plural(Math.min(PAGE, total - groups.length), t("再显示 {n} 组", { n: fmtInt(Math.min(PAGE, total - groups.length)) }), "Show 1 more group")}
        </button>
      )}
    </div>
  );
}

export function groupName(g: DupeGroup): string {
  const first = g.members[0];
  const name = first ? (first.path.split(/[\\/]/).pop() ?? first.path) : "";
  return g.names > 1 ? t("{name}（另有 {n} 个不同的文件名）", { name, n: fmtInt(g.names - 1) }) : name;
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
              ? t("必须保留：它另有硬链接或名字，或名字以点或空格结尾（删除篮删不了）")
              : t("按「{rule}」保留", { rule: ruleLabel(rule) });

/** One group, compact: name, copies and gain on one line, the verdict as an icon, a line per copy
 * with only what tells the copies apart. */
export function GroupRow({ g, jobId, scanId, viewId, lit, onHover }: { g: DupeGroup; jobId: string; scanId: string; viewId: string | undefined; lit: boolean; onHover: () => void }) {
  const confirmed = g.state === "confirmed";
  const verdict = confirmed ? t("内容相同（已读完全文核对）") : t("同一份数据（硬链接或块克隆）：删了不腾空间");
  const shown = distinctPaths(
    g.members.map((m) => m.path),
    46,
  );
  return (
    <div className={`bk-row dp-group${lit ? " lit" : ""}`} role="listitem" onMouseEnter={onHover} onFocus={onHover}>
      <div className="dp-ghead">
        <span className={`dp-verdict-icon${confirmed ? "" : " shared"}`} title={verdict} aria-label={verdict} role="img">
          <Icon name={confirmed ? "check" : "layers"} size={13} />
        </span>
        <span className="bk-name dp-gname" title={groupName(g)}>
          {groupName(g)}
        </span>
        <span className="muted small nowrap">
          {plural(g.copies, t("{n} 份 · 每份 {size}", { n: fmtInt(g.copies), size: fmtSize(g.size) }), "1 copy · {size}", { size: fmtSize(g.size) })}
          {g.cross ? ` · ${g.volumes.join(" · ")}` : ""}
        </span>
        <span className="mono dp-reclaim nowrap" title={t("可腾出")}>
          {confirmed ? fmtSize(g.reclaim) : "0 B"}
        </span>
      </div>
      <div className="dp-members">
        {g.members.map((m, i) => {
          const why = m.role === "keep" || m.role === "needed" ? keepWhyText(m.why ?? g.keep_why, keepOptions().rule) : "";
          const other = m.scan_id !== scanId;
          return (
            <div key={`${m.scan_id}:${m.path}`} className={`dp-member ${m.role}`}>
              <span className={`dp-role ${m.role}`} title={why || undefined}>
                {roleLabel(m.role)}
              </span>
              <button
                className="mono dp-path"
                title={other ? t("切换到它的页签并找到它：{path}", { path: m.path }) : t("在图里找到它：{path}", { path: m.path })}
                onClick={() => void revealCopy(viewId, m.node, m.scan_id)}
              >
                {shown[i]}
              </button>
              {m.view && <span className="muted dp-view" title={t("同一个文件的另一条路径（重叠的扫描）")}>{t("同一个文件")}</span>}
              {m.role === "extra" && !m.view && (
                <button className="ghost dp-keepbtn nowrap" title={t("保留这一份，这组别的副本里挑多余的")} onClick={() => void keepThis(jobId, g.id, m.path)}>
                  {t("保留这一份")}
                </button>
              )}
            </div>
          );
        })}
        {g.total_members > g.members.length && <div className="muted small">{plural(g.total_members - g.members.length, t("另 {n} 份", { n: fmtInt(g.total_members - g.members.length) }), "1 more copy")}</div>}
      </div>
    </div>
  );
}

function FolderList({ s, scanId, viewId }: { s: DupesStatus; scanId: string; viewId: string | undefined }) {
  const folders = useDupes((x) => x.folders);
  const total = useDupes((x) => x.foldersTotal);
  const enter = (node: number, twinScan: string, twin: number) => {
    const items = [node];
    if (twin > 0 && (twinScan === scanId || !twinScan)) items.push(twin);
    litFolders(scanId, items);
  };
  return (
    <div className="bk-list dp-list" role="list" aria-label={t("多余的副本最多的文件夹")} onMouseLeave={() => litFolders(null)}>
      {folders.length === 0 && <div className="muted bk-empty">{isRunning(s) ? t("核对完后这里列出多余的副本最多的文件夹") : t("没有文件夹里有多余的副本")}</div>}
      {folders.map((f) => (
        <button key={f.node} className="bk-row dp-folder" role="listitem" title={t("在图里找到它：{path}", { path: f.path })} onMouseEnter={() => enter(f.node, f.twin_scan_id, f.twin_node)} onFocus={() => enter(f.node, f.twin_scan_id, f.twin_node)} onClick={() => void revealCopy(viewId, f.node)}>
          <div className="bk-row-main">
            <span className="bk-icon">
              <Icon name="folder" />
            </span>
            <div className="bk-text">
              <span className="bk-name mono">{fitPath(f.path, 44)}</span>
              {f.twin && (
                <span className="bk-path dp-twin" title={f.twin}>
                  {t("{pct} 在 {twin} 也有", { pct: `${Math.round((100 * f.twin_bytes) / Math.max(1, f.alloc))}%`, twin: fitPath(f.twin, 34) })}
                </span>
              )}
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
      {total > folders.length && (
        <button className="ghost dp-more" onClick={() => void loadFolders(s.job_id, scanId, true)}>
          {plural(Math.min(PAGE, total - folders.length), t("再显示 {n} 个文件夹", { n: fmtInt(Math.min(PAGE, total - folders.length)) }), "Show 1 more folder")}
        </button>
      )}
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
          <span className="dp-commit-label">{plural(s.extra, t("把多余的 {n} 份加入删除篮", { n: fmtInt(s.extra) }), "Add 1 extra to the basket")}</span>
          <span className="mono dp-commit-size">{fmtSize(s.reclaim)}</span>
        </button>
      </div>
    </div>
  );
}
