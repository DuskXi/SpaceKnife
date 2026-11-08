import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "electron-vite";
import license from "rollup-plugin-license";

import { helpPictures } from "./vite-plugins/help-pictures";

const here = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  main: {
    build: { rollupOptions: { input: { index: resolve(here, "src/main/index.ts") } } },
  },
  preload: {
    build: { rollupOptions: { input: { index: resolve(here, "src/preload/index.ts") } } },
  },
  renderer: {
    root: resolve(here, "src/renderer"),
    resolve: {
      alias: {
        "@": resolve(here, "src/renderer/src"),
        "@design": resolve(here, "../../design/build"),
        // the documentation the website builds (site/src/content/docs): the help page bundles it
        "@site": resolve(here, "../../site/src"),
      },
    },
    server: { port: 5173, strictPort: true, fs: { allow: [resolve(here, "../..")] } },
    build: { rollupOptions: { input: { index: resolve(here, "src/renderer/index.html") } } },
    plugins: [
      react(),
      // the help pages' pictures, with their sizes (only the ones the pages name)
      helpPictures(resolve(here, "../../site")),
      // the licenses of everything bundled into the renderer (shipped with the installers, M8)
      license({ thirdParty: { includePrivate: false, output: { file: resolve(here, "out/renderer/THIRD-PARTY-LICENSES.txt") } } }),
    ],
  },
});
