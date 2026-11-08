# Protocol parity gate

A black-box comparison of two SpaceKnife backends over the WebSocket protocol
([docs/protocol.md](../../docs/protocol.md)): the **reference** is the 0.3.4 Python backend
(`python -m spaceknife serve` from a worktree of tag `v0.3.4`), the **candidate** is
`spaceknife-cli serve`. Both get the same scenario, an isolated `SPACEKNIFE_HOME`, the same small
fixture trees and the same local release feed; what they answer is normalized and compared.

Two ways to run it:

| | needs | what it compares |
|---|---|---|
| `parity.py run` | Python 3.12 + uv, a `v0.3.4` worktree, a built `spaceknife-cli` | reference and candidate, live, on this machine |
| `parity.py check` / `cargo test -p sk-server --test parity_replay` | only the candidate | the candidate against `golden/<platform>/`, recorded from the reference |

The Rust replayer ([crates/sk-server/tests/parity_replay.rs](../../crates/sk-server/tests/parity_replay.rs))
runs in the normal test suite, so the gate keeps working once Python is gone. Goldens exist for
`win32` (recorded on Windows 11) and `linux` (Ubuntu 22.04 under WSL); a platform without goldens
(macOS today) replays nothing and says so. Re-record both after changing a scenario.

## Running

```bash
# a reference worktree, outside your checkout
git worktree add --detach ../sk-ref-0.3.4 v0.3.4
(cd ../sk-ref-0.3.4 && uv sync --frozen)        # builds 0.3.4's native extension too
cargo build -p sk-server --bin spaceknife-cli

uv run tests/parity/parity.py run    --ref ../sk-ref-0.3.4 --cand target/debug/spaceknife-cli [scenario ...]
uv run tests/parity/parity.py record --ref ../sk-ref-0.3.4 [scenario ...]   # rewrite golden/<platform>/
uv run tests/parity/parity.py check  --cand target/debug/spaceknife-cli    # candidate vs goldens
uv run tests/parity/parity.py coverage                                       # methods the scenarios call
cargo test -p sk-server --test parity_replay [-- --include-ignored]
```

`--ref` / `--cand` default to `SPACEKNIFE_PARITY_REF` / `SPACEKNIFE_PARITY_RUST`. `run --out DIR`
also writes both normalized transcripts. Everything happens in fresh temp folders; nothing reads
the real disk beyond the fixture trees (a few hundred small files, two of them 50 MB sized but not
written: `fill: zero`). No MFT, no elevation, no downloads, no trash: `basket.execute` deletes
fixture files permanently.

Status lines: `PASS` / `FAIL` (unexpected differences), `XFAIL` / `XPASS` for a scenario marked
`xfail` (expected to differ; an `XPASS` means the mark can go). `known bugs: …` names the known
differences of the scenario that are product bugs (see below).

## Layout

| path | what |
|---|---|
| `parity.py` | the command line (PEP 723 script: `uv run` installs `websockets`) |
| `skparity/engine.py` | backends, the WebSocket client, the steps |
| `skparity/norm.py` | normalization and comparison |
| `skparity/frames.py` | binary frame decoding |
| `skparity/fixtures.py` | fixture trees |
| `normalize.json` | the normalization rules (shared with the Rust replayer) |
| `known_diffs.json` | differences accepted in every scenario |
| `fixtures/trees.json` | fixture trees; `fixtures/feed.json` the release list served as `SPACEKNIFE_UPDATE_FEED` |
| `scenarios/*.json` | the scenarios (language-neutral) |
| `golden/<platform>/*.json` | the reference's normalized transcripts; `upgrade.home/` the data folder 0.3.4 wrote; `listing.json` how the recording's file system listed the fixture folders (below, "Directory order") |

`scenarios/invalid_params.json` and `scenarios/filter_oracle.json` are generated
(`parity.py gen-invalid` from `crates/sk-protocol/generated/protocol.schema.json`,
`parity.py gen-filter` from the SpaceSniffer filter oracle
`crates/sk-core/tests/data/spacesniffer_filter_oracle.json`).

