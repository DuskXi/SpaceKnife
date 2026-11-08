// 0.3.4's headless launch forms reach spaceknife-cli headless (plan §4 gap 1). The same cases are in
// the Tauri shell's src-tauri/src/cli.rs.

import { describe, expect, it } from "vitest";

import { headlessArgs } from "./cli";

describe("headlessArgs", () => {
  it("opens the window without a headless option", () => {
    expect(headlessArgs([])).toBeNull();
    expect(headlessArgs(["."])).toBeNull();
    expect(headlessArgs(["--updated"])).toBeNull();
    expect(headlessArgs(["--no-sandbox", "-psn_0_12345", "--disable-gpu"])).toBeNull();
    // only with --headless or --listen, as in 0.3.4
    expect(headlessArgs(["--yes"])).toBeNull();
  });

  it("takes --headless and --listen anywhere among the arguments", () => {
    expect(headlessArgs(["--headless"])).toEqual(["headless"]);
    expect(headlessArgs([".", "--no-sandbox", "--headless"])).toEqual(["headless"]);
    expect(headlessArgs(["--listen", "0.0.0.0:8375", "--yes"])).toEqual(["headless", "--listen=0.0.0.0:8375", "--yes"]);
    expect(headlessArgs(["--yes", "--headless", "--listen=:9000"])).toEqual(["headless", "--yes", "--listen=:9000"]);
    expect(headlessArgs(["--no-sandbox", "--listen", "[::]:8375", "--headless"])).toEqual(["headless", "--listen=[::]:8375"]);
  });

  it("passes the TLS files and the interface folder, and nothing else", () => {
    expect(headlessArgs(["--headless", "--tls-cert", "/etc/sk/cert.pem", "--tls-key=/etc/sk/key.pem", "--frobnicate", "x"])).toEqual([
      "headless",
      "--tls-cert=/etc/sk/cert.pem",
      "--tls-key=/etc/sk/key.pem",
    ]);
    expect(headlessArgs(["--listen", "8375", "--ui", "C:\\sk ui\\public"])).toEqual(["headless", "--listen=8375", "--ui=C:\\sk ui\\public"]);
  });

  it("leaves a missing value missing, for spaceknife-cli to say so", () => {
    expect(headlessArgs(["--listen"])).toEqual(["headless", "--listen"]);
    expect(headlessArgs(["--listen", "--yes"])).toEqual(["headless", "--listen", "--yes"]);
    expect(headlessArgs(["--headless", "--tls-cert"])).toEqual(["headless", "--tls-cert"]);
  });

  it("shows headless mode's help for --help, as 0.3.4 did", () => {
    expect(headlessArgs(["--help"])).toEqual(["headless", "--help"]);
    expect(headlessArgs(["-h"])).toEqual(["headless", "--help"]);
    expect(headlessArgs(["--headless", "--help"])).toEqual(["headless", "--help"]);
  });
});
