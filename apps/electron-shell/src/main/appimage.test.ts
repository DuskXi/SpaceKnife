import { chmodSync, closeSync, existsSync, mkdtempSync, openSync, readFileSync, readSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  type AppImageFiles,
  appImageEnv,
  carriedSwitches,
  judgeStartup,
  lastLine,
  nextOf,
  nodeFiles,
  placeAppImage,
  previousOf,
  runStartupCheck,
  startupCheck,
  startupCheckArgs,
  startupCheckEnv,
} from "./appimage";

const dirs: string[] = [];
const pids: number[] = [];
afterEach(() => {
  for (const pid of pids.splice(0)) {
    try {
      process.kill(pid);
    } catch {
      // gone
    }
  }
  for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

function temp(): string {
  const dir = mkdtempSync(join(tmpdir(), "sk-appimage-"));
  dirs.push(dir);
  return dir;
}

const alive = (pid: number) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

const until = async (what: () => boolean, ms: number) => {
  const end = Date.now() + ms;
  while (!what() && Date.now() < end) await new Promise((r) => setTimeout(r, 50));
  return what();
};

describe("the environment the new AppImage gets", () => {
  const mount = "/tmp/.mount_SpaceKabc123";
  const running = {
    APPDIR: mount,
    APPIMAGE: "/home/lenny/Apps/SpaceKnife-0.3.5-x86_64.AppImage",
    ARGV0: "./SpaceKnife-0.3.5-x86_64.AppImage",
    OWD: "/home/lenny/Apps",
    // what AppRun put in front of the system's
    PATH: `${mount}:${mount}/usr/sbin:/usr/local/bin:/usr/bin`,
    LD_LIBRARY_PATH: `${mount}/usr/lib:`,
    XDG_DATA_DIRS: `${mount}/usr/share/:./share/:/usr/share/`,
    GSETTINGS_SCHEMA_DIR: `${mount}/usr/share/glib-2.0/schemas:`,
    HOME: "/home/lenny",
    LANG: "zh_CN.UTF-8",
    // another mount whose name only starts the same, a home the user chose
    OTHER: "/tmp/.mount_SpaceKabc1234/usr/lib",
    SPACEKNIFE_HOME: "/home/lenny/sk",
    SPACEKNIFE_READY_FILE: "/tmp/ready.json",
  };

  it("is this process's without what the running AppImage set for itself", () => {
    expect(appImageEnv(running)).toEqual({
      PATH: "/usr/local/bin:/usr/bin",
      XDG_DATA_DIRS: "./share/:/usr/share/",
      HOME: "/home/lenny",
      LANG: "zh_CN.UTF-8",
      OTHER: "/tmp/.mount_SpaceKabc1234/usr/lib",
      SPACEKNIFE_HOME: "/home/lenny/sk",
    });
    // a trailing slash on APPDIR changes nothing
    expect(appImageEnv({ ...running, APPDIR: `${mount}/` })).toEqual(appImageEnv(running));
  });

  it("keeps everything else as it is", () => {
    const env = { PATH: "/usr/bin:/bin", LD_LIBRARY_PATH: "/opt/lib", HOME: "/home/lenny" };
    expect(appImageEnv(env)).toEqual(env);
    expect(appImageEnv({ ...env, APPDIR: "" })).toEqual(env);
  });

  it("carries --no-sandbox, the switch a start needs where this one needed it, and nothing else", () => {
    expect(carriedSwitches(["--no-sandbox", "--updated"])).toEqual(["--no-sandbox"]);
    expect(carriedSwitches(["--updated", "--remote-debugging-port=0"])).toEqual([]);
  });

  it("asks the question with the start's switches, every symbol bound at once", () => {
    expect(startupCheckArgs(["--no-sandbox"])).toEqual(["--startup-check", "--no-sandbox"]);
    expect(startupCheckEnv({ HOME: "/home/lenny" })).toEqual({ HOME: "/home/lenny", LD_BIND_NOW: "1" });
  });
});

describe("the answer to the startup check", () => {
  it("is yes only with the answer and exit code 0", () => {
    expect(judgeStartup("S.AppImage.new", 0, null, '{"ok":true,"electron":"44.9.3"}\n', "")).toBeNull();
    expect(judgeStartup("S.AppImage.new", 0, null, '\n  {"ok":true}  \n', "noise\n")).toBeNull();
    expect(judgeStartup("S.AppImage.new", 1, null, '{"ok":true}\n', "")).toBe("S.AppImage.new ended with exit code 1");
  });

  it("says why not", () => {
    expect(judgeStartup("S", 1, null, '{"ok":false,"error":"no libwebkit2gtk-4.1"}\n', "")).toBe("no libwebkit2gtk-4.1");
    expect(judgeStartup("S", 1, null, '{"ok":false}\n', "")).toBe("S says it can't start");
    expect(judgeStartup("S", 0, null, "", "")).toBe("S did not answer --startup-check");
    expect(judgeStartup("S", 0, null, "hello\n", "something went wrong\n")).toBe("S did not answer --startup-check");
    expect(judgeStartup("S", null, "SIGTRAP", "", "[1:2:FATAL:setuid_sandbox_host.cc] The SUID sandbox helper binary was found, but is not configured correctly.\n")).toBe(
      "S was ended by signal SIGTRAP: [1:2:FATAL:setuid_sandbox_host.cc] The SUID sandbox helper binary was found, but is not configured correctly.",
    );
    expect(judgeStartup("S", 127, null, "", "/tmp/.mount_x/spaceknife: error while loading shared libraries: libgtk-4.so.1: cannot open shared object file\n")).toBe(
      "S ended with exit code 127: error while loading shared libraries: libgtk-4.so.1: cannot open shared object file",
    );
    expect(judgeStartup("S", null, null, "", "")).toBe("S ended without an exit code");
  });

  it("quotes the last line that says something, at most 300 characters", () => {
    expect(lastLine("")).toBeNull();
    expect(lastLine("first\n\nlast  \n\n")).toBe("last");
    expect(lastLine(`${"x".repeat(400)}\n`)).toBe("x".repeat(300));
  });
});

describe("running the startup check", () => {
  const node = process.execPath;
  const name = basename(node);
  /** A program that does what `body` says (node runs it). */
  const script = (body: string) => {
    const file = join(temp(), "program.mjs");
    writeFileSync(file, body);
    return file;
  };

  it("is yes when the program answers yes, and passes the arguments and environment on", async () => {
    const seen = join(temp(), "seen.json");
    const program = script(`
      import { writeFileSync } from "node:fs";
      writeFileSync(${JSON.stringify(seen)}, JSON.stringify({ args: process.argv.slice(2), bind: process.env.LD_BIND_NOW, home: process.env.SK_TEST_HOME }));
      process.stdout.write(JSON.stringify({ ok: true, electron: "44.9.3" }) + "\\n");
    `);
    const env = startupCheckEnv({ ...process.env, SK_TEST_HOME: "/home/lenny" });
    expect(await runStartupCheck(node, [program, ...startupCheckArgs(["--no-sandbox"])], env)).toBeNull();
    expect(JSON.parse(readFileSync(seen, "utf8"))).toEqual({ args: ["--startup-check", "--no-sandbox"], bind: "1", home: "/home/lenny" });
  });

  it("is the program's own reason, or what it did", async () => {
    const no = script(`process.stdout.write(JSON.stringify({ ok: false, error: "too old" }) + "\\n"); process.exitCode = 1;`);
    expect(await runStartupCheck(node, [no], process.env)).toBe("too old");
    const crash = script(`process.stderr.write("x: error while loading shared libraries: libnss3.so: cannot open shared object file\\n"); process.exit(127);`);
    expect(await runStartupCheck(node, [crash], process.env)).toBe(`${name} ended with exit code 127: error while loading shared libraries: libnss3.so: cannot open shared object file`);
    const silent = script(`process.exit(0);`);
    expect(await runStartupCheck(node, [silent], process.env)).toBe(`${name} did not answer --startup-check`);
  });

  it("gives up on a program that doesn't answer, and ends it", async () => {
    const pidFile = join(temp(), "pid");
    const hang = script(`
      import { writeFileSync } from "node:fs";
      writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));
      process.stderr.write("still starting\\n");
      setInterval(() => {}, 1000);
    `);
    const why = await runStartupCheck(node, [hang], process.env, 1500);
    expect(why).toBe(`${name} did not answer --startup-check within 1.5 s (it said: still starting)`);
    const pid = Number(readFileSync(pidFile, "utf8"));
    pids.push(pid);
    expect(await until(() => !alive(pid), 5000)).toBe(true);
  });

  it("doesn't wait for what the program started and left holding its output", async () => {
    const pidFile = join(temp(), "pid");
    // Chromium's zygote and an AppImage's mount outlive the answer for a moment
    const parent = script(`
      import { spawn } from "node:child_process";
      import { writeFileSync } from "node:fs";
      const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 10000)"], { stdio: "inherit" });
      writeFileSync(${JSON.stringify(pidFile)}, String(child.pid));
      child.unref();
      process.stdout.write(JSON.stringify({ ok: true }) + "\\n");
    `);
    const started = Date.now();
    expect(await runStartupCheck(node, [parent], process.env)).toBeNull();
    pids.push(Number(readFileSync(pidFile, "utf8")));
    expect(Date.now() - started).toBeLessThan(8000);
  });

  it("says when the program can't be run at all", async () => {
    const missing = join(temp(), "SpaceKnife-0.4.0-x86_64.AppImage.new");
    expect(await runStartupCheck(missing, ["--startup-check"], process.env)).toMatch(/^SpaceKnife-0\.4\.0-x86_64\.AppImage\.new can't be run: /);
  });

  it.runIf(process.platform !== "win32")("asks an AppImage-like program its question, as the update does", async () => {
    const dir = temp();
    const program = join(dir, "SpaceKnife-0.4.0-x86_64.AppImage.new");
    const seen = join(dir, "seen");
    writeFileSync(program, `#!/bin/sh\necho "$* $LD_BIND_NOW" > '${seen}'\necho '{"ok":true}'\n`);
    chmodSync(program, 0o755);
    expect(await startupCheck(program, ["--no-sandbox"], process.env)).toBeNull();
    expect(readFileSync(seen, "utf8").trim()).toBe("--startup-check --no-sandbox 1");
  });
});

describe("putting the new AppImage in place", () => {
  /** The user's AppImage (the old version) and the downloaded one in the updates folder. */
  function files() {
    const dir = temp();
    const target = join(dir, "SpaceKnife-0.3.5-x86_64.AppImage");
    const file = join(dir, "updates-SpaceKnife-0.4.0-x86_64.AppImage");
    writeFileSync(target, "0.3.5");
    writeFileSync(file, "0.4.0");
    return { dir, target, file };
  }
  const read = (path: string) => readFileSync(path, "utf8");

  it("checks the copy next to the old file, then keeps the old one as .previous", async () => {
    const { target, file } = files();
    const asked: string[] = [];
    const placed = await placeAppImage(file, target, nodeFiles, async (program) => {
      asked.push(program);
      expect(read(program)).toBe("0.4.0");
      if (process.platform !== "win32") expect(statSync(program).mode & 0o777).toBe(0o755);
      return null;
    });
    expect(placed).toEqual({ ok: true, previous: previousOf(target) });
    expect(asked).toEqual([nextOf(target)]);
    expect(read(target)).toBe("0.4.0");
    expect(read(previousOf(target))).toBe("0.3.5");
    expect(existsSync(nextOf(target))).toBe(false);
    expect(read(file)).toBe("0.4.0"); // the download stays where it is (the backend clears it)
  });

  it("keeps one old version: the one before goes, and so does a copy an earlier attempt left", async () => {
    const { target, file } = files();
    writeFileSync(previousOf(target), "0.3.4");
    writeFileSync(nextOf(target), "half");
    expect((await placeAppImage(file, target, nodeFiles, async () => null)).ok).toBe(true);
    expect(read(target)).toBe("0.4.0");
    expect(read(previousOf(target))).toBe("0.3.5");
  });

  it("changes nothing when the new version can't start here", async () => {
    const { target, file } = files();
    writeFileSync(previousOf(target), "0.3.4");
    const placed = await placeAppImage(file, target, nodeFiles, async () => "no libgtk-4");
    expect(placed).toEqual({ ok: false, key: "updateCantStart", error: "no libgtk-4" });
    expect(read(target)).toBe("0.3.5");
    expect(read(previousOf(target))).toBe("0.3.4");
    expect(existsSync(nextOf(target))).toBe(false);
  });

  it("changes nothing when the copy fails", async () => {
    const { dir, target } = files();
    let asked = false;
    const placed = await placeAppImage(join(dir, "gone.AppImage"), target, nodeFiles, async () => {
      asked = true;
      return null;
    });
    expect(placed.ok).toBe(false);
    if (!placed.ok) expect(placed.key).toBe("updateReplaceFailed");
    expect(asked).toBe(false);
    expect(read(target)).toBe("0.3.5");
    expect(existsSync(nextOf(target))).toBe(false);
    expect(existsSync(previousOf(target))).toBe(false);
  });

  it("renames the old file where a file system has no hard links", async () => {
    const { target, file } = files();
    const noLinks: AppImageFiles = {
      ...nodeFiles,
      link: async () => {
        throw new Error("EPERM: operation not permitted, link");
      },
    };
    expect((await placeAppImage(file, target, noLinks, async () => null)).ok).toBe(true);
    expect(read(target)).toBe("0.4.0");
    expect(read(previousOf(target))).toBe("0.3.5");
    expect(existsSync(nextOf(target))).toBe(false);
  });

  it("leaves the old file in its place when the last rename fails, with or without links", async () => {
    for (const linkWorks of [true, false]) {
      const { target, file } = files();
      const failing: AppImageFiles = {
        ...nodeFiles,
        link: linkWorks
          ? nodeFiles.link
          : async () => {
              throw new Error("EPERM");
            },
        rename: async (from, to) => {
          if (from === nextOf(target)) throw new Error("EACCES: permission denied, rename");
          await nodeFiles.rename(from, to);
        },
      };
      const placed = await placeAppImage(file, target, failing, async () => null);
      expect(placed, String(linkWorks)).toEqual({ ok: false, key: "updateReplaceFailed", error: "EACCES: permission denied, rename" });
      expect(read(target)).toBe("0.3.5");
      expect(existsSync(nextOf(target))).toBe(false);
      expect(existsSync(previousOf(target))).toBe(false);
    }
  });

  it.runIf(process.platform !== "win32")("never leaves $APPIMAGE missing, and the running app keeps reading its old file", async () => {
    const { target, file } = files();
    const fd = openSync(target, "r"); // as the AppImage's mount holds it
    let missing = false;
    const watching: AppImageFiles = {
      ...nodeFiles,
      rename: async (from, to) => {
        await nodeFiles.rename(from, to);
        if (!existsSync(target)) missing = true;
      },
    };
    try {
      expect((await placeAppImage(file, target, watching, async () => null)).ok).toBe(true);
      const buf = Buffer.alloc(16);
      expect(buf.toString("utf8", 0, readSync(fd, buf, 0, buf.length, 0))).toBe("0.3.5");
    } finally {
      closeSync(fd);
    }
    expect(missing).toBe(false);
    expect(read(target)).toBe("0.4.0");
    expect(statSync(previousOf(target)).nlink).toBe(1);
  });
});
