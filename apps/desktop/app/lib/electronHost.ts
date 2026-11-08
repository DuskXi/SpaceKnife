// window.sk in the Electron shell's window (apps/electron-shell): the host API (shared/api.ts) on top
// of the preload's window.skElectron, a thin bridge to the main process. Every call is one of the IPC
// channels of apps/electron-shell/src/main/index.ts (registerIpc), the names below; the rest of the
// interface calls window.sk exactly as it does on Tauri (tauriHost.ts).
//
// Files dropped on the window: Electron gives the page the system's own drag events, and the
// preload tells a dropped File's path (webUtils.getPathForFile). onDropPaths and onDragState follow
// those events on the window, for files only (a drag inside the page is not one), as Tauri's
// drag-drop event does; a drop anywhere else in the window opens nothing.

import type { BackendState, FileDialogOptions, SkApi } from "@/shared/api";
import type { InstallHow } from "@/shared/install";
import type { ThemeSetting } from "@/shared/theme";

/** What the preload exposes as window.skElectron (apps/electron-shell/src/preload/index.ts). */
export interface ElectronBridge {
  platform: SkApi["platform"];
  version: string;
  justUpdated: boolean;
  versions: SkApi["versions"];
  invoke(channel: string, ...args: unknown[]): Promise<unknown>;
  send(channel: string, ...args: unknown[]): void;
  on(channel: string, cb: (value: unknown) => void): () => void;
  pathOf(file: File): string;
}

declare global {
  interface Window {
    skElectron?: ElectronBridge;
  }
}

/** How long a drag over the window may go without a dragover before it counts as gone (a dragleave
 *  can go missing when the files leave over another window). Browsers send dragover while files are
 *  held over the page, also when they don't move, at least every 350 ms ± 200 ms (HTML's drag-and-drop
 *  processing model). */
export const DRAG_GONE_MS = 1000;

const carriesFiles = (e: DragEvent) => !!e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files");

/** Files dragged over and dropped on the window: the overlay's state and the dropped paths. */
export class DropWatcher {
  private readonly drops = new Set<(paths: string[]) => void>();
  private readonly states = new Set<(over: boolean) => void>();
  private over = false;
  private depth = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    target: Pick<Window, "addEventListener">,
    private readonly pathOf: (file: File) => string,
  ) {
    // the capture phase: before the page's own handlers, which may stop the event
    target.addEventListener("dragenter", (e) => this.enter(e), true);
    target.addEventListener("dragover", (e) => this.overEvent(e), true);
    target.addEventListener("dragleave", (e) => this.leave(e), true);
    target.addEventListener("drop", (e) => this.drop(e), true);
  }

  onDrop(cb: (paths: string[]) => void): () => void {
    this.drops.add(cb);
    return () => this.drops.delete(cb);
  }

  onState(cb: (over: boolean) => void): () => void {
    this.states.add(cb);
    return () => this.states.delete(cb);
  }

  private set(over: boolean): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = over ? setTimeout(() => ((this.depth = 0), this.set(false)), DRAG_GONE_MS) : null;
    if (over === this.over) return;
    this.over = over;
    for (const cb of [...this.states]) cb(over);
  }

  private enter(e: DragEvent): void {
    if (!carriesFiles(e)) return;
    this.depth++;
    this.set(true);
  }

  private overEvent(e: DragEvent): void {
    if (!carriesFiles(e)) return;
    // files may be dropped anywhere in the window (else Chromium would open the file)
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    this.set(true);
  }

  private leave(e: DragEvent): void {
    if (!carriesFiles(e)) return;
    this.depth = Math.max(0, this.depth - 1);
    if (this.depth === 0) this.set(false);
  }

  private drop(e: DragEvent): void {
    if (!carriesFiles(e)) return;
    e.preventDefault();
    this.depth = 0;
    this.set(false);
    const paths = Array.from(e.dataTransfer?.files ?? [])
      .map((f) => this.pathOf(f))
      .filter(Boolean);
    if (!paths.length) return;
    for (const cb of [...this.drops]) cb(paths);
  }
}

export function electronApi(bridge: ElectronBridge, target: Pick<Window, "addEventListener"> = window): SkApi {
  const invoke = <T>(channel: string, ...args: unknown[]) => bridge.invoke(channel, ...args) as Promise<T>;
  const on = <T>(channel: string, cb: (value: T) => void) => bridge.on(channel, (v) => cb(v as T));
  const drops = new DropWatcher(target, (f) => bridge.pathOf(f));
  return {
    platform: bridge.platform,
    version: bridge.version,
    host: "app",
    remote: null,
    justUpdated: bridge.justUpdated,
    versions: bridge.versions,
    backend: () => invoke<BackendState>("sk:backend"),
    onBackend: (cb) => on<BackendState>("sk:backend", cb),
    setLanguage: (lang: "zh-CN" | "en") => bridge.send("sk:setLanguage", lang),
    setTheme: (theme: ThemeSetting) => bridge.send("sk:setTheme", theme),
    window: {
      minimize: () => bridge.send("sk:win", "minimize"),
      toggleMaximize: () => bridge.send("sk:win", "toggleMaximize"),
      close: () => bridge.send("sk:win", "close"),
      isMaximized: () => invoke<boolean>("sk:isMaximized"),
      flash: () => bridge.send("sk:win", "flash"),
      onMaximized: (cb) => on<boolean>("sk:maximized", cb),
    },
    shell: {
      showItemInFolder: (path) => bridge.send("sk:showItemInFolder", path),
      openPath: (path) => invoke<string>("sk:openPath", path),
      openTerminal: (dir) => bridge.send("sk:openTerminal", dir),
      copyText: (text) => bridge.send("sk:copyText", text),
      openFullDiskAccess: () => bridge.send("sk:openFullDiskAccess"),
      openExternal: (url) => bridge.send("sk:openExternal", url),
    },
    update: {
      install: (path: string, how: InstallHow) => invoke<string>("sk:installUpdate", path, how),
    },
    dialog: {
      openSnapshot: () => invoke<string | null>("sk:openSnapshot"),
      openFolders: () => invoke<string[]>("sk:openFolders"),
      openFile: (options: FileDialogOptions) => invoke<string | null>("sk:openFile", options),
      saveFile: (options: FileDialogOptions & { defaultName: string }) => invoke<string | null>("sk:saveFile", options),
    },
    files: {
      pathOf: (file) => bridge.pathOf(file),
      onDropPaths: (cb) => drops.onDrop(cb),
      onDragState: (cb) => drops.onState(cb),
    },
  };
}
