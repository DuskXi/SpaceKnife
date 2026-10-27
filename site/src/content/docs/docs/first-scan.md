---
title: Your first scan
description: Pick a volume or folders on the start page, choose the scan method and the disk load limit, read the progress, and pause, stop or continue a scan.
---

Every new tab opens on the start page. Pick what to scan, check the two or three options, and press <kbd>Enter</kbd>. The view fills in while the scan runs, and you can zoom and look around before it is done.

:::tip[On macOS]
<kbd>⌘</kbd> works wherever this page says <kbd>Ctrl</kbd> (<kbd>Ctrl</kbd> itself works too). All keys are listed in [Keyboard shortcuts](../shortcuts/).
:::

## The start page

The left column is **Choose what to scan**: the volumes, a path box and the scan options, with **Start scan** at the bottom. The right column starts with the **[Disks](../disks/)** card (your disks at a glance; click it for the whole page), then **Recent**: your earlier scans and snapshot files.

### Volumes

Each volume has a card: its name (the drive letter and label on Windows, the mount point on Linux and macOS), file system, media type and capacity, a usage bar with the used and free space, and a badge that says how it will be scanned:

| Badge | Meaning |
|---|---|
| **MFT quick scan** | A whole local NTFS volume on Windows: read through the master file table, in seconds. |
| **MFT quick scan · needs admin rights** | The same, but Windows will ask for administrator rights first. |
| **Parallel full scan** | Walks the folders in parallel. |
| **Parallel full scan · throttled for USB HDD** | A hard drive on USB: the scan reads fewer folders at a time, because seeking slows a hard drive down. |
| **Network scan · slower** | A network drive. |
| **Not recommended · very slow** | A Windows drive seen from WSL over 9P. Hover for the reason. |

A volume that can't be scanned right now (no media, network volume not connected, unreadable) stays in the list, greyed out with the reason. The list updates itself every few seconds as drives come and go; **Refresh** reads it again at once. The system volume is selected when the page opens.

With the keyboard: <kbd>Tab</kbd> into the cards, then the arrow keys move the selection (<kbd>←</kbd> <kbd>→</kbd> by one card, <kbd>↑</kbd> <kbd>↓</kbd> by two), and <kbd>Enter</kbd> starts the scan.

### Paths

Under the volumes, **Or enter a path (separate several with ;)** takes one or more folders:

```text
C:\Users\lenny\Videos; D:\Downloads
```

```text
~/Videos; ~/Downloads
```

