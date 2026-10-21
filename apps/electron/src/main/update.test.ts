import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { type FileProbe, type InstallKind, installKind, installRefusal, projectPage } from "./update";

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
  const winDownloads = "C:\\Users\\lenny\\Downloads";
  const winFile = (name: string) => `${winDownloads}\\${name}`;

  it("accepts each install kind's release file in the download folder", () => {
    const cases: [InstallKind, NodeJS.Platform, string, string][] = [
      ["nsis", "win32", winDownloads, "SpaceKnife-0.4.0-Setup-x64.exe"],
      ["nsis", "win32", winDownloads, "SpaceKnife-1.0.0-beta.2-Setup-x64.exe"],
      ["dmg", "darwin", "/Users/lenny/Downloads", "SpaceKnife-0.4.0-mac-arm64.dmg"],
      ["dmg", "darwin", "/Users/lenny/Downloads", "SpaceKnife-0.4.0-mac-x64.dmg"],
      ["deb", "linux", "/home/lenny/Downloads", "spaceknife_0.4.0_amd64.deb"],
      ["rpm", "linux", "/home/lenny/Downloads", "spaceknife-0.4.0.x86_64.rpm"],
      // every part a number of any length
      ["nsis", "win32", winDownloads, "SpaceKnife-0.3.10-Setup-x64.exe"],
      ["nsis", "win32", winDownloads, "SpaceKnife-0.10.100-Setup-x64.exe"],
      ["nsis", "win32", winDownloads, "SpaceKnife-12.34.567-Setup-x64.exe"],
      ["dmg", "darwin", "/Users/lenny/Downloads", "SpaceKnife-0.3.10-mac-arm64.dmg"],
      ["deb", "linux", "/home/lenny/Downloads", "spaceknife_0.3.10_amd64.deb"],
      ["rpm", "linux", "/home/lenny/Downloads", "spaceknife-0.3.10.x86_64.rpm"],
      // a pre-release as the Linux packages may spell it
      ["deb", "linux", "/home/lenny/Downloads", "spaceknife_0.4.0~beta.1_amd64.deb"],
      ["rpm", "linux", "/home/lenny/Downloads", "spaceknife-0.4.0_beta.1.x86_64.rpm"],
    ];
    for (const [kind, platform, folder, name] of cases) {
      const file = platform === "win32" ? `${folder}\\${name}` : `${folder}/${name}`;
      expect(installRefusal(file, kind, folder, platform, fakeFiles([file], [folder])), name).toBeNull();
    }
  });

  it("has no install step for portable copies, AppImages and development runs", () => {
    for (const kind of ["portable", "appimage", "dev", "unknown"] as const) {
      const file = "/home/lenny/Downloads/SpaceKnife-0.4.0-x86_64.AppImage";
      expect(installRefusal(file, kind, "/home/lenny/Downloads", "linux", fakeFiles([file], ["/home/lenny/Downloads"]))).toBe("updateNoInstall");
    }
  });

  it("refuses another kind's file, other names and other folders", () => {
    const files = fakeFiles(
      [winFile("SpaceKnife-0.4.0-mac-arm64.dmg"), winFile("setup.exe"), winFile("SpaceKnife-0.4.0-Setup-x64.exe.bat"), "C:\\Temp\\SpaceKnife-0.4.0-Setup-x64.exe"],
      [winDownloads, winFile("SpaceKnife-0.5.0-Setup-x64.exe"), "C:\\Temp"],
    );
    const refusal = (file: string) => installRefusal(file, "nsis", winDownloads, "win32", files);
    expect(refusal(winFile("SpaceKnife-0.4.0-mac-arm64.dmg"))).toBe("updateNotAnUpdate");
    expect(refusal(winFile("setup.exe"))).toBe("updateNotAnUpdate");
    expect(refusal(winFile("SpaceKnife-0.4.0-Setup-x64.exe.bat"))).toBe("updateNotAnUpdate");
    expect(refusal("C:\\Temp\\SpaceKnife-0.4.0-Setup-x64.exe")).toBe("updateNotInDownloads");
    expect(refusal(winFile("SpaceKnife-0.5.0-Setup-x64.exe"))).toBe("updateMissing"); // a folder
    expect(refusal(winFile("SpaceKnife-0.6.0-Setup-x64.exe"))).toBe("updateMissing");
    expect(installRefusal(winFile("x.exe"), "nsis", "C:\\Users\\lenny\\Gone", "win32", files)).toBe("updateMissing");
  });

  it("judges the file a link leads to, and ignores case where the file system does", () => {
    const lnk = "/home/lenny/Downloads/spaceknife_0.4.0_amd64.deb";
    const linked: FileProbe = {
      realpath: (path) => (path === lnk ? "/tmp/spaceknife_0.4.0_amd64.deb" : path),
      isFile: () => true,
    };
    expect(installRefusal(lnk, "deb", "/home/lenny/Downloads", "linux", linked)).toBe("updateNotInDownloads");
    const file = "C:\\Users\\lenny\\downloads\\SpaceKnife-0.4.0-Setup-x64.exe";
    expect(installRefusal(file, "nsis", winDownloads, "win32", fakeFiles([file], [winDownloads]))).toBeNull();
    const deb = "/home/lenny/downloads/spaceknife_0.4.0_amd64.deb";
    expect(installRefusal(deb, "deb", "/home/lenny/Downloads", "linux", fakeFiles([deb], ["/home/lenny/Downloads"]))).toBe("updateNotInDownloads");
  });

  describe("on this machine's file system", () => {
    const dirs: string[] = [];
    afterEach(() => {
      for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
    });

    it("finds the file and its folder", () => {
      const downloads = mkdtempSync(join(tmpdir(), "sk-update-"));
      dirs.push(downloads);
      const name = "spaceknife_0.4.0_amd64.deb";
      writeFileSync(join(downloads, name), "");
      mkdirSync(join(downloads, "sub"));
      writeFileSync(join(downloads, "sub", name), "");
      const platform = process.platform;
      expect(installRefusal(join(downloads, name), "deb", downloads, platform)).toBeNull();
      expect(installRefusal(join(downloads, "sub", "..", name), "deb", downloads, platform)).toBeNull();
      expect(installRefusal(join(downloads, "sub", name), "deb", downloads, platform)).toBe("updateNotInDownloads");
      expect(installRefusal(join(downloads, "spaceknife_0.5.0_amd64.deb"), "deb", downloads, platform)).toBe("updateMissing");
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
