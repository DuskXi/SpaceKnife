---
title: Headless mode
description: Run SpaceKnife without a window and use it in a browser, on this computer or from another one, with the risks of opening it to the network and how to keep it safe.
---

In headless mode SpaceKnife opens no window. The backend scans as usual, and the app serves its interface over HTTP, so you use SpaceKnife in a browser: on a server or NAS without a display, or on a desktop you reach from a laptop or a tablet. Everything you see and do in that browser happens on the computer that runs headless mode.

## Start it

Give the app's executable one of these options:

```bash
SpaceKnife --headless                  # this computer only (127.0.0.1, port 8375)
SpaceKnife --listen 192.168.1.20       # one address of this computer, port 8375
SpaceKnife --listen 0.0.0.0:9000       # every network interface, port 9000
```

It prints the address to open in a browser, with a token at the end, and keeps running until you press <kbd>Ctrl</kbd>+<kbd>C</kbd>:

```text
SpaceKnife headless mode · 0.3.3
Open in a browser:
  http://192.168.1.20:8375/#token=n3Vq…
Press Ctrl+C to stop.
```

While it runs, the terminal shows when a browser connects or goes away, and the backend's warnings and errors. The full log goes to the usual log file ([Data locations](../data-locations/)).

| How you installed SpaceKnife | The command |
|---|---|
| Windows installer, current user | `%LOCALAPPDATA%\Programs\SpaceKnife\SpaceKnife --headless` (the default folder) |
| Windows installer, all users | `"C:\Program Files\SpaceKnife\SpaceKnife" --headless` (the default folder) |
| Windows portable | `SpaceKnife --headless` in its folder |
| deb / rpm | `spaceknife --headless` |
| Linux portable | `./spaceknife --headless` in its folder |
| AppImage | `./SpaceKnife-<version>-x86_64.AppImage --headless` |
| macOS | `/Applications/SpaceKnife.app/Contents/MacOS/SpaceKnife --headless` |

:::note[Windows: leave out .exe]
`SpaceKnife.exe` is a windowed program: a terminal doesn't wait for it, so it couldn't ask its question or stop with <kbd>Ctrl</kbd>+<kbd>C</kbd>. Next to it is `SpaceKnife.com`, a small console program that starts it and waits. The command processor and PowerShell try `.com` before `.exe`, so write `SpaceKnife` without an extension.
:::

## Options

| Option | What it does |
|---|---|
| `--headless` | Headless mode, listening on this computer only: `127.0.0.1:8375` |
| `--listen <address>` | Listen on this address; starts headless mode by itself |
| `--yes` | Don't ask before opening SpaceKnife to the network. Needed when there is no terminal to answer in |
| `--tls-cert <file>` | Serve HTTPS with this certificate (PEM), together with `--tls-key` |
| `--tls-key <file>` | The certificate's private key (PEM) |
| `--help` | List the options |

The address can be written as:

| Address | Means |
|---|---|
| `192.168.1.20` | That address, port 8375 |
| `192.168.1.20:9000` | That address and port |
| `:9000` or `9000` | This computer only, that port |
| `0.0.0.0` | Every IPv4 interface |
| `::` or `[::]:9000` | Every interface, IPv6 and IPv4 |
| `[fd00::5]:9000` | An IPv6 address and port |
| `nas.lan` | A name; whatever it resolves to |

Port `0` lets the system pick a free port; the printed address has it.

Exit codes: `1` when you answered no, the address couldn't be used or the backend stopped; `2` for a wrong option, or no terminal to answer in. Stopped with <kbd>Ctrl</kbd>+<kbd>C</kbd> or by the system, it ends with `0` on Linux and macOS; on Windows <kbd>Ctrl</kbd>+<kbd>C</kbd> ends it at once, and its backend stops by itself.

## Opening it to the network

Anyone who can reach the address and has the link can browse the files on this computer with **your account's rights**, move them to the Recycle Bin, and delete them permanently. So before it listens on anything other than this computer (any address but `127.x.x.x`, `::1` or `localhost`), SpaceKnife shows a warning in red and asks:

```text
 ⚠ DANGER: SpaceKnife will be open to the network
  Listening on 0.0.0.0:8375: every network interface of this computer. …
Open it to the network? Type Y to go on, N to quit [y/N]:
```

Only <kbd>Y</kbd> (or `yes`) goes on. <kbd>N</kbd>, anything else, an empty line or the end of the input quits without listening.

Without a terminal (a service, a scheduled task, input from a file or `/dev/null`) there is nobody to ask: SpaceKnife refuses, unless you add `--yes`. The warning is still printed, to the log.

How it is protected:

- **A token in the link.** Each start makes a new random token. It sits after the `#`, which browsers don't send to the server; the page then takes it out of the address bar and keeps it for that browser tab, so a reload still works and the address you share or bookmark has no token. Every connection to the backend needs it, so after a restart the old links stop working: open the new one.
- **Only this site's pages.** The browser's WebSocket must come from the page this server served. Another website open in the same browser can't connect, even with the token.
- **The backend stays private.** It listens on `127.0.0.1` as always; headless mode forwards the browser's connection to it. The page's files themselves are public: they hold nothing about your files.

