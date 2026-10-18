// W2 (UI-W2-10…50): the progress band above the treemap and the 「扫描详情」 side panel while a scan
// runs — scan method and the MFT switch, the disk-load limit with its 60-second curve, concurrency,
// pause / continue.

import { useEffect, useState } from "react";

import type { IoStatus, ScanStatus } from "@/protocol/generated";
import { call, pauseScan, resumeScan, setScanLoad, useMft } from "@/state/actions";
import { type LoadLevel, set, setting, toast, useApp } from "@/state/store";
import { fmtInt, fmtKeys, fmtSize, middlePath } from "@/lib/format";
import { Icon, Kbd } from "@/ui/Icon";

export const isRunning = (s: ScanStatus | undefined) => s?.state === "scanning" || s?.state === "queued";

/** 「约 14 秒」「约 3 分钟」「约 1 小时 20 分」 (UI-W2-15): rounded first, then split. */
export function fmtRemaining(seconds: number): string {
  const s = Math.max(1, Math.round(seconds));
  if (s < 60) return `约 ${s} 秒`;
  const minutes = Math.round(s / 60);
  if (minutes < 60) return `约 ${minutes} 分钟`;
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return m ? `约 ${h} 小时 ${m} 分` : `约 ${h} 小时`;
}

/** Progress as shown while scanning (UI-W2-10): rounded, at most 99% until the scan is done. */
export function scanPercent(progress: number): number {
  return Math.min(99, Math.round(progress * 100));
}

/** Items per second: hundreds for fast scans, exact for slow ones (UI-W2-14). */
export function fmtRate(rate: number): string {
  return fmtInt(rate >= 1000 ? Math.round(rate / 100) * 100 : Math.round(rate));
}

export const LEVELS: { id: LoadLevel; label: string }[] = [
  { id: "max", label: "极速" },
  { id: "balanced", label: "均衡 70%" },
  { id: "quiet", label: "安静 35%" },
  { id: "idle", label: "空闲" },
  { id: "custom", label: "自定义" },
];

/** 「均衡 ≤70%」 for the status bar; 极速 and 空闲 carry no percentage (UI-W1-13). */
export function levelText(io: Pick<IoStatus, "level" | "limit">): string {
  const name = { max: "极速", balanced: "均衡", quiet: "安静", idle: "空闲", custom: "自定义" }[io.level];
  return io.level === "max" || io.level === "idle" ? name : `${name} ≤${Math.round(io.limit)}%`;
}

const PHASE_ZH: Record<string, string> = {
  helper: "正在等待管理员授权…",
  mft: "正在读取 MFT 主文件表…",
  receive: "正在接收扫描结果…",
  build: "正在建立目录树…",
};

function targetName(scan: ScanStatus, title: string): string {
  const root = scan.roots[0];
  if (!root) return title;
  if (root.whole_volume) return root.path.replace(/[\\/]+$/, "");
  return root.path.split(/[\\/]/).filter(Boolean).pop() ?? root.path;
}

