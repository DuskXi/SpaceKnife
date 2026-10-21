// Start page (W1, UI-W1-01…51): pick volumes or folders, set this scan's options, start; open a
// recent scan or a snapshot file. The form lives in the store per tab (switching tabs keeps it).

import { useEffect, useMemo, useRef, useState } from "react";

import type { SnapshotInfo, VolumeInfo } from "@/protocol/generated";
import { call, openSnapshot, refreshLists, refreshVolumes, startScan } from "@/state/actions";
import { MachineCard } from "@/pages/MachineCard";
import { type LoadLevel, patchStartForm, type ScanMethod, setting, startFormOf, type Tab, toast, useApp } from "@/state/store";
import { fmtInt, fmtKeys, fmtShare, fmtSize, fmtSizeAs } from "@/lib/format";
import { Icon, type IconName, Kbd } from "@/ui/Icon";
import { compareWithNow } from "@/state/compare";
import { byLang, t } from "@/lib/i18n";

// the labels translate each time they are read (getters), so a language switch applies (M7.5)
export const MEDIA_ZH: Record<string, string> = {
  nvme_ssd: "NVMe SSD",
  sata_ssd: "SATA SSD",
  ssd: "SSD",
  get hdd() {
    return t("机械硬盘");
  },
  usb_hdd: "USB HDD",
  usb_ssd: "USB SSD",
  get usb_flash() {
    return t("U 盘");
  },
  get sd_card() {
    return t("存储卡");
  },
  get network() {
    return t("网络");
  },
  get virtual() {
    return t("虚拟磁盘");
  },
  get unknown() {
    return t("未知介质");
  },
};
export const LOAD_ZH: Record<LoadLevel, [string, string]> = {
  get max(): [string, string] {
    return [t("极速"), t("不限制磁盘读取")];
  },
  get balanced(): [string, string] {
    return [t("均衡 70%"), t("扫描最多占用 70% 的磁盘读取，给前台程序留出余量")];
  },
  get quiet(): [string, string] {
    return [t("安静 35%"), t("扫描最多占用 35% 的磁盘读取，几乎感觉不到")];
  },
  get idle(): [string, string] {
    return [t("空闲"), t("只在磁盘空闲时读取，并使用系统低 I/O 优先级")];
  },
  get custom(): [string, string] {
    return [t("自定义"), t("按设置里的「自定义上限」")];
  },
};
const METHODS: ScanMethod[] = ["auto", "mft", "hard"];
function methodText(m: ScanMethod): [string, string] {
  const table: Record<ScanMethod, [string, string]> = {
    auto: [t("自动"), t("整个 NTFS 卷用 MFT 快速扫描，其他位置逐目录硬扫描")],
    mft: [t("快速 MFT"), t("读取 NTFS 主文件表，整盘几秒完成（需要管理员）")],
    hard: [t("硬扫描"), t("逐目录遍历，耗时随文件数量增长")],
  };
  return table[m];
}
const IS_WIN = () => window.sk.platform === "win32";

/** 「C: 系统」, or the folder name of a mount point on Linux / macOS. */
export function volumeName(v: VolumeInfo): string {
  const mp = v.mount_point.replace(/[\\/]+$/, "") || v.mount_point;
  return v.label && v.label !== mp && v.label !== mp.split("/").pop() ? `${mp} ${v.label}` : mp;
}

/** A whole local NTFS volume on Windows: the MFT fast scan can read it (UI-W1-18). */
export function mftCapable(v: VolumeInfo | null | undefined): boolean {
  return !!v && IS_WIN() && !v.reason && !v.is_remote && v.fs_type.toUpperCase() === "NTFS";
}

/** What this volume will actually be scanned with (UI-W1-18). */
function methodBadge(v: VolumeInfo, method: ScanMethod, elevated: boolean): { icon: IconName; text: string } {
  if (v.reason) return { icon: "warning", text: v.reason };
  if (v.note) return { icon: "warning", text: t("不推荐 · 很慢") };
  if (method !== "hard" && mftCapable(v)) return { icon: "shield", text: elevated ? t("MFT 快速扫描") : t("MFT 快速扫描 · 需要管理员") };
  if (v.is_remote) return { icon: "clock", text: t("网络扫描 · 较慢") };
  if (v.media === "usb_hdd") return { icon: "gauge", text: t("并行硬扫描 · USB HDD 自动降速") };
  return { icon: "gauge", text: t("并行硬扫描") };
}

