// Pairs a view's binary frames with their JSON FrameInfo (docs/protocol.md §4): either may arrive
// first. A binary frame nobody waits for yet is kept (the latest 32); a waiter gives up after a
// timeout, and a frame of a layout this renderer cannot draw is never kept or waited for.

import type { BinaryFrame } from "@/protocol/rpc";
import { isKnownLayout } from "@/visual/frame";

const KEEP = 32;
const TIMEOUT_MS = 10_000;

/** Key of a frame: its stream and sequence number ("view:<id>#<seq>"). */
export const frameKey = (stream: string, seq: number): string => `${stream}#${seq}`;

export class FramePairer {
  private readonly frames = new Map<string, BinaryFrame>();
  private readonly waiters = new Map<string, { resolve: (f: BinaryFrame | null) => void; timer: ReturnType<typeof setTimeout> }>();

  /** `accepts`: the layouts worth keeping (by default the ones visual/frame.ts decodes). */
  constructor(private readonly accepts: (layout: string) => boolean = isKnownLayout) {}

  /** A binary frame came in: to its waiter, or kept for the JSON that follows. */
  accept(bin: BinaryFrame): void {
    const key = frameKey(bin.header.stream, bin.header.seq);
    if (!this.accepts(bin.header.layout)) {
      this.settle(key, null); // whoever waits for it cannot use it
      return;
    }
    if (this.settle(key, bin)) return;
    this.frames.delete(key);
    this.frames.set(key, bin);
    if (this.frames.size > KEEP) this.frames.delete(this.frames.keys().next().value!);
  }

  /** The binary frame of a key: at once when it is here, else when it comes; null after the timeout. */
  waitFor(key: string, timeoutMs: number = TIMEOUT_MS): Promise<BinaryFrame | null> {
    const got = this.frames.get(key);
    if (got) {
      this.frames.delete(key);
      return Promise.resolve(got);
    }
    this.settle(key, null); // a second wait for the same frame replaces the first
    return new Promise((resolve) => {
      const timer = setTimeout(() => this.settle(key, null), timeoutMs);
      this.waiters.set(key, { resolve, timer });
    });
  }

  /** Forget everything (a new connection numbers its frames again); waiters get null. */
  clear(): void {
    for (const key of [...this.waiters.keys()]) this.settle(key, null);
    this.frames.clear();
  }

  /** Frames kept and waiters pending (tests). */
  get sizes(): { frames: number; waiters: number } {
    return { frames: this.frames.size, waiters: this.waiters.size };
  }

  private settle(key: string, f: BinaryFrame | null): boolean {
    const w = this.waiters.get(key);
    if (!w) return false;
    this.waiters.delete(key);
    clearTimeout(w.timer);
    w.resolve(f);
    return true;
  }
}
