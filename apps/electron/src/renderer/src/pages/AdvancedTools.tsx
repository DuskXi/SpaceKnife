// 「设置 › 高级」 tools (UI-W7-36) and 「标签」's clear-all (UI-W7-34): settings export / import (also
// SpaceSniffer's SpaceSnifferConfig.xml, previewed before anything changes), restore everything,
// the scan helper, the cache, remembered tags.

import { useEffect, useState } from "react";

import type { MaintenanceInfo, SettingsImportResult } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call, resetSettings } from "@/state/actions";
import { toast } from "@/state/store";
import { fmtInt, fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const fail = (err: unknown) => toast(err instanceof RpcError ? err.message : String(err));
const pad = (n: number) => String(n).padStart(2, "0");

function useMaintenance(): [MaintenanceInfo | null, (p: Promise<MaintenanceInfo>) => void] {
  const [info, setInfo] = useState<MaintenanceInfo | null>(null);
  useEffect(() => {
    void call("system.maintenance", {})
      .then(setInfo)
      .catch(() => undefined);
  }, []);
  return [info, (p) => void p.then(setInfo).catch(fail)];
}

function Row(props: { name: string; desc?: string; children: React.ReactNode }) {
  return (
    <div className="set-row">
      <div className="set-text">
        <div className="set-name">{props.name}</div>
        {props.desc && <div className="set-desc">{props.desc}</div>}
      </div>
      <div className="set-control">{props.children}</div>
    </div>
  );
}

/** A danger button that asks once more in place. */
function Confirm({ label, ask, onYes, disabled }: { label: string; ask: string; onYes: () => void; disabled?: boolean }) {
  const [asking, setAsking] = useState(false);
  if (!asking)
    return (
      <button className="ghost small" disabled={disabled} onClick={() => setAsking(true)}>
        {label}
      </button>
    );
  return (
    <span className="class-confirm">
      <span className="small">{ask}</span>
      <button className="danger-btn small" onClick={() => (setAsking(false), onYes())}>
        {t("确定")}
      </button>
      <button className="ghost small" onClick={() => setAsking(false)}>
        {t("取消")}
      </button>
    </span>
  );
}

export function AdvancedTools() {
  const [info, refresh] = useMaintenance();
  const [preview, setPreview] = useState<{ path: string; plan: SettingsImportResult } | null>(null);

  const exportSettings = async () => {
    const d = new Date();
    const path = await window.sk.dialog.saveFile({
      title: t("导出设置"),
      defaultName: t("SpaceKnife-设置-{date}.json", { date: `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}` }),
      filters: [{ name: t("SpaceKnife 设置"), extensions: ["json"] }],
    });
    if (!path) return;
    try {
      await call("settings.export", { path });
      toast(t("设置已导出"));
    } catch (err) {
      fail(err);
    }
  };
  const importFrom = async (spacesniffer: boolean) => {
    const path = await window.sk.dialog.openFile({
      title: spacesniffer ? t("选择 SpaceSnifferConfig.xml") : t("导入设置"),
      filters: spacesniffer ? [{ name: t("SpaceSniffer 配置"), extensions: ["xml"] }] : [{ name: t("SpaceKnife 设置"), extensions: ["json"] }],
    });
    if (!path) return;
    try {
      setPreview({ path, plan: await call("settings.import", { path, apply: false }) });
    } catch (err) {
      fail(err);
    }
  };
  const apply = async () => {
    if (!preview) return;
    try {
      const r = await call("settings.import", { path: preview.path, apply: true });
      setPreview(null);
      const n = fmtInt(r.changes.length);
      toast(r.classes ? t("已导入 {n} 项设置和文件类别", { n }) : t("已导入 {n} 项设置", { n }));
    } catch (err) {
      fail(err);
    }
  };

  return (
    <>
      <section className="set-group" aria-label={t("设置文件")}>
        <div className="set-group-head">
          <h2>{t("导入与导出")}</h2>
        </div>
        <div className="set-card">
          <Row name={t("导出设置")} desc={t("所有改过的设置和自定义的文件类别，存成一个文件")}>
            <button className="ghost small" onClick={() => void exportSettings()}>
              <Icon name="upload" size={14} /> {t("导出…")}
            </button>
          </Row>
          <Row name={t("导入设置")} desc={t("先列出会改变的项，确认后才生效")}>
            <button className="ghost small" onClick={() => void importFrom(false)}>
              {t("导入…")}
            </button>
          </Row>
          <Row name={t("从 SpaceSniffer 导入")} desc={t("读取 SpaceSnifferConfig.xml：几何、动画、行为、外观和文件类别")}>
            <button className="ghost small" onClick={() => void importFrom(true)}>
              {t("选择配置文件…")}
            </button>
          </Row>
          <Row name={t("全部恢复默认")} desc={t("所有设置和文件类别回到初始值")}>
            <Confirm
              label={t("全部恢复默认…")}
              ask={t("所有设置都会恢复默认，确定？")}
              onYes={() => {
                void resetSettings(null);
                void call("classes.reset", {}).catch(fail);
                toast(t("已全部恢复默认"));
              }}
            />
          </Row>
        </div>
      </section>
      <section className="set-group" aria-label={t("扫描助手与缓存")}>
        <div className="set-group-head">
          <h2>{t("扫描助手与缓存")}</h2>
        </div>
        <div className="set-card">
          <Row
            name={t("扫描助手")}
            desc={
              info
                ? info.helper_running
                  ? info.helper_elevated
                    ? t("运行中（管理员权限）：MFT 快速扫描与 USN 日志由它读取")
                    : t("运行中：MFT 快速扫描与 USN 日志由它读取")
                  : t("未运行")
                : "…"
            }
          >
            <button className="ghost small" disabled={!info?.helper_running} onClick={() => refresh(call("system.helper_stop", {}))}>
              {t("停止扫描助手")}
            </button>
          </Row>
          <Row name={t("快照")} desc={info ? t("{n} 个快照，共 {size} · {dir}", { n: fmtInt(info.snapshot_count), size: fmtSize(info.snapshots_bytes), dir: info.snapshots_dir }) : "…"}>
            <button className="ghost small" disabled={!info} onClick={() => info && void window.sk.shell.openPath(info.snapshots_dir)}>
              <Icon name="reveal" size={14} /> {t("打开")}
            </button>
          </Row>
          <Row name={t("缓存")} desc={info ? `${fmtSize(info.cache_bytes)} · ${info.cache_dir}` : "…"}>
            <Confirm label={t("清除缓存…")} ask={t("正在使用的缓存会保留，确定清除？")} disabled={!info?.cache_bytes} onYes={() => refresh(call("system.cache_clear", {}))} />
          </Row>
        </div>
      </section>
      {preview && <ImportPreview preview={preview} onApply={() => void apply()} onCancel={() => setPreview(null)} />}
    </>
  );
}

function show(v: unknown): string {
  if (typeof v === "boolean") return v ? t("开") : t("关");
  return String(v);
}

function ImportPreview({ preview, onApply, onCancel }: { preview: { path: string; plan: SettingsImportResult }; onApply: () => void; onCancel: () => void }) {
  const { plan } = preview;
  const nothing = !plan.changes.length && !plan.classes;
  return (
    <div className="modal-back" role="presentation" onMouseDown={onCancel}>
      <div className="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="import-title" onMouseDown={(e) => e.stopPropagation()}>
        <h3 id="import-title">{plan.source === "spacesniffer" ? t("从 SpaceSniffer 导入") : t("导入设置")}</h3>
        <p className="muted small mono">{preview.path}</p>
        <div className="import-body">
          {nothing && <p>{t("没有需要改变的设置。")}</p>}
          {plan.changes.length > 0 && (
            <>
              <div className="muted small">{t("将改变 {n} 项设置", { n: fmtInt(plan.changes.length) })}</div>
              <table className="import-table">
                <tbody>
                  {plan.changes.map((c) => (
                    <tr key={c.key}>
                      <td>{c.name}</td>
                      <td className="mono muted">{show(c.before)}</td>
                      <td className="muted">→</td>
                      <td className="mono">{show(c.after)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          )}
          {plan.classes && (
            <>
              <div className="muted small">{t("文件类别将变为")}</div>
              <ul className="import-classes">
                {plan.classes.map((c) => (
                  <li key={c.id}>
                    <strong>{c.name}</strong> <span className="muted small mono">{c.extensions.join(";") || t("未归类的扩展名")}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
          {plan.skipped.length > 0 && (
            <>
              <div className="muted small">{t("不导入")}</div>
              <ul className="import-skipped muted small">
                {plan.skipped.map((s) => (
                  <li key={s}>{s}</li>
                ))}
              </ul>
            </>
          )}
        </div>
        <div className="modal-buttons">
          <button className="ghost" onClick={onCancel}>
            {t("取消")}
          </button>
          <button className="primary" disabled={nothing} onClick={onApply}>
            {t("导入")}
          </button>
        </div>
      </div>
    </div>
  );
}

/** 「标签」: forget every remembered tag (UI-W7-34). */
export function ClearTags() {
  const [info, refresh] = useMaintenance();
  return (
    <section className="set-group" aria-label={t("已保存的标签")}>
      <div className="set-group-head">
        <h2>{t("已保存的标签")}</h2>
      </div>
      <div className="set-card">
        <Row name={t("清除所有已保存的标签")} desc={info ? t("{n} 个扫描位置有已保存的标签", { n: fmtInt(info.saved_tags) }) : "…"}>
          <Confirm label={t("清除…")} ask={t("所有扫描位置的标签都会被清除，确定？")} disabled={!info?.saved_tags} onYes={() => refresh(call("tags.clear_saved", {}))} />
        </Row>
      </div>
    </section>
  );
}
