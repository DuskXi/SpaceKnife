import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "electron-vite";
import license from "rollup-plugin-license";

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
      },
    },
    server: { port: 5173, strictPort: true, fs: { allow: [resolve(here, "../..")] } },
    build: { rollupOptions: { input: { index: resolve(here, "src/renderer/index.html") } } },
    plugins: [
      react(),
      // the licenses of everything bundled into the renderer (shipped with the installers, M8)
      license({ thirdParty: { includePrivate: false, output: { file: resolve(here, "out/renderer/THIRD-PARTY-LICENSES.txt") } } }),
    ],
  },
});
