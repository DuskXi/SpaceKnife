// Docked log panel (W8, UI-W8-20…33): events with level, source and message; locate / copy the
// path of a line; follows the newest line while scrolled to the bottom; a scan statistics page.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { call, updateSettings, viewCall } from "@/state/actions";
import { activeTab, type LogEntry, set, toast, useApp } from "@/state/store";
import { fmtDateTime, fmtDuration, fmtInt, fmtSize } from "@/lib/format";
import { Icon, Kbd } from "@/ui/Icon";

const ROW_H = 20;
const SOURCE_ZH: Record<string, string> = {
  scan: "扫描",
  perm: "权限",
  monitor: "监控",
  delete: "删除",
  snapshot: "快照",
  export: "导出",
  system: "系统",
};
const LEVEL_ZH: Record<LogEntry["level"], string> = { INFO: "信息", WARNING: "警告", ERROR: "错误" };

export function LogPanel() {
  const open = useApp((s) => s.logOpen);
  if (!open) return <div className="logdock" />;
  return (
    <section className="logdock open" aria-label="日志面板">
      <LogBody />
    </section>
  );
}

function LogBody() {
  const tab = useApp((s) => s.logTab);
  const logs = useApp((s) => s.logs);
  const auto = useApp((s) => s.logAuto);
  const [onlyWarnings, setOnlyWarnings] = useState(false);
  const warnings = useMemo(() => logs.filter((l) => l.level !== "INFO").length, [logs]);
  const shown = useMemo(() => (onlyWarnings ? logs.filter((l) => l.level !== "INFO") : logs), [logs, onlyWarnings]);

  return (
    <>
      <div className="log-head">
        <div className="segtabs small" role="tablist" aria-label="停靠面板">
          <button role="tab" aria-selected={tab === "log"} className={tab === "log" ? "on" : ""} onClick={() => set({ logTab: "log" })}>
            <Icon name="log" size={14} />
            日志
          </button>
          <button role="tab" aria-selected={tab === "stats"} className={tab === "stats" ? "on" : ""} onClick={() => set({ logTab: "stats" })}>
            扫描统计
          </button>
        </div>
        <span className="log-summary">
          本次会话 · {fmtInt(logs.length)} 条 ·{" "}
          <button className={`link${onlyWarnings ? " on" : ""}`} aria-pressed={onlyWarnings} title={onlyWarnings ? "显示全部事件" : "只看警告和错误"} onClick={() => setOnlyWarnings(!onlyWarnings)}>
            {fmtInt(warnings)} 条警告
          </button>
        </span>
        <span className="flex" />
        <label className="switch-row">
          <span>弹出事件时自动打开</span>
          <button
            role="switch"
            aria-checked={auto}
            className={`switch${auto ? " on" : ""}`}
            onClick={() => void updateSettings({ "general.log_auto_open": !auto })}
          >
            <span />
          </button>
        </label>
        <span className="vsep" />
        <button className="ghost small" onClick={() => set({ logs: [] })}>
          清空
        </button>
        <button className="icon-btn small" aria-label="收起日志面板" title="收起日志面板（Ctrl+J）" onClick={() => set({ logOpen: false, logDismissed: true })}>
          <Icon name="chevronDown" />
        </button>
      </div>
      {tab === "log" ? <LogList logs={shown} /> : <ScanStats />}
    </>
  );
}

