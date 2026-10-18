// Status bar (UI-MAIN-48…53): scan result, live monitoring, disk load, free space.

import { LOAD_ZH } from "@/pages/StartPage";
import type { ScanStatus } from "@/protocol/generated";
import { continueScan, updateSettings } from "@/state/actions";
import { activeTab, set, setting, startFormOf, toast, toggleLog, useApp } from "@/state/store";
import { fmtDuration, fmtInt, fmtSize } from "@/lib/format";
import { levelText, scanPercent } from "@/shell/ScanProgress";
import { Icon } from "@/ui/Icon";

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
  const level = form ? LOAD_ZH[form.load][0] : "均衡 70%";
  const [levelName, levelPct] = level.split(" ");
  const cap = io ? (io.level === "max" || io.level === "idle" ? 0 : io.limit) : { max: 100, balanced: 70, quiet: 35, idle: 0, custom: 70 }[form?.load ?? "balanced"];
  const used = vol ? vol.total - vol.free : 0;

  let first: React.ReactNode;
  if (conn !== "ready") {
    first = (
      <span className="danger">
        <Icon name="warning" size={14} /> {conn === "failed" ? "后端不可用" : "后端连接中断 · 正在重连"}
      </span>
    );
  } else if (!scan) {
    first = <span>尚未扫描 · 选择位置后开始</span>;
  } else if (scan.state === "scanning" || scan.state === "queued") {
    first = (
      <>
        <Icon name={scan.paused ? "pause" : "refresh"} size={14} />
        <span className="strong">
          {scan.paused ? "已暂停" : "扫描中…"}
          {scan.progress != null ? `${scan.paused ? " ·" : ""} ${scanPercent(scan.progress)}%` : ""}
        </span>
        <span>·</span>
        <span>{fmtInt(scan.items)} 项</span>
      </>
    );
  } else {
    first = (
      <>
        <span className="accent">
          <Icon name="bolt" size={14} />
        </span>
        <span className="strong">{scan.source === "snapshot" ? "快照" : scan.method === "mft" ? "MFT 快速扫描" : "硬扫描"}</span>
        <span>·</span>
        <span>{fmtInt(scan.items)} 项</span>
        <span>·</span>
        <span>{fmtDuration(scan.elapsed_s)}</span>
      </>
    );
  }

  return (
    <footer className="statusbar">
      <button className="status-item" title={warnings ? `${warnings} 条警告` : "扫描统计与日志"} onClick={() => toggleLog()}>
        {first}
        {warnings > 0 && <span className="warn-count">{warnings}</span>}
      </button>
      {scan && scan.source === "scan" && scan.state === "cancelled" && scan.pending_dirs > 0 && (
        <button className="status-item" title={`还有 ${fmtInt(scan.pending_dirs)} 个文件夹没有读取`} onClick={() => void continueScan(scan.scan_id)}>
          <Icon name="play" size={14} />
          <span>继续扫描</span>
        </button>
      )}
      <span className="status-sep" />
      <button
        className="status-item"
        title={live.title}
        onClick={() => {
          void updateSettings({ "scan.monitor": !monitorOn }).then((ok) => ok && toast(monitorOn ? "实时监控已关闭" : "实时监控已开启"));
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
        title={io ? `SpaceKnife 当前占用这块磁盘 ${Math.round(io.self_pct)}%，整盘 ${Math.round(io.total_pct)}%` : "扫描时按这个上限限制磁盘读取"}
        onClick={() => io && set({ inspectorHidden: false })}
      >
        <Icon name="gauge" size={14} />
        <span>{form ? "磁盘负载上限" : "磁盘负载"}</span>
        {io ? (
          <>
            <span className="strong">{levelText(io)}</span>
            <span>· 当前 {io.measured ? `${Math.round(io.self_pct)}%` : "—"}</span>
          </>
        ) : (
          <span className="strong">
            {levelName}
            {levelPct ? ` ≤${levelPct}` : ""}
          </span>
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
      {vol && (
        <button className="status-item" title={`${vol.fs_type} · ${fmtSize(vol.total, true)}\n已用 ${fmtSize(used)} / ${fmtSize(vol.total, true)}`}>
          <span className="capacity">
            <span style={{ width: `${vol.total ? (used / vol.total) * 100 : 0}%` }} />
          </span>
          <span>可用</span>
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
  const toggle = on ? "点击关闭实时监控" : "点击开启实时监控";
  if (!scan) return on ? { text: "实时监控 · 扫描后自动开启", title: toggle, tone: "muted" } : { text: "实时监控已关闭", title: toggle, tone: "muted" };
  if (scan.source === "snapshot") return { text: "快照 · 不监控变化", title: "快照是过去的状态，不随磁盘变化", tone: "muted" };
  if (!on || !m) return { text: "实时监控已关闭", title: toggle, tone: "muted" };
  const counts = `${fmtInt(m.changes)} 处变化已更新${m.pending ? `，${fmtInt(m.pending)} 项待处理` : ""}`;
  if (m.state === "failed") return { text: "实时监控 · 失败", title: `${m.detail || "监视中断"}。${toggle}`, tone: "warn-text" };
  if (m.state === "degraded") return { text: "实时监控 · 部分轮询", title: `${m.detail}。${counts}`, tone: "warn-text" };
  if (m.state === "starting") return { text: "实时监控 · 启动中", title: toggle, tone: "muted" };
  return { text: "实时监控中", title: `${counts}。${toggle}`, tone: "ok-text" };
}
