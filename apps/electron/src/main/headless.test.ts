import { describe, expect, it } from "vitest";

import {
  DEFAULT_PORT,
  browserUrls,
  confirmed,
  exposure,
  ht,
  langOf,
  listenError,
  parseCommandLine,
  parseListen,
  questionInput,
  showListen,
  warningLines,
} from "./headless";

describe("parseCommandLine", () => {
  it("opens the window without headless options, whatever else is there", () => {
    expect(parseCommandLine([])).toEqual({ mode: "window" });
    expect(parseCommandLine([".", "--updated", "--no-sandbox", "-psn_0_123"])).toEqual({ mode: "window" });
  });

  it("--headless alone listens on this computer only", () => {
    expect(parseCommandLine(["--headless"])).toEqual({ mode: "headless", listen: { host: "127.0.0.1", port: DEFAULT_PORT }, yes: false, tls: null });
  });

  it("--listen starts headless mode by itself, with the value after it or after =", () => {
    const want = { mode: "headless", listen: { host: "0.0.0.0", port: 9000 }, yes: false, tls: null };
    expect(parseCommandLine(["--listen", "0.0.0.0:9000"])).toEqual(want);
    expect(parseCommandLine(["--listen=0.0.0.0:9000"])).toEqual(want);
    expect(parseCommandLine([".", "--headless", "--listen", "0.0.0.0:9000"])).toEqual(want);
  });

  it("takes --yes and the certificate pair", () => {
    expect(parseCommandLine(["--listen", "::", "--yes", "--tls-cert", "c.pem", "--tls-key=k.pem"])).toEqual({
      mode: "headless",
      listen: { host: "::", port: DEFAULT_PORT },
      yes: true,
      tls: { cert: "c.pem", key: "k.pem" },
    });
  });

  it("explains what is wrong", () => {
    expect(parseCommandLine(["--listen"])).toMatchObject({ mode: "error", error: "listenMissing" });
    expect(parseCommandLine(["--listen", "--yes"])).toMatchObject({ mode: "error", error: "listenMissing" });
    expect(parseCommandLine(["--listen", "not an address"])).toMatchObject({ mode: "error", error: "badAddress", value: "not an address" });
    expect(parseCommandLine(["--listen", "0.0.0.0:70000"])).toMatchObject({ mode: "error", error: "badPort" });
    expect(parseCommandLine(["--headless", "--tls-cert", "c.pem"])).toMatchObject({ mode: "error", error: "tlsPair", value: "--tls-key" });
    expect(parseCommandLine(["--headless", "--tls-key"])).toMatchObject({ mode: "error", error: "tlsMissing" });
    expect(parseCommandLine(["--yes"])).toMatchObject({ mode: "error", error: "headlessOnly", value: "--yes" });
    expect(parseCommandLine(["--tls-cert", "c.pem", "--tls-key", "k.pem"])).toMatchObject({ mode: "error", error: "headlessOnly" });
  });

  it("--help wins", () => {
    expect(parseCommandLine(["--listen", "0.0.0.0", "--help"])).toEqual({ mode: "help" });
    expect(parseCommandLine(["-h"])).toEqual({ mode: "help" });
  });
});

describe("parseListen", () => {
  it.each([
    ["0.0.0.0", "0.0.0.0", DEFAULT_PORT],
    ["192.168.1.20:8080", "192.168.1.20", 8080],
    [":8080", "127.0.0.1", 8080],
    ["8080", "127.0.0.1", 8080],
    ["localhost", "localhost", DEFAULT_PORT],
    ["nas.lan.", "nas.lan", DEFAULT_PORT],
    ["::", "::", DEFAULT_PORT],
    ["[::]:9000", "::", 9000],
    ["[fd00::5]", "fd00::5", DEFAULT_PORT],
    ["fe80::1", "fe80::1", DEFAULT_PORT],
    ["0.0.0.0:0", "0.0.0.0", 0],
  ])("%s", (text, host, port) => {
    expect(parseListen(text)).toEqual({ host, port });
  });

  it.each([["host name"], ["[::"], ["[nope]:80"], ["1:2:3"], ["-bad.example"], ["a..b"]])("refuses the address %s", (text) => {
    expect(parseListen(text)).toBe("badAddress");
  });

  it.each([["0.0.0.0:x"], ["0.0.0.0:65536"], ["[::]:"], ["host:"]])("refuses the port of %s", (text) => {
    expect(parseListen(text)).toBe("badPort");
  });
});

describe("exposure", () => {
  it.each([
    ["127.0.0.1", "loopback"],
    ["127.10.0.3", "loopback"],
    ["localhost", "loopback"],
    ["::1", "loopback"],
    ["[::1]", "loopback"],
    ["::ffff:127.0.0.1", "loopback"],
    ["0.0.0.0", "all"],
    ["::", "all"],
    ["0:0:0:0:0:0:0:0", "all"],
    ["192.168.1.20", "network"],
    ["10.0.0.5", "network"],
    ["fd00::5", "network"],
    ["nas.lan", "network"],
  ] as const)("%s → %s", (host, reach) => {
    expect(exposure(host)).toBe(reach);
  });
});

