// 「设置 › 高级」 tools (UI-W7-36) and 「标签」's clear-all (UI-W7-34): settings export / import (also
// SpaceSniffer's SpaceSnifferConfig.xml, previewed before anything changes), restore everything,
// the scan helper, the cache, remembered tags.

import { useEffect, useState } from "react";

import type { MaintenanceInfo, SettingsImportResult } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call, resetSettings } from "@/state/actions";
import { toast } from "@/state/store";
import { fmtInt, fmtSize } from "@/lib/format";
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
        确定
      </button>
      <button className="ghost small" onClick={() => setAsking(false)}>
        取消
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
      title: "导出设置",
      defaultName: `SpaceKnife-设置-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}.json`,
      filters: [{ name: "SpaceKnife 设置", extensions: ["json"] }],
    });
    if (!path) return;
    try {
      await call("settings.export", { path });
      toast("设置已导出");
    } catch (err) {
      fail(err);
    }
  };
  const importFrom = async (spacesniffer: boolean) => {
    const path = await window.sk.dialog.openFile({
      title: spacesniffer ? "选择 SpaceSnifferConfig.xml" : "导入设置",
      filters: spacesniffer ? [{ name: "SpaceSniffer 配置", extensions: ["xml"] }] : [{ name: "SpaceKnife 设置", extensions: ["json"] }],
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
      toast(`已导入 ${fmtInt(r.changes.length)} 项设置${r.classes ? "和文件类别" : ""}`);
    } catch (err) {
      fail(err);
    }
  };

  return (
    <>
      <section className="set-group" aria-label="设置文件">
        <div className="set-group-head">
          <h2>导入与导出</h2>
        </div>
        <div className="set-card">
          <Row name="导出设置" desc="所有改过的设置和自定义的文件类别，存成一个文件">
            <button className="ghost small" onClick={() => void exportSettings()}>
              <Icon name="upload" size={14} /> 导出…
            </button>
          </Row>
          <Row name="导入设置" desc="先列出会改变的项，确认后才生效">
            <button className="ghost small" onClick={() => void importFrom(false)}>
              导入…
            </button>
          </Row>
          <Row name="从 SpaceSniffer 导入" desc="读取 SpaceSnifferConfig.xml：几何、动画、行为、外观和文件类别">
            <button className="ghost small" onClick={() => void importFrom(true)}>
              选择配置文件…
            </button>
          </Row>
          <Row name="全部恢复默认" desc="所有设置和文件类别回到初始值">
            <Confirm
              label="全部恢复默认…"
              ask="所有设置都会恢复默认，确定？"
              onYes={() => {
                void resetSettings(null);
                void call("classes.reset", {}).catch(fail);
                toast("已全部恢复默认");
              }}
            />
          </Row>
        </div>
      </section>
      <section className="set-group" aria-label="扫描助手与缓存">
        <div className="set-group-head">
          <h2>扫描助手与缓存</h2>
        </div>
        <div className="set-card">
          <Row name="扫描助手" desc={info ? (info.helper_running ? `运行中${info.helper_elevated ? "（管理员权限）" : ""}：MFT 快速扫描与 USN 日志由它读取` : "未运行") : "…"}>
            <button className="ghost small" disabled={!info?.helper_running} onClick={() => refresh(call("system.helper_stop", {}))}>
              停止扫描助手
            </button>
          </Row>
          <Row name="快照" desc={info ? `${fmtInt(info.snapshot_count)} 个快照，共 ${fmtSize(info.snapshots_bytes)} · ${info.snapshots_dir}` : "…"}>
            <button className="ghost small" disabled={!info} onClick={() => info && void window.sk.shell.openPath(info.snapshots_dir)}>
              <Icon name="reveal" size={14} /> 打开
            </button>
          </Row>
          <Row name="缓存" desc={info ? `${fmtSize(info.cache_bytes)} · ${info.cache_dir}` : "…"}>
            <Confirm label="清除缓存…" ask="正在使用的缓存会保留，确定清除？" disabled={!info?.cache_bytes} onYes={() => refresh(call("system.cache_clear", {}))} />
          </Row>
        </div>
      </section>
      {preview && <ImportPreview preview={preview} onApply={() => void apply()} onCancel={() => setPreview(null)} />}
    </>
  );
}

function show(v: unknown): string {
  if (typeof v === "boolean") return v ? "开" : "关";
  return String(v);
}

function ImportPreview({ preview, onApply, onCancel }: { preview: { path: string; plan: SettingsImportResult }; onApply: () => void; onCancel: () => void }) {
  const { plan } = preview;
  const nothing = !plan.changes.length && !plan.classes;
  return (
    <div className="modal-back" role="presentation" onMouseDown={onCancel}>
      <div className="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="import-title" onMouseDown={(e) => e.stopPropagation()}>
        <h3 id="import-title">{plan.source === "spacesniffer" ? "从 SpaceSniffer 导入" : "导入设置"}</h3>
        <p className="muted small mono">{preview.path}</p>
        <div className="import-body">
          {nothing && <p>没有需要改变的设置。</p>}
          {plan.changes.length > 0 && (
            <>
              <div className="muted small">将改变 {fmtInt(plan.changes.length)} 项设置</div>
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
              <div className="muted small">文件类别将变为</div>
              <ul className="import-classes">
                {plan.classes.map((c) => (
                  <li key={c.id}>
                    <strong>{c.name}</strong> <span className="muted small mono">{c.extensions.join(";") || "未归类的扩展名"}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
          {plan.skipped.length > 0 && (
            <>
              <div className="muted small">不导入</div>
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
            取消
          </button>
          <button className="primary" disabled={nothing} onClick={onApply}>
            导入
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
    <section className="set-group" aria-label="已保存的标签">
      <div className="set-group-head">
        <h2>已保存的标签</h2>
      </div>
      <div className="set-card">
        <Row name="清除所有已保存的标签" desc={info ? `${fmtInt(info.saved_tags)} 个扫描位置有已保存的标签` : "…"}>
          <Confirm label="清除…" ask="所有扫描位置的标签都会被清除，确定？" disabled={!info?.saved_tags} onYes={() => refresh(call("tags.clear_saved", {}))} />
        </Row>
      </div>
    </section>
  );
}
