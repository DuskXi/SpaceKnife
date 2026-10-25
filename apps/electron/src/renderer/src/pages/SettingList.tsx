// The settings page's controls for plugins' structured settings (docs/plugin-dev.md 「设置」):
// text of several lines (control "textarea") and a list of entries (control "list"), each entry the
// same few fields — add, remove, reorder, edit in place. The backend validates every change (each
// field as it is declared, the most entries, the longest text) and broadcasts the result.

import { useEffect, useRef, useState } from "react";

import type { SettingField, SettingMeta } from "@/protocol/generated";
import { fmtInt } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon } from "@/ui/Icon";
import { MultiChoice } from "@/ui/MultiChoice";

export type Scalar = boolean | number | string;
export type Entry = Record<string, Scalar>;

const TEXT_MAX = 64; // a text value's longest when the setting does not say (max_length)
const TEXTAREA_MAX = 4096;

/** Text of several lines; saved when it loses focus, or with Ctrl+Enter. */
export function TextAreaBox({ meta, value, disabled, onSave }: { meta: Pick<SettingMeta, "name" | "placeholder" | "max_length">; value: string; disabled: boolean; onSave: (v: string) => void }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const limit = meta.max_length ?? TEXTAREA_MAX;
  const commit = () => text !== value && onSave(text);
  return (
    <div className="set-textarea-wrap">
      <textarea
        className="set-textarea"
        aria-label={meta.name}
        placeholder={meta.placeholder}
        maxLength={limit}
        rows={Math.min(12, Math.max(3, text.split("\n").length + 1))}
        value={text}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) commit();
        }}
      />
      <span className={`set-count mono${text.length >= limit * 0.9 ? " near" : ""}`}>
        {fmtInt(text.length)} / {fmtInt(limit)}
      </span>
    </div>
  );
}

