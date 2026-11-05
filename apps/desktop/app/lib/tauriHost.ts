// window.sk in SpaceKnife's own window: the host API (shared/api.ts) on top of Tauri. Every call is a
// command of src-tauri/src/commands.rs (the names below), every subscription one of its events; the
// rest of the interface calls window.sk exactly as it called Electron's preload API.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import type { BackendState, FileDialogOptions, SkApi } from "@/shared/api";
import type { InstallHow } from "@/shared/install";
import type { ThemeSetting } from "@/shared/theme";

/** The events src-tauri emits (src-tauri/src/events.rs). */
export const EVENTS = { backend: "sk://backend", maximized: "sk://maximized" } as const;

/** What the window knows before anything else (commands.rs app_info). */
export interface AppInfo {
  platform: SkApi["platform"];
  version: string;
  justUpdated: boolean;
  versions: SkApi["versions"];
}

/** Subscribe now, return an unsubscribe that works before the subscription settled as well. */
function subscribe<T>(start: (cb: (value: T) => void) => Promise<UnlistenFn>, cb: (value: T) => void): () => void {
  let stop: UnlistenFn | null = null;
  let stopped = false;
  void start(cb).then((un) => {
    if (stopped) un();
    else stop = un;
  });
  return () => {
    stopped = true;
    stop?.();
    stop = null;
  };
}

const on = <T>(event: string, cb: (value: T) => void) => subscribe<T>((f) => listen<T>(event, (e) => f(e.payload)), cb);

/** Calls that answer nothing: fire and forget, failures to the console (as ipcRenderer.send). */
const send = (cmd: string, args?: Record<string, unknown>): void => {
  invoke(cmd, args).catch((err: unknown) => console.warn(`[sk] ${cmd}:`, err));
};

export async function tauriApi(): Promise<SkApi> {
  const info = await invoke<AppInfo>("app_info");
  return {
    platform: info.platform,
    version: info.version,
    host: "app",
    remote: null,
    justUpdated: info.justUpdated,
    versions: info.versions,
    backend: () => invoke<BackendState>("backend_state"),
    onBackend: (cb) => on<BackendState>(EVENTS.backend, cb),
    setLanguage: (lang: "zh-CN" | "en") => send("set_language", { lang }),
    setTheme: (theme: ThemeSetting) => send("set_theme", { theme }),
    window: {
      minimize: () => send("window_minimize"),
      toggleMaximize: () => send("window_toggle_maximize"),
      close: () => send("window_close"),
      isMaximized: () => invoke<boolean>("window_is_maximized"),
      flash: () => send("window_flash"),
      onMaximized: (cb) => on<boolean>(EVENTS.maximized, cb),
    },
    shell: {
      showItemInFolder: (path) => send("show_item_in_folder", { path }),
      openPath: (path) => invoke<string>("open_path", { path }),
      openTerminal: (dir) => send("open_terminal", { dir }),
      copyText: (text) => send("copy_text", { text }),
      openFullDiskAccess: () => send("open_full_disk_access"),
      openExternal: (url) => send("open_external", { url }),
    },
    update: {
      install: (path: string, how: InstallHow) => invoke<string>("install_update", { path, how }),
    },
    dialog: {
      openSnapshot: () => invoke<string | null>("dialog_open_snapshot"),
      openFolders: () => invoke<string[]>("dialog_open_folders"),
      openFile: (options: FileDialogOptions) => invoke<string | null>("dialog_open_file", { options }),
      saveFile: (options: FileDialogOptions & { defaultName: string }) => invoke<string | null>("dialog_save_file", { options }),
    },
    files: {
      // a webview's File carries no path; drops come with their paths through onDropPaths
      pathOf: () => "",
      onDropPaths: (cb) =>
        subscribe<string[]>(
          (f) =>
            getCurrentWebview().onDragDropEvent((e) => {
              if (e.payload.type === "drop") f(e.payload.paths);
            }),
          cb,
        ),
    },
  };
}
