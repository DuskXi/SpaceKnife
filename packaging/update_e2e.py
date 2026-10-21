"""End-to-end test of 「无缝更新」 and of the uninstaller's cleanup, on a CI runner only.

    python packaging/update_e2e.py portable dist/electron/SpaceKnife-<v>-win-x64-portable.zip
    xvfb-run -a python packaging/update_e2e.py portable dist/electron/SpaceKnife-<v>-linux-x64-portable.tar.gz
    python packaging/update_e2e.py nsis dist/electron/SpaceKnife-<v>-Setup-x64.exe [--previous <older>]

Both modes serve a release list on 127.0.0.1 (SPACEKNIFE_UPDATE_FEED) that offers the next patch
version, whose file is this build again (renamed; the portable archive also gets a marker file). The
app is started minimized with the DevTools port on; the test waits until the update badge says the
file is downloaded and verified, then clicks the badge and 「无缝更新」 like a user.

- portable: the program files are swapped by the new version's own backend (--apply-update, no
  script), the data folder stays, the app comes back with --updated and its backend clears the old
  files out of the updates folder.
- nsis (Windows): with --previous, the published installer is installed and upgraded the way the
  app runs the new installer (--updated /S --force-run /currentuser). Then the in-app update, a silent
  uninstall (keeps the snapshots, settings and logs; removes the caches and downloaded updates), the
  uninstaller's 「你的数据」 page with its box ticked (removes everything), and --delete-app-data.

It installs, updates and uninstalls SpaceKnife in this user's profile and starts its window, so it
refuses to run outside GitHub Actions. Exit status 1 on failure.
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.request
import zipfile
from collections.abc import Callable
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, NoReturn

from smoke_app import Page
from websockets.sync.client import connect

from spaceknife.update import parse_checksums

WIN = sys.platform == "win32"


APP_LOG = Path(tempfile.gettempdir()) / f"sk-update-e2e-app-{os.getpid()}.log"


def fail(message: str) -> NoReturn:
    if APP_LOG.exists():  # what the app printed (main's "[update] …" lines, backend errors)
        lines = APP_LOG.read_text(encoding="utf-8", errors="replace").splitlines()
        print("---- the app's output (last 80 lines)", *lines[-80:], "----", sep="\n", flush=True)
    raise SystemExit(f"FAIL {message}")


def wait(what: str, check: Callable[[], Any], timeout: float = 120.0, every: float = 0.5) -> Any:
    deadline = time.monotonic() + timeout
    while True:
        value = check()
        if value:
            return value
        if time.monotonic() > deadline:
            fail(f"timed out after {timeout:.0f} s waiting for {what}")
        time.sleep(every)


def step(text: str) -> None:
    print(f"\n== {text}", flush=True)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def next_patch(version: str) -> str:
    m = re.match(r"^(\d+)\.(\d+)\.(\d+)", version)
    if not m:
        fail(f"not a version: {version}")
    major, minor, patch = (int(g) for g in m.groups())
    return f"{major}.{minor}.{patch + 1}"


# ------------------------------------------------------------------ the release list on 127.0.0.1


class Feed:
    """GitHub's release list with one release, ``version``, and its files (served from disk)."""

    def __init__(self, version: str, files: list[Path]) -> None:
        self.files = {f.name: f for f in files}
        feed = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                if self.path == "/releases":
                    body = json.dumps(feed.releases()).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                file = feed.files.get(self.path.removeprefix("/files/"))
                if file is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(file.stat().st_size))
                self.end_headers()
                with contextlib.suppress(ConnectionError), file.open("rb") as f:
                    shutil.copyfileobj(f, self.wfile, 1 << 20)

            def log_message(self, format: str, *args: Any) -> None:
                print(f"  feed: {format % args}", flush=True)

        self.version = version
        self.http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.http.daemon_threads = True
        self.base = f"http://127.0.0.1:{self.http.server_address[1]}"
        threading.Thread(target=self.http.serve_forever, daemon=True, name="feed").start()

    @property
    def url(self) -> str:
        return f"{self.base}/releases"

    def releases(self) -> list[dict[str, Any]]:
        assets = [
            {"name": n, "browser_download_url": f"{self.base}/files/{n}", "size": f.stat().st_size}
            for n, f in self.files.items()
        ]
        return [
            {
                "tag_name": f"v{self.version}",
                "name": f"SpaceKnife {self.version}",
                "draft": False,
                "prerelease": False,
                "html_url": f"{self.base}/tag/v{self.version}",
                "body": "The end-to-end test's release.",
                "published_at": "2026-10-21T00:00:00Z",
                "assets": assets,
            }
        ]

    def close(self) -> None:
        self.http.shutdown()


