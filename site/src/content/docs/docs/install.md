---
title: Install and upgrade
description: Pick the right download for Windows, macOS or Linux, get past the first-launch checks, upgrade to a new version and uninstall.
---

SpaceKnife is published on the [Releases page](https://github.com/DuskXi/SpaceKnife/releases). Each release has installers and portable archives for Windows, Linux and macOS, and SHA-256 checksum files for all of them. The interface follows the system language (English or Simplified Chinese); change it any time in **Settings › General › Interface language**.

## Which file to download

| System | File | What it is |
|---|---|---|
| Windows 10 or later, x64 | `SpaceKnife_<version>_x64-setup.exe` | Installer, for the current user |
| Windows 10 or later, x64 | `SpaceKnife-<version>-win-x64-portable.zip` | Portable: unzip and run, no installation |
| Windows 11 on ARM | `SpaceKnife-<version>-win-arm64-portable.zip` | Portable, ARM (arm64) |
| macOS 14 or later, Apple Silicon | `SpaceKnife_<version>_aarch64.dmg` | Disk image |
| macOS 14 or later, Intel | `SpaceKnife_<version>_x64.dmg` | Disk image |
| macOS 14 or later | `SpaceKnife-<version>-mac-<arm64 or x64>-portable.zip` | Portable: unzip and run, data next to the app |
| Ubuntu 22.04 or later, Debian family | `SpaceKnife_<version>_amd64.deb` | Package |
| RHEL 9 or later, Fedora family | `SpaceKnife-<version>-1.x86_64.rpm` | Package |
| Other Linux, x64 | `SpaceKnife_<version>_amd64.AppImage` | Single file, needs FUSE 2 |
| Linux, x64 | `SpaceKnife-<version>-linux-x64-portable.tar.gz` | Portable: extract and run, no FUSE |
| Linux, ARM (arm64) | `SpaceKnife-<version>-linux-arm64-portable.tar.gz` | Portable, ARM |

On ARM, Windows and Linux have the portable archives; the installers and packages are x64 only (Windows on ARM can also run the x64 installer, emulated). There is no 32-bit Windows build. The Linux builds need glibc 2.34 or later (the level of RHEL 9; Ubuntu 22.04 has 2.35) and WebKitGTK 4.1. <!-- TODO: confirm the glibc floor of the Tauri builds (it follows the CI build machine). -->

### Check the download

The in-app update checks the checksum for you (see [Updates and privacy](../updates-privacy/)). To check a file you downloaded yourself, compare its SHA-256 with the line for that file in the release's checksum files (`SHA256SUMS.txt` and the `SHA256SUMS-portable-…` / `SHA256SUMS-mac-…` files next to it):

```powershell
Get-FileHash .\SpaceKnife_<version>_x64-setup.exe -Algorithm SHA256
```

```bash
sha256sum -c --ignore-missing SHA256SUMS.txt
```

```bash
shasum -a 256 SpaceKnife_<version>_aarch64.dmg
```

## Windows

### Installer

1. Run `SpaceKnife_<version>_x64-setup.exe`.
2. The installer isn't code-signed yet, so on the first run SmartScreen warns about an unknown publisher. Click **More info**, then **Run anyway**.
3. It installs for your account only, without administrator rights, into `%LOCALAPPDATA%\SpaceKnife` unless you pick another folder.

The installer speaks English or Chinese, following the system language. It creates a Start menu shortcut and can start SpaceKnife when it is done. The window is drawn by Microsoft Edge WebView2, which Windows 11 includes; on Windows 10 the installer sets it up when it is missing.

:::note[A folder your account can write to]
The install folder is one your own account can write to. The [MFT quick scan](../first-scan/#scan-methods) starts its elevated, read-only scan helper from there.
:::

### Silent install

For scripted deployment, the installer takes the usual switches:

```powershell
.\SpaceKnife_<version>_x64-setup.exe /S
```

`/D=<folder>` (last on the line) picks another install folder. To uninstall silently, run the `QuietUninstallString` from SpaceKnife's uninstall entry in the registry, which is `"uninstall.exe" /S` in the install folder. A silent uninstall keeps the snapshots, settings and logs.

### Portable zip

Unzip `SpaceKnife-<version>-win-x64-portable.zip` somewhere you can write (your user folder, a USB drive; not Program Files) and run `spaceknife.exe`. It needs Microsoft Edge WebView2, which Windows 11 includes. The first launch takes a few seconds longer while Windows checks the new files.

Next to `spaceknife.exe` there is a `data` folder. While it is there, SpaceKnife keeps everything in it (snapshots, settings, logs, cache) and writes nothing to your user profile. See [Portable mode](../data-locations/#portable-mode).

## macOS

### Install

Open the dmg and drag SpaceKnife into **Applications**. You need macOS 14 or later; take the `aarch64` dmg for Apple Silicon and the `x64` dmg for Intel Macs.

### Portable zip

Unzip `SpaceKnife-<version>-mac-arm64-portable.zip` (Apple Silicon) or `…-mac-x64-portable.zip` (Intel) anywhere you can write, such as your user folder or a USB drive. The folder holds `SpaceKnife.app` and a `data` folder; keep the two together and open `SpaceKnife.app`. It keeps everything in `data` and nothing in your user Library ([portable mode](../data-locations/#portable-mode)). The first launch needs the same one-time permission as the dmg (below), for the app in that folder.

### First launch: allow it once

The macOS build is not notarized by Apple yet, so macOS blocks it the first time:

1. Open SpaceKnife. macOS says it could not verify that SpaceKnife is free of malware. Click **Done**.
2. Open **System Settings › Privacy & Security**, scroll down to the message that SpaceKnife was blocked, and click **Open Anyway**. Click **Open Anyway** again and enter your password.

From then on SpaceKnife opens normally. After you install a new version, allow it once more.

Other ways to do the same:

- On macOS 14, right-click SpaceKnife in Applications and choose **Open**.
- In Terminal: `xattr -dr com.apple.quarantine /Applications/SpaceKnife.app`

### Full Disk Access

Without Full Disk Access, macOS asks separately before SpaceKnife may read the Desktop, Documents, Downloads, iCloud Drive and other cloud drives, the Photos library, other apps' data, and removable and network drives. A scan then brings up one prompt after another, and some places (Mail, Messages) can't be read at all: they count as [unknown space](../first-scan/#free-space-and-unknown-space).

Before each scan (while **Protected places** is set to **Ask before scanning**, the default), SpaceKnife checks whether it has Full Disk Access; the check itself never brings up a prompt. If it hasn't, the dialog **Let SpaceKnife read every file at once** offers three ways on:

| Button | What happens |
|---|---|
| **Open System Settings** | Opens the Full Disk Access list. Switch SpaceKnife on (if it is not there, add it with **+**), then reopen SpaceKnife when asked. The scan doesn't start. |
| **Skip the protected places** | Scans without any prompt; those places show as not accessible. |
| **Scan and ask one by one** | Scans, and macOS asks for each place. |

Tick **Remember “skip” or “ask one by one”** before you click one of the last two buttons, and the dialog won't come back. The choice is the setting **Settings › Scan & performance › Protected places** (**Ask before scanning**, **Skip them, no prompts**, **Access them**).

:::caution[After each update]
The build is ad-hoc signed, and macOS ties the permission to that exact build. After installing a new version, open the Full Disk Access list and switch SpaceKnife off and on again.
:::

Scanning runs in a background process of SpaceKnife; macOS still shows its prompts and permissions under the name SpaceKnife.

## Linux

### deb and rpm

```bash
sudo apt install ./SpaceKnife_<version>_amd64.deb
```

```bash
sudo dnf install ./SpaceKnife-<version>-1.x86_64.rpm
```

Both put the app (`spaceknife`) and the command line (`spaceknife-cli`) in `/usr/bin`, and add SpaceKnife to the app menu with its icon. The package manager installs the dependencies (WebKitGTK, GTK 3 and so on).

### AppImage

```bash
chmod +x SpaceKnife_<version>_amd64.AppImage
./SpaceKnife_<version>_amd64.AppImage
```

The AppImage needs FUSE 2 (on Ubuntu 22.04, the package `libfuse2`). Without it, start it with `--appimage-extract-and-run`.

<!-- From 0.4.0 the release has no .AppImage.zsync (Tauri's AppImage carries no update information): SpaceKnife's own update check replaces the AppImage. -->

### Portable tar.gz

```bash
tar xzf SpaceKnife-<version>-linux-x64-portable.tar.gz
cd SpaceKnife-<version>-linux-x64-portable
./spaceknife
```

No FUSE needed. Like the Windows zip, it keeps all its data in the `data` folder next to the program ([portable mode](../data-locations/#portable-mode)).

## Upgrading

SpaceKnife checks for new releases at most once a day and shows them in the title bar. It downloads and verifies the new version in the background; then the update card has two buttons. **Seamless update** (Windows installer, portable copy, AppImage) has SpaceKnife put the new version in place itself and open again; the classic way hands over to the installer, the disk image or the package, or lets you replace the file yourself. Nothing is installed until you click. How that works, how to skip a series and how to turn the check off: [Updates and privacy](../updates-privacy/).

Whichever way you upgrade, your scans, snapshots and settings stay: they live outside the program folder, or in the `data` folder of a portable copy.

| Install | How the new version replaces the old one |
|---|---|
| Windows installer | Click **Seamless update** in the update card: SpaceKnife closes, the installer replaces the old version in the background without showing its pages, and SpaceKnife opens again. **Run the installer** instead quits SpaceKnife and opens the installer's wizard, the same as running a newer installer by hand. |
| Windows or Linux portable | Click **Seamless update**: SpaceKnife puts the new program files in place itself and keeps the `data` folder. With **Replace it yourself**, or by hand: quit SpaceKnife, unpack the new archive and replace the contents of the old folder, but keep its `data` folder. |
| macOS dmg | Click **Open the disk image**: SpaceKnife quits and opens the disk image. Drag SpaceKnife into Applications to replace the old one. Then allow it once more and switch Full Disk Access off and on (see above). |
| deb or rpm | Click **Open the package**: SpaceKnife quits and opens the package in the system's software installer. Or install the new package with `apt` or `dnf` as above. |
| AppImage | Click **Seamless update**: the new AppImage takes the place of the old file. With **Replace it yourself**, or by hand: quit SpaceKnife and replace the old file with the new one. |

## Uninstalling

| Install | How to remove it |
|---|---|
| Windows installer | Uninstall SpaceKnife from the app list in Windows Settings. It removes the program; your snapshots, settings and logs stay. <!-- TODO: confirm what the Tauri uninstaller removes besides the program (its "delete the application data" box). --> |
| Portable | Delete the folder. Its `data` folder goes with it. |
| macOS | Quit SpaceKnife and move it from Applications to the Trash. |
| deb | `sudo apt remove spaceknife` |
| rpm | `sudo dnf remove spaceknife` |
| AppImage | Delete the file. |

Unless you remove a portable copy, your scans, snapshots and settings stay, so a later install picks them up again. To remove them by hand, delete the folders listed in [Data locations](../data-locations/). A folder set with `SPACEKNIFE_HOME` is never touched by the uninstaller.

## The command line

Every installed copy also has the command line `spaceknife-cli`, for scans and snapshots in a terminal, and for [headless mode](../headless/):

| Install | Path |
|---|---|
| Windows | `<install folder>\spaceknife-cli.exe` |
| Linux deb or rpm | `/usr/bin/spaceknife-cli` |
| Portable | `spaceknife-cli` next to the program |
| macOS | `/Applications/SpaceKnife.app/Contents/MacOS/spaceknife-cli` |

What it can do: [Command line](../command-line/).