/** The band (UI-W2-10…21): percentage, figures, current path, progress bar, pause / continue, stop. */
export function ScanBand({ scan, title }: { scan: ScanStatus; title: string }) {
  const mft = scan.method === "mft";
  // an MFT scan's share comes from how much of the $MFT was read (then receiving and building)
  const pct = scan.progress != null ? scanPercent(scan.progress) : null;
  const used = scan.roots.every((r) => r.whole_volume && r.volume) ? scan.roots.reduce((a, r) => a + (r.volume ? r.volume.total - r.volume.free : 0), 0) : null;
  const name = targetName(scan, title);
  const figures = used != null ? `已扫描 ${fmtSize(scan.bytes_alloc)} / 已用 ${fmtSize(used)}` : `已扫描 ${fmtSize(scan.bytes_alloc)}`;
  const label = mft ? PHASE_ZH[scan.phase] ?? "MFT 快速扫描" : `${pct != null ? `${pct}%，` : ""}${figures}`;
  return (
    <div className={`scanband${scan.paused ? " paused" : ""}`} role="group" aria-label="扫描进度">
      <div className="sb-pct mono" aria-hidden="true">
        {pct != null ? `${pct}%` : "…"}
      </div>
      <div className="sb-mid">
        <div className="sb-line">
          <span className="strong">{scan.state === "queued" ? `等待扫描 ${name}` : `正在扫描 ${name}`}</span>
          {mft ? (
            <>
              <span className="sb-mft">
                <Icon name="bolt" size={13} /> MFT 快速扫描
              </span>
              <span className="dot" />
              <span>{PHASE_ZH[scan.phase] ?? "正在读取…"}</span>
              {scan.phase === "mft" && (
                <>
                  <span className="dot" />
                  <span>{scan.remaining_s != null ? `剩余${fmtRemaining(scan.remaining_s)}` : "正在估算…"}</span>
                </>
              )}
              {scan.phase !== "build" && <span className="muted">读完后一次建好目录树</span>}
            </>
          ) : (
            <>
              <span>{figures}</span>
              <span className="dot" />
              <span className="mono">{fmtInt(scan.items)} 项</span>
              {scan.expected_items != null && used == null && <span className="muted">（上次 {fmtInt(scan.expected_items)} 项）</span>}
              <span className="dot" />
              {scan.paused ? (
                <span>磁盘读取已暂停，可随时继续</span>
              ) : (
                <>
                  <span>每秒 {fmtRate(scan.rate)} 项</span>
                  <span className="dot" />
                  <span>{scan.remaining_s != null ? `剩余${fmtRemaining(scan.remaining_s)}` : "正在估算…"}</span>
                </>
              )}
            </>
          )}
          <span className="flex" />
          {scan.current_path && !scan.paused && (
            <span className="sb-current" title={scan.current_path}>
              当前 <span className="mono">{middlePath(scan.current_path, 48)}</span>
            </span>
          )}
        </div>
        <div
          className={`sb-bar${pct == null ? " indeterminate" : ""}`}
          role="progressbar"
          aria-label={label}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct ?? undefined}
          aria-valuetext={scan.paused ? `${label}，已暂停` : label}
        >
          <span style={pct != null ? { width: `${pct}%` } : undefined} />
        </div>
      </div>
      <div className="sb-actions">
        {scan.paused ? (
          <button className="primary" onClick={() => void resumeScan(scan.scan_id)}>
            <Icon name="play" size={14} />
            继续
          </button>
        ) : (
          <button className="ghost" disabled={mft || scan.state !== "scanning"} title={mft ? "MFT 快速扫描几秒内完成，无需暂停" : undefined} onClick={() => void pauseScan(scan.scan_id)}>
            <Icon name="pause" size={14} />
            暂停
          </button>
        )}
        <button
          className="ghost"
          onClick={() => {
            void call("scan.cancel", { scan_id: scan.scan_id }).catch((e) => toast(String(e)));
          }}
        >
          <Icon name="stop" size={14} />
          停止
        </button>
      </div>
    </div>
  );
}

/** 「扫描详情」 (UI-W2-23…50). */
export function ScanDetails({ scan }: { scan: ScanStatus }) {
  const denied = scan.errors;
  return (
    <aside className="inspector scandetails" aria-label="扫描详情">
      <div className="res-head">
        <h2>扫描详情</h2>
        <span className="flex" />
        <button className="icon-btn small" aria-label="收起侧栏" title="收起侧栏" onClick={() => set({ inspectorHidden: true })}>
          <Icon name="sidebar" />
        </button>
      </div>
      <Method scan={scan} />
      {scan.io && <Load scan={scan} io={scan.io} />}
      {denied > 0 && (
        <section className="sd-section">
          <div className="sd-problem">
            <span className="warn">
              <Icon name="warning" size={16} />
            </span>
            <span className="flex">{fmtInt(denied)} 个文件夹无法访问</span>
            <button className="ghost tiny" onClick={() => set({ logOpen: true, logTab: "log", logDismissed: false })}>
              <Icon name="log" size={13} />
              查看日志
            </button>
          </div>
          {scan.denied[0] && (
            <div className="muted small">
              已计入未知空间，例如
              <div className="mono sd-path" title={scan.denied.slice(0, 5).join("\n")}>
                {scan.denied[0]}
              </div>
            </div>
          )}
        </section>
      )}
      <span className="flex" />
      <div className="sd-tip">
        <Icon name="info" size={16} />
        <div>
          <div className="strong">扫描中可以随时放大和导航</div>
          <div className="muted small sd-keys">
            <span>
              <Kbd>{fmtKeys("Enter")}</Kbd> 放大进入
            </span>
            <span>
              <Kbd>{fmtKeys("Backspace")}</Kbd> 后退
            </span>
            <span>
              <Kbd>{fmtKeys("Ctrl+Home")}</Kbd> 根
            </span>
          </div>
        </div>
      </div>
    </aside>
  );
}