def sums_file(folder: Path, name: str, files: list[Path]) -> Path:
    out = folder / name
    out.write_text("".join(f"{sha256(f)}  {f.name}\n" for f in files), encoding="utf-8")
    return out


# ------------------------------------------------------------------ processes


def processes() -> list[tuple[int, str]]:
    """(pid, command line) of every process this user can see."""
    if WIN:
        script = (
            "Get-CimInstance Win32_Process | ForEach-Object "
            "{ [string]$_.ProcessId + [char]9 + [string]$_.CommandLine }"
        )
        out = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        ).stdout
        rows = []
        for line in out.splitlines():
            pid, _, cmd = line.partition("\t")
            if pid.strip().isdigit():
                rows.append((int(pid), cmd))
        return rows
    rows = []
    for d in Path("/proc").iterdir():
        if d.name.isdigit():
            with contextlib.suppress(OSError):
                cmd = (d / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
                rows.append((int(d.name), cmd))
    return rows


def app_processes(exe: Path) -> list[tuple[int, str]]:
    needle = str(exe).lower()
    return [(p, c) for p, c in processes() if needle in c.lower()]


def updated_main(exe: Path) -> int | None:
    """The app's main process when it was started with --updated (not a Chromium helper)."""
    for pid, cmd in app_processes(exe):
        if "--updated" in cmd and "--type=" not in cmd:
            return pid
    return None


def kill_app(folder: Path) -> None:
    """Every process started from ``folder`` (the app, its helpers and its backend)."""
    needle = str(folder).lower()
    for pid, cmd in processes():
        if needle in cmd.lower() and pid != os.getpid():
            if WIN:
                subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True, check=False)
            else:
                with contextlib.suppress(OSError):
                    os.kill(pid, 9)
    time.sleep(1.0)


# ------------------------------------------------------------------ the window


def devtools_page(port_file: Path, proc: subprocess.Popen[bytes] | None) -> str:
    def port() -> int | None:
        if proc is not None and proc.poll() is not None:
            fail(f"the app exited early with {proc.returncode}")
        text = port_file.read_text() if port_file.exists() else ""
        return int(text.split()[0]) if text.strip() else None

    devtools = wait("the DevTools port", port, 90)

    def target() -> str | None:
        with urllib.request.urlopen(f"http://127.0.0.1:{devtools}/json/list", timeout=5) as r:
            pages = [t for t in json.load(r) if t.get("type") == "page"]
        return pages[0]["webSocketDebuggerUrl"] if pages else None

    return str(wait("the window", target, 90))


def restart_and_update(url: str, version: str) -> None:
    """Wait for the badge to say the update is ready, then click it and 「无缝更新」."""
    with connect(url, max_size=None) as ws:
        page = Page(ws)
        state = wait(
            "the backend",
            lambda: page.eval("window.sk.backend().then(s => s.state === 'starting' ? null : s)"),
        )
        if state.get("state") != "ready":
            fail(f"backend {state}")
        print(f"backend ready, version {page.eval('window.sk.version')}", flush=True)
        # the first automatic check comes a minute after the start, the download right after it
        wait(
            f"the badge of {version} to be ready",
            lambda: page.eval("!!document.querySelector('.update-badge.ready')"),
            240,
            1,
        )
        badge = page.eval("document.querySelector('.update-badge').textContent")
        print(f"badge: {badge!r}", flush=True)
        page.eval("document.querySelector('.update-badge').click(), true")
        button = "document.querySelector('.update-card .uc-choice.seamless')"
        wait("the update card", lambda: page.eval(f"!!{button}"), 30)
        card = page.eval("document.querySelector('.update-card').innerText")
        print(f"card: {card!r}", flush=True)
        with contextlib.suppress(Exception):  # the window goes away while it answers
            page.eval(f"{button}.click(), true")
        with contextlib.suppress(Exception):  # still here after a while: the card says why
            time.sleep(8)
            card = page.eval("(document.querySelector('.update-card') || document.body).innerText")
            print(f"after the click, still open: {card[:600]!r}", flush=True)