describe("browserUrls", () => {
  const nics = [
    { address: "127.0.0.1", family: "IPv4", internal: true },
    { address: "192.168.1.20", family: "IPv4", internal: false },
    { address: "10.8.0.2", family: 4, internal: false },
    { address: "fe80::1", family: "IPv6", internal: false },
    { address: "fd00::5", family: "IPv6", internal: false },
  ];

  it("names this computer for loopback, the token in the fragment", () => {
    expect(browserUrls({ host: "127.0.0.1", port: 8375 }, 8375, "tok/en", false, nics)).toEqual(["http://localhost:8375/#token=tok%2Fen"]);
  });

  it("lists every reachable address for 0.0.0.0, IPv6 only for ::", () => {
    expect(browserUrls({ host: "0.0.0.0", port: 0 }, 41000, "t", false, nics)).toEqual([
      "http://192.168.1.20:41000/#token=t",
      "http://10.8.0.2:41000/#token=t",
      "http://localhost:41000/#token=t",
    ]);
    expect(browserUrls({ host: "::", port: 8375 }, 8375, "t", true, nics)).toEqual([
      "https://192.168.1.20:8375/#token=t",
      "https://10.8.0.2:8375/#token=t",
      "https://[fd00::5]:8375/#token=t",
      "https://localhost:8375/#token=t",
    ]);
  });

  it("uses the one address listened on", () => {
    expect(browserUrls({ host: "fd00::5", port: 8375 }, 8375, "t", false, nics)).toEqual(["http://[fd00::5]:8375/#token=t"]);
  });
});

describe("the question", () => {
  it("only Y or yes goes on", () => {
    for (const a of ["y", "Y", " yes ", "YES", "Ｙ"]) expect(confirmed(a)).toBe(true);
    for (const a of ["", "n", "N", "no", "yy", "ok", null]) expect(confirmed(a)).toBe(false);
  });

  it("is asked in a terminal, or in the console SpaceKnife.com waits in", () => {
    expect(questionInput("linux", {}, true)).toBe("stdin");
    expect(questionInput("linux", {}, false)).toBeNull();
    expect(questionInput("win32", { SPACEKNIFE_CONSOLE: "1" }, false)).toBe("console");
    expect(questionInput("win32", {}, false)).toBeNull();
    expect(questionInput("darwin", { SPACEKNIFE_CONSOLE: "1" }, false)).toBeNull();
  });
});

describe("texts", () => {
  it("warns about every interface, the user's rights and plain HTTP", () => {
    const lines = warningLines("en", { host: "0.0.0.0", port: 8375 }, "lenny", false);
    expect(lines[0]).toMatch(/DANGER/);
    expect(lines.join("\n")).toContain("0.0.0.0:8375");
    expect(lines.join("\n")).toContain("every network interface");
    expect(lines.join("\n")).toContain("lenny");
    expect(lines.join("\n")).toContain("plain HTTP");
    expect(warningLines("zh-CN", { host: "192.168.1.20", port: 8375 }, "lenny", true).join("\n")).toContain("HTTPS");
  });

  it("brackets IPv6 in addresses", () => {
    expect(showListen({ host: "::", port: 8375 })).toBe("[::]:8375");
  });

  it("explains listening errors by their code", () => {
    const listen = { host: "0.0.0.0", port: 80 };
    const e = (code: string) => Object.assign(new Error(code), { code });
    expect(listenError("en", listen, e("EADDRINUSE"))).toMatch(/in use/);
    expect(listenError("en", listen, e("EADDRNOTAVAIL"))).toMatch(/isn't an address of this computer/);
    expect(listenError("en", listen, e("EACCES"))).toMatch(/permission/);
    expect(listenError("zh-CN", listen, e("EOTHER"))).toContain("0.0.0.0:80");
  });

  it("has every text in both languages with the same placeholders", () => {
    const keys = ["usage", "warnTitle", "warnAll", "warnPower", "question", "noTerminal", "windowsHint", "ready", "connected"] as const;
    for (const key of keys) {
      const zh = ht("zh-CN", key);
      const en = ht("en", key);
      expect(zh).not.toBe(en);
      expect(en).not.toMatch(/[一-鿿]/);
      expect([...zh.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort()).toEqual([...en.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort());
    }
    expect(ht("en", "usage", { port: 8375 })).toContain("127.0.0.1:8375");
  });

  it("picks the language from a system tag", () => {
    expect(langOf("zh-Hans-CN")).toBe("zh-CN");
    expect(langOf("zh_CN.UTF-8")).toBe("zh-CN");
    expect(langOf("en-US")).toBe("en");
    expect(langOf(undefined)).toBe("en");
  });
});