/** UI-W2-23…26: the method actually used and why; the one-click switch to the MFT fast scan. */
function Method({ scan }: { scan: ScanStatus }) {
  const platform = window.sk.platform;
  const mft = scan.method === "mft";
  const waiting = scan.phase === "helper";
  const canSwitch = scan.mft_available && !mft && isRunning(scan);
  // a whole volume reads its $MFT in about a second per million entries, plus starting the helper
  const estimate = Math.max(5, Math.round(((scan.expected_items ?? 2_000_000) / 1_000_000) * 1.2 + 3));
  const reason = platform === "win32" ? (scan.mft_available ? "未获得管理员权限" : "目标不是整个本地 NTFS 卷") : "";
  return (
    <section className="sd-section">
      <h3>扫描方式</h3>
      <div className="sd-method">
        <span className={`sd-method-icon${mft ? " fast" : ""}`}>
          <Icon name={mft ? "bolt" : "folder"} size={16} />
        </span>
        <div>
          <div className="strong">{mft ? "MFT 快速扫描（只读提权助手）" : reason ? `硬扫描（${reason}）` : "并行硬扫描"}</div>
          <div className="muted small">{mft ? "直接读取 NTFS 主文件表，整盘几秒完成；主程序不提权" : "逐目录遍历，耗时随文件数量增长"}</div>
        </div>
      </div>
      {canSwitch && (
        <button className="primary sd-elevate" disabled={waiting} onClick={() => void useMft(scan.scan_id)}>
          <Icon name="shield" size={15} />
          <span className="sd-elevate-text">
            <span>{waiting ? "正在等待管理员授权…" : "以管理员身份启用 MFT 快速扫描"}</span>
            <span className="small">约 {estimate} 秒完成 · 仅启动只读扫描助手</span>
          </span>
        </button>
      )}
      {scan.fallback && (
        <div className="sd-note muted small" role="status">
          <Icon name="warning" size={13} /> {scan.fallback}
        </div>
      )}
    </section>
  );
}