## Scenarios

| scenario | covers |
|---|---|
| `handshake` | ready line, `session.hello` result, wrong token (-32001/4401), other major version (-32003/4426), minor versions, bad params (-32602/4400), no subprotocol (HTTP 400), first frame binary / not hello / not JSON, hello timeout (4408), other paths (404), foreign origin (403), capability intersection, a second hello, an old client |
| `invalid_params` | every method: an unknown field, a missing required field; params by position; unknown topic and method |
| `scan_tree` | `scan.check_paths`, scans in-process and pooled, status/list, `tree.*`, `path.forms`, pause/resume/continue/set_load on a finished scan, `snapshot.save/load/list` and bad snapshots, rescan after changes, two roots, close, error codes |
| `views` | views and their binary frames (treemap, flame graph, polar flame graph): navigation, selection, settings, colour modes, filters and results, reveal, tags, close, sessions without `visuals.v1` |
| `filter_oracle` | the 190 distinct SpaceSniffer oracle probes (results, dialogs, units) through `filter.parse`, `view.filter`, `view.results` on scanned fixture trees |
| `monitor` | live monitoring over the protocol: created/deleted/renamed files and folders reach the tree, `scan.progress` pushes, `scan.monitor` off and on |
| `settings_classes` | settings get/update/conflict/invalid/reset/export/import, `settings.changed` to another session in order, file classes and folder classes, `classes.changed`, both after a restart |
| `palette` | commands, queries, checks, path group, previews, recents across a restart |
| `basket_ops` | the delete basket across sessions (`basket.changed` as the state the other session ends up with: both backends re-send it after background lock checks), a permanent deletion with undo, `ops.progress` |
| `compare` | auto-saved snapshots, `compare.*`, a compare view's frames |
| `system` | update checks against the local feed, maintenance, machine/volume/io (shapes), `log` and `io` topics, exports (file contents), `app.shutdown` |
| `upgrade` | **0.3.4 writes, 0.3.5 reads back** (below) |

## Scenario format

A scenario is `{name, description, fixtures, steps, …}`. `fixtures` maps a folder under `$ROOT` to
a tree of `fixtures/trees.json`. Optional: `args` (extra `serve` arguments), `env`, `platforms`,
`normalize` (rules added to `normalize.json`), `known_diffs`, `xfail`.

Values: a string `$ROOT/…`, `$HOME/…` or `$TMP/…` is a real path (`/` becomes the platform's
separator); `$name.a.0.b` is a field of a result saved with `"as": "name"` (raw, before
normalization).

