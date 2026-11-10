// @vitest-environment happy-dom
// The update card keeps saying why the last click did not start the update, whatever the
// backend's download is at: the shell restarts the backend then (the files had to be free), and the
// new backend has no finished download until its first check verifies the file again, a minute
// later (seen in the 0.3.5 → next version experiment, packaging/update-next.mjs).

import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { UpdateDownload, UpdateInfo, UpdateStatus } from "@/protocol/generated";
import type { SkApi } from "@/shared/api";
import { setLang } from "@/lib/i18n";
import { resetState, set } from "@/state/store";

import UpdateCard from "./UpdateCard.vue";

const REASON = "无法启动更新：文件夹里还有不是 SpaceKnife 的东西（OtherTool、e2e-notes.txt）";

const latest: UpdateInfo = {
  asset: { name: "SpaceKnife-0.3.6-win-x64-portable.zip", sha256: "ab".repeat(32), size: 36 << 20, url: "https://example.invalid/f.zip" },
  name: "SpaceKnife 0.3.6",
  notes: "",
  prerelease: true,
  published_at: null,
  tag: "v0.3.6",
  url: "https://example.invalid/release",
  version: "0.3.6",
};

function status(download: Partial<UpdateDownload>): UpdateStatus & { latest: UpdateInfo } {
  return {
    channel: "preview",
    checked_at: null,
    checking: false,
    current: "0.3.5",
    dismissed: false,
    download: { state: "idle", version: null, received: 0, total: 0, path: null, verified: null, error: null, ...download },
    enabled: true,
    error: null,
    install_kind: "portable",
    latest,
    level: "patch",
    muted: false,
    next_check_at: null,
    rollback: null,
    series: null,
  };
}

describe("UpdateCard", () => {
  beforeEach(() => {
    resetState();
    window.sk = { platform: "win32", host: "app", shell: { openExternal: vi.fn(), showItemInFolder: vi.fn() } } as unknown as SkApi;
  });
  afterEach(() => setLang("zh-CN"));

  const alerts = (card: ReturnType<typeof mount>) => card.findAll("[role=alert]").map((a) => a.text());

  it("says why the last click did not start the update, with the file ready", () => {
    set({ updateInstallError: REASON });
    const card = mount(UpdateCard, { props: { status: status({ state: "done", version: "0.3.6", path: "C:\\u\\SpaceKnife-0.3.6-win-x64-portable.zip", verified: true }), placement: "anchored" } });
    expect(alerts(card)).toEqual([REASON]);
    expect(card.find(".uc-choice.seamless").exists()).toBe(true);
    card.unmount();
  });

  it("still says it while the restarted backend has no finished download yet", () => {
    set({ updateInstallError: REASON });
    const card = mount(UpdateCard, { props: { status: status({ state: "idle" }), placement: "anchored" } });
    expect(alerts(card)).toEqual([REASON]);
    card.unmount();
  });

  it("and while that backend checks the file again", () => {
    set({ updateInstallError: REASON });
    const card = mount(UpdateCard, { props: { status: status({ state: "verifying", version: "0.3.6", received: 1, total: 1 }), placement: "anchored" } });
    expect(alerts(card)).toEqual([REASON]);
    card.unmount();
  });

  it("says nothing of the kind when there was no such click", () => {
    for (const state of ["idle", "verifying", "done"] as const) {
      const card = mount(UpdateCard, { props: { status: status({ state, version: "0.3.6", path: state === "done" ? "C:\\u\\f.zip" : null, verified: true }), placement: "anchored" } });
      expect(alerts(card)).toEqual([]);
      card.unmount();
    }
  });
});