def start_app(exe: Path, env: dict[str, str]) -> subprocess.Popen[bytes]:
    argv = [str(exe), "--remote-debugging-port=0"]
    out = APP_LOG.open("ab")
    return subprocess.Popen(argv, env=env, stdout=out, stderr=subprocess.STDOUT)


def app_env(feed: Feed, **extra: str) -> dict[str, str]:
    env = {**os.environ, "SPACEKNIFE_START_MINIMIZED": "1", "SPACEKNIFE_UPDATE_FEED": feed.url, **extra}
    env.pop("SPACEKNIFE_HOME", None)
    return env


# ------------------------------------------------------------------ portable


def portable(archive: Path) -> None:
    m = re.match(r"^SpaceKnife-(.+)-(win|linux)-x64-portable\.(zip|tar\.gz)$", archive.name)
    if not m:
        fail(f"not a portable archive: {archive.name}")
    version, target, ext = m.groups()
    newer = next_patch(version)
    top_old = f"SpaceKnife-{version}-{target}-x64-portable"
    top_new = f"SpaceKnife-{newer}-{target}-x64-portable"
    exe_name = "SpaceKnife.exe" if WIN else "spaceknife"
    work = Path(tempfile.mkdtemp(prefix="sk-update-e2e-"))
    try:
        step(f"unpack {archive.name}")
        if ext == "zip":
            with zipfile.ZipFile(archive) as zf:
                zf.extractall(work)
        else:
            subprocess.run(["tar", "-xzf", str(archive), "-C", str(work)], check=True)
        app_dir = work / top_old
        exe = app_dir / exe_name
        if not exe.is_file():
            fail(f"no {exe_name} in {app_dir}")
        data = app_dir / "data"
        keep = data / "e2e-keep.txt"
        keep.write_text("must survive the update", encoding="utf-8")

        step(f"the next version {newer}: this build again, with a marker file")
        offered = work / "feed" / f"{top_new}.{ext}"
        offered.parent.mkdir()
        marker = "E2E_NEW_BUILD.txt"
        if ext == "zip":
            with zipfile.ZipFile(archive) as src, zipfile.ZipFile(offered, "w", zipfile.ZIP_STORED) as dst:
                for info in src.infolist():
                    new = zipfile.ZipInfo(top_new + info.filename[len(top_old) :], info.date_time)
                    new.external_attr, new.compress_type = info.external_attr, zipfile.ZIP_STORED
                    if info.is_dir():
                        dst.writestr(new, b"")
                        continue
                    with src.open(info) as r, dst.open(new, "w") as w:
                        shutil.copyfileobj(r, w, 1 << 20)
                dst.writestr(f"{top_new}/{marker}", f"{newer}\n")
        else:
            with tarfile.open(archive, "r:gz") as src, tarfile.open(offered, "w:gz", compresslevel=1) as dst:
                for member in src.getmembers():
                    fileobj = src.extractfile(member) if member.isfile() else None
                    member.name = top_new + member.name[len(top_old) :]
                    if member.islnk() and member.linkname.startswith(top_old):
                        member.linkname = top_new + member.linkname[len(top_old) :]
                    # a long name is in the PAX header too, and that one wins when it is written
                    pax = member.pax_headers
                    member.pax_headers = {k: v for k, v in pax.items() if k not in ("path", "linkpath")}
                    dst.addfile(member, fileobj)
                note = tarfile.TarInfo(f"{top_new}/{marker}")
                body = f"{newer}\n".encode()
                note.size, note.mode, note.mtime = len(body), 0o644, int(time.time())
                dst.addfile(note, io.BytesIO(body))
        sums = sums_file(offered.parent, f"SHA256SUMS-portable-{target}-x64.txt", [offered])
        feed = Feed(newer, [offered, sums])
        print(f"offering {offered.name} ({offered.stat().st_size >> 20} MB) at {feed.url}", flush=True)

        step("start the app and restart into the update")
        proc = start_app(exe, app_env(feed))
        try:
            restart_and_update(devtools_page(data / "electron" / "DevToolsActivePort", proc), newer)
            wait("the old app to quit", lambda: proc.poll() is not None, 60)
            updates = data / "data" / "updates"
            log = updates / f"update-{newer}.log"

            def swapped() -> str | None:  # read before the new backend clears the updates folder
                try:
                    text = log.read_text(encoding="utf-8")
                except OSError:  # not there yet
                    return None
                last = text.splitlines()[-1] if text.strip() else ""
                ended = last.endswith(("updated", "kept the old version")) or "did not quit" in last
                return text if ended else None

            text = wait("the swap's log to say how it ended", swapped, 120)
            print(f"the swap's log:\n{text}", flush=True)
            if not text.splitlines()[-1].endswith(" updated"):
                fail("the swap did not finish with 'updated'")
            pid = wait("the app to come back with --updated", lambda: updated_main(exe), 120)
            print(f"back with --updated (pid {pid})", flush=True)
            if not (app_dir / marker).is_file():
                fail("the new program files are not in place")
            if not keep.is_file():
                fail("the data folder lost a file")
            # the new backend clears what the swap left: the old files (version <= the running one)
            wait(
                f"previous-{version} to be cleared",
                lambda: not (updates / f"previous-{version}").exists(),
                90,
            )
            print(f"updates folder now holds: {sorted(p.name for p in updates.iterdir())}", flush=True)
        finally:
            if proc.poll() is None:
                proc.kill()
            kill_app(app_dir)
            feed.close()
        print("portable update: ok")
    finally:
        shutil.rmtree(work, ignore_errors=True)


