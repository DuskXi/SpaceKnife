// @vitest-environment happy-dom
// The custom load limit's slider keeps where the user put it while progress events (a new `io`
// each, about four a second) bring the same limit; a new limit from the backend moves it.

import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it } from "vitest";
import { nextTick } from "vue";

import type { IoStatus, ScanStatus } from "@/protocol/generated";
import { resetState } from "@/state/store";

import ScanDetails from "./ScanDetails.vue";

const io = (limit: number): IoStatus =>
  ({
    active_pct: 10,
    auto: true,
    concurrency: 4,
    disk: "d0",
    duty: 0.5,
    history: [],
    inflight: 1,
    level: "custom",
    limit,
    low_io_priority: false,
    measured: true,
    media: "ssd",
    others_pct: 0,
    paused: false,
    q_eff: null,
    read_bps: null,
    read_latency_ms: null,
    self_pct: 5,
    total_pct: 10,
    yielding: false,
  }) as IoStatus;

const scan = (limit: number): ScanStatus =>
  ({ scan_id: "s1", state: "scanning", phase: "scanning", method: "hard", mft_available: false, paused: false, errors: 0, denied: 0, started_at: 0, expected_items: null, roots: [], io: io(limit) }) as unknown as ScanStatus;

beforeEach(() => {
  resetState();
  window.sk = { platform: "win32" } as unknown as typeof window.sk;
});

describe("ScanDetails", () => {
  it("keeps the custom limit where it is dragged while progress events bring the same limit", async () => {
    const w = mount(ScanDetails, { props: { scan: scan(40) } });
    const slider = w.find('input[type="range"]');
    expect((slider.element as HTMLInputElement).value).toBe("40");
    await slider.setValue("70"); // being dragged
    for (let i = 0; i < 3; i++) await w.setProps({ scan: scan(40) }); // progress, the same limit
    await nextTick();
    expect((slider.element as HTMLInputElement).value).toBe("70");
    await w.setProps({ scan: scan(55) }); // the backend took another limit
    await nextTick();
    expect((slider.element as HTMLInputElement).value).toBe("55");
    w.unmount();
  });
});
