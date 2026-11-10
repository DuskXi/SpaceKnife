// 「在终端中打开」 never runs a folder's name through a shell (review issue 1): 0.3.4 ran
// `cmd.exe /c start "" wt.exe -d <folder>`, and a folder named `x&whoami` ran whoami. The same cases
// are in the Tauri shell's src-tauri/src/terminal.rs.

import { describe, expect, it } from "vitest";

import { isAbsoluteFor, terminalCommands, win32Unaddressable } from "./terminal";

/** Folder names a shell would read as more than a name. */
const AWKWARD = ["x&whoami", "a^b", "%PATH%", "(x)", "a;b", "two  spaces", "q\"uote", "it's", "$(id)", "`id`", "a|b", "a>b", "!x!"];

describe("terminalCommands on Windows", () => {
  it("starts Windows Terminal itself, never cmd.exe, with the folder as one argument and as its working directory", () => {
    for (const name of AWKWARD.filter((n) => !/[";|>]/.test(n))) {
      const dir = `D:\\Work\\${name}`;
      const [only, ...rest] = terminalCommands("win32", dir);
      expect(rest, name).toEqual([]);
      expect(only, name).toEqual({ file: "wt.exe", args: ["-d", dir], cwd: dir });
    }
  });

  it("passes a folder with ';' as '.' and its working directory: Windows Terminal splits its command line at ';'", () => {
    for (const dir of ["D:\\a;b", "D:\\a\\;b", "D:\\x;new-tab -p cmd calc.exe"]) {
      expect(terminalCommands("win32", dir)).toEqual([{ file: "wt.exe", args: ["-d", "."], cwd: dir }]);
    }
  });

  it("opens folders with spaces, on other drives and on shares as they are", () => {
    for (const dir of ["C:\\Program Files (x86)", "D:\\x y\\", "\\\\server\\share\\a & b", "E:\\"]) {
      expect(terminalCommands("win32", dir)[0]).toEqual({ file: "wt.exe", args: ["-d", dir], cwd: dir });
    }
  });

  it("gives no terminal for a folder Win32 can't name (trailing dot or space): it would open the folder without it", () => {
    for (const dir of ["D:\\setup.", "D:\\setup.\\sub", "D:\\trail ", "D:\\a\\...", "\\\\?\\D:\\setup."]) {
      expect(win32Unaddressable(dir), dir).toBe(true);
      expect(terminalCommands("win32", dir), dir).toEqual([]);
    }
    for (const dir of ["D:\\a.b", "D:\\.git", "D:\\a\\..\\b", "D:\\", "\\\\server\\share"]) expect(win32Unaddressable(dir), dir).toBe(false);
  });

  it("only takes absolute folders", () => {
    for (const dir of ["", "relative\\x", "\\x", "D:x", "-d", "\\\\?\\", "D:\\a\0b"]) expect(terminalCommands("win32", dir), JSON.stringify(dir)).toEqual([]);
    expect(isAbsoluteFor("win32", "\\\\?\\D:\\x")).toBe(true);
    expect(isAbsoluteFor("win32", "\\\\server\\share")).toBe(true);
  });
});

describe("terminalCommands on macOS and Linux", () => {
  it("opens Terminal.app with the folder as one argument", () => {
    for (const name of AWKWARD) {
      const dir = `/Users/lenny/${name}`;
      expect(terminalCommands("darwin", dir)).toEqual([{ file: "open", args: ["-a", "Terminal", dir], cwd: dir }]);
    }
  });

  it("tries the desktop's default terminal first, then the common ones (RHEL 9 has no x-terminal-emulator)", () => {
    const dir = "/home/lenny/x&whoami;$(id)";
    const tries = terminalCommands("linux", dir);
    expect(tries.map((t) => t.file)).toEqual(["x-terminal-emulator", "gnome-terminal", "ptyxis", "kgx", "konsole", "xfce4-terminal", "mate-terminal", "tilix", "xterm"]);
    expect(tries[0]).toEqual({ file: "x-terminal-emulator", args: [], cwd: dir });
    expect(tries[1]).toEqual({ file: "gnome-terminal", args: [`--working-directory=${dir}`], cwd: dir });
    expect(tries.find((t) => t.file === "konsole")?.args).toEqual(["--workdir", dir]);
    // every one starts in the folder, and none gets it in anything but a whole argument
    for (const t of tries) {
      expect(t.cwd).toBe(dir);
      expect(t.args.every((a) => a === dir || a === `--working-directory=${dir}` || a.startsWith("--"))).toBe(true);
    }
  });

  it("only takes absolute folders", () => {
    for (const dir of ["", "relative", "~/x", "-e", "/a\0b"]) {
      expect(terminalCommands("linux", dir)).toEqual([]);
      expect(terminalCommands("darwin", dir)).toEqual([]);
    }
  });
});
