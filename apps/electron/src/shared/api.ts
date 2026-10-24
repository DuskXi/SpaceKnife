// The API the preload script exposes to the renderer as `window.sk` (main ↔ renderer contract).

import type { InstallHow } from "./install";
import type { ThemeSetting } from "./theme";

export interface BackendInfo {
  url: string;
  token: string;
  pid: number;
}

export type BackendState =
  | { state: "starting" }
  | { state: "ready"; info: BackendInfo }
  | { state: "failed"; message: string }
  | { state: "exited"; code: number | null };

export interface FileDialogOptions {
  title: string;
  filters: { name: string; extensions: string[] }[];
}

/** Browser mode: the computer the files are on (headless mode's /sk/info, main/headless-server.ts). */
export interface RemoteInfo {
  /** its name, shown in the title bar and next to deleting */
  hostname: string;
  /** the user's home folder there: where a typed path starts */
  home: string;
  /** reached over HTTPS */
  secure: boolean;
}

export interface SkApi {
  /** the platform of the computer the files are on (the server's in browser mode, not the browser's) */
  platform: "win32" | "darwin" | "linux";
  version: string;
  /** "app": SpaceKnife's own window. "browser": a browser tab served by headless mode (--headless,
   *  --listen): no window buttons, file dialogs, file manager, terminal or update install there. */
  host: "app" | "browser";
  /** browser mode: the computer the files are on; null in the app */
  remote: RemoteInfo | null;
  /** started by 「无缝更新」: the window says once that the update is done */
  justUpdated: boolean;
  /** the runtime's versions, for 「关于与更新」 */
  versions: { electron: string; chrome: string; node: string };
  backend(): Promise<BackendState>;
  /** The interface language changed (M7.5): the main process's own texts follow. */
  setLanguage(lang: "zh-CN" | "en"): void;
  /** The theme setting (W7 外观 · 主题): the window's background, Electron's own UI and the page's
   *  prefers-color-scheme follow, and the next start opens in it. Nothing in browser mode. */
  setTheme(theme: ThemeSetting): void;
  onBackend(cb: (state: BackendState) => void): () => void;
  window: {
    minimize(): void;
    toggleMaximize(): void;
    close(): void;
    isMaximized(): Promise<boolean>;
    /** Ask for attention: taskbar flash / dock bounce / urgency hint (W7 常规 · 扫描结束提醒). */
    flash(): void;
    onMaximized(cb: (maximized: boolean) => void): () => void;
  };
  shell: {
    showItemInFolder(path: string): void;
    openPath(path: string): Promise<string>;
    openTerminal(dir: string): void;
    copyText(text: string): void;
    /** macOS: System Settings › Privacy & Security › Full Disk Access (nothing elsewhere). */
    openFullDiskAccess(): void;
    /** Open a page of this project on GitHub (a release page) in the browser; other URLs are refused. */
    openExternal(url: string): void;
    /** A plugin's https page in the browser (its `url` action or panel), after the user said yes. */
    openUrl(url: string): void;
  };
  update: {
    /** One of the update card's two buttons (shared/install.ts installStep): 「无缝更新」 puts the
     * version in place and starts it again; the classic way opens the installer, disk image or package
     * and quits (or shows the file in its folder). Only a file in the backend's updates folder named
     * like a release asset; "" when under way, else why not. Only ever after a click. */
    install(path: string, how: InstallHow): Promise<string>;
  };
  dialog: {
    /** Pick a snapshot file (UI-W1-45, W4 「打开快照文件…」); null when cancelled. */
    openSnapshot(): Promise<string | null>;
    /** Pick one or more folders (UI-W1-26 「浏览…」); [] when cancelled. */
    openFolders(): Promise<string[]>;
    /** Pick one file to read (settings import, SpaceSniffer configuration); null when cancelled. */
    openFile(options: FileDialogOptions): Promise<string | null>;
    /** Choose where to write a file (export, settings export); null when cancelled. */
    saveFile(options: FileDialogOptions & { defaultName: string }): Promise<string | null>;
  };
  files: {
    /** Absolute path of a file or folder dropped on the window ('' when unknown, e.g. browser mode). */
    pathOf(file: File): string;
  };
}

declare global {
  interface Window {
    sk: SkApi;
  }
}
