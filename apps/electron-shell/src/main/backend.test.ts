import { posix, resolve, win32 } from "node:path";

import { describe, expect, it } from "vitest";

import { backendArgs, backendEnv, cliPath, takeReadyFile } from "./backend";

describe("the ready file (SPACEKNIFE_READY_FILE)", () => {
  it("is taken out of the environment once, as a full path", () => {
    const env: NodeJS.ProcessEnv = { PATH: "/usr/bin", SPACEKNIFE_READY_FILE: "ready.json" };
    expect(takeReadyFile(env)).toBe(resolve("ready.json"));
    expect(env).toEqual({ PATH: "/usr/bin" });
    expect(takeReadyFile(env)).toBeUndefined();
    expect(takeReadyFile({ SPACEKNIFE_READY_FILE: "" })).toBeUndefined();
  });

  it("never reaches the backend's environment (it gets --ready-file, once)", () => {
    const env = backendEnv({}, { PATH: "/usr/bin", SPACEKNIFE_READY_FILE: "/tmp/spaceknife-ready-0/ready.json" });
    expect(env).toEqual({ PATH: "/usr/bin", SPACEKNIFE_SHELL: "electron" });
  });
});

describe("the backend the Electron shell starts", () => {
  it("is spaceknife-cli serve with the token on stdin, as 0.3.4's backend was", () => {
    expect(backendArgs([])).toEqual(["serve", "--token-stdin", "--log-format", "plain"]);
    expect(backendArgs(["http://localhost:3000"])).toEqual(["serve", "--token-stdin", "--log-format", "plain", "--allow-origin", "http://localhost:3000"]);
    // the ready file of the end-to-end tests (SPACEKNIFE_READY_FILE), as the Tauri shell writes it
    expect(backendArgs([], "ready.json")).toEqual(["serve", "--token-stdin", "--log-format", "plain", "--ready-file", resolve("ready.json")]);
  });

  it("is told how the app runs in its environment, this shell included", () => {
    const env = backendEnv(
      { home: "data", locale: "zh-CN", appVersion: "0.3.5", installKind: "nsis" },
      { PATH: "/usr/bin", SPACEKNIFE_SHELL: "tauri" },
    );
    expect(env).toEqual({
      PATH: "/usr/bin",
      SPACEKNIFE_SHELL: "electron",
      SPACEKNIFE_HOME: resolve("data"),
      SPACEKNIFE_SYSTEM_LOCALE: "zh-CN",
      SPACEKNIFE_APP_VERSION: "0.3.5",
      SPACEKNIFE_INSTALL_KIND: "nsis",
    });
    // nothing it isn't told: the backend's own defaults
    expect(backendEnv({}, {})).toEqual({ SPACEKNIFE_SHELL: "electron" });
  });

  it("is the spaceknife-cli next to an installed app's executable", () => {
    expect(cliPath("C:\\Program Files\\SpaceKnife\\SpaceKnife.exe", "win32", true, {}, "C:\\repo")).toBe("C:\\Program Files\\SpaceKnife\\spaceknife-cli.exe");
    expect(cliPath("/opt/SpaceKnife/spaceknife", "linux", true, {}, "/repo")).toBe("/opt/SpaceKnife/spaceknife-cli");
    expect(cliPath("/tmp/.mount_SpaceKabc/spaceknife", "linux", true, {}, "/repo")).toBe("/tmp/.mount_SpaceKabc/spaceknife-cli");
  });

  it("is cargo's build in a checkout, the release one first; SPACEKNIFE_CLI wins", () => {
    const release = posix.join("/repo", "target", "release", "spaceknife-cli");
    const debug = posix.join("/repo", "target", "debug", "spaceknife-cli");
    expect(cliPath("/x/electron", "linux", false, {}, "/repo", (p) => p === release || p === debug)).toBe(release);
    expect(cliPath("/x/electron", "linux", false, {}, "/repo", (p) => p === debug)).toBe(debug);
    expect(cliPath("/x/electron", "linux", false, {}, "/repo", () => false)).toBe(release);
    expect(cliPath("/x/electron", "linux", false, { CARGO_TARGET_DIR: "/t" }, "/repo", () => false)).toBe("/t/release/spaceknife-cli");
    expect(cliPath("C:\\x\\electron.exe", "win32", false, {}, "C:\\repo", () => false)).toBe(win32.join("C:\\repo", "target", "release", "spaceknife-cli.exe"));
    expect(cliPath("/x/electron", "linux", true, { SPACEKNIFE_CLI: "/bin/sk" }, "/repo")).toBe("/bin/sk");
  });
});
