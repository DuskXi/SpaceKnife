---
title: Live monitoring
description: How an open scan stays up to date while files change on Windows, Linux and macOS, how to read the status bar, and how to rescan by hand.
---

A scan in SpaceKnife is not a photo that goes stale. While the tab is open, SpaceKnife watches the scanned locations and folds every change into the tree: a download that grows, a build folder you delete, a game update that lands. The map, the details panel, free space and the delete basket follow on their own.

Monitoring starts together with the scan, so changes made while the scan is still running are not lost. It stops when you close the tab. Opened [snapshots](../snapshots-compare/) are never monitored: they show a past state on purpose.

## What a change looks like

- **Flash.** A block whose contents changed flashes twice, then fades out. If the changed item is too small to have a block of its own, the block that contains it flashes instead.
- **Static badge.** With **Reduce motion** on (Settings › Appearance › Animation), there is no flashing. Instead, the change in size is written on the block, for example `+212 MB` or `−1.2 GB`, for about 10 seconds where the block has room for it.
- **Everything else moves with it.** Sizes of all parent folders, the free and unknown space blocks, the status bar's free space (read again at most once per second) and the sizes in the [delete basket](../cleanup/) update with the change.

To turn the flashing off entirely, switch off **Flash changed blocks** (Settings › Visualization › Change highlights). The same switch also controls the breathing outline of folders that are still being scanned.

## The status bar

The live monitoring item in the status bar shows the state of monitoring for the active tab. Hover it for details, click it to turn monitoring on or off.

| Status bar shows | Meaning |
|---|---|
| Live monitoring | Every scanned folder is watched with the system's change notifications. The tooltip counts the changes applied so far ("{n} changes applied"), and how many are still waiting ("{pending} pending"). |
| Live monitoring · starting | The watchers are being set up (FSEvents can take a moment). |
| Live monitoring · partly polling | Some or all folders are checked by polling instead of notifications. The tooltip says why, for example "Not enough inotify watches; polling {count} folders instead". |
| Live monitoring · failed | Watching stopped. The tooltip gives the reason, for example "The watched folder was deleted". |
| Live monitoring off | Monitoring is switched off. |
| Snapshot · not monitored | The tab shows a snapshot. |
| Live monitoring · starts after a scan | No scan in this tab yet, monitoring is on. |

:::note
The click changes the **Live monitoring** setting, so it applies to every open scan, not only to the active tab. Turning it back on starts watching all open scans again.
:::

## How it works on each platform

SpaceKnife picks the best mechanism the system offers and falls back to polling when it can't use it. The watchers only report *where* something changed. Structural changes (new, deleted, renamed items) are always resolved by listing the affected folder again, so the tree ends up exactly as it is on disk.

### Windows

- One recursive `ReadDirectoryChangesExW` watch per scan root. Its extended information carries each file's new size, allocation and modification time, so a file that grows is updated in place without touching the disk again.
- Volumes or file systems without the extended information fall back to `ReadDirectoryChangesW`. Size changes then need the folder to be listed again.
- When the system's notification buffer overflows, events were lost. If the elevated scan helper is already running (an MFT quick scan started it), SpaceKnife reads the NTFS **USN change journal** from a recent checkpoint and lists exactly the folders that were touched. Otherwise, the affected subtree is read again. SpaceKnife never asks for administrator rights just for monitoring.
- If a scan root is deleted, moved or renamed, monitoring of that root stops with "failed".

### Linux

- One `inotify` watch per folder. New folders get a watch as soon as they appear, and their contents are read at once, so `mkdir` followed by a quick write, or a whole tree moved in, is not missed.
- A scanned folder gets its watch a moment after the scan read it. Once the watch is on, SpaceKnife checks the folder's modification time against what the scan recorded, so a file created, deleted or renamed in that moment is not missed either: the folder is read again. A file that only changed size in that moment is picked up at its next change, or by a rescan.
- All open scans share a budget of 80 % of `fs.inotify.max_user_watches`. Folders beyond the budget are polled, and the status bar shows "partly polling".
- Network file systems (NFS, SMB) and Windows drives under WSL are always polled: the kernel never hears about changes made by other machines (or by Windows).
- When the kernel's event queue overflows, SpaceKnife reads the watched tree again once things are quiet. Lost events are never ignored silently.

