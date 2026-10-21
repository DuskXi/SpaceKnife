import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { spawn, spawnSync } from "node:child_process";

import { type FileProbe, type InstallKind, installKind, installRefusal, nsisUpdateArgs, projectPage, restartMode, swapLauncher, swapScript, swapScriptFile } from "./update";

const none = () => false;
const only = (...paths: string[]) => (path: string) => paths.includes(path);

describe("installKind", () => {
  it("is dev when not packaged and portable with a data folder, on every platform", () => {
    expect(installKind("C:\\dev\\electron.exe", "win32", false, {}, null)).toBe("dev");
    expect(installKind("/home/lenny/SpaceKnife/node_modules/electron/dist/electron", "linux", false, { APPIMAGE: "/x" }, null)).toBe("dev");
    expect(installKind("/Applications/SpaceKnife.app/Contents/MacOS/SpaceKnife", "darwin", false, {}, null)).toBe("dev");
    expect(installKind("D:\\SpaceKnife\\SpaceKnife.exe", "win32", true, {}, "D:\\SpaceKnife\\data")).toBe("portable");
    expect(installKind("/home/lenny/SpaceKnife/spaceknife", "linux", true, {}, "/home/lenny/SpaceKnife/data")).toBe("portable");
  });

  it("knows the Windows installer by its uninstaller next to the executable", () => {
    const exe = "C:\\Users\\lenny\\AppData\\Local\\Programs\\SpaceKnife\\SpaceKnife.exe";
    const uninstaller = "C:\\Users\\lenny\\AppData\\Local\\Programs\\SpaceKnife\\Uninstall SpaceKnife.exe";
    expect(installKind(exe, "win32", true, {}, null, only(uninstaller))).toBe("nsis");
    expect(installKind(exe, "win32", true, {}, null, none)).toBe("unknown");
  });

  it("tells the deb from the rpm by dpkg's file list", () => {
    const exe = "/opt/SpaceKnife/spaceknife";
    expect(installKind(exe, "linux", true, {}, null, only("/var/lib/dpkg/info/spaceknife.list"))).toBe("deb");
    expect(installKind(exe, "linux", true, {}, null, none)).toBe("rpm");
    expect(installKind("/opt/SpaceKnifeOld/spaceknife", "linux", true, {}, null, none)).toBe("unknown");
  });

  it("knows an AppImage by its environment, not one inherited by a package or another program", () => {
    const env = { APPIMAGE: "/home/lenny/Apps/SpaceKnife-0.3.0-x86_64.AppImage", APPDIR: "/tmp/.mount_SpaceKabc123" };
    expect(installKind("/tmp/.mount_SpaceKabc123/spaceknife", "linux", true, env, null, none)).toBe("appimage");
    expect(installKind("/tmp/.mount_SpaceKabc123/spaceknife", "linux", true, { APPIMAGE: env.APPIMAGE }, null, none)).toBe("appimage");
    expect(installKind("/opt/SpaceKnife/spaceknife", "linux", true, env, null, none)).toBe("rpm");
    expect(installKind("/home/lenny/build/linux-unpacked/spaceknife", "linux", true, env, null, none)).toBe("unknown");
    expect(installKind("/home/lenny/build/linux-unpacked/spaceknife", "linux", true, {}, null, none)).toBe("unknown");
  });

  it("is a disk image on macOS", () => {
    expect(installKind("/Applications/SpaceKnife.app/Contents/MacOS/SpaceKnife", "darwin", true, {}, null, none)).toBe("dmg");
  });
});

/** A file system of the given files (and folders), without links. */
function fakeFiles(files: string[], folders: string[]): FileProbe {
  return {
    realpath(path) {
      if (!files.includes(path) && !folders.includes(path)) throw new Error(`ENOENT: ${path}`);
      return path;
    },
    isFile: (path) => files.includes(path),
  };
}

