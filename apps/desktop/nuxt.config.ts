// The desktop app's interface: a Nuxt 4 single-page app (no server rendering), generated to static
// files that Tauri (src-tauri) serves in its window. `pnpm dev` / `pnpm tauri dev` serve it from the
// dev server on port 3000.

import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { helpPictures } from "./vite-plugins/help-pictures.ts";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");

export default defineNuxtConfig({
  compatibilityDate: "2026-11-01",
  ssr: false,
  // one window, one page: the tabs are state, not routes (no vue-router)
  pages: false,
  // explicit imports everywhere, as the framework-free modules have them
  imports: { autoImport: false },
  components: false,
  devtools: { enabled: false },
  telemetry: false,
  alias: {
    // "@" and "~" are app/ (Nuxt's srcDir), as "@" was src/renderer/src in the Electron renderer
    "@design": resolve(repo, "design/build"),
    // the documentation the website builds (site/src/content/docs): the help page bundles it
    "@site": resolve(repo, "site/src"),
  },
  css: [
    "@fontsource-variable/geist",
    "@fontsource-variable/geist-mono",
    "@fontsource-variable/noto-sans-sc",
    resolve(repo, "design/build/tokens.css"),
    "~/styles/app.css",
    "~/styles/help.css",
    "~/styles/browser.css",
  ],
  app: {
    head: {
      title: "SpaceKnife",
      htmlAttrs: { lang: "zh-CN" },
      meta: [
        { charset: "utf-8" },
        // the page's background before the CSS arrives follows the system (then tokens.css, lib/theme.ts)
        { name: "color-scheme", content: "light dark" },
      ],
      // the theme chosen in the settings, before the styles apply (lib/theme.ts keeps it)
      script: [{ src: "/theme-boot.js", tagPosition: "head" }],
    },
  },
  spaLoadingTemplate: false,
  devServer: { port: 3000 },
  vite: {
    // Tauri prints its own output; keep the dev server's on screen
    clearScreen: false,
    envPrefix: ["VITE_", "TAURI_ENV_"],
    server: { strictPort: true, fs: { allow: [repo] } },
    // the help pages' pictures, with their sizes (only the ones the pages name)
    plugins: [helpPictures(resolve(repo, "site"))],
  },
  typescript: {
    strict: true,
    typeCheck: false,
    tsConfig: {
      compilerOptions: {
        noUnusedLocals: true,
        noUnusedParameters: true,
        noFallthroughCasesInSwitch: true,
        noUncheckedIndexedAccess: false,
      },
      include: ["../app/env.d.ts"],
    },
    nodeTsConfig: {
      include: ["../vitest.config.ts", "../vite-plugins/**/*.ts"],
    },
  },
});
