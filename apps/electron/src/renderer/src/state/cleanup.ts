// The duplicate-file cleanup plan (docs/dupes.md §16): filled in by the cleanup mode.

import { client } from "./actions";

/** The backend offers cleanup plans (capability dupes.cleanup.v1). */
export function cleanupSupported(): boolean {
  try {
    return client().hello?.capabilities.includes("dupes.cleanup.v1") ?? false;
  } catch {
    return false;
  }
}

export async function openCleanup(_tabId: string, _jobId: string): Promise<void> {}

export async function openHistory(_tabId: string): Promise<void> {}
