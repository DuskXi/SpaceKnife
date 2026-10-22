// Settings tab (W7, UI-W7-01…37). Every row is rendered from the backend's metadata
// (settings.get): name, description, control, options, range, when it applies and whether it can
// be changed here. Changes save immediately; the backend validates and broadcasts them.

import { useEffect, useMemo, useRef, useState } from "react";

import type { PaletteCommand, SettingMeta } from "@/protocol/generated";
import { call, closeSettings, resetSettings, settingsGo, updateSettings } from "@/state/actions";
import { type SettingsPageId, set, toast, useApp } from "@/state/store";
import { fmtKeys } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon, type IconName, Kbd } from "@/ui/Icon";
import { PluginIcon } from "@/plugins/icons";
import { pluginName } from "@/plugins/store";

import { AboutEntry, AboutHeader, ComponentsGroup, ReleaseTimeline } from "./AboutPage";
import { AdvancedTools, ClearTags } from "./AdvancedTools";
import { ClassesEditor } from "./ClassesEditor";
import { PluginsPage } from "./PluginsPage";

type Value = boolean | number | string;

// name and note translate each time they are read (getters), so a language switch applies (M7.5)
export const PAGES: { id: SettingsPageId; readonly name: string; icon: IconName; readonly note: string }[] = [
  {
    id: "general",
    get name() {
      return t("常规");
    },
    icon: "settings",
    get note() {
      return t("语言、悬停提示、窗口提醒与日志");
    },
  },
  {
    id: "appearance",
    get name() {
      return t("外观");
    },
    icon: "palette",
    get note() {
      return t("主题、强调色、动画与可视化外观；更改立即生效");
    },
  },
  {
    // the category id stays "treemap" (settings keys treemap.*); the page covers every visualization
    id: "treemap",
    get name() {
      return t("可视化");
    },
    icon: "layers",
    get note() {
      return t("几何、火焰图方向与动画；修改后已打开的视图立即重排，「新视图…」类只影响之后新开的视图");
    },
  },
  {
    id: "scan",
    get name() {
      return t("扫描与性能");
    },
    icon: "gauge",
    get note() {
      return t("扫描方式、扫描范围与硬链接计算从下一次扫描起生效，其余更改立即生效");
    },
  },
  {
    id: "classes",
    get name() {
      return t("文件类别");
    },
    icon: "file",
    get note() {
      return t("按扩展名归类，用于树图着色、:class: 过滤和类别统计");
    },
  },
  {
    id: "tags",
    get name() {
      return t("标签");
    },
    icon: "tag",
    get note() {
      return t("四个固定标签：红▲、黄■、绿●、蓝◆，颜色与形状不可修改");
    },
  },
  {
    id: "plugins",
    get name() {
      return t("插件");
    },
    icon: "bolt",
    get note() {
      return t("安装、启用与停用插件；插件自己的设置也在这里");
    },
  },
  {
    id: "keys",
    get name() {
      return t("快捷键");
    },
    icon: "command",
    get note() {
      return t("全部命令与当前快捷键，与命令面板同一份命令清单");
    },
  },
  {
    id: "advanced",
    get name() {
      return t("高级");
    },
    icon: "terminal",
    get note() {
      return t("设置文件、日志、缓存与快照");
    },
  },
  {
    id: "about",
    get name() {
      return t("关于与更新");
    },
    icon: "info",
    get note() {
      return t("版本信息、更新与发布时间线");
    },
  },
];
const GROUPS: SettingsPageId[][] = [
  ["general", "appearance", "treemap"],
  ["scan", "classes", "tags", "plugins"],
  ["keys", "advanced", "about"],
];
const pageName = (id: string) => PAGES.find((p) => p.id === id)?.name ?? id;

