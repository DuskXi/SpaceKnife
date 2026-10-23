import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  fmtAgo,
  fmtDateTime,
  fmtDelta,
  fmtDuration,
  fmtInt,
  fmtKeys,
  fmtPct,
  fmtShare,
  fmtSize,
  distinctPaths,
  fitPath,
  middlePath,
  setUnitBase,
  sizeParts,
} from "./format";

const KiB = 1024;
const MiB = KiB ** 2;
const GiB = KiB ** 3;
const TiB = KiB ** 4;
const PiB = KiB ** 5;

function stubPlatform(platform: "win32" | "darwin" | "linux"): void {
  vi.stubGlobal("window", { sk: { platform } });
}

afterEach(() => {
  setUnitBase(1024);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("sizeParts / fmtSize (base 1024)", () => {
  beforeEach(() => setUnitBase(1024));

  it("keeps bytes whole and unit-less below one KB", () => {
    expect(sizeParts(0)).toEqual(["0", "B"]);
    expect(sizeParts(1)).toEqual(["1", "B"]);
    expect(sizeParts(1023)).toEqual(["1023", "B"]);
    expect(fmtSize(0)).toBe("0 B");
    expect(fmtSize(1023)).toBe("1023 B");
  });

  it("switches unit exactly at the base", () => {
    expect(sizeParts(1024)).toEqual(["1.0", "KB"]);
    expect(fmtSize(KiB)).toBe("1.0 KB");
    expect(fmtSize(MiB)).toBe("1.0 MB");
    expect(fmtSize(GiB)).toBe("1.0 GB");
    expect(fmtSize(TiB)).toBe("1.0 TB");
    expect(fmtSize(PiB)).toBe("1.0 PB");
  });

  it("keeps one decimal for scaled sizes", () => {
    expect(fmtSize(1536)).toBe("1.5 KB");
    expect(fmtSize(63.6 * GiB)).toBe("63.6 GB");
    expect(fmtSize(961 * GiB)).toBe("961.0 GB");
    expect(fmtSize(1023 * KiB)).toBe("1023.0 KB");
  });

  it("drops the decimal for near-whole values only when asked", () => {
    expect(fmtSize(961 * GiB, true)).toBe("961 GB");
    expect(sizeParts(961 * GiB, true)).toEqual(["961", "GB"]);
    expect(fmtSize(1.04 * GiB, true)).toBe("1 GB");
    expect(fmtSize(1.96 * GiB, true)).toBe("2 GB");
    expect(fmtSize(63.6 * GiB, true)).toBe("63.6 GB");
    expect(fmtSize(1.5 * TiB, true)).toBe("1.5 TB");
    // the flag never affects plain bytes
    expect(fmtSize(512, true)).toBe("512 B");
  });

  it("caps the unit at PB for huge values", () => {
    expect(fmtSize(1024 * PiB)).toBe("1024.0 PB");
    expect(fmtSize(2 ** 70)).toBe("1048576.0 PB");
  });

  it("accepts bigint input", () => {
    expect(fmtSize(0n)).toBe("0 B");
    expect(fmtSize(5n * 1024n ** 4n)).toBe("5.0 TB");
    expect(fmtSize(2n ** 63n - 1n)).toBe("8192.0 PB");
  });

  it("scales negative values by magnitude", () => {
    expect(sizeParts(-5)).toEqual(["-5", "B"]);
    expect(fmtSize(-2048)).toBe("-2.0 KB");
  });

  // Unit is chosen before rounding, so a value just below the next unit prints as "1024.0 KB"
  // instead of "1.0 MB" (UI-MAIN-59: pick the unit automatically, keep one decimal).
  it("rolls over to the next unit when rounding reaches the base", () => {
    expect(fmtSize(MiB - 1)).toBe("1.0 MB");
  });
});

describe("sizeParts / fmtSize (base 1000)", () => {
  beforeEach(() => setUnitBase(1000));

  it("uses decimal units", () => {
    expect(fmtSize(999)).toBe("999 B");
    expect(fmtSize(1000)).toBe("1.0 KB");
    expect(fmtSize(1024)).toBe("1.0 KB");
    expect(fmtSize(1_500_000)).toBe("1.5 MB");
    expect(fmtSize(961e9, true)).toBe("961 GB");
    expect(fmtSize(2e12)).toBe("2.0 TB");
    expect(fmtSize(3e15)).toBe("3.0 PB");
    expect(fmtSize(5e18)).toBe("5000.0 PB");
  });

  it("can be switched back to 1024", () => {
    expect(fmtSize(1000)).toBe("1.0 KB");
    setUnitBase(1024);
    expect(fmtSize(1000)).toBe("1000 B");
  });
});

describe("default unit base", () => {
  it("follows the platform at load time: 1000 on macOS, 1024 elsewhere", async () => {
    vi.resetModules();
    stubPlatform("darwin");
    const mac = await import("./format");
    expect(mac.fmtSize(1000)).toBe("1.0 KB");

    vi.resetModules();
    stubPlatform("linux");
    const linux = await import("./format");
    expect(linux.fmtSize(1000)).toBe("1000 B");

    vi.resetModules();
    vi.unstubAllGlobals();
    const headless = await import("./format");
    expect(headless.fmtSize(1000)).toBe("1000 B");
  });
});

describe("fmtDelta", () => {
  it("signs with + and a true minus (U+2212)", () => {
    expect(fmtDelta(14.8 * GiB)).toBe("+14.8 GB");
    expect(fmtDelta(-5.7 * GiB)).toBe("\u22125.7 GB");
    expect(fmtDelta(-5.7 * GiB)).not.toContain("-");
    expect(fmtDelta(0)).toBe("+0 B");
    expect(fmtDelta(-1)).toBe("\u22121 B");
    expect(fmtDelta(2048)).toBe("+2.0 KB");
  });
});

describe("fmtInt", () => {
  it("adds thousands separators", () => {
    expect(fmtInt(0)).toBe("0");
    expect(fmtInt(999)).toBe("999");
    expect(fmtInt(1000)).toBe("1,000");
    expect(fmtInt(1_284_635)).toBe("1,284,635");
    expect(fmtInt(-1234)).toBe("-1,234");
    expect(fmtInt(2n ** 40n)).toBe("1,099,511,627,776");
  });
});

describe("fmtShare", () => {
  it("keeps one decimal under 10% and rounds from 10% up", () => {
    expect(fmtShare(82, 1000)).toBe("8.2%");
    expect(fmtShare(0.5, 100)).toBe("0.5%");
    expect(fmtShare(0, 100)).toBe("0.0%");
    expect(fmtShare(10, 100)).toBe("10%");
    expect(fmtShare(54, 100)).toBe("54%");
    expect(fmtShare(1, 3)).toBe("33%");
    expect(fmtShare(1, 1)).toBe("100%");
  });

  it("is 0% for an empty whole", () => {
    expect(fmtShare(5, 0)).toBe("0%");
    expect(fmtShare(0, 0)).toBe("0%");
  });

  // 9.96% is under 10%, gets one decimal and then prints as "10.0%" (UI-MAIN-59: >= 10% is whole).
  it("never prints a decimal once the rounded share reaches 10%", () => {
    expect(fmtShare(9.96, 100)).toBe("10%");
  });
});

describe("fmtPct", () => {
  it("is always whole", () => {
    expect(fmtPct(96, 100)).toBe("96%");
    expect(fmtPct(4, 100)).toBe("4%");
    expect(fmtPct(1, 3)).toBe("33%");
    expect(fmtPct(2, 3)).toBe("67%");
    expect(fmtPct(0.4, 100)).toBe("0%");
    expect(fmtPct(82, 1000)).toBe("8%");
  });

  it("is 0% for an empty whole", () => {
    expect(fmtPct(5, 0)).toBe("0%");
  });
});

describe("fmtDuration", () => {
  it("shows seconds with one decimal under a minute", () => {
    expect(fmtDuration(0)).toBe("0.0 秒");
    expect(fmtDuration(12.34)).toBe("12.3 秒");
    expect(fmtDuration(59.9)).toBe("59.9 秒");
  });

  it("shows minutes and seconds under an hour", () => {
    expect(fmtDuration(60)).toBe("1 分 0 秒");
    expect(fmtDuration(125)).toBe("2 分 5 秒");
    expect(fmtDuration(125.4)).toBe("2 分 5 秒");
    expect(fmtDuration(3599)).toBe("59 分 59 秒");
  });

  it("shows hours and minutes from an hour on", () => {
    expect(fmtDuration(3600)).toBe("1 小时 0 分");
    expect(fmtDuration(3725)).toBe("1 小时 2 分");
    expect(fmtDuration(7199)).toBe("1 小时 59 分");
    expect(fmtDuration(26 * 3600 + 5 * 60)).toBe("26 小时 5 分");
  });

  // Seconds are rounded after the minutes were taken, so 119.6 s prints as "1 分 60 秒".
  it("carries rounded seconds into the minute (119.6 s -> 2 分 0 秒)", () => {
    expect(fmtDuration(119.6)).toBe("2 分 0 秒");
  });

  // 59.96 s is under a minute, gets one decimal and prints as "60.0 秒".
  it("switches to the minute form when the seconds round up to 60 (59.96 s)", () => {
    expect(fmtDuration(59.96)).toBe("1 分 0 秒");
  });
});

describe("fmtDateTime", () => {
  it("formats local time as YYYY-MM-DD HH:mm with zero padding", () => {
    const t = (y: number, mo: number, d: number, h: number, mi: number, s = 0) => new Date(y, mo, d, h, mi, s).getTime() / 1000;
    expect(fmtDateTime(t(2024, 0, 2, 3, 4, 59))).toBe("2024-01-02 03:04");
    expect(fmtDateTime(t(2025, 11, 31, 23, 59))).toBe("2025-12-31 23:59");
    expect(fmtDateTime(t(2026, 9, 18, 12, 0))).toBe("2026-10-18 12:00");
  });

  it("is a dash for an unknown time", () => {
    expect(fmtDateTime(0)).toBe("—");
  });
});

describe("fmtAgo", () => {
  const now = 1_760_000_000;
  const M = 60;
  const H = 3600;
  const D = 86400;

  it("is 刚刚 under a minute and for times in the future", () => {
    expect(fmtAgo(now, now)).toBe("刚刚");
    expect(fmtAgo(now - 59, now)).toBe("刚刚");
    expect(fmtAgo(now + 1000, now)).toBe("刚刚");
  });

  it("walks through minutes, hours, days, months and years", () => {
    expect(fmtAgo(now - M, now)).toBe("1 分钟前");
    expect(fmtAgo(now - (H - 1), now)).toBe("59 分钟前");
    expect(fmtAgo(now - H, now)).toBe("1 小时前");
    expect(fmtAgo(now - 2 * H, now)).toBe("2 小时前");
    expect(fmtAgo(now - (D - 1), now)).toBe("23 小时前");
    expect(fmtAgo(now - D, now)).toBe("1 天前");
    expect(fmtAgo(now - 3 * D, now)).toBe("3 天前");
    expect(fmtAgo(now - 29 * D, now)).toBe("29 天前");
    expect(fmtAgo(now - 30 * D, now)).toBe("1 个月前");
    expect(fmtAgo(now - 61 * D, now)).toBe("2 个月前");
    expect(fmtAgo(now - 364 * D, now)).toBe("12 个月前");
    expect(fmtAgo(now - 365 * D, now)).toBe("1 年前");
    expect(fmtAgo(now - 800 * D, now)).toBe("2 年前");
  });

  it("is 未知 for an unknown time", () => {
    expect(fmtAgo(0, now)).toBe("未知");
  });

  it("defaults to the current clock", () => {
    vi.spyOn(Date, "now").mockReturnValue(now * 1000);
    expect(fmtAgo(now - 2 * H)).toBe("2 小时前");
    expect(fmtAgo(now - 3 * D)).toBe("3 天前");
  });
});

describe("fmtKeys", () => {
  it("keeps neutral notation on Windows and Linux, with arrow glyphs", () => {
    for (const platform of ["win32", "linux"] as const) {
      stubPlatform(platform);
      expect(fmtKeys("Ctrl+Shift+F5")).toBe("Ctrl+Shift+F5");
      expect(fmtKeys("Alt+Left")).toBe("Alt+←");
      expect(fmtKeys("Alt+Right")).toBe("Alt+→");
      expect(fmtKeys("Alt+Up")).toBe("Alt+↑");
      expect(fmtKeys("Down")).toBe("↓");
      expect(fmtKeys("Ctrl+Home")).toBe("Ctrl+Home");
      expect(fmtKeys("F2")).toBe("F2");
      expect(fmtKeys("Ctrl+=")).toBe("Ctrl+=");
    }
  });

  it("uses macOS modifier glyphs without separators on macOS", () => {
    stubPlatform("darwin");
    expect(fmtKeys("Ctrl+Shift+F5")).toBe("⌘⇧F5");
    expect(fmtKeys("Alt+Left")).toBe("⌥←");
    expect(fmtKeys("Ctrl+Up")).toBe("⌘↑");
    expect(fmtKeys("Shift+F5")).toBe("⇧F5");
    expect(fmtKeys("Ctrl+-")).toBe("⌘-");
    expect(fmtKeys("Ctrl+,")).toBe("⌘,");
    expect(fmtKeys("Enter")).toBe("Enter");
  });
});

describe("middlePath", () => {
  it("leaves short paths alone", () => {
    expect(middlePath("C:\\Users\\lenny")).toBe("C:\\Users\\lenny");
    expect(middlePath("/home/lenny")).toBe("/home/lenny");
    expect(middlePath("")).toBe("");
    const exact = "/" + "a".repeat(51);
    expect(exact).toHaveLength(52);
    expect(middlePath(exact)).toBe(exact);
  });

  it("keeps the drive and the last two folders of a long Windows path", () => {
    const p = "C:\\Users\\lenny\\AppData\\Local\\Packages\\CanonicalGroupLimited\\LocalState\\wsl\\disk";
    expect(p.length).toBeGreaterThan(52);
    expect(middlePath(p)).toBe("C:\\…\\wsl\\disk");
  });

  it("keeps the first folder and the last two of a long POSIX path", () => {
    const p = "/home/lenny/projects/spaceknife/packages/backend/src/spaceknife/server";
    expect(p.length).toBeGreaterThan(52);
    expect(middlePath(p)).toBe("/home/…/spaceknife/server");
  });

  it("keeps a trailing separator as an empty last part", () => {
    const p = "/home/lenny/projects/spaceknife/packages/backend/src/spaceknife/";
    expect(middlePath(p)).toBe("/home/…/spaceknife/");
  });

  it("leaves long paths with few parts alone", () => {
    const p = "C:\\" + "x".repeat(60) + "\\y";
    expect(middlePath(p)).toBe(p);
    const q = "/" + "x".repeat(60) + "/y/z";
    expect(middlePath(q)).toBe(q);
  });

  it("honours a custom limit", () => {
    expect(middlePath("/a/b/c/d/e", 5)).toBe("/a/…/d/e");
    expect(middlePath("/a/b/c/d/e", 10)).toBe("/a/b/c/d/e");
    expect(middlePath("C:\\a\\b\\c\\d", 5)).toBe("C:\\…\\c\\d");
  });

  // A UNC path splits into ["", "", "server", "share", ...], so the head becomes a lone "\" and the
  // server and share names are dropped: "\\…\c\d" instead of keeping the root.
  it("keeps the server of a long UNC path", () => {
    const p = "\\\\fileserver\\share\\projects\\spaceknife\\packages\\backend\\src\\server";
    expect(p.length).toBeGreaterThan(52);
    expect(middlePath(p).startsWith("\\\\fileserver")).toBe(true);
  });
});

describe("fitPath", () => {
  it("fills the budget from the right and always keeps the name", () => {
    const p = "C:\\Users\\leizi\\Downloads\\Photos\\2021\\a.jpg";
    expect(fitPath(p, 100)).toBe(p);
    expect(fitPath(p, 24)).toBe("C:\\…\\Photos\\2021\\a.jpg");
    expect(fitPath(p, 5)).toBe("C:\\…\\a.jpg");
    expect(fitPath("/home/x/a/b/c.txt", 15)).toBe("/home/…/b/c.txt");
  });
});

describe("distinctPaths", () => {
  it("keeps the segment where copies differ and folds what they share", () => {
    const a = "C:\\Users\\leizi\\Downloads\\Projects\\site\\assets\\video\\intro.mp4";
    const b = "C:\\Users\\leizi\\Downloads\\Projects\\site-old\\assets\\video\\intro.mp4";
    expect(distinctPaths([a, b], 40)).toEqual(["C:\\…\\site\\…\\intro.mp4", "C:\\…\\site-old\\…\\intro.mp4"]);
  });

  it("tells twenty logo.png files apart by their client folder", () => {
    const paths = Array.from({ length: 20 }, (_, i) => `C:\\work\\clients\\client-${String(i).padStart(2, "0")}\\assets\\logo.png`);
    const out = distinctPaths(paths, 30);
    expect(new Set(out).size).toBe(20);
    expect(out[3]).toBe("C:\\…\\client-03\\…\\logo.png");
  });

  it("leaves short paths whole and cuts a long difference from the left", () => {
    expect(distinctPaths(["C:\\a\\x.bin", "C:\\b\\x.bin"], 40)).toEqual(["C:\\a\\x.bin", "C:\\b\\x.bin"]);
    const out = distinctPaths(["D:\\one\\two\\three\\four\\five\\x.bin", "D:\\other\\x.bin"], 22);
    expect(out[0]!.endsWith("five\\x.bin")).toBe(true);
    expect(out[0]!.length).toBeLessThanOrEqual(22);
  });
});
