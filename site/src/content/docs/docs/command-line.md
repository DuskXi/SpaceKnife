---
title: Command line
description: Scan, script and export from a terminal with the backend's command line, and the environment variables that change how SpaceKnife works.
---

The backend that does the scanning for the app has its own command line. Use it to scan without a window, to run scans and exports from scripts or scheduled tasks, and to look inside saved snapshots. Snapshots it saves go to the same folder the app uses, so they show up under **Recent** on the start page and you can compare against them.

## Where the program is

The installers don't put the command line on your PATH. It is the file `spaceknife-backend` inside the app's `resources/backend` folder:

| How you installed SpaceKnife | The command line |
|---|---|
| Windows installer, current user | `%LOCALAPPDATA%\Programs\SpaceKnife\resources\backend\spaceknife-backend.exe` (the default folder) |
| Windows installer, all users | `C:\Program Files\SpaceKnife\resources\backend\spaceknife-backend.exe` (the default folder) |
| Windows portable | `resources\backend\spaceknife-backend.exe` next to `SpaceKnife.exe` |
| deb / rpm | `/opt/SpaceKnife/resources/backend/spaceknife-backend` |
| Linux portable | `resources/backend/spaceknife-backend` next to `spaceknife` |
| AppImage | inside the image: `./SpaceKnife-<version>-x86_64.AppImage --appimage-extract` unpacks it to `squashfs-root/`, then use `squashfs-root/resources/backend/spaceknife-backend` |
| macOS | `/Applications/SpaceKnife.app/Contents/Resources/backend/spaceknife-backend` |
| From source | `uv run spaceknife` in the repository |

The examples on this page write `spaceknife-backend`. Add the folder to your PATH, or make an alias:

```bash
alias skb=/opt/SpaceKnife/resources/backend/spaceknife-backend
```

:::note
On a deb or rpm install, `spaceknife` (`/usr/bin/spaceknife`) starts the app, not the command line.

The app's own executable takes options too: `--headless` and `--listen` run SpaceKnife without a window and serve its interface to a browser, on this computer or another one. See [Headless mode](../headless/).
:::

