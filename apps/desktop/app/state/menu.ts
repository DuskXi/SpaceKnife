// The context menu (components/shell/ContextMenu.vue) lives in the store as `menu`: closing it is
// clearing that, from the menu itself or from anything that takes the view away.

import { set } from "@/state/store";

export const closeMenu = (): void => set({ menu: null });
