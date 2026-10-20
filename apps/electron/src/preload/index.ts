import { contextBridge, ipcRenderer, webUtils } from "electron";

import type { BackendState, SkApi } from "../shared/api";

// main passes app.getVersion() (package.json of the app) as an argument of this renderer process
const VERSION_ARG = "--sk-app-version=";
const appVersion = process.argv.find((arg) => arg.startsWith(VERSION_ARG))?.slice(VERSION_ARG.length);

const api: SkApi = {
  platform: process.platform as SkApi["platform"],
  version: appVersion ?? process.env.npm_package_version ?? "0.3.1",
  backend: () => ipcRenderer.invoke("sk:backend") as Promise<BackendState>,
  onBackend(cb) {
    const listener = (_e: unknown, state: BackendState) => cb(state);
    ipcRenderer.on("sk:backend", listener);
    return () => ipcRenderer.off("sk:backend", listener);
  },
  window: {
    minimize: () => ipcRenderer.send("sk:win", "minimize"),
    toggleMaximize: () => ipcRenderer.send("sk:win", "toggleMaximize"),
    close: () => ipcRenderer.send("sk:win", "close"),
    isMaximized: () => ipcRenderer.invoke("sk:isMaximized") as Promise<boolean>,
    flash: () => ipcRenderer.send("sk:win", "flash"),
    onMaximized(cb) {
      const listener = (_e: unknown, maximized: boolean) => cb(maximized);
      ipcRenderer.on("sk:maximized", listener);
      return () => ipcRenderer.off("sk:maximized", listener);
    },
  },
  shell: {
    showItemInFolder: (path) => ipcRenderer.send("sk:showItemInFolder", path),
    openPath: (path) => ipcRenderer.invoke("sk:openPath", path) as Promise<string>,
    openTerminal: (dir) => ipcRenderer.send("sk:openTerminal", dir),
    copyText: (text) => ipcRenderer.send("sk:copyText", text),
    openFullDiskAccess: () => ipcRenderer.send("sk:openFullDiskAccess"),
    openExternal: (url) => ipcRenderer.send("sk:openExternal", url),
  },
  update: {
    install: (path) => ipcRenderer.invoke("sk:installUpdate", path) as Promise<string>,
  },
  setLanguage: (lang) => ipcRenderer.send("sk:setLanguage", lang),
  dialog: {
    openSnapshot: () => ipcRenderer.invoke("sk:openSnapshot") as Promise<string | null>,
    openFolders: () => ipcRenderer.invoke("sk:openFolders") as Promise<string[]>,
    openFile: (options) => ipcRenderer.invoke("sk:openFile", options) as Promise<string | null>,
    saveFile: (options) => ipcRenderer.invoke("sk:saveFile", options) as Promise<string | null>,
  },
  files: {
    pathOf: (file) => webUtils.getPathForFile(file),
  },
};

contextBridge.exposeInMainWorld("sk", api);
