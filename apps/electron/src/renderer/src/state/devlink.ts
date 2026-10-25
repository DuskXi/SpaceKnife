// The development link's extra words (`spaceknife plugin dev` / `plugin shot`, docs/plugin-dev.md
// 「看真实界面」): after the linked scan's tab opens, the address may say what to show, so a page
// can be brought to a state without clicking:
//   #…&scan=<id>&theme=light&lang=en&select=logs/app.log&do=view.visual.<id>,report.export
//     &open=report:<full id>|panel:<full id>|settings:<plugin id>|export:<format or template>
// When it is done (or failed), <html data-sk-dev-ready="1"> (data-sk-dev-error says what failed):
// what plugin shot waits for before it takes the picture.

import type { ScanStatus } from "@/protocol/generated";

import { toast } from "./store";

/** The words of the address that this module reads (the rest is the shim's). */
export const DEV_WORDS = ["theme", "lang", "select", "do", "open"] as const;

export interface DevLink {
  theme: "light" | "dark" | null;
  lang: "en" | "zh-CN" | null;
  select: string | null;
  run: string[];
  open: string | null;
}

/** What the address asks for (pure: tested under node). */
export function parseDevLink(hash: string): DevLink {
  const p = new URLSearchParams(hash.replace(/^#/, ""));
  const theme = p.get("theme");
  const lang = p.get("lang");
  return {
    theme: theme === "light" || theme === "dark" ? theme : null,
    lang: lang ? (lang.startsWith("en") ? "en" : "zh-CN") : null,
    select: p.get("select") || null,
    run: (p.get("do") ?? "")
      .split(",")
      .map((x) => x.trim())
      .filter(Boolean),
    open: p.get("open") || null,
  };
}

/** A path below a scan root as the backend has it (its separator). */
export function joinRoot(root: string, rel: string): string {
  const sep = root.includes("\\") && !root.includes("/") ? "\\" : "/";
  const parts = rel.split(/[\\/]+/).filter(Boolean);
  return parts.length ? `${root.replace(/[\\/]+$/, "")}${sep}${parts.join(sep)}` : root;
}

function mark(error?: string): void {
  if (typeof document === "undefined") return;
  if (error) document.documentElement.dataset.skDevError = error.slice(0, 500);
  document.documentElement.dataset.skDevReady = "1";
}

const settle = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Bring the page to the state the address asks for (the linked scan's tab is open). */
export async function applyDevLink(scan: ScanStatus): Promise<void> {
  const link = parseDevLink(typeof location === "undefined" ? "" : location.hash);
  try {
    const { updateSettings } = await import("./actions");
    const values: Record<string, string> = {};
    if (link.theme) values["appearance.theme"] = link.theme;
    if (link.lang) values["general.language"] = link.lang;
    if (Object.keys(values).length) await updateSettings(values);
    if (link.select) {
      const { showNode } = await import("@/plugins/nodes");
      const root = scan.roots[0]?.path ?? "";
      await showNode({ scan_id: scan.scan_id, node: 0, path: joinRoot(root, link.select) });
    }
    if (link.run.length) {
      const { runShortcut } = await import("./commands");
      for (const id of link.run) {
        await runShortcut(id);
        await settle(150);
      }
    }
    if (link.open?.startsWith("export:")) {
      const { set } = await import("./store");
      set({ exportOpen: true, exportPick: link.open.slice(7) }); // the export dialog, on that format
    } else if (link.open) {
      const { openTarget } = await import("@/plugins/open");
      const { actionContext } = await import("@/plugins/actions");
      openTarget(link.open, actionContext({ owner: null }));
    }
    await settle(300);
    mark();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    toast(message);
    mark(message);
  }
}
