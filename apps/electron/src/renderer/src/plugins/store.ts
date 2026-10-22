// What plugins add, as the renderer keeps it (docs/plugins.md, 「extensions.get / extensions.changed」):
// the registry's frontend view (extensions.get), every command with its keys and action
// (palette.commands, the key table), and the installed plugins (plugin.list). Read on connect,
// again for the kinds extensions.changed names and after a language switch (texts come localized).

import type { ExtensionsChangedEvent, PluginInfo, PluginProgressEvent, PluginToastEvent, ReportInfo, PanelInfo } from "@/protocol/generated";
import type { Rpc } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { type AppState, get, set, toast } from "@/state/store";
import { t } from "@/lib/i18n";

import { attachPlugins } from "./client";

/** The session negotiated plugins.v1 (a backend before it: no plugins, the core app as before). */
export const pluginsOn = (s: AppState = get()): boolean => s.extensions !== null;

/** A plugin's name as the interface shows it. */
export function pluginName(id: string | null | undefined, s: AppState = get()): string {
  if (!id) return "";
  return s.extensions?.plugins.find((p) => p.id === id)?.name ?? s.plugins?.plugins.find((p) => p.id === id)?.name ?? id;
}

/** The permissions a plugin was granted (empty while unknown). */
export function granted(id: string, s: AppState = get()): readonly string[] {
  const info = s.plugins?.plugins.find((p) => p.id === id);
  return info?.granted ?? [];
}

export const reportInfo = (id: string, s: AppState = get()): ReportInfo | undefined => s.extensions?.reports.find((r) => r.id === id);
export const panelInfo = (id: string, s: AppState = get()): PanelInfo | undefined => s.extensions?.panels.find((p) => p.id === id);

export async function refreshExtensions(): Promise<void> {
  try {
    set({ extensions: await call("extensions.get", {}) });
  } catch {
    /* not connected, or no plugins.v1 */
  }
}

export async function refreshCommands(): Promise<void> {
  try {
    set({ commands: (await call("palette.commands", {})).commands });
  } catch {
    /* not connected */
  }
}

export async function refreshPlugins(): Promise<void> {
  try {
    set({ plugins: await call("plugin.list", {}) });
  } catch {
    /* not connected, or no plugins.v1 */
  }
}

/** One plugin's entry changed (enable, disable, restart): put it in place. */
export function putPlugin(info: PluginInfo): void {
  set((s) => (s.plugins ? { plugins: { ...s.plugins, plugins: s.plugins.plugins.map((p) => (p.id === info.id ? info : p)) } } : s));
}

/** What each kind of contribution makes the renderer read again. */
async function refreshFor(kinds: readonly string[]): Promise<void> {
  const has = (...k: string[]) => k.some((x) => kinds.includes(x));
  const jobs: Promise<unknown>[] = [refreshExtensions()];
  if (has("command", "keybinding", "filter_preset", "visualization", "color_mode")) jobs.push(refreshCommands());
  if (has("visualization")) jobs.push(call("visual.list", {}).then((r) => set({ visuals: r.visuals })));
  if (has("setting")) jobs.push(call("settings.get", {}).then(async (st) => (await import("@/state/actions")).applySettings(st)));
  if (has("node_set", "filter_preset", "file_class", "folder_rule")) jobs.push(call("filter.vocab", {}).then((vocab) => set({ vocab })));
  if (has("export_format", "export_template")) set((s) => ({ exportsEpoch: s.exportsEpoch + 1 }));
  await Promise.allSettled(jobs);
}

function onToast(ev: PluginToastEvent): void {
  const name = pluginName(ev.plugin);
  toast(name ? t("{plugin}：{text}", { plugin: name, text: ev.text }) : ev.text, ev.tone === "danger" || ev.tone === "warn" ? 5000 : 2500);
}

function onProgress(ev: PluginProgressEvent): void {
  const key = String(ev.request ?? "");
  if (!key) return;
  set((s) => ({ pluginProgress: { ...s.pluginProgress, [key]: ev } }));
}

/** A new session (state/actions.ts connect): read what plugins add and follow its changes. */
export async function installPlugins(r: Rpc): Promise<void> {
  const caps = r.hello?.capabilities ?? [];
  set({ capabilities: caps });
  const commands = refreshCommands();
  if (!caps.includes("plugins.v1")) {
    set({ extensions: null, plugins: null });
    await commands;
    return;
  }
  attachPlugins(r);
  r.on("extensions.changed", (ev: ExtensionsChangedEvent) => void refreshFor(ev.kinds));
  r.on("plugin.changed", () => void refreshPlugins());
  r.on("plugin.toast", onToast);
  r.on("plugin.progress", onProgress);
  await Promise.allSettled([commands, refreshExtensions(), refreshPlugins()]);
}

/** The interface language changed: names, titles and legends come in the new one. */
export function relocalize(): void {
  void refreshCommands();
  if (pluginsOn()) {
    void refreshExtensions();
    void refreshPlugins();
  }
}
