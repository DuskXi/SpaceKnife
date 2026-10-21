// 「本机磁盘」 (machine.disks): every physical disk of this computer, scanned or not, with its
// interface and link (the negotiated rate, and what the disk could do), the volumes on it in disk
// order, and what SpaceKnife knows of each volume from an open scan or its newest snapshot. It opens
// only when asked (the start page, the palette) and reads the disks when shown and on 「刷新」.

import { useCallback, useEffect, useState } from "react";

import type { MachineDisks, PhysicalDisk, VolumeOverview, VolumeScan } from "@/protocol/generated";
import { activate, call, newStartTab, openSnapshot } from "@/state/actions";
import { classHexNow } from "@/lib/classes";
import { patchStartForm, get, useApp } from "@/state/store";
import { MEDIA_ZH } from "@/pages/StartPage";
import { diskLabel, diskMap, diskName, elsewhere, interfaceName, linkBandwidth, linkNotes, linkText, orderedDisks, showMap, summary, volumesOf } from "@/state/machine";
import { fmtAgo, fmtDateTime, fmtInt, fmtSize, fmtSizeAs } from "@/lib/format";
import { byLang, t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

// the last answer, shown at once when the tab is shown again (only the active tab is mounted)
let last: MachineDisks | null = null;

const seconds = (iso: string | null | undefined): number => (iso ? Math.floor(new Date(iso).getTime() / 1000) || 0 : 0);

export function MachinePage() {
  const conn = useApp((s) => s.conn);
  const [data, setData] = useState<MachineDisks | null>(last);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    setLoading(true);
    try {
      last = await call("machine.disks", {});
      setData(last);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => {
    if (conn === "ready") void load();
  }, [conn, load]);

  const sum = data ? summary(data) : null;
  const other = data ? elsewhere(data) : [];
  return (
    <div className="machine">
      <div className="mc-inner">
        <header className="mc-head">
          <div className="mc-head-text">
            <h1>{t("本机磁盘")}</h1>
            <p className="muted">
              {sum && data?.disks.length ? t("{counts}，共 {size}", { counts: sum.counts, size: fmtSize(sum.total, true) }) : t("这台电脑的物理磁盘、接口速率，以及每块盘上的卷")}
            </p>
          </div>
          <div className="mc-head-actions">
            {data && <span className="muted small">{t("读取于 {time}", { time: fmtDateTime(seconds(data.collected_at)) })}</span>}
            <button className="ghost small" disabled={loading || conn !== "ready"} onClick={() => void load()}>
              <Icon name="refresh" size={14} />
              {loading ? t("正在读取…") : t("刷新")}
            </button>
          </div>
        </header>
        {error && (
          <div className="mc-note warn" role="alert">
            <Icon name="warning" size={13} />
            {t("读取磁盘失败：{error}", { error })}
          </div>
        )}
        {!data ? (
          <div className="muted">{t("正在读取磁盘…")}</div>
        ) : (
          <>
            {orderedDisks(data).map((d) => (
              <Disk key={d.id} disk={d} data={data} />
            ))}
            {!data.disks.length && <div className="mc-empty muted">{t("这个系统没有告诉 SpaceKnife 有哪些物理磁盘；下面是它的卷")}</div>}
            {other.length > 0 && (
              <section className="mc-other" aria-label={t("其他位置")}>
                <h2>{t("其他位置")}</h2>
                <p className="muted small">{t("网络位置、虚拟卷，以及找不到所在磁盘的卷")}</p>
                <div className="mc-vols card">
                  {other.map((v) => (
                    <Volume key={v.volume.mount_point} v={v} />
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

function Disk({ disk, data }: { disk: PhysicalDisk; data: MachineDisks }) {
  const vols = volumesOf(data, disk);
  const map = diskMap(disk, data.volumes);
  const link = linkText(disk.link);
  const bandwidth = linkBandwidth(disk.link);
  const name = diskName(disk);
  const empty = disk.size == null; // a card reader's empty slot: its name and its drive letter only
  return (
    <section className={`mc-disk${empty ? " no-medium" : ""}`} aria-label={name}>
      <div className="mc-disk-head">
        <span className="mc-icon">
          <Icon name="drive" size={18} />
        </span>
        <div className="mc-title">
          <div className="mc-name" title={name}>
            {name}
          </div>
          <div className="mc-meta">{[MEDIA_ZH[disk.media] ?? disk.media, disk.size ? fmtSize(disk.size, true) : t("没有插入介质"), diskLabel(disk)].join(" · ")}</div>
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
  const pct = vol.total ? Math.min(100, (vol.used / vol.total) * 100) : 0;
  return (
    <div className="mc-vol">
      <div className="mc-vol-name">
        <span className="mono">{vol.mount_point}</span>
        {vol.label && <span className="mc-vol-label">{vol.label}</span>}
        <span className="muted small">{[vol.fs_type, vol.remote_path].filter(Boolean).join(" · ")}</span>
      </div>
      <div className="mc-vol-use">
        <span className="vol-bar">
          <span style={{ width: `${pct}%` }} />
        </span>
        <span className="mono small">{vol.total ? `${fmtSizeAs(vol.used, vol.total)} / ${fmtSize(vol.total, true)}` : "—"}</span>
      </div>
      <ScanLine v={v} />
    </div>
  );
}

function ScanLine({ v }: { v: VolumeOverview }) {
  const s = v.scan;
  const reason = v.volume.reason;
  return (
    <div className="mc-vol-scan">
      <div className="mc-scan-text">
        {!s ? (
          <span className="muted">{reason || t("还没有扫描过")}</span>
        ) : (
          <>
            <span>{scanSummary(s)}</span>
            {s.classes.length > 0 && (
              <span className="mc-classes">
                {s.classes.map((c) => (
                  <span key={c.class_id} className="mc-class" title={`${className(c.class_id)} · ${fmtInt(c.files)}`}>
                    <span className="chip-swatch" style={{ background: classHexNow(c.class_id) }} />
                    {className(c.class_id)} {fmtSize(c.bytes, true)}
                  </span>
                ))}
              </span>
            )}
            {s.folders.length > 0 && (
              <span className="muted small">
                {t("最大的文件夹：{list}", { list: s.folders.map((f) => `${f.name} ${fmtSize(f.bytes, true)}`).join(byLang("、", ", ")) })}
              </span>
            )}
          </>
        )}
      </div>
      <div className="mc-vol-act">
        <VolumeAction v={v} />
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
