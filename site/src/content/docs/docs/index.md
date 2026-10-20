---
title: Overview
description: What SpaceKnife does, which pages to read first, a map of the documentation and the keyboard shortcuts worth learning on day one.
---

SpaceKnife shows you what fills your drives. It scans a drive or a folder and draws everything in it as a picture in which size is space, so the largest files and folders stand out at a glance. The picture fills in while the scan runs, and afterwards it keeps up with changes on disk.

It runs on Windows 10 and later, on Linux, and on macOS 14 and later, in English and Simplified Chinese.

## What it does

- **Three visualizations**: a nested treemap, a flame graph and a polar flame graph of the same scan. Switch at any time with <kbd>V</kbd>.
- **Fast scans**: folders are read in parallel. On Windows, a whole NTFS volume can be read from its Master File Table instead, which takes seconds.
- **Polite to your disk**: you can cap the disk load a scan causes, and it backs off when other programs need the disk.
- **Live monitoring**: after the scan, changes on disk keep flowing in and the changed blocks flash.
- **Filters**: by name, file class, size, dates, attributes and tags, combined freely. Matches stay lit, the rest dims, and the side panel lists them.
- **Tags**: four color tags (red ▲, yellow ■, green ●, blue ◆) to mark what to keep or remove.
- **Snapshots and comparison**: a snapshot is saved after each scan. Compare any two, or a snapshot with now, and see what grew and what is new.
- **Safe cleanup**: collect items in the delete basket first. They go to the recycle bin by default, and system folders are protected.
- **Export and command line**: CSV, JSON and HTML reports, and a command line for scans and snapshots.

## Where to start

1. [Install and upgrade](./install/): get SpaceKnife onto your system.
2. [Your first scan](./first-scan/): pick a drive or folder and read the result.
3. [Visualizations](./visualizations/): what the treemap, the flame graph and the polar flame graph show, colorings, detail levels and the special blocks.
4. [Navigating the picture](./navigation/): zoom, history, the details panel, the context menu, split view and tabs.
5. [Filters](./filtering/): find exactly the files you care about.
6. [Keyboard shortcuts](./shortcuts/): everything on one page.

If you only read one page after the first scan, make it [Navigating the picture](./navigation/): almost everything there has a key, and the keys are what make SpaceKnife fast to use.

## The documentation

**Getting started**

- [Install and upgrade](./install/): installers and portable builds for each system, and how to upgrade.
- [Your first scan](./first-scan/): starting a scan and what you see while it runs.

**Using SpaceKnife**

- [Visualizations](./visualizations/): the three ways to draw a scan, and how to read them.
- [Navigating the picture](./navigation/): moving around, selecting, menus, split view, tabs, the results list, jumping to a path.
- [Filters](./filtering/): the filter language, with examples.
- [Tags and file classes](./tags-classes/): color tags, and the file classes behind the colors.
- [Live monitoring](./monitoring/): how SpaceKnife follows changes after the scan.
- [Snapshots and comparison](./snapshots-compare/): saving, opening and comparing scans.
- [Freeing up space](./cleanup/): the delete basket, the recycle bin and protected places.
- [Export and reports](./export/): getting the data out.

**Advanced**

- [Keyboard shortcuts](./shortcuts/): every key.
- [Command palette](./command-palette/): commands, paths and filters from one input.
- [Command line](./command-line/): scanning and working with snapshots from a terminal.
- [Scan speed and disk load](./performance/): scan methods, parallelism and the disk load cap.
- [Settings reference](./settings/): every setting.
- [Data locations and portable mode](./data-locations/): where settings, snapshots and logs live.

**Help**

- [Updates and privacy](./updates-privacy/): what SpaceKnife checks online and what it never sends.
- [Troubleshooting](./troubleshooting/): common problems and how to solve them.
- [Building and contributing](./contributing/): building from source and helping out.

## Shortcuts to learn first

| Keys | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Command palette: run a command, jump to a path, find a file by name, apply a filter |
| Double-click, <kbd>Enter</kbd> | Zoom into a folder |
| <kbd>Backspace</kbd> | Back to where you were |
| <kbd>Alt</kbd>+<kbd>↑</kbd> | Up one level |
| <kbd>Ctrl</kbd>+<kbd>Home</kbd> | Back to the top of the scan |
| <kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>F</kbd> | Filter |
| <kbd>V</kbd> / <kbd>Shift</kbd>+<kbd>V</kbd> | Next / previous visualization |
| <kbd>C</kbd> | Next coloring |
| <kbd>Ctrl</kbd>+<kbd>=</kbd> / <kbd>Ctrl</kbd>+<kbd>-</kbd> | More / fewer levels of detail |
| <kbd>F</kbd> / <kbd>U</kbd> | Show or hide free space / unknown space |
| <kbd>Shift</kbd>+<kbd>F10</kbd> | Context menu of the selection |
| <kbd>Delete</kbd> | Put the selection in the delete basket (nothing is deleted yet) |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> / <kbd>Ctrl</kbd>+<kbd>W</kbd> | New tab / close tab |

Single-letter keys don't fire while you type in a text field. On macOS, press <kbd>⌘</kbd> wherever a key says <kbd>Ctrl</kbd>, and <kbd>Option</kbd> for <kbd>Alt</kbd>. The full list is in [Keyboard shortcuts](./shortcuts/).

## A few habits that pay off

- **Zoom, don't squint.** Keep the detail level moderate and double-click into the part you care about; <kbd>Backspace</kbd> brings you back exactly where you were, selection included.
- **Let the side panel do the reading.** Select a folder and open **Largest files** to see its biggest files wherever they sit.
- **Find all copies of a name.** Select a folder such as `node_modules`, press <kbd>Ctrl</kbd>+<kbd>K</kbd> and leave the input empty: the palette lists other items whose names contain it.
- **Two views at once.** Split view puts a treemap and a flame graph of the same scan side by side.
- **Filter, then walk.** After filtering, click the first match in the side panel and press <kbd>↓</kbd>: each match lights up in the picture.
