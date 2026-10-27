// Unit tests for the renderer's pure modules (formatting, draw-list decoding, treemap colours).
// Aliases mirror the renderer section of electron.vite.config.ts.

import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

import { helpPictures } from "./vite-plugins/help-pictures";

const here = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  plugins: [helpPictures(resolve(here, "../../site"))],
  resolve: {
    alias: {
      "@": resolve(here, "src/renderer/src"),
      "@design": resolve(here, "../../design/build"),
      "@site": resolve(here, "../../site/src"),
    },
  },
  test: {
    environment: "node",
    include: ["src/renderer/src/**/*.test.ts", "src/main/**/*.test.ts", "vite-plugins/**/*.test.ts"],
    setupFiles: ["src/renderer/src/test-setup.ts"],
    // the module cache keeps lib/i18n.ts with its import.meta.glob result: catalogs added later went unseen
    fsModuleCache: false,
  },
});
