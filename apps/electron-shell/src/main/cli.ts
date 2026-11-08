// 0.3.4's headless launch forms (`SpaceKnife --headless`, `SpaceKnife --listen <address>`, with the
// options anywhere among the arguments, and `--help`): scripts and service units start the app that
// way. They now go to `spaceknife-cli headless …` instead of a window (index.ts runs it with this
// process's terminal). The Tauri shell's src-tauri/src/cli.rs reads them the same way.
//
// Only the headless options are passed on; everything else among the arguments (Chromium's own
// switches, the app's path in a checkout, macOS's -psn_…) is left out, as 0.3.4 ignored it. A value
// is the next argument unless that is missing or another option (`--…`), or follows "=".

/** Options with a value, as spaceknife-cli headless takes them. */
const VALUE_OPTIONS = new Set(["--listen", "--tls-cert", "--tls-key", "--ui"]);

/** The arguments for spaceknife-cli (`headless …`) when these are a headless launch, else null (the
 *  window). `args` are the program's, after its own path. */
export function headlessArgs(args: readonly string[]): string[] | null {
  let headless = false;
  const out: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!;
    const eq = arg.startsWith("--") ? arg.indexOf("=") : -1;
    const name = eq > 0 ? arg.slice(0, eq) : arg;
    if (name === "--headless" && eq < 0) {
      headless = true;
    } else if (name === "--help" || name === "-h") {
      headless = true;
      out.push("--help");
    } else if (name === "--yes" && eq < 0) {
      out.push("--yes");
    } else if (VALUE_OPTIONS.has(name)) {
      if (name === "--listen") headless = true;
      let value: string | null = eq > 0 ? arg.slice(eq + 1) : null;
      if (value === null) {
        const next = args[i + 1];
        if (next !== undefined && !next.startsWith("--")) {
          value = next;
          i++;
        }
      }
      // without a value spaceknife-cli says the value is missing
      out.push(value === null ? name : `${name}=${value}`);
    }
  }
  return headless ? ["headless", ...out] : null;
}
