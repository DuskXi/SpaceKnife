// Status bar (UI-MAIN-48…53): scan result, live monitoring, disk load, free space.

import type { ScanStatus } from "@/protocol/generated";
import { continueScan, updateSettings } from "@/state/actions";
import { activeTab, type LoadLevel, set, setting, startFormOf, toast, toggleLog, useApp } from "@/state/store";
import { fmtDuration, fmtInt, fmtSize } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { activeTip, levelName, levelText, scanPercent } from "@/shell/ScanProgress";
import { Icon } from "@/ui/Icon";
import { StatusPluginItems } from "@/plugins/slots";

/** The limit of a load level before a scan reports its own (the start page's choice, UI-W1-13). */
const LEVEL_CAP: Record<LoadLevel, number> = { max: 100, balanced: 70, quiet: 35, idle: 0, custom: 70 };

export function StatusBar() {
  const tab = useApp((s) => activeTab(s));
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  const monitorOn = useApp((s) => setting(s, "scan.monitor", true));
  const live = monitorText(scan, monitorOn);
  const conn = useApp((s) => s.conn);
  const volumes = useApp((s) => s.volumes);
  const warnings = useApp((s) => s.logs.filter((l) => l.level === "WARNING" || l.level === "ERROR").length);
  // the start page shows its chosen target and load level (UI-W1-13, UI-W1-14)
  const form = useApp((s) => (tab.kind === "start" ? startFormOf(s, tab.id) : null));
  const formVolume = form ? volumes.find((v) => v.mount_point === (form.path.trim() ? form.check?.items.find((i) => i.volume)?.volume : form.picked)) : undefined;
  const vol = scan?.roots[0]?.volume ?? formVolume ?? volumes.find((v) => v.is_system) ?? volumes[0];
  const io = scan && (scan.state === "scanning" || scan.state === "queued") ? scan.io : null;
  const load: LoadLevel = form ? form.load : "balanced";
  // 「均衡 ≤70%」, 「安静 ≤35%」; 极速, 空闲 and 自定义 without a percentage
  const levelLabel = load === "balanced" || load === "quiet" ? `${levelName(load)} ≤${LEVEL_CAP[load]}%` : levelName(load);
  const cap = io ? (io.level === "max" || io.level === "idle" ? 0 : io.limit) : LEVEL_CAP[load];
  const used = vol ? vol.total - vol.free : 0;

  let first: React.ReactNode;
  if (conn !== "ready") {
    first = (
      <span className="danger">
        <Icon name="warning" size={14} /> {conn === "failed" ? t("后端不可用") : t("后端连接中断 · 正在重连")}
      </span>
    );
  } else if (!scan) {
    first = <span>{t("尚未扫描 · 选择位置后开始")}</span>;
  } else if (scan.state === "scanning" || scan.state === "queued") {
    first = (
      <>
        <Icon name={scan.paused ? "pause" : "refresh"} size={14} />
        <span className="strong">
          {scan.paused ? t("已暂停") : t("扫描中…")}
          {scan.progress != null ? `${scan.paused ? " ·" : ""} ${scanPercent(scan.progress)}%` : ""}
        </span>
        <span>·</span>
        <span>{t("{n} 项", { n: fmtInt(scan.items) })}</span>
      </>
    );
  } else {
    first = (
      <>
        <span className="accent">
          <Icon name="bolt" size={14} />
        </span>
        {/* 「快照」 here is the one snapshot the tab shows (elsewhere the same word names the snapshots, plural) */}
        <span className="strong">{scan.source === "snapshot" ? byLang("快照", "Snapshot") : scan.method === "mft" ? t("MFT 快速扫描") : scan.method === "source" ? t("插件来源") : t("硬扫描")}</span>
        <span>·</span>
        <span>{t("{n} 项", { n: fmtInt(scan.items) })}</span>
        <span>·</span>
        <span>{fmtDuration(scan.elapsed_s)}</span>
      </>
    );
  }

  return (
    <footer className="statusbar">
      <button className="status-item" title={warnings ? t("{n} 条警告", { n: warnings }) : t("扫描统计与日志")} onClick={() => toggleLog()}>
        {first}
        {warnings > 0 && <span className="warn-count">{warnings}</span>}
      </button>
      {scan && scan.source === "scan" && scan.state === "cancelled" && scan.pending_dirs > 0 && (
        <button className="status-item" title={t("还有 {n} 个文件夹没有读取", { n: fmtInt(scan.pending_dirs) })} onClick={() => void continueScan(scan.scan_id)}>
          <Icon name="play" size={14} />
          <span>{t("继续扫描")}</span>
        </button>
      )}
      <span className="status-sep" />
      <button
        className="status-item"
        title={live.title}
        onClick={() => {
          void updateSettings({ "scan.monitor": !monitorOn }).then((ok) => ok && toast(monitorOn ? t("实时监控已关闭") : t("实时监控已开启")));
        }}
      >
        <span className={live.tone}>
          <Icon name="pulse" size={14} />
        </span>
        <span>{live.text}</span>
      </button>
      <span className="status-sep" />
      <button
        className="status-item"
        title={
          io
            ? t("SpaceKnife 当前占用这块磁盘 {self}%，整盘 {total}%，活动时间 {active}%\n{tip}", {
                self: Math.round(io.self_pct),
                total: Math.round(io.total_pct),
                active: Math.round(io.active_pct),
                tip: activeTip(),
              })
            : t("扫描时按这个上限限制磁盘读取")
        }
        onClick={() => io && set({ inspectorHidden: false })}
      >
        <Icon name="gauge" size={14} />
        <span>{form ? t("磁盘负载上限") : t("磁盘负载")}</span>
        {io ? (
          <>
            <span className="strong">{levelText(io)}</span>
            <span>· {t("当前 {value}", { value: io.measured ? `${Math.round(io.self_pct)}%` : "—" })}</span>
            {io.measured && <span className="muted">· {t("活动时间 {n}%", { n: Math.round(io.active_pct) })}</span>}
          </>
        ) : (
          <span className="strong">{levelLabel}</span>
        )}
        {cap > 0 && cap < 100 && (
          <span className="meter">
            <span className="meter-track" />
            {io && io.measured && <span className="meter-fill" style={{ width: `${Math.min(100, io.self_pct)}%` }} />}
            <span className="meter-mark" style={{ left: `${cap}%` }} />
          </span>
        )}
      </button>
      <span className="flex" />
      {/* plugins.v1: reports placed in the status bar (slot "status") */}
      <StatusPluginItems scanId={tab.scanId ?? null} />
      {vol && (
        <button className="status-item" title={t("{fs} · {size}\n已用 {used} / {total}", { fs: vol.fs_type, size: fmtSize(vol.total, true), used: fmtSize(used), total: fmtSize(vol.total, true) })}>
          <span className="capacity">
            <span style={{ width: `${vol.total ? (used / vol.total) * 100 : 0}%` }} />
          </span>
          <span>{t("可用")}</span>
          <span className="strong">{fmtSize(vol.free)}</span>
          <span>/ {fmtSize(vol.total, true)}</span>
        </button>
      )}
    </footer>
  );
}

