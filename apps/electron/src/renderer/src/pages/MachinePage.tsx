// 「本机磁盘」 (machine.disks): a dashboard of this computer's disks, scanned or not. On top, the
// headline numbers (disks, capacity and room, volumes, the fastest link, findings), the capacity by
// media and the findings; then a card per disk: how full it is, its interface and link (the rate it
// runs at against what the disk supports), its volumes in disk order and what SpaceKnife knows of
// each from an open scan or its snapshots (classes, largest folders, growth). It opens only when
// asked (the start page's card, the palette) and reads the disks when shown and on 「刷新」.

import { useEffect } from "react";

import type { MachineDisks, PhysicalDisk, VolumeOverview, VolumeScan } from "@/protocol/generated";
import { activate, newStartTab, openSnapshot, refreshMachine } from "@/state/actions";
import { classHexNow } from "@/lib/classes";
import { patchStartForm, get, useApp } from "@/state/store";
import { MEDIA_ZH } from "@/pages/StartPage";
import {
  bandwidthText,
  byMedia,
  diskLabel,
  diskMap,
  diskName,
  diskUsed,
  elsewhere,
  fastest,
  findings,
  growth,
  interfaceName,
  linkBandwidth,
  linkNotes,
  linkShare,
  linkText,
  orderedDisks,
  showMap,
  summary,
  totals,
  volumesOf,
} from "@/state/machine";
import { fmtAgo, fmtDateTime, fmtDelta, fmtInt, fmtShare, fmtSize, fmtSizeAs } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { Icon, type IconName } from "@/ui/Icon";
import { Meter, Ring, Sparkline, StackBar } from "@/ui/charts";

const seconds = (iso: string | null | undefined): number => (iso ? Math.floor(new Date(iso).getTime() / 1000) || 0 : 0);
const FULL = 0.9; // a volume this full is a finding (status colour, with its icon and words)