/** UI-W2-27…45: the load limit, live load, the 60-second curve, concurrency. */
function Load({ scan, io }: { scan: ScanStatus; io: IoStatus }) {
  const customDefault = useApp((s) => setting(s, "scan.custom_limit", 70));
  const [custom, setCustom] = useState(io.level === "custom" ? Math.round(io.limit) : customDefault);
  useEffect(() => {
    if (io.level === "custom") setCustom(Math.round(io.limit));
  }, [io.level, io.limit]);
  const pick = (level: LoadLevel) => {
    if (level === io.level && level !== "custom") return;
    void setScanLoad(scan.scan_id, level, level === "custom" ? custom : null, io.auto ? null : io.concurrency);
  };
  const onKey = (e: React.KeyboardEvent) => {
    const i = LEVELS.findIndex((l) => l.id === io.level);
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    pick(LEVELS[(i + step + LEVELS.length) % LEVELS.length]!.id);
  };
  const mft = scan.method === "mft";
  return (
    <section className="sd-section">
      <div className="row-between">
        <h3>磁盘负载上限</h3>
        <span className="small" title="SpaceKnife 最近 1 秒在这块磁盘上造成的读取占用（受上限控制）">
          {io.measured ? (
            <>
              当前 <span className="strong mono">{Math.round(io.self_pct)}%</span>
              <span className="muted"> · 整盘 {Math.round(io.total_pct)}%</span>
              <span className="muted" title={ACTIVE_TIP}>
                {" "}
                · 活动时间 {Math.round(io.active_pct)}%
              </span>
            </>
          ) : (
            <span className="muted">无法测量此磁盘</span>
          )}
        </span>
      </div>
      <div className="segmented small sd-levels" role="radiogroup" aria-label="磁盘负载上限" onKeyDown={onKey}>
        {LEVELS.map((l) => (
          <button key={l.id} role="radio" aria-checked={io.level === l.id} tabIndex={io.level === l.id ? 0 : -1} className={io.level === l.id ? "on" : ""} onClick={() => pick(l.id)}>
            {l.id === "custom" && io.level === "custom" ? `自定义 ${Math.round(io.limit)}%` : l.label}
          </button>
        ))}
      </div>
      {io.level === "custom" && (
        <div className="set-slider">
          <input
            type="range"
            aria-label="自定义上限"
            min={5}
            max={100}
            step={5}
            value={custom}
            onChange={(e) => setCustom(Number(e.target.value))}
            onPointerUp={() => void setScanLoad(scan.scan_id, "custom", custom, io.auto ? null : io.concurrency)}
            onKeyUp={() => void setScanLoad(scan.scan_id, "custom", custom, io.auto ? null : io.concurrency)}
          />
          <span className="set-value mono">
            {custom}
            <span className="muted"> %</span>
          </span>
        </div>
      )}
      {io.yielding && (
        <div className="sd-note small warn-text" role="status">
          <Icon name="warning" size={13} /> 其他程序正在大量读写这块磁盘，扫描已让路（最低进度）
        </div>
      )}
      <LoadCurve io={io} startedAt={scan.started_at} paused={scan.paused} />
      {!mft && (
        <div className="row-between sd-concurrency">
          <span className="small">
            并发 <span className="strong mono">{io.concurrency}</span>
            <span className="muted"> · 进行中 {io.inflight}</span>
          </span>
          <span className="sd-auto">
            {!io.auto && (
              <span className="stepper" aria-label="手动并发数">
                <button aria-label="减少并发" disabled={io.concurrency <= 1} onClick={() => void setScanLoad(scan.scan_id, io.level, io.level === "custom" ? custom : null, io.concurrency - 1)}>
                  <Icon name="minus" size={13} />
                </button>
                <span className="stepper-mid">
                  <span className="num">{io.concurrency}</span>
                </span>
                <button aria-label="增加并发" disabled={io.concurrency >= 64} onClick={() => void setScanLoad(scan.scan_id, io.level, io.level === "custom" ? custom : null, io.concurrency + 1)}>
                  <Icon name="plus" size={13} />
                </button>
              </span>
            )}
            <span className="small">自动调节</span>
            <button
              role="switch"
              aria-checked={io.auto}
              aria-label="自动调节并发"
              className={`switch${io.auto ? " on" : ""}`}
              onClick={() => void setScanLoad(scan.scan_id, io.level, io.level === "custom" ? custom : null, io.auto ? io.concurrency : null)}
            >
              <span />
            </button>
          </span>
        </div>
      )}
      {mft && <p className="muted small">MFT 读取是一次顺序读取，按档位放慢读取节奏，不分并发。</p>}
    </section>
  );
}

const W = 300;
const H = 96;
const PAD_T = 6;
const PAD_B = 14;

