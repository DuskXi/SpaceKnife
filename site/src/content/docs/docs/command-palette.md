---
title: Command palette
description: Open the command palette with Ctrl+K to run commands, find files by name, go to a path, apply a filter and reuse recent items, and read what each command's status means.
---

The command palette is one text field that searches four things at once: commands, the files and folders of the current scan, a path you type, and a filter you type. Once you know it, you rarely need to look for a button.

## Open and close

- Press <kbd>Ctrl</kbd>+<kbd>K</kbd> (<kbd>⌘</kbd>+<kbd>K</kbd> on macOS), or click the Command palette button on the toolbar. It works on every tab (the start page, a scan, Settings) and while the focus is in the filter box, but not while you type in the start page's path field, the path editor, or the search, number and tag-name fields of Settings.
- To close it, press <kbd>Esc</kbd> or <kbd>Ctrl</kbd>+<kbd>K</kbd> again, click outside it, or run any row. The focus goes back to where it was, and the input is not kept.
- While the palette is open, the window behind it ignores the mouse and the keyboard. Live monitoring keeps updating the picture.

On a scan tab, the right end of the input shows the tab's scan target: the palette acts on this scan. Searching by name, filter previews and Rescan current view all use it. In a split view, the palette acts on the focused pane's view.

## What you type, what you get

