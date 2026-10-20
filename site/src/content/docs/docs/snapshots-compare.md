---
title: Snapshots and comparison
description: Snapshots saved after every scan, how to open them, and how to compare two points in time to see exactly what grew, what shrank and what appeared.
---

"The disk was half empty last month" is a question SpaceKnife can answer. Every finished scan is saved as a **snapshot**. Comparing a snapshot with the disk as it is now (or with another snapshot) shows only what changed, sized by how much it changed.

## Snapshots

- **Saved automatically.** Every scan that finishes is saved as a snapshot right away. A scan you stopped is not saved automatically (you can still save it by hand).
- **Saved by hand.** Run **Save snapshot** from the [command palette](../command-palette/) (<kbd>Ctrl</kbd>+<kbd>K</kbd>), or use **Save snapshot** in the comparison's timeline. This works once the scan has finished or was stopped.
- **One file per scan.** A snapshot is a compressed `.sks` file holding the whole tree. It is written to a temporary name and renamed when complete, so a crash never leaves half a snapshot behind.
- **Kept per location.** Snapshots are grouped by the scanned location. After each automatic save, SpaceKnife keeps the newest ones for that location (**Recent snapshots to keep**, default 10) plus the newest one of each older week, and removes the rest.

To find the files, open the start page, click the **…** button next to **Recent**, and choose **Open snapshot folder**. Where that folder is on each system is listed in [Data locations](../data-locations/).

| Setting | Where | Default |
|---|---|---|
| Save a snapshot after each scan | Advanced › Cache & snapshots | On (applies from the next scan) |
| Recent snapshots to keep | Advanced › Cache & snapshots | 10 (1–100). Of older snapshots, one per week is kept. |

## Opening a snapshot

A snapshot opens in a second, without touching the disk it came from.

- **Start page › Recent.** The newest snapshot of each location, four at first; **Show all scans** lists every snapshot. Each entry has **Open** and **Compare with now**.
- **Drag and drop.** Drop a `.sks` file anywhere on the start page. (Dropping folders there adds them to the scan path instead.)
- **File dialog.** **Open snapshot file…** on the start page, or the same command in the command palette.

A snapshot tab works like any scan: zoom, [filters](../filtering/), [tags](../tags-classes/), [export](../export/). It is a past state, so it is never monitored, and the status bar says "Snapshot · not monitored".

## Comparing

### Starting a comparison

- **From a scan tab:** run **Compare with snapshot…** from the command palette. It compares the most recent earlier snapshot of this location (the base) with **Now**, the tree in the tab. The scan must have finished, and there must be at least one snapshot of the same location.
- **From the start page:** **Compare with now** on a recent entry. If a tab already shows a finished scan of that location, SpaceKnife switches to it and compares at once. Otherwise it scans the location in this tab first ("When the scan finishes, it will be compared with this snapshot.").

### Choosing the two ends

In compare mode the breadcrumb is replaced by the end picker: **Compare** · base ▾ › target ▾ · swap.

- Each drop-down lists **Now** and every snapshot of this location, with its size. **Open snapshot file…** at the bottom picks any other `.sks` file.
- Picking in one drop-down the end that the other one already shows swaps them. The swap button does the same.
- The target does not have to be **Now**: compare any two snapshots.
- Both ends must be the same location. A snapshot of a different location is refused ("The snapshot is of a different location than this tab: …"), and the base and the target can't be the same data.
- While a new pair is computed, the previous result stays on screen, dimmed.

### Reading the change map

The map now shows a **change tree**: only items whose size changed, and their folders.

- **Area is the amount of change**, measured in allocated size. A 40 GB folder that grew by 2 MB is a tiny block; a 300 MB folder that is new is a big one.
- **Colour is the direction and the amount.** Growth has four shades, decrease three. The legend in the side panel shows the steps: 1, 3 and 8 GB by default, scaled down when the largest change is smaller, so a comparison of a few megabytes still shows depth.
- **New** and **Deleted** badges mark items that exist on one side only, on blocks large enough to hold the badge.
- Navigation is the usual one: double-click or <kbd>Enter</kbd> to zoom in, <kbd>Backspace</kbd> to go back. All visualizations can show a comparison (<kbd>V</kbd> / <kbd>Shift</kbd>+<kbd>V</kbd>). Split view isn't available while comparing.

How items are matched:

- By path, level by level. A renamed or moved item counts as deleted at the old path and new at the new one.
- A file and a folder with the same name are two different items.
- On Windows, a change of letter case only is not a change.
- Only the modification time changed: not a change. A change in allocated or logical size is.