/** UI-W2-38…43: 「磁盘读取占用 · 近 60 秒」 — whole-disk area, own load line, dashed limit. */
function LoadCurve({ io, startedAt, paused }: { io: IoStatus; startedAt: string | null; paused: boolean }) {
  const hist = io.history;
  const now = hist.length ? hist[hist.length - 1]!.t : Date.now() / 1000;
  const x = (t: number) => W * (1 - (now - t) / 60);
  const y = (pct: number) => PAD_T + (H - PAD_T - PAD_B) * (1 - Math.min(100, Math.max(0, pct)) / 100);
  const pts = hist.filter((p) => now - p.t <= 60);
  const total = pts.length ? `M ${x(pts[0]!.t)} ${y(0)} ${pts.map((p) => `L ${x(p.t).toFixed(1)} ${y(p.total_pct).toFixed(1)}`).join(" ")} L ${x(pts[pts.length - 1]!.t)} ${y(0)} Z` : "";
  const own = pts.map((p, i) => `${i ? "L" : "M"} ${x(p.t).toFixed(1)} ${y(p.self_pct).toFixed(1)}`).join(" ");
  const active = pts.map((p, i) => `${i ? "L" : "M"} ${x(p.t).toFixed(1)} ${y(p.active_pct).toFixed(1)}`).join(" ");
  const limited = io.level !== "max" && io.level !== "idle" && io.limit < 100;
  const start = startedAt ? Date.parse(startedAt) / 1000 : null;
  const showStart = start != null && now - start < 60 && now - start >= 0;
  const last = pts[pts.length - 1];
  return (
    <figure className="sd-curve">
      <figcaption className="row-between small">
        <span className="muted">磁盘读取占用 · 近 60 秒</span>
        <span className="sd-legend muted">
          <span className="lg-line" /> 占用 <span className="lg-dash" /> 上限{" "}
          <span className="lg-dot" title={ACTIVE_TIP} /> <span title={ACTIVE_TIP}>活动时间</span>
        </span>
      </figcaption>
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={curveSummary(io, start, paused)}>
        {[25, 50, 75].map((g) => (
          <line key={g} className="grid" x1={0} x2={W} y1={y(g)} y2={y(g)} />
        ))}
        {total && <path className="total" d={total} />}
        {active && <path className="active" d={active} />}
        {own && <path className="own" d={own} />}
        {limited && <line className="limit" x1={0} x2={W} y1={y(io.limit)} y2={y(io.limit)} />}
        {showStart && start != null && (
          <g className="start">
            <line x1={x(start)} x2={x(start)} y1={PAD_T} y2={H - PAD_B} />
            <text x={Math.min(W - 44, x(start) + 3)} y={H - 3}>
              扫描开始
            </text>
          </g>
        )}
        {last && <circle className={`now${paused ? " paused" : ""}`} cx={x(last.t)} cy={y(last.self_pct)} r={3} />}
      </svg>
    </figure>
  );
}

/** UI-W2-43: a text summary of the curve for assistive technology. */
export function curveSummary(io: IoStatus, start: number | null, paused: boolean): string {
  const before = io.history.filter((p) => start != null && p.t < start);
  const after = io.history.filter((p) => start == null || p.t >= start).slice(-10);
  const avg = (xs: number[]) => (xs.length ? Math.round(xs.reduce((a, b) => a + b, 0) / xs.length) : 0);
  const parts = ["近 60 秒："];
  if (before.length) parts.push(`扫描开始前整盘读取占用约 ${avg(before.map((p) => p.total_pct))}%；`);
  const self = avg(after.map((p) => p.self_pct));
  const total = avg(after.map((p) => p.total_pct));
  if (paused) parts.push(`扫描已暂停，整盘约 ${total}%`);
  else parts.push(`扫描开始后 SpaceKnife 占用约 ${self}%，整盘约 ${total}%`);
  if (io.level !== "max" && io.level !== "idle") parts.push(self <= io.limit ? `，均低于上限 ${Math.round(io.limit)}%` : `，上限 ${Math.round(io.limit)}%`);
  return parts.join("");
}

/** Why 「活动时间」 is shown but not limited (the load is measured by queue depth, see TODO / native.md). */
export const ACTIVE_TIP = "活动时间：任务管理器的口径，磁盘有请求在处理的时间占比。NVMe 固态盘上几个并发读就会到 100%，所以负载上限按请求队列折算的「占用」计算，活动时间只作参考";

/** Status bar and others: the disk-load state of the scan in a tab, if any. */
export function useScanIo(scanId: string | null | undefined): IoStatus | null {
  return useApp((s) => (scanId ? s.scans[scanId]?.io ?? null : null));
}
