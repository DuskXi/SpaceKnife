// 「导出报告…」 (W4 palette, docs/export-templates.md): a SpaceSniffer template or CSV / JSON / HTML,
// the zoom root or the whole scan, filtered or not, written by the backend to a file the user picks.

import { useEffect, useState } from "react";

import type { ExportTarget } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { focusedViewId, set, toast, useApp } from "@/state/store";
import { fmtSize } from "@/lib/format";
import { Icon } from "@/ui/Icon";

const pad = (n: number) => String(n).padStart(2, "0");

function defaultName(t: ExportTarget, title: string): string {
  if (t.file_name) return t.file_name;
  const d = new Date();
  const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}`;
  const base = title.replace(/[\\/:*?"<>|]+/g, "_").trim() || "SpaceKnife";
  return `${base}-${stamp}.${t.extension}`;
}

export function ExportDialog() {
  const open = useApp((s) => s.exportOpen);
  const viewId = useApp((s) => focusedViewId(s));
  const filtering = useApp((s) => (viewId ? !!s.views[viewId]?.filter?.active : false));
  const zoomed = useApp((s) => (viewId ? (s.views[viewId]?.info?.breadcrumb.length ?? 1) > 1 : false));
  const title = useApp((s) => s.tabs.find((t) => t.id === s.active)?.title ?? "");
  const [targets, setTargets] = useState<ExportTarget[] | null>(null);
  const [pick, setPick] = useState("Grouped by folder");
  const [scope, setScope] = useState<"zoom" | "all">("zoom");
  const [useFilter, setUseFilter] = useState(true);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open || targets) return;
    void call("export.targets", {})
      .then((r) => setTargets(r.targets))
      .catch(() => setTargets([]));
  }, [open, targets]);
  useEffect(() => {
    if (!open) return;
    const key = (e: KeyboardEvent) => e.key === "Escape" && set({ exportOpen: false });
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [open]);
  if (!open) return null;

  const close = () => set({ exportOpen: false });
  const target = targets?.find((t) => t.name === pick) ?? targets?.[0];
  const run = async () => {
    if (!target || !viewId) return;
    const path = await window.sk.dialog.saveFile({
      title: "导出到",
      defaultName: defaultName(target, title),
      filters: [{ name: target.title, extensions: [target.extension] }],
    });
    if (!path) return;
    setBusy(true);
    try {
      const r = await call("export.run", { view_id: viewId, target: target.name, source: null, path, scope: zoomed ? scope : "all", use_filter: filtering && useFilter });
      close();
      toast(`已导出 ${fmtSize(r.bytes)}：${r.path.split(/[\\/]/).pop()}`);
      window.sk.shell.showItemInFolder(r.path);
    } catch (err) {
      toast(err instanceof RpcError ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  const group = (kind: ExportTarget["kind"]) => (targets ?? []).filter((t) => t.kind === kind);

  return (
    <div className="modal-back" role="presentation" onMouseDown={close}>
      <div className="modal export-modal" role="dialog" aria-modal="true" aria-labelledby="export-title" onMouseDown={(e) => e.stopPropagation()}>
        <h3 id="export-title">导出报告</h3>
        {!viewId && <p className="muted">先打开一次扫描</p>}
        {targets === null && <p className="muted">正在读取模板…</p>}
        {targets && (
          <div className="export-groups" role="radiogroup" aria-label="模板与格式">
            {(["template", "format"] as const).map((kind) => (
              <div key={kind} className="export-group">
                <div className="muted small">{kind === "template" ? "模板（与 SpaceSniffer 相同的模板语言）" : "数据格式"}</div>
                {group(kind).map((t) => (
                  <label key={t.name} className={`export-option${t.name === target?.name ? " on" : ""}`}>
                    <input type="radio" name="export-target" checked={t.name === target?.name} onChange={() => setPick(t.name)} />
                    <span className="flex">
                      <span className="export-name">
                        {t.title}
                        {kind === "template" && <span className="muted small mono"> {t.name}</span>}
                      </span>
                      {t.description && <span className="muted small export-desc">{t.description}</span>}
                    </span>
                    <span className="muted small mono">.{t.extension}</span>
                  </label>
                ))}
              </div>
            ))}
          </div>
        )}
        <div className="export-scope">
          {zoomed && (
            <div className="segmented small" role="radiogroup" aria-label="范围">
              <button role="radio" aria-checked={scope === "zoom"} className={scope === "zoom" ? "on" : ""} onClick={() => setScope("zoom")}>
                当前位置
              </button>
              <button role="radio" aria-checked={scope === "all"} className={scope === "all" ? "on" : ""} onClick={() => setScope("all")}>
                整个扫描
              </button>
            </div>
          )}
          {filtering && (
            <label className="export-check">
              <input type="checkbox" checked={useFilter} onChange={(e) => setUseFilter(e.target.checked)} />
              只导出过滤匹配的项
            </label>
          )}
        </div>
        <div className="modal-buttons">
          <button className="ghost" onClick={close}>
            取消
          </button>
          <button className="primary" disabled={!target || !viewId || busy} onClick={() => void run()}>
            <Icon name="upload" size={14} /> 导出…
          </button>
        </div>
      </div>
    </div>
  );
}
