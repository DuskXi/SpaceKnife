// The panel SDK the app ships (resources/plugin-sdk): the files the scheme serves, the guard's work.
// (That base.css starts with the theme the bridge sends is the renderer's bridge.test.ts.)

import { readdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { SDK_FILES } from "./plugin-files";

const SDK = resolve(dirname(fileURLToPath(import.meta.url)), "../../resources/plugin-sdk");

describe("the panel SDK", () => {
  it("ships exactly the files the scheme serves", () => {
    expect(new Set(readdirSync(SDK))).toEqual(SDK_FILES);
  });

  it("guard.js takes WebRTC away and refuses frames", () => {
    const js = readFileSync(resolve(SDK, "guard.js"), "utf8");
    for (const name of ["RTCPeerConnection", "webkitRTCPeerConnection", "RTCDataChannel", "contentWindow", "createElement", "MutationObserver"]) expect(js).toContain(name);
  });
});
