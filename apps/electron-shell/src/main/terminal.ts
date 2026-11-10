// 「在终端中打开」: a terminal in a folder, the system's own (the Tauri shell's src-tauri/src/terminal.rs
// is the same list). The folder never goes through a shell: 0.3.4 ran `cmd.exe /c start "" wt.exe -d
// <folder>`, and cmd ran whatever followed a `&` in a folder's name (review issue 1). Now each
// terminal is started directly with an argument list, and the folder is also its working directory.
//
// - Windows: Windows Terminal (`wt.exe -d <folder>`). Windows Terminal splits its own command line at
//   a `;` and keeps a `\;` as it is, so a folder with a `;` in its name is passed as `-d .` with the
//   folder as the working directory. A folder Win32 can't name (a part ending in a dot or a space,
//   `setup.`) gets no terminal: it would open in the folder without the dot.
// - macOS: Terminal.app (`open -a Terminal <folder>`).
// - Linux: the desktop's default terminal (x-terminal-emulator, Debian and Ubuntu), else the first
//   of the common terminals that is installed (RHEL 9 has no x-terminal-emulator: GNOME Terminal).

export interface TerminalCommand {
  file: string;
  args: string[];
  /** the terminal's working directory: the folder */
  cwd: string;
}

/** Whether a Windows path has a part Win32 doesn't keep as it is: one ending in a dot or a space
 *  ("." and ".." aside), which CreateProcess and the terminals would open without that end. */
export function win32Unaddressable(path: string): boolean {
  const body = path.replace(/^(?:\\\\[?.]\\|\\\\)/, "");
  return body.split(/[\\/]/).some((part) => part !== "." && part !== ".." && /[. ]$/.test(part));
}

/** Absolute in the platform's own sense (a drive or a UNC share on Windows, "/" elsewhere). */
export function isAbsoluteFor(platform: NodeJS.Platform, dir: string): boolean {
  if (platform === "win32") return /^(?:\\\\\?\\)?[A-Za-z]:[\\/]/.test(dir) || /^\\\\[^\\/?.][^\\/]*[\\/][^\\/]+/.test(dir);
  return dir.startsWith("/");
}

/** The terminals to try, in order, for a folder (none when there is none for it). */
export function terminalCommands(platform: NodeJS.Platform, dir: string): TerminalCommand[] {
  if (!dir || dir.includes("\0") || !isAbsoluteFor(platform, dir)) return [];
  if (platform === "win32") {
    if (win32Unaddressable(dir)) return [];
    return [{ file: "wt.exe", args: ["-d", dir.includes(";") ? "." : dir], cwd: dir }];
  }
  if (platform === "darwin") return [{ file: "open", args: ["-a", "Terminal", dir], cwd: dir }];
  const at = `--working-directory=${dir}`;
  return [
    { file: "x-terminal-emulator", args: [], cwd: dir },
    { file: "gnome-terminal", args: [at], cwd: dir },
    { file: "ptyxis", args: ["--new-window", at], cwd: dir },
    { file: "kgx", args: [at], cwd: dir },
    { file: "konsole", args: ["--workdir", dir], cwd: dir },
    { file: "xfce4-terminal", args: [at], cwd: dir },
    { file: "mate-terminal", args: [at], cwd: dir },
    { file: "tilix", args: [at], cwd: dir },
    { file: "xterm", args: [], cwd: dir },
  ];
}
