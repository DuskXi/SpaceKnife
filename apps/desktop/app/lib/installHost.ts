// Where window.sk comes from: Tauri in SpaceKnife's own window (lib/tauriHost.ts), else the browser
// shim (shim.ts: headless mode, the development demo, `pnpm dev` in a plain browser). The interface
// loads after it (plugins/host.client.ts): modules read window.sk.platform when they load.

import { isTauri } from "@tauri-apps/api/core";

import { installShim } from "@/shim";

export async function installHost(): Promise<void> {
  if (window.sk) return;
  if (isTauri()) {
    const { tauriApi } = await import("./tauriHost");
    window.sk = await tauriApi();
    return;
  }
  await installShim();
}