interface Row {
  id: number;
  entry: Entry;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** A new entry: every field at its default. */
export function blankEntry(fields: readonly SettingField[]): Entry {
  return Object.fromEntries(fields.map((f) => [f.name, f.default]));
}

/** `rows` with entry `i` moved by `by` (−1 up, +1 down); the same rows when it can't move. */
export function moved<T>(rows: readonly T[], i: number, by: -1 | 1): T[] {
  const j = i + by;
  if (j < 0 || j >= rows.length) return [...rows];
  const out = [...rows];
  [out[i], out[j]] = [out[j]!, out[i]!];
  return out;
}

/** A list setting: its entries, each a row of its fields, with add / remove / move up and down. */
export function ListEditor({ meta, value, disabled, onSave }: { meta: SettingMeta; value: Entry[]; disabled: boolean; onSave: (v: Entry[]) => void }) {
  const ids = useRef(0);
  const withIds = (list: readonly Entry[]): Row[] => list.map((entry) => ({ id: ++ids.current, entry }));
  const [rows, setRows] = useState<Row[]>(() => withIds(value));
  // what the backend now holds; our own change coming back keeps the rows (and the focus) as they are
  useEffect(() => setRows((r) => (same(r.map((x) => x.entry), value) ? r : withIds(value))), [value]); // eslint-disable-line react-hooks/exhaustive-deps
  const limit = meta.max_items ?? 50;
  const save = (next: Row[]) => {
    setRows(next);
    onSave(next.map((x) => x.entry));
  };
  const update = (i: number, name: string, v: Scalar) => save(rows.map((r, k) => (k === i ? { ...r, entry: { ...r.entry, [name]: v } } : r)));
  return (
    <div className="set-list" role="list" aria-label={meta.name}>
      {rows.length === 0 && <div className="set-list-empty muted small">{t("还没有条目")}</div>}
      {rows.map((r, i) => (
        <div key={r.id} className="set-list-item" role="listitem" aria-label={t("第 {n} 项", { n: i + 1 })}>
          <span className="set-list-index mono">{i + 1}</span>
          <div className="set-list-fields">
            {meta.fields.map((f) => (
              <FieldControl key={f.name} field={f} value={r.entry[f.name] ?? f.default} disabled={disabled} onChange={(v) => update(i, f.name, v)} />
            ))}
          </div>
          <div className="set-list-actions">
            <button className="icon-btn tiny" title={t("上移")} aria-label={t("上移")} disabled={disabled || i === 0} onClick={() => save(moved(rows, i, -1))}>
              <Icon name="up" size={14} />
            </button>
            <button className="icon-btn tiny flip" title={t("下移")} aria-label={t("下移")} disabled={disabled || i === rows.length - 1} onClick={() => save(moved(rows, i, 1))}>
              <Icon name="up" size={14} />
            </button>
            <button className="icon-btn tiny danger" title={t("删除这一项")} aria-label={t("删除这一项")} disabled={disabled} onClick={() => save(rows.filter((_, k) => k !== i))}>
              <Icon name="close" size={14} />
            </button>
          </div>
        </div>
      ))}
      <div className="set-list-foot">
        <button className="ghost small" disabled={disabled || rows.length >= limit} onClick={() => save([...rows, { id: ++ids.current, entry: blankEntry(meta.fields) }])}>
          <Icon name="plus" size={14} />
          {t("添加一项")}
        </button>
        <span className="muted small mono">
          {fmtInt(rows.length)} / {fmtInt(limit)}
        </span>
      </div>
    </div>
  );
}

/** Why `text` is no number this field takes, or "" (a whole number when its default is one). */
export function numberProblem(field: Pick<SettingField, "min" | "max" | "default">, text: string): string {
  const n = Number(text.trim());
  const lo = field.min ?? -Infinity;
  const hi = field.max ?? Infinity;
  const whole = Number.isInteger(field.default);
  if (!text.trim() || !Number.isFinite(n) || (whole && !Number.isInteger(n)) || n < lo || n > hi) {
    return whole ? t("请输入 {lo}–{hi} 的整数", { lo, hi }) : t("请输入 {lo}–{hi} 的数", { lo, hi });
  }
  return "";
}

/** One field of an entry: its title above its control. */
function FieldControl({ field, value, disabled, onChange }: { field: SettingField; value: Scalar; disabled: boolean; onChange: (v: Scalar) => void }) {
  const wide = field.control === "textarea" || field.control === "multi";
  return (
    // a div, not a label: a multi choice holds labels of its own (each control has its aria-label)
    <div className={`set-list-field${wide ? " wide" : ""}`} title={field.description || undefined}>
      <span>{field.title}</span>
      <FieldInput field={field} value={value} disabled={disabled} onChange={onChange} />
    </div>
  );
}

function FieldInput({ field, value, disabled, onChange }: { field: SettingField; value: Scalar; disabled: boolean; onChange: (v: Scalar) => void }) {
  const [text, setText] = useState(String(value));
  const [error, setError] = useState("");
  useEffect(() => setText(String(value)), [value]);
  switch (field.control) {
    case "switch":
      return (
        <button role="switch" aria-checked={Boolean(value)} aria-label={field.title} className={`switch${value ? " on" : ""}`} disabled={disabled} onClick={() => onChange(!value)}>
          <span />
        </button>
      );
    case "segmented":
    case "select":
      return (
        <select className="set-select" aria-label={field.title} value={String(value)} disabled={disabled} onChange={(e) => onChange(field.options.find((o) => String(o.value) === e.target.value)?.value ?? e.target.value)}>
          {field.options.map((o) => (
            <option key={String(o.value)} value={String(o.value)}>
              {o.label}
            </option>
          ))}
        </select>
      );
    case "multi":
      return <MultiChoice name={field.title} options={field.options} value={String(value ?? "")} disabled={disabled} onChange={onChange} />;
    case "textarea":
      return <TextAreaBox meta={{ name: field.title, placeholder: field.placeholder, max_length: field.max_length }} value={String(value)} disabled={disabled} onSave={onChange} />;
    case "number":
    case "slider": {
      const commit = () => {
        const why = numberProblem(field, text);
        setError(why);
        if (why) return setText(String(value));
        if (Number(text) !== value) onChange(Number(text));
      };
      return (
        <span className="set-list-number">
          <span className={`set-number${error ? " invalid" : ""}`}>
            <input
              inputMode="decimal"
              aria-label={field.title}
              aria-invalid={!!error}
              value={text}
              disabled={disabled}
              onChange={(e) => setText(e.target.value)}
              onBlur={commit}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") commit();
              }}
            />
            {field.unit && <span className="muted">{field.unit}</span>}
          </span>
          {error && <span className="set-error">{error}</span>}
        </span>
      );
    }
    default: {
      const commit = () => text !== value && onChange(text);
      return (
        <input
          className="set-text-input small"
          aria-label={field.title}
          placeholder={field.placeholder}
          maxLength={field.max_length ?? TEXT_MAX}
          value={text}
          disabled={disabled}
          onChange={(e) => setText(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") commit();
          }}
        />
      );
    }
  }
}
