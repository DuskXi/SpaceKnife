// 0.3.4's headless launch forms (`SpaceKnife --headless`, `SpaceKnife --listen <address>`, with the
// options anywhere among the arguments, and `--help`): scripts and service units start the app that
// way. They now go to spaceknife-cli instead of a window (index.ts runs it with this process's
// terminal), with the arguments as given: spaceknife-cli reads 0.3.4's form itself, wherever the
// options are (crates/sk-server/src/cli/headless.rs, `Form::App`), skips what is not headless mode's
// (Chromium's switches, the app's path in a checkout) and says what is wrong with the rest. The
// decision is the one spaceknife-cli's `wants_headless`, the Tauri shell (src-tauri/src/cli.rs) and
// SpaceKnife.com (packaging/console-launcher `route`) make, so every shell treats a command line
// alike: `--yes` or a TLS file alone is a mistake (exit 2, no window), as in 0.3.4.

/** The options of 0.3.4's app that start (or belong to) headless mode, `--name` and `--name=…`. */
const HEADLESS_OPTIONS = new Set(["--headless", "--listen", "--yes", "--tls-cert", "--tls-key"]);
const HELP_OPTIONS = new Set(["--help", "-h"]);

/** `--name=value` → `--name`; anything else unchanged. */
function optionName(arg: string): string {
  const eq = arg.startsWith("--") ? arg.indexOf("=") : -1;
  return eq > 2 ? arg.slice(0, eq) : arg;
}

/** The arguments for spaceknife-cli when these are a headless launch, else null (the window).
 *  `args` are the program's, after its own path: they go on as given, with `--headless` in front
 *  when only the help was asked for (0.3.4 answered `--help` with headless mode's options). */
export function headlessArgs(args: readonly string[]): string[] | null {
  const names = args.map(optionName);
  if (names.some((n) => HEADLESS_OPTIONS.has(n))) return [...args];
  if (names.some((n) => HELP_OPTIONS.has(n))) return ["--headless", ...args];
  return null;
}
