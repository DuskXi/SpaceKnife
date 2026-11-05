import { afterEach, describe, expect, it } from "vitest";

import { setLang } from "./i18n";
import { LOG_LEVELS, LOG_SOURCES, logLevelNames, logLevelText, logSourceNames, messageParts, shortPath, sourceText } from "./logPanel";

afterEach(() => setLang("zh-CN"));

describe("log panel texts", () => {
  it("names sources and levels, unknown sources as they are", () => {
    expect(sourceText("perm")).toBe("权限");
    expect(sourceText("plugin")).toBe("plugin");
    expect(logLevelText("WARNING")).toBe("警告");
    setLang("en");
    expect(sourceText("perm")).not.toBe("权限");
  });

  it("lists every name a column can show, in the current language", () => {
    expect(logLevelNames()).toEqual(["信息", "警告", "错误"]);
    expect(logSourceNames()).toHaveLength(LOG_SOURCES.length);
    expect(logSourceNames()).toContain("权限");
    setLang("en");
    expect(logLevelNames()).toHaveLength(LOG_LEVELS.length);
    expect(logSourceNames()).not.toContain("权限");
  });

  it("cuts long paths from the left", () => {
    expect(shortPath("C:\\short")).toBe("C:\\short");
    expect(shortPath("C:\\Users\\someone\\AppData\\Local\\Packages\\Some.Long.Package\\cache")).toBe("…\\Packages\\Some.Long.Package\\cache");
    expect(shortPath("/home/someone/projects/a-very-long-folder-name/sub/file.txt")).toBe("…/a-very-long-folder-name/sub/file.txt");
  });

  it("puts the path of a message in its own part", () => {
    expect(messageParts("plain", "")).toEqual([{ text: "plain", mono: false }]);
    expect(messageParts("no path here", "C:\\x")).toEqual([{ text: "no path here", mono: false }]);
    expect(messageParts("拒绝访问：C:\\x，已跳过", "C:\\x")).toEqual([
      { text: "拒绝访问：", mono: false },
      { text: "C:\\x", mono: true },
      { text: "，已跳过", mono: false },
    ]);
  });
});