:::caution[Portable copies]
A portable copy keeps its data in the `data` folder next to it, but only the app knows that. To make the command line use the same snapshots and settings, set `SPACEKNIFE_HOME` to that `data` folder (see [Environment variables](#environment-variables)).
:::

Messages appear in the language chosen in **Settings › General › Interface language** (the command line reads your settings file). The `--help` texts are in English.

## Commands

| Command | What it does |
|---|---|
| `scan <paths>…` | Scan folders or drives and print where the space goes; saves a snapshot |
| `run <commands>…` | Run a sequence of `scan`, `load`, `filter`, `save` and `export` commands without a window |
| `snapshot list` | List the saved snapshots, newest first |
| `snapshot show <file>` | Show where the space went in a snapshot, without rescanning |
| `version` | Print the backend, protocol and native extension versions |
| `serve` | Run the backend server that the app talks to |
| `schema` | Write the protocol's JSON Schema (for developers) |

`spaceknife-backend <command> --help` lists every option of a command.

## scan

```bash
spaceknife-backend scan "D:\Photos"
spaceknife-backend scan /home/lenny --top 20 --depth 3
```

The scan runs in parallel, the same way as in the app, and shows a live progress line. When it ends you get:

- **Scan result**: status, time, speed, the number of items, files and folders, size on disk and logical size. For a whole volume also the free space and the unknown space, and how many folders couldn't be accessed (they count as unknown space).
- **Largest items in** each root, `--depth` levels deep with `--top` rows per level; the rest of a level is summed up in one `…N more` row.
- **Largest files**, with their age and full path.

| Option | Default | What it does |
|---|---|---|
| `--top N` | 15 | Rows per level in the summary, and the number of largest files |
| `--depth N` | 2 | Levels to expand in the summary |
| `--disk-limit LEVEL` | no limit | Limit the disk load like the app does: `max`, `balanced` (70 %), `quiet` (35 %), `idle`, or a percentage from 1 to 100 |
| `--workers N` | as in the app | Listing threads (worker processes when the native extension is missing). `0` scans in this process with the Python walker, on one thread and without a load limit |
| `--cross-mounts` / `--no-cross-mounts` | off | Descend into file systems mounted below the roots |
| `--save FILE` | — | Also save the snapshot to this file |
| `--no-snapshot` | — | Don't save a snapshot automatically |
| `--profile` | — | Also print **Where the time went** (see below) |
| `--mft` | off | Windows: read the drive's $MFT (the MFT quick scan, as in the app) instead of listing folder by folder. Only for one whole local NTFS drive, such as `C:\`; asks for administrator rights once |

Good to know:

- Several paths in one command become one scan with several roots, like picking several locations on the start page.
- A scan that finishes is saved to the app's snapshot folder. The command line keeps the 10 newest snapshots of each location there, and one a week before that.
- Without `--mft` the command line always does a full scan, folder by folder. With `--mft`, a path the MFT quick scan can't read (a folder rather than a whole drive, a ReFS, FAT, exFAT or Btrfs drive, a network drive, several paths, or not Windows) is scanned folder by folder anyway, and the command first prints `The MFT quick scan can't read this: …` with the reason. The same happens, with the reason, when you refuse the administrator prompt. The **Scan result** then says `Method full scan (…)`; a scan that read the $MFT says `Method MFT quick scan`, and `--profile` shows its phases (starting the helper, reading and parsing the $MFT, building the tree) instead of the listing threads.
- `--disk-limit idle` also reads at the system's low I/O priority, as the **Idle** level does in the app. How the limit works is explained in [Scan speed and disk load](../performance/).
- Exit codes: `0` when the scan ran, `2` when it couldn't start (for example, the path doesn't exist), `130` when you pressed <kbd>Ctrl</kbd>+<kbd>C</kbd>.

### Recipes

A gentle scan while you keep working:

```bash
spaceknife-backend scan "D:\" --disk-limit quiet
```

A history for snapshot comparison: run a scan every night from Task Scheduler, cron or launchd. The app lists these snapshots under **Recent** and can compare any two of them (see [Snapshots and comparison](../snapshots-compare/)).

```bash
spaceknife-backend scan /home/lenny --disk-limit idle
```

A whole system drive by its $MFT, much faster than folder by folder (Windows, administrator rights):

```bash
spaceknife-backend scan "C:\" --mft
```

Measuring without leaving a snapshot behind:

```bash
spaceknife-backend scan "C:\Program Files" --no-snapshot --profile
```

On Linux, folders your account can't read count as unknown space. To see into them, run the command line with `sudo`. Add `--no-snapshot` so it doesn't save a snapshot into root's folders:

```bash
sudo /opt/SpaceKnife/resources/backend/spaceknife-backend scan / --no-snapshot
```

### Where the time went

`--profile` adds a panel that tells you what held the scan back:

| Line | What it tells you |
|---|---|
| `Native walker · 16 listing threads` | Which walker listed the folders (`Python walker` without the native extension) and how many listing threads were allowed |
| `Listing threads busy …` | Share of the listing threads' time spent in system calls. Near 100 % means the disk or the file system is the limit |
| `Main thread folding results …` | Share of the time spent adding results to the tree. Near 100 % means folding is the limit, and more threads won't help |
| `… batches` | How many batches of folders were listed |
| `Held by the disk-load limit …` | Only with `--disk-limit`: time spent waiting for the limit to allow another batch |
| `Bulk listing (getattrlistbulk): …` | macOS only: whether files are listed in bulk (`on`, `off: <reason>` or `untested`) |

The app writes the same numbers to its log file after every full scan; [Scan speed and disk load](../performance/#the-scan-profile-in-the-log) explains how to read them.

## run: scripted mode

```text
spaceknife-backend run <command> [<command> …]
```

`run` executes a sequence of commands in order, without opening a window. `scan` and `load` each open a **view**; `filter`, `save` and `export` act on the view opened last. The whole sequence is checked before anything runs: an unknown command, a missing parameter, or a `filter`, `save` or `export` with no `scan` or `load` before it stops everything, and nothing runs.

| Command | What it does |
|---|---|
| `help` | Show the built-in help (`run` with no commands does the same) |
| `scan "<path>"` | Scan a folder or drive and open a view. Separate several paths with `;` for one view with several roots. A path that is itself an existing folder is never split, so folder names may contain `;` |
| `load "<snapshot>"` | Open a SpaceKnife snapshot (`.sks`) as a view; one snapshot per `load` |
| `filter "<expression>"` | Filter the view, with the same syntax as the filter box ([Filters](../filtering/)). Errors are listed with their position and stop the run; warnings are shown and the run goes on |
| `save "<file>"` | Save the view's full result as a snapshot. The filter doesn't apply: the whole tree is saved |
| `export "<template>" "<file>"` | Export the view to a file, with only what the filter left. `<template>` is a built-in template name, `csv`, `json`, `html`, or the path of a template file |
| `autoclose` | Don't print the view summaries at the end |

Built-in templates (the name is not case-sensitive):

| Name | Content |
|---|---|
| `Grouped by folder` | One section per folder that directly contains files: its full path and size on disk, followed by its files |
| `First level folders` | Each folder on the first level below the root: full path and size on disk |
| `Simple file list` | The full path of each file, one per line |
| `Summary` | Total size on disk, number of folders and number of files |
| `Folder tree` | Folders and their size on disk, indented by level |

What happens when it runs:

- Scans in `run` use up to 4 listing threads, have no disk-load limit, and don't save snapshots by themselves; use `save` for that.
- Progress and messages go to standard error. Unless you add `autoclose`, a summary of every view goes to standard output at the end: the root, its 10 largest items with their share, and the totals.
- `export` writes the file in one go (a temporary file, then a rename), so a reader never sees half a file. The target folder must exist. A template file must be UTF-8 and at most 1 MB; if it has errors, nothing is exported and each error is listed with its line and column. The formats and the template language are described in [Export and reports](../export/).
- Exit codes: `0` when every command ran, `2` when the command line has errors or a command failed (the run stops at that command), `130` when interrupted.

### Examples

These are written for Command Prompt on Windows and for POSIX shells (bash, zsh) on Linux and macOS.

```bat
spaceknife-backend run scan "D:\Photos" filter "*.jpg;>5mb" export "Simple file list" big.txt autoclose
spaceknife-backend run scan "C:\" save "D:\snapshots\c.sks" autoclose
spaceknife-backend run load "D:\snapshots\c.sks" filter "\node_modules" export html node_modules.html
spaceknife-backend run scan "C:\;D:\" export Summary totals.txt autoclose
```

```bash
spaceknife-backend run scan /home/lenny filter '>1gb' export csv big-files.csv autoclose
spaceknife-backend run load ~/c.sks filter '\node_modules' export html node_modules.html
```

The first Windows example finds JPEG files over 5 MB in `D:\Photos` and writes their paths to `big.txt`. The second scans the whole `C:` drive into a snapshot that the app can open or compare later. The third opens that snapshot again, without rescanning, and writes an HTML report of every `node_modules` folder.

### Parameters and delimiters

Parameters are separated by white space. A parameter that starts with the opening delimiter (`"` by default) runs to the closing delimiter, and there is no escape character. So `"C:\"` is simply `C:\`: the backslash before the closing quote is not an escape. On Windows, SpaceKnife reads the raw command line to make this work. On Linux and macOS the shell has already split the arguments and removed its quotes, and the same rule is applied to the arguments it passes.

Two meta commands change how the rest of the line is read. Each must be a single word without delimiters:

| Meta command | What it does |
|---|---|
| `@delim=X` | From here on, parameters are delimited by `X` instead of `"` |
| `@delim=XY` | From here on, parameters start with `X` and end with `Y`, for example `@delim={}` |
| `@parser=OS` | Read the whole line again from the start, using the arguments exactly as the system split them. No `@delim=` is accepted after it |

You need another delimiter when a parameter itself contains `"`, for example a filter that matches the literal name `a;b.jpg`:

```bat
spaceknife-backend run @delim={} scan {D:\Photos} filter {"a;b.jpg";>1mb} export {Grouped by folder} {D:\out\files.txt}
```

```bash
spaceknife-backend run @delim={} scan /home/lenny/Photos filter '{"a;b.jpg";>1mb}' export 'Grouped by folder' files.txt
```

In a POSIX shell, quote the filter with single quotes so the shell leaves `;`, `>` and `\` alone. Without `@delim={}`, the argument `"a;b.jpg";>1mb` would start with the default delimiter and end at the second `"`, and the rest would be read as an unknown command.

:::caution[PowerShell]
PowerShell gives `{ }` and `@` meanings of its own. Run command lines that use `@delim=` from Command Prompt.
:::

## snapshot

```bash
spaceknife-backend snapshot list
spaceknife-backend snapshot show "D:\snapshots\c.sks" --top 10 --depth 3
```

`snapshot list` shows every snapshot in the snapshot folder with its time, what was scanned, size on disk, number of items and file name, and then the folder itself. `snapshot show` prints when a snapshot was saved and the largest items in it; `--top` and `--depth` work as for `scan`.

## version

```bash
spaceknife-backend version
```

Prints the backend version, the protocol version, a hash of the protocol schema, and the version of the native extension. `native none` means the native extension didn't load: scans then use the Python walker and are several times slower. Include this line in bug reports.

## serve

The app starts the backend with `serve` itself; you only need it to connect your own client or to debug. It prints one JSON line on standard output once it listens (`{"event": "ready", "url": "ws://127.0.0.1:…/v1", …}`) and logs to standard error.

| Option | Default | What it does |
|---|---|---|
| `--host` | `127.0.0.1` | Interface to listen on. Keep the loopback address unless you know why not |
| `--port` | `0` | TCP port; `0` picks a free one, reported in the ready line |
| `--token-stdin` | off | Read the session token from the first line of standard input, and exit when standard input closes. Without it a token is generated and printed in the ready line |
| `--allow-origin` | — | Another allowed WebSocket origin, for example a development server; can be repeated |
| `--hello-timeout` | 5 | Seconds a client has to introduce itself |
| `--log-level` | `INFO` | `TRACE`, `DEBUG`, `INFO`, `WARNING` or `ERROR` |
| `--log-format` | `rich` | `rich`, `plain` or `json`, on standard error |
| `--ready-file` | — | Also write the ready line, with the token, to this file (readable only by you; removed on exit) |

The messages are described in the [protocol document](https://github.com/DuskXi/SpaceKnife/blob/main/docs/protocol.md) (in Chinese).

## schema

`schema --output FILE` writes the protocol's JSON Schema (standard output without `--output`). `schema --check FILE` exits with `1` when the file differs from the current schema; the project's CI uses it.

## Environment variables

Set these before you start the app or the command line. The app passes its environment on to the backend.

| Variable | What it does |
|---|---|
| `SPACEKNIFE_HOME` | Keep everything SpaceKnife writes in one folder: `data` (snapshots, tags, logs), `config` (settings, file classes), `cache`, and for the app also `electron` (its browser data). Useful for a second, separate setup, for tests, and for portable copies. See [Data locations and portable mode](../data-locations/) |
| `SPACEKNIFE_SCAN_THREADS` | Number of listing threads, 1 to 64. Default: one per CPU, between 2 and 16; on macOS twice the CPUs, between 4 and 16. It also caps the manual concurrency |
| `SPACEKNIFE_LANGUAGE` | `en` or `zh-CN`: messages of the command line in this language, whatever the settings say. Handy for scripts and bug reports |
| `SPACEKNIFE_NO_NATIVE=1` | Don't use the native extension: the Python walker lists the folders, several times slower. For comparisons, and to rule out the native code when you track down a bug |
| `SPACEKNIFE_NO_BULK=1` | macOS: list files with `readdir` and `fstatat` instead of `getattrlistbulk`, to compare the speed |
| `SPACEKNIFE_MEMTRACE=1` | When the memory watchdog warns, the log file also lists the code lines holding the most memory. Makes the backend slower; use it only to investigate a memory warning |