| You type | The palette lists |
| --- | --- |
| Nothing | Recent (up to 5), the 9 common commands and, when something is selected, other items whose name contains the selection's name |
| A command name or a word, like `rescan` or `snap` | Matching commands (up to 20), and the files and folders of this scan with a matching name |
| A path starting with a drive letter, `\\`, `/` or `~` | A Go to row for that path, then the items inside that folder |
| Text with `*`, `?`, `;`, `\|`, `<`, `>`, `:`, `"`, or a single leading `\` | A Filter row with live totals, then the files and folders whose name matches the wildcard |

Results come in groups, always in this order: the direct row (Go to or Filter), Recent, Commands, Paths. A group with nothing in it is hidden. When nothing matches, you see "No matching commands or paths".

## Keys

| Keys | Action |
| --- | --- |
| <kbd>↑</kbd>, <kbd>↓</kbd> | Move the highlight, wrapping around at the ends |
| <kbd>PageUp</kbd>, <kbd>PageDown</kbd> | Move 10 rows at a time |
| <kbd>Enter</kbd> | Run the highlighted row |
| <kbd>Tab</kbd> | Complete, see below |
| <kbd>Esc</kbd> | Close |

- When the palette opens or the input changes, the highlight goes to the first row that can run. Usually you just type and press <kbd>Enter</kbd>.
- If you press <kbd>Enter</kbd> before the results for your input arrive, the palette waits for them and then runs the first row that can run (the direct row, then Recent, then Commands). It never runs a row left over from your previous input.
- Moving the mouse over a row highlights it; a click runs it.
- While an input method is composing, <kbd>Enter</kbd>, the arrow keys and <kbd>Esc</kbd> belong to the input method.

<kbd>Tab</kbd> completes from the highlighted row:

| Highlighted row | What <kbd>Tab</kbd> does |
| --- | --- |
| A command | Puts the command name into the input |
| A path (the Go to row or the Paths group) | Completes the path. Folders get a trailing separator, so you can type on or press <kbd>Tab</kbd> again |
| A filter | Completes the word at the cursor, like the completion of the filter box |
| A recent item | Puts it into the input; folders get a trailing separator |

## Search commands

A command matches when the letters you type appear in its name in that order, not necessarily next to each other. Spaces are ignored. Runs of letters, word starts and a match at the very start rank higher; an exact name or a prefix ranks highest. The matched letters are shown in bold.

You can find a command by:

- **Its name**: `snapshot`, `tag`.
- **Its other words**: `rescan`, `terminal`, `trash`. The table at the end of [Keyboard shortcuts](../shortcuts/) lists them for every command.
- **Its Chinese name and pinyin**: the pinyin initials `qhps` find Cycle coloring (切换配色) in either interface language.

For equally good matches, a hit in the name ranks above a hit in the Chinese name or pinyin, and those above the other words.

### Commands listed only for a typed query

"Coloring: file classes", "Coloring: flat", "Coloring: date modified", "Coloring: top-level branch" and "Visualization: treemap", "Visualization: flame graph", "Visualization: polar flame graph" are not in the list for an empty input, nor in the key map in Settings. They show up when you type a word like `color`, `color flat` or `treemap`, and pick that coloring or visualization directly, instead of cycling like <kbd>C</kbd> and <kbd>V</kbd>.

### The 9 commands of an empty input

With nothing typed, the Commands group always lists: Rescan current view, Rescan everything, Cycle coloring, Show/hide free space, Open delete basket, Save snapshot, Compare with snapshot…, Export report…, Settings. This list and its order are fixed. Each row shows its key on the right, so the group doubles as a cheat sheet.

### Status and why a command is unavailable

The gray text after a command name is its current status, worked out when the palette opens and refreshed every 2 seconds while a scan runs. For example:

| Command | Status |
| --- | --- |
| Rescan everything | How the current scan reads the disk, "MFT quick scan" or "Full scan"; once the scan is done, with the time it took, like "MFT quick scan · about 5 s" |
| Cycle coloring, Cycle visualization | "Current: File classes", "Current: Treemap" |
| Show/hide free space, Show/hide unknown space | "Current: shown" or "Current: hidden" |
| Open delete basket | The number of items in the delete basket, or "Empty" |
| Compare with snapshot… | How many snapshots this scan target has, like "3 snapshots" |
| Export report… | "CSV · JSON · HTML" |
| Check for updates | "Checking…", "… available" or "Current version …" |
| Coloring: …, Visualization: … | "Current" on the one in use |

A command that can't run now is grayed out, its reason replaces the status, and <kbd>Enter</kbd> does nothing. The usual reasons:

| Reason | Meaning |
| --- | --- |
| Needs a finished scan | This tab has no scan results yet (on the start page or in Settings, for example) |
| A scan is running | Wait until the scan ends, for example to rescan or save a snapshot |
| No scan is running, The scan isn't paused, Paused | The scan isn't in a state where you can pause, resume or stop it |
| MFT quick scans finish in seconds; no need to pause | An MFT quick scan can't be paused |
| No scan to continue | Continue the stopped scan is only for a full scan that was stopped with folders left to read |
| Nothing selected | Zoom in, Copy path, tags and the like need a selected block |
| Nothing to go back to, Nothing to go forward to, Already at the root | You are at the end of the navigation |
| No snapshots | No snapshot has been saved for this place yet |
| This window can't show this visualization | The window, or a snapshot comparison, can't show that visualization |
| No update available | Show update is available only after a newer version was found |

A command whose name ends in "…" asks one more thing: Compare with snapshot… lets you pick a snapshot, Export report… opens the export dialog, Open snapshot file… asks for a file. Preview (<kbd>Space</kbd>) isn't implemented yet and shows as unavailable.

## Find files and folders by name

On a scan tab, text that is neither a path nor a filter searches every name in the scan. The Paths group ranks the hits in tiers:

1. The name is exactly what you typed
2. The name starts with it
3. The name contains it
4. Its letters appear in the name in order (for 3 or more letters; for Chinese and other non-Latin text, from 2 characters)

Within a tier, larger items come first. Latin letters match in any case. Up to 50 items are listed, and the group header shows "Showing 50 / total". Each row has the name (matched letters in bold), its folder and its size. <kbd>Enter</kbd> zooms to it; for a file, to its folder with the file selected.

- With `*` or `?` in the input, names are matched as a wildcard, by the same rules as name conditions in filters, and a Filter row appears on top.
- While the scan is still running, the header warns "Scan not finished; results may be incomplete".
- Deleted items don't show up.

:::tip[Find items with the same name]
Select a file or folder in the picture, press <kbd>Ctrl</kbd>+<kbd>K</kbd> and type nothing. The Paths group header says "Matching the selection "name"" and lists other items in the scan whose name is or contains that name (up to 5). Copies spread over the disk and cache folders of the same name show up at once.
:::

## Go to a path

Input that starts with a drive letter (`C:`, `D:\`), `\\` (a network path), `/` or `~` (your home folder) is a path. On Windows, `/` works as a separator too.

The first row, Go to, tells you about the path:

| Case | Shows | <kbd>Enter</kbd> |
| --- | --- | --- |
| In this scan | Its size on the right | Zooms to it; for a file, to its folder with the file selected |
| On disk, but outside this scan | "Outside the current scan" and "Scan this location" | Scans that place in a new tab. Typing a path on the start page or in Settings works the same way |
| Doesn't exist | "Path doesn't exist" | Can't run |

The Paths group below lists the items inside that folder that match the last, partly typed part (starts with, contains, letters in order). With nothing typed after the last separator, it lists the largest items.

<kbd>Tab</kbd> completes one part at a time: among the items of the typed folder that start with the last part, folders come first and larger ones before smaller, and a folder gets a trailing separator. Press <kbd>Tab</kbd> again and again to walk down:

```text
C:\Users\lenny\Vi      → Tab → C:\Users\lenny\Videos\
~/Down                 → Tab → /home/lenny/Downloads/
```

Paths inside the scan complete from the scan results; paths outside it complete from what is on disk.

## Apply a filter

When the input contains filter syntax, the first row is Filter. The condition is colored like in the filter box, and the right side shows "N items · total size", worked out on the current scan as you type. A condition with an error shows the error in red, and the row can't run.

<kbd>Enter</kbd> puts the condition into the focused view's filter box and applies it, just as if you had typed it there. For example:

```text
*.iso;>1gb
:class:video;>500mb
```

The filter syntax is described in [Filtering](../filtering/).

## Recent

The palette remembers two kinds of things you run from it: going to a path, and applying a filter.

- Running an item again moves it to the top; nothing is listed twice.
- Paths are remembered per scan target; filters are shared by all scans.
- They are kept across restarts, up to 50 of each kind. An empty input shows the newest 5; as you type, matching recent items are listed too.
- A path that no longer exists in the current scan shows "No longer exists" and can't run. A recent filter shows its totals on the current scan.
- Clear, at the right of the Recent header, empties this scan target's paths and all recent filters at once, without asking.

Recent items run only on a scan tab.

## Advanced examples

| To | Type |
| --- | --- |
| Scan a new place | `~/Downloads` or `D:\Games`, then <kbd>Enter</kbd> (outside the current scan, the row offers Scan this location) |
| See the largest items of a folder | `C:\Users\lenny\`, with nothing after the last separator |
| Preview a filter before you apply it | `*.iso;>1gb`: read "N items · total size", then <kbd>Enter</kbd> |
| Switch straight to one coloring | `color time`, then pick "Coloring: date modified" |
| Find the biggest folders with one name | `node_modules`: the largest come first |
| Rescan the folder you are zoomed into | `rescan`, then pick Rescan current view |

For every command and its key, see [Keyboard shortcuts](../shortcuts/).