function LogList({ logs }: { logs: LogEntry[] }) {
  const box = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const [range, setRange] = useState({ first: 0, count: 12 });
  const [active, setActive] = useState(-1);

  // stay on the newest line while following (UI-W8-32)
  useLayoutEffect(() => {
    const el = box.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [logs.length, follow]);

  const onScroll = () => {
    const el = box.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < ROW_H;
    if (atBottom !== follow) setFollow(atBottom);
    const first = Math.floor(el.scrollTop / ROW_H);
    const count = Math.ceil(el.clientHeight / ROW_H) + 1;
    if (first !== range.first || count !== range.count) setRange({ first, count });
  };
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(onScroll);
    ro.observe(el);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // keys pressed in the log belong to the log: they must never act on the treemap behind it
  const onKey = (e: React.KeyboardEvent) => {
    const ctrl = e.ctrlKey || e.metaKey;
    const own = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    const row = active >= 0 ? logs[active] : undefined;
    if (ctrl && e.key.toLowerCase() === "c") {
      own(); // Ctrl+C copies the log line's path, never the treemap selection (UI-W8-28)
      if (row?.path) copyPath(row.path);
      return;
    }
    if (e.key === "Enter") {
      own();
      if (row?.path) void locate(row);
      return;
    }
    if (!ctrl && !e.altKey && (e.key.length === 1 || e.key === "Delete" || e.key === "Backspace" || e.key === "Escape")) e.stopPropagation();
    const n = logs.length;
    if (!n) return;
    let k = active;
    if (e.key === "ArrowDown") k = Math.min(n - 1, active < 0 ? n - 1 : active + 1);
    else if (e.key === "ArrowUp") k = Math.max(0, active < 0 ? n - 1 : active - 1);
    else if (e.key === "Home") k = 0;
    else if (e.key === "End") k = n - 1;
    else return;
    own();
    setActive(k);
    const el = box.current;
    if (el) {
      if (k * ROW_H < el.scrollTop) el.scrollTop = k * ROW_H;
      else if ((k + 1) * ROW_H > el.scrollTop + el.clientHeight) el.scrollTop = (k + 1) * ROW_H - el.clientHeight;
    }
  };

  const first = Math.max(0, range.first - 4);
  const last = Math.min(logs.length, range.first + range.count + 4);
  return (
    <div className="log-wrap">
      <div
        ref={box}
        className="log-list"
        role="listbox"
        aria-label="事件日志"
        aria-activedescendant={active >= 0 ? `log-row-${active}` : undefined}
        tabIndex={0}
        onScroll={onScroll}
        onKeyDown={onKey}
      >
        <div style={{ height: logs.length * ROW_H, position: "relative" }}>
          {logs.slice(first, last).map((e, i) => (
            <LogRow key={`${e.seq}-${first + i}`} id={`log-row-${first + i}`} entry={e} top={(first + i) * ROW_H} active={first + i === active} onPick={() => setActive(first + i)} />
          ))}
        </div>
        {logs.length === 0 && <div className="log-empty">暂无事件</div>}
      </div>
      {/* new events only are announced, politely (UI-W8-36); the virtualised rows are not a live region */}
      <div className="sr-only" role="log" aria-live="polite">
        {logs.length ? `${LEVEL_ZH[logs[logs.length - 1]!.level]}：${logs[logs.length - 1]!.message}` : ""}
      </div>
      {!follow && logs.length > 0 && (
        <button
          className="log-latest"
          onClick={() => {
            setFollow(true);
            const el = box.current;
            if (el) el.scrollTop = el.scrollHeight;
          }}
        >
          <Icon name="chevronDown" size={14} />
          跳到最新
        </button>
      )}
    </div>
  );
}

function LogRow({ id, entry: e, top, active, onPick }: { id: string; entry: LogEntry; top: number; active: boolean; onPick: () => void }) {
  const time = new Date(e.time);
  const hhmmss = Number.isNaN(time.getTime()) ? "" : time.toLocaleTimeString("zh-CN", { hour12: false });
  return (
    <div id={id} role="option" aria-selected={active} className={`log-row${active ? " on" : ""} ${e.level.toLowerCase()}`} style={{ top }} onMouseDown={onPick} title={e.message}>
      <span className="log-time mono">{hhmmss}</span>
      <span className="log-level">{LEVEL_ZH[e.level]}</span>
      <span className="log-source">{SOURCE_ZH[e.source] ?? e.source}</span>
      <span className="log-msg">{renderMessage(e.message, e.path)}</span>
      {e.path && (
        <span className="log-actions">
          {e.scanId && (
            <button className="ghost tiny" onClick={() => void locate(e)}>
              在树图中定位
            </button>
          )}
          <button className="icon-btn tiny" aria-label="复制路径（Ctrl+C）" title="复制路径（Ctrl+C）" onClick={() => copyPath(e.path)}>
            <Icon name="copy" size={13} />
          </button>
        </span>
      )}
    </div>
  );
}

/** Paths in messages in monospace, long ones cut from the left as "…\\a\\b\\c" (UI-W8-25). */
function renderMessage(message: string, path: string): React.ReactNode {
  if (!path) return message;
  const at = message.indexOf(path);
  if (at < 0) return message;
  return (
    <>
      {message.slice(0, at)}
      <span className="mono">{shortPath(path)}</span>
      {message.slice(at + path.length)}
    </>
  );
}

function shortPath(path: string): string {
  if (path.length <= 48) return path;
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(sep).filter(Boolean);
  return `…${sep}${parts.slice(-3).join(sep)}`;
}

function copyPath(path: string): void {
  window.sk.shell.copyText(path);
  toast("已复制");
}

/** Locate an event's path in the focused pane of its scan's tab (UI-W8-27). */
async function locate(e: LogEntry): Promise<void> {
  const s = useApp.getState();
  const tab = s.tabs.find((t) => t.scanId === e.scanId);
  if (!tab) {
    toast("这次扫描已经关闭");
    return;
  }
  if (s.active !== tab.id) set({ active: tab.id });
  const viewId = tab.viewIds[tab.focusedPane] ?? tab.viewIds[0];
  try {
    const found = await call("tree.find", { scan_id: e.scanId, path: e.path });
    if (!found.node) {
      toast("该路径已不存在");
      return;
    }
    await viewCall(viewId ?? null, "view.reveal", { key: found.node.id });
    if (viewId && useApp.getState().views[viewId]?.filter?.active) toast("该项可能被当前过滤隐藏");
  } catch {
    toast("无法定位该路径");
  }
}

/** 扫描统计 (UI-W8-33, W8 待定 4): the scan of the active tab, live while it runs. */
function ScanStats() {
  const tab = useApp((s) => activeTab(s));
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  if (!scan) return <div className="log-empty">当前标签没有扫描</div>;
  const running = scan.state === "scanning" || scan.state === "queued";
  const rows: [string, React.ReactNode][] = [
    ["扫描方式", scan.source === "snapshot" ? "快照" : scan.method === "mft" ? "MFT 快速扫描" : "硬扫描"],
    ["状态", scan.paused && running ? "已暂停" : { queued: "排队中", scanning: "扫描中", done: "已完成", cancelled: "已停止（结果保留）", failed: "失败" }[scan.state]],
    ["开始", scan.started_at ? fmtDateTime(Date.parse(scan.started_at) / 1000) : "—"],
    ["结束", scan.finished_at ? fmtDateTime(Date.parse(scan.finished_at) / 1000) : running ? "进行中" : "—"],
    ["耗时", fmtDuration(scan.elapsed_s)],
    ["文件 / 文件夹", `${fmtInt(scan.files)} / ${fmtInt(scan.dirs)}`],
  ];
  const right: [string, React.ReactNode][] = [
    ["已扫描合计", fmtSize(scan.bytes_alloc)],
    ["未知空间", scan.unknown != null ? fmtSize(scan.unknown) : "—（不是整卷扫描）"],
    [
      "无法访问",
      scan.errors > 0 ? (
        <button className="link" onClick={() => set({ logTab: "log" })}>
          {fmtInt(scan.errors)} 个文件夹
        </button>
      ) : (
        "0"
      ),
    ],
    ["平均吞吐", scan.elapsed_s > 0 ? `${fmtInt(Math.round(scan.items / scan.elapsed_s))} 项/秒 · ${fmtSize(scan.bytes_alloc / scan.elapsed_s)}/秒` : "—"],
    ["磁盘负载历史", <span className="muted">M4 接入</span>],
  ];
  return (
    <div className="log-stats">
      <dl>
        {rows.map(([k, v]) => (
          <div key={k}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
      <dl>
        {right.map(([k, v]) => (
          <div key={k}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
      <div className="log-stats-hint muted small">
        <Kbd>Ctrl+J</Kbd> 显示 / 隐藏日志面板
      </div>
    </div>
  );
}
