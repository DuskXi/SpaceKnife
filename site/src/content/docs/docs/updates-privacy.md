---
title: Updates and privacy
description: How SpaceKnife finds, downloads, verifies and installs new versions, what the About & updates page shows, how to change or turn off the check, and the only network requests it makes.
---

SpaceKnife has no telemetry and no account. The only things it fetches from the internet are the list of its own releases on GitHub and the new version's file with its checksums.

## The update check

With **Settings › About & updates › Updates › Check for updates automatically** on (the default), SpaceKnife reads the release list of its GitHub repository:

- about a minute after it starts, when the last successful check is more than 24 hours old;
- then once every 24 hours while it runs;
- 6 hours after a check that failed (no connection, GitHub busy), instead of 24.

Each check sends the tag (ETag) of the list it already has, so an unchanged list costs almost nothing. Turning the setting on, or changing which versions to offer, checks right away. What the checks remember (the last check, the recent releases with their notes, the versions whose card you closed) is kept in `update.json` in the [data folder](../data-locations/).

**Check for updates** in the [command palette](../command-palette/) or on the [About & updates page](#the-about--updates-page) checks at any time, also with the automatic check off. Its status shows your current version, or the newer one it found.

### Which versions are offered

**Settings › About & updates › Updates › Versions to offer**:

| Choice | Offers |
|---|---|
| **Include previews** (default) | The newest release, previews included |
| **Releases only** | Only releases that are not previews |

A preview is a release marked as a pre-release on GitHub, or one whose version has a pre-release part, such as `0.4.0-beta.1`. Draft releases are never offered. With **Releases only**, nothing is offered until there is a newer release that isn't a preview.

## How a new version is shown

How loudly SpaceKnife tells you depends on how far behind you are. Versions are compared part by part as whole numbers, so `0.3.10` is newer than `0.3.9`.

| The new version differs in | Example | What you see |
|---|---|---|
| The last part, or only the preview part | 0.3.1 → 0.3.2, 0.4.0-beta.1 → 0.4.0 | Only the badge in the title bar |
| The middle part (a minor release) | 0.3.2 → 0.4.0 | The badge, and the update card at the top left |
| The first part (a major release) | 0.4.0 → 1.0.0 | The badge, and the update card as a dialog in the window |

- **The badge** sits at the right of the title bar and reads "Update 0.4.0". While the new version downloads, it fills up and shows the percentage; once the file is ready, it reads **Update ready**. Hover for "SpaceKnife 0.4.0 is available (you have 0.3.2)"; click it to open the update card under it. A click elsewhere or <kbd>Esc</kbd> closes the card.
- **The update card** shows the version, its release date, the version you have, a **Preview** tag for previews, the start of the release notes with **Full release notes**, the file for your kind of install and its size, and **Later**, **Release page** and **Download** (or the download's progress, or **Restart and update** once the file is ready).
- **The dialog** of a major release holds the same card, marked **Major update**. While it is open, the keyboard shortcuts behind it don't work.

Closing the card or the dialog (the close button, **Later** or <kbd>Esc</kbd>) means that version won't open by itself again, also after a restart; the badge stays. Click the badge, or run **Show update** in the command palette, to open the card again.

## Download and verify

With **Settings › About & updates › Updates › Download updates automatically** on (the default), a check that finds a newer version downloads its file right away, in the background: the badge and the card show the progress, and nothing opens for it. With the setting off, click **Download** in the card. Only a version the check offers is downloaded (see [Which versions are offered](#which-versions-are-offered)), and only when the release has a file for your kind of install.

The file goes into the `updates` folder in the [data folder](../data-locations/), not into your Downloads folder:

| Your copy | File it downloads |
|---|---|
| Windows installer | `SpaceKnife-<version>-Setup-x64.exe` |
| Windows portable | `SpaceKnife-<version>-win-x64-portable.zip` |
| macOS | `SpaceKnife-<version>-mac-arm64.dmg` or `…-mac-x64.dmg`, for your Mac |
| deb | `spaceknife_<version>_amd64.deb` |
| rpm | `spaceknife-<version>.x86_64.rpm` |
| AppImage | `SpaceKnife-<version>-x86_64.AppImage` |
| Linux portable | `SpaceKnife-<version>-linux-x64-portable.tar.gz` |

Then SpaceKnife checks the file:

- It compares the file's SHA-256 with the checksum published in the same release (`SHA256SUMS*.txt`). On a match the card says **SHA-256 checksum verified**.
- A file that doesn't match is deleted at once: "The downloaded file doesn't match the published SHA-256 checksum and was deleted".
- If the release publishes checksum files but they can't be read, nothing is downloaded.
- If the release publishes no checksum for that file, the file is kept, marked "This release publishes no checksums: the file was not verified".
- A file already in the `updates` folder with the right checksum is used as it is, without downloading it again.

An automatic download that failed (no connection, for example) is tried again after the next check, or when you click **Retry**. For a copy SpaceKnife can't match to a release file (for example one run from the source code), **Download** is disabled and the card says "For this kind of install, download it from the release page".

### Nothing left behind

- Each start removes from the `updates` folder what is no longer needed: partial downloads, and everything of the version you run or an older one (the file that installed it and, for a portable copy, the old program files it replaced). A newer version you haven't installed yet stays.
- Uninstalling the Windows installer removes the `updates` folder; see [Uninstalling](../install/#uninstalling).

## Install

After the download, the card says how the new version replaces yours:

| Your copy | Button | What happens |
|---|---|---|
| Windows installer | **Restart and update** | SpaceKnife closes, the installer replaces the old version in the background without showing its pages, and SpaceKnife opens again. For an installation for all users, Windows asks once for administrator permission. Scan data and settings are kept. |
| Portable | **Restart and update** | SpaceKnife closes, its program files are replaced by the new ones, and it opens again. The `data` folder (scan data and settings) stays as it is. If a file can't be replaced, the old files are put back and the old version opens again. |
| AppImage | **Restart and update** | SpaceKnife closes, the new AppImage (already made executable) takes the place of the old file, and it opens again. Scan data and settings are kept. |
| macOS | **Install** | SpaceKnife quits and opens the disk image. Drag SpaceKnife into Applications to replace the old one, then [allow it once more](../install/#first-launch-allow-it-once). |
| deb, rpm | **Install** | SpaceKnife quits and opens the package in the system's software installer. Scan data and settings are kept. |

After a **Restart and update**, the window says "Updated to SpaceKnife 0.4.0" (the version it now runs) once.

If a scan is still running when you click the button, the card warns "A scan is still running. Updating stops it (what it has read so far isn't saved)"; click **Restart and update anyway** (or **Install anyway**) to go ahead.

SpaceKnife never installs anything without your click. It only starts a file that is directly in its `updates` folder and named like the release file for your kind of install.

## The About & updates page

**Settings › About & updates** gathers everything about your version and its updates. **About & updates** in the command palette opens it, and so does the **About** row at the bottom of **Settings › General**, which also says when a newer version is known.

- **At the top**: your version, how this copy was installed and which versions are offered; what the last check found, when it ran and when the next automatic check is due. **Check for updates** checks now; **All releases** opens the [Releases page](https://github.com/DuskXi/SpaceKnife/releases). When a newer version is known, its update card is shown here too, and stays.
- **Updates**: the three update settings (check, download, versions to offer).
- **Components**: the versions of SpaceKnife, Electron, Chromium, Node.js, the backend's Python and the protocol, and your system. **Copy version info** copies them, for a bug report.
- **Release timeline**: the last 30 releases, newest first, marked **Current**, **New** (newer than yours) and **Preview**. Click one to read its notes in the interface language; the newest is open. **View on GitHub** opens its release page. The timeline comes from the last check, so it is there offline too; before the first check it is empty.

## Turning the check off

Switch off **Settings › About & updates › Updates › Check for updates automatically**. Automatic downloads stop with it, and SpaceKnife then makes no network request unless you ask for one with **Check for updates** or **Download**. A release found by an earlier check can still show its badge. Releases stay on the [Releases page](https://github.com/DuskXi/SpaceKnife/releases), and the [install page](../install/#upgrading) explains each way to upgrade by hand.

## Privacy

### No telemetry

SpaceKnife sends nothing about you, your computer or your files: no usage statistics, no crash reports, no identifiers, no account. Scans, snapshots, tags and settings stay on your computer, in the folders listed in [Data locations](../data-locations/).

### The only network requests

All of them go to GitHub, and only for updates:

| Request | When | What is sent |
|---|---|---|
| The release list, from `api.github.com/repos/DuskXi/SpaceKnife/releases` | The automatic check, or **Check for updates** | A plain request with `User-Agent: SpaceKnife/<version>` and the ETag of the cached list. No token, no cookies. |
| The checksum files and the release file, from GitHub | After a check found a newer version, with **Download updates automatically** on; or when you click **Download** | The same `User-Agent` |

- **Release page**, **Full release notes**, **All releases** and **View on GitHub** open GitHub in your web browser; SpaceKnife itself fetches nothing for them. The release timeline shows the notes the last check already fetched.
- The requests go through your proxy settings: the usual proxy environment variables, and on Windows and macOS the system's proxy settings.
- The connection to GitHub is verified with HTTPS certificates.
- GitHub limits requests without a token per address. When many computers share one address, a check can be refused for a while ("GitHub is limiting requests for now…"); SpaceKnife tries again later.
- The command line (`spaceknife-backend scan`, `run`, `snapshot`) makes no network requests.

### On your computer

- The window talks to its scanning backend over a local connection on this computer only (the loopback address, with a new random token each start), so other computers can't reach it.
- The log file and the log panel contain the paths of files and folders, for example of folders that couldn't be read.
- **Copy diagnostics** (**Settings › Advanced**) only copies to the clipboard. What it copies includes the paths of your settings file and log folder, which usually contain your user name; check before you post it.
- A snapshot file (`.sks`) holds the names, sizes and dates of everything that was scanned. Sharing one shares your folder structure; the same goes for [exports](../export/).
