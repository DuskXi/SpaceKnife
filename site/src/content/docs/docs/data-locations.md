---
title: Data locations and portable mode
description: Where SpaceKnife keeps snapshots, settings, logs and cache on each platform, how SPACEKNIFE_HOME moves all of it, and how portable mode works.
---

A scan only reads the disk. What SpaceKnife keeps lives in a few per-user folders, or, for a portable copy, in the `data` folder next to the program.

## Where things are

| | Windows | Linux | macOS |
|---|---|---|---|
| Data: snapshots, tags, logs | `%LOCALAPPDATA%\SpaceKnife` | `~/.local/share/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| Configuration: settings, file classes | `%LOCALAPPDATA%\SpaceKnife` | `~/.config/SpaceKnife` | `~/Library/Application Support/SpaceKnife` |
| Cache | `%LOCALAPPDATA%\SpaceKnife\Cache` | `~/.cache/SpaceKnife` | `~/Library/Caches/SpaceKnife` |
| Interface profile (the window's storage and caches) | `%APPDATA%\SpaceKnife\electron` | `~/.config/SpaceKnife/electron` | `~/Library/Application Support/SpaceKnife/electron` |

On Linux, the folders follow `XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` when you set them.

You don't have to look them up: **Settings › Advanced** shows them.

- **Files & diagnostics**: **Settings file** and **Log folder**, each with **Open**.
- **Scan helper & cache**: **Snapshots** (count, total size and folder, with **Open**) and **Cache** (size and folder, with **Clear cache…**; cache in use is kept).
- **Open settings file** at the bottom of the settings list, and **Open snapshot folder** in the **…** menu of **Recent** on the start page.

## What is in them

| File or folder | In | What it holds |
|---|---|---|
| `snapshots/` | data | One folder per scanned location, one `.sks` file per snapshot |
| `tags/` | data | Saved tags, one file per scanned location |
| `logs/spaceknife.log` | data | The log file |
| `update.json` | data | The last update check, the recent releases with their notes, and the versions whose update card you closed |
| `updates/` | data | A downloaded new version until it runs, and while a portable copy is replaced, its new and old program files. Emptied at the next start ([Nothing left behind](../updates-privacy/#nothing-left-behind)) |
| `palette_recent.json` | data | Recent jumps and filters of the command palette |
| `volume_summaries.json` | data | What the Disks page kept of each volume's last complete scan (file classes, largest folders) |
| `settings.json` | configuration | Your settings |
| `classes.json` | configuration | Your file classes |
| `columns/` | cache | Scan data moved out of memory for very large scans |

SpaceKnife's own folders, like the folder it is installed in, are protected: you can't delete or move them from inside SpaceKnife.

### Snapshots

Automatic snapshots are saved as `snapshots/<location>-<id>/<date>-<time>.sks`, for example `snapshots/Videos-3f2a9c41d0be/20261020-081530.sks`. The date and time are in UTC.

For each location SpaceKnife keeps the newest 10 (**Settings › Advanced › Recent snapshots to keep**, 1–100), and of the older ones the newest of each week; the rest are deleted after each automatic save.

:::tip[Keep a snapshot for good]
Files whose names aren't a date and time are never deleted by that cleanup: rename a snapshot in its folder (say, `before-cleanup.sks`) to keep it. A renamed file still counts toward the newest 10. Or copy it anywhere else and open it later with **Open snapshot file…**.
:::

### Settings

`settings.json` holds your settings. You can edit it by hand while SpaceKnife runs: it checks the file every second and applies your change. If the file isn't valid, SpaceKnife says so at the top of the settings page and keeps the last valid settings. To move settings between computers, use **Settings › Advanced › Export settings** and **Import settings**: the import lists what would change and applies nothing until you confirm. See [Settings](../settings/).

### Logs

`logs/spaceknife.log` grows to 10 MB, then starts a new file; the 5 newest old files are kept. **Settings › Advanced › Log file level** chooses **Info** or **Debug**. The log panel (<kbd>Ctrl</kbd>+<kbd>J</kbd>) shows the events of this session.

For a bug report, **Settings › Advanced › Copy diagnostics** puts the versions, platform, capabilities and the paths of the settings file and log folder on the clipboard. Nothing is sent anywhere.

### Cache

Scans of more than about two million items (**Settings › Advanced › Keep large scans in mapped files**, on by default), and scan data beyond **Settings › Advanced › Scan data memory limit** (25% of physical memory by default), go to memory-mapped files in the cache instead of private memory. These files remove themselves when they are no longer needed, and anything a crash leaves behind is removed at the next start. **Clear cache…** removes what isn't in use.

## SPACEKNIFE_HOME

Set the environment variable `SPACEKNIFE_HOME` to a folder, and SpaceKnife keeps everything there instead of the per-user folders:

| Folder | Holds |
|---|---|
| `SPACEKNIFE_HOME/data` | Snapshots, tags, logs, update state |
| `SPACEKNIFE_HOME/config` | Settings, file classes |
| `SPACEKNIFE_HOME/cache` | Cache |
| `SPACEKNIFE_HOME/electron` | Interface profile |

Use it for a separate profile (to try settings without touching your own), to keep snapshots on another drive, or for demos and tests. Give an absolute path.

Windows, for one start from PowerShell:

```powershell
$env:SPACEKNIFE_HOME = "D:\SpaceKnife-data"
& "<install folder>\SpaceKnife.exe"
```

Windows, for every program started afterwards (to undo it, delete the variable in the Windows environment variable settings):

```powershell
setx SPACEKNIFE_HOME "D:\SpaceKnife-data"
```

Linux:

```bash
SPACEKNIFE_HOME=/home/lenny/spaceknife-data spaceknife
```

macOS:

```bash
SPACEKNIFE_HOME=/Users/lenny/spaceknife-data /Applications/SpaceKnife.app/Contents/MacOS/SpaceKnife
```

The [command line](../command-line/) reads the same variable, so `spaceknife-backend snapshot list` with `SPACEKNIFE_HOME` set lists the snapshots in that folder.

## Portable mode

The portable archives (`SpaceKnife-<version>-win-x64-portable.zip` and `SpaceKnife-<version>-linux-x64-portable.tar.gz`) have an empty `data` folder next to the program, with a short README in it. Each time SpaceKnife starts on Windows or Linux, it looks for that folder: if there is a `data` folder next to the executable, it is used exactly like `SPACEKNIFE_HOME`. Snapshots, settings, logs, cache and the interface profile all stay in it, and nothing is written to your user profile.

- An explicitly set `SPACEKNIFE_HOME` wins over the `data` folder.
- Delete or rename `data` and the copy uses the normal per-user folders. Put it back and it is portable again.
- Unpack the archive somewhere you can write: a folder under Program Files, for example, doesn't work.
- The in-app update knows it is a portable copy: it downloads the portable archive, and **Seamless update** replaces the program files and keeps `data`; **Replace it yourself** shows the archive so that you unpack it over the old files (see [Upgrading](../install/#upgrading)).
- The macOS app and the AppImage have no portable mode. On macOS, use `SPACEKNIFE_HOME` instead.

:::caution[The command line and portable mode]
Only the app looks for the `data` folder; the command line inside a portable copy doesn't. To work with a portable copy's snapshots from a terminal, point `SPACEKNIFE_HOME` at its `data` folder, as in the example below.
:::

```powershell
$env:SPACEKNIFE_HOME = "C:\Users\lenny\Apps\SpaceKnife-portable\data"
& "C:\Users\lenny\Apps\SpaceKnife-portable\resources\backend\spaceknife-backend.exe" snapshot list
```

### Moving your data

To take your settings into a portable copy (or out of one), export them in one copy and import them in the other (**Settings › Advanced › Export settings** / **Import settings**). Snapshots are plain files: copy the `.sks` files you want and open them with **Open snapshot file…**, or quit SpaceKnife and copy the whole `snapshots` folder into the other copy's data folder (`data/data/snapshots` in a portable copy).

## Removing all data

The Windows uninstaller does it for you: tick **Also delete the snapshots, settings and logs** on its second page ([Uninstalling](../install/#uninstalling)). Otherwise, to start fresh or to remove everything after uninstalling, quit SpaceKnife and delete the folders in the [first table](#where-things-are) (on Windows: `%LOCALAPPDATA%\SpaceKnife` and `%APPDATA%\SpaceKnife`), or the `SPACEKNIFE_HOME` folder or `data` folder you use. **Settings › Advanced › Restore all defaults** resets only the settings and file classes.
