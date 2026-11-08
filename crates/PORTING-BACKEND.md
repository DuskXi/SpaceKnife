# Backend port: Python → Rust (shared brief)

Branch `dev/rust`. The Python backend (`packages/backend/src/spaceknife`, tests in `packages/backend/tests`) is the
reference; it is deleted at the end. Target: no Python, no Electron. Old snapshot files / old clients need not be
supported. **Performance and memory are goals**: use the zero-copy column APIs, rayon where it pays, avoid per-node
allocation, avoid Python-isms (dicts of objects per node).

## Crates (read their README / crate docs before using them)

| Crate | What | State |
|---|---|---|
| `sk-native` | parallel dir walker (`hardscan::scan_slice`), NTFS MFT parser (`mft`) | done (`crates/sk-native/README.md`) |
| `sk-protocol` | wire types (same JSON as pydantic), JSON-RPC envelope, binary frames (`FrameBuilder`), filter-syntax parser (`CompiledVocab`), TS generation | done (`crates/sk-protocol/README.md`) |
| `sk-store` | columnar node store, threading contract, mutate (monitor), subtree, PathIndex, snapshots (format 3), `testing` fixtures (feature `test-support`) | done (`crates/sk-store/src/lib.rs` docs) |
| `sk-core` | all other backend logic, one module per Python package | being ported (this brief) |
| `sk-server` | WebSocket JSON-RPC server, sessions, handlers, topics, CLI (`scan` / `run` / `snapshot` / `serve`) | later |

## sk-core module ownership

Each module of `crates/sk-core/src/lib.rs` is owned by exactly one porting task. Only edit your own modules (a
module may become `x/mod.rs` + submodules: delete the stub `x.rs`). Do not edit other modules; if you need
something from a module that is not there yet, read its current state, and if it is still missing, define a small
private stand-in in your own module with a `// TODO(<owner module>)` note and mention it in your report.
`Cargo.toml` of sk-core and the root `[workspace.dependencies]` are shared: make small targeted edits, re-read first.

| Task | Modules (Python source) |
|---|---|
| platform | `i18n` (+ `locales/*.json`), `paths`, `fmt`, `privacy`, `log` (log.py + logbus.py: tracing + an in-memory bus for the `log` topic, rotating file log), `memory`, `settings` (settings.py + settings_io.py), `update` (update.py + update_swap.py), `classes`, `events` |
| system | `disks` (disks/*), `io` (io/*: governor, sampler, priority, registry, disks), `helper` (helper/*: the elevated helper process — its entry point will be a subcommand of the single app binary) |
| scan | `volume` (scan/volume.py), `media` (scan/media.py), `scan` (scanner.py, worker.py, mft.py → `sk_store::StoreBuilder`), `monitor` (monitor/*), `pathclass` |
| view | `filter` (filter/* incl. the name index of filter/names.py), `layout` (layout/*), `view` (view/*) |
| analysis | `diff`, `ops` (ops/*: basket, locks, paths, trash), `insights` |
| output | `export` (export/* incl. builtin `.sktpl` templates), `palette` (palette/* incl. pinyin.json) |

Server-side glue (`packages/backend/src/spaceknife/server/*`, `cli*.py`) is NOT part of sk-core: expose clean
Rust APIs that the server task will call (typed with `sk_protocol` message types where the Python code returned
protocol models).

## Rules

- Port the relevant Python tests to Rust tests (unit or `crates/sk-core/tests/<module>_*.rs`). Use
  `sk_store::testing` fixtures (`sk-store` with feature `test-support` is a dev-dependency of sk-core).
- `cargo test -p sk-core <your filter>` and `cargo clippy -p sk-core --all-targets` must be clean for your modules.
  Other tasks build concurrently; a failure clearly inside another task's module is theirs — note it, move on.
- Platform code: Windows is testable here; Linux/macOS code must be cfg-gated and written carefully (not compiled
  here). Prefer well-known crates (windows-sys 0.61 is already used; libc).
- Do NOT git commit. Do not delete anything under `packages/` or `apps/electron`.
- Finish with a report: public API summary (it is handed to the server task), deviations from Python, what is
  not done, test results.

## Known gaps

The integration notes of the porting tasks (and of the 0.3.4 main merge) are resolved in
sk-server / the Tauri shell; what is still open:

- **macOS NVMe SMART**: `sk_core::disks::mac_smart` is a stub (the IOKit SMART user client FFI is not
  ported), so macOS disks show no NVMe health record.
- **Untested platforms**: the Linux and macOS code paths (sk-native's POSIX lister, `monitor::linux` /
  `monitor::mac`, the freedesktop / macOS trash, glibc `memory::tune_allocator` / `trim`, privacy and
  volume probes) are cfg-gated and written carefully but compiled and tested on Windows only here.
- **`ops::names`** is still the ops module's own copy of the scan worker's name checks
  (`TODO(scan)` in `ops/names.rs`, `ops/win.rs`).
- **Names that are not Unicode in the basket**: items keep their real path (`BasketItem::os_path` /
  `os_members`), are found and executed by it, and two siblings whose shown names are the same are
  two items (their keys keep such names exactly); the lock check (Restart Manager, by shown path)
  does not name who holds such a file.

## Wave 3: sk-server (server task + cli task)

Binaries (no compatibility with old launch scripts needed):
- `spaceknife` — the Tauri GUI app (`apps/desktop/src-tauri`): starts the server in-process via `sk_server::start`.
- `spaceknife-cli` — console binary in `crates/sk-server/src/bin/spaceknife-cli.rs`: `scan`, `run`, `snapshot`,
  `serve` (ports `cli*.py`), `headless`/`--listen` (HTTP + WS for a browser, replaces Electron's headless mode),
  `helper` (→ `sk_core::helper::run_main`), `--apply-update <plan>` (→ `sk_core::update::swap::main`).
  The GUI binary must also dispatch `helper` and `--apply-update` (it re-execs itself).

Contract between the two tasks (server task owns it, cli task calls it):
```rust
pub struct ServeOptions { pub host: IpAddr, pub port: u16 /*0 = random*/, pub token: String,
    pub allow_origins: Vec<String>, pub app_version: Option<String>, pub install_kind: Option<String>,
    pub system_locale: Option<String>, pub static_dir: Option<PathBuf> /* headless: serve the UI */,
    pub tls: Option<(PathBuf, PathBuf)> }
pub struct ServerHandle { pub url: String /* ws://host:port/v1 */, pub port: u16, pub pid: u32, /* … */ }
impl ServerHandle { pub fn shutdown(&self); pub async fn wait(self); }
pub async fn start(opts: ServeOptions) -> anyhow::Result<ServerHandle>;   // returns once listening
```
Origins the server always allows: `tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`.
