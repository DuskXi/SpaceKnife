// What the page gets from the Electron shell: window.skElectron, a thin bridge to the main process
// (index.ts registerIpc). The interface builds its host API (window.sk) on top of it in
// apps/desktop/app/lib/electronHost.ts, as it does on Tauri in tauriHost.ts. Only the channels below
// pass; the page gets no Node or Electron object.

import { contextBridge, ipcRenderer, webUtils } from "electron";

// main passes app.getVersion() (package.json of the app) as an argument of this renderer process
const VERSION_ARG = "--sk-app-version=";
const appVersion = process.argv.find((arg) => arg.startsWith(VERSION_ARG))?.slice(VERSION_ARG.length);

/** Calls with an answer (ipcMain.handle). */
const INVOKE = new Set(["sk:backend", "sk:isMaximized", "sk:openPath", "sk:installUpdate", "sk:openFolders", "sk:openFile", "sk:saveFile", "sk:openSnapshot"]);
/** Calls without one (ipcMain.on). */
const SEND = new Set(["sk:win", "sk:showItemInFolder", "sk:copyText", "sk:setLanguage", "sk:setTheme", "sk:openTerminal", "sk:openFullDiskAccess", "sk:openExternal"]);
/** What main tells the page. */
const EVENTS = new Set(["sk:backend", "sk:maximized"]);

const bridge = {
  platform: process.platform,
  version: appVersion ?? "",
  justUpdated: process.argv.includes("--sk-updated"),
  versions: { runtime: "Electron", runtimeVersion: process.versions.electron ?? "", webview: `Chromium ${process.versions.chrome ?? ""}`.trim() },
  invoke(channel: string, ...args: unknown[]): Promise<unknown> {
    if (!INVOKE.has(channel)) return Promise.reject(new Error(`not a SpaceKnife call: ${channel}`));
    return ipcRenderer.invoke(channel, ...args);
  },
  send(channel: string, ...args: unknown[]): void {
    if (SEND.has(channel)) ipcRenderer.send(channel, ...args);
  },
  on(channel: string, cb: (value: unknown) => void): () => void {
    if (!EVENTS.has(channel)) return () => {};
    const listener = (_e: unknown, value: unknown) => cb(value);
    ipcRenderer.on(channel, listener);
    return () => ipcRenderer.off(channel, listener);
  },
  /** The absolute path of a file or folder dropped on the window ("" when it has none). */
  pathOf(file: File): string {
    try {
      return webUtils.getPathForFile(file);
    } catch {
      return "";
    }
  },
};

contextBridge.exposeInMainWorld("skElectron", bridge);
