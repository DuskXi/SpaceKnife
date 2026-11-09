// electron-builder.yml as electron-builder itself reads it (its own config loader and publish
// resolution, app-builder-lib, pinned with electron-builder): no publish target and so no update
// information (latest.yml, app-update.yml), whatever tokens the build's environment holds. The
// update check is SpaceKnife's own (crates/sk-core update), as in 0.3.4. Without `publish: null`,
// a GH_TOKEN in the environment (the CI job's, for `gh release download`) makes electron-builder
// pick GitHub as the publish provider, fail to find the repository ("Cannot detect repository by
// .git/config") and stop in its update information ("Cannot read properties of null (reading
// 'channel')"): the installers were not built.

import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

const shellDir = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const require = createRequire(import.meta.url);
const lib = dirname(require.resolve("app-builder-lib/package.json", { paths: [dirname(require.resolve("electron-builder/package.json"))] }));
const { getConfig, validateConfiguration } = require(join(lib, "out", "util", "config", "config.js")) as {
  getConfig(projectDir: string, configPath: string | null, configFromOptions: object | null): Promise<Record<string, unknown>>;
  validateConfiguration(config: object, debugLogger: { isEnabled: boolean; add(key: string, value: unknown): void }): Promise<void>;
};
const { getPublishConfigs } = require(join(lib, "out", "publish", "PublishManager.js")) as {
  getPublishConfigs(packager: object, targetOptions: object | null, arch: null, errorIfCannot: boolean): Promise<unknown[] | null>;
};

const TOKENS = ["GH_TOKEN", "GITHUB_TOKEN"] as const;
const saved = Object.fromEntries(TOKENS.map((k) => [k, process.env[k]]));
afterEach(() => {
  for (const k of TOKENS) {
    if (saved[k] === undefined) delete process.env[k];
    else process.env[k] = saved[k];
  }
});

describe("electron-builder.yml", () => {
  it("publishes nothing and writes no update information, with a GitHub token around too", async () => {
    const config = await getConfig(shellDir, null, null);
    expect(config.publish).toBeNull();
    // and electron-builder's own schema takes the whole file
    await validateConfiguration(structuredClone(config), { isEnabled: false, add: () => {} });
    for (const token of [null, ...TOKENS]) {
      for (const k of TOKENS) delete process.env[k];
      if (token) process.env[token] = "a-token-of-the-ci-job";
      // the targets electron-builder writes update information for: the NSIS installer, the AppImage;
      // and the rpm, which takes the same path
      for (const [platform, target] of [["win", "nsis"], ["linux", "appImage"], ["linux", "rpm"]] as const) {
        const packager = { platformSpecificBuildOptions: config[platform] ?? {}, config, info: {} };
        const found = await getPublishConfigs(packager, (config[target] as object | undefined) ?? null, null, false).catch((e: Error) => e.message);
        expect(found, `${platform}/${target} with ${token ?? "no token"}`).toBeNull();
      }
    }
  });
});