### Growth or decrease only

**Changes shown: All | Growth | Decrease** sits in the toolbar. <kbd>C</kbd> cycles through the three (in compare mode it replaces the colour switch). With **Growth**, the map is laid out by growth alone, so what ate the space stands out even when other things were cleaned up in the same period.

### Filtering a comparison

The filter box works as usual. In a comparison, **size conditions measure the change**: `>1gb` shows items that changed by more than 1 GB, `*.vhdx;>500mb` virtual disks that changed by more than 500 MB. See [Filters](../filtering/).

### The side panel

The **Snapshot comparison** panel follows the folder you are zoomed into and the filter. When you are zoomed in, its header shows "Scope: name".

- **Net change** in large type (**Net growth**, **Net decrease** or **No net change**), the time between the two ends ("6 d 12 h apart"), and a bar of growth against decrease.
- **Free space forecast.** When the target is **Now**, the ends are at least a day apart and the location grew, the panel estimates when free space runs out: "Daily average +4.2 GB · free space runs out in about 23 days". It is shown for up to a year ahead and turns into a warning at 30 days or less.
- **Most growth** (top 12) and **Decrease**, depending on **Changes shown**. Each row has the change, a bar, and a **New** or **Deleted** badge. Click a row to show it on the map; hover it to highlight its block.
- **Change units.** The lists don't just repeat the biggest folders. When a folder's change comes mostly from a few children, those children are listed instead, and what is left becomes one row such as "37 other items in Downloads". The rows add up to the total change without counting anything twice.
- **Change annotations.** Items that match SpaceKnife's built-in rules for well-known folders get a short grey note after their name: "Game update" after a game folder, "Package cache", "Browser cache", "System update", "Docker data" and so on. The notes are in the interface language. More in [Freeing up space](../cleanup/#known-folders).
- The **…** menu has **Copy summary**: both ends, the time between them, net change, growth, decrease and both lists, as plain text.

### The timeline

The bottom of the panel shows a timeline of every snapshot of this location, plus **Now**. The range between the base and the target is highlighted.

- Click a point to make it the base.
- <kbd>Shift</kbd>+click a point to make it the target.
- **Save snapshot** adds a point for the current scan.

### The hover card

Hover a block for a moment and a card shows:

- the item, its change and a short path;
- **Base** size › target size, and the change in percent;
- up to four children with the largest changes, and how many new items each has;
- "Double-click to zoom in · right-click for more".

With **Keep open when moving between blocks** (Settings › General › Hover tips) the card follows the pointer from block to block without waiting.

### Live comparisons

When the target is **Now** and [live monitoring](../monitoring/) is on, the comparison follows the disk. After file changes it is computed again once the tree has been quiet for 3 seconds, at most every 10 seconds.

### Acting on what changed

Right-click works in compare mode, and <kbd>Delete</kbd> adds the selected item to the [delete basket](../cleanup/). SpaceKnife maps it to the same item in the live tree. That is only possible when the target is **Now** and the item still exists: "Only items still on disk ("Now") can be acted on".

### Leaving compare mode

Click **Exit comparison**, or press <kbd>Esc</kbd>. The first <kbd>Esc</kbd> clears the selection, the next one leaves. The selected item stays selected in the normal view if it still exists.

## Keys in compare mode

| Key | Action |
|---|---|
| <kbd>C</kbd> | Changes shown: All → Growth → Decrease |
| <kbd>V</kbd> / <kbd>Shift</kbd>+<kbd>V</kbd> | Next / previous visualization |
| <kbd>Enter</kbd> | Zoom into the selected item |
| <kbd>Backspace</kbd> | Back |
| <kbd>Delete</kbd> | Add the selected item to the delete basket (target **Now** only) |
| <kbd>Shift</kbd>+<kbd>F10</kbd> | Context menu of the selected item |
| <kbd>Esc</kbd> | Clear the selection; with nothing selected, exit the comparison |
| <kbd>Shift</kbd>+click on the timeline | Set the target |

The commands **Save snapshot**, **Compare with snapshot…** and **Open snapshot file…** are in the command palette. All keys are listed in [Keyboard shortcuts](../shortcuts/).

## Tips

- **Find the culprit of a sudden drop in free space.** Compare with the newest snapshot, switch to **Growth**, and read the top of **Most growth**.
- **Check what a cleanup really freed.** Save a snapshot before cleaning, then compare it with **Now**. Switch to **Decrease**.
- **Track slow growth over weeks.** Keep the default retention: older weeks keep one snapshot each, so the timeline reaches back far enough to show a trend and the free space forecast has a long span to work from.
