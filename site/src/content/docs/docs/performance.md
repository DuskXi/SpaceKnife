---
title: Scan speed and disk load
description: How SpaceKnife scans, how to make a scan faster or gentler with the disk-load limit and concurrency, and how to read the numbers in the scan details and the log.
---

SpaceKnife tries to scan as fast as your disk allows while leaving room for the programs you are using. This page explains the two scan methods, the disk-load limit that keeps a scan polite, and the numbers that tell you what a scan is waiting for.

## Quick recipes

| You want | Do this |
|---|---|
| The fastest scan of a Windows drive | Scan the whole drive (not a folder) with **Auto** or **Quick MFT** and accept the administrator prompt: the MFT quick scan reads a whole disk in seconds |
| The fastest full scan | Choose **Full speed** under **Disk load limit** before you start, or in **Scan details** while it runs |
| To keep working undisturbed | **Quiet 35%**, or **Idle**, which reads only while the disk is idle |
| A scan overnight or from a script | The command line with `--disk-limit idle` (see [Command line](../command-line/)) |
| To find out what slows a scan | Read the scan profile in the log, or run the command line with `--profile` |

## Two ways to scan

| | MFT quick scan | Full scan |
|---|---|---|
| Where | Windows, a whole local NTFS volume | Everywhere |
| How | Reads the NTFS master file table in one sequential read, through the read-only scan helper | Walks every folder, many folders in parallel |
| Speed | A whole disk in seconds | Time grows with the number of files |
| Disk-load limit | Slows the pace of the read; there is no concurrency to adjust | Limits how many batches of folders are listed at once |
| Pause | Not available: it finishes in seconds | Yes |
| Rights | The scan helper asks for administrator rights; the app itself is never elevated | Your own |

**Settings › Scan & performance › Scan method › Default scan method** chooses between them:

- **Auto** (the default): whole NTFS volumes get an MFT quick scan; other locations a folder-by-folder full scan.
- **Quick MFT**: always try the MFT quick scan. If it can't be used, the scan falls back to a full scan and says why.
- **Full scan**: always walk the folders.

On the start page, **Scan options** shows the same choice, and every volume shows what it will be scanned with: `MFT quick scan · needs admin rights`, `Parallel full scan`, `Parallel full scan · throttled for USB HDD`, `Network scan · slower`, or `Not recommended · very slow`.

### The administrator prompt

The MFT can only be read with administrator rights, so SpaceKnife starts a small read-only **scan helper** for it, and only the helper asks for those rights (a UAC prompt). The helper does a fixed set of read-only operations; it reads the MFT for quick scans and the USN journal for live monitoring.

- If you decline the prompt, the scan goes on as a full scan, and **Scan details** says so.
- **Ask for administrator rights when needed** (on by default, same settings page): when off, a whole NTFS volume gets a full scan instead of a prompt, unless SpaceKnife already has the rights.
- One helper serves the whole session. An idle helper quits after 10 minutes; the next quick scan then asks again, unless SpaceKnife itself runs as administrator. **Settings › Advanced › Scan helper & cache** shows whether it runs and has **Stop scan helper**.
- During a full scan of a whole NTFS volume, **Scan details › Scan method** offers **Use MFT quick scan as administrator**. The full scan keeps going while the helper starts, and the MFT result replaces it once ready.

An MFT quick scan shows its phases instead of a percentage: `Waiting for administrator approval…`, `Reading the MFT (master file table)…`, `Receiving scan results…`, `Building the folder tree…`. The tree appears in one go when reading is done.

## How a full scan works

A full scan lists folders in batches: each call lists up to 256 folders and returns after at most 50 ms with what it has. Batches run on a pool of **listing threads**; the main thread folds every batch into the tree, which is why the picture can grow while the scan runs.