# ------------------------------------------------------------------ the Windows installer


LOCAL = Path(os.environ.get("LOCALAPPDATA", ""))
ROAMING = Path(os.environ.get("APPDATA", ""))
DATA = LOCAL / "SpaceKnife"  # snapshots, settings, logs; Cache and updates inside
PROFILE = ROAMING / "SpaceKnife" / "electron"  # the window's storage and caches
UPDATER_CACHE = LOCAL / "spaceknife-electron-updater"


def uninstall_entry() -> dict[str, str] | None:
    """SpaceKnife's entry under HKCU\\…\\Uninstall (a per-user install), None when there is none."""
    import winreg

    root = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
    with contextlib.suppress(OSError), winreg.OpenKey(winreg.HKEY_CURRENT_USER, root) as key:
        for i in range(winreg.QueryInfoKey(key)[0]):
            with winreg.OpenKey(key, winreg.EnumKey(key, i)) as sub:
                values: dict[str, str] = {}
                for j in range(winreg.QueryInfoKey(sub)[1]):
                    name, value, _ = winreg.EnumValue(sub, j)
                    values[name] = str(value)
                if values.get("DisplayName", "").startswith("SpaceKnife"):
                    return values
    return None


def install_folder(entry: dict[str, str]) -> Path:
    """The program folder: where the uninstaller is (the quoted path of UninstallString)."""
    m = re.match(r'^"([^"]+)"', entry.get("UninstallString", ""))
    return Path(m.group(1)).parent if m else LOCAL / "Programs" / "SpaceKnife"


