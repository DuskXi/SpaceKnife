import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { effectScope, nextTick, ref } from "vue";

import { useThrottled } from "./useThrottled";

describe("useThrottled", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("passes at most one change per interval, the latest one last", async () => {
    const scope = effectScope();
    const value = ref(0);
    const shown = scope.run(() => useThrottled(value, 1000))!;
    expect(shown.value).toBe(0);
    value.value = 1;
    await nextTick();
    expect(shown.value).toBe(0); // within the first interval
    value.value = 2;
    await nextTick();
    vi.advanceTimersByTime(1000);
    expect(shown.value).toBe(2);
    vi.advanceTimersByTime(5000);
    value.value = 3;
    await nextTick();
    expect(shown.value).toBe(3); // the interval passed: at once
    scope.stop();
  });

  it("passes every change with ms <= 0", async () => {
    const scope = effectScope();
    const value = ref("a");
    const shown = scope.run(() => useThrottled(value, 0))!;
    value.value = "b";
    await nextTick();
    expect(shown.value).toBe("b");
    scope.stop();
  });
});
