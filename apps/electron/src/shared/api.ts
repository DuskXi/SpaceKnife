// The API the preload script exposes to the renderer as `window.sk` (main ↔ renderer contract).

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

export interface SkApi {
  platform: "win32" | "darwin" | "linux";
  version: string;
  backend(): Promise<BackendState>;
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
  };
  dialog: {
    /** Pick a snapshot file (UI-W1-45, W4 「打开快照文件…」); null when cancelled. */
    openSnapshot(): Promise<string | null>;
    /** Pick one or more folders (UI-W1-26 「浏览…」); [] when cancelled. */
    openFolders(): Promise<string[]>;
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