describe("installRefusal", () => {
  const winUpdates = "C:\\Users\\lenny\\AppData\\Local\\SpaceKnife\\updates";
  const winFile = (name: string) => `${winUpdates}\\${name}`;

  it("accepts each install kind's release file in the updates folder", () => {
    const cases: [InstallKind, NodeJS.Platform, string, string][] = [
      ["nsis", "win32", winUpdates, "SpaceKnife-0.4.0-Setup-x64.exe"],
      ["nsis", "win32", winUpdates, "SpaceKnife-1.0.0-beta.2-Setup-x64.exe"],
      ["dmg", "darwin", "/Users/lenny/Library/Application Support/SpaceKnife/updates", "SpaceKnife-0.4.0-mac-arm64.dmg"],
      ["dmg", "darwin", "/Users/lenny/Library/Application Support/SpaceKnife/updates", "SpaceKnife-0.4.0-mac-x64.dmg"],
      ["deb", "linux", "/home/lenny/.local/share/SpaceKnife/updates", "spaceknife_0.4.0_amd64.deb"],
      ["rpm", "linux", "/home/lenny/.local/share/SpaceKnife/updates", "spaceknife-0.4.0.x86_64.rpm"],
      // every part a number of any length
      ["nsis", "win32", winUpdates, "SpaceKnife-0.3.10-Setup-x64.exe"],
      ["nsis", "win32", winUpdates, "SpaceKnife-0.10.100-Setup-x64.exe"],
      ["nsis", "win32", winUpdates, "SpaceKnife-12.34.567-Setup-x64.exe"],
      ["dmg", "darwin", "/Users/lenny/Library/Application Support/SpaceKnife/updates", "SpaceKnife-0.3.10-mac-arm64.dmg"],
      ["deb", "linux", "/home/lenny/.local/share/SpaceKnife/updates", "spaceknife_0.3.10_amd64.deb"],
      ["rpm", "linux", "/home/lenny/.local/share/SpaceKnife/updates", "spaceknife-0.3.10.x86_64.rpm"],
      // a pre-release as the Linux packages may spell it
      ["deb", "linux", "/home/lenny/.local/share/SpaceKnife/updates", "spaceknife_0.4.0~beta.1_amd64.deb"],
      ["rpm", "linux", "/home/lenny/.local/share/SpaceKnife/updates", "spaceknife-0.4.0_beta.1.x86_64.rpm"],
    ];
    for (const [kind, platform, folder, name] of cases) {
      const file = platform === "win32" ? `${folder}\\${name}` : `${folder}/${name}`;
      expect(installRefusal(file, kind, folder, platform, fakeFiles([file], [folder])), name).toBeNull();
    }
  });

  it("takes a portable archive of this platform and an AppImage; a development run has no install step", () => {
    const linux = "/home/lenny/.local/share/SpaceKnife/updates";
    const cases: [InstallKind, NodeJS.Platform, string, string][] = [
      ["portable", "win32", winUpdates, "SpaceKnife-0.4.0-win-x64-portable.zip"],
      ["portable", "linux", linux, "SpaceKnife-0.4.0-linux-x64-portable.tar.gz"],
      ["portable", "linux", linux, "SpaceKnife-0.4.0-beta.1-linux-x64-portable.tar.gz"],
      ["appimage", "linux", linux, "SpaceKnife-0.4.0-x86_64.AppImage"],
    ];
    for (const [kind, platform, folder, name] of cases) {
      const file = platform === "win32" ? `${folder}\\${name}` : `${folder}/${name}`;
      expect(installRefusal(file, kind, folder, platform, fakeFiles([file], [folder])), name).toBeNull();
    }
    const zip = `${linux}/SpaceKnife-0.4.0-win-x64-portable.zip`; // the other platform's archive
    expect(installRefusal(zip, "portable", linux, "linux", fakeFiles([zip], [linux]))).toBe("updateNotAnUpdate");
    for (const kind of ["dev", "unknown"] as const) {
      const file = `${linux}/SpaceKnife-0.4.0-x86_64.AppImage`;
      expect(installRefusal(file, kind, linux, "linux", fakeFiles([file], [linux]))).toBe("updateNoInstall");
    }
  });

  it("refuses another kind's file, other names and other folders", () => {
    const files = fakeFiles(
      [winFile("SpaceKnife-0.4.0-mac-arm64.dmg"), winFile("setup.exe"), winFile("SpaceKnife-0.4.0-Setup-x64.exe.bat"), "C:\\Temp\\SpaceKnife-0.4.0-Setup-x64.exe"],
      [winUpdates, winFile("SpaceKnife-0.5.0-Setup-x64.exe"), "C:\\Temp"],
    );
    const refusal = (file: string) => installRefusal(file, "nsis", winUpdates, "win32", files);
    expect(refusal(winFile("SpaceKnife-0.4.0-mac-arm64.dmg"))).toBe("updateNotAnUpdate");
    expect(refusal(winFile("setup.exe"))).toBe("updateNotAnUpdate");
    expect(refusal(winFile("SpaceKnife-0.4.0-Setup-x64.exe.bat"))).toBe("updateNotAnUpdate");
    expect(refusal("C:\\Temp\\SpaceKnife-0.4.0-Setup-x64.exe")).toBe("updateNotInUpdates");
    expect(refusal(winFile("SpaceKnife-0.5.0-Setup-x64.exe"))).toBe("updateMissing"); // a folder
    expect(refusal(winFile("SpaceKnife-0.6.0-Setup-x64.exe"))).toBe("updateMissing");
    expect(installRefusal(winFile("x.exe"), "nsis", "C:\\Users\\lenny\\Gone", "win32", files)).toBe("updateMissing");
  });

  it("judges the file a link leads to, and ignores case where the file system does", () => {
    const lnk = "/home/lenny/.local/share/SpaceKnife/updates/spaceknife_0.4.0_amd64.deb";
    const linked: FileProbe = {
      realpath: (path) => (path === lnk ? "/tmp/spaceknife_0.4.0_amd64.deb" : path),
      isFile: () => true,
    };
    expect(installRefusal(lnk, "deb", "/home/lenny/.local/share/SpaceKnife/updates", "linux", linked)).toBe("updateNotInUpdates");
    const file = "C:\\Users\\lenny\\appdata\\local\\spaceknife\\updates\\SpaceKnife-0.4.0-Setup-x64.exe";
    expect(installRefusal(file, "nsis", winUpdates, "win32", fakeFiles([file], [winUpdates]))).toBeNull();
    const deb = "/home/lenny/.local/share/spaceknife/updates/spaceknife_0.4.0_amd64.deb";
    expect(installRefusal(deb, "deb", "/home/lenny/.local/share/SpaceKnife/updates", "linux", fakeFiles([deb], ["/home/lenny/.local/share/SpaceKnife/updates"]))).toBe("updateNotInUpdates");
  });

  describe("on this machine's file system", () => {
    const dirs: string[] = [];
    afterEach(() => {
      for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
    });

    it("finds the file and its folder", () => {
      const updates = mkdtempSync(join(tmpdir(), "sk-update-"));
      dirs.push(updates);
      const name = "spaceknife_0.4.0_amd64.deb";
      writeFileSync(join(updates, name), "");
      mkdirSync(join(updates, "sub"));
      writeFileSync(join(updates, "sub", name), "");
      const platform = process.platform;
      expect(installRefusal(join(updates, name), "deb", updates, platform)).toBeNull();
      expect(installRefusal(join(updates, "sub", "..", name), "deb", updates, platform)).toBeNull();
      expect(installRefusal(join(updates, "sub", name), "deb", updates, platform)).toBe("updateNotInUpdates");
      expect(installRefusal(join(updates, "spaceknife_0.5.0_amd64.deb"), "deb", updates, platform)).toBe("updateMissing");
    });
  });
});