/** 「实时监控」 (UI-MAIN-49, UI-W1-12): the change hooks of the active scan; a click switches them. */
export function monitorText(scan: ScanStatus | undefined, on: boolean): { text: string; title: string; tone: string } {
  const m = scan?.monitor;
  const toggle = on ? t("点击关闭实时监控") : t("点击开启实时监控");
  if (!scan) return on ? { text: t("实时监控 · 扫描后自动开启"), title: toggle, tone: "muted" } : { text: t("实时监控已关闭"), title: toggle, tone: "muted" };
  if (scan.source === "snapshot") return { text: t("快照 · 不监控变化"), title: t("快照是过去的状态，不随磁盘变化"), tone: "muted" };
  if (!on || !m) return { text: t("实时监控已关闭"), title: toggle, tone: "muted" };
  const counts = m.pending ? t("{n} 处变化已更新，{pending} 项待处理", { n: fmtInt(m.changes), pending: fmtInt(m.pending) }) : t("{n} 处变化已更新", { n: fmtInt(m.changes) });
  if (m.state === "failed") return { text: t("实时监控 · 失败"), title: t("{a}。{b}", { a: m.detail || t("监视中断"), b: toggle }), tone: "warn-text" };
  if (m.state === "degraded") return { text: t("实时监控 · 部分轮询"), title: t("{a}。{b}", { a: m.detail, b: counts }), tone: "warn-text" };
  if (m.state === "starting") return { text: t("实时监控 · 启动中"), title: toggle, tone: "muted" };
  return { text: t("实时监控中"), title: t("{a}。{b}", { a: counts, b: toggle }), tone: "ok-text" };
}
