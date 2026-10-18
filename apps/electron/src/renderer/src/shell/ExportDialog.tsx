// 「导出报告…」 (W4 palette, docs/export-templates.md): a SpaceSniffer template or CSV / JSON / HTML,
// the zoom root or the whole scan, filtered or not, written by the backend to a file the user picks.

import { useEffect, useState } from "react";

import type { ExportTarget } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { focusedViewId, set, toast, useApp } from "@/state/store";
import { fmtSize } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";

const pad = (n: number) => String(n).padStart(2, "0");

function defaultName(target: ExportTarget, title: string): string {
  if (target.file_name) return target.file_name;
  const d = new Date();
  const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}`;
  const base = title.replace(/[\\/:*?"<>|]+/g, "_").trim() || "SpaceKnife";
  return `${base}-${stamp}.${target.extension}`;
}

export function ExportDialog() {
  const open = useApp((s) => s.exportOpen);
  const viewId = useApp((s) => focusedViewId(s));
  const filtering = useApp((s) => (viewId ? !!s.views[viewId]?.filter?.active : false));
  const zoomed = useApp((s) => (viewId ? (s.views[viewId]?.info?.breadcrumb.length ?? 1) > 1 : false));
  const title = useApp((s) => s.tabs.find((tab) => tab.id === s.active)?.title ?? "");
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
  const target = targets?.find((x) => x.name === pick) ?? targets?.[0];
  const run = async () => {
    if (!target || !viewId) return;
    const path = await window.sk.dialog.saveFile({
      title: t("导出到"),
      defaultName: defaultName(target, title),
      filters: [{ name: target.title, extensions: [target.extension] }],
    });
    if (!path) return;
    setBusy(true);
    try {
      const r = await call("export.run", { view_id: viewId, target: target.name, source: null, path, scope: zoomed ? scope : "all", use_filter: filtering && useFilter });
      close();
      toast(t("已导出 {size}：{name}", { size: fmtSize(r.bytes), name: r.path.split(/[\\/]/).pop() ?? r.path }));
      window.sk.shell.showItemInFolder(r.path);
    } catch (err) {
      toast(err instanceof RpcError ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  const group = (kind: ExportTarget["kind"]) => (targets ?? []).filter((x) => x.kind === kind);

  return (
    <div className="modal-back" role="presentation" onMouseDown={close}>
      <div className="modal export-modal" role="dialog" aria-modal="true" aria-labelledby="export-title" onMouseDown={(e) => e.stopPropagation()}>
        <h3 id="export-title">{t("导出报告")}</h3>
        {!viewId && <p className="muted">{t("先打开一次扫描")}</p>}
        {targets === null && <p className="muted">{t("正在读取模板…")}</p>}
        {targets && (
          <div className="export-groups" role="radiogroup" aria-label={t("模板与格式")}>
            {(["template", "format"] as const).map((kind) => (
              <div key={kind} className="export-group">
                <div className="muted small">{kind === "template" ? t("模板（与 SpaceSniffer 相同的模板语言）") : t("数据格式")}</div>
                {group(kind).map((x) => (
                  <label key={x.name} className={`export-option${x.name === target?.name ? " on" : ""}`}>
                    <input type="radio" name="export-target" checked={x.name === target?.name} onChange={() => setPick(x.name)} />
                    <span className="flex">
                      <span className="export-name">
                        {x.title}
                        {kind === "template" && <span className="muted small mono"> {x.name}</span>}
                      </span>
                      {x.description && <span className="muted small export-desc">{x.description}</span>}
                    </span>
                    <span className="muted small mono">.{x.extension}</span>
                  </label>
                ))}
              </div>
            ))}
          </div>
        )}
        <div className="export-scope">
          {zoomed && (
            <div className="segmented small" role="radiogroup" aria-label={t("范围")}>
              <button role="radio" aria-checked={scope === "zoom"} className={scope === "zoom" ? "on" : ""} onClick={() => setScope("zoom")}>
                {t("当前位置")}
              </button>
              <button role="radio" aria-checked={scope === "all"} className={scope === "all" ? "on" : ""} onClick={() => setScope("all")}>
                {t("整个扫描")}
              </button>
            </div>
          )}
          {filtering && (
            <label className="export-check">
              <input type="checkbox" checked={useFilter} onChange={(e) => setUseFilter(e.target.checked)} />
              {t("只导出过滤匹配的项")}
            </label>
          )}
        </div>
        <div className="modal-buttons">
          <button className="ghost" onClick={close}>
            {t("取消")}
          </button>
          <button className="primary" disabled={!target || !viewId || busy} onClick={() => void run()}>
            <Icon name="upload" size={14} /> {t("导出…")}
          </button>
        </div>
      </div>
    </div>
  );
}
