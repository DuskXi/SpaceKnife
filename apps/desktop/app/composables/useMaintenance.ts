// What 「高级」 and 「标签」 show of the backend's upkeep (system.maintenance): read once when shown,
// and again from the answer of an action that changes it. (useMaintenance in pages/AdvancedTools.tsx.)

import { onMounted, type ShallowRef, shallowRef } from "vue";

import type { MaintenanceInfo } from "@/protocol/generated";
import { RpcError } from "@/protocol/rpc";
import { call } from "@/state/actions";
import { toast } from "@/state/store";

/** An error of an action, as a toast. */
export const fail = (err: unknown): void => toast(err instanceof RpcError ? err.message : String(err));

export function useMaintenance(): [Readonly<ShallowRef<MaintenanceInfo | null>>, (p: Promise<MaintenanceInfo>) => void] {
  const info = shallowRef<MaintenanceInfo | null>(null);
  onMounted(() => {
    void call("system.maintenance", {})
      .then((r) => (info.value = r))
      .catch(() => undefined);
  });
  return [info, (p) => void p.then((r) => (info.value = r)).catch(fail)];
}