- **Listing threads**: one per CPU, between 2 and 16. On macOS twice the CPUs, between 4 and 16, because APFS listings wait in the kernel often enough that extra threads keep the CPUs busy. `SPACEKNIFE_SCAN_THREADS` sets the number (1 to 64).
- **Native walker**: the listing is done by a native extension written in Rust, included in every installer. Without it (a source checkout without Rust) a Python walker runs in worker processes and is several times slower.
- **System calls**: on Windows one `NtQueryDirectoryFile` call returns names, sizes, size on disk, times, attributes and file IDs together; on Linux `openat`, `getdents64` and `fstatat`; on macOS `getattrlistbulk` for files and `fstatat` for folders.
- **How many batches run at once** is decided by the disk-load limit and the kind of disk (below).

### Progress and the time left

- If the location was scanned before, the last complete snapshot says how many items to expect, and progress counts items. For a folder, the progress band also shows `(last scan: N items)`.
- Otherwise, for a whole volume, progress compares the bytes found with the volume's used space, up to 95 %. Used space is no exact total (hard links, clones, snapshots and system areas), so past that point the percentage shows `…` instead of sitting at 99 %.
- The time left appears after the first 3 seconds and uses the speed of the last few seconds, not the average since the start.

### Kind of disk

The start page shows each volume's media. Disks that suffer from parallel reads get fewer batches at once, whatever the limit, unless you set the concurrency by hand:

| Media | Batches at once, at most |
|---|---|
| HDD, USB HDD, USB drive, Memory card | 2 |
| USB SSD | 4 |
| Network | 8 |
| NVMe SSD, SATA SSD, SSD, Virtual disk, Unknown media | All listing threads |

A hard disk spends its time seeking when many folders are read at once, so two at a time is faster there.

## The disk-load limit

The limit caps how much of the disk's read capacity a scan may use:

| Level | What it does |
|---|---|
| **Full speed** | No limit on disk reads |
| **Balanced 70%** | Scans put at most 70% read load on the disk, leaving room for the apps you are using |
| **Quiet 35%** | Scans put at most 35% read load on the disk. You'll hardly notice them |
| **Idle** | Reads only while the disk is idle, at the system's low I/O priority |
| **Custom** | Uses **Custom limit** from the settings |

Where to set it:

- **Start page › Scan options › Disk load limit**, for the scan you are about to start.
- **Scan details** (the side panel during a scan) › **Disk load limit**, at any time. The new limit applies at once; the arrow keys move between the levels.
- **Settings › Scan & performance › Disk load limit & concurrency**: **Default for manual scans** (Balanced 70%) is what the start page starts with, and **Custom limit** (70 %, from 5 to 100 in steps of 5) is the percentage of the Custom level.
- The command line: `--disk-limit`.

The status bar shows the level and the current load, for example `Balanced ≤70%`; clicking it during a scan opens **Scan details**.

### How the load is measured

SpaceKnife samples every physical disk from the moment it starts, without administrator rights: Windows performance counters, `/sys/block/<disk>/stat` on Linux, and on macOS the IOKit block-storage statistics that `iostat` reads too.

- **Load** is the smaller of two numbers: the share of time the disk had requests in progress, and the average number of requests waiting divided by how many the disk handles in parallel (1 for a hard disk, 8 for a SATA SSD, 32 for an NVMe SSD, refined while it scans).
- **Active time** is the first number alone, as Task Manager shows it. A few parallel reads take an NVMe SSD to 100 % active time while it still has plenty of capacity, so the limit uses the load, and active time is for reference only.
- **SpaceKnife's own share** is the load minus what other programs cause. SpaceKnife measures the others while it has nothing in flight: before the scan, while paused, and in short probes (dispatch held for a moment) every 8 to 32 seconds, more often while it yields.
- **Control**: the scan aims a little under the limit. With every sample it adjusts: below the limit it adds up to one batch in flight per half second (faster when far below, as at the start); above it, it cuts back by up to half. It never drops below 5 % of one batch, so a scan always makes some progress.
- **One limiter per physical disk**: scans on the same disk share it.
- **macOS** reports no idle time, so the share of busy time is estimated from the queue length.
- When a disk's load can't be measured (for example a network drive), **Scan details** shows `Can't measure this disk`, and the limit becomes a fixed share of the maximum number of batches.

### Giving way to other programs

