// Browser mode: when the page runs outside Electron (a plain browser tab used for development and
// automated checks), stand in for the preload API. The backend address and token come from the
// URL hash (#ws=…&token=…), as tools/demo/m3_web.py prints them; window controls do nothing.

import type { BackendState, SkApi } from "../../shared/api";

import { t } from "@/lib/i18n";

export function installShim(): void {
  if (window.sk) return;
  const params = new URLSearchParams(location.hash.slice(1));
  const url = params.get("ws");
  const token = params.get("token");
  const state: BackendState = url && token ? { state: "ready", info: { url, token, pid: 0 } } : { state: "failed", message: t("浏览器模式：地址里缺少 #ws=…&token=…") };
  const platform = (params.get("platform") as SkApi["platform"]) ?? (navigator.userAgent.includes("Windows") ? "win32" : navigator.userAgent.includes("Mac") ? "darwin" : "linux");
  const noop = () => {};
  window.sk = {
    platform,
    version: "0.3.0-web",
    backend: async () => state,
    onBackend: () => noop,
    window: {
      minimize: noop,
      toggleMaximize: noop,
      close: noop,
      isMaximized: async () => false,
      flash: noop,
      onMaximized: () => noop,
    },
    shell: {
      showItemInFolder: (path) => console.info("showItemInFolder", path),
      openPath: async (path) => (console.info("openPath", path), ""),
      openTerminal: (dir) => console.info("openTerminal", dir),
      copyText: (text) => void navigator.clipboard?.writeText(text).catch(noop),
      openFullDiskAccess: () => console.info("openFullDiskAccess"),
    },
    setLanguage: () => undefined,
    dialog: {
      openSnapshot: async () => window.prompt(t("快照文件路径（.sks）"))?.trim() || null,
      openFile: async (o) => window.prompt(o.title)?.trim() || null,
      saveFile: async (o) => window.prompt(o.title, o.defaultName)?.trim() || null,
      openFolders: async () => {
        const text = window.prompt(t("文件夹路径（多个用 ; 分隔）"))?.trim();
        return text ? text.split(";").map((x) => x.trim()).filter(Boolean) : [];
      },
    },
    files: {
      pathOf: () => "",
    },
  };
}
