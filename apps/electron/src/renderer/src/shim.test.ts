import { describe, expect, it, vi } from "vitest";

import { type BrowserEnv, browserApi, fetchInfo, linkToken, socketUrl } from "./shim";

describe("the headless link", () => {
  it("takes the token from the fragment, else the one the tab kept", () => {
    expect(linkToken("#token=abc", null)).toBe("abc");
    expect(linkToken("#token=abc", "old")).toBe("abc");
    expect(linkToken("", "kept")).toBe("kept");
    expect(linkToken("#other=1", null)).toBeNull();
    expect(linkToken("#token=a%2Fb", null)).toBe("a/b");
  });

  it("reaches the backend through the page's own origin", () => {
    expect(socketUrl({ protocol: "http:", host: "192.168.1.20:8375" })).toBe("ws://192.168.1.20:8375/v1");
    expect(socketUrl({ protocol: "https:", host: "[fd00::5]:8375" })).toBe("wss://[fd00::5]:8375/v1");
  });

  it("asks /sk/info with the token", async () => {
    const info = { platform: "linux", version: "0.3.3", hostname: "nas", home: "/home/lenny", secure: false };
    const ok = vi.fn(async () => new Response(JSON.stringify(info), { status: 200 }));
    expect(await fetchInfo(ok as unknown as typeof fetch, "tok")).toEqual(info);
    expect(ok).toHaveBeenCalledWith("/sk/info", expect.objectContaining({ headers: { authorization: "Bearer tok" } }));
    const denied = vi.fn(async () => new Response("", { status: 401 }));
    expect(await fetchInfo(denied as unknown as typeof fetch, "tok")).toBe("unauthorized");
    const broken = vi.fn(async () => new Response("", { status: 500 }));
    await expect(fetchInfo(broken as unknown as typeof fetch, "tok")).rejects.toThrow("HTTP 500");
  });
});

function env(answer: string | null = null): BrowserEnv & { notices: string[]; copied: string[]; asked: unknown[]; opened: string[] } {
  const e = {
    notices: [] as string[],
    copied: [] as string[],
    asked: [] as unknown[],
    opened: [] as string[],
    notice: (text: string) => void e.notices.push(text),
    copy: (text: string) => void e.copied.push(text),
    open: (url: string) => void e.opened.push(url),
    askPath: async (req: unknown) => (e.asked.push(req), answer),
    flash: () => {},
  };
  return e;
}

const remote = { hostname: "nas", home: "/home/lenny/", secure: false };
const ready = { state: "ready", info: { url: "ws://nas:8375/v1", token: "t", pid: 0 } } as const;

describe("browserApi", () => {
  it("is a browser tab on the server's platform", () => {
    const sk = browserApi({ state: ready, platform: "darwin", version: "0.3.3", host: "browser", remote }, env());
    expect(sk.host).toBe("browser");
    expect(sk.platform).toBe("darwin");
    expect(sk.remote).toEqual(remote);
    expect(sk.files.pathOf({} as File)).toBe("");
    return expect(sk.backend()).resolves.toEqual(ready);
  });

  it("says what needs the server's desktop and copies the path", async () => {
    const e = env();
    const sk = browserApi({ state: ready, platform: "linux", version: "", host: "browser", remote }, e);
    sk.shell.showItemInFolder("/home/lenny/big.iso");
    expect(await sk.shell.openPath("/home/lenny/a.txt")).toBe("");
    sk.shell.openTerminal("/home/lenny");
    expect(e.copied).toEqual(["/home/lenny/big.iso", "/home/lenny/a.txt", "/home/lenny"]);
    expect(e.notices).toHaveLength(3);
    expect(e.notices[0]).toContain("nas");
    expect(e.notices[0]).toContain("路径已复制");
    expect(await sk.update.install("/x", "seamless")).toContain("nas");
    sk.shell.openExternal("https://github.com/DuskXi/SpaceKnife/releases");
    expect(e.opened).toEqual(["https://github.com/DuskXi/SpaceKnife/releases"]);
  });

  it("asks for paths on the server, starting in its home folder", async () => {
    const e = env("D:\\Data; E:\\  ;");
    const sk = browserApi({ state: ready, platform: "win32", version: "", host: "browser", remote: { ...remote, home: "C:\\Users\\lenny" } }, e);
    expect(await sk.dialog.openFolders()).toEqual(["D:\\Data", "E:\\"]);
    await sk.dialog.saveFile({ title: "导出", filters: [{ name: "CSV", extensions: ["csv"] }], defaultName: "report.csv" });
    await sk.dialog.openFile({ title: "导入", filters: [{ name: "JSON", extensions: ["json", "ini"] }] });
    await sk.dialog.openSnapshot();
    expect(e.asked).toEqual([
      expect.objectContaining({ value: "C:\\Users\\lenny\\" }),
      expect.objectContaining({ title: "导出", value: "C:\\Users\\lenny\\report.csv" }),
      expect.objectContaining({ title: "导入", hint: expect.stringContaining(".json .ini") }),
      expect.objectContaining({ hint: expect.stringContaining(".sks") }),
    ]);
  });

  it("returns nothing for a cancelled dialog", async () => {
    const sk = browserApi({ state: ready, platform: "linux", version: "", host: "browser", remote }, env(null));
    expect(await sk.dialog.openFolders()).toEqual([]);
    expect(await sk.dialog.openSnapshot()).toBeNull();
  });

  it("the demo looks like the app and only logs what it can't do", () => {
    const e = env();
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    const sk = browserApi({ state: ready, platform: "win32", version: "0.3.2-web", host: "app", remote: null }, e);
    sk.shell.showItemInFolder("C:\\x");
    expect(sk.host).toBe("app");
    expect(e.notices).toEqual([]);
    expect(e.copied).toEqual([]);
    expect(info).toHaveBeenCalled();
    info.mockRestore();
  });
});
