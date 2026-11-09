// The Electron shell's main process and preload (out/main, out/preload). The interface is not built
// here: it is apps/desktop's Nuxt build (`pnpm generate` there), which the app serves on app://
// (src/main/index.ts) and electron-builder.yml packs as resources/ui.

import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "electron-vite";

const here = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  main: {
    build: { rollupOptions: { input: { index: resolve(here, "src/main/index.ts") } } },
  },
  preload: {
    build: { rollupOptions: { input: { index: resolve(here, "src/preload/index.ts") } } },
  },
});
