// The interface's one Pinia (state/pinia.ts), shared with code outside components (state/store.ts).

import { defineNuxtPlugin } from "#app";

import { pinia } from "@/state/pinia";

export default defineNuxtPlugin({
  name: "sk-pinia",
  setup(nuxtApp) {
    nuxtApp.vueApp.use(pinia);
  },
});
