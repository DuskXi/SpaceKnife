---
title: Settings reference
description: How the settings tab works (search, when changes apply, restoring defaults, import and export, editing the settings file by hand) and every setting with its values, default and key.
---

All of SpaceKnife's settings live in one Settings tab. Changes are saved at once; there is no Apply button. This page first explains the settings tab itself, then lists every setting page by page.

## Open Settings

- Press <kbd>Ctrl</kbd>+<kbd>,</kbd> (<kbd>⌘</kbd>+<kbd>,</kbd> on macOS), click the Settings button in the title bar, or run Settings from the [command palette](../command-palette/).
- There is only one Settings tab. When it is open, <kbd>Ctrl</kbd>+<kbd>,</kbd> switches back to it. It opens on the page you saw last.
- Done, at the bottom right, closes Settings and takes you back to the tab you came from.

The pages are listed on the left in three groups: General, Appearance, Visualization; Scan & performance, File classes, Tags; Shortcuts, Advanced. <kbd>Backspace</kbd> or <kbd>Alt</kbd>+<kbd>←</kbd> goes back to the page you saw before, <kbd>Alt</kbd>+<kbd>→</kbd> forward.

## Search settings

Press <kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>F</kbd> to go to Search settings on the toolbar. What you type is looked for, in any case and anywhere in the text, in:

- the name and description of each setting;
- its group and page name;
- the name and value of each option, so `1024` finds Size units;
- extra search words, such as `mft`, `tooltip`, `uac` or `inotify`. In the English interface, the Chinese name of each setting works too.

The results are listed as "page › group", with the matched text highlighted. On the left, each page shows how many hits it has, and pages without hits fade. You can change settings right in the results. <kbd>Esc</kbd> clears the search; press it again to leave the search box.

## When changes apply

Most settings apply as soon as you change them. Two kinds don't, and a short note under the setting says so:

| Note | Meaning |
| --- | --- |
| Applies from the next scan | Scan method, scope, hard link counting and the like. The note shows while a scan is running: the running scan isn't affected |
| Only affects views opened later | "New view…" settings and default colorings: views that are already open stay as they are |

- A grayed-out setting with a badge can't be changed on this platform or in this version; the badge says why.
- A setting that depends on another one is grayed out until that one allows it. For example, Manual concurrency can be changed only when Maximum concurrency is set to Manual.
- When the connection to the backend is lost, the top of the page says "Lost the connection to the backend. Settings can't be changed for now."
- The bottom left shows whether your change is saved: "Changes are saved automatically", "Saving…" or "Couldn't save: …".
- A number field takes only whole numbers in its range. For anything else it says "Enter a whole number from … to …" and goes back to the old value. A slider saves when you let go of it; a text field saves on <kbd>Enter</kbd> or when you leave it, without leading and trailing spaces.

## Restore defaults

- **One page**: Restore defaults, at the bottom, resets every setting of the current page that can be changed. Hover over the button to see how many settings it would reset. It is unavailable when the page is all defaults already, and while you search. Afterwards the bottom shows "Restored the defaults of "…" · Undo": click Undo within 5 seconds to get your values back.
- **Everything**: Advanced › Import & export › Restore all defaults… resets every setting and the file classes. It asks once more and can't be undone.

## Import and export

Both are under Advanced › Import & export.

**Export settings** saves every setting you changed and your custom file classes in one JSON file, named with the date by default, like `SpaceKnife-settings-20261020.json`. Only values that differ from the defaults are written.

**Import settings** reads such a file and shows what would change before anything happens:

- "Settings that will change": each setting with its old → new value;
- "File classes will become": the file classes after the import;
- "Not imported": what is skipped and why, for example unknown settings from another version, invalid values, or settings this version can't change.

Import applies it; Cancel changes nothing. An export lists only changed settings, so an import puts every setting the file doesn't mention back to its default; the preview lists those changes too. A file exported by a newer SpaceKnife (a higher format version) can't be imported.

**Import from SpaceSniffer** reads `SpaceSnifferConfig.xml` and converts its geometry, animation, behavior, appearance and file classes into the matching settings, again with a preview first. What has no matching setting, such as its colors, is listed under "Not imported" with the reason.

## Edit the settings file by hand

