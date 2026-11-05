// The one Pinia instance of the interface. Kept in a module of its own so the Nuxt plugin
// (plugins/pinia.ts) can install it without loading the store, and so state modules and unit tests
// reach the store without an app (state/store.ts binds every call to this instance).

import { createPinia } from "pinia";

export const pinia = createPinia();