:::tip[Watch more folders on Linux]
If you see "Not enough inotify watches", raise the limit, for example:

```sh
sudo sysctl fs.inotify.max_user_watches=524288
```

Scans started afterwards use the new limit. To keep it after a reboot, put the setting in a file under `/etc/sysctl.d/`.
:::

### macOS

- One `FSEvents` stream per scan root. When the system reports that events were dropped, the affected folders are read again.
- If FSEvents can't be started, SpaceKnife polls instead.

### Polling

Polling is the fallback everywhere. It compares each folder's modification time every 30 seconds, checking at most 2000 folders per second, so it stays a light background load.

:::caution
Polling sees files being added, deleted and renamed, but **not** a file that changes size in place (a growing log or a virtual disk). The tooltip says so: "…so changes in file size go unnoticed". Rescan by hand when it matters.
:::

## How changes are applied

1. **Merging.** Events are collected until nothing has happened for the **Event merge interval** (default 150 ms). Under a steady stream of events, they are still applied at least once per second.
2. **Updating.** A file whose size or time changed is updated in place. A folder whose entries changed is listed again, one level only, with the same code the scan uses. New folders are then scanned.
3. **Storms.** When one batch would touch more than 2000 folders or holds more than 20 000 events (an unpacked archive, a big `git checkout`), SpaceKnife stops chasing single events. It marks their common folder and reads it again once there has been 2 seconds of quiet. The log panel (<kbd>Ctrl</kbd>+<kbd>J</kbd>) records this as "Too many file changes".
4. **While a scan runs.** Changes inside folders the scan has not read yet are simply picked up by the scan. During an MFT quick scan, all changes wait until the whole tree has arrived (a few seconds), then they are applied.

## Rescanning by hand

You rarely need it with monitoring on, but it is there when you want certainty, when monitoring is off, or after polling missed a size change.

| What | How | What it reads |
|---|---|---|
| Rescan this folder | Right-click › **Rescan this folder** | The folder and everything below it. On a file: its parent folder. |
| Rescan current view | [Command palette](../command-palette/) (<kbd>Ctrl</kbd>+<kbd>K</kbd>) › **Rescan current view** | The folder you are zoomed into, with everything below it. |
| Rescan everything | Command palette › **Rescan everything** | A new scan of the same locations, in the same tab. |

- A rescan of a folder starts at once (it does not wait for a quiet period) and works with monitoring switched off too.
- Rescanned blocks flash where something changed, like any other change.
- While the tab's scan is still running, folder rescans are refused: "A scan is running. Scan again once it is done."

## Continuing a stopped scan

If you stop a hard scan before it finishes, the results so far are kept, and the folders it had not read yet are remembered. The status bar then shows **Continue scan** (tooltip: "{n} folders not read yet"). Click it, or run **Continue the stopped scan** from the command palette, and the scan picks up where it stopped. When it completes, it is saved as a snapshot like any finished scan.

This works for hard scans only. An MFT quick scan finishes in seconds and has nothing to continue.

## Settings

| Setting | Where | Default | Effect |
|---|---|---|---|
| Live monitoring | Scan & performance › Live monitoring | On | Watch open scans for changes. Same switch as the status bar item. |
| Event merge interval | Scan & performance › Live monitoring | 150 ms | Changes within this time are merged into one refresh (50–5000 ms). Raise it if a folder that changes constantly keeps the map busy. |
| Flash changed blocks | Visualization › Change highlights | On | Flash (or badge) changed blocks. |
| Reduce motion | Appearance › Animation | Match system | On: static badges instead of flashes. |

Open settings with <kbd>Ctrl</kbd>+<kbd>,</kbd>. See [Settings](../settings/) for the full list.

## Limits

- Only the scanned locations are watched. Mount points inside a scanned folder are not entered by the scan, so they are not watched either.
- Monitoring lives in memory. Nothing is recorded while SpaceKnife is closed. To see what changed since your last scan, [compare with a snapshot](../snapshots-compare/).
- Polled folders can't report size changes of existing files (see above).
- If monitoring fails or keeps falling back to polling, see [Troubleshooting](../troubleshooting/).
