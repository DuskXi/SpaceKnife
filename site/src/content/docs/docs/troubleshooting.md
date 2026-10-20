---
title: Troubleshooting
description: Fixes for first-launch warnings, permissions, unknown space, slow or stuck scans and memory warnings, where the logs are, and how to report a bug.
---

Start with the section that matches what you see. If nothing here helps, [report a bug](#reporting-a-bug) with the information from the first section.

## Collecting information

- **Log panel**: <kbd>Ctrl</kbd>+<kbd>J</kbd> (<kbd>⌘</kbd>+<kbd>J</kbd> on macOS), or click the left end of the status bar. **Log** lists this session's events: denied folders, fallbacks, failed operations. Click the `N warnings` link to show only warnings and errors. **Scan statistics** shows the current tab's scan: status, scan method, start, end, duration, files and folders, total scanned, unknown space, folders that couldn't be accessed and the average throughput.
- **Log files**: `spaceknife.log` in the `logs` folder of SpaceKnife's data folder. It starts a new file at 10 MB and keeps the 5 previous ones.

  | System | Log folder |
  |---|---|
  | Windows | `%LOCALAPPDATA%\SpaceKnife\logs` |
  | Linux | `~/.local/share/SpaceKnife/logs` |
  | macOS | `~/Library/Application Support/SpaceKnife/logs` |

  **Settings › Advanced › Files & diagnostics › Log folder › Open** opens the folder, wherever it is (a portable copy or `SPACEKNIFE_HOME` moves it; see [Data locations and portable mode](../data-locations/)).
- **More detail**: set **Settings › Advanced › Log › Log file level** to **Debug**, reproduce the problem, then set it back to **Info**.
- **Copy diagnostics** (**Settings › Advanced › Files & diagnostics**): copies the version of the interface and the backend, the platform, the backend's capabilities, and the paths of the settings file and the log folder.
- **The command line**: `spaceknife-backend version` prints the versions; `native none` there means the native extension didn't load (see [Command line](../command-line/#version)).

## First launch

### Windows: SmartScreen warns about an unknown publisher

The installer and the app aren't code-signed yet, so SmartScreen shows a warning the first time. Click **More info**, then **Run anyway**. Every release has a `SHA256SUMS.txt` to check your download against.

### Windows portable: slow first start, or nothing is saved

- The first start after unzipping takes a few seconds longer while Windows checks the new files.
- Unzip to a folder you can write to. In a place such as `C:\Program Files`, the portable copy can't write its `data` folder.

### macOS: "SpaceKnife can't be verified"

The macOS build isn't notarized by Apple yet, so macOS blocks it once:

1. Open SpaceKnife. When macOS says it could not verify it, click **Done**.
2. Open **System Settings › Privacy & Security**, scroll down to the message that SpaceKnife was blocked, and click **Open Anyway**. Click **Open Anyway** again and enter your password.

From then on it opens normally. After you install a new version, allow it once more. On macOS 14 you can also right-click SpaceKnife in Applications and choose **Open**. In a terminal, this does the same:

```bash
xattr -dr com.apple.quarantine /Applications/SpaceKnife.app
```

### Linux: the AppImage doesn't start

- The AppImage needs FUSE 2 (`libfuse2` on Ubuntu 22.04). Without it, run it as `./SpaceKnife-<version>-x86_64.AppImage --appimage-extract-and-run`, or use the deb, rpm or portable tar.gz, which don't need FUSE.
- On distributions that restrict unprivileged user namespaces (the AppArmor rules of Ubuntu 23.10 and later), the AppImage and the portable tar.gz may not start because their Chromium sandbox helper isn't setuid. Start them with `--no-sandbox`. Ubuntu 22.04 and RHEL 9 aren't affected.

### "The backend didn't start"

The window is there, but a message covers it, and the status bar says `Backend unavailable`. The message under the title says why: `The backend wasn't ready within 30 seconds.`, `The backend failed to start (exit code N).` or `The backend exited (exit code N).`

- Restart SpaceKnife. If it happens again, run the backend by itself in a terminal: `spaceknife-backend version` (where it is: [Command line](../command-line/#where-the-program-is)). An error there is what to report.
- On Linux and macOS, starting SpaceKnife from a terminal shows the backend's own messages, prefixed with `[backend]`.

## Permissions

### Windows: the administrator prompt

The MFT quick scan needs administrator rights. SpaceKnife never runs elevated itself: it starts a small read-only scan helper, and only the helper asks (a UAC prompt). Details in [Scan speed and disk load](../performance/#the-administrator-prompt).

- **You declined**: the scan goes on as a full scan, folder by folder. **Scan details** shows the reason.
- **It asks again later**: an idle helper quits after 10 minutes, and the next quick scan starts a new one.
- **You never want the prompt**: switch off **Settings › Scan & performance › Scan method › Ask for administrator rights when needed**, or set **Default scan method** to **Full scan**.
- **No prompt appears and the scan waits** at `Waiting for administrator approval…`: look for the UAC prompt in the taskbar. **Stop** cancels the scan.
- With an install for the current user, the helper starts from a folder your account can write to. If that concerns you, install SpaceKnife for all users (in Program Files).

### macOS: Full Disk Access

Without Full Disk Access, macOS asks separately before the Desktop, Documents, Downloads, iCloud Drive and other cloud drives, the Photos library and other apps' data can be read, so a scan brings up one prompt after another. Places such as Mail and Messages can't be read at all and count as unknown space.

Before such a scan SpaceKnife shows **Let SpaceKnife read every file at once**:

- **Open System Settings**: switch SpaceKnife on in the Full Disk Access list (if it isn't there, add it with **+**), then reopen SpaceKnife when asked.
- **Skip the protected places**: no prompts; those places show as inaccessible.
- **Scan and ask one by one**: let macOS ask for each place.

Tick **Remember "skip" or "ask one by one"** to stop the question; change the answer later in **Settings › Scan & performance › Scan scope › Protected places** (**Ask before scanning**, **Skip them, no prompts**, **Access them**).

:::caution[After every update]
The permission belongs to the exact build. After installing a new version, switch SpaceKnife off and on again in the Full Disk Access list.
:::

### Access denied and unknown space

**Unknown space** is the used space the system reports minus everything the scan found. It is shown only when you scan a whole volume, as a grey striped block; <kbd>U</kbd> shows or hides it. It is usually made of folders the scan wasn't allowed to read, and areas the system reserves for itself.

- Folders that couldn't be read are listed in **Scan details** (`N folders couldn't be accessed`, with an example) and in the log panel (`Can't access …: Access denied; counted as unknown space`).
- **Windows**: an MFT quick scan reads the whole volume, so it leaves much less unknown space than a full scan.
- **macOS**: give SpaceKnife Full Disk Access (above).
- **Linux**: SpaceKnife scans with your own rights. For a look into system folders, scan with `sudo` from the [command line](../command-line/#recipes).
- Too many warnings in the log? Switch off **Settings › General › Log › Log scan warnings**: denied folders are still counted, just not listed. **Open the log panel on new events** controls whether the panel opens by itself.

## A scan seems stuck or slow

Check these in order:

1. **Paused?** The status bar says `Paused`, and the progress band `Disk reads paused, resume any time`. Click **Resume**.
2. **Giving way?** **Scan details** says `Other programs are reading or writing a lot on this disk, so the scan is yielding (minimum progress).` Another program is using the disk heavily. Wait, or choose **Full speed** under **Disk load limit**, which applies at once. Switching off **Give way when the disk is busy** takes effect from the next scan.
3. **Idle level?** At **Idle** the scan reads only while the disk is otherwise quiet. On a busy disk choose another level; the change applies at once.
4. **Waiting for a prompt?** On Windows an administrator prompt (`Waiting for administrator approval…`); on macOS a permission prompt for a protected folder. Answer it.
5. **A slow location?** Network drives, hard disks, USB drives and memory cards are read with few batches at once, and Windows drives seen from WSL are very slow. See [Scan speed and disk load](../performance/#network-drives-and-other-slow-locations).
6. **The percentage shows `…` near the end?** Without an earlier snapshot of the location, progress is measured against the volume's used space and stops counting at 95 %, because that total is only approximate. The scan is still running; the item count keeps growing.

To see what a scan waits for, read the scan profile line in the log file after the scan ([how to read it](../performance/#the-scan-profile-in-the-log)). A full scan you stopped can be continued later with **Continue scan** in the status bar: it reads only the folders it hadn't read yet.

## Memory warning

The log panel shows `Backend memory use is abnormal: N GB (scan data about N GB); diagnostics were written to the log file`.

Every 15 seconds the backend compares its memory with what the open scans need. It warns only when it uses well over that (twice the scan data plus 1.5 GB, plus room for the largest scan closed in the last hour) even after handing freed memory back to the system. How much a scan needs is in [Scan speed and disk load](../performance/#memory-and-very-large-disks).

- Close the scans and tabs you don't need.
- If the warning comes back with few or no scans open, report it. The log file then contains a block after `memory above what the scans account for:` that lists every scan's data, the views and the queues; attach it.
- To add the code lines that hold the most memory, start SpaceKnife with the environment variable `SPACEKNIFE_MEMTRACE=1`, reproduce, and attach the new block. It makes SpaceKnife slower, so remove it afterwards.

## Resetting settings

- **One settings page**: **Restore defaults** at the bottom of the page. **Undo** stays available for a few seconds.
- **Everything**: **Settings › Advanced › Import & export › Restore all defaults…** resets every setting and the file classes, after a confirmation.
- **Keep a copy first**: **Export settings** on the same page saves every changed setting and your custom file classes to one file; **Import settings** brings them back and lists what will change before it applies.
- **By hand**: quit SpaceKnife and delete or rename the settings file. It holds only the settings you changed. **Open settings file** at the bottom of the settings list opens it.

  | System | Settings file |
  |---|---|
  | Windows | `%LOCALAPPDATA%\SpaceKnife\settings.json` |
  | Linux | `~/.config/SpaceKnife/settings.json` |
  | macOS | `~/Library/Application Support/SpaceKnife/settings.json` |

- A damaged settings file doesn't stop SpaceKnife: the settings show `The settings file is invalid, so the last valid settings stay in use: …`, and the next change you make writes a clean file.
- **Clear cache…** (**Settings › Advanced › Scan helper & cache**) removes cached data; anything in use is kept.
- **A clean start without touching your data**: start SpaceKnife with `SPACEKNIFE_HOME` set to an empty folder. It then uses only that folder; remove the variable to return to your own data.

## Reporting a bug

Open a [bug report](https://github.com/DuskXi/SpaceKnife/issues/new?template=bug_report.yml) on GitHub after searching the [existing issues](https://github.com/DuskXi/SpaceKnife/issues). Questions about how something works go to [Discussions › Q&A](https://github.com/DuskXi/SpaceKnife/discussions/categories/q-a).

Include:

- The output of **Copy diagnostics**, or at least the version, your operating system and how you installed SpaceKnife.
- What you did, what you expected and what happened instead; the exact steps if you can repeat it.
- The relevant part of `spaceknife.log`, ideally with the log level set to **Debug** while you reproduce it.
- For a slow scan: the `scan done` and `scan profile` lines. For a memory warning: the block described above.

:::caution[Before you paste]
Logs contain file paths, and paths can contain your name or other private details. Replace them before you post.
:::

Security problems must not go into a public issue. Report them privately, as described in [SECURITY.md](https://github.com/DuskXi/SpaceKnife/blob/main/SECURITY.md).
