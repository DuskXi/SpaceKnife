// Unit tests for the renderer's pure modules (formatting, draw-list decoding, treemap colours).
// Aliases mirror the renderer section of electron.vite.config.ts.

import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const here = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  resolve: {
    alias: {
      "@": resolve(here, "src/renderer/src"),
      "@design": resolve(here, "../../design/build"),
    },
  },
  test: {
    environment: "node",
    include: ["src/renderer/src/**/*.test.ts"],
  },
});
