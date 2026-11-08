// Unit tests of the main process's pure modules (src/main/*.test.ts): no Electron, no window.

import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/main/**/*.test.ts"],
  },
});
