// Docked log panel (W8, UI-W8-20…33): events with level, source and message; locate / copy the
// path of a line; follows the newest line while scrolled to the bottom; a scan statistics page.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { call, updateSettings, viewCall } from "@/state/actions";
import { activeTab, type LogEntry, set, toast, useApp } from "@/state/store";
import { fmtDateTime, fmtDuration, fmtInt, fmtSize } from "@/lib/format";
import { byLang, getLang, t } from "@/lib/i18n";
import { Icon, Kbd } from "@/ui/Icon";

const ROW_H = 20;
/** The sources the backend sends (events.py `Source`): their names size the source column. */
const SOURCES = ["scan", "perm", "monitor", "delete", "snapshot", "export", "system"];
const LEVELS: LogEntry["level"][] = ["INFO", "WARNING", "ERROR"];

/** An event's source in the current language (read at render time, M7.5); unknown ones as they are. */
function sourceText(source: string): string {
  switch (source) {
    case "scan":
      return t("扫描");
    case "perm":
      return t("权限");
    case "monitor":
      return t("监控");
    case "delete":
      return t("删除");
    case "snapshot":
      return t("快照");
    case "export":
      return t("导出");
    case "system":
      return t("系统");
    default:
      return source;
  }
}

const levelText = (level: LogEntry["level"]): string => ({ INFO: t("信息"), WARNING: t("警告"), ERROR: t("错误") })[level];

export function LogPanel() {
  const open = useApp((s) => s.logOpen);
  if (!open) return <div className="logdock" />;
  return (
    <section className="logdock open" aria-label={t("日志面板")}>
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
        <div className="segtabs small" role="tablist" aria-label={t("停靠面板")}>
          <button role="tab" aria-selected={tab === "log"} className={tab === "log" ? "on" : ""} onClick={() => set({ logTab: "log" })}>
            <Icon name="log" size={14} />
            {t("日志")}
          </button>
          <button role="tab" aria-selected={tab === "stats"} className={tab === "stats" ? "on" : ""} onClick={() => set({ logTab: "stats" })}>
            {t("扫描统计")}
          </button>
        </div>
        <span className="log-summary">
          {t("本次会话 · {n} 条 ·", { n: fmtInt(logs.length) })}{" "}
          <button className={`link${onlyWarnings ? " on" : ""}`} aria-pressed={onlyWarnings} title={onlyWarnings ? t("显示全部事件") : t("只看警告和错误")} onClick={() => setOnlyWarnings(!onlyWarnings)}>
            {t("{n} 条警告", { n: fmtInt(warnings) })}
          </button>
        </span>
        <span className="flex" />
        <label className="switch-row">
          <span>{t("弹出事件时自动打开")}</span>
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
          {t("清空")}
        </button>
        <button className="icon-btn small" aria-label={t("收起日志面板")} title={t("收起日志面板（Ctrl+J）")} onClick={() => set({ logOpen: false, logDismissed: true })}>
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
        aria-label={t("事件日志")}
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
        {logs.length === 0 && <div className="log-empty">{t("暂无事件")}</div>}
      </div>
      {/* new events only are announced, politely (UI-W8-36); the virtualised rows are not a live region */}
      <div className="sr-only" role="log" aria-live="polite">
        {logs.length ? t("{level}：{message}", { level: levelText(logs[logs.length - 1]!.level), message: logs[logs.length - 1]!.message }) : ""}
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
          {t("跳到最新")}
        </button>
      )}
    </div>
  );
}

function LogRow({ id, entry: e, top, active, onPick }: { id: string; entry: LogEntry; top: number; active: boolean; onPick: () => void }) {
  const time = new Date(e.time);
  const hhmmss = Number.isNaN(time.getTime()) ? "" : time.toLocaleTimeString(getLang(), { hour12: false });
  return (
    <div id={id} role="option" aria-selected={active} className={`log-row${active ? " on" : ""} ${e.level.toLowerCase()}`} style={{ top }} onMouseDown={onPick} title={e.message}>
      <span className="log-time mono">{hhmmss}</span>
      <FitCell className="log-level" text={levelText(e.level)} names={LEVELS.map(levelText)} />
      <FitCell className="log-source" text={sourceText(e.source)} names={SOURCES.map(sourceText)} />
      <span className="log-msg">{renderMessage(e.message, e.path)}</span>
      {e.path && (
        <span className="log-actions">
          {e.scanId && (
            <button className="ghost tiny" onClick={() => void locate(e)}>
              {t("在树图中定位")}
            </button>
          )}
          <button className="icon-btn tiny" aria-label={t("复制路径（Ctrl+C）")} title={t("复制路径（Ctrl+C）")} onClick={() => copyPath(e.path)}>
            <Icon name="copy" size={13} />
          </button>
        </span>
      )}
    </div>
  );
}

