// One line typed in the terminal: headless mode's Y/N question (headless-main.ts).
//
// stdin when it is a terminal. On Windows SpaceKnife.exe is a windowed program with no stdin of its
// own: started by SpaceKnife.com (packaging/console-launcher), it is attached to that console, which
// waits for it, and the answer is read from the console's input (CONIN$) directly. Electron's fs
// rewrites device paths (asar support), so the device is opened with original-fs.

import { createRequire } from "node:module";
import { createInterface } from "node:readline";
import { ReadStream } from "node:tty";

const requireCjs = createRequire(import.meta.url);

function rawFs(): typeof import("node:fs") {
  try {
    return requireCjs("original-fs") as typeof import("node:fs"); // Electron
  } catch {
    return requireCjs("node:fs") as typeof import("node:fs");
  }
}

/** The next line from the terminal, without its line break; null at end of input or when the
 *  console can't be opened. */
export function readLine(from: "stdin" | "console"): Promise<string | null> {
  let input: NodeJS.ReadableStream;
  if (from === "stdin") input = process.stdin;
  else {
    try {
      input = new ReadStream(rawFs().openSync("\\\\.\\CONIN$", "r+"));
    } catch {
      return Promise.resolve(null);
    }
  }
  return new Promise((done) => {
    const rl = createInterface({ input, terminal: false });
    let answered = false;
    const finish = (line: string | null) => {
      if (answered) return;
      answered = true;
      rl.close();
      if (from === "stdin") process.stdin.pause();
      else (input as ReadStream).destroy(); // closes the descriptor
      done(line);
    };
    rl.once("line", (line) => finish(line));
    rl.once("close", () => finish(null));
  });
}
