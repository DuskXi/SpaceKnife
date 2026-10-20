// Command palette (W4, UI-W4-01…49). The backend ranks everything (palette.query / palette.paths /
// palette.preview); this component renders the groups, keeps one row highlighted, and runs the
// chosen row. Requests are cancelled as the input changes; old rows stay until new ones arrive.

import { useEffect, useMemo, useRef, useState } from "react";

import type {
  PaletteCommand,
  PaletteDirect,
  PalettePath,
  PalettePaths,
  PalettePreview,
  PaletteRecent,
  PaletteResult,
  Span,
} from "@/protocol/generated";
import { call, cancellable, editFilter, startScan, viewCall } from "@/state/actions";
import { runCommand } from "@/state/commands";
import { activeTab, basketCount, focusedViewId, set, useApp } from "@/state/store";
import { fmtInt, fmtKeys, fmtSize, middlePath } from "@/lib/format";
import { t } from "@/lib/i18n";
import { Icon, type IconName, Kbd } from "@/ui/Icon";

import { diagnosticText, renderColoured } from "./FilterBox";

type Row =
  | { kind: "filter"; d: PaletteDirect }
  | { kind: "goto"; d: PaletteDirect }
  | { kind: "recent"; r: PaletteRecent }
  | { kind: "command"; c: PaletteCommand }
  | { kind: "path"; p: PalettePath };

type Group = "direct" | "recent" | "command" | "path";

interface Line {
  row: Row;
  group: Group;
  enabled: boolean;
}

export function Palette() {
  const open = useApp((s) => s.paletteOpen);
  return open ? <PaletteDialog /> : null;
}

