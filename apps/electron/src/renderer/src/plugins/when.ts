// `when` of a report, a panel or a cleanup (docs/plugins.md 「when 条件」): all must hold, else the
// place it sits in says why. Commands' `when` the backend decides (palette.check); these are shown
// without asking, so the renderer reads the same names here. Pure: tested under node.

import { t } from "@/lib/i18n";

/** What the conditions look at. */
export interface WhenEnv {
  /** the scan it is about, and its state */
  scan: { state: string; method?: string; source?: string | null } | null;
  /** a view of that scan exists */
  view: boolean;
  selection: boolean;
  compare: boolean;
  setting(key: string): unknown;
  capabilities: readonly string[];
  /** enabled plugins */
  plugins: readonly string[];
  /** the plugin whose names are local (setting:<name>, source:<name>) */
  owner: string | null;
  /** the scan source of the scan, when it came from one (scan.start source) */
  sourceId?: string | null;
  /** the selected node, when the place knows it (file / dir / ext: need it; without it a selection
   * is enough) */
  node?: { kind: "file" | "dir" | "other"; name: string; virtual?: boolean } | null;
}

/** Why the conditions don't hold (the first that fails), or null when they all do; an unknown
 * condition never holds, so a newer plugin's place shows why instead of misbehaving. */
export function whenReason(when: readonly string[], env: WhenEnv): string | null {
  for (const raw of when) {
    const [name, ...rest] = raw.split(":");
    const arg = rest.join(":");
    const why = check(name ?? "", arg, env);
    if (why) return why;
  }
  return null;
}

const full = (owner: string | null, name: string) => (owner && !name.includes(".") ? `${owner}.${name}` : name);

function check(name: string, arg: string, env: WhenEnv): string | null {
  const scanning = env.scan?.state === "scanning" || env.scan?.state === "queued";
  switch (name) {
    case "scan":
      return env.scan ? null : t("需要先扫描");
    case "done":
      return env.scan?.state === "done" ? null : scanning ? t("扫描完成后可用") : t("需要先完成扫描");
    case "scanning":
      return scanning ? null : t("只在扫描进行中可用");
    case "view":
      return env.view ? null : t("需要打开一个扫描视图");
    case "selection":
      return env.selection ? null : t("没有选中项");
    case "scan_view":
      return env.compare ? t("对比模式下不可用") : null;
    case "compare":
      return env.compare ? null : t("只在快照对比中可用");
    case "setting": {
      const key = arg.startsWith("plugin.") || arg.includes(".") ? arg : `plugin.${full(env.owner, arg)}`;
      return env.setting(key) ? null : t("需要先在设置里打开这一项");
    }
    case "cap":
      return env.capabilities.includes(arg) ? null : t("这个界面不支持：{what}", { what: arg });
    case "plugin":
      return env.plugins.includes(arg) ? null : t("需要启用插件「{id}」", { id: arg });
    case "source":
      return env.sourceId && env.sourceId === full(env.owner, arg) ? null : t("只用于这个插件的扫描来源");
    case "file":
    case "dir": {
      if (!env.selection) return t("没有选中项");
      if (!env.node) return null;
      if (env.node.kind !== name) return name === "file" ? t("只用于文件") : t("只用于文件夹");
      return null;
    }
    case "ext": {
      if (!env.selection) return t("没有选中项");
      if (!env.node) return null;
      const exts = arg.split(",").map((e) => e.trim().toLowerCase().replace(/^\./, "")).filter(Boolean);
      const dot = env.node.name.lastIndexOf(".");
      const ext = dot > 0 ? env.node.name.slice(dot + 1).toLowerCase() : "";
      return env.node.kind === "file" && exts.includes(ext) ? null : t("只用于这几种文件");
    }
    case "on_disk":
      return env.sourceId || env.node?.virtual ? t("这次扫描的条目不在磁盘上") : null;
    default:
      return t("这个版本不认识的条件：{name}", { name: name || "?" });
  }
}