export function MachinePage() {
  const conn = useApp((s) => s.conn);
  const data = useApp((s) => s.machine);
  const loading = useApp((s) => s.machineLoading);
  useEffect(() => {
    if (conn === "ready") void refreshMachine();
  }, [conn]);

  const sum = data ? summary(data) : null;
  const other = data ? elsewhere(data) : [];
  return (
    <div className="machine">
      <div className="mc-inner">
        <header className="mc-head">
          <div className="mc-head-text">
            <h1>{t("本机磁盘")}</h1>
            <p className="muted">{sum && data?.disks.length ? t("{counts}，共 {size}", { counts: sum.counts, size: fmtSize(sum.total, true) }) : t("这台电脑的物理磁盘、接口速率，以及每块盘上的卷")}</p>
          </div>
          <div className="mc-head-actions">
            {data && <span className="muted small">{t("读取于 {time}", { time: fmtDateTime(seconds(data.collected_at)) })}</span>}
            <button className="ghost small" disabled={loading || conn !== "ready"} onClick={() => void refreshMachine(true)}>
              <Icon name="refresh" size={14} />
              {loading ? t("正在读取…") : t("刷新")}
            </button>
          </div>
        </header>
        {!data ? (
          <div className="muted">{t("正在读取磁盘…")}</div>
        ) : (
          <>
            <Kpis data={data} />
            <div className="mc-panels">
              <MediaPanel data={data} />
              <FindingsPanel data={data} />
            </div>
            <h2 className="mc-h2">{t("磁盘")}</h2>
            <div className="mc-grid">
              {orderedDisks(data).map((d) => (
                <Disk key={d.id} disk={d} data={data} />
              ))}
            </div>
            {!data.disks.length && <div className="mc-empty muted">{t("这个系统没有告诉 SpaceKnife 有哪些物理磁盘；下面是它的卷")}</div>}
            {other.length > 0 && (
              <section className="mc-other" aria-label={t("其他位置")}>
                <h2 className="mc-h2">{t("其他位置")}</h2>
                <p className="muted small">{t("网络位置、虚拟卷，以及找不到所在磁盘的卷")}</p>
                <div className="mc-grid">
                  {other.map((v) => (
                    <div key={v.volume.mount_point} className="mc-disk mc-place">
                      <Volume v={v} />
                    </div>
                  ))}
                </div>
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ the summary

function Kpi(props: { icon?: IconName; visual?: React.ReactNode; label: string; value: string; sub: string }) {
  return (
    <div className="mc-kpi">
      <span className="mc-kpi-visual">{props.visual ?? <Icon name={props.icon ?? "info"} size={18} />}</span>
      <span className="mc-kpi-text">
        <span className="mc-kpi-label">{props.label}</span>
        <span className="mc-kpi-value">{props.value}</span>
        <span className="mc-kpi-sub" title={props.sub}>
          {props.sub}
        </span>
      </span>
    </div>
  );
}

function Kpis({ data }: { data: MachineDisks }) {
  const tt = totals(data);
  const fast = fastest(data);
  const room = tt.used + tt.free;
  const used = room ? tt.used / room : 0;
  return (
    <section className="mc-kpis" aria-label={t("汇总")}>
      <Kpi icon="drive" label={t("物理磁盘")} value={fmtInt(tt.disks)} sub={summary(data).counts || "—"} />
      <Kpi
        visual={
          <Ring share={used} size={40} stroke={5} tone={used >= FULL ? "warn" : "accent"} title={t("已用 {pct}", { pct: fmtShare(tt.used, room || 1) })}>
            {Math.round(used * 100)}
          </Ring>
        }
        label={t("容量")}
        value={fmtSize(tt.capacity, true)}
        sub={t("已用 {used} · 可用 {free}", { used: fmtSize(tt.used, true), free: fmtSize(tt.free, true) })}
      />
      <Kpi icon="layers" label={t("卷")} value={fmtInt(tt.volumes)} sub={t("{n} 个有扫描或快照", { n: tt.known })} />
      <Kpi icon="gauge" label={t("最快链路")} value={fast ? (linkText(fast.disk.link) ?? "—") : "—"} sub={fast ? `${bandwidthText(fast.mbps)} · ${diskName(fast.disk)}` : t("系统没有报告链路")} />
    </section>
  );
}

function MediaPanel({ data }: { data: MachineDisks }) {
  const media = byMedia(data);
  const total = media.reduce((s, m) => s + m.bytes, 0);
  return (
    <section className="mc-panel" aria-label={t("容量构成")}>
      <h2>{t("容量构成")}</h2>
      <p className="muted small">{t("按存储类型，越亮越快")}</p>
      {total > 0 ? (
        <>
          <StackBar height={12} title={t("容量构成")} segments={media.map((m) => ({ key: m.tier, share: m.bytes / total, color: m.color, title: `${m.label} · ${fmtSize(m.bytes, true)}` }))} />
          <table className="mc-legend">
            <tbody>
              {media.map((m) => (
                <tr key={m.tier}>
                  <td>
                    <span className="chip-swatch" style={{ background: m.color }} />
                    {m.label}
                  </td>
                  <td className="muted">{t("{n} 块", { n: m.disks })}</td>
                  <td className="mono">{fmtSize(m.bytes, true)}</td>
                  <td className="mono muted">{fmtShare(m.bytes, total)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      ) : (
        <p className="muted small">{t("系统没有报告磁盘容量")}</p>
      )}
    </section>
  );
}

function FindingsPanel({ data }: { data: MachineDisks }) {
  const found = findings(data);
  const warn = found.filter((f) => f.level === "warn").length;
  return (
    <section className="mc-panel" aria-label={t("发现")}>
      <h2>
        {t("发现")} {found.length > 0 && <span className="mono muted">{found.length}</span>}
      </h2>
      {warn > 0 && <p className="muted small">{t("{n} 条需要留意", { n: warn })}</p>}
      {found.length ? (
        <ul className="mc-findings">
          {found.map((f) => (
            <li key={f.text} className={f.level}>
              <Icon name={f.level === "warn" ? "warning" : "info"} size={14} />
              <span>{f.text}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="mc-allgood">
          <Icon name="check" size={14} />
          {t("一切正常：没有快满的卷，每条链路都跑在磁盘支持的速度上")}
        </p>
      )}
    </section>
  );
}

// ------------------------------------------------------------------ the disks

function Disk({ disk, data }: { disk: PhysicalDisk; data: MachineDisks }) {
  const vols = volumesOf(data, disk);
  const map = diskMap(disk, data.volumes);
  const link = linkText(disk.link);
  const bandwidth = linkBandwidth(disk.link);
  const share = linkShare(disk.link);
  const name = diskName(disk);
  const empty = disk.size == null; // a card reader's empty slot: its name and its drive letter only
  const used = !empty ? diskUsed(disk, data) : null;
  const fill = used != null && disk.size ? used / disk.size : null;
  return (
    <section className={`mc-disk${empty ? " no-medium" : ""}`} aria-label={name}>
      <div className="mc-disk-head">
        {fill != null ? (
          <Ring share={fill} size={44} stroke={5} tone={fill >= FULL ? "warn" : "accent"} title={t("已用 {pct}", { pct: fmtShare(used ?? 0, disk.size ?? 1) })}>
            {Math.round(fill * 100)}
          </Ring>
        ) : (
          <span className="mc-icon">
            <Icon name="drive" size={18} />
          </span>
        )}
        <div className="mc-title">
          <div className="mc-name" title={name}>
            {name}
          </div>
          <div className="mc-meta">
            {[MEDIA_ZH[disk.media] ?? disk.media, disk.size ? fmtSize(disk.size, true) : t("没有插入介质"), used != null ? t("已用 {size}", { size: fmtSize(used, true) }) : null, diskLabel(disk)].filter(Boolean).join(" · ")}
          </div>
        </div>
        <div className="mc-tags">
          {disk.system && <span className="mc-tag accent">{t("系统盘")}</span>}
          {disk.removable && <span className="mc-tag">{t("可移动")}</span>}
        </div>
      </div>
      {!empty && (
        <div className="mc-link">
          <span className="mc-chip" title={t("接口")}>
            <Icon name="bolt" size={12} />
            {interfaceName(disk.interface)}
          </span>
          {link && (
            <span className="mc-chip" title={t("当前链路速率")}>
              <Icon name="gauge" size={12} />
              {link}
              {bandwidth && <span className="mc-chip-sub">{bandwidth}</span>}
            </span>
          )}
          {share != null && (
            <span className="mc-linkmeter" title={t("链路速率占这块盘支持的最高速率的比例")}>
              <Meter share={share} tone={share < 1 ? "muted" : "accent"} title={t("链路速率占这块盘支持的最高速率的比例")} />
              <span className="small muted">{share >= 0.999 ? t("满速") : t("最高速率的 {pct}", { pct: `${Math.round(share * 100)}%` })}</span>
            </span>
          )}
        </div>
      )}
      {!empty &&
        linkNotes(disk).map((n) => (
          <div key={n.text} className={`mc-note${n.warn ? " warn" : ""}`}>
            <Icon name={n.warn ? "warning" : "info"} size={13} />
            {n.text}
          </div>
        ))}
      {showMap(map) && (
        <div className="mc-map" aria-hidden="true">
          {map.map((s) => (
            <span key={s.mount} className="mc-seg" style={{ width: `${s.share * 100}%` }} title={s.mount}>
              <span style={{ width: `${s.used * 100}%` }} />
            </span>
          ))}
        </div>
      )}
      {vols.length > 0 && (
        <div className="mc-vols">
          {vols.map((v) => (
            <Volume key={v.volume.mount_point} v={v} />
          ))}
        </div>
      )}
      <details className="mc-details">
        <summary>{t("详细信息")}</summary>
        <dl>
          <dt>{t("型号")}</dt>
          <dd>{disk.model || "—"}</dd>
          {disk.vendor && (
            <>
              <dt>{t("厂商")}</dt>
              <dd>{disk.vendor}</dd>
            </>
          )}
          <dt>{t("序列号")}</dt>
          <dd className="mono">{disk.serial || "—"}</dd>
          <dt>{t("固件")}</dt>
          <dd className="mono">{disk.firmware || "—"}</dd>
          <dt>TRIM</dt>
          <dd>{disk.trim == null ? "—" : disk.trim ? t("支持") : t("不支持")}</dd>
          <dt>{t("系统名称")}</dt>
          <dd className="mono">{disk.id}</dd>
        </dl>
      </details>
    </section>
  );
}

function Volume({ v }: { v: VolumeOverview }) {
  const vol = v.volume;
  const share = vol.total ? Math.min(1, vol.used / vol.total) : 0;
  return (
    <div className="mc-vol">
      <div className="mc-vol-top">
        <div className="mc-vol-name">
          <span className="mono">{vol.mount_point}</span>
          {vol.label && <span className="mc-vol-label">{vol.label}</span>}
          <span className="muted small">{[vol.fs_type, vol.remote_path].filter(Boolean).join(" · ")}</span>
        </div>
        <div className="mc-vol-act">
          <VolumeAction v={v} />
        </div>
      </div>
      <div className="mc-vol-use">
        <Meter share={share} tone={share >= FULL ? "warn" : "muted"} title={vol.total ? t("已用 {pct}", { pct: fmtShare(vol.used, vol.total) }) : undefined} />
        <span className="mono small">
          {vol.total ? `${fmtSizeAs(vol.used, vol.total)} / ${fmtSize(vol.total, true)}` : "—"}
          {vol.total > 0 && <span className={share >= FULL ? "mc-full" : "muted"}> · {fmtShare(vol.used, vol.total)}</span>}
        </span>
      </div>
      <ScanLine v={v} />
    </div>
  );
}

function ScanLine({ v }: { v: VolumeOverview }) {
  const s = v.scan;
  const reason = v.volume.reason;
  const g = growth(v.history);
  return (
    <div className="mc-vol-scan">
      <div className="mc-scan-text">
        {!s ? (
          <span className="muted">{reason || t("还没有扫描过")}</span>
        ) : (
          <>
            <span>{scanSummary(s)}</span>
            {s.classes.length > 0 && s.bytes_alloc > 0 && (
              <>
                <StackBar
                  height={6}
                  title={t("文件类别")}
                  segments={s.classes.map((c) => ({ key: c.class_id, share: c.bytes / s.bytes_alloc, color: classHexNow(c.class_id), title: `${className(c.class_id)} · ${fmtSize(c.bytes, true)}` }))}
                />
                <span className="mc-classes">
                  {s.classes.map((c) => (
                    <span key={c.class_id} className="mc-class" title={`${className(c.class_id)} · ${t("{n} 个文件", { n: fmtInt(c.files) })}`}>
                      <span className="chip-swatch" style={{ background: classHexNow(c.class_id) }} />
                      {className(c.class_id)} {fmtSize(c.bytes, true)}
                    </span>
                  ))}
                </span>
              </>
            )}
            {s.folders.length > 0 && <span className="muted small">{t("最大的文件夹：{list}", { list: s.folders.map((f) => `${f.name} ${fmtSize(f.bytes, true)}`).join(byLang("、", ", ")) })}</span>}
          </>
        )}
        {g && (
          <span className="mc-growth">
            <Sparkline
              values={[...v.history].reverse().map((h) => h.bytes_alloc)}
              title={t("最近 {n} 次快照：{from} → {to}", { n: v.history.length, from: fmtSize(v.history[v.history.length - 1]!.bytes_alloc, true), to: fmtSize(v.history[0]!.bytes_alloc, true) })}
            />
            <span className="small muted">{t("比 {when} 的快照 {delta}", { when: fmtAgo(seconds(g.since)), delta: fmtDelta(g.delta) })}</span>
          </span>
        )}
      </div>
    </div>
  );
}

function scanSummary(s: VolumeScan): string {
  const files = t("{n} 个文件", { n: fmtInt(s.files) });
  if (s.running) return t("正在扫描 · {files}", { files });
  const when = s.at ? fmtAgo(seconds(s.at)) : "";
  const size = fmtSize(s.bytes_alloc, true);
  return s.source === "scan" ? t("扫描于 {when} · {files} · {size}", { when, files, size }) : t("快照 {when} · {files} · {size}", { when, files, size });
}

function className(id: string): string {
  return get().vocab?.classes.find((c) => c.id === id)?.name ?? id;
}

function VolumeAction({ v }: { v: VolumeOverview }) {
  const tabs = useApp((s) => s.tabs);
  const s = v.scan;
  const open = s?.scan_id ? tabs.find((x) => x.scanId === s.scan_id) : undefined;
  if (open) {
    return (
      <button className="ghost small" onClick={() => activate(open.id)}>
        {t("查看")}
      </button>
    );
  }
  if (s?.snapshot && !s.scan_id) {
    return (
      <button className="ghost small" onClick={() => void openSnapshot(s.snapshot!)}>
        <Icon name="snapshot" size={13} />
        {t("打开快照")}
      </button>
    );
  }
  if (v.volume.reason) return null;
  return (
    <button className="ghost small" onClick={() => scanVolume(v.volume.mount_point)}>
      {t("扫描…")}
    </button>
  );
}

/** A start tab with this volume picked: the scan options stay the user's to check. */
function scanVolume(mount: string): void {
  newStartTab();
  patchStartForm(get().active, { picked: mount, path: "", check: null });
}
