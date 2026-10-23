// 「导出报告…」 (W4 palette, docs/export-templates.md): a SpaceSniffer template or CSV / JSON / HTML,
// the zoom root or the whole scan, filtered or not, written by the backend to a file the user picks.

import { useEffect, useState } from "react";

import type { ExportOption, ExportTarget } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { focusedViewId, set, toast, useApp } from "@/state/store";
import { fmtSize, parseSizeText } from "@/lib/format";
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
  // the selected node (a real one, not an aggregate block): 「选中项」 exports just it
  const selected = useApp((s) => {
    const sel = viewId ? s.views[viewId]?.state.selected : null;
    return sel != null && sel > 0 && sel >>> 30 === 0 ? sel : null;
  });
  const epoch = useApp((s) => s.exportsEpoch); // plugins added or removed formats: read them again
  const [targets, setTargets] = useState<ExportTarget[] | null>(null);
  const [pick, setPick] = useState("Grouped by folder");
  const [scope, setScope] = useState<"zoom" | "all" | "selection">("zoom");
  const [useFilter, setUseFilter] = useState(true);
  const [busy, setBusy] = useState(false);
  const [options, setOptions] = useState<Record<string, OptionValue>>({});

  useEffect(() => setTargets(null), [epoch]);
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
  const values: Record<string, OptionValue> = Object.fromEntries((target?.options ?? []).map((o) => [o.name, options[`${target!.name}:${o.name}`] ?? o.default]));
  const setOption = (o: ExportOption, v: OptionValue) => target && setOptions((x) => ({ ...x, [`${target.name}:${o.name}`]: v }));
  const where = scope === "selection" && selected == null ? (zoomed ? "zoom" : "all") : scope === "zoom" && !zoomed ? "all" : scope;
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
      const r = await call("export.run", {
        view_id: viewId,
        target: target.name,
        source: null,
        path,
        scope: where,
        use_filter: filtering && useFilter,
        node: null,
        selection: where === "selection" && selected != null ? [selected] : [],
        options: values,
      });
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
        {target && target.options.length > 0 && (
          <div className="export-options" aria-label={t("这种格式的选项")}>
            {target.options.map((o) => (
              <OptionField key={o.name} o={o} value={values[o.name]!} onChange={(v) => setOption(o, v)} />
            ))}
          </div>
        )}
        <div className="export-scope">
          {(zoomed || selected != null) && (
            <div className="segmented small" role="radiogroup" aria-label={t("范围")}>
              {zoomed && (
                <button role="radio" aria-checked={where === "zoom"} className={where === "zoom" ? "on" : ""} onClick={() => setScope("zoom")}>
                  {t("当前位置")}
                </button>
              )}
              {selected != null && (
                <button role="radio" aria-checked={where === "selection"} className={where === "selection" ? "on" : ""} onClick={() => setScope("selection")}>
                  {t("选中项")}
                </button>
              )}
              <button role="radio" aria-checked={where === "all"} className={where === "all" ? "on" : ""} onClick={() => setScope("all")}>
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

type OptionValue = boolean | number | string;

/** One option of a plugin's format (declared like a setting): a switch, a choice, a number, a size or text. */
function OptionField({ o, value, onChange }: { o: ExportOption; value: OptionValue; onChange: (v: OptionValue) => void }) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(o.unit === "B" && typeof value === "number" ? fmtSize(value) : String(value)), [value, o.unit]);
  const label = (
    <span className="flex">
      <span>{o.title}</span>
      {o.description && <span className="muted small export-desc">{o.description}</span>}
    </span>
  );
  if (o.control === "switch")
    return (
      <label className="export-check">
        <input type="checkbox" checked={!!value} onChange={(e) => onChange(e.target.checked)} />
        {label}
      </label>
    );
  if (o.options.length)
    return (
      <label className="export-field">
        {label}
        <select value={String(value)} onChange={(e) => onChange(o.options.find((x) => String(x.value) === e.target.value)?.value ?? e.target.value)}>
          {o.options.map((x) => (
            <option key={String(x.value)} value={String(x.value)}>
              {x.label}
            </option>
          ))}
        </select>
      </label>
    );
  const numeric = o.control === "number" || o.control === "slider";
  const commit = () => {
    if (!numeric) return onChange(text);
    const n = o.unit === "B" ? parseSizeText(text) : Number(text.trim());
    if (n == null || !Number.isFinite(n) || (o.min != null && n < o.min) || (o.max != null && n > o.max)) {
      setText(o.unit === "B" && typeof value === "number" ? fmtSize(value) : String(value));
      return;
    }
    onChange(n);
  };
  return (
    <label className="export-field">
      {label}
      <input
        value={text}
        placeholder={o.placeholder}
        inputMode={numeric && o.unit !== "B" ? "numeric" : undefined}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") commit();
        }}
      />
      {o.unit && o.unit !== "B" && <span className="muted small">{o.unit}</span>}
    </label>
  );
}