export function StartPage({ tab }: { tab: Tab }) {
  const volumes = useApp((s) => s.volumes);
  const conn = useApp((s) => s.conn);
  const form = useApp((s) => startFormOf(s, tab.id));
  const [refreshing, setRefreshing] = useState(false);
  const [starting, setStarting] = useState(false);
  const [dragging, setDragging] = useState(false);
  const pathInput = useRef<HTMLInputElement>(null);
  const patch = (p: Parameters<typeof patchStartForm>[1]) => patchStartForm(tab.id, p);

  // lists on connect; volumes every few seconds so inserted / removed drives show up (UI-W1-21)
  useEffect(() => {
    if (conn !== "ready") return;
    void refreshLists();
    const timer = setInterval(() => void refreshVolumes(), 5000);
    return () => clearInterval(timer);
  }, [conn]);

  const paths = useMemo(
    () =>
      form.path
        .split(";")
        .map((x) => x.trim())
        .filter(Boolean),
    [form.path],
  );
  // a path that is exactly a volume root selects that card (UI-W1-24)
  const rootMatch = paths.length === 1 ? volumes.find((v) => sameRoot(v.mount_point, paths[0]!)) : undefined;
  const selected = paths.length === 0 ? volumes.find((v) => v.mount_point === form.picked && !v.reason) : rootMatch;

  // validate typed paths shortly after typing stops (UI-W1-25)
  useEffect(() => {
    if (!paths.length || rootMatch) {
      if (form.check) patch({ check: null });
      return;
    }
    const timer = setTimeout(() => {
      void call("scan.check_paths", { paths })
        .then((check) => patch({ check }))
        .catch(() => {});
    }, 300);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form.path, rootMatch?.mount_point]);

  const check = form.check && paths.length && !rootMatch ? form.check : null;
  const errors = check?.items.filter((i) => i.state !== "ok") ?? [];
  const merged = check?.items.filter((i) => i.merged_into) ?? [];
  const roots = selected ? [selected.mount_point] : paths.length ? (check && check.items.length === paths.length && !errors.length ? check.roots : []) : [];
  const ready = conn === "ready" && roots.length > 0;
  const targetName = selected ? volumeName(selected) : roots.length > 1 ? t("{n} 个位置", { n: roots.length }) : roots[0]?.split(/[\\/]/).filter(Boolean).pop() ?? "";
  const elevated = useApp((s) => s.volumesElevated);
  const mayElevate = useApp((s) => setting(s, "scan.elevate", true));
  // the MFT fast scan reads a whole local NTFS volume; it may need the administrator prompt (UI-W1-34)
  const viaMft = form.method !== "hard" && mftCapable(selected) && (elevated || mayElevate);
  const needsAdmin = viaMft && !elevated;

  const start = () => {
    if (!ready || starting) return;
    const capable = mftCapable(selected);
    setStarting(true);
    void startScan({ roots, method: capable ? form.method : "hard", load_level: form.load, ads: capable && form.ads }, tab.id).finally(() =>
      setStarting(false),
    );
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key !== "Enter") return;
    const el = e.target as HTMLElement;
    // Enter starts the scan unless another button has the focus (UI-W1-35)
    if (el.tagName === "BUTTON" && !el.closest(".volumes, .start-options")) return;
    if (el.tagName === "INPUT" && el !== pathInput.current) return;
    e.preventDefault();
    start();
  };

  // drag folders or snapshot files anywhere on the page (UI-W1-27, UI-W1-44)
  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const items = [...e.dataTransfer.items];
    const files = [...e.dataTransfer.files];
    const folders: string[] = [];
    for (let i = 0; i < files.length; i++) {
      const file = files[i]!;
      const path = window.sk.files.pathOf(file);
      if (!path) {
        toast(t("浏览器模式下无法读取拖入文件的路径"));
        return;
      }
      const isDir = items[i]?.webkitGetAsEntry()?.isDirectory ?? false;
      if (isDir) folders.push(path);
      else if (/\.sks$/i.test(path)) {
        void openSnapshot(path, tab.id);
        return;
      } else {
        toast(t("只接受文件夹或 SpaceKnife 快照文件（.sks）"));
        return;
      }
    }
    if (folders.length) patch({ path: folders.join("; "), picked: null });
  };

  return (
    <div
      className={`start${dragging ? " dragging" : ""}`}
      onKeyDown={onKey}
      onDragOver={(e) => {
        e.preventDefault();
        if (!dragging) setDragging(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDragging(false);
      }}
      onDrop={onDrop}
    >
      <div className="start-cols">
        <section className="start-main" aria-label={t("选择要扫描的位置")}>
          <div className="start-scroll">
            <div className="start-title">
              <h1>{t("选择要扫描的位置")}</h1>
              <button
                className="ghost small"
                disabled={refreshing}
                onClick={() => {
                  setRefreshing(true);
                  void refreshVolumes().finally(() => setRefreshing(false));
                }}
              >
                <Icon name="refresh" size={14} />
                {refreshing ? t("正在刷新…") : t("刷新")}
              </button>
            </div>
            <Volumes volumes={volumes} selected={selected?.mount_point ?? null} method={form.method} elevated={elevated} onPick={(mp) => patch({ picked: mp, path: "", check: null })} />
            <div className="path-area">
              <div className="path-label">
                <span>{t("或输入路径（多个路径用 ; 分隔）")}</span>
                <span className="muted small">
                  <Icon name="upload" size={13} /> {t("也可以把文件夹直接拖进窗口")}
                </span>
              </div>
              <div className="path-row">
                <span className="path-icon">
                  <Icon name="folder" />
                </span>
                <input
                  ref={pathInput}
                  className="path-input"
                  placeholder={t("例如 {paths}", { paths: IS_WIN() ? "C:\\Users\\Public\\Videos; D:\\Downloads" : "~/Videos; ~/Downloads" })}
                  spellCheck={false}
                  aria-label={t("扫描路径")}
                  aria-invalid={errors.length > 0}
                  value={form.path}
                  onChange={(e) => patch({ path: e.target.value })}
                  onKeyDown={(e) => {
                    e.stopPropagation(); // typing is not a shortcut
                    if (e.key === "Enter" && !e.nativeEvent.isComposing) {
                      e.preventDefault();
                      start(); // Enter in the path box starts the scan (UI-W1-28)
                    }
                  }}
                />
                <button
                  className="ghost"
                  onClick={async () => {
                    const picked = await window.sk.dialog.openFolders();
                    if (picked.length) patch({ path: [...paths, ...picked].join("; "), picked: null });
                  }}
                >
                  {t("浏览…")}
                </button>
              </div>
              {errors.map((e) => (
                <div key={e.path} className="path-error" role="alert">
                  <Icon name="warning" size={13} />
                  <span className="mono">{e.path}</span>
                  {byLang("：", ": ")}
                  {e.message}
                </div>
              ))}
              {merged.length > 0 && !errors.length && (
                <div className="path-note">
                  <Icon name="info" size={13} />
                  {t("{items}，将合并扫描", {
                    items: merged.map((m) => t("{path} 已包含在 {parent} 中", { path: m.path, parent: m.merged_into ?? "" })).join(byLang("；", "; ")),
                  })}
                </div>
              )}
            </div>
            <Options method={form.method} load={form.load} ads={form.ads} volume={selected ?? null} elevated={elevated} onChange={patch} />
          </div>
          <div className="start-bar">
            <div className="start-summary">
              <div>
                {targetName ? (
                  <>
                    <strong>{targetName}</strong> · {mftCapable(selected) ? methodText(form.method)[0] : t("并行硬扫描")}
                    {mftCapable(selected) && form.method === "auto" && (viaMft ? t("（MFT 快速扫描）") : t("（并行硬扫描）"))}
                    {" · "}
                    {t("磁盘负载上限 {level}", { level: LOAD_ZH[form.load][0].replace(/ (\d+%)/, " ≤$1") })}
                    {form.ads && viaMft && ` · ${t("备用数据流")}`}
                  </>
                ) : (
                  <span className="muted">{t("选择一个卷，或输入要扫描的文件夹")}</span>
                )}
              </div>
              {selected?.note && <div className="warn-text small">{selected.note}</div>}
              {needsAdmin && (
                <div className="muted small">
                  <Icon name="shield" size={13} /> {t("需要管理员权限：仅启动只读扫描助手，主程序不提权")}
                </div>
              )}
            </div>
            <button className="primary big" disabled={!ready || starting} aria-busy={starting} onClick={start}>
              <Icon name="play" />
              {starting ? t("正在开始…") : t("开始扫描")}
              <Kbd>Enter</Kbd>
            </button>
          </div>
        </section>
        <Recents tab={tab} />
      </div>
      {dragging && (
        <div className="drop-hint" aria-hidden="true">
          <Icon name="upload" size={28} />
          {t("松开以添加文件夹，或打开快照文件")}
        </div>
      )}
    </div>
  );
}