**Give way when the disk is busy** (on by default) applies to Balanced, Quiet and Custom: when other programs alone push the whole disk past the limit, the scan slows down to the minimum until they drop below 85 % of the limit. **Scan details** then says `Other programs are reading or writing a lot on this disk, so the scan is yielding (minimum progress).`

The **Idle** level always gives way, as soon as other programs use more than 10 % of the disk. **Full speed** never does.

### Low I/O priority

**Use low I/O priority when idle** (on by default) makes the Idle level read at the operating system's low I/O priority, so the system itself serves other programs first:

- Windows: the very-low I/O priority class.
- Linux: the idle I/O class. Only I/O schedulers that honour priorities (BFQ, recent mq-deadline) act on it; the `none` scheduler, common on NVMe drives, ignores it.
- macOS: the throttled I/O policy.

### Concurrency

**Scan details** shows `Concurrency N · In flight N`. With **Auto** on, the limit decides; switch it off to set the number yourself with − and +. **Settings › Scan & performance › Maximum concurrency** sets **Auto** or **Manual** for new scans, and **Manual concurrency** the number (4 by default). A manual number can't go above the number of listing threads; raise those with `SPACEKNIFE_SCAN_THREADS`.

## Pause, stop and continue

- **Pause** stops sending new batches: SpaceKnife's reads drop to zero within a second. **Resume** carries on from the same place.
- **Stop** keeps what was found (`Stopped (results kept)`). A stopped full scan can be continued later: **Continue scan** in the status bar (it tells you how many folders weren't read yet), or **Continue the stopped scan** in the command palette.
- **Rescan current view** (command palette) rescans only the folder you zoomed into; **Rescan everything** scans all roots again. From 0.3.2, press <kbd>F5</kbd> and <kbd>Shift</kbd>+<kbd>F5</kbd> (see [Keyboard shortcuts](../shortcuts/#start-page-and-scanning)). At the top of a scan of several locations, Rescan current view reads all of them again. With live monitoring on, the tree stays current anyway, so a full rescan is rarely needed (see [Live monitoring](../monitoring/)).

## Reading the numbers

### Scan details

| Shown | Meaning |
|---|---|
| `Current N%` | The read load SpaceKnife put on this disk in the last second (kept under the limit) |
| `Whole disk N%` | The load of the whole disk, including other programs |
| `Active time N%` | Task Manager's measure; for reference only (see above) |
| **Disk read load · last 60 s** | Area: the whole disk. Solid line: SpaceKnife. Dashed line: the limit. Active time is drawn as well, for reference |
| `Disk reads … /s · average latency N ms` | The whole disk's read speed and the average time per read in the last sample, other programs included |
| `Concurrency N · In flight N` | Batches allowed at once, and how many are running now |

### The scan profile in the log

After every full scan, the log file gets two lines like these (the numbers are an example):

```text
scan done: 1,234,567 items, 1,103,402 files, 210.4 GiB in 41.20s (29,965 items/s), 12 errors
scan profile: 1,234,567 items in 41.2s (29,965/s); listing threads busy 92% of 16; main thread folding 38%; held by the disk-load limit 0%; 4,823 slices; getattrlistbulk on
```

The log file is `spaceknife.log` in the log folder (**Settings › Advanced › Files & diagnostics › Log folder**; see [Troubleshooting](../troubleshooting/)). The second line tells you what the scan waited for:

| Part | Meaning | What helps |
|---|---|---|
| `listing threads busy 92% of 16` | The listing threads spent 92 % of their time in system calls | Near 100 %: the disk or file system is the limit. On an SSD with headroom, more threads (`SPACEKNIFE_SCAN_THREADS`) may help; on a busy or slow disk nothing will |
| `main thread folding 38%` | Share of the scan the main thread spent adding results to the tree | Near 100 %: folding is the limit; more threads won't help |
| `held by the disk-load limit 0%` | Time no folder was being listed because the limit held the next batch | High: the limit is doing its job. Choose a higher level or Full speed if you want speed |
| `4,823 slices` | Batches listed | — |
| `getattrlistbulk on` | macOS only: bulk listing is on, `off: <reason>`, or `untested` | — |

When busy and folding are both low, the listing threads were waiting for work: a deep, narrow folder tree, or a scan that was yielding to other programs. The command line's `--profile` prints the same numbers.

## Memory and very large disks

The scan data is kept compactly, in columns: about 126 bytes per item (a file or folder) with an average name length.

| Items | Memory |
|---|---|
| 1 million | about 0.13 GB |
| 10 million | about 1.3 GB |
| 50 million | about 6.3 GB |
| 100 million | about 12.6 GB |

- **Keep large scans in mapped files** (**Settings › Advanced › Memory**, off by default): when on, a scan whose node table passes 256 MB (about two million items) goes to memory-mapped files in the cache folder. They don't count as the process's private memory; the system keeps the parts in use in RAM and drops the others, reading them back from disk when needed (a pass over a whole scan can then take some tenths of a second longer). The cost: after the scan the data is written to these files once, about 200 bytes per item (about 1.8 GB for a drive of 8.6 million items). Scans are as fast as before.
- **Scan data memory limit** (same page, 25 % by default, from 5 to 90): once the open scans pass this share of physical memory, smaller scans go to mapped files too.
- The mapped files are deleted automatically, even after a crash. If there is no room on the disk, SpaceKnife stays in memory.
- **Up to 8 scans** stay in memory. Starting another one drops the oldest scan that has finished and has no open view.
- **Closing a scan** frees its data. The process may keep some freed memory to reuse for the next scan; that is expected.
- **MFT quick scan**: the helper's peak is about the size of the result (about 1.6 GB for 8.4 million entries); building the tree then needs about 85 bytes per entry for a moment, and about 100 bytes per item afterwards; a large volume then moves to mapped files.
- **Live monitoring**: while files keep changing after a scan, memory no longer jumps every few seconds: the details panel's summary is worked out in pieces and once for everyone who asks, and refreshes less often on large scans (at most every 10 seconds); file changes no longer double the memory of the scan data either.
- **Memory watchdog**: every 15 seconds the backend compares its private memory with what the open scans account for. Well above that (twice the scan data plus 1.5 GB, plus the largest scan closed in the last hour), it first hands freed memory back to the system, and only if that doesn't help shows `Backend memory use is abnormal: …` in the log panel and writes the details to the log file. See [Troubleshooting](../troubleshooting/#memory-warning).

For a very large disk: on Windows use the MFT quick scan; elsewhere, scan the folders you care about rather than the whole disk; keep **Save a snapshot after each scan** on, so the next scan of the same location shows its progress by item count; and close tabs you no longer need.

## Network drives and other slow locations

- **Network drives** show `Network scan · slower`. Their load can't be measured, and at most 8 batches are listed at once.
- **Windows drives seen from WSL** (`/mnt/c` and the like) are marked `Not recommended · very slow`: they go through the 9P protocol. Scan them with the Windows version of SpaceKnife instead.
- A network volume that isn't connected can't be scanned (`Network volume not connected`).
- **Cross mount points** (**Settings › Scan & performance › Scan scope**, off by default): when off, a scan stays on the volume it started on and doesn't descend into other volumes mounted below it.

## macOS

- **Full Disk Access**: without it, macOS asks one by one before the Desktop, Documents, Downloads, cloud drives, the Photos library and other apps' data can be read, and places such as Mail and Messages can't be read at all (they count as unknown space). See [Troubleshooting](../troubleshooting/#macos-full-disk-access).
- **Bulk listing**: files come from `getattrlistbulk` (folders, mount points and firmlinks still from `fstatat`). The first batch each process lists is checked against `fstatat`; on any difference SpaceKnife switches back to `readdir` and `fstatat` for good. `SPACEKNIFE_NO_BULK=1` turns bulk listing off for comparisons.
- **Scanning `/`** skips `/System/Volumes/Data`: its files are already in the tree through the firmlinks.
- **Cloud files**: files that live only in iCloud Drive or another cloud provider count as 0 bytes on disk, their folders aren't entered, and nothing is downloaded.
