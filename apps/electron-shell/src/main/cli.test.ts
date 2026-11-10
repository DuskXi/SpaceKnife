// 0.3.4's headless launch forms reach spaceknife-cli (plan §4 gap 1), which reads them itself. The
// same decision is spaceknife-cli's `wants_headless`, the Tauri shell's src-tauri/src/cli.rs and
// SpaceKnife.com's `route` (packaging/console-launcher).

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { STARTUP_CHECK, headlessArgs, isStartupCheck, startupCheckAnswer } from "./cli";

describe("the startup check", () => {
  it("is the flag the swap helper asks with (crates/sk-core/src/update/swap.rs STARTUP_CHECK)", () => {
    const swap = readFileSync(fileURLToPath(new URL("../../../../crates/sk-core/src/update/swap.rs", import.meta.url)), "utf8");
    expect(swap).toContain(`pub const STARTUP_CHECK: &str = "${STARTUP_CHECK}";`);
  });

  it("is asked by the flag, in a checkout after the app's folder too, and is not headless mode", () => {
    expect(isStartupCheck(["--startup-check"])).toBe(true);
    expect(isStartupCheck([".", "--startup-check"])).toBe(true);
    expect(isStartupCheck([])).toBe(false);
    expect(isStartupCheck(["--updated"])).toBe(false);
    expect(isStartupCheck(["--startup-checks"])).toBe(false);
    expect(headlessArgs(["--startup-check"])).toBeNull();
  });

  it("answers with one line the helper reads as ok", () => {
    const line = startupCheckAnswer({ electron: "44.9.3" });
    expect(line.endsWith("\n")).toBe(true);
    expect(line.trim().includes("\n")).toBe(false);
    expect(JSON.parse(line)).toEqual({ ok: true, electron: "44.9.3" });
  });
});

describe("headlessArgs", () => {
  it("opens the window without a headless option", () => {
    expect(headlessArgs([])).toBeNull();
    expect(headlessArgs(["."])).toBeNull();
    expect(headlessArgs(["--updated"])).toBeNull();
    expect(headlessArgs(["--no-sandbox", "-psn_0_12345", "--disable-gpu"])).toBeNull();
    expect(headlessArgs(["--listener"])).toBeNull();
  });

  it("takes headless mode anywhere among the arguments and passes them on as given", () => {
    expect(headlessArgs(["--headless"])).toEqual(["--headless"]);
    expect(headlessArgs([".", "--no-sandbox", "--headless"])).toEqual([".", "--no-sandbox", "--headless"]);
    expect(headlessArgs(["--listen", "0.0.0.0:8375", "--yes"])).toEqual(["--listen", "0.0.0.0:8375", "--yes"]);
    expect(headlessArgs(["--yes", "--headless", "--listen=:9000"])).toEqual(["--yes", "--headless", "--listen=:9000"]);
    expect(headlessArgs(["--headless", "--tls-cert", "/etc/sk/cert.pem", "--tls-key=/etc/sk/key.pem", "--ui", "C:\\sk ui\\public"])).toEqual([
      "--headless",
      "--tls-cert",
      "/etc/sk/cert.pem",
      "--tls-key=/etc/sk/key.pem",
      "--ui",
      "C:\\sk ui\\public",
    ]);
  });

  // v0.3.4 apps/electron/src/main/headless.ts parseCommandLine: "headlessOnly", not the window
  it("sends headless options without --headless / --listen to spaceknife-cli, which refuses them as 0.3.4 did", () => {
    expect(headlessArgs(["--yes"])).toEqual(["--yes"]);
    expect(headlessArgs(["--tls-cert", "c.pem"])).toEqual(["--tls-cert", "c.pem"]);
    expect(headlessArgs(["--tls-key=k.pem"])).toEqual(["--tls-key=k.pem"]);
  });

  it("shows headless mode's help for --help, as 0.3.4 did", () => {
    expect(headlessArgs(["--help"])).toEqual(["--headless", "--help"]);
    expect(headlessArgs(["--no-sandbox", "-h"])).toEqual(["--headless", "--no-sandbox", "-h"]);
    expect(headlessArgs(["--headless", "--help"])).toEqual(["--headless", "--help"]);
  });
});
