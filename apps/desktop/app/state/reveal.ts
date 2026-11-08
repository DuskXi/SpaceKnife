// Locating a block from a list (shell/Inspector in the Electron renderer).

import { viewCall } from "@/state/actions";

/** Locate a block: select it, navigating (with history) until it is drawn (UI-MAIN-40, UI-W3-33). */
export async function reveal(viewId: string, key: number): Promise<void> {
  await viewCall(viewId, "view.reveal", { key });
}