describe("projectPage", () => {
  it("opens the project's pages on GitHub", () => {
    const page = "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.4.0";
    expect(projectPage(page)).toBe(page);
    expect(projectPage("https://GitHub.com/DuskXi/SpaceKnife/releases")).toBe("https://github.com/DuskXi/SpaceKnife/releases");
    // the repository itself, as the documentation links to it
    expect(projectPage("https://github.com/DuskXi/SpaceKnife")).toBe("https://github.com/DuskXi/SpaceKnife");
    expect(projectPage("https://github.com/DuskXi/SpaceKnife#readme")).toBe("https://github.com/DuskXi/SpaceKnife#readme");
  });

  it("opens the website's pages (the help page's links, 「在网站上查看」)", () => {
    expect(projectPage("https://spaceknife.kaltsit.dev/zh/docs/filtering/#大小")).toBe("https://spaceknife.kaltsit.dev/zh/docs/filtering/#%E5%A4%A7%E5%B0%8F");
    expect(projectPage("https://SpaceKnife.Kaltsit.dev")).toBe("https://spaceknife.kaltsit.dev/");
    expect(projectPage("https://spaceknife.kaltsit.dev/docs/../")).toBe("https://spaceknife.kaltsit.dev/");
  });

  it("refuses everything else", () => {
    for (const url of [
      "https://github.com/DuskXi/SpaceKnife?tab=evil",
      "https://github.com/DuskXi/SpaceKnife/../../evil/repo",
      "https://github.com/DuskXi/SpaceKnifeEvil/releases",
      "https://github.com.evil.example/DuskXi/SpaceKnife/",
      "https://user@github.com/DuskXi/SpaceKnife/releases",
      "http://github.com/DuskXi/SpaceKnife/releases",
      "http://spaceknife.kaltsit.dev/docs/",
      "https://spaceknife.kaltsit.dev.evil.example/docs/",
      "https://evil.example/?https://spaceknife.kaltsit.dev/",
      "https://user@spaceknife.kaltsit.dev/docs/",
      "https://spaceknife.kaltsit.dev:8443/docs/",
      "file:///C:/Windows/System32/calc.exe",
      "not a url",
      42,
      undefined,
    ]) {
      expect(projectPage(url), String(url)).toBeNull();
    }
  });
});

