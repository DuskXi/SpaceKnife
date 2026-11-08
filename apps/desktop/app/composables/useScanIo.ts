// The disk-load state of the scan in a tab, if any (useScanIo in shell/ScanProgress of the Electron
// renderer): status bar, scan details.

import { type ComputedRef, type MaybeRefOrGetter, toValue } from "vue";

import type { IoStatus } from "@/protocol/generated";
import { useApp } from "@/state/store";

export function useScanIo(scanId: MaybeRefOrGetter<string | null | undefined>): ComputedRef<IoStatus | null> {
  return useApp((s) => {
    const id = toValue(scanId);
    return id ? (s.scans[id]?.io ?? null) : null;
  });
}
