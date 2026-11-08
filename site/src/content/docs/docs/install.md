---
title: Install and upgrade
description: Pick the right download for Windows, macOS or Linux, get past the first-launch checks, upgrade to a new version (also from 0.3.4) and uninstall.
---

SpaceKnife is published on the [Releases page](https://github.com/DuskXi/SpaceKnife/releases). Each release has installers and portable archives for Windows, Linux and macOS, and SHA-256 checksum files for all of them. The interface follows the system language (English or Simplified Chinese); change it any time in **Settings › General › Interface language**.

## Which file to download

| System | File | What it is |
|---|---|---|
| Windows 10 or later, x64 | `SpaceKnife-<version>-Setup-x64.exe` | Installer, for the current user or all users |
| Windows 10 or later, x64 | `SpaceKnife-<version>-win-x64-portable.zip` | Portable: unzip and run, no installation |
| Windows 11 on ARM | `SpaceKnife-<version>-win-arm64-portable.zip` | Portable, ARM (arm64) |
| macOS 14 or later, Apple Silicon | `SpaceKnife-<version>-mac-arm64.dmg` | Disk image |
| macOS 14 or later, Intel | `SpaceKnife-<version>-mac-x64.dmg` | Disk image |
| macOS 14 or later | `SpaceKnife-<version>-mac-<arm64 or x64>-portable.zip` | Portable: unzip and run, data next to the app |
| Ubuntu 22.04 or later, Debian family | `spaceknife_<version>_amd64.deb` | Package |
| RHEL 9 or later, Fedora family | `spaceknife-<version>.x86_64.rpm` | Package |
| Other Linux, x64 | `SpaceKnife-<version>-x86_64.AppImage` | Single file, needs FUSE 2 |
| Linux with WebKitGTK 4.1, x64 or ARM (arm64) | `SpaceKnife-<version>-linux-<x64 or arm64>-tauri.tar.gz` | Small portable: extract and run, uses the system's WebKitGTK |
| Any Linux, x64 or ARM (arm64) | `SpaceKnife-<version>-linux-<x64 or arm64>-portable.tar.gz` | Portable that brings its own browser engine, also for RHEL 9 |

On ARM, Windows and Linux have the portable archives; the installers and packages are x64 only (Windows on ARM can also run the x64 installer, emulated). There is no 32-bit Windows build. The Linux builds need glibc 2.34 or later (the level of RHEL 9; Ubuntu 22.04 has 2.35).

The window looks and works the same in every download; what draws it differs. Most of them use the web view the system already has: Microsoft Edge WebView2 on Windows, WebKit on macOS, WebKitGTK 4.1 on Linux. That keeps them small. RHEL 9 has no WebKitGTK 4.1, so the rpm, the AppImage and the portable `…-linux-<arch>-portable.tar.gz` bring their own engine (Electron, with Chromium) and are much larger. **Settings › About & updates › Components** shows which one your copy uses.

### Check the download

The in-app update checks the checksum for you (see [Updates and privacy](../updates-privacy/)). To check a file you downloaded yourself, compare its SHA-256 with the line for that file in the release's checksum files (`SHA256SUMS.txt` and the `SHA256SUMS-portable-…`, `SHA256SUMS-tauri-linux-…` and `SHA256SUMS-mac-…` files next to it):

```powershell
Get-FileHash .\SpaceKnife-<version>-Setup-x64.exe -Algorithm SHA256
```

```bash
sha256sum -c --ignore-missing SHA256SUMS.txt
```

```bash
shasum -a 256 SpaceKnife-<version>-mac-arm64.dmg
```

## Windows

### Installer

1. Run `SpaceKnife-<version>-Setup-x64.exe`.
2. The installer isn't code-signed yet, so on the first run SmartScreen warns about an unknown publisher. Click **More info**, then **Run anyway**.
3. Choose who it is for: **Only for me** (the default, no administrator rights needed) or everyone who uses the computer (all users, asks for administrator rights). For a new installation you can change the folder; the defaults are `%LOCALAPPDATA%\Programs\SpaceKnife` for you alone and `C:\Program Files\SpaceKnife` for all users.

The installer speaks English or Chinese, following the system language. It creates Start menu and desktop shortcuts and can start SpaceKnife when it is done. The window is drawn by the Microsoft Edge WebView2 Runtime, which Windows 11 includes and most Windows 10 computers already have; where it is missing, the installer downloads and installs it from Microsoft first, which needs an internet connection.

If SpaceKnife is already installed (also 0.3.4), the installer keeps its folder and who it was installed for, and upgrades it in place: see [Upgrading from 0.3.4](#upgrading-from-034).

:::note[Current user or all users]
A current-user install lives in a folder your own account can write to. The [MFT quick scan](../first-scan/#scan-methods) starts its elevated, read-only scan helper from the install folder. If you want that program to sit where only administrators can change it, choose the all-users install (Program Files).
:::

### Silent install

For scripted deployment, the installer takes the usual switches:

```powershell
.\SpaceKnife-<version>-Setup-x64.exe /S /currentuser
```

Use `/allusers` instead of `/currentuser` for a machine-wide install (run it as administrator). `/D=<folder>` (last on the line) picks the folder of a new installation. To uninstall silently, run the `QuietUninstallString` from SpaceKnife's uninstall entry in the registry, which is `"uninstall.exe" /currentuser /S` (or `/allusers /S`) in the install folder. The uninstaller copies itself to a temporary folder and runs from there, so the command returns at once. A silent uninstall keeps the snapshots, settings and logs; add `--delete-app-data` to remove them too.

### Portable zip

Unzip `SpaceKnife-<version>-win-x64-portable.zip` somewhere you can write (your user folder, a USB drive; not Program Files) and run `SpaceKnife.exe`. The first launch takes a few seconds longer while Windows checks the new files.

The portable copy needs the Microsoft Edge WebView2 Runtime. Windows 11 includes it, and most Windows 10 computers already have it; if SpaceKnife doesn't open, install the runtime from Microsoft (it is free), or use the installer, which installs it for you.

Next to `SpaceKnife.exe` there is a `data` folder. While it is there, SpaceKnife keeps everything in it (snapshots, settings, logs, cache) and writes nothing to your user profile. See [Portable mode](../data-locations/#portable-mode).

## macOS

### Install

Open the dmg and drag SpaceKnife into **Applications**. You need macOS 14 or later; take the `arm64` dmg for Apple Silicon and the `x64` dmg for Intel Macs.

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

Scanning runs inside SpaceKnife; macOS shows its prompts and permissions under the name SpaceKnife.

## Linux

### deb and rpm

```bash
sudo apt install ./spaceknife_<version>_amd64.deb
```

```bash
sudo dnf install ./spaceknife-<version>.x86_64.rpm
```

Both add SpaceKnife to the app menu with its icon, and both put the app (`spaceknife`) and the command line (`spaceknife-cli`) on your PATH, in `/usr/bin`. The package manager installs the dependencies.

| | deb | rpm |
|---|---|---|
| The program | `/usr/bin/spaceknife`, its files in `/usr/lib/SpaceKnife` | `/opt/SpaceKnife`, linked from `/usr/bin/spaceknife` |
| The command line | `/usr/bin/spaceknife-cli` | `/opt/SpaceKnife/spaceknife-cli`, linked from `/usr/bin/spaceknife-cli` |
| The window | The system's WebKitGTK 4.1 and GTK 3 | Its own engine (Electron), with GTK 3, NSS and the other usual libraries of a desktop app |

### AppImage

```bash
chmod +x SpaceKnife-<version>-x86_64.AppImage
./SpaceKnife-<version>-x86_64.AppImage
```

The AppImage needs FUSE 2 (on Ubuntu 22.04, the package `libfuse2`). Without it, start it with `--appimage-extract-and-run`. It brings its own engine (Electron), so it runs on any x64 distribution with glibc 2.34 or later.

From 0.3.2 on, the AppImage carries update information: `./SpaceKnife-<version>-x86_64.AppImage --appimage-updateinformation` prints it. AppImage update tools use it, and the `.zsync` file published next to the AppImage, to download only what changed.

### Portable tar.gz

There are two portable archives for Linux, each for x64 and arm64. Both keep all their data in the `data` folder next to the program ([portable mode](../data-locations/#portable-mode)), neither needs FUSE, and each updates only to its own kind.

- **The small one**, `SpaceKnife-<version>-linux-<arch>-tauri.tar.gz`, draws its window with the system's WebKitGTK 4.1 (`libwebkit2gtk-4.1-0` on Ubuntu 22.04 and later and on Debian 12; `webkit2gtk4.1` on Fedora). RHEL 9 doesn't have it.
- **The one that brings its own engine**, `SpaceKnife-<version>-linux-<arch>-portable.tar.gz` (Electron, with Chromium), runs on RHEL 9 too. It has the same name as the Linux portable of 0.3.4, so a 0.3.4 portable copy updates to it.

```bash
tar xzf SpaceKnife-<version>-linux-x64-tauri.tar.gz
cd SpaceKnife-<version>-linux-x64-tauri
./spaceknife
```

The other archive unpacks into `SpaceKnife-<version>-linux-x64-portable` in the same way; its program is `spaceknife` too.

:::caution[Newer Ubuntu releases]
In the AppImage and the portable `…-portable.tar.gz`, Chromium's `chrome-sandbox` is not setuid. On distributions that restrict unprivileged user namespaces (the AppArmor rule Ubuntu has from 23.10 on), they may not start; run them with `--no-sandbox`, or use the deb or the small portable instead. Ubuntu 22.04 and RHEL 9 are not affected.
:::

## Upgrading

SpaceKnife checks for new releases at most once a day and shows them in the title bar. It downloads and verifies the new version in the background; then the update card has two buttons. **Seamless update** (Windows installer, portable copy, AppImage) has SpaceKnife put the new version in place itself and open again; the classic way hands over to the installer, the disk image or the package, or lets you replace the file yourself. Nothing is installed until you click. How that works, how to skip a series and how to turn the check off: [Updates and privacy](../updates-privacy/).

Whichever way you upgrade, your scans, snapshots and settings stay: they live outside the program folder, or in the `data` folder of a portable copy.

| Install | How the new version replaces the old one |
|---|---|
| Windows installer | Click **Seamless update** in the update card: SpaceKnife closes, the installer replaces the old version in the background without showing its pages, and SpaceKnife opens again. For an installation for all users, Windows asks once for administrator permission. **Run the installer** instead quits SpaceKnife and opens the installer's wizard, the same as running a newer installer by hand. |
| Portable (Windows, Linux, macOS) | Click **Seamless update**: SpaceKnife puts the new program files in place itself and keeps the `data` folder. With **Replace it yourself**, or by hand: quit SpaceKnife, unpack the new archive and replace the contents of the old folder, but keep its `data` folder. |
| macOS dmg | Click **Open the disk image**: SpaceKnife quits and opens the disk image. Drag SpaceKnife into Applications to replace the old one. Then allow it once more and switch Full Disk Access off and on (see above). |
| deb or rpm | Click **Open the package**: SpaceKnife quits and opens the package in the system's software installer. Or install the new package with `apt` or `dnf` as above. |
| AppImage | Click **Seamless update**: the new AppImage takes the place of the old file. With **Replace it yourself**, or by hand: quit SpaceKnife and replace the old file with the new one. |

### Upgrading from 0.3.4

SpaceKnife 0.3.5 is rewritten underneath: the backend is new, and so is the window around the same interface. The release files keep 0.3.4's names, so 0.3.4's update check finds 0.3.5 and the update card works as for any update. What stays:

- **Your data**: settings, file classes, classified folders, tags, the command palette's recent entries and the update state are read from the same folders ([Data locations](../data-locations/)). The theme you chose is kept.
- **Your snapshots**: those 0.3.x saved open and compare as they are, without a conversion. 0.3.5 saves a newer format, which 0.3.x can't open: if you go back to 0.3.x, it shows only the snapshots it saved itself.
- **The command line** is now `spaceknife-cli`, next to the app (it was `resources/backend/spaceknife-backend`), and headless mode is `spaceknife-cli headless`. 0.3.4's ways of starting headless mode still work ([Headless mode](../headless/)).

What the first update does, by kind of install:

| 0.3.4 install | The update to 0.3.5 |
|---|---|
| Windows installer | **Seamless update** runs the new installer. It removes 0.3.4 with 0.3.4's own uninstaller, keeping your data, and installs 0.3.5 into the same folder, for the same users: one SpaceKnife in the app list, the same Start menu and desktop shortcuts, and pins on the taskbar keep working. Where the WebView2 Runtime is missing, it installs that first; if it can't (no internet connection, for example), nothing is changed and 0.3.4 opens again. |
| Windows portable | **Seamless update** first checks that 0.3.5 can start on this computer (it needs the WebView2 Runtime). If it can't, nothing is replaced, the card says **Can't start the update: …** and 0.3.4 goes on as before. On Windows on ARM, an x64 copy moves to the ARM build. |
| macOS dmg | **Open the disk image**, then drag SpaceKnife into Applications to replace 0.3.4. Allow it once and switch Full Disk Access off and on, as after every update. |
| macOS portable | **Seamless update**, as before; then switch Full Disk Access off and on, as after every update. |
| deb | **Open the package**: the software installer upgrades the `spaceknife` package. The program moves from `/opt/SpaceKnife` to `/usr/bin` and `/usr/lib/SpaceKnife`, and the package manager adds WebKitGTK 4.1. |
| rpm | **Open the package**: an upgrade with the same layout as 0.3.4 (`/opt/SpaceKnife`), plus `/usr/bin/spaceknife-cli`. |
| AppImage | **Seamless update**, as before; AppImage update tools keep working with the `.zsync` file. |
| Linux portable | **Seamless update** to the portable of the same name, which brings its own engine and so runs on RHEL 9 too. To move to the small one instead, unpack it and copy the old copy's `data` folder into it. |

## Uninstalling

| Install | How to remove it |
|---|---|
| Windows installer | Uninstall SpaceKnife from the app list in Windows Settings. It removes the program, the window's caches, the cache and the downloaded updates. Its second page, **Your data**, asks about the rest: tick **Also delete the snapshots, settings and logs** (unticked by default) to remove `%LOCALAPPDATA%\SpaceKnife` too. |
| Portable | Delete the folder. Its `data` folder goes with it. |
| macOS | Quit SpaceKnife and move it from Applications to the Trash. |
| deb | `sudo apt remove spaceknife` |
| rpm | `sudo dnf remove spaceknife` |
| AppImage | Delete the file. |

Unless you tick that box, or remove a portable copy, your scans, snapshots and settings stay, so a later install picks them up again. To remove them by hand, delete the folders listed in [Data locations](../data-locations/). A folder set with `SPACEKNIFE_HOME` is never touched by the uninstaller.

## The command line

Every installed copy also has the command line `spaceknife-cli`, for scans and snapshots in a terminal, and for [headless mode](../headless/):

| Install | Path |
|---|---|
| Windows installer | `<install folder>\spaceknife-cli.exe` (`%LOCALAPPDATA%\Programs\SpaceKnife` or `C:\Program Files\SpaceKnife`) |
| Linux deb or rpm | `/usr/bin/spaceknife-cli` |
| Portable | `spaceknife-cli` next to the program |
| AppImage | Inside the image ([Command line](../command-line/#where-the-program-is)) |
| macOS | `/Applications/SpaceKnife.app/Contents/MacOS/spaceknife-cli` |

What it can do: [Command line](../command-line/).
