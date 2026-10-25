// Fetching a report (docs/plugins.md 「报告文档」): its method with a ReportContext, the latest call
// only (a newer one cancels the one before, so does closing it), again when what its `refresh`
// names changes: the selection, the scan (done again), the view's frames (at most once a second),
// the comparison, a timer (interval:<s>, at least 5 s), a shared state of its plugin (state:<name>),
// its plugin's settings ("settings"), or the language (its texts come localized). "manual" adds a
// refresh button (ReportView).

import { useEffect, useRef, useState } from "react";

import type { PluginProgressEvent, Report, ReportContext } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { t } from "@/lib/i18n";
import { useThrottled } from "@/lib/throttle";
import { set, useApp } from "@/state/store";

import { callPlugin, subscribePlugin, wireName } from "../client";

export interface ReportSource {
  method: string;
  plugin: string;
  refresh: readonly string[];
}

export interface ReportState {
  report: Report | null;
  loading: boolean;
  error: string;
  /** plugin.progress of the running call */
  progress: PluginProgressEvent | null;
  reload(): void;
}

/** The interval of an interval:<seconds> refresh, ms (at least 5 s); null without one. */
export function intervalOf(refresh: readonly string[]): number | null {
  for (const r of refresh) {
    const m = /^interval:(\d+(?:\.\d+)?)$/.exec(r);
    if (m) return Math.max(5, Number(m[1])) * 1000;
  }
  return null;
}

/** What a report with refresh "settings" follows: its plugin's setting values, as one key. */
export function settingsKey(values: Record<string, unknown> | undefined, plugin: string): string {
  if (!values) return "";
  const prefix = `plugin.${plugin}.`;
  const own = Object.keys(values)
    .filter((k) => k.startsWith(prefix))
    .sort()
    .map((k) => [k, values[k]]);
  return JSON.stringify(own);
}

/** The shared states a report follows (state:<name>). */
export const statesOf = (refresh: readonly string[]): string[] => refresh.filter((r) => r.startsWith("state:")).map((r) => r.slice(6)).filter(Boolean);

/** A report that is a value already (a call action's answer) or comes from its method. */
export function useReport(src: ReportSource | null, ctx: ReportContext, fixed: Report | null = null): ReportState {
  const refresh = src?.refresh ?? [];
  const has = (r: string) => refresh.includes(r);
  const [report, setReport] = useState<Report | null>(fixed);
  const [loading, setLoading] = useState(!fixed && !!src);
  const [error, setError] = useState("");
  const [request, setRequest] = useState<number | null>(null);
  const [tick, setTick] = useState(0);
  const lang = useApp((s) => s.lang);
  const scanKey = useApp((s) => {
    const st = ctx.scan_id ? s.scans[ctx.scan_id] : undefined;
    return has("scan") && st ? `${st.state}|${st.finished_at ?? ""}` : "";
  });
  const seq = useApp((s) => (has("frame") && ctx.view_id ? (s.views[ctx.view_id]?.info?.seq ?? 0) : 0));
  const frameKey = useThrottled(seq, 1000);
  const progress = useApp((s) => (request != null ? (s.pluginProgress[String(request)] ?? null) : null));
  const selKey = has("selection") ? `${ctx.node ?? ""}|${(ctx.selection ?? []).join(",")}` : "";
  const compareKey = has("compare") ? (ctx.compare_id ?? "") : "";
  const ownSettings = useApp((s) => (has("settings") && src ? settingsKey(s.settings?.values, src.plugin) : ""));
  const every = intervalOf(refresh);
  const states = statesOf(refresh).join(",");
  const ctxRef = useRef(ctx);
  ctxRef.current = ctx;

  useEffect(() => {
    if (!every) return;
    const timer = setInterval(() => setTick((n) => n + 1), every);
    return () => clearInterval(timer);
  }, [every]);

  useEffect(() => {
    if (!src || !states) return;
    // the first value is the state as it is: only a change asks again
    const offs = states.split(",").map((name) => {
      let first = true;
      return subscribePlugin(wireName(src.plugin, name), null, () => {
        if (first) first = false;
        else setTick((n) => n + 1);
      });
    });
    return () => offs.forEach((off) => off());
  }, [src?.plugin, states]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (fixed) {
      setReport(fixed);
      setLoading(false);
      setError("");
      return;
    }
    if (!src) return;
    const ac = new AbortController();
    let id: number | null = null;
    setLoading(true);
    setError("");
    callPlugin(src.method, ctxRef.current, { signal: ac.signal, onId: (n) => setRequest((id = n)) })
      .then((value) => {
        if (ac.signal.aborted) return;
        const r = value as Report | null;
        const ok = !!r && typeof r === "object" && Array.isArray(r.blocks);
        setReport(ok ? r : null);
        if (!ok) setError(t("插件返回的不是报告"));
      })
      .catch((err: unknown) => {
        if (ac.signal.aborted || (err instanceof RpcError && err.code === -32800)) return;
        setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (ac.signal.aborted) return;
        setLoading(false);
        if (id != null) dropProgress(id);
      });
    return () => {
      ac.abort();
      if (id != null) dropProgress(id);
    };
  }, [fixed, src?.method, ctx.scan_id, selKey, scanKey, frameKey, compareKey, ownSettings, tick, lang]); // eslint-disable-line react-hooks/exhaustive-deps

  return { report, loading, error, progress, reload: () => setTick((n) => n + 1) };
}

function dropProgress(id: number): void {
  set((s) => {
    if (!(String(id) in s.pluginProgress)) return s;
    const next = { ...s.pluginProgress };
    delete next[String(id)];
    return { pluginProgress: next };
  });
}
