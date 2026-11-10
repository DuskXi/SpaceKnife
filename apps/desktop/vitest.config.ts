// Unit tests: the framework-free modules (formatting, draw-list decoding, treemap colours, state),
// the composables and the Vue components. Aliases mirror nuxt.config.ts. Tests run in node; a
// component test opts into a DOM with `// @vitest-environment happy-dom`.

import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

import { helpPictures } from "./vite-plugins/help-pictures.ts";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");

export default defineConfig({
  plugins: [vue(), helpPictures(resolve(repo, "site"))],
  // the design tokens and the website's documentation live outside this folder
  server: { fs: { allow: [repo] } },
  resolve: {
    alias: {
      "@design": resolve(repo, "design/build"),
      "@site": resolve(repo, "site/src"),
      "@": resolve(here, "app"),
      "~": resolve(here, "app"),
    },
  },
  test: {
    environment: "node",
    include: ["app/**/*.test.ts", "vite-plugins/**/*.test.ts"],
    setupFiles: ["app/test-setup.ts"],
    // the module cache keeps lib/i18n.ts with its import.meta.glob result: catalogs added later went unseen
    fsModuleCache: false,
  },
});