/**
 * A cell as wide as the longest of `names` in the current language, so a column lines up in every
 * row whatever the language: the names sit stacked out of sight and size the cell. `text` fills it,
 * cut with "…" when longer (a source the panel has no name for).
 */
function FitCell({ className, text, names }: { className: string; text: string; names: string[] }) {
  return (
    <span className={`log-fit ${className}`}>
      {names.map((name, i) => (
        <span key={i} className="log-fit-size" aria-hidden="true">
          {name}
        </span>
      ))}
      <span className="log-fit-text">{text}</span>
    </span>
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
  toast(t("已复制"));
}

/** Locate an event's path in the focused pane of its scan's tab (UI-W8-27). */
async function locate(e: LogEntry): Promise<void> {
  const s = useApp.getState();
  const tab = s.tabs.find((x) => x.scanId === e.scanId);
  if (!tab) {
    toast(t("这次扫描已经关闭"));
    return;
  }
  if (s.active !== tab.id) set({ active: tab.id });
  const viewId = tab.viewIds[tab.focusedPane] ?? tab.viewIds[0];
  try {
    const found = await call("tree.find", { scan_id: e.scanId, path: e.path });
    if (!found.node) {
      toast(t("该路径已不存在"));
      return;
    }
    await viewCall(viewId ?? null, "view.reveal", { key: found.node.id });
    if (viewId && useApp.getState().views[viewId]?.filter?.active) toast(t("该项可能被当前过滤隐藏"));
  } catch {
    toast(t("无法定位该路径"));
  }
}

/** 扫描统计 (UI-W8-33, W8 待定 4): the scan of the active tab, live while it runs. */
function ScanStats() {
  const tab = useApp((s) => activeTab(s));
  const scan = useApp((s) => (tab.scanId ? s.scans[tab.scanId] : undefined));
  if (!scan) return <div className="log-empty">{t("当前标签没有扫描")}</div>;
  const running = scan.state === "scanning" || scan.state === "queued";
  const state = { queued: t("排队中"), scanning: t("扫描中"), done: t("已完成"), cancelled: t("已停止（结果保留）"), failed: t("失败") }[scan.state];
  const rows: [string, React.ReactNode][] = [
    // 「快照」: the one snapshot this tab shows (as a log source and elsewhere: the snapshots, plural)
    [t("扫描方式"), scan.source === "snapshot" ? byLang("快照", "Snapshot") : scan.method === "mft" ? t("MFT 快速扫描") : t("硬扫描")],
    [t("状态"), scan.paused && running ? t("已暂停") : state],
    [t("开始"), scan.started_at ? fmtDateTime(Date.parse(scan.started_at) / 1000) : "—"],
    [t("结束"), scan.finished_at ? fmtDateTime(Date.parse(scan.finished_at) / 1000) : running ? t("进行中") : "—"],
    [t("耗时"), fmtDuration(scan.elapsed_s)],
    [t("文件 / 文件夹"), `${fmtInt(scan.files)} / ${fmtInt(scan.dirs)}`],
  ];
  const right: [string, React.ReactNode][] = [
    [t("已扫描合计"), fmtSize(scan.bytes_alloc)],
    [t("未知空间"), scan.unknown != null ? fmtSize(scan.unknown) : t("—（不是整卷扫描）")],
    [
      t("无法访问"),
      scan.errors > 0 ? (
        <button className="link" onClick={() => set({ logTab: "log" })}>
          {t("{n} 个文件夹", { n: fmtInt(scan.errors) })}
        </button>
      ) : (
        "0"
      ),
    ],
    [t("平均吞吐"), scan.elapsed_s > 0 ? t("{rate} 项/秒 · {size}/秒", { rate: fmtInt(Math.round(scan.items / scan.elapsed_s)), size: fmtSize(scan.bytes_alloc / scan.elapsed_s) }) : "—"],
    [t("磁盘负载历史"), <span className="muted">{t("M4 接入")}</span>],
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
        <Kbd>Ctrl+J</Kbd> {t("显示 / 隐藏日志面板")}
      </div>
    </div>
  );
}
