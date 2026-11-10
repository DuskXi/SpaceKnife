import { afterEach, describe, expect, it, vi } from "vitest";

import type { ViewState } from "@/protocol/generated";

import { setLang } from "./i18n";
import { colorLabel, crumbLabel, fullPath, SPACE_SWITCHES, spaceSwitches } from "./toolbar";

afterEach(() => {
  setLang("zh-CN");
  vi.unstubAllGlobals();
});

describe("toolbar words", () => {
  it("names colour modes in the current language", () => {
    expect(colorLabel("class")).toBe("类别");
    setLang("en");
    expect(colorLabel("class")).not.toBe("类别");
  });

  it("shows the recycle bin switch only when the scan holds one, named for the system", () => {
    expect(spaceSwitches({ has_trash: false }).map((s) => s.id)).toEqual(["free", "unknown"]);
    expect(spaceSwitches({ has_trash: true }).map((s) => s.id)).toEqual(["free", "unknown", "trash"]);
    const bin = SPACE_SWITCHES[2]!;
    vi.stubGlobal("window", { sk: { platform: "darwin" } });
    expect(bin.label()).toBe("废纸篓");
    vi.stubGlobal("window", { sk: { platform: "win32" } });
    expect(bin.label()).toBe("回收站");
    const st = { show_free: true, show_unknown: false, show_trash: false } as ViewState;
    expect(SPACE_SWITCHES[0]!.flip(st)).toEqual({ show_free: false });
    expect(SPACE_SWITCHES[1]!.on(st)).toBe(false);
  });

  it("labels breadcrumb segments", () => {
    expect(crumbLabel({ key: 1, label: "small:12345" }, 2, false, "")).toBe("12,345 个小项");
    expect(crumbLabel({ key: 1, label: "more:7" }, 2, false, "")).toBe("+7 项");
    expect(crumbLabel({ key: 1, label: "C:\\" }, 0, true, "系统 (C:)")).toBe("系统 (C:)");
    expect(crumbLabel({ key: 1, label: "C:\\" }, 0, false, "x")).toBe("C:");
    expect(crumbLabel({ key: 2, label: "Users" }, 1, true, "x")).toBe("Users");
  });

  it("joins the breadcrumb into the path to edit", () => {
    expect(fullPath([], "fallback", "win32")).toBe("fallback");
    expect(fullPath([{ key: 1, label: "C:\\" }], "", "win32")).toBe("C:\\");
    expect(
      fullPath(
        [
          { key: 1, label: "C:\\" },
          { key: 2, label: "Users" },
          { key: 3, label: "small:4" },
        ],
        "",
        "win32",
      ),
    ).toBe("C:\\Users");
    expect(
      fullPath(
        [
          { key: 1, label: "/" },
          { key: 2, label: "home" },
        ],
        "",
        "linux",
      ),
    ).toBe("/home");
  });
});