function sameRoot(a: string, b: string): boolean {
  const norm = (x: string) => {
    const p = x.trim().replace(/[\\/]+$/, "");
    return IS_WIN() ? p.toLowerCase() : p || "/";
  };
  return norm(a) === norm(b);
}

function Volumes(props: { volumes: VolumeInfo[]; selected: string | null; method: ScanMethod; elevated: boolean; onPick: (mp: string) => void }) {
  const { volumes, selected, method, elevated, onPick } = props;
  const group = useRef<HTMLDivElement>(null);
  const usable = volumes.filter((v) => !v.reason);
  const onKey = (e: React.KeyboardEvent) => {
    const step = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: 2, ArrowUp: -2 }[e.key];
    if (!step || !usable.length) return;
    e.preventDefault();
    const at = Math.max(0, usable.findIndex((v) => v.mount_point === selected));
    const next = usable[Math.max(0, Math.min(usable.length - 1, at + step))]!;
    onPick(next.mount_point);
    group.current?.querySelector<HTMLElement>(`[data-mp="${CSS.escape(next.mount_point)}"]`)?.focus();
  };
  if (!volumes.length) return <div className="volumes-empty muted">{t("正在读取卷…")}</div>;
  return (
    <div ref={group} className="volumes" role="radiogroup" aria-label={t("卷")} onKeyDown={onKey}>
      {volumes.map((v) => {
        const on = v.mount_point === selected;
        const badge = methodBadge(v, method, elevated);
        const pct = v.total ? Math.round((v.used / v.total) * 100) : 0;
        return (
          <button
            key={v.mount_point}
            data-mp={v.mount_point}
            role="radio"
            aria-checked={on}
            aria-disabled={!!v.reason}
            tabIndex={on || (!selected && v === volumes[0]) ? 0 : -1}
            className={`volume${on ? " on" : ""}${v.reason ? " unavailable" : ""}`}
            title={v.remote_path || v.mount_point}
            onClick={() => !v.reason && onPick(v.mount_point)}
          >
            <span className="vol-top">
              <span className="vol-icon">
                <Icon name="drive" size={18} />
              </span>
              <span className="vol-text">
                <span className="vol-name">
                  {volumeName(v)}
                  {v.remote_path && <span className="vol-remote mono">{v.remote_path}</span>}
                </span>
                <span className="vol-meta">
                  {[v.fs_type || "—", MEDIA_ZH[v.media] ?? v.media, v.total ? fmtSize(v.total, true) : "—"].join(" · ")}
                </span>
              </span>
              <span className={`vol-radio${on ? " on" : ""}`}>{on && <Icon name="check" size={12} strokeWidth={3} />}</span>
            </span>
            <span className="vol-bar">
              <span style={{ width: `${v.total ? (v.used / v.total) * 100 : 0}%` }} />
            </span>
            <span className="vol-figures mono">
              <span>{v.total ? t("已用 {size} · {pct}%", { size: fmtSizeAs(v.used, v.total), pct }) : t("已用 —")}</span>
              <span>{v.total ? t("可用 {size}", { size: fmtSizeAs(v.free, v.total) }) : t("可用 —")}</span>
            </span>
            <span className={`vol-badge${v.reason || v.note ? " warn" : ""}`} title={v.note || undefined}>
              <Icon name={badge.icon} size={13} />
              {badge.text}
            </span>
          </button>
        );
      })}
    </div>
  );
}