function PaletteDialog() {
  const tab = useApp((s) => activeTab(s));
  const viewId = useApp((s) => focusedViewId(s));
  const basket = useApp(basketCount);
  // live values (UI-W4-24): a scan state change refreshes the lists; while a scan runs, every 2 s
  const scanState = useApp((s) => (tab.scanId ? (s.scans[tab.scanId]?.state ?? "") : ""));
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (scanState !== "scanning" && scanState !== "queued") return;
    const timer = setInterval(() => setTick((x) => x + 1), 2000);
    return () => clearInterval(timer);
  }, [scanState]);
  const scanTab = tab.kind === "scan" && !!tab.scanId;
  const [returnTo] = useState(() => document.activeElement as HTMLElement | null);
  const [text, setText] = useState("");
  const [res, setRes] = useState<PaletteResult | null>(null);
  const [paths, setPaths] = useState<PalettePaths | null>(null);
  const [searching, setSearching] = useState(false);
  const [previews, setPreviews] = useState<Record<string, PalettePreview>>({});
  const [active, setActive] = useState(-1);
  const [reload, setReload] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const composing = useRef(false);
  const shownText = useRef<string | null>(null);
  const pendingEnter = useRef(false); // Enter pressed before the answer for the current input arrived

  const close = () => set({ paletteOpen: false });
  useEffect(() => {
    input.current?.focus();
    return () => returnTo?.focus?.();
  }, [returnTo]);

  // palette.query, then palette.paths and filter previews; superseded requests are cancelled
  useEffect(() => {
    // the help tab is, to the backend's commands, a tab without a scan like the settings tab
    const context = { tab: tab.kind === "help" ? ("settings" as const) : tab.kind, scan_id: tab.scanId ?? null, view_id: viewId, basket };
    const refresh = shownText.current === text && paths !== null;
    const jobs: { cancel: () => void }[] = [];
    let alive = true;
    const q = cancellable("palette.query", { text, context });
    jobs.push(q);
    void q.promise
      .then((r) => {
        if (!alive || !r) return;
        setRes(r);
        if (shownText.current !== r.text) setActive(-1); // new input: highlight the first row again
        shownText.current = r.text;
        if (pendingEnter.current && r.text === text) {
          pendingEnter.current = false;
          setTimeout(() => runFirst.current(r), 0);
        }
        if (r.paths && tab.scanId) {
          if (!refresh) setSearching(true); // a live refresh keeps the shown rows without 「搜索中…」 (UI-W4-44)
          const p = cancellable("palette.paths", { scan_id: tab.scanId, text, view_id: viewId, limit: text.trim() ? 50 : 5 });
          jobs.push(p);
          void p.promise
            .then((found) => {
              if (alive && found) {
                setPaths(found);
                setSearching(false);
              }
            })
            .catch(() => alive && setSearching(false));
        } else {
          setPaths(null);
          setSearching(false);
        }
        if (tab.scanId) {
          const exprs = [...r.direct.filter((d) => d.kind === "filter" && d.valid).map((d) => d.text), ...r.recent.filter((x) => x.kind === "filter").map((x) => x.text)];
          for (const expr of exprs) {
            const job = cancellable("palette.preview", { scan_id: tab.scanId, text: expr });
            jobs.push(job);
            void job.promise.then((pv) => alive && pv && setPreviews((m) => ({ ...m, [expr]: pv }))).catch(() => {});
          }
        }
      })
      .catch(() => {});
    return () => {
      alive = false;
      for (const j of jobs) j.cancel();
    };
  }, [text, tab.kind, tab.scanId, viewId, basket, reload, scanState, tick]);

  const lines = useMemo<Line[]>(() => {
    if (!res) return [];
    const out: Line[] = [];
    for (const d of res.direct) {
      if (d.kind === "filter") out.push({ row: { kind: "filter", d }, group: "direct", enabled: d.valid && scanTab });
      else out.push({ row: { kind: "goto", d }, group: "direct", enabled: d.state === "outside" || (d.state === "in_scan" && scanTab) });
    }
    for (const r of res.recent) out.push({ row: { kind: "recent", r }, group: "recent", enabled: scanTab && r.exists });
    for (const c of res.commands) out.push({ row: { kind: "command", c }, group: "command", enabled: c.enabled });
    if (res.paths && paths) for (const p of paths.items) out.push({ row: { kind: "path", p }, group: "path", enabled: true });
    return out;
  }, [res, paths, scanTab]);

  const firstEnabled = lines.findIndex((l) => l.enabled);
  const current = active >= 0 && active < lines.length ? active : firstEnabled >= 0 ? firstEnabled : 0;

  useEffect(() => {
    document.getElementById(`pal-${current}`)?.scrollIntoView({ block: "nearest" });
  }, [current]);

  // ------------------------------------------------------------------ running rows

  const jump = async (node: number, path: string) => {
    if (!viewId) return;
    await viewCall(viewId, "view.zoom", { key: node });
    void call("palette.remember", { kind: "jump", text: path, scan_id: tab.scanId ?? null }).catch(() => {});
  };

  // Enter that waited for this input's answer runs its first executable row
  const runFirst = useRef<(r: PaletteResult) => void>(() => {});
  runFirst.current = (r) => {
    const direct = r.direct.find((d) => (d.kind === "filter" ? d.valid && scanTab : d.state === "outside" || (d.state === "in_scan" && scanTab)));
    if (direct) return run({ row: direct.kind === "filter" ? { kind: "filter", d: direct } : { kind: "goto", d: direct }, group: "direct", enabled: true });
    const rec = r.recent.find((x) => scanTab && x.exists);
    if (rec) return run({ row: { kind: "recent", r: rec }, group: "recent", enabled: true });
    const cmd = r.commands.find((c) => c.enabled);
    if (cmd) run({ row: { kind: "command", c: cmd }, group: "command", enabled: true });
  };

  const run = (line: Line | undefined) => {
    if (!line || !line.enabled) return;
    const row = line.row;
    close();
    setTimeout(() => {
      switch (row.kind) {
        case "filter":
          if (viewId) {
            void editFilter(viewId, row.d.text, null, true);
            void call("palette.remember", { kind: "filter", text: row.d.text, scan_id: null }).catch(() => {});
          }
          return;
        case "goto":
          if (row.d.state === "in_scan" && row.d.node != null) void jump(row.d.node, row.d.text);
          else if (row.d.state === "outside") void startScan({ roots: [row.d.text] });
          return;
        case "recent":
          if (row.r.kind === "jump" && row.r.node != null) void jump(row.r.node, row.r.text);
          else if (row.r.kind === "filter" && viewId) {
            void editFilter(viewId, row.r.text, null, true);
            void call("palette.remember", { kind: "filter", text: row.r.text, scan_id: null }).catch(() => {});
          }
          return;
        case "command":
          void runCommand(row.c.id);
          return;
        case "path":
          void jump(row.p.node, row.p.complete.replace(/[\\/]+$/, "") || row.p.complete);
      }
    }, 0);
  };

  const complete = (line: Line | undefined) => {
    if (!line) return;
    const row = line.row;
    let next: string | null = null;
    if (row.kind === "goto") next = row.d.complete ?? null;
    else if (row.kind === "path") next = row.p.complete;
    else if (row.kind === "command") next = row.c.name;
    else if (row.kind === "recent") next = row.r.kind === "jump" && row.r.is_dir ? withSep(row.r.text) : row.r.text;
    if (row.kind === "filter") {
      void call("filter.parse", { text, cursor: text.length }).then((p) => {
        const c = p.completion;
        if (c && c.candidates[0] !== undefined) setText(text.slice(0, c.start) + c.candidates[0] + text.slice(c.end));
      });
      return;
    }
    if (next != null) setText(next);
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (composing.current || e.nativeEvent.isComposing || e.keyCode === 229) return; // the IME owns these keys
    const n = lines.length;
    const move = (to: number) => {
      e.preventDefault();
      if (n) setActive(((to % n) + n) % n);
    };
    if (e.key === "ArrowDown") move(current + 1);
    else if (e.key === "ArrowUp") move(current - 1);
    else if (e.key === "PageDown") move(Math.min(n - 1, current + 10));
    else if (e.key === "PageUp") move(Math.max(0, current - 10));
    else if (e.key === "Enter") {
      e.preventDefault();
      // the rows on screen may still answer an older input: run once the current one is answered (UI-W4-42)
      if (res?.text !== text) pendingEnter.current = true;
      else run(lines[current]);
    } else if (e.key === "Tab") {
      e.preventDefault();
      complete(lines[current]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else return;
    e.stopPropagation();
  };

  // ------------------------------------------------------------------ rendering

  const groups: { id: Group; title: string; extra?: React.ReactNode }[] = [
    { id: "direct", title: "" },
    {
      id: "recent",
      title: t("最近"),
      extra: (
        <button
          className="pal-clear"
          aria-label={t("清除最近记录")}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            void call("palette.clear", { scan_id: tab.scanId ?? null }).then(() => setReload((x) => x + 1));
            input.current?.focus();
          }}
        >
          {t("清除")}
        </button>
      ),
    },
    { id: "command", title: t("命令") },
    {
      id: "path",
      title: t("路径"),
      extra: searching ? (
        <span>{t("搜索中…")}</span>
      ) : paths?.partial ? (
        <span className="warn">{t("扫描未完成，结果可能不全")}</span>
      ) : res?.seed ? (
        <span>{t("匹配选中项「{seed}」", { seed: res.seed })}</span>
      ) : paths?.folder ? (
        <span>{t("{n} 项", { n: fmtInt(paths.total) })}</span>
      ) : paths && paths.total > paths.items.length ? (
        <span>{t("显示 {shown} / {total}", { shown: paths.items.length, total: fmtInt(paths.total) })}</span>
      ) : null,
    },
  ];

  const pathGroupVisible = !!res?.paths && (searching || (paths?.items.length ?? 0) > 0);
  const noResults = !!res && lines.length === 0 && !searching;
  let index = 0;

  return (
    <div className="pal-layer">
      <div className="scrim" onMouseDown={close} />
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label={t("命令面板")}
        onMouseDown={(e) => {
          // keep the caret in the input: headers, hints and padding must not take the focus (UI-W4-38)
          if (e.target !== input.current && !(e.target as HTMLElement).closest("button")) e.preventDefault();
        }}
      >
        <div className="pal-input">
          <Icon name="search" size={20} />
          <input
            ref={input}
            value={text}
            placeholder={t("输入命令、路径或过滤条件…")}
            spellCheck={false}
            role="combobox"
            aria-label={t("命令面板输入")}
            aria-expanded="true"
            aria-controls="sk-palette-list"
            aria-autocomplete="list"
            aria-activedescendant={lines.length ? `pal-${current}` : undefined}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKey}
            onCompositionStart={() => (composing.current = true)}
            onCompositionEnd={() => (composing.current = false)}
          />
          {scanTab && (
            <span className="pal-scope" title={t("命令面板作用于这个扫描")}>
              <Icon name="drive" size={14} />
              {tab.title}
            </span>
          )}
        </div>
        <div ref={list} id="sk-palette-list" className="pal-list" role="listbox" aria-label={t("命令、路径与最近")}>
          {groups.map((g) => {
            const members = lines.filter((l) => l.group === g.id);
            const visible = g.id === "path" ? pathGroupVisible : members.length > 0;
            if (!visible) return null;
            return (
              <div key={g.id} role="group" aria-label={g.title || t("直达")} className="pal-group">
                {g.title && (
                  <div className="pal-head">
                    <span aria-hidden="true">{g.title}</span>
                    <span className="pal-head-extra" role={g.id === "path" ? "status" : undefined}>
                      {g.extra}
                    </span>
                  </div>
                )}
                {members.map((l) => {
                  const k = index++;
                  return (
                    <PaletteRow
                      key={k}
                      id={`pal-${k}`}
                      line={l}
                      on={k === current}
                      previews={previews}
                      onHover={() => setActive(k)}
                      onRun={() => run(l)}
                    />
                  );
                })}
              </div>
            );
          })}
          {noResults && <div className="pal-empty">{t("没有匹配的命令或路径")}</div>}
        </div>
        <div className="pal-hints">
          <span>
            <Kbd>↑</Kbd>
            <Kbd>↓</Kbd> {t("选择")}
          </span>
          <span>
            <Kbd>Enter</Kbd> {t("执行")}
          </span>
          <span>
            <Kbd>Tab</Kbd> {t("补全")}
          </span>
          <span>
            <Kbd>Esc</Kbd> {t("关闭")}
          </span>
        </div>
        <div className="sr-only" role="status" aria-live="polite">
          {res ? t("{n} 个结果", { n: lines.length }) : ""}
        </div>
      </div>
    </div>
  );
}

