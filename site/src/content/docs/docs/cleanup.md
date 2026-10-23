---
title: Freeing up space
description: The context menu, copying paths, the delete basket, moving to the trash with undo, permanent deletion with a 5-second delay, finding duplicate files, protected locations and the rules for well-known folders.
---

SpaceKnife never deletes anything the moment you press a key. Everything you want gone goes into the **delete basket** first. The basket shows what it would free, checks what is in use or protected, and only then moves the items to the trash (the default) or deletes them permanently, both with a way back.

## The context menu

Right-click any block: a file, a folder or a small-items block. The block is selected first, then the menu opens beside it, so every action applies to what you clicked. From the keyboard, press <kbd>Shift</kbd>+<kbd>F10</kbd> or the <kbd>Menu</kbd> key for the selected block. The **More actions** button at the top of the details panel opens the same menu.

The header shows the full name, then size · file class · parent folder, and the item's tag if it has one.

| Item | Key | What it does |
|---|---|---|
| Open | | Files open in their default app; folders open in the file manager. |
| Show in Explorer | <kbd>Ctrl</kbd>+<kbd>E</kbd> | Opens the parent folder with the item selected. On macOS **Show in Finder**, on Linux **Show in file manager**. |
| Open terminal here | <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> | Opens a terminal in the folder (for a file: its folder). Windows Terminal (`wt.exe`) on Windows, Terminal on macOS, `x-terminal-emulator` on Linux. |
| Copy path ▸ | <kbd>Ctrl</kbd>+<kbd>C</kbd> copies **As is** | Four formats, each with a preview of the exact text (see below). |
| Add tag ▸ | <kbd>Ctrl</kbd>+<kbd>1</kbd>–<kbd>4</kbd>, <kbd>Ctrl</kbd>+<kbd>0</kbd> | **Red tag**, **Yellow tag**, **Green tag**, **Blue tag**, **Clear tag**. See [Tags and file classes](../tags-classes/). |
| Show only this (zoom in) | <kbd>Enter</kbd> | Zooms into the item, like a double-click. Not shown for the folder you are already zoomed into. |
| Rescan this folder | | Reads the folder again with everything below it; for a file, its parent folder. See [Live monitoring](../monitoring/#rescanning-by-hand). |
| Add to delete basket | <kbd>Delete</kbd> | See [the delete basket](#the-delete-basket). |
| System menu… | | Shown greyed out: not available in this version. |

The keys in the table also work without the menu: they act on the selected block whenever the map has the focus (not while you type in the filter box, where <kbd>Ctrl</kbd>+<kbd>C</kbd> copies text as usual).

Items that don't apply are greyed out, and hovering one tells you why. A small-items block is not a real location ("A small items block isn't a real location."), so the open, show, terminal and copy items are off, and it can't be tagged. It can go into the basket. NTFS metadata and orphaned files from an MFT quick scan have no location on disk, so the file actions are off for them.

**In the menu:** <kbd>↑</kbd> / <kbd>↓</kbd> move (and wrap around), <kbd>→</kbd> or <kbd>Enter</kbd> opens a submenu, <kbd>←</kbd> or <kbd>Esc</kbd> closes it, <kbd>Enter</kbd> runs the highlighted item, <kbd>Esc</kbd> closes the menu. Clicking elsewhere, resizing the window or switching to another window closes it too.

### Copy path formats

The four formats are made by the backend, so they are the same in every front end.

| Format | Windows | Linux / macOS |
|---|---|---|
| As is | `C:\Users\lenny\Downloads\big file.iso` | `/home/lenny/My Videos/it's.mp4` |
| Quoted | `"C:\Users\lenny\Downloads\big file.iso"` | `'/home/lenny/My Videos/it'\''s.mp4'` |
| POSIX path | `/mnt/c/Users/lenny/Downloads/big file.iso` | same as As is |
| file:// URI | `file:///C:/Users/lenny/Downloads/big%20file.iso` | `file:///home/lenny/My%20Videos/it%27s.mp4` |

- **Quoted** on Linux and macOS uses single quotes, with each `'` written as `'\''`. Inside single quotes the shell expands nothing (`$`, backticks, `!`), so the result pastes safely into any shell.
- **POSIX path** on Windows is the WSL form: drive letter in lower case under `/mnt/`. A network path `\\nas\share\x` becomes `//nas/share/x`.
- **file:// URI** follows RFC 8089: spaces, non-ASCII and reserved characters are percent-encoded as UTF-8. A network path becomes `file://nas/share/x`.

## The delete basket

### Adding items

- <kbd>Delete</kbd> on the selected block (on a Mac keyboard: <kbd>fn</kbd>+<kbd>Delete</kbd>).
- **Add to delete basket** in the context menu or in the details panel.
- <kbd>Delete</kbd> on a row of the filter's results list: filter, then walk the list with <kbd>↓</kbd> and add what you want.
- **Add to delete basket** in the [command palette](../command-palette/).

Adding never asks and never touches the disk. A short message confirms it: "Added to the delete basket", "Already in the delete basket", or "Can't delete: …" with the reason.

**Small-items block.** Right-click it and choose **Add to delete basket**: the basket gets one item, "{n} small items", listed under its folder. It holds exactly the files in the block at that moment: not the larger files of the same folder that have blocks of their own, and not files that appear later. (The <kbd>Delete</kbd> key does not add small-items blocks: use the menu.)

**Nesting is resolved for you.**

- The same path never goes in twice.
- Adding a folder whose contents are already in the basket absorbs them into the folder's item ("Added to the delete basket (3 items already in it were merged)").
- Adding something inside a folder that is already in the basket adds nothing new.

The basket belongs to the app, not to a tab: items from several tabs can wait in it together. It is emptied when you quit SpaceKnife.

### The drawer

The basket button at the right end of the toolbar shows the number of items. Click it (or run **Open delete basket** in the command palette) to open the drawer in place of the side panel; click it again or the **×** to close it. Opening and closing changes nothing in the basket.

- **Can free**: the total that deleting would free, in large type.
- **Capacity bar** of the volume with the most to free: used | in the basket | free. Below it a line such as "C:\ 182.4 GB free › 271.6 GB", where the second number is the free space after deleting, and the volume's total.
- **To delete**: the items, sorted **By size**, **By name** or **By location**. Each row shows the name, tag, folder, size and share of the basket, and a **×** that takes it out of the basket (nothing on disk changes).
- **Total** and a note about the trash: "Space moved to the trash is freed only when the trash is emptied.", with what the volume's trash holds now, or which volumes have no trash.
- The **…** button empties the basket. No files are deleted.

### What counts as freeable

- Allocated size (the space on disk), not the logical size.
- Nested items are counted once.
- A file with several hard links frees space only when **all** its names are in the basket. Otherwise deleting one name frees nothing, and it counts as 0.
- Cloud placeholders (files that live only online) count as 0.
- Items that are in use still count: they free their space once the program lets go of them.

### Items that can't be deleted now

Some items stay in the basket with a warning line, and the drawer says "{n} items can't be deleted now and will be skipped."

| Warning | Why |
|---|---|
| In use by {names}; quit it first | Another program holds the file open. Windows names the program (Restart Manager). Linux looks at the open files in `/proc`, only for processes you may inspect. macOS uses `lsof`. Checked when the item is added and again right before deleting. |
| No longer exists | The item was deleted or moved outside SpaceKnife. |
| Protected system location, can't be deleted | See [protected locations](#protected-locations). |

When files change on disk, the basket checks its items again about a second after things settle: sizes follow, and items that disappeared are marked.

## Move to trash

**Move to trash** is the main button. It runs at once, without asking.

- A message shows the progress ("Moving to trash 1,204 / 18,406…") with **Stop**. Stopping keeps what was done; the rest stays in the basket.
- At the end: "Moved {n} items to trash ({size})". Items that failed stay in the basket with the reason; the log panel (<kbd>Ctrl</kbd>+<kbd>J</kbd>) lists them.
- **Undo (5 s)** puts everything back where it was, and back into the basket. An item whose original place is taken by a new file with the same name is not overwritten ("An item with the same name is at the original location; not overwritten").

:::caution[Moving to the trash never turns into a permanent deletion]
When an item can't go to the trash, SpaceKnife leaves it alone and tells you why. It stays in the basket, and only **Delete permanently…** can remove it. This happens with:

- network locations and removable disks, which have no trash;
- a volume whose trash is set to remove files immediately;
- an item too large for the trash ("Too large for the trash (…, the trash holds at most …)") or with a path too long for it;
- on Linux, a file that can only reach a trash on another mount point. SpaceKnife follows the freedesktop.org trash specification and always moves by renaming, never by copying.

On Windows there is a second guard: if the system starts to delete an item instead of recycling it, SpaceKnife stops it before anything is touched ("The system was about to delete it instead of moving it to trash; blocked, nothing was changed").
:::

:::note
The trash usually lives on the same volume, so moving to it frees nothing yet: free space stays the same and the trash folder grows. Empty the trash in your system to get the space back.
:::

## Delete permanently

**Delete permanently…** skips the trash.

1. A dialog asks "Delete {n} items ({size}) permanently?": "They skip the trash and can't be restored from it. You have 5 seconds to undo before it starts." **Cancel** has the focus, so <kbd>Enter</kbd> cancels.
2. After you confirm, a countdown shows "{n} items will be deleted permanently" with **Undo (5 s)**. Nothing is touched yet. **Undo** calls it off, and the items stay in the basket. Closing the message does not call it off.
3. Then the items are deleted, with progress and **Stop**. Stopping halfway through a folder can leave part of it deleted ("Canceled; part of it was already deleted").
4. At the end the message shows what was freed: "Deleted {n} items permanently, {size} freed".

Only one file operation runs at a time.

## Right before deleting

Whichever way you delete, every item is checked once more right before it is touched: it must still exist, still not be protected, and not be in use. Items that fail a check are skipped and stay in the basket.

Afterwards the map follows: with [live monitoring](../monitoring/) on, the changes arrive as file changes (blocks disappear, parents flash); with it off, SpaceKnife reads the affected parent folders again.

## Duplicate files

**Find duplicate files** finds files with identical contents, sorts them by the space you get back, and puts the extra copies in the delete basket once you have chosen which copy to keep. It never starts by itself: use **Find duplicate files…** in the command palette, or right-click a file for **Find files identical to it**, or a folder for **Does this folder exist elsewhere?**. Snapshots can't be searched: they show the past, so their contents can't be checked.

- **It reads very little.** Only files that share their size with another file are read (a file whose size is unique is not read at all): a few kilobytes at the head and tail first, and the whole file only while the copies still match, stopping at the first difference. By default only files of 1 MB or more are compared (**Settings › Scan › Duplicate files**), and system and program folders, recycle bins and network drives are left out. The search keeps to the disk load limit (**Quiet** by default), can be paused and stopped, and waits while a scan of the same drive runs.
- **Files are duplicates only after their whole contents were read and found identical.** The check uses BLAKE3-256 hashes and compares file contents only: not names, times, permissions or alternate data streams. Files still being checked are drawn with grey stripes and never called duplicates.
- **Not counted as duplicates:** hard links of one file, block clones (ReFS, Btrfs and others) and one file seen through two paths are "the same data": deleting one frees nothing, so they are never offered for deleting. Cloud placeholder files are not read: reading one would download it, and deleting it frees no local space.
- **Your files are left as they are.** Reads leave access times alone (a file where the system does not allow that is skipped by default), and nothing is written. If access times still change after reading, an antivirus most likely checked the files when they were opened; the panel says so.
- **A second search reads almost nothing.** What was verified is kept in a cache (by file ID, size and modification time, without file names), so unchanged files only have their metadata looked at; renamed or moved files are read once more.

**The panel** takes the right-hand column: the space to free (by volume), the progress (same size → after merging names of one file → head and tail read → identical groups), the disk load, pause and stop, then the groups sorted by the space they free (each copy with its role: keep, extra, same file…) and a **Folders** page (the folders holding the most extra copies). Hovering over a group lights its copies in every pane. The coloring switch gains **Duplicates**: extra copies are magenta stripes, the copy kept has an outline, and folders are colored by their share of extra copies.

**Which copy is kept**: **Oldest created** by default, or **Newest modified** or **Shortest path**. **Keep copies on the same side of a folder pair** keeps all copies between two folders on one side, so the other side can be emptied as a whole afterwards. At least one copy of each group always stays. **Keep this one** on a copy picks it by hand.

**Into the delete basket**: **Put the N extra copies in the delete basket** adds one item per volume. Right before deleting, each copy is checked again: if it changed, if the copy kept changed or is gone, if the copy kept is among what is being deleted too, or if the copy kept is open in another program, that group is skipped and nothing in it is touched.

**Compare across drives** (off by default) puts all open, finished scans together. There are no hard links or block clones between volumes, so every copy takes its own space; but copies on other drives are often deliberate backups, so by default one copy stays on each volume and copies on other volumes are not preselected.

**Does this folder exist elsewhere?** shows a bar: how much of the folder also exists elsewhere, how much only here, and how much was not checked (too small, for example). It only informs: it never deletes a whole folder.

## Protected locations

Some places can't go into the basket at all. Adding one shows "Can't delete: …" with the reason. "Itself" below means the folder can't be deleted, but single items inside it can. Links in a path are resolved first, so a link that leads into a protected folder does not open a way around the list.

| | Protected |
|---|---|
| Everywhere | The root of a volume. A folder where another volume is mounted. SpaceKnife's own data folders and the folder of the running program. |
| Windows | `C:\Windows` and everything in it (except the contents of `C:\Windows\Temp`). `Program Files`, `Program Files (x86)` and `ProgramData` themselves. `C:\Users` itself, every home folder itself, and your `AppData`, `AppData\Local`, `AppData\LocalLow`, `AppData\Roaming` themselves. `Recovery`, `Boot`, `EFI`, `$WinREAgent`, `bootmgr` and `BOOTNXT` on the system drive. `pagefile.sys`, `hiberfil.sys`, `swapfile.sys`, `$Recycle.Bin` and `System Volume Information` on any volume. |
| Linux | `/bin`, `/boot`, `/dev`, `/etc`, `/lib`, `/lib32`, `/lib64`, `/libx32`, `/proc`, `/run` (except what is inside `/run/media`, `/run/user`, `/run/mount`), `/sbin`, `/sys`, `/usr`, `/efi` and everything in them. The package databases in `/var/lib/dpkg`, `/var/lib/rpm`, `/var/lib/dnf`, `/var/lib/pacman`. `/var`, `/var/lib`, `/var/log`, `/var/cache`, `/var/tmp`, `/tmp`, `/opt`, `/srv`, `/mnt`, `/media`, `/snap`, `/lost+found` themselves. `/home` itself, every home folder itself, `/root`. Your `~/.config`, `~/.local`, `~/.local/share`, `~/.local/state` themselves. Trash folders. |
| macOS | `/System`, `/bin`, `/sbin`, `/usr`, `/dev`, `/etc`, `/private/etc`, `/private/var/db`, `/private/var/vm`, `/Library/Apple`, `/cores` and everything in them. `/Applications`, `/System/Applications`, `/Library`, `/Volumes`, `/private`, `/private/var`, `/private/tmp`, `/var`, `/tmp`, `/opt` themselves. `/Users` itself, every home folder itself. Your `~/Library`, `~/Library/Application Support`, `~/Library/Mobile Documents`, `~/.config`, `~/.local`, `~/.local/share`, `~/.local/state` themselves. `~/.Trash` and `.Trashes`. |

:::tip
Trash folders are protected on purpose: "Trash folder: use the system's "Empty trash" instead".
:::

## Known folders

SpaceKnife has a built-in library of about 110 rules for well-known folders and files: what they are and whether they can be cleaned. Today you see it in [snapshot comparisons](../snapshots-compare/): a changed item that matches a rule gets a short note after its name. Some examples:

| Kind | Notes you may see |
|---|---|
| Virtual machines and containers | Docker data, WSL disk, Virtual machine, Emulator |
| Development | Dependency install (`node_modules`), Virtual environment, Package cache, Build cache, Toolchain, IDE cache |
| Browsers and apps | Browser cache, App cache, WeChat data, Telegram cache |
| Games | Steam library, Game update, Shader cache, Workshop |
| AI models | LLM models, Model cache |
| System | System update, System upgrade, Hibernation file, Virtual memory, System components, Restore points, Temporary files, Thumbnail cache, Crash dumps, System logs |

A note says what something is, not that it is safe to delete. What SpaceKnife actually refuses to delete is the list of [protected locations](#protected-locations) above.

## Keys

| Key | Action |
|---|---|
| <kbd>Delete</kbd> | Add the selected item to the delete basket |
| <kbd>Shift</kbd>+<kbd>F10</kbd> / <kbd>Menu</kbd> | Context menu of the selected item |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy the path (As is) |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Show in Explorer / Finder / file manager |
| <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> | Open terminal here |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>–<kbd>4</kbd>, <kbd>Ctrl</kbd>+<kbd>0</kbd> | Tag red, yellow, green, blue; clear the tag (without <kbd>Ctrl</kbd> too, while the map has the focus, unless switched off in the tag settings) |
| <kbd>Enter</kbd> | Zoom into the selected item |
| In the results list: <kbd>Delete</kbd>, <kbd>Space</kbd>, <kbd>Enter</kbd> | Add to the basket, show on the map, zoom in |

On macOS, <kbd>⌘</kbd> works wherever <kbd>Ctrl</kbd> is listed. All keys are in [Keyboard shortcuts](../shortcuts/).

## Workflows

**Big files nobody touched for a year.** Press <kbd>/</kbd> and type `>1gb;>1y` (larger than 1 GB, modified more than a year ago). Click the first row of the results list, go down with <kbd>↓</kbd> and press <kbd>Delete</kbd> on what can go. Open the basket, sort **By location**, check, **Move to trash**.

**Only the clutter of one folder.** Zoom into the folder and right-click its small-items block. **Add to delete basket** takes exactly those files; the large files beside them stay.

**Before and after.** Save a snapshot before a big cleanup (command palette › **Save snapshot**, or <kbd>Ctrl</kbd>+<kbd>S</kbd> from 0.3.2). Afterwards, [compare](../snapshots-compare/) it with **Now** and switch to **Decrease** to see what really went.

**Hand a path to a terminal.** Select the item and use **Copy path › Quoted** for a shell, **POSIX path** for WSL, or press <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> to open a terminal right there.