function Options(props: {
  method: ScanMethod;
  load: LoadLevel;
  ads: boolean;
  volume: VolumeInfo | null;
  elevated: boolean;
  onChange: (p: { method?: ScanMethod; load?: LoadLevel; ads?: boolean }) => void;
}) {
  // the method and the NTFS streams only mean something for a whole local NTFS volume on Windows:
  // anywhere else (Linux, macOS, ReFS / Btrfs / exFAT drives, folders) a hard scan is the only way
  const capable = mftCapable(props.volume);
  return (
    <div className="start-options">
      <h2>{t("扫描选项")}</h2>
      <div className="set-card">
        {capable && (
          <div className="set-row">
            <div className="set-text">
              <div className="set-name">{t("扫描方式")}</div>
              <div className="set-desc">{methodText(props.method)[1]}</div>
            </div>
            <div className="segmented small" role="radiogroup" aria-label={t("扫描方式")}>
              {METHODS.map((m) => (
                <button
                  key={m}
                  role="radio"
                  aria-checked={props.method === m}
                  className={props.method === m ? "on" : ""}
                  onClick={() => props.onChange({ method: m })}
                >
                  {methodText(m)[0]}
                </button>
              ))}
            </div>
          </div>
        )}
        <div className="set-row">
          <div className="set-text">
            <div className="set-name">{t("磁盘负载上限")}</div>
            <div className="set-desc">{LOAD_ZH[props.load][1]}</div>
          </div>
          <div className="segmented small" role="radiogroup" aria-label={t("磁盘负载上限")}>
            {(["max", "balanced", "quiet", "idle"] as LoadLevel[]).map((l) => (
              <button key={l} role="radio" aria-checked={props.load === l} className={props.load === l ? "on" : ""} onClick={() => props.onChange({ load: l })}>
                {LOAD_ZH[l][0]}
              </button>
            ))}
          </div>
        </div>
        {capable && (
          <div className="set-row">
            <div className="set-text">
              <div className="set-name">{t("扫描 NTFS 备用数据流")}</div>
              <div className="set-desc">{t("统计附加在文件上的隐藏数据流（MFT 快速扫描时计入）")}</div>
            </div>
            <button
              role="switch"
              aria-checked={props.ads}
              aria-label={t("扫描 NTFS 备用数据流")}
              className={`switch${props.ads ? " on" : ""}`}
              onClick={() => props.onChange({ ads: !props.ads })}
            >
              <span />
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ recents (UI-W1-38…46)

interface Record_ {
  key: string;
  snap: SnapshotInfo;
  name: string;
  volume: boolean;
}

function fmtWhen(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const hm = d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(now) - day(d)) / 86400000);
  if (diff === 0) return t("今天 {time}", { time: hm });
  if (diff === 1) return t("昨天 {time}", { time: hm });
  const pad = (n: number) => String(n).padStart(2, "0");
  if (d.getFullYear() === now.getFullYear()) return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function Recents({ tab }: { tab: Tab }) {
  const snapshots = useApp((s) => s.snapshots);
  const volumes = useApp((s) => s.volumes);
  const snapDir = useApp((s) => s.settings?.snapshots ?? "");
  const [all, setAll] = useState(false);
  const [menu, setMenu] = useState(false);
  const lang = useApp((s) => s.lang); // the names below are in the interface language
  const records = useMemo<Record_[]>(() => {
    const seen = new Set<string>();
    const out: Record_[] = [];
    for (const s of snapshots) {
      const key = s.roots.map((r) => r.toLowerCase()).join(";");
      if (!all && seen.has(key)) continue; // newest per target unless showing everything
      seen.add(key);
      const vol = s.roots.length === 1 ? volumes.find((v) => sameRoot(v.mount_point, s.roots[0]!)) : undefined;
      const name = vol ? volumeName(vol) : s.roots.length > 1 ? t("{n} 个位置", { n: s.roots.length }) : (s.roots[0]?.split(/[\\/]/).filter(Boolean).pop() ?? s.roots[0] ?? "");
      out.push({ key: s.path, snap: s, name, volume: !!vol });
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshots, volumes, all, lang]);
  const shown = all ? records : records.slice(0, 4);

  return (
    <aside className="start-side" aria-label={t("最近")}>
      <MachineCard />
      <div className="start-title">
        <h2>
          {t("最近")} <span className="mono muted">{records.length}</span>
        </h2>
        <div className="menu-anchor">
          <button className="icon-btn" aria-label={t("最近记录选项")} aria-haspopup="menu" aria-expanded={menu} onClick={() => setMenu(!menu)}>
            <Icon name="more" />
          </button>
          {menu && (
            <div className="menu" role="menu" onMouseLeave={() => setMenu(false)}>
              <button role="menuitem" className="menu-item" onClick={() => (setMenu(false), void refreshLists())}>
                <span className="menu-check" />
                {t("刷新列表")}
              </button>
              <button role="menuitem" className="menu-item" disabled={!snapDir} onClick={() => (setMenu(false), void window.sk.shell.openPath(snapDir))}>
                <span className="menu-check" />
                {t("打开快照文件夹")}
              </button>
            </div>
          )}
        </div>
      </div>
      <div className="recent-card">
        {records.length === 0 && <p className="muted recent-empty">{t("还没有扫描记录。每次扫描完成后会自动保存快照，之后可以秒开。")}</p>}
        <div className={all ? "recent-list all" : "recent-list"}>
          {shown.map((r) => (
            <div key={r.key} className="recent">
              <span className="recent-icon">
                <Icon name={r.volume ? "drive" : "folder"} size={16} />
              </span>
              <div className="recent-body">
                <div className="recent-line">
                  <span className="recent-name">{r.name}</span>
                  <span className="muted small mono">{fmtWhen(r.snap.created_at)}</span>
                </div>
                <div className="recent-line muted small">
                  <span className="recent-sub">{r.volume ? t("{size} · {method} · {files} 个文件", { size: fmtSize(r.snap.bytes_alloc), method: r.snap.method === "mft" ? t("MFT 快扫") : t("硬扫描"), files: fmtInt(r.snap.files) }) : `${r.snap.roots.join("; ")} · ${fmtSize(r.snap.bytes_alloc)}`}</span>
                  <span className="recent-badge">
                    <Icon name="snapshot" size={12} />
                    {t("有快照")}
                  </span>
                </div>
                <div className="recent-actions">
                  <button className="ghost tiny" onClick={() => void openSnapshot(r.snap.path, tab.id)}>
                    {t("打开")}
                  </button>
                  <button className="link small" title={t("与这个位置现在的状态对比（需要时先扫描）")} onClick={() => void compareWithNow(r.snap, tab.id)}>
                    <Icon name="diff" size={13} /> {t("与现在对比")}
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
        {records.length > 4 || all ? (
          <button className="recent-all" onClick={() => setAll(!all)}>
            {all ? t("只看最近 4 条") : t("查看全部扫描记录")} <Icon name="chevronRight" size={12} />
          </button>
        ) : null}
      </div>
      <div className="drop-zone">
        <span className="drop-icon">
          <Icon name="upload" size={18} />
        </span>
        <div className="strong">{t("把快照文件拖到这里")}</div>
        <div className="muted small">{t("打开后可直接浏览，或与现在的磁盘对比")}</div>
        <button
          className="ghost small"
          onClick={async () => {
            const path = await window.sk.dialog.openSnapshot();
            if (path) await openSnapshot(path, tab.id);
          }}
        >
          <Icon name="snapshot" size={14} />
          {t("打开快照文件…")}
        </button>
      </div>
    </aside>
  );
}

export const fmtShareOf = fmtShare;
export const keysOf = fmtKeys;