describe("restartMode", () => {
  it("runs the installer, replaces in place, opens the package, or has nothing to do", () => {
    expect(restartMode("nsis")).toBe("installer");
    expect([restartMode("portable"), restartMode("appimage")]).toEqual(["replace", "replace"]);
    expect([restartMode("dmg"), restartMode("deb"), restartMode("rpm")]).toEqual(["open", "open", "open"]);
    expect([restartMode("dev"), restartMode("unknown")]).toEqual(["none", "none"]);
  });
});

describe("nsisUpdateArgs", () => {
  const env = {
    ProgramFiles: "C:\\Program Files",
    "ProgramFiles(x86)": "C:\\Program Files (x86)",
    LOCALAPPDATA: "C:\\Users\\lenny\\AppData\\Local",
  };
  it("updates silently, keeps the data and starts the new version", () => {
    expect(nsisUpdateArgs("C:\\Users\\lenny\\AppData\\Local\\Programs\\SpaceKnife\\SpaceKnife.exe", env)).toEqual([
      "--updated",
      "/S",
      "--force-run",
      "/currentuser",
    ]);
  });
  it("names the scope where the folder makes it plain, else leaves it to the installer", () => {
    expect(nsisUpdateArgs("C:\\Program Files\\SpaceKnife\\SpaceKnife.exe", env).at(-1)).toBe("/allusers");
    expect(nsisUpdateArgs("c:\\program files (x86)\\SpaceKnife\\SpaceKnife.exe", env).at(-1)).toBe("/allusers");
    expect(nsisUpdateArgs("D:\\Apps\\SpaceKnife\\SpaceKnife.exe", env)).toEqual(["--updated", "/S", "--force-run"]);
    expect(nsisUpdateArgs("C:\\Program Files Other\\SpaceKnife\\SpaceKnife.exe", env)).toHaveLength(3);
  });
});

