// window.sk before anything of the interface loads (lib/installHost.ts). app.vue loads the shell
// asynchronously, after the plugins ran.

import { defineNuxtPlugin } from "#app";

import { installHost } from "@/lib/installHost";

export default defineNuxtPlugin({
  name: "sk-host",
  enforce: "pre",
  async setup() {
    await installHost();
  },
});
