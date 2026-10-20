---
title: Updates and privacy
description: How SpaceKnife finds, downloads, verifies and installs new versions, how to change or turn off the check, and the only network requests it makes.
---

SpaceKnife has no telemetry and no account. The only things it fetches from the internet are the list of its own releases on GitHub and, when you click **Download**, the new version and its checksums.

## The update check

With **Settings › General › Updates › Check for updates automatically** on (the default), SpaceKnife reads the release list of its GitHub repository:

- about a minute after it starts, when the last successful check is more than 24 hours old;
- then once every 24 hours while it runs;
- 6 hours after a check that failed (no connection, GitHub busy), instead of 24.

Each check sends the tag (ETag) of the list it already has, so an unchanged list costs almost nothing. Turning the setting on, or changing which versions to offer, checks right away. What the checks remember (the last check, the newest release, the versions whose card you closed) is kept in `update.json` in the [data folder](../data-locations/).

**Check for updates** in the [command palette](../command-palette/) checks at any time, also with the automatic check off. Its status shows your current version, or the newer one it found.

### Which versions are offered

**Settings › General › Updates › Versions to offer**:

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

- **The badge** sits at the right of the title bar and reads "Update 0.4.0". Hover for "SpaceKnife 0.4.0 is available (you have 0.3.2)"; click it to open the update card under it. A click elsewhere or <kbd>Esc</kbd> closes the card.
- **The update card** shows the version, its release date, the version you have, a **Preview** tag for previews, the start of the release notes with **Full release notes**, the file for your kind of install and its size, and **Later**, **Release page** and **Download**.
- **The dialog** of a major release holds the same card, marked **Major update**. While it is open, the keyboard shortcuts behind it don't work.

Closing the card or the dialog (the close button, **Later** or <kbd>Esc</kbd>) means that version won't open by itself again, also after a restart; the badge stays. Click the badge, or run **Show update** in the command palette, to open the card again.

## Download and verify

**Download** fetches the file for your kind of install and saves it in your system's Downloads folder:

| Your copy | File it downloads |
|---|---|
| Windows installer | `SpaceKnife-<version>-Setup-x64.exe` |
| Windows portable | `SpaceKnife-<version>-win-x64-portable.zip` |
| macOS | `SpaceKnife-<version>-mac-arm64.dmg` or `…-mac-x64.dmg`, for your Mac |
| deb | `spaceknife_<version>_amd64.deb` |
| rpm | `spaceknife-<version>.x86_64.rpm` |
| AppImage | `SpaceKnife-<version>-x86_64.AppImage` |
| Linux portable | `SpaceKnife-<version>-linux-x64-portable.tar.gz` |

While it downloads, the card shows the progress with **Cancel**, and the badge fills up and shows the percentage. Then SpaceKnife checks the file:

- It compares the file's SHA-256 with the checksum published in the same release (`SHA256SUMS*.txt`). On a match the card says **SHA-256 checksum verified**.
- A file that doesn't match is deleted at once: "The downloaded file doesn't match the published SHA-256 checksum and was deleted".
- If the release publishes checksum files but they can't be read, nothing is downloaded.
- If the release publishes no checksum for that file, the file is kept, marked "This release publishes no checksums: the file was not verified".
- A file already in the Downloads folder with the right checksum is used as it is, without downloading it again.

For a copy SpaceKnife can't match to a release file (for example one run from the source code), **Download** is disabled and the card says "For this kind of install, download it from the release page".

## Install

After the download, the card says how the new version replaces yours:

| Your copy | Button | What happens |
|---|---|---|
| Windows installer | **Install** | SpaceKnife quits and starts the installer, which replaces the old version. Scan data and settings are kept. |
| macOS | **Install** | SpaceKnife quits and opens the disk image. Drag SpaceKnife into Applications to replace the old one, then [allow it once more](../install/#first-launch-allow-it-once). |
| deb, rpm | **Install** | SpaceKnife quits and opens the package in the system's software installer. Scan data and settings are kept. |
| Portable | **Show in Explorer** (Windows) / **Show in file manager** (Linux) | Quit SpaceKnife, unpack the archive and replace the old folder's contents, but keep its `data` folder. |
| AppImage | **Show in file manager** | Quit SpaceKnife and replace the old AppImage with the new one (it is already made executable). |

If a scan is still running when you click **Install**, the card warns that installing stops it and what it has scanned so far is not saved; click **Install anyway** to go ahead.

SpaceKnife never installs anything without your click. It only starts a file that is directly in your Downloads folder and named like the release file for your kind of install.

## Turning the check off

Switch off **Settings › General › Updates › Check for updates automatically**. SpaceKnife then makes no network request unless you ask for one with **Check for updates** or **Download**. A release found by an earlier check can still show its badge. Releases stay on the [Releases page](https://github.com/DuskXi/SpaceKnife/releases), and the [install page](../install/#upgrading) explains each way to upgrade by hand.

## Privacy

### No telemetry

SpaceKnife sends nothing about you, your computer or your files: no usage statistics, no crash reports, no identifiers, no account. Scans, snapshots, tags and settings stay on your computer, in the folders listed in [Data locations](../data-locations/).

### The only network requests

All of them go to GitHub, and only for updates:

| Request | When | What is sent |
|---|---|---|
| The release list, from `api.github.com/repos/DuskXi/SpaceKnife/releases` | The automatic check, or **Check for updates** | A plain request with `User-Agent: SpaceKnife/<version>` and the ETag of the cached list. No token, no cookies. |
| The checksum files and the release file, from GitHub | Only when you click **Download** | The same `User-Agent` |

- **Release page** and **Full release notes** open the release on GitHub in your web browser; SpaceKnife itself fetches nothing for them.
- The requests go through your proxy settings: the usual proxy environment variables, and on Windows and macOS the system's proxy settings.
- The connection to GitHub is verified with HTTPS certificates.
- GitHub limits requests without a token per address. When many computers share one address, a check can be refused for a while ("GitHub is limiting requests for now…"); SpaceKnife tries again later.
- The command line (`spaceknife-backend scan`, `run`, `snapshot`) makes no network requests.

### On your computer

- The window talks to its scanning backend over a local connection on this computer only (the loopback address, with a new random token each start), so other computers can't reach it.
- The log file and the log panel contain the paths of files and folders, for example of folders that couldn't be read.
- **Copy diagnostics** (**Settings › Advanced**) only copies to the clipboard. What it copies includes the paths of your settings file and log folder, which usually contain your user name; check before you post it.
- A snapshot file (`.sks`) holds the names, sizes and dates of everything that was scanned. Sharing one shares your folder structure; the same goes for [exports](../export/).