def install_location() -> str | None:
    """InstallLocation under HKCU\\Software\\<guid>: where the next installer looks for the program."""
    import winreg

    with contextlib.suppress(OSError), winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Software") as key:
        for i in range(winreg.QueryInfoKey(key)[0]):
            with contextlib.suppress(OSError), winreg.OpenKey(key, winreg.EnumKey(key, i)) as sub:
                if winreg.QueryValueEx(sub, "ShortcutName")[0] == "SpaceKnife":
                    return str(winreg.QueryValueEx(sub, "InstallLocation")[0])
    return None


def expect_registered(exe: Path, version: str) -> None:
    """After an update SpaceKnife is still in Windows' app list, as ``version`` in its folder, and
    still a while later: an in-app upgrade from 0.3.1 to 0.3.2 once ended without both keys."""
    for check in range(2):
        if check:
            time.sleep(10)
        entry = uninstall_entry()
        if entry is None:
            fail("the uninstall entry is gone after the update")
        if entry.get("DisplayVersion") != version or install_folder(entry) != exe.parent:
            fail(f"the uninstall entry says {entry.get('DisplayVersion')!r} in {install_folder(entry)}")
        location = install_location()
        if location is None or Path(location) != exe.parent:
            fail(f"InstallLocation under HKCU\\Software\\<guid> is {location!r}")
    print(f"registered: {version} in {exe.parent}", flush=True)


def installed() -> tuple[Path, str]:
    entry = wait("SpaceKnife's uninstall entry", uninstall_entry, 60)
    folder = install_folder(entry)
    exe = folder / "SpaceKnife.exe"
    if not exe.is_file():
        fail(f"no SpaceKnife.exe in {folder}")
    return exe, entry.get("DisplayVersion", "")


def install(setup: Path, *args: str) -> tuple[Path, str]:
    argv = [str(setup), *(args or ("/S", "/currentuser"))]
    print(f"$ {' '.join(argv)}", flush=True)
    code = subprocess.run(argv, check=False, timeout=600).returncode
    if code != 0:
        fail(f"{setup.name} exited with {code}")
    exe, version = installed()
    print(f"installed {version} in {exe.parent}", flush=True)
    return exe, version


def seed() -> dict[str, Path]:
    """Files in each place the uninstaller treats on its own."""
    files = {
        "data": DATA / "e2e-keep.txt",
        "cache": DATA / "Cache" / "e2e.bin",
        "updates": DATA / "updates" / "SpaceKnife-0.0.1-Setup-x64.exe",
        "profile": PROFILE / "e2e.txt",
        "updater": UPDATER_CACHE / "e2e.txt",
    }
    for f in files.values():
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text("e2e", encoding="utf-8")
    return files


def expect(files: dict[str, Path], kept: set[str]) -> None:
    for name, f in files.items():
        if f.exists() != (name in kept):
            fail(f"{name} ({f}) was {'removed' if name in kept else 'left behind'}")
    print(f"kept: {sorted(kept) or 'nothing'}; removed: {sorted(set(files) - kept) or 'nothing'}", flush=True)


def wait_uninstalled(folder: Path) -> None:
    wait("the uninstall entry to go", lambda: uninstall_entry() is None, 180)
    wait("the program folder to go", lambda: not (folder / "SpaceKnife.exe").exists(), 60)


def quiet_uninstall(*extra: str) -> None:
    entry = uninstall_entry()
    if entry is None:
        fail("nothing to uninstall")
    command = entry["QuietUninstallString"] + "".join(f" {a}" for a in extra)
    folder = install_folder(entry)
    print(f"$ {command}", flush=True)
    subprocess.run(command, check=False, timeout=300)  # returns at once: it runs from a copy in %TEMP%
    wait_uninstalled(folder)


def run_restarted_app(exe: Path, stale: Path) -> None:
    """The installer starts the new version with --updated; its backend clears ``stale`` (an update
    file of an older version, put there after the old app quit) when it starts."""
    pid = wait("the app to come back with --updated", lambda: updated_main(exe), 300, 1)
    print(f"the installer started the new version with --updated (pid {pid})", flush=True)
    wait(f"the new backend to clear {stale.name}", lambda: not stale.exists(), 120)
    kill_app(exe.parent)


def stale_update() -> Path:
    f = DATA / "updates" / "update-0.0.1.log"
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text("an update file of an old version", encoding="utf-8")
    return f