- `~` stands for your home folder. A path that is exactly a volume root (`D:\`, `/`) selects that volume's card instead.
- The paths are checked shortly after you stop typing. One that doesn't exist, isn't a folder or can't be opened is named under the box with the reason, and **Start scan** stays disabled.
- A path inside another one you entered is merged into it ("… is inside …. They will be merged into one scan.").
- Several paths are scanned together, as side-by-side roots of one view.
- <kbd>Enter</kbd> in the path box starts the scan.

**Browse…** opens the system folder picker, where you can select several folders; they are added to the end of the box. You can also drag folders from your file manager anywhere onto the window: they fill the path box (the scan doesn't start by itself). Drop a SpaceKnife snapshot file (`.sks`) to open it; other files are refused.

### Scan options

The options below the path apply to this scan only. Their defaults come from **Settings › Scan & performance**, and changing them here doesn't change the settings.

| Option | Shown for | Choices |
|---|---|---|
| **Scan method** | A whole local NTFS volume on Windows | **Auto**, **Quick MFT**, **Full scan** ([details](#scan-methods)) |
| **Disk load limit** | Everything | **Full speed**, **Balanced 70%**, **Quiet 35%**, **Idle** ([details](#disk-load-limit)) |
| **Scan NTFS alternate data streams** | A whole local NTFS volume on Windows | Counts the hidden data streams attached to files, in MFT quick scans |

The bar at the bottom sums up what will happen, for example `C: · Auto (MFT quick scan) · Disk load limit: Balanced ≤70%`. When the MFT quick scan needs administrator rights, it adds: "Needs administrator rights: only the read-only scan helper is elevated, never the app itself."

### Recent scans and snapshots

Every finished scan saves a snapshot (unless you turned that off), and **Recent** lists the newest one of each location with its time: for a volume its size, scan method and file count, for folders their paths and size. For each:

- **Open** shows the snapshot at once, without scanning. It is a past state: it isn't monitored for changes, and file actions only work on items that still exist.
- **Compare with now** compares the snapshot with the same location as it is now, scanning it first if needed. See [Snapshots and comparison](../snapshots-compare/).

**Show all scans** lists every snapshot, not just the newest per location. The **…** menu has **Refresh list** and **Open snapshot folder**. Below the list, drop a snapshot file onto **Drop a snapshot file here**, or use **Open snapshot file…**.

## Scan methods

SpaceKnife has two ways to read a disk:

| Method | How it works | Where |
|---|---|---|
| **MFT quick scan** | Reads the NTFS master file table (MFT) in one go, through a read-only scan helper that runs with administrator rights. A whole disk takes seconds. | A whole local NTFS volume on Windows |
| **Full scan** | Walks every folder, many at a time. The time grows with the number of files. | Everywhere: folders, other file systems, network drives, Linux and macOS |

With a whole NTFS volume selected, **Auto** and **Quick MFT** both use the MFT quick scan, and **Full scan** walks the folders instead. For anything else the method row is hidden: a full scan is the only way.

### Administrator rights

Only the scan helper is elevated, never SpaceKnife itself. Starting it brings up the Windows administrator prompt once; the scan shows "Waiting for administrator approval…" meanwhile.

- **Approved**: the MFT quick scan runs.
- **Declined**: the scan carries on as a full scan, and Scan details says why. You can switch over later (see [Switching to the MFT quick scan](#switching-to-the-mft-quick-scan)).
- **No prompt at all** when SpaceKnife already runs as administrator, or when the helper is still running from an earlier quick scan. The helper exits after 10 minutes without work; the next quick scan asks again.

**Settings › Scan & performance › Ask for administrator rights when needed** turns the prompt off: without administrator rights, scans are then full scans. **Settings › Advanced › Scan helper** shows whether the helper is running and has **Stop scan helper**.

### What the MFT quick scan sees

Because it reads the file table itself, an MFT quick scan differs from a full scan in a few places:

- It sees folders a full scan isn't allowed to open, such as `System Volume Information`.
- The file system's own files are grouped in a folder `$NTFS metadata`, and entries whose parent folder is missing in `$Orphaned files`. These have no location on disk you could open or delete.
- Cloud placeholder files keep the space the MFT records for them (a full scan counts them as 0).
- Alternate data streams are counted only when **Scan NTFS alternate data streams** is on (a full scan never reads them).

It runs in phases: **Waiting for administrator approval…**, **Reading the MFT (master file table)…**, **Receiving scan results…**, **Building the folder tree…**. The folder tree is built once reading is done, so the view appears at the end, while a full scan draws as it goes.

### Switching to the MFT quick scan

While a full scan of a whole NTFS volume runs, **Scan details** offers **Use MFT quick scan as administrator** ("Done in about … s · starts only the read-only scan helper"). The full scan goes on while the helper starts; once the helper is ready, its result replaces the tree. If you decline the prompt, the full scan simply continues.

### How a full scan counts

- It stays on one volume: folders where another volume is mounted are not entered, unless you turn on **Settings › Scan & performance › Cross mount points**.
- It doesn't follow symbolic links.
- A file with several hard links counts once (**Settings › Scan & performance › Hard links**).
- On macOS, scanning `/` doesn't go into `/System/Volumes/Data`: its files are already in the tree through the system's firmlinks. Files that are only in the cloud count as 0, and the scan never makes them download.

## Disk load limit

A scan can read a disk hard enough to slow down everything else. The limit caps the read load SpaceKnife itself causes:

| Level | What it does |
|---|---|
| **Full speed** | No limit. |
| **Balanced 70%** | At most 70% read load, leaving room for the apps you are using. The default. |
| **Quiet 35%** | At most 35%. You'll hardly notice the scan. |
| **Idle** | Reads only while the disk is idle, at the system's low I/O priority. |
| **Custom** | Your own percentage, 5–100%. |

- **Load** is worked out from the disk's request queue. Task Manager's **Active time** is shown next to it for reference only: a few parallel reads take an NVMe SSD to 100% active time long before it is busy.
- With **Give way when the disk is busy** on (the default), the scan drops to a minimum while other programs push the whole disk past the limit. Scan details then says "Other programs are reading or writing a lot on this disk, so the scan is yielding (minimum progress)."
- An MFT quick scan is one sequential read: the level slows its pace instead of limiting parallel reads.

The start page offers the first four levels. You can change the level while a scan runs, in **Scan details › Disk load limit** (the arrow keys move between levels, **Custom** brings a slider); it applies within half a second. During a scan, clicking the disk load item in the status bar opens that panel.

The defaults live in **Settings › Scan & performance**: **Default for manual scans** (Balanced), **Custom limit**, **Use low I/O priority when idle** and the concurrency settings. More on tuning: [Performance](../performance/).

## Following the progress

The band above the view shows:

- the percentage (at most 99% until the scan is done; `…` when it can't be estimated) and a progress bar;
- **Scanning** and the target name;
- for a whole volume, "… scanned of … used"; for a folder, "… scanned", with "(last scan: … items)" when it was scanned before;
- the item count, items per second, and the time left (**Estimating…** at first);
- **Current** and the folder being read.

When the location was scanned before, progress counts items against the last scan. Otherwise a whole volume compares scanned space with the volume's used space, up to 95%; a folder scanned for the first time shows no percentage, and the time left stays at **Estimating…**.

You don't have to wait: <kbd>Enter</kbd> zooms into the selected block, <kbd>Backspace</kbd> goes back, <kbd>Ctrl</kbd>+<kbd>Home</kbd> returns to the root. See [Navigation](../navigation/).

### Scan details

While the scan runs and nothing is selected, the side panel shows **Scan details**. Selecting a block shows that block's details instead; <kbd>Esc</kbd> clears the selection and brings Scan details back. **Hide sidebar** and **Show sidebar** close and reopen the panel.

- **Scan method**: the method used, and why a full scan when it is one ("Full scan (no administrator rights)", "Full scan (the target isn't a whole local NTFS volume)").
- **Disk load limit**: the levels, and **Current** (SpaceKnife's own load over the last second), **Whole disk** and **Active time**.
- **Disk read load · last 60 s**: SpaceKnife's load as a line, the limit dashed, the whole disk as an area, active time, and where the scan started.
- **Disk reads**: the whole disk's read speed and average latency (other programs included).
- **Concurrency** and **In flight**, with **Auto**: switch it off to set the number of parallel reads by hand (1–64).
- "… folders couldn't be accessed", with **View log** and an example path. Those folders are counted as unknown space.

### Status bar and log

The status bar shows the scan state and item count, **Live monitoring**, the disk load and the volume's free space. Its first item opens the log panel (<kbd>Ctrl</kbd>+<kbd>J</kbd>): **Log** lists the session's events, such as folders that couldn't be read, and **Scan statistics** has the method, status, start, finish, duration, files and folders, total scanned, unknown space, access denied and average throughput.

The tab title also says **Scanning**, **Paused**, **Incomplete** or **Scan failed**.

## Pause, stop and continue

| Action | Where | What happens |
|---|---|---|
| **Pause** | The progress band, or **Pause scan** in the [command palette](../command-palette/) | No new reads start; SpaceKnife's disk reads drop to zero within about a second. Full scans only: an MFT quick scan is over in seconds. |
| **Resume** | The progress band, or **Resume scan** | Goes on exactly where it paused. |
| **Stop** | The progress band, or **Stop scan** | Ends the scan and keeps what was read ("Stopped (results kept)"). The tab shows **Incomplete**. |
| **Continue scan** | The status bar, or **Continue the stopped scan** | Reads the folders a stopped full scan hadn't reached ("… folders not read yet"); the tree grows on from where it stopped. |

A stopped scan is not saved as a snapshot by itself. To keep it as it is, run **Save snapshot** from the command palette (from 0.3.2, or press <kbd>Ctrl</kbd>+<kbd>S</kbd>). Once a continued scan reaches the end, it is saved like any finished scan.

Closing a tab stops its scan.

## When the scan is done

- A snapshot is saved (**Settings › Advanced › Save a snapshot after each scan**). SpaceKnife keeps the newest 10 of each location, and one per week of the older ones (**Recent snapshots to keep**).
- With live monitoring on (the default), changes have been watched since the scan started; they keep coming in and the changed blocks flash. See [Live monitoring](../monitoring/).
- If the window is in the background, it asks for your attention: the taskbar button flashes on Windows, the Dock icon bounces on macOS, the window gets an urgent hint on Linux (**Settings › General › Flash the window when a scan finishes in the background**).

## Free space and unknown space

At the root of a whole-volume scan, the view has two more blocks besides your files:

- **Free space**: what the volume reports as free.
- **Unknown space**: what the volume reports as used, minus everything the scan found. It is the part SpaceKnife couldn't see: folders it wasn't allowed to read, the places macOS protects without Full Disk Access, and space the file system uses that no file accounts for. An MFT quick scan sees more of a volume (protected system folders, the file system's own files), so it leaves less unknown space.

Press <kbd>F</kbd> or <kbd>U</kbd>, or click the **Free space** and **Unknown space** chips in the toolbar, to hide or show them; the other blocks grow to fill the room. Folder scans have neither block, since a folder has no free space of its own. Whether new views show them is set in **Settings › Visualization › Show free space in new views** and **Show unknown space in new views**. More about what the view draws: [Visualizations](../visualizations/).

## Several scans at once

Each tab holds one scan. <kbd>Ctrl</kbd>+<kbd>T</kbd> opens a new tab on the start page, so you can scan several places side by side.

| Keys | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | New tab |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Close tab (stops its scan) |
| <kbd>Ctrl</kbd>+<kbd>Tab</kbd> or <kbd>Ctrl</kbd>+<kbd>PageDown</kbd> | Next tab |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd> or <kbd>Ctrl</kbd>+<kbd>PageUp</kbd> | Previous tab |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Command palette |
| <kbd>Ctrl</kbd>+<kbd>J</kbd> | Log panel |

SpaceKnife keeps up to 8 scans and opened snapshots at a time; close a tab to make room.
