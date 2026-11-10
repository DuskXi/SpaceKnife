import { afterEach, describe, expect, it } from "vitest";

import type { MonitorStatus, ScanStatus } from "@/protocol/generated";

import { setLang } from "./i18n";
import { LEVEL_CAP, monitorText } from "./statusBar";

afterEach(() => setLang("zh-CN"));

const scan = (patch: Partial<ScanStatus>, monitor?: Partial<MonitorStatus>): ScanStatus =>
  ({ source: "scan", monitor: monitor ? { changes: 0, detail: "", pending: 0, state: "running", ...monitor } : null, ...patch }) as ScanStatus;

describe("monitorText", () => {
  it("says what the monitor will do before a scan", () => {
    expect(monitorText(undefined, true)).toEqual({ text: "实时监控 · 扫描后自动开启", title: "点击关闭实时监控", tone: "muted" });
    expect(monitorText(undefined, false).text).toBe("实时监控已关闭");
  });

  it("never monitors a snapshot", () => {
    expect(monitorText(scan({ source: "snapshot" }, {}), true).text).toBe("快照 · 不监控变化");
  });

  it("counts the changes and colours the state", () => {
    const running = monitorText(scan({}, { changes: 1234, pending: 5 }), true);
    expect(running.tone).toBe("ok-text");
    expect(running.title).toBe("1,234 处变化已更新，5 项待处理。点击关闭实时监控");
    expect(monitorText(scan({}, { state: "failed" }), true)).toMatchObject({ text: "实时监控 · 失败", title: "监视中断。点击关闭实时监控", tone: "warn-text" });
    expect(monitorText(scan({}, { state: "degraded", detail: "x" }), true).title).toBe("x。0 处变化已更新");
    expect(monitorText(scan({}, { state: "starting" }), true).text).toBe("实时监控 · 启动中");
    // switched off, or no hooks
    expect(monitorText(scan({}, {}), false).text).toBe("实时监控已关闭");
    expect(monitorText(scan({}), true).text).toBe("实时监控已关闭");
  });

  it("follows the language", () => {
    setLang("en");
    expect(monitorText(undefined, false).text).not.toBe("实时监控已关闭");
  });

  it("caps each load level", () => {
    expect(LEVEL_CAP.balanced).toBe(70);
    expect(LEVEL_CAP.quiet).toBe(35);
  });
});