class Windows:
    """Just enough Win32 to drive the uninstaller's pages (by window class, control and text)."""

    def __init__(self) -> None:
        import ctypes
        from ctypes import wintypes

        self.ct, self.wt = ctypes, wintypes
        self.user32 = ctypes.WinDLL("user32", use_last_error=True)
        self.user32.SendMessageW.restype = ctypes.c_ssize_t
        self.user32.SendMessageW.argtypes = (wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM)
        self.proc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def text(self, hwnd: int) -> str:
        buf = self.ct.create_unicode_buffer(512)
        self.user32.GetWindowTextW(hwnd, buf, 512)
        return buf.value

    def cls(self, hwnd: int) -> str:
        buf = self.ct.create_unicode_buffer(256)
        self.user32.GetClassNameW(hwnd, buf, 256)
        return buf.value

    def _collect(self, found: list[int]) -> Any:
        def keep(hwnd: int, _param: int) -> bool:
            found.append(hwnd)
            return True

        return self.proc(keep)

    def tops(self) -> list[int]:
        found: list[int] = []
        self.user32.EnumWindows(self._collect(found), 0)
        return found

    def children(self, hwnd: int) -> list[int]:
        found: list[int] = []
        self.user32.EnumChildWindows(hwnd, self._collect(found), 0)
        return found

    def uninstaller(self) -> int | None:
        for h in self.tops():
            if self.user32.IsWindowVisible(h) and self.cls(h) == "#32770" and "SpaceKnife" in self.text(h):
                return h
        return None

    def button(self, dialog: int, control: int) -> int:
        return int(self.user32.GetDlgItem(dialog, control) or 0)

    def press(self, dialog: int, control: int) -> None:
        """What a click on the button does: WM_COMMAND with BN_CLICKED to the dialog."""
        self.user32.SendMessageW(dialog, 0x0111, control, self.button(dialog, control))

    def checked(self, hwnd: int) -> bool:
        return bool(self.user32.SendMessageW(hwnd, 0x00F0, 0, 0) == 1)  # BM_GETCHECK

    def check(self, hwnd: int) -> None:
        self.user32.SendMessageW(hwnd, 0x00F1, 1, 0)  # BM_SETCHECK, BST_CHECKED


BOX_TEXTS = ("Also delete the snapshots, settings and logs", "同时删除快照、设置和日志")


def uninstall_with_the_box_ticked() -> None:
    entry = uninstall_entry()
    if entry is None:
        fail("nothing to uninstall")
    folder = install_folder(entry)
    print(f"$ {entry['UninstallString']}", flush=True)
    subprocess.Popen(entry["UninstallString"])
    w = Windows()
    dialog = wait("the uninstaller's window", w.uninstaller, 60)
    print(f"window {w.text(dialog)!r}", flush=True)
    wait("its Next button", lambda: w.user32.IsWindowEnabled(w.button(dialog, 1)), 30)
    w.press(dialog, 1)  # past the welcome page

    def box() -> int | None:
        for h in w.children(dialog):
            if w.cls(h) == "Button" and w.text(h) in BOX_TEXTS:
                return h
        return None

    tick = wait("the 「你的数据」 page's box", box, 30)
    texts = [w.text(h) for h in w.children(dialog) if w.cls(h) == "Static" and w.text(h)]
    print(f"page texts: {texts}", flush=True)
    if w.checked(tick):
        fail("the box is ticked by default")
    if not any(str(DATA).lower() in t.lower() for t in texts):
        fail(f"the page doesn't name {DATA}")
    w.check(tick)
    w.press(dialog, 1)  # uninstall

    def finished() -> bool:
        h = w.uninstaller()
        if h is None:
            return True
        b = w.button(h, 1)
        if (
            b
            and w.user32.IsWindowEnabled(b)
            and w.text(b).replace("&", "") in ("Finish", "Close", "完成", "关闭")
        ):
            w.press(h, 1)
        return False

    wait("the uninstaller to finish", finished, 180, 1)
    wait_uninstalled(folder)