describe("swapScript", () => {
  const plan = {
    pid: 4321,
    appDir: "C:\\Users\\lenny\\Apps\\Space Knife's",
    staged: "C:\\x\\staging-0.4.0\\SpaceKnife-0.4.0-win-x64-portable",
    previous: "C:\\x\\previous-0.3.2",
    exe: "C:\\Users\\lenny\\Apps\\Space Knife's\\SpaceKnife.exe",
    log: "C:\\x\\update-0.4.0.log",
  };
  it("quotes every path for its shell and waits for the app", () => {
    const ps = swapScript("win32", plan);
    expect(ps).toContain("$App = 'C:\\Users\\lenny\\Apps\\Space Knife''s'");
    expect(ps).toContain("Wait-Process -Id 4321");
    expect(ps).toContain("Where-Object { $_.Name -ne 'data' }");
    const sh = swapScript("linux", { ...plan, appDir: "/home/lenny/apps/it's", exe: "/home/lenny/apps/it's/spaceknife" });
    expect(sh.startsWith("#!/bin/sh\n")).toBe(true);
    expect(sh).toContain("APP='/home/lenny/apps/it'\\''s'");
    expect(sh).toContain("kill -0 4321");
  });

  // run the script for real on folders of this machine (the app it starts again is a missing file:
  // nothing is shown, and the swap is done before that last step)
  const shell = process.platform === "win32" ? "win32" : process.platform === "linux" ? "linux" : null;
  const dirs: string[] = [];
  afterEach(() => {
    for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
  });

  function layout(withStaged: boolean) {
    const root = mkdtempSync(join(tmpdir(), "sk-swap-"));
    dirs.push(root);
    const app = join(root, "app");
    const updates = join(app, "data", "data", "updates");
    mkdirSync(join(app, "resources"), { recursive: true });
    mkdirSync(updates, { recursive: true });
    writeFileSync(join(app, "SpaceKnife.exe"), "old");
    writeFileSync(join(app, "resources", "old.txt"), "old");
    writeFileSync(join(app, "data", "data", "snapshot.sks"), "mine");
    const staged = join(updates, "staging-0.4.0", "SpaceKnife-0.4.0-portable");
    if (withStaged) {
      mkdirSync(join(staged, "resources"), { recursive: true });
      mkdirSync(join(staged, "data"), { recursive: true });
      writeFileSync(join(staged, "SpaceKnife.exe"), "new");
      writeFileSync(join(staged, "resources", "new.txt"), "new");
      writeFileSync(join(staged, "data", "README.txt"), "the archive's note");
    }
    const p = {
      pid: 2147483000, // not running: the swap starts at once
      appDir: app,
      staged,
      previous: join(updates, "previous-0.3.2"),
      exe: join(root, "missing", "SpaceKnife.exe"),
      log: join(updates, "update-0.4.0.log"),
    };
    const file = join(updates, shell === "win32" ? "update-0.4.0.ps1" : "update-0.4.0.sh");
    writeFileSync(file, swapScript(shell!, p));
    const args = shell === "win32" ? ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", file] : [file];
    spawnSync(shell === "win32" ? "powershell.exe" : "/bin/sh", args, { windowsHide: true, timeout: 60_000 });
    return { app, updates, p };
  }

  it.runIf(shell)("puts the new files in place and keeps data", () => {
    const { app, p } = layout(true);
    expect(readFileSync(join(app, "SpaceKnife.exe"), "utf8")).toBe("new");
    expect(existsSync(join(app, "resources", "new.txt")) && !existsSync(join(app, "resources", "old.txt"))).toBe(true);
    expect(readFileSync(join(app, "data", "data", "snapshot.sks"), "utf8")).toBe("mine");
    expect(existsSync(join(app, "data", "README.txt"))).toBe(false); // the archive's data folder stays out
    expect(readFileSync(join(p.previous, "SpaceKnife.exe"), "utf8")).toBe("old"); // until the backend cleans
    expect(readFileSync(p.log, "utf8")).toContain("updated");
  }, 60_000);

  it.runIf(shell)("keeps the old version when the new files can't be moved in", () => {
    const { app, p } = layout(false);
    expect(readFileSync(join(app, "SpaceKnife.exe"), "utf8")).toBe("old");
    expect(readdirSync(join(app, "resources"))).toEqual(["old.txt"]);
    expect(readFileSync(p.log, "utf8")).toContain("kept the old version");
  }, 60_000);
});

describe("starting the swap", () => {
  it("writes a BOM on Windows only (PowerShell 5.1 reads UTF-8 without one as ANSI)", () => {
    expect([...swapScriptFile("win32", "x").subarray(0, 3)]).toEqual([0xef, 0xbb, 0xbf]);
    expect(swapScriptFile("linux", "x").toString("utf8")).toBe("x");
  });

  it("starts the Windows swap through a hidden PowerShell that it waits for", () => {
    const how = swapLauncher("win32", "C:\\Users\\lenny\\Space Knife's\\update-0.4.0.ps1");
    expect(how.cmd).toBe("powershell.exe");
    expect(how.wait).toBe(true);
    const command = how.args[how.args.indexOf("-Command") + 1]!;
    expect(command).toContain("Start-Process -FilePath powershell.exe -WindowStyle Hidden");
    expect(command).toContain(`-File "C:\\Users\\lenny\\Space Knife''s\\update-0.4.0.ps1"'`);
    expect(swapLauncher("linux", "/tmp/x.sh")).toEqual({ cmd: "/bin/sh", args: ["/tmp/x.sh"], wait: false });
  });

  // for real: the script must still run once the starter (and the test's own process, here) is gone
  it.runIf(process.platform === "win32")("runs the script after the starter is done, from a folder with Chinese in its name", async () => {
    const root = mkdtempSync(join(tmpdir(), "sk-launch-"));
    try {
      const dir = join(root, "软件 it's");
      mkdirSync(dir, { recursive: true });
      const log = join(dir, "ran.log");
      const script = join(dir, "update-0.4.0.ps1");
      writeFileSync(script, swapScriptFile("win32", `Start-Sleep -Milliseconds 500\r\nAdd-Content -LiteralPath '${log.replace(/'/g, "''")}' -Value 'ran'\r\n`));
      const how = swapLauncher("win32", script);
      const code = await new Promise<number | null>((done) => spawn(how.cmd, how.args, { stdio: "ignore", windowsHide: true }).once("exit", done));
      expect(code).toBe(0);
      for (let i = 0; i < 100 && !existsSync(log); i++) await new Promise((r) => setTimeout(r, 200));
      expect(readFileSync(log, "utf8")).toContain("ran");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, 60_000);
});