The settings are stored in `settings.json` in the config folder. Open settings file, at the bottom left, opens it with your default app, and Advanced › Files & diagnostics shows its full path. Where the folders are is described in [Data locations](../data-locations/).

The file holds only the values that differ from the defaults. The keys are the ones in the Key column of the reference below:

```json
{
  "version": 1,
  "values": {
    "general.language": "en",
    "scan.load_manual": "quiet",
    "scan.unit_base": "1000",
    "treemap.initial_detail": 7,
    "tags.name_red": "To delete"
  }
}
```

- Types must match exactly: switches are `true` or `false`; sliders and number fields take whole numbers in their range (`7`, not `7.0` or `"7"`); choices are the strings listed in the reference, and `"1024"` and `"60"` are strings too; text is up to 64 characters.
- SpaceKnife picks up changes to the file while it runs, within about a second, and an open settings tab follows.
- If the file is broken (not valid JSON, or a value isn't allowed), the top of the settings tab says "The settings file is invalid, so the last valid settings stay in use: …", and SpaceKnife keeps using the last valid settings until the file is fixed.
- Unknown keys are ignored. The next time you change a setting in the app, the file is written again from the current settings, with only the values that differ from the defaults.

:::tip
The settings tab offers four swatches for Accent color; in the settings file you can use any `#RRGGBB` color.
:::

Interface language also sets the language of the command line's output; see [Command line](../command-line/).

## More on some pages

Some pages hold more than the settings in the reference below:

- **File classes**: the file class editor. See [Tags and file classes](../tags-classes/).
- **Tags**: Saved tags › Clear all saved tags shows how many scanned places have saved tags, and clears them all after you confirm.
- **Shortcuts**: the Key map lists every command with its keys ("Unassigned" when it has none). Search commands or keys, above the map, filters it by command name or key: type `ctrl+` to list every key with Ctrl. Keys can't be changed yet; every key is explained in [Keyboard shortcuts](../shortcuts/).
- **Advanced › Scan helper & cache**: Scan helper shows whether the helper runs and whether it has administrator rights (it reads the MFT quick scans and the USN journal), with Stop scan helper. Snapshots shows how many there are, their total size and folder, with a button to open it. Cache shows the size and place of the cache; Clear cache… keeps the cache that is in use.
- **Advanced › Files & diagnostics**: the paths of the settings file and the log folder, each with Open, and Copy diagnostics, which copies the versions, platform and backend details to the clipboard for a problem report.

## All settings

Every setting, in the order of the settings pages. Default and Values use the interface's words; the code after a value is what goes into `settings.json`. A note like "Windows only." means the setting is grayed out on the other platforms.

What "Match system" means for two of them:

- **Interface language**: Simplified Chinese when the system language is any kind of Chinese, English otherwise.
- **Size units**: 1024 on Windows; 1000 on macOS; on Linux, 1000 on the GNOME desktop and 1024 on other desktops.

<!-- generated:settings:start -->

### General

Language, hover tips, window alerts and logs.

#### Language

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Interface language | Match system | Match system `"system"` · 简体中文 `"zh-CN"` · English `"en"` | All text, dates and number formats change at once. | `general.language` |

#### Hover tips

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Keep open when moving between blocks | On | On `true` · Off `false` | When the mouse moves from one block to another, the hover card switches to the new content. | `general.hover_keep_open` |
| Show dates and times | On | On `true` · Off `false` | The hover card shows the created, modified and accessed times. | `general.hover_dates` |
| Show file age | On | On `true` · Off `false` | Shown as a relative time, like "2 months ago". | `general.hover_age` |

#### Window

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Flash the window when a scan finishes in the background | On | On `true` · Off `false` | Flashes the taskbar on Windows, bounces the Dock icon on macOS, sets the urgent hint on Linux. | `general.flash_on_done` |

#### Log

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Open the log panel on new events | On | On `true` · Off `false` | Opens the log panel on a warning or an error. The same setting as the switch in the log panel. | `general.log_auto_open` |
| Log scan warnings | On | On `true` · Off `false` | For example "Can't access …: Access denied". | `general.log_scan_warnings` |

### Appearance

Theme, accent color, animation and the look of the visualizations. Changes apply at once.

#### Theme

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Theme | Match system | Match system `"system"` · Light `"light"` · Dark `"dark"` | With Match system, the theme follows your system's light or dark setting. | `appearance.theme` |
| Accent color | `"#8B9CFF"` | `"#8B9CFF"` · `"#D4FF3A"` · `"#5EC8B8"` · `"#F2A65A"` | Used by primary buttons, the current navigation item, switches, sliders and focus rings. | `appearance.accent` |

#### Animation

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Reduce motion | Match system | Match system `"system"` · On `"on"` · Off `"off"` | When on, zooming switches at once and the stripes stand still while scanning. | `appearance.reduce_motion` |

#### Treemap visuals

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Level contrast | 10 | 0 to 50 | How much the shade differs between neighboring levels (flat ↔ deep). | `appearance.level_contrast` |
| Border contrast | 10 | 0 to 50 | How strong block borders are (soft ↔ sharp). | `appearance.border_contrast` |
| Highlight halo levels | 3 | 0 to 10 | How many ancestor title bars light up on hover or selection (subtle ↔ vivid). | `appearance.halo_levels` |
| Hover highlight | On | On `true` · Off `false` | The block under the mouse gets an inner outline (white in the dark theme, near-black in the light one). | `appearance.hover_highlight` |
| Mouse trail | Off | On `true` · Off `false` | Blocks the mouse passes over fade out. | `appearance.mouse_trail` |
| Recycle Bin effect | Moving | Moving `"animated"` · Still `"static"` · Off `"off"` | What lies in the Recycle Bin / Trash is shaded and hatched inside a dashed outline, and the bin itself says how much emptying it frees. "Moving": the dashes march and a band of light sweeps across now and then (still with "Reduce motion"). | `appearance.trash_effect` |
| Selection shadow | On | On `true` · Off `false` | The selected block casts a shadow, so it is easier to follow. | `appearance.selection_shadow` |

#### Coloring

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Default treemap coloring | File classes | File classes `"class"` · Flat `"flat"` · Date modified `"time"` · Top-level branch `"branch"` | The same choice as the coloring buttons on the toolbar. Only affects views opened later. | `appearance.default_color_mode` |

#### Visualization

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Default visualization for new views | Treemap | Treemap `"treemap"` · Flame graph `"flame"` · Polar flame graph `"flame_polar"` | The same choice as the visualization buttons on the toolbar. Only affects views opened later. | `appearance.default_visual` |
| Default coloring for flame graphs and polar flame graphs | Top-level branch | File classes `"class"` · Flat `"flat"` · Date modified `"time"` · Top-level branch `"branch"` | Used when a new view switches to the flame graph or the polar flame graph. Only affects views opened later. | `appearance.partition_color_mode` |

### Visualization

Geometry, flame graph direction and animation. Open views are laid out again at once; "New view…" settings only affect views opened later.

#### Geometry

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Font size | 100 % | 80 to 140 %, step 5 | Title bar, bar and ring heights and the size at which labels appear scale with the font size. | `treemap.font_scale` |
| Minimum block size | 10 px | 3 to 20 px | Blocks smaller than this are merged into an "N small items" block. | `treemap.min_element` |
| Block proportions | 0 | -60 to 60, step 5 | Treemap only. Taller ↔ wider. 0 keeps blocks as square as possible. | `treemap.aspect` |
| Initial detail level | 5 | 1 to 15 | The detail level new views start with, the same as the level control on the toolbar. Only affects views opened later. | `treemap.initial_detail` |
| Show free space in new views | On | On `true` · Off `false` | Shows the free space block in whole-volume views. Only affects views opened later. | `treemap.show_free` |
| Show unknown space in new views | On | On `true` · Off `false` | Shows the unknown space block in whole-volume views. Only affects views opened later. | `treemap.show_unknown` |
| Show the Recycle Bin in new views | On | On `true` · Off `false` | Draws the Recycle Bin / Trash when the scan holds one. Off: the bin takes no room and the other blocks grow to fill the view. Only affects views opened later. | `treemap.show_trash` |
| Sort by size | On | On `true` · Off `false` | Larger blocks go to the top left within a folder. | `treemap.sort_by_size` |

#### Flame graph

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Flame graph direction | Root at bottom | Root at top `"top"` · Root at bottom `"bottom"` | Which side the root bar is on. | `treemap.flame_origin` |

#### Animation

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Zoom animation duration | 250 ms | 0 to 600 ms, step 25 | How long the transition takes when you double-click to zoom in or go back. 0 turns it off. | `treemap.zoom_ms` |
| Animation frame rate | Match display | Match display `"display"` · 60 fps `"60"` · 30 fps `"30"` | How often animations redraw. | `treemap.fps` |
| Rearrange while zooming (DynaZoom) | Off | On `true` · Off `false` | Blocks are rearranged live while zooming. Not available yet (shown grayed out). | `treemap.dynazoom` |
| Adapt the frame rate to interaction (DynaSmooth) | On | On `true` · Off `false` | Uses a high frame rate only while you interact, which saves power. | `treemap.dynasmooth` |

#### Change highlights

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Flash changed blocks | On | On `true` · Off `false` | Blocks flash while scanning or when their files change. | `treemap.flash_changes` |

### Scan & performance

Scan method, scope and hard link counting apply from the next scan. Everything else applies at once.

#### Scan method

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Default scan method | Auto | Auto `"auto"` · Quick MFT `"mft"` (Windows only) · Full scan `"hard"` | Auto and Quick MFT: a whole NTFS volume gets an MFT quick scan (asking once for administrator rights when needed, a full scan if refused), every other location a full scan. Applies from the next scan. | `scan.method` |
| Ask for administrator rights when needed | On | On `true` · Off `false` | Only the read-only scan helper is elevated, never the app itself. When off, a full scan is used without administrator rights. Windows only. | `scan.elevate` |

#### Disk load limit & concurrency

Limits the disk read load a scan may cause.

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Default for manual scans | Balanced 70% | Full speed `"max"` · Balanced 70% `"balanced"` · Quiet 35% `"quiet"` · Idle `"idle"` · Custom `"custom"` | Scans started from the start page, and rescans. Applies from the next scan. | `scan.load_manual` |
| Default for incremental rescans | Quiet 35% | Full speed `"max"` · Balanced 70% `"balanced"` · Quiet 35% `"quiet"` · Idle `"idle"` · Custom `"custom"` | Targeted rescans after live monitoring sees a change. Not available yet (shown grayed out). | `scan.load_incremental` |
| Custom limit | 70 % | 5 to 100 %, step 5 | Used when the "Custom" level is chosen. Applies from the next scan. | `scan.custom_limit` |
| Use low I/O priority when idle | On | On `true` · Off `false` | The "Idle" level uses the low I/O priority and leaves the disk to the apps you are using. Applies from the next scan. | `scan.idle_low_io` |
| Give way when the disk is busy | On | On `true` · Off `false` | When other programs push the whole disk past the limit, the scan slows down to a minimum. Applies from the next scan. | `scan.yield_when_busy` |
| Maximum concurrency | Auto | Auto `"auto"` · Manual `"manual"` | Auto: chosen from the number of processor cores. Manual: the number below. Applies from the next scan. | `scan.concurrency` |
| Manual concurrency | 4 | 1 to 64 | How many worker processes scan at the same time in manual mode. Applies from the next scan. Can be changed when "Maximum concurrency" is set to "Manual". | `scan.workers` |

#### Scan scope

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Scan NTFS alternate data streams | Off | On `true` · Off `false` | Counts the hidden data streams attached to files in their size during an MFT quick scan (a full scan doesn't read streams). Applies from the next scan. Windows only. | `scan.ads` |
| Cross mount points | Off | On `true` · Off `false` | Goes into other volumes mounted in folders. When off, only the current volume is counted. Applies from the next scan. | `scan.cross_mounts` |
| Protected places | Ask before scanning | Ask before scanning `"ask"` · Skip them, no prompts `"skip"` · Access them `"allow"` | Without Full Disk Access, macOS asks one by one before the Desktop, Documents, Downloads, cloud drives, the Photos library and other apps' data can be read. Applies from the next scan. macOS only. | `scan.protected_places` |

#### Counting

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Hard links | Count once | Count once `"once"` · Count every link `"each"` | Counted once, the totals match the real size on disk. Applies from the next scan. | `scan.hardlinks` |
| Size units | Match system | Match system `"system"` · `"1024"` · `"1000"` | With 1000, 778.6 GB shows as 836.0 GB. | `scan.unit_base` |

#### Live monitoring

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Live monitoring | On | On `true` · Off `false` | Windows: Uses the USN journal and ReadDirectoryChangesEx. Changed files flash. macOS: Uses FSEvents. Changed files flash. Linux: Uses inotify. Changed files flash. | `scan.monitor` |
| Event merge interval | 150 ms | 50 to 5000 ms, step 10 | Changes within this time are merged into one refresh. Can be changed when "Live monitoring" is on. | `scan.coalesce_ms` |

### File classes

Groups files by extension for treemap colors, :class: filters and class statistics.

This page has no switches or choices: it is the file class editor. See [Tags and file classes](../tags-classes/).

### Tags

Four fixed tags: red ▲, yellow ■, green ●, blue ◆. Their colors and shapes can't be changed.

#### Display names

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Red tag ▲ | Empty | Text, up to 64 characters | The display name appears in the details panel and the hover card. Filters still use :red. | `tags.name_red` |
| Yellow tag ■ | Empty | Text, up to 64 characters | Filters still use :yellow. | `tags.name_yellow` |
| Green tag ● | Empty | Text, up to 64 characters | Filters still use :green. | `tags.name_green` |
| Blue tag ◆ | Empty | Text, up to 64 characters | Filters still use :blue. | `tags.name_blue` |

#### Shortcuts

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Ctrl is optional when the treemap has focus | On | On `true` · Off `false` | Press 1–4 to tag and 0 to clear. | `tags.omit_ctrl` |

#### Saving

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Remember tags | On | On `true` · Off `false` | Tags are saved per scan root and come back after a rescan or when a snapshot is opened. | `tags.remember` |

### Plugins

Install, enable and disable plugins; their own settings are here too.

This page lists the installed plugins: the permissions a plugin asks for are shown before it is first enabled, and its settings stay when it is turned off. A plugin's own settings appear on this page once it is enabled.

### Shortcuts

Every command and its current shortcut, the same list as the command palette.

#### Key preset

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Key preset | Modern | Modern `"modern"` · SpaceSniffer classic `"classic"` | Shows as "Custom" once any key is changed. Not available yet (shown grayed out). | `keys.preset` |

### Advanced

Settings file, logs, cache and snapshots.

#### Log

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Log file level | Info | Info `"INFO"` · Debug `"DEBUG"` | The lowest level written to the log file. | `advanced.log_level` |

#### Cache & snapshots

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Save a snapshot after each scan | On | On `true` · Off `false` | Used by the recent scans on the start page and by snapshot comparison. Applies from the next scan. | `advanced.auto_snapshot` |
| Recent snapshots to keep | 10 | 1 to 100 | Of older snapshots, one per week is kept. | `advanced.snapshot_keep` |

#### Memory

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Keep large scans in mapped files | Off | On `true` · Off `false` | When on, scans whose node table passes 256 MB (about two million items) go to memory-mapped files on disk and take no private memory; after the scan this writes about 200 bytes per item to disk once. Applies from the next scan. | `advanced.map_large_scans` |
| Scan data memory limit | 25 % | 5 to 90 %, step 5 | Once scan data passes this share of physical memory, new node tables go to memory-mapped files on disk: they take no private commit, and the system keeps the parts in use in memory. Applies from the next scan. | `advanced.memory_budget` |

#### Plugins

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Developer mode | Off | On `true` · Off `false` | Plugin code may run inside the backend (without isolation) and plugin folders under development can be linked; turn on only while developing plugins. | `advanced.plugin_dev_mode` |

### About & updates

Version, updates and the release timeline.

#### Updates

| Setting | Default | Values | Description | Key |
| --- | --- | --- | --- | --- |
| Check for updates automatically | On | On `true` · Off `false` | At most once a day, it only reads this project's release list on GitHub and sends no usage data; small updates only show a badge at the top right. | `general.update_check` |
| Download updates automatically | On | On `true` · Off `false` | A new version is downloaded and checked in the background. It is installed only when you click Seamless update or the other way in the update card, never by itself. Can be changed when "Check for updates automatically" is on. | `general.update_download` |
| Versions to offer | Include previews | Include previews `"preview"` · Releases only `"stable"` | Previews are the releases marked as pre-release on GitHub. | `general.update_channel` |

<!-- generated:settings:end -->