def nsis(setup: Path, previous: Path | None) -> None:
    if not WIN:
        fail("the installer test is for Windows")
    m = re.match(r"^SpaceKnife-(.+)-Setup-x64\.exe$", setup.name)
    if not m:
        fail(f"not the installer: {setup.name}")
    version = m.group(1)
    if uninstall_entry() is not None:
        fail("SpaceKnife is installed already")
    update_args = ("--updated", "/S", "--force-run", "/currentuser")  # main/update.ts nsisUpdateArgs

    if previous is not None:
        old = re.match(r"^SpaceKnife-(.+)-Setup-x64\.exe$", previous.name)
        if old is None or old.group(1) == version:
            print(f"\n== skipping the upgrade from {previous.name}: not an older version", flush=True)
        else:
            step(f"upgrade from {old.group(1)}, the way the app starts the new installer")
            sums = previous.parent / "SHA256SUMS.txt"
            listed = parse_checksums(sums.read_text(encoding="utf-8")) if sums.is_file() else {}
            if listed.get(previous.name) != sha256(previous):
                fail(f"{previous.name} doesn't match {sums}")
            install(previous)
            files = seed()
            stale = stale_update()
            exe, now = install(setup, *update_args)
            if now != version:
                fail(f"the upgrade left version {now!r}")
            run_restarted_app(exe, stale)
            expect_registered(exe, version)
            # an update touches nothing of the user's; the new backend cleared the old update files
            expect(files, {"data", "cache", "profile"})
            quiet_uninstall()

    step("in-app update: 「无缝更新」 runs the next version's installer silently")
    exe, _ = install(setup)
    files = seed()
    newer = next_patch(version)
    work = Path(tempfile.mkdtemp(prefix="sk-update-e2e-"))
    offered = work / f"SpaceKnife-{newer}-Setup-x64.exe"
    shutil.copyfile(setup, offered)
    feed = Feed(newer, [offered, sums_file(work, "SHA256SUMS.txt", [offered])])
    port_file = PROFILE / "DevToolsActivePort"
    port_file.unlink(missing_ok=True)
    proc = start_app(exe, app_env(feed))
    try:
        restart_and_update(devtools_page(port_file, proc), newer)
        wait("the old app to quit", lambda: proc.poll() is not None, 60)
        stale = stale_update()  # the installer is still at work: only the new backend can clear it
        run_restarted_app(exe, stale)
        expect_registered(exe, version)
        # the old app's backend cleared the seeded 0.0.1 installer when it started; the offered
        # (newer) file stays until a version at least as new runs
        expect(files, {"data", "cache", "profile"})
        if not (DATA / "updates" / offered.name).is_file():
            fail("the downloaded update is not in the updates folder")
    finally:
        if proc.poll() is None:
            proc.kill()
        kill_app(exe.parent)
        feed.close()
        shutil.rmtree(work, ignore_errors=True)

    step("silent uninstall: keeps the snapshots, settings and logs, removes the rest")
    files = seed()
    quiet_uninstall()
    expect(files, {"data"})

    step("uninstall with 「同时删除快照、设置和日志」 ticked")
    install(setup)
    files = seed()
    uninstall_with_the_box_ticked()
    expect(files, set())
    if DATA.exists():
        fail(f"{DATA} is still there")

    step("silent uninstall with --delete-app-data")
    install(setup)
    files = seed()
    quiet_uninstall("--delete-app-data")
    expect(files, set())
    print("installer update and uninstall: ok")


def main(argv: list[str]) -> int:
    if os.environ.get("GITHUB_ACTIONS") != "true":
        fail("this test installs, updates and uninstalls SpaceKnife in this user's profile: CI only")
    if len(argv) >= 2 and argv[0] == "portable":
        portable(Path(argv[1]).resolve())
    elif len(argv) >= 2 and argv[0] == "nsis":
        previous = Path(argv[argv.index("--previous") + 1]).resolve() if "--previous" in argv else None
        nsis(Path(argv[1]).resolve(), previous)
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
