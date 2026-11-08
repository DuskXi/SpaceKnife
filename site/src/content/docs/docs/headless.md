---
title: Headless mode
description: Run SpaceKnife without a window and use it in a browser, on this computer or from another one, with the risks of opening it to the network and how to keep it safe.
---

In headless mode SpaceKnife opens no window. The command-line program `spaceknife-cli` scans as the app does and serves the app's interface over HTTP, so you use SpaceKnife in a browser: on a server or NAS without a display, or on a desktop you reach from a laptop or a tablet. Everything you see and do in that browser happens on the computer that runs headless mode.

## Start it

Run `spaceknife-cli headless`, with an address to listen on if other devices should reach it:

```bash
spaceknife-cli headless                          # this computer only (127.0.0.1, port 8375)
spaceknife-cli headless --listen 192.168.1.20    # one address of this computer, port 8375
spaceknife-cli headless --listen 0.0.0.0:9000    # every network interface, port 9000
```

It prints the address to open in a browser, with a token at the end, and keeps running until you press <kbd>Ctrl</kbd>+<kbd>C</kbd>:

```text
SpaceKnife headless mode · 0.3.5
Open in a browser:
  http://192.168.1.20:8375/#token=n3Vq…
Press Ctrl+C to stop.
```

While it runs, the terminal shows when a browser connects or goes away, and the backend's warnings and errors. The full log goes to the usual log file ([Data locations](../data-locations/)).

`spaceknife-cli` is installed next to the app ([where it is](../command-line/#where-the-program-is)):

| How you installed SpaceKnife | The command |
|---|---|
| Windows installer | `%LOCALAPPDATA%\SpaceKnife\spaceknife-cli headless` (the default folder) |
| Windows portable | `.\spaceknife-cli headless` in its folder |
| deb / rpm | `spaceknife-cli headless` |
| Linux portable | `./spaceknife-cli headless` in its folder |
| macOS | `/Applications/SpaceKnife.app/Contents/MacOS/spaceknife-cli headless` |

It is a console program: the terminal waits for it, and <kbd>Ctrl</kbd>+<kbd>C</kbd> stops it. The pages it serves are the built interface in the `ui` folder next to `spaceknife-cli`; `--ui` names another folder.

<!-- TODO: confirm — the portable archives and the Windows installer put `ui` next to spaceknife-cli (apps/desktop/scripts/portable.mjs, src-tauri/tauri.bundle.json resources); the deb / rpm put it in /usr/lib/SpaceKnife/ui and the macOS app in Contents/Resources/ui, which spaceknife-cli doesn't look in yet (headless.rs default_ui_dir): there `--ui` is needed until it does. The AppImage has no way to run spaceknife-cli. -->

## Options

| Option | What it does |
|---|---|
| `--listen <address>` | Listen on this address. Without it: this computer only, `127.0.0.1:8375` |
| `--yes` | Don't ask before opening SpaceKnife to the network. Needed when there is no terminal to answer in |
| `--tls-cert <file>` | Serve HTTPS with this certificate (PEM), together with `--tls-key` |
| `--tls-key <file>` | The certificate's private key (PEM) |
| `--ui <folder>` | The built interface to serve (default: the `ui` folder next to the program) |
| `--help` | List the options |

`spaceknife-cli --headless` and `spaceknife-cli --listen <address>`, as earlier versions of the app took them, still work and mean `spaceknife-cli headless …`.

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

Exit codes: `1` when you answered no, the address couldn't be used or the backend didn't start; `2` for a wrong option, no terminal to answer in, a certificate that can't be read, or no built interface in the `ui` folder. Stopped with <kbd>Ctrl</kbd>+<kbd>C</kbd> or by the system, it ends with `0`.

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
- **One address.** The pages and the backend's WebSocket are served together on the address you chose; nothing else listens. The page's files themselves are public: they hold nothing about your files.

:::caution[Plain HTTP]
Without a certificate the traffic is plain HTTP. Anyone on the same network, or on the way, can read the token and every path and file name. On a network you don't control, use HTTPS or an SSH tunnel.
:::

**HTTPS.** Give a certificate and its key in PEM files, for example made with [mkcert](https://github.com/FiloSottile/mkcert) for the computer's name or address:

```bash
spaceknife-cli headless --listen 0.0.0.0 --tls-cert nas.lan.pem --tls-key nas.lan-key.pem
```

The printed addresses then start with `https://`. A browser warns about a certificate it doesn't trust (self-signed): install your own certificate authority on the device you browse from instead of clicking past the warning.

**An SSH tunnel** needs no open port at all. On the server, listen on the server itself:

```bash
spaceknife-cli headless
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

`spaceknife-cli` is a console program: it needs no X11 or Wayland and none of the window's libraries (GTK, WebKitGTK). On a server, the portable tar.gz is enough: unpack it and run `./spaceknife-cli headless`.

Run it as the user whose files you want to see. It runs as root too, but then everything in the browser acts with root's rights.

As a service, with systemd (`~/.config/systemd/user/spaceknife-headless.service`, then `systemctl --user enable --now spaceknife-headless`):

```ini
[Unit]
Description=SpaceKnife headless mode

[Service]
ExecStart=/usr/bin/spaceknife-cli headless --listen 192.168.1.20:8375 --yes
Restart=on-failure

[Install]
WantedBy=default.target
```

The token changes at every start: read the address with `journalctl --user -u spaceknife-headless`.

## On macOS

Run `spaceknife-cli` inside the app bundle (see the table above). Headless mode shows no Dock icon. macOS asks for access to protected places on behalf of the program that started it, usually Terminal: to see everything, grant **Full Disk Access** to that program ([Install and upgrade](../install/#full-disk-access)).