export function SettingsPage() {
  const state = useApp((s) => s.settings);
  const page = useApp((s) => s.settingsPage);
  const query = useApp((s) => s.settingsQuery);
  const conn = useApp((s) => s.conn);
  const lang = useApp((s) => s.lang); // the search also matches the page names, in the interface language
  const meta = state?.meta ?? EMPTY_META;
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const hits = useMemo(() => (query.trim() ? search(meta, query.trim()) : null), [meta, query, lang]);
  const counts = useMemo(() => {
    const m = new Map<string, number>();
    for (const h of hits ?? []) m.set(h.category, (m.get(h.category) ?? 0) + 1);
    return m;
  }, [hits]);

  return (
    <div className="settings">
      <nav className="settings-nav" aria-label={t("设置分类")}>
        <div className="settings-nav-list">
          {GROUPS.map((group, gi) => (
            <div key={gi} className="settings-nav-group">
              {group.map((id) => {
                const p = PAGES.find((x) => x.id === id)!;
                const n = counts.get(id) ?? 0;
                const on = !hits && page === id;
                return (
                  <button
                    key={id}
                    className={`settings-nav-item${on ? " on" : ""}${hits && !n ? " faded" : ""}`}
                    aria-current={on ? "page" : undefined}
                    onClick={() => settingsGo(id)}
                  >
                    <Icon name={p.icon} />
                    <span className="flex">{p.name}</span>
                    {hits && n > 0 && <span className="settings-hit-count mono">{n}</span>}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
        <button className="settings-nav-file" disabled={!state} onClick={() => state && void window.sk.shell.openPath(state.path)}>
          <Icon name="reveal" size={14} />
          {t("打开设置文件")}
        </button>
      </nav>
      <div className="settings-main">
        <div className="settings-scroll">
          {conn !== "ready" && <div className="settings-banner warn">{t("与后端的连接已断开，设置暂不可修改")}</div>}
          {state?.error && <div className="settings-banner warn">{state.error}</div>}
          {!state ? (
            <div className="muted">{t("正在读取设置…")}</div>
          ) : hits ? (
            <SearchResults hits={hits} query={query.trim()} />
          ) : (
            <Page id={page} meta={meta} />
          )}
        </div>
        <Footer page={page} />
      </div>
    </div>
  );
}

const EMPTY_META: SettingMeta[] = [];

function Page({ id, meta }: { id: SettingsPageId; meta: SettingMeta[] }) {
  const p = PAGES.find((x) => x.id === id) ?? PAGES[0]!;
  const rows = meta.filter((m) => m.category === p.id);
  const groups = groupBy(rows);
  return (
    <>
      <header className="settings-head">
        <h1>{p.name}</h1>
        <p className="muted">{p.note}</p>
      </header>
      {p.id === "about" && <AboutHeader />}
      {p.id === "classes" && <ClassesEditor />}
      {p.id === "plugins" && <PluginsPage />}
      {p.id === "keys" && <KeysPage rows={rows} />}
      {p.id !== "keys" && (
        <div className="settings-columns">
          {groups.map(([group, items]) => (
            <Group key={group} title={group} note={items[0]?.group_note ?? ""} rows={items} />
          ))}
          {p.id === "tags" && <ClearTags />}
          {p.id === "advanced" && <AdvancedTools />}
          {p.id === "advanced" && <FilesGroup />}
          {p.id === "about" && <ComponentsGroup />}
          {p.id === "general" && <AboutEntry />}
        </div>
      )}
      {p.id === "about" && <ReleaseTimeline />}
    </>
  );
}

function groupBy(rows: SettingMeta[]): [string, SettingMeta[]][] {
  const out = new Map<string, SettingMeta[]>();
  for (const r of rows) out.set(r.group, [...(out.get(r.group) ?? []), r]);
  return [...out.entries()];
}

function Group({ title, note, rows, query }: { title: string; note: string; rows: SettingMeta[]; query?: string }) {
  return (
    <section className="set-group" aria-label={title}>
      <div className="set-group-head">
        <h2>{title}</h2>
        {note && <span className="muted small">{note}</span>}
      </div>
      <div className="set-card">
        {rows.map((m) => (
          <Row key={m.key} meta={m} query={query} />
        ))}
      </div>
    </section>
  );
}

function Row({ meta, query }: { meta: SettingMeta; query?: string }) {
  const value = useApp((s) => s.settings?.values[meta.key]);
  const values = useApp((s) => s.settings?.values);
  const conn = useApp((s) => s.conn);
  const scanning = useApp((s) => Object.values(s.scans).some((x) => x.state === "scanning" || x.state === "queued"));
  const dependsOk = useMemo(() => {
    if (!meta.depends || !values) return true;
    const [k, want] = meta.depends.split("=");
    const v = values[k!];
    return want === undefined ? Boolean(v) : String(v) === want;
  }, [meta.depends, values]);
  const disabled = !meta.enabled || !dependsOk || conn !== "ready";
  const id = `set-${meta.key}`;
  const save = (v: Value) => void updateSettings({ [meta.key]: v });
  return (
    <div className={`set-row${disabled ? " disabled" : ""}`} id={id}>
      <div className="set-text">
        <div className="set-name">
          <Mark text={meta.name} query={query} />
          {!meta.enabled && meta.reason && <span className="set-badge">{meta.reason}</span>}
        </div>
        <div className="set-desc" id={`${id}-desc`}>
          {meta.description}
        </div>
        {meta.applies === "next_scan" && scanning && meta.enabled && <div className="set-note">{t("将在下一次扫描生效")}</div>}
        {meta.applies === "new_views" && meta.enabled && <div className="set-note subtle">{t("只影响之后新开的视图")}</div>}
      </div>
      <div className="set-control">
        <Control meta={meta} value={value} disabled={disabled} onSave={save} describedBy={`${id}-desc`} />
      </div>
    </div>
  );
}

function Control(props: { meta: SettingMeta; value: Value | undefined; disabled: boolean; onSave: (v: Value) => void; describedBy: string }) {
  const { meta, value, disabled, onSave } = props;
  switch (meta.control) {
    case "switch":
      return (
        <button
          role="switch"
          aria-checked={Boolean(value)}
          aria-label={meta.name}
          aria-describedby={props.describedBy}
          className={`switch${value ? " on" : ""}`}
          disabled={disabled}
          title={disabled ? meta.reason : undefined}
          onClick={() => onSave(!value)}
        >
          <span />
        </button>
      );
    case "segmented":
      return (
        <div className="segmented small" role="radiogroup" aria-label={meta.name} aria-describedby={props.describedBy}>
          {meta.options.map((o) => (
            <button
              key={String(o.value)}
              role="radio"
              aria-checked={value === o.value}
              className={value === o.value ? "on" : ""}
              disabled={disabled || !!o.reason}
              title={o.reason || undefined}
              onClick={() => value !== o.value && onSave(o.value)}
            >
              {o.label}
            </button>
          ))}
        </div>
      );
    case "color":
      return (
        <div className="set-swatches" role="radiogroup" aria-label={meta.name}>
          {meta.options.map((o) => (
            <button
              key={String(o.value)}
              role="radio"
              aria-checked={value === o.value}
              aria-label={String(o.label)}
              className={`set-swatch${value === o.value ? " on" : ""}`}
              style={{ background: String(o.value) }}
              disabled={disabled}
              onClick={() => onSave(o.value)}
            />
          ))}
        </div>
      );
    case "slider":
      return <Slider meta={meta} value={Number(value ?? 0)} disabled={disabled} onSave={onSave} />;
    case "number":
      return <NumberBox meta={meta} value={Number(value ?? 0)} disabled={disabled} onSave={onSave} />;
    default:
      return <TextBox meta={meta} value={String(value ?? "")} disabled={disabled} onSave={onSave} />;
  }
}

function Slider({ meta, value, disabled, onSave }: { meta: SettingMeta; value: number; disabled: boolean; onSave: (v: Value) => void }) {
  const [live, setLive] = useState(value);
  useEffect(() => setLive(value), [value]);
  const commit = () => live !== value && onSave(live);
  return (
    <div className="set-slider">
      <input
        type="range"
        aria-label={t("{name}滑块", { name: meta.name })}
        min={meta.min ?? 0}
        max={meta.max ?? 100}
        step={meta.step ?? 1}
        value={live}
        disabled={disabled}
        onChange={(e) => setLive(Number(e.target.value))}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      <span className="set-value mono">
        {live}
        {meta.unit && <span className="muted"> {meta.unit}</span>}
      </span>
    </div>
  );
}

function NumberBox({ meta, value, disabled, onSave }: { meta: SettingMeta; value: number; disabled: boolean; onSave: (v: Value) => void }) {
  const [text, setText] = useState(String(value));
  const [error, setError] = useState("");
  useEffect(() => setText(String(value)), [value]);
  const commit = () => {
    const n = Number(text.trim());
    const lo = meta.min ?? -Infinity;
    const hi = meta.max ?? Infinity;
    if (!Number.isInteger(n) || n < lo || n > hi) {
      setError(t("请输入 {lo}–{hi} 的整数", { lo, hi }));
      setText(String(value));
      return;
    }
    setError("");
    if (n !== value) onSave(n);
  };
  return (
    <div className="set-number-wrap">
      <span className={`set-number${error ? " invalid" : ""}`}>
        <input
          inputMode="numeric"
          aria-label={meta.name}
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
        {meta.unit && <span className="muted">{meta.unit}</span>}
      </span>
      {error && <span className="set-error">{error}</span>}
    </div>
  );
}

function TextBox({ meta, value, disabled, onSave }: { meta: SettingMeta; value: string; disabled: boolean; onSave: (v: Value) => void }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const commit = () => text.trim() !== value && onSave(text.trim());
  return (
    <input
      className="set-text-input"
      aria-label={meta.name}
      placeholder={t("未命名")}
      maxLength={64}
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

// ------------------------------------------------------------------ search (UI-W7-05)

function search(meta: SettingMeta[], query: string): SettingMeta[] {
  const q = query.toLowerCase();
  return meta.filter((m) =>
    [m.name, m.description, m.group, pageName(m.category), ...m.synonyms, ...m.options.map((o) => `${o.label} ${o.value}`)].some((x) => x.toLowerCase().includes(q)),
  );
}

function SearchResults({ hits, query }: { hits: SettingMeta[]; query: string }) {
  if (!hits.length) return <div className="settings-empty muted">{t("没有找到与「{query}」相关的设置", { query })}</div>;
  const byPage = PAGES.map((p) => [p, hits.filter((h) => h.category === p.id)] as const).filter(([, h]) => h.length);
  // the key cap sits inside the sentence: split the translated text around {key}
  const [before, after] = t("{n} 项设置 · 按 {key} 清空", { n: hits.length }).split("{key}");
  return (
    <>
      <header className="settings-head">
        <h1>{t("搜索结果")}</h1>
        <p className="muted">
          {before}
          <Kbd>Esc</Kbd>
          {after}
        </p>
      </header>
      {byPage.map(([p, rows]) => (
        <div key={p.id} className="settings-result-page">
          {groupBy(rows).map(([group, items]) => (
            <Group key={group} title={`${p.name} › ${group}`} note="" rows={items} query={query} />
          ))}
        </div>
      ))}
    </>
  );
}

function Mark({ text, query }: { text: string; query?: string }) {
  if (!query) return <>{text}</>;
  const at = text.toLowerCase().indexOf(query.toLowerCase());
  if (at < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, at)}
      <mark>{text.slice(at, at + query.length)}</mark>
      {text.slice(at + query.length)}
    </>
  );
}

// ------------------------------------------------------------------ special pages

function KeysPage({ rows }: { rows: SettingMeta[] }) {
  const [commands, setCommands] = useState<PaletteCommand[] | null>(null);
  const [q, setQ] = useState("");
  useEffect(() => {
    void call("palette.commands", {}).then((r) => setCommands(r.commands));
  }, []);
  const shown = (commands ?? []).filter((c) => !q || c.name.includes(q) || c.keys.some((k) => k.toLowerCase().includes(q.toLowerCase())));
  return (
    <div className="settings-columns single">
      {groupBy(rows).map(([group, items]) => (
        <Group key={group} title={group} note="" rows={items} />
      ))}
      <section className="set-group" aria-label={t("键位表")}>
        <div className="set-group-head">
          <h2>{t("键位表")}</h2>
          <input className="set-text-input small" placeholder={t("搜索命令或按键")} value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
        </div>
        <div className="set-card keys-table">
          {!commands && <div className="set-row muted">{t("正在读取…")}</div>}
          {shown.map((c) => (
            <div key={c.id} className="key-row">
              <PluginIcon plugin={c.plugin} name={c.icon || "command"} size={14} />
              <span className="flex">
                {c.name}
                {c.plugin && <span className="muted small"> · {pluginName(c.plugin)}</span>}
              </span>
              {c.keys.length ? c.keys.map((k) => <Kbd key={k}>{fmtKeys(k)}</Kbd>) : <span className="muted small">{t("未绑定")}</span>}
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

function FilesGroup() {
  const state = useApp((s) => s.settings);
  const version = useApp((s) => s.serverVersion);
  if (!state) return null;
  const copyDiagnostics = async () => {
    const info = await call("system.info", {}).catch(() => null);
    const lines = [
      t("SpaceKnife 前端 {frontend} · 后端 {backend}", { frontend: window.sk.version, backend: version }),
      t("平台 {platform} · {agent}", { platform: window.sk.platform, agent: navigator.userAgent }),
      info ? t("后端 {info}", { info: JSON.stringify(info) }) : t("后端信息不可用"),
      t("设置文件 {path}", { path: state.path }),
      t("日志目录 {path}", { path: state.logs }),
    ];
    window.sk.shell.copyText(lines.join("\n"));
    toast(t("诊断信息已复制"));
  };
  return (
    <section className="set-group" aria-label={t("文件与诊断")}>
      <div className="set-group-head">
        <h2>{t("文件与诊断")}</h2>
      </div>
      <div className="set-card">
        <FileRow name={t("设置文件")} path={state.path} />
        <FileRow name={t("日志文件夹")} path={state.logs} />
        <div className="set-row">
          <div className="set-text">
            <div className="set-name">{t("复制诊断信息")}</div>
            <div className="set-desc">{t("版本、平台与能力清单，用于反馈问题")}</div>
          </div>
          <div className="set-control">
            <button className="ghost small" onClick={() => void copyDiagnostics()}>
              <Icon name="copy" size={14} />
              {t("复制")}
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}

function FileRow({ name, path }: { name: string; path: string }) {
  return (
    <div className="set-row">
      <div className="set-text">
        <div className="set-name">{name}</div>
        <div className="set-desc mono" title={path}>
          {path}
        </div>
      </div>
      <div className="set-control">
        <button className="ghost small" onClick={() => void window.sk.shell.openPath(path)}>
          <Icon name="reveal" size={14} />
          {t("打开")}
        </button>
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ footer (UI-W7-22…24)

function Footer({ page }: { page: SettingsPageId }) {
  const state = useApp((s) => s.settings);
  const save = useApp((s) => s.settingsSave);
  const query = useApp((s) => s.settingsQuery);
  const [undo, setUndo] = useState<Record<string, Value> | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const keys = (state?.meta ?? []).filter((m) => m.category === page && m.enabled).map((m) => m.key);
  const changed = state ? keys.filter((k) => state.values[k] !== state.defaults[k]) : [];
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);

  const restore = async () => {
    if (!state) return;
    const before = Object.fromEntries(changed.map((k) => [k, state.values[k]!]));
    await resetSettings(page);
    setUndo(before);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setUndo(null), 5000);
  };

  return (
    <footer className="settings-foot">
      {save.state === "saving" ? (
        <span className="muted">{t("正在保存…")}</span>
      ) : save.state === "error" ? (
        <span className="warn settings-save">
          <Icon name="warning" size={14} />
          {t("保存失败：{message}", { message: save.message })}
        </span>
      ) : (
        <span className="muted settings-save">
          <Icon name="check" size={14} />
          {t("更改已自动保存")}
        </span>
      )}
      {undo && (
        <span className="settings-undo">
          {t("已恢复「{page}」的默认值", { page: pageName(page) })} ·{" "}
          <button
            className="link"
            onClick={() => {
              void updateSettings(undo);
              setUndo(null);
            }}
          >
            {t("撤销")}
          </button>
        </span>
      )}
      <span className="flex" />
      <button className="ghost" disabled={!!query.trim() || changed.length === 0} title={changed.length ? t("把「{page}」的 {n} 项恢复为默认值", { page: pageName(page), n: changed.length }) : t("当前页已经全部是默认值")} onClick={() => void restore()}>
        {t("恢复默认")}
      </button>
      <button className="primary small" onClick={() => void closeSettings()}>
        {t("完成")}
      </button>
    </footer>
  );
}

/** Toolbar search box of the settings tab (UI-W7-05). */
export function SettingsSearch() {
  const query = useApp((s) => s.settingsQuery);
  return (
    <div className="filter settings-search">
      <span className="filter-icon">
        <Icon name="search" />
      </span>
      <input
        id="sk-settings-search"
        className="filter-input plain"
        placeholder={t("搜索设置")}
        aria-label={t("搜索设置")}
        value={query}
        spellCheck={false}
        onChange={(e) => set({ settingsQuery: e.target.value })}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") {
            if (query) set({ settingsQuery: "" });
            else (e.target as HTMLInputElement).blur();
          }
        }}
      />
      <kbd className="kbd filter-kbd">/</kbd>
    </div>
  );
}
