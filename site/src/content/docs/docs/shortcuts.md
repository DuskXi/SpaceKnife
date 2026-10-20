---
title: Keyboard shortcuts
description: Every key and mouse action in SpaceKnife, grouped by where you use it, with the macOS differences and the full list of palette commands.
---

You can do almost everything in SpaceKnife from the keyboard. This page lists every key by where you use it, and ends with the keys of every command in the command palette.

:::tip[Forgot a key? Press Ctrl+K]
The command palette lists every command with its key. Type a few letters to find one and run it. See [Command palette](../command-palette/).
:::

The keys below are for Windows and Linux. For macOS, see [Differences on macOS](../shortcuts/#differences-on-macos).

## When keys work

- **Single keys never fire in a text field.** <kbd>/</kbd>, <kbd>F</kbd>, <kbd>U</kbd>, <kbd>C</kbd>, <kbd>V</kbd>, the digits, <kbd>Enter</kbd>, <kbd>Delete</kbd>, <kbd>Backspace</kbd> and <kbd>Esc</kbd> act on the view only when the focus is not in a text field. Typing `f` in the filter box doesn't hide free space, and <kbd>Ctrl</kbd>+<kbd>C</kbd> in a text field is the normal copy.
- **Global keys also work in the filter box.** You don't have to leave it for <kbd>Ctrl</kbd>+<kbd>K</kbd>, <kbd>Ctrl</kbd>+<kbd>J</kbd>, <kbd>Ctrl</kbd>+<kbd>T</kbd>, <kbd>Ctrl</kbd>+<kbd>W</kbd>, <kbd>Ctrl</kbd>+<kbd>,</kbd> or <kbd>Ctrl</kbd>+<kbd>Tab</kbd>. Some text fields keep every key for typing, so global keys don't work in them: the start page's path field, the path editor (<kbd>Ctrl</kbd>+<kbd>L</kbd>), and the search, number and tag-name fields of Settings.
- **View keys act on the focused pane.** In a split view, the pane with the accent border has the focus. Click the other pane to move the focus there.
- **A focused control keeps its own keys.** On a button, radio button or menu item, <kbd>Enter</kbd>, <kbd>Delete</kbd>, <kbd>Esc</kbd> and <kbd>Shift</kbd>+<kbd>F10</kbd> belong to that control (<kbd>Enter</kbd> on a button presses it). On a drop-down list, <kbd>C</kbd> and <kbd>V</kbd> pick options by their first letter.
- **Dialogs block the window behind them.** While the command palette is open, only <kbd>Ctrl</kbd>+<kbd>K</kbd> (close the palette) and, from 0.3.2, <kbd>F1</kbd> (open the palette's help page) still work at window level. While the dialog of a major update is open, no shortcut works.

## Global

| Keys | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Open or close the command palette |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Open Settings, or switch to it when it is already open |
| <kbd>Ctrl</kbd>+<kbd>J</kbd> | Show or hide the log panel |
| <kbd>F1</kbd> | From 0.3.2: open the help on the page that explains where you are; the Help button in the title bar does the same |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | New tab (a "New scan" start page) |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Close the current tab. Closing a tab that is scanning stops that scan; closing the last tab leaves a new start page |
| <kbd>Ctrl</kbd>+<kbd>Tab</kbd>, <kbd>Ctrl</kbd>+<kbd>PageDown</kbd> | Next tab (after the last one, back to the first) |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd>, <kbd>Ctrl</kbd>+<kbd>PageUp</kbd> | Previous tab |

## Navigation

These work in a scan view, also while the scan is still running.

| Keys | Action |
| --- | --- |
| <kbd>Enter</kbd> | Zoom into the selected block. On a file, zoom to its folder and keep the file selected; on an "N small items" block, open that block |
| <kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>←</kbd> | Back to the previous place |
| <kbd>Shift</kbd>+<kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>→</kbd> | Forward |
| <kbd>Alt</kbd>+<kbd>↑</kbd> | Up one level |
| <kbd>Ctrl</kbd>+<kbd>Home</kbd> | Back to the root |
| <kbd>Ctrl</kbd>+<kbd>L</kbd> | Edit the path: the path on the toolbar turns into a text field. Type a path inside this scan and press <kbd>Enter</kbd> to go there, or <kbd>Esc</kbd> to cancel. Clicking the last part of the path does the same. Not available in split view or in a snapshot comparison; in split view, type the path in the command palette instead |
| <kbd>/</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd> | Put the focus in the filter box |

Back and forward remember where you zoomed and what was selected, like browser history. To go to a path outside the current scan, use the command palette: type the path and it offers "Scan this location". For more ways to move around, see [Navigation](../navigation/).

## Picture

| Keys | Action |
| --- | --- |
| <kbd>C</kbd> | Cycle the coloring: File classes → Flat → Date modified → Top-level branch → File classes. In a snapshot comparison, cycle what is shown instead: All → Growth → Decrease |
| <kbd>V</kbd> | Next visualization: treemap, flame graph, polar flame graph. In a snapshot comparison, only the ones that can show a comparison |
| <kbd>Shift</kbd>+<kbd>V</kbd> | Previous visualization |
| <kbd>F</kbd> | Show or hide the free space block (no effect in a snapshot comparison) |
| <kbd>U</kbd> | Show or hide the unknown space block (no effect in a snapshot comparison) |
| <kbd>Ctrl</kbd>+<kbd>=</kbd>, <kbd>Ctrl</kbd>+<kbd>+</kbd> | Raise the detail level, up to 15 |
| <kbd>Ctrl</kbd>+<kbd>-</kbd> | Lower the detail level, down to 1 |

The plus and minus keys of the numeric keypad work too. Coloring, visualization and detail level change the current view only; what new views start with is set under Appearance and Visualization in [Settings](../settings/). The visualizations are explained in [Visualizations](../visualizations/).

## Selection and tags

| Keys | Action |
| --- | --- |
| <kbd>Esc</kbd> | Clear the selection. In a snapshot comparison with nothing selected, leave the comparison |
| <kbd>Delete</kbd> | Add the selected file or folder to the delete basket (for an "N small items" block, use the context menu) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy the full path of the selection |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Show in Explorer (Finder on macOS, the file manager on Linux) |
| <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> | Open terminal here: in the selected folder, or in the folder of the selected file |
| <kbd>Shift</kbd>+<kbd>F10</kbd>, Menu key | Open the context menu of the selected block, next to it. With nothing selected, open the menu of the flame graph's root bar or the polar flame graph's center disc. The More actions button of the details panel does the same |
| <kbd>Ctrl</kbd>+<kbd>1</kbd> … <kbd>Ctrl</kbd>+<kbd>4</kbd> | Tag red ▲, yellow ■, green ●, blue ◆ |
| <kbd>Ctrl</kbd>+<kbd>0</kbd> | Clear the tag |
| <kbd>1</kbd> … <kbd>4</kbd>, <kbd>0</kbd> | The same, without <kbd>Ctrl</kbd> |

The digits without <kbd>Ctrl</kbd> work while the setting Tags › Shortcuts › "Ctrl is optional when the treemap has focus" is on, which is the default. Turn it off and only <kbd>Ctrl</kbd> plus a digit tags. For what tags are good for, see [Tags and file classes](../tags-classes/); for the delete basket, see [Cleanup](../cleanup/).

:::note
NTFS metadata and orphaned files appear only in an MFT quick scan and have no place on disk: <kbd>Ctrl</kbd>+<kbd>E</kbd> and <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> only show a message for them.
:::

## Context menu

Right-click a block, or press <kbd>Shift</kbd>+<kbd>F10</kbd>. The focus moves into the menu.

| Keys | Action |
| --- | --- |
| <kbd>↑</kbd>, <kbd>↓</kbd> | Move up and down |
| <kbd>Enter</kbd> | Run the item. On Copy path and Add tag, open the submenu |
| <kbd>→</kbd> | Open the submenu |
| <kbd>←</kbd> | Back from the submenu |
| <kbd>Esc</kbd> | Close the submenu, or the menu when no submenu is open |

The items are: Open, Show in Explorer, Open terminal here, Copy path ▸, Add tag ▸, Show only this (zoom in), Rescan this folder, Add to delete basket, and System menu… (not wired up yet). The Copy path submenu has four forms, each previewed: As is (what <kbd>Ctrl</kbd>+<kbd>C</kbd> copies), Quoted, POSIX path and `file://` URI. A click outside the menu, resizing the window or switching to another window closes it.

## Matches list

When a filter is active, the sidebar shows Matches. Click the list, or move the focus into it with <kbd>Tab</kbd>:

| Keys | Action |
| --- | --- |
| <kbd>↑</kbd>, <kbd>↓</kbd> | Move up and down; the block lights up in the picture |
| <kbd>Home</kbd>, <kbd>End</kbd> | First item, last item |
| <kbd>PageUp</kbd>, <kbd>PageDown</kbd> | One screen up or down |
| <kbd>Space</kbd> | Locate the item in the picture |
| <kbd>Enter</kbd> | Zoom in |
| <kbd>Delete</kbd> | Add to the delete basket (files and folders) |

With the mouse: click to locate, double-click to zoom in, right-click for the same context menu. Keys with <kbd>Ctrl</kbd> or <kbd>Alt</kbd> still go to the view, so <kbd>Alt</kbd>+<kbd>↑</kbd> in the list goes up a level to search a wider area.

## Filter box

| Keys | Action |
| --- | --- |
| <kbd>/</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd> | Go to the filter box |
| <kbd>Enter</kbd> | Apply now (the filter also applies by itself as you type) |
| <kbd>↑</kbd>, <kbd>↓</kbd> | Pick a suggestion in the completion list |
| <kbd>Tab</kbd> | Complete with the highlighted suggestion |
| <kbd>Esc</kbd> | In turn: close the completion list → clear the filter → leave the filter box |

The filter syntax is described in [Filtering](../filtering/).

## Log panel

<kbd>Ctrl</kbd>+<kbd>J</kbd> shows or hides it. While the log list has the focus, the keys below stay in the log, also with <kbd>Ctrl</kbd> or <kbd>Alt</kbd> held, and so do letters, digits, <kbd>Delete</kbd>, <kbd>Backspace</kbd> and <kbd>Esc</kbd>:

| Keys | Action |
| --- | --- |
| <kbd>↑</kbd>, <kbd>↓</kbd>, <kbd>Home</kbd>, <kbd>End</kbd> | Pick a line |
| <kbd>Enter</kbd> | Locate the line's path in the picture (switching to that scan's tab) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy the line's path |

Other shortcuts, such as <kbd>Alt</kbd>+<kbd>←</kbd>, <kbd>Ctrl</kbd>+<kbd>E</kbd> or <kbd>Shift</kbd>+<kbd>F10</kbd>, still act on the view behind the log.

## Command palette

| Keys | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> | Open; press again to close |
| <kbd>↑</kbd>, <kbd>↓</kbd> | Move up and down (wrapping around at the ends) |
| <kbd>PageUp</kbd>, <kbd>PageDown</kbd> | Move 10 rows at a time |
| <kbd>Enter</kbd> | Run the highlighted row |
| <kbd>Tab</kbd> | Complete: put the command name, path or filter into the input to keep editing |
| <kbd>Esc</kbd> | Close; the focus goes back to where it was |

While an input method is composing, <kbd>Enter</kbd>, the arrow keys and <kbd>Esc</kbd> belong to the input method. See [Command palette](../command-palette/).

## Start page and scanning

| Keys | Action |
| --- | --- |
| <kbd>Enter</kbd> | Start scan (unless another button has the focus: then it presses that button). <kbd>Enter</kbd> in the path field does the same |
| <kbd>←</kbd>, <kbd>→</kbd>, <kbd>↑</kbd>, <kbd>↓</kbd> | Pick another volume in the volume list (left and right one at a time, up and down one row) |
| <kbd>←</kbd>, <kbd>→</kbd> (or <kbd>↑</kbd>, <kbd>↓</kbd>) | With the focus on Disk load limit in the scan details, switch to the next level |

## Settings tab

| Keys | Action |
| --- | --- |
| <kbd>/</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd> | Go to Search settings |
| <kbd>Esc</kbd> (in the search box) | Clear the search; when it is empty, leave the search box |
| <kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>←</kbd> | Back to the settings page you saw before |
| <kbd>Alt</kbd>+<kbd>→</kbd> | Forward |

The global keys work as usual. How the settings tab works is described in [Settings reference](../settings/).

## Help tab

From 0.3.2 the help has one tab, like Settings. <kbd>F1</kbd> opens it.

| Keys | Action |
| --- | --- |
| <kbd>/</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd> | Go to Search help |
| <kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>←</kbd> | Back to the place you saw before |
| <kbd>Alt</kbd>+<kbd>→</kbd> | Forward |

## Dialogs

<kbd>Esc</kbd> closes Export report, the update prompts and the macOS Full Disk Access prompt (the same as Cancel). The preview of a settings import closes with Cancel or a click outside it.

## Mouse

| Action | Result |
| --- | --- |
| Click a block | Select it; the details panel shows it. Click empty space to clear the selection. A click on the flame graph's root bar or the polar flame graph's center disc shows the zoom root in the details panel |
| Double-click a block | Zoom in. Double-clicking a file zooms to its folder and selects the file |
| Double-click the root bar or center disc | Up one level (flame graph, polar flame graph) |
| Right-click a block | Select it and open the context menu |
| Hover | Show the hover card; adjust it under General › Hover tips in [Settings](../settings/) |
| Click a part of the path | Go to that level; click the last part to edit the path |
| Middle-click a tab | Close the tab |
| Double-click empty space in the title bar | Maximize or restore the window |
| Drop folders on the start page | Add them to the scan path; drop a `.sks` snapshot file to open it |

## Differences on macOS

- **Both ⌘ and Control work.** Wherever this page says <kbd>Ctrl</kbd>, you can press <kbd>⌘</kbd> or <kbd>Control</kbd> on macOS. The command palette and the key map in Settings show keys with ⌘, ⌥ and ⇧, for example <kbd>⌘</kbd><kbd>K</kbd> and <kbd>⌥</kbd><kbd>←</kbd>.
- **Alt is Option (⌥).**
- **The delete key of a Mac keyboard is Backspace.** <kbd>delete</kbd> goes back; to add to the delete basket, press <kbd>fn</kbd>+<kbd>delete</kbd> (forward delete).
- **Use fn when there are no Home, PageUp and PageDown keys.** <kbd>fn</kbd>+<kbd>←</kbd> is Home, <kbd>fn</kbd>+<kbd>↑</kbd> and <kbd>fn</kbd>+<kbd>↓</kbd> are PageUp and PageDown, so Back to the root is <kbd>⌘</kbd>+<kbd>fn</kbd>+<kbd>←</kbd>.
- **Switch tabs with Control+Tab.** <kbd>⌘</kbd>+<kbd>Tab</kbd> is the system's app switcher. <kbd>⌘</kbd>+<kbd>fn</kbd>+<kbd>↑</kbd> and <kbd>↓</kbd> work too.
- **Open a terminal with Control+&#96;.** By default, the system uses <kbd>⌘</kbd>+<kbd>&#96;</kbd> to switch between windows of the same app.
- **Function keys:** when the function keys act as media keys, add <kbd>fn</kbd> to <kbd>Shift</kbd>+<kbd>F10</kbd> (and, from 0.3.2, to <kbd>F1</kbd>).
- **Context menu:** Mac keyboards have no Menu key. Press <kbd>Shift</kbd>+<kbd>F10</kbd>, Control-click, or tap with two fingers.

## Keys that don't work yet

:::caution
The command palette and the key map under Settings › Shortcuts also list <kbd>F5</kbd> (Rescan current view), <kbd>Shift</kbd>+<kbd>F5</kbd> (Rescan everything) and <kbd>Ctrl</kbd>+<kbd>S</kbd> (Save snapshot), but in this version these keys do nothing yet. Run those commands from the command palette. To rescan one folder, you can also use Rescan this folder in the context menu. <kbd>Space</kbd> for a preview, shown on the hover card, isn't implemented yet.
:::

You can't change the keys yet: Settings › Shortcuts › Key preset is grayed out. The key map below it lists every current key, and you can search it by command name or key.

## Every command in the palette

This table comes from the same command list as the command palette and the key map under Settings › Shortcuts. "Also found by" lists the words that find a command besides its name. The Chinese name and its pinyin work as well, in either interface language.

<!-- generated:shortcuts:start -->

### Listed when the input is empty

When you open the palette and type nothing, the Commands group lists these, in this order:

| Command | Keys | Also found by | Notes |
| --- | --- | --- | --- |
| Rescan current view | <kbd>F5</kbd> | `rescan` · `refresh` | <kbd>F5</kbd> not active yet |
| Rescan everything | <kbd>Shift</kbd>+<kbd>F5</kbd> | `rescan all` · `full rescan` | <kbd>Shift</kbd>+<kbd>F5</kbd> not active yet |
| Cycle coloring | <kbd>C</kbd> | `color` · `colour` · `theme` | — |
| Show/hide free space | <kbd>F</kbd> | `free space` | — |
| Open delete basket | — | `basket` · `trash` · `delete` | — |
| Save snapshot | <kbd>Ctrl</kbd>+<kbd>S</kbd> | `save snapshot` | <kbd>Ctrl</kbd>+<kbd>S</kbd> not active yet |
| Compare with snapshot… | — | `compare` · `diff` | — |
| Export report… | — | `export` · `report` · `csv` · `json` · `html` | — |
| Settings | <kbd>Ctrl</kbd>+<kbd>,</kbd> | `settings` · `preferences` · `options` | — |

### Listed once you type

Every other command shows up when you type its name or one of the words below:

| Command | Keys | Also found by | Notes |
| --- | --- | --- | --- |
| Back | <kbd>Backspace</kbd> · <kbd>Alt</kbd>+<kbd>←</kbd> | `back` | — |
| Forward | <kbd>Alt</kbd>+<kbd>→</kbd> | `forward` | — |
| Up one level | <kbd>Alt</kbd>+<kbd>↑</kbd> | `up` · `parent` | — |
| Root | <kbd>Ctrl</kbd>+<kbd>Home</kbd> | `home` · `root` | — |
| Raise detail level | <kbd>Ctrl</kbd>+<kbd>=</kbd> | `more detail` · `deeper` | — |
| Lower detail level | <kbd>Ctrl</kbd>+<kbd>-</kbd> | `less detail` | — |
| Cycle visualization | <kbd>V</kbd> | `visualization` · `chart` · `treemap` · `flame` · `icicle` · `flame_polar` · `radial` · `rings` | — |
| Focus filter | <kbd>/</kbd> · <kbd>Ctrl</kbd>+<kbd>F</kbd> | `filter` · `find` | — |
| Edit path | <kbd>Ctrl</kbd>+<kbd>L</kbd> | `path` · `go to` · `address` | — |
| Zoom in | <kbd>Enter</kbd> | `zoom` · `enter` | — |
| Add to delete basket | <kbd>Delete</kbd> | `delete` · `remove` · `trash` | — |
| Preview | <kbd>Space</kbd> | `preview` · `quick look` | Planned |
| Copy path | <kbd>Ctrl</kbd>+<kbd>C</kbd> | `copy path` | — |
| Show in Explorer (Windows) / Show in Finder (macOS) / Show in file manager (Linux) | <kbd>Ctrl</kbd>+<kbd>E</kbd> | `reveal` · `explorer` · `finder` · `show in folder` | — |
| Open terminal here | <kbd>Ctrl</kbd>+<kbd>&#96;</kbd> | `terminal` · `shell` · `console` | — |
| New tab | <kbd>Ctrl</kbd>+<kbd>T</kbd> | `new tab` | — |
| Close tab | <kbd>Ctrl</kbd>+<kbd>W</kbd> | `close tab` | — |
| Red tag | <kbd>1</kbd> · <kbd>Ctrl</kbd>+<kbd>1</kbd> | `red tag` | — |
| Yellow tag | <kbd>2</kbd> · <kbd>Ctrl</kbd>+<kbd>2</kbd> | `yellow tag` | — |
| Green tag | <kbd>3</kbd> · <kbd>Ctrl</kbd>+<kbd>3</kbd> | `green tag` | — |
| Blue tag | <kbd>4</kbd> · <kbd>Ctrl</kbd>+<kbd>4</kbd> | `blue tag` | — |
| Clear tag | <kbd>0</kbd> · <kbd>Ctrl</kbd>+<kbd>0</kbd> | `clear tag` · `untag` | — |
| Show/hide unknown space | <kbd>U</kbd> | `unknown space` | — |
| Split view | — | `split` · `side by side` | — |
| Pause scan | — | `pause` | — |
| Resume scan | — | `resume` · `continue` | — |
| Stop scan | — | `stop` · `cancel` | — |
| Continue the stopped scan | — | `continue` · `go on` | — |
| New scan | — | `new scan` · `scan` | — |
| Open snapshot file… | — | `open snapshot` · `load` | — |
| Log panel | <kbd>Ctrl</kbd>+<kbd>J</kbd> | `log` · `logs` · `console` | — |
| Help | <kbd>F1</kbd> | `help` · `docs` · `documentation` · `manual` · `guide` | — |
| Check for updates | — | `check for updates` · `update` · `upgrade` · `new version` | — |
| Show update | — | `update` · `release notes` · `what's new` · `download update` | — |

### Listed only by name

These pick one coloring or visualization directly. They show up only for a typed query and are not in the key map in Settings:

| Command | Keys | Also found by | Notes |
| --- | --- | --- | --- |
| Coloring: file classes | — | `color class` | — |
| Coloring: flat | — | `color flat` | — |
| Coloring: date modified | — | `color time` · `age` | — |
| Coloring: top-level branch | — | `color branch` · `top level` | — |
| Visualization: treemap | — | `visualization treemap` · `treemap` | — |
| Visualization: flame graph | — | `visualization flame` · `flame graph` · `icicle` | — |
| Visualization: polar flame graph | — | `visualization polar` · `polar flame graph` · `radial` · `rings` | — |

<!-- generated:shortcuts:end -->
