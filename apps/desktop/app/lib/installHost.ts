// Where window.sk comes from, in this order: one already there (tests); the Electron shell's window
// (apps/electron-shell: its preload exposes window.skElectron, lib/electronHost.ts); the Tauri shell's
// window (lib/tauriHost.ts); else the browser shim (shim.ts: headless mode, the development demo,
// `pnpm dev` in a plain browser). The interface loads after it (plugins/host.client.ts): modules read
// window.sk.platform when they load.

import { isTauri } from "@tauri-apps/api/core";

import { installShim } from "@/shim";

export async function installHost(): Promise<void> {
  if (window.sk) return;
  if (window.skElectron) {
    const { electronApi } = await import("./electronHost");
    window.sk = electronApi(window.skElectron);
    return;
  }
  if (isTauri()) {
    const { tauriApi } = await import("./tauriHost");
    window.sk = await tauriApi();
    return;
  }
  await installShim();
}