| step | does | records |
|---|---|---|
| `{"call": M, "params": P}` | a request (`as`, `on` a session, `timeout`, `binary`: also the binary frame(s) of this request, `pick` / `omit` top-level keys, `shape`: types only, `record: false`) | `result` or `error` (`code` and the first item's `loc`), `frames` |
| `{"wait_scan": "$s.scan_id"}` | subscribes to `scan` until a final status (`snapshot: true`: with its snapshot) | the final status, whether `items` only grew |
| `{"until": {"call", "params", "field", "null" / "equals"}}` | polls a call until a field is (not) null / equals | whether it happened in `timeout` |
| `{"subscribe": T, "params": P}` | `$/subscribe` | the result |
| `{"events": E, "count": N}` | waits for N events (`mode`: `all`, `last`, `latest` = newest queued, `shape`, `count`; `stop_at: {field, in}`: up to the first matching one; `settle`: wait that long before taking them; `dedupe`: a state event repeated unchanged counts once) | the events |
| `{"drain": E}` | forgets queued events (`settle`: wait first) | — |
| `{"handshake": {…}}` | a raw connection: `token`, `protocol`, `params`, `subprotocol`, `origin`, `path`, `send` (`hello`, `binary`, `request`, `garbage`, `nothing`), `timeout` | the answer and the close code |
| `{"connect": name, "capabilities": […]}` | another session | — |
| `{"fs": {"op": …}}` | `write` (size, mtime), `touch`, `mkdir`, `remove`, `rename` (`to`), `fixture` | — |
| `{"read": path}` | a file a backend wrote | its lines, normalized |
| `{"sleep": s}`, `{"restart": true}`, `{"exit": true}`, `{"ready": true}`, `{"hello": true}`, `{"note": …}` | | |

Any step may say `"live_only": true`: it is compared in `parity.py run` only and skipped when
replaying goldens, because it needs what only a live reference has (0.3.4's own snapshot files) or
describes this machine (disk shapes, the `io` topic).

Step errors (timeouts, a closed connection, a missing saved field) are recorded as
`{"step_error": …}`, with texts both engines write the same.

## Normalization

`normalize.json`, applied to every result and event (and in the replayer, rule for rule):

- dropped keys: timing (`elapsed_s`, `rate`, `byte_rate`, `remaining_s`, `layout_ms`), `pid`,
  `schema_hash`, `request_id`, `current_path`, `free`/`used`, and `volume` and `io` (this
  machine's disk and its load: a replay on another machine differs; the live-only `volume.list` /
  `io.status` shapes cover them); inside `monitor` its counters; in a server info (`name` +
  `platform` + `python`/`rust`) `python`, `rust`, `version`;
- `time_keys` (and any string that is an ISO date-time): checked to be a time, then `<time>`;
- ids by first appearance: `session_id`, `scan_id`, `view_id`, `sub`, `compare_id`, `task_id`,
  basket item ids → `<scan#1>` …; any string equal to a known id too (`view:<view#1>` streams);
- node ids (`node`, `parent`, `zoom_node`, a NodeInfo's `id`, the `node` column) → `@` + the node's
  path from `tree.node`; layout keys (`key`, `zoom_key`, `selected`, the `key` column) the same, with
  `@small:` / `@more:` / `@free` / `@unknown` for synthetic blocks;
- paths: the run's folders → `$ROOT`, `$HOME`, `$TMP` (also with `/`, JSON-escaped, and the WSL
  form `/mnt/c/…`), then `\` → `/` on Windows; snapshot time stamps → `<stamp>`;
- floats equal within a relative 1e-9 (`<f4` columns 1e-6).

A scenario's `normalize` adds to these: `handshake` compares `system.info`'s `platform.release` as a
string only (`shape_keys`), since it names the machine that recorded the golden (Windows 11, WSL's
kernel), not the one a replay runs on (a CI runner is a Windows Server).

Binary frames are decoded to `{stream, layout, seq, final, count, columns: {name: {dtype, values}},
names}` (the `names` blob cut by `name_len`).

## Directory order

0.3.4 sorts by size and keeps the order the file system lists a folder in for entries of equal
size (stable sorts over the scan's order: `tree.children`, `tree.summary` top files, palette
results, the layouts and their binary frames). Most fixture files are smaller than a block, so
hundreds of them tie at 4 KiB, and which of them fills a page or a frame's row is that order.
0.3.4 does not define it, the file system does:

- NTFS lists by name (case folded), the same on every NTFS volume;
- ext4 lists by a hash of the names with a seed of the file system's own (`dumpe2fs -h`: Directory
  Hash Seed): the same names come in another order on another ext4 (a CI runner);
- tmpfs, XFS, Btrfs list otherwise again, and allocate otherwise too (a folder has no block of its
  own on tmpfs), so they cannot replay the ext4 goldens at all.

So the goldens hold for a file system that lists folders as the recording's did, and
`golden/<platform>/listing.json` records how that was: each fixture tree's folders with their
entries in the recording's order (`parity.py listing`; `record` writes it), and the file system
(for ext4: its hash and seed). Normalizing the order instead would mean taking tied entries'
identities out of pages, palette results and every frame (keys, names, branch colours, selected
rows), which hides real differences; fixtures without ties would need hundreds of files of
distinct block counts, and new goldens.

The replayer and `parity.py check` replay each scenario in the system's temp folder (a folder on
its volume, as when recorded: the basket's volumes and trash), list the fixture folders they built
and compare with `listing.json`:

| in the system's temp folder | `SPACEKNIFE_PARITY_TMP` unset | set (a folder) |
|---|---|---|
| folders listed as recorded, or no differences | compared exactly | compared exactly |
| listed otherwise, and differences | not counted, with the reason (`SKIP`) | replayed again in that folder, which must list them as recorded (else a failure), and compared exactly there |

CI's Linux job makes an ext4 with the recording's hash seed (`mkfs.ext4 -E hash_seed=…`,
`.github/workflows/ci.yml`), mounts it and sets `SPACEKNIFE_PARITY_TMP` to it; Windows needs
nothing (NTFS). On the machine the Linux goldens were recorded on (WSL's ext4) the system's temp
folder already lists them as recorded. ext4's order is its directory hash (`debugfs` `dx_hash -h
half_md4 -s <seed>` of each name gives the recorded order), so any ext4 made with that seed lists
the same names alike. A scenario that needed both the recording's order and its volume (a
basket's mount point) could not pass anywhere but there; none does.

## Known differences

`known_diffs.json` (every scenario) and a scenario's `known_diffs` list path patterns of the
comparison (`steps/<step index>/…`; `*` within a segment, `**` any segments) with why. One with
`"bug"` is a product bug found by the gate, kept visible in the status line until it is fixed.
None is open. Those found so far are fixed, each with a test in the normal suites:

| bug | what it was | test |
|---|---|---|
| `scan-topic-silent-monitor` | the final status of a scan had `monitor: null`, and applied monitor changes never pushed `scan.progress`: the status bar (which reads only these pushes) said live monitoring was off | sk-server `scan_server.rs` `a_watched_scan_says_so_from_its_final_status_on`, `applied_changes_and_the_monitor_switch_push_the_scan_status` |
| `import-change-order` | `settings.import` listed changes A→Z by key instead of the settings' own order | sk-core `settings::io` `changes_are_listed_in_the_settings_order` |
| `io-topic-before-registry` | an `io` subscription made before disk sampling had started never delivered `io.load` | sk-server `scan_server.rs` `an_io_subscription_made_at_once_gets_the_disks_of_the_started_registry` |
| `linux-root-ctime` | Linux: the scan root had a creation time (std's `created()`, statx) while every other entry has none, as in 0.3.4 | sk-core `scan_scanner.rs` `the_root_has_a_creation_time_where_its_entries_do` |

The known differences left are accepted ones (texts, a sort direction nobody calls, the empty
`loc` of Rust's parameter errors, and the fields 0.3.5 adds by design: `generation` of frames,
result pages and summaries, `rev` of basket items).

## The upgrade scenario

`upgrade` has a `setup`: the reference writes a data folder (settings, file classes, a folder class,
tags, palette jumps and filters, update choices, an auto-saved snapshot); then the reference and
the candidate each start on a copy of it and read everything back, and the reference's read-back is
the expectation. It passes since the data migration (plan 5.5: SHA-1 root keys, a format-2
snapshot reader) landed; before it, tags, jumps and the 0.3.4 snapshots were lost.

`record` also stores the data folder as `golden/<platform>/upgrade.home/`: its text files with the
run's folders, the feed URL and the root keys (`{key:N}`, 0.3.4's SHA-1 key of the scenario's
`template_roots`) as placeholders, so the replayer can rebuild it for any temp folder. 0.3.4's
snapshot files are binary and name their machine's paths, so they are not stored: the steps that
need them (snapshot list, load, expected items) are `live_only`: only `parity.py run` checks them,
the replayer (`cargo test -p sk-server --test parity_replay upgrade`) checks the rest.