:::caution[Plain HTTP]
Without a certificate the traffic is plain HTTP. Anyone on the same network, or on the way, can read the token and every path and file name. On a network you don't control, use HTTPS or an SSH tunnel.
:::

**HTTPS.** Give a certificate and its key in PEM files, for example made with [mkcert](https://github.com/FiloSottile/mkcert) for the computer's name or address:

```bash
spaceknife --listen 0.0.0.0 --tls-cert nas.lan.pem --tls-key nas.lan-key.pem
```

The printed addresses then start with `https://`. A browser warns about a certificate it doesn't trust (self-signed): install your own certificate authority on the device you browse from instead of clicking past the warning.

**An SSH tunnel** needs no open port at all. On the server, listen on the server itself:

```bash
spaceknife --headless
```

On your computer, forward a local port to it, then open the printed address there (`http://localhost:8375/#token=…`):

```bash
ssh -L 8375:localhost:8375 lenny@nas.lan
```

**Firewalls.** The first time SpaceKnife listens on the network, Windows asks whether to allow it through the firewall: allow **Private networks** only. On Linux, open the port if a firewall is on, for example `sudo firewall-cmd --add-port=8375/tcp` or `sudo ufw allow 8375/tcp`.

## In the browser

The interface is the same as in the app, with a few differences because the files are on another computer:

- **The title bar** shows that computer's name instead of the window buttons, marked **HTTP** when the connection isn't encrypted (not over `localhost`). Its tooltip says that scans and deletions act on that computer. The browser tab's title has the name too.
- **Paths are that computer's.** Type them in the start page's path box (the placeholder shows that computer's path style); **Browse…** isn't there, and dropping a folder on the page doesn't work, since it would be a folder of the computer you browse from. Opening a snapshot, exporting a report and importing or exporting settings ask for a path on that computer, starting in its home folder.
- **No file manager, terminal or system menu.** **Open**, **Show in Explorer** (Finder, file manager), **Open terminal here** and the system menu aren't in the menus. Their keys (<kbd>Ctrl</kbd>+<kbd>E</kbd>, <kbd>Ctrl</kbd>+<kbd>&#96;</kbd>) copy the path instead and say so. On macOS, Full Disk Access is granted on that Mac.
- **Deleting happens on that computer.** **Move to trash** uses that computer's Recycle Bin (Trash), and **Delete permanently** deletes there. The delete basket and the confirmation say so, and the 5 seconds to undo work as in the app ([Freeing up space](../cleanup/)).
- **Updates.** The update card links to the release page; downloading and installing happen on that computer, in the app or with a new package.
- **The browser's keys win.** <kbd>Ctrl</kbd>+<kbd>W</kbd>, <kbd>Ctrl</kbd>+<kbd>T</kbd> and <kbd>Ctrl</kbd>+<kbd>N</kbd> close and open the browser's tabs, not SpaceKnife's: close a SpaceKnife tab with its × or a middle click. While a scan or a deletion runs, or a scan tab is open, the browser asks before you leave the page.
- **Several browsers at once.** Each browser tab has its own SpaceKnife tabs and views. Settings, snapshots and the delete basket belong to the computer, so they are shared.
- **Copying** uses the clipboard API over HTTPS and `localhost`; over plain HTTP the browser's older copy command.

When a scan finishes while the tab is in the background, a dot appears in the tab's title instead of the taskbar flashing.

## Linux servers without a display

Headless mode needs no X11 or Wayland: `--headless` is also Chromium's switch for running without a display, and `--listen` adds it by itself. The program still needs the libraries it is built against (GTK 3, NSS, GBM, ALSA and a few X11 libraries, but no X server); the deb and rpm packages install them, and on a minimal system with the AppImage or the portable copy install them yourself (for example `libgtk-3-0 libnss3 libgbm1 libasound2` on Ubuntu).

Chromium doesn't run as root without `--no-sandbox`. Run SpaceKnife as the user whose files you want to see, or add `--no-sandbox` when you must run it as root.

As a service, with systemd (`~/.config/systemd/user/spaceknife-headless.service`, then `systemctl --user enable --now spaceknife-headless`):

```ini
[Unit]
Description=SpaceKnife headless mode

[Service]
ExecStart=/usr/bin/spaceknife --listen 192.168.1.20:8375 --yes
Restart=on-failure

[Install]
WantedBy=default.target
```

The token changes at every start: read the address with `journalctl --user -u spaceknife-headless`.

## On macOS

Run the executable inside the app bundle (see the table above). Headless mode shows no Dock icon. What SpaceKnife may read is the same as in the app: grant **Full Disk Access** to SpaceKnife on that Mac to see everything ([Install and upgrade](../install/#full-disk-access)).