function PaletteRow(props: { id: string; line: Line; on: boolean; previews: Record<string, PalettePreview>; onHover: () => void; onRun: () => void }) {
  const { line, on } = props;
  const row = line.row;
  let icon: IconName = "command";
  let main: React.ReactNode = null;
  let secondary: React.ReactNode = null;
  let right: React.ReactNode = null;
  let label = "";
  let tone = "";
  const stats = (expr: string) => {
    const pv = props.previews[expr];
    return pv ? (pv.valid ? t("{n} 项 · {size}", { n: fmtInt(pv.files), size: fmtSize(pv.alloc) }) : "") : t("搜索中…");
  };
  switch (row.kind) {
    case "filter": {
      icon = "filter";
      const errs = row.d.diagnostics;
      main = (
        <>
          <span className="pal-verb">{t("过滤")}</span>
          <code className="pal-expr">{renderColoured(row.d.text, { text: row.d.text, tokens: row.d.tokens, diagnostics: errs })}</code>
        </>
      );
      if (!row.d.valid && errs[0]) {
        tone = " error";
        secondary = <span className="danger">{diagnosticText(errs[0])}</span>;
      } else right = <span className="mono">{stats(row.d.text)}</span>;
      label = errs[0] ? t("过滤 {text}，错误：{error}", { text: row.d.text, error: diagnosticText(errs[0]) }) : t("过滤 {text}", { text: row.d.text });
      break;
    }
    case "goto": {
      icon = row.d.is_dir ? "folder" : "file";
      main = (
        <>
          <span className="pal-verb">{t("跳转到")}</span>
          <span className="mono pal-path">{row.d.text}</span>
        </>
      );
      if (row.d.state === "in_scan") right = <span className="mono">{fmtSize(row.d.alloc ?? 0)}</span>;
      else if (row.d.state === "outside") {
        secondary = <span>{t("不在当前扫描范围内")}</span>;
        right = <span className="pal-action">{t("扫描此位置")}</span>;
      } else secondary = <span>{t("路径不存在")}</span>;
      label = t("跳转到 {path}", { path: row.d.text });
      break;
    }
    case "recent": {
      const r = row.r;
      icon = r.kind === "filter" ? "filter" : r.is_dir ? "folder" : "file";
      if (r.kind === "filter") {
        main = (
          <>
            <span className="pal-verb">{t("过滤")}</span>
            <code className="pal-expr">{renderColoured(r.text, { text: r.text, tokens: r.tokens, diagnostics: [] })}</code>
          </>
        );
        right = <span className="mono">{stats(r.text)}</span>;
        label = t("过滤 {text}", { text: r.text });
      } else {
        main = (
          <>
            <span className="pal-verb">{t("跳转到")}</span>
            <span className="mono pal-path">
              <Hl text={r.text} hits={r.hits} />
            </span>
          </>
        );
        if (r.exists) right = <span className="mono">{fmtSize(r.alloc ?? 0)}</span>;
        else secondary = <span>{t("已不存在")}</span>;
        label = t("跳转到 {path}", { path: r.text });
      }
      break;
    }
    case "command": {
      const c = row.c;
      icon = (c.icon as IconName) || "command";
      main = <Hl text={c.name} hits={c.hits} />;
      secondary = c.status ? <span>{c.status}</span> : null;
      right = c.keys[0] ? <Kbd>{fmtKeys(c.keys[0])}</Kbd> : null;
      label = [c.name, c.status, c.keys[0] ? fmtKeys(c.keys[0]) : ""].filter(Boolean).join(t("，"));
      break;
    }
    case "path": {
      const p = row.p;
      icon = p.is_dir ? "folder" : "file";
      main = <Hl text={p.name} hits={p.hits} />;
      secondary = (
        <span className="mono" title={p.parent}>
          {middlePath(p.parent)}
        </span>
      );
      right = <span className="mono">{fmtSize(p.alloc)}</span>;
      label = [p.name, p.parent, fmtSize(p.alloc)].join(t("，"));
      break;
    }
  }
  return (
    <div
      id={props.id}
      className={`pal-row${on ? " on" : ""}${line.enabled ? "" : " disabled"}${tone}`}
      role="option"
      aria-selected={on}
      aria-disabled={!line.enabled}
      aria-label={label}
      onMouseMove={props.onHover}
      onMouseDown={(e) => e.preventDefault()}
      onClick={props.onRun}
    >
      <span className="pal-icon">
        <Icon name={icon} size={16} />
      </span>
      <span className="pal-main">{main}</span>
      {secondary && <span className="pal-sub">{secondary}</span>}
      <span className="pal-right">{right}</span>
    </div>
  );
}

/** Text with the matched characters (UTF-16 spans from the backend) in bold accent. */
function Hl({ text, hits }: { text: string; hits: Span[] }) {
  if (!hits.length) return <>{text}</>;
  const out: React.ReactNode[] = [];
  let at = 0;
  hits.forEach((h, i) => {
    if (h.start > at) out.push(text.slice(at, h.start));
    out.push(
      <b key={i} className="pal-hit">
        {text.slice(h.start, h.end)}
      </b>,
    );
    at = h.end;
  });
  if (at < text.length) out.push(text.slice(at));
  return <>{out}</>;
}

function withSep(path: string): string {
  const sep = path.includes("\\") || /^[A-Za-z]:/.test(path) ? "\\" : "/";
  return path.endsWith(sep) ? path : path + sep;
}
