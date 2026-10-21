"""Put a portable copy's new version in place: the backend executable's ``--apply-update`` mode.

「无缝更新」 on a portable copy (the Windows zip, the Linux tar.gz): the app unpacks the new version
into its updates folder, writes a plan (:class:`Plan`, JSON) and starts *the new version's* backend
executable with ``--apply-update <plan>``, detached, just before it quits. That process then:

1. says ``{"ok": true}`` on stdout once the plan is usable (the app waits for this line: when the
   helper can't start, the app goes on as it was), and lets go of its stdio;
2. waits for the app's main process to exit;
3. moves the app folder's entries, except ``data``, to the plan's ``old`` folder, trying again
   while files are still held (the closing app's own helpers, a virus scanner);
4. moves the new version's entries into the app folder. This process runs from among them: what
   can't be moved at once (Windows doesn't rename a folder with a running program in it) moves
   piece by piece, and is copied where even that fails;
5. starts the app again, with ``--updated`` once the new version is in place. When anything fails,
   the old files go back and the old version starts.

Every step is written to the plan's log. It never runs by itself: only the app starts it, after the
user's click. Only the standard library here: the frozen backend dispatches to it before anything
else (packaging/backend_entry.py).

The plan is a contract with every older version: an app hands its plan to the *newer* version's
backend, so later helpers keep reading ``v: 1``.
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import subprocess
import sys
import time
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

PLAN_VERSION = 1
QUIT_TIMEOUT_S = 60.0  # the app quits right after starting this
BUSY_TIMEOUT_S = 30.0  # files of the old version still held after it quit


class PlanError(ValueError):
    """The plan can't be used: nothing was touched."""


@dataclass(frozen=True)
class Plan:
    pid: int  # the app's main process, to wait for
    app: Path  # the portable copy's folder
    new: Path  # the unpacked new version (the folder with its executable)
    old: Path  # where the replaced files go (the new version's backend clears it)
    exe: Path  # the app's executable, in ``app``
    log: Path
    keep: tuple[str, ...] = ("data",)  # entries of ``app`` that stay where they are

    @classmethod
    def read(cls, path: Path) -> Plan:
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise PlanError(f"can't read the plan {path}: {exc}") from exc
        if not isinstance(raw, dict) or raw.get("v") != PLAN_VERSION:
            raise PlanError(f"{path} is not an update plan of version {PLAN_VERSION}")
        try:
            paths = {key: Path(str(raw[key])) for key in ("app", "new", "old", "exe", "log")}
            keep = tuple(str(k) for k in raw.get("keep", ["data"]))
            plan = cls(pid=int(raw["pid"]), keep=keep, **paths)
        except (KeyError, TypeError, ValueError) as exc:
            raise PlanError(f"{path}: {exc!r}") from exc
        plan.check()
        return plan

    def check(self) -> None:
        """The new version must be complete before anything is moved."""
        for name, p in (
            ("app", self.app),
            ("new", self.new),
            ("old", self.old),
            ("exe", self.exe),
            ("log", self.log),
        ):
            if not p.is_absolute():
                raise PlanError(f"{name} is not an absolute path: {p}")
        if not self.app.is_dir():
            raise PlanError(f"no folder {self.app}")
        if not _same(self.exe.parent, self.app):
            raise PlanError(f"{self.exe} is not in {self.app}")
        if not (self.new / self.exe.name).is_file():
            raise PlanError(f"the new version is incomplete: no {self.exe.name} in {self.new}")
        if _same(self.new, self.app) or _same(self.old, self.app):
            raise PlanError("the new and the old folder must not be the app's folder")
        if self.old.exists() and any(self.old.iterdir()):
            raise PlanError(f"{self.old} is not empty")


def _same(a: Path, b: Path) -> bool:
    return os.path.normcase(os.path.abspath(a)) == os.path.normcase(os.path.abspath(b))


class Log:
    """One timestamped line per step in the plan's log file (never fails)."""

    def __init__(self, path: Path) -> None:
        self.path = path

    def __call__(self, message: str) -> None:
        line = f"{datetime.now(UTC):%Y-%m-%d %H:%M:%S} {message}\n"
        with contextlib.suppress(OSError):
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with self.path.open("a", encoding="utf-8") as fh:
                fh.write(line)


# ---------------------------------------------------------------- waiting


def wait_gone(pid: int, timeout: float) -> bool:
    """True once process ``pid`` has exited (or never was), False when it still runs after ``timeout``."""
    if sys.platform == "win32":
        import ctypes

        synchronize = 0x00100000
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined,unused-ignore]
        handle = k32.OpenProcess(synchronize, False, pid)
        if not handle:
            return True  # gone
        try:
            return int(k32.WaitForSingleObject(handle, int(timeout * 1000))) == 0
        finally:
            k32.CloseHandle(handle)
    deadline = time.monotonic() + timeout
    while True:
        if not _alive(pid):
            return True
        if time.monotonic() > deadline:
            return False
        time.sleep(0.1)


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # a process that exited but was not reaped by its parent yet counts as gone
    with contextlib.suppress(OSError, IndexError):
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii", errors="replace")
        return stat.rsplit(")", 1)[1].split()[0] != "Z"
    return True


# ---------------------------------------------------------------- moving


def entries(folder: Path, keep: tuple[str, ...]) -> list[Path]:
    """The folder's entries, except the kept names (case-insensitive on Windows)."""
    kept = {os.path.normcase(k) for k in keep}
    return sorted(p for p in folder.iterdir() if os.path.normcase(p.name) not in kept)


def move_out(src: Path, dst: Path, keep: tuple[str, ...], log: Log, timeout: float = BUSY_TIMEOUT_S) -> None:
    """Move ``src``'s entries (except ``keep``) into ``dst``, trying again while one is held.
    On failure what was moved goes back, and OSError is raised."""
    dst.mkdir(parents=True, exist_ok=True)
    moved: list[str] = []
    deadline = time.monotonic() + timeout
    try:
        for entry in entries(src, keep):
            said = False
            while True:
                try:
                    os.replace(entry, dst / entry.name)
                    break
                except OSError as exc:
                    if time.monotonic() > deadline:
                        raise
                    if not said:
                        log(f"busy: {entry.name}: {exc}")
                        said = True
                    time.sleep(0.5)
            moved.append(entry.name)
    except OSError:
        for name in reversed(moved):
            with contextlib.suppress(OSError):
                os.replace(dst / name, src / name)
        raise


def place(src: Path, dst: Path) -> None:
    """Move ``src`` to ``dst`` (which doesn't exist). What can't be renamed at once (an open file, a
    folder holding one) moves piece by piece, and is copied where even that fails."""
    try:
        os.replace(src, dst)
        return
    except OSError:
        if not src.is_dir() or src.is_symlink():
            shutil.copy2(src, dst, follow_symlinks=False)
            return
    dst.mkdir()
    with contextlib.suppress(OSError):
        shutil.copystat(src, dst)
    for child in sorted(src.iterdir()):
        place(child, dst / child.name)
    with contextlib.suppress(OSError):
        src.rmdir()  # stays when something in it had to be copied


def clear(folder: Path, keep: tuple[str, ...], log: Log) -> None:
    """Remove ``folder``'s entries except ``keep`` (taking out new files that only partly arrived)."""
    for entry in entries(folder, keep):
        try:
            if entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry)
            else:
                entry.unlink()
        except OSError as exc:
            log(f"could not take out {entry.name}: {exc}")


# ---------------------------------------------------------------- starting the app


def launch_env(env: Mapping[str, str]) -> dict[str, str]:
    """The environment for the app: without what the frozen backend's bootloader set for itself
    (``_PYI_*``; its own library folder on ``LD_LIBRARY_PATH``, restored from ``…_ORIG``)."""
    out = {k: v for k, v in env.items() if not k.startswith(("_PYI_", "_MEIPASS"))}
    base = getattr(sys, "_MEIPASS", None)
    for var in ("LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"):
        orig = out.pop(f"{var}_ORIG", None)
        if orig is not None:
            out[var] = orig
        elif base and base in out.get(var, ""):
            out.pop(var)
    return out


def launch(exe: Path, args: list[str]) -> None:
    """Start the app on its own: it outlives this process, without its stdio."""
    argv = [str(exe), *args]
    io, cwd, env = subprocess.DEVNULL, str(exe.parent), launch_env(os.environ)
    if sys.platform == "win32":
        flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
        try:  # out of any job this process is in, so that closing it doesn't end the app
            flags_out = flags | subprocess.CREATE_BREAKAWAY_FROM_JOB
            subprocess.Popen(argv, stdin=io, stdout=io, stderr=io, cwd=cwd, env=env, creationflags=flags_out)
        except OSError:  # a job that doesn't allow it
            subprocess.Popen(argv, stdin=io, stdout=io, stderr=io, cwd=cwd, env=env, creationflags=flags)
    else:
        subprocess.Popen(argv, stdin=io, stdout=io, stderr=io, cwd=cwd, env=env, start_new_session=True)


# ---------------------------------------------------------------- the swap


def run(
    plan: Plan,
    log: Log,
    *,
    wait: Callable[[int, float], bool] = wait_gone,
    start: Callable[[Path, list[str]], None] = launch,
) -> int:
    """Steps 2-5 of the module docstring; 0 when the new version is in place."""
    log(f"waiting for SpaceKnife ({plan.pid}) to quit")
    if not wait(plan.pid, QUIT_TIMEOUT_S):
        log("SpaceKnife did not quit: kept the old version")
        return 1
    try:
        move_out(plan.app, plan.old, plan.keep, log)
    except OSError as exc:
        log(f"could not move the old files out: {exc}")
        return _keep_old(plan, log, start)
    try:
        for entry in entries(plan.new, plan.keep):
            place(entry, plan.app / entry.name)
    except OSError as exc:
        log(f"could not move the new files in: {exc}")
        clear(plan.app, plan.keep, log)
        try:
            move_out(plan.old, plan.app, (), log)
        except OSError as back:
            log(f"could not restore the old files: {back}")
        return _keep_old(plan, log, start)
    log("updated")
    try:
        start(plan.exe, ["--updated"])
    except OSError as exc:
        log(f"could not start SpaceKnife: {exc}")
        return 1
    return 0


def _keep_old(plan: Plan, log: Log, start: Callable[[Path, list[str]], None]) -> int:
    log("kept the old version")
    try:
        start(plan.exe, [])
    except OSError as exc:
        log(f"could not start SpaceKnife: {exc}")
    return 1


def _say(message: dict[str, object]) -> None:
    if sys.stdout is not None:
        with contextlib.suppress(OSError, ValueError):
            sys.stdout.write(json.dumps(message) + "\n")
            sys.stdout.flush()


def _let_go_of_stdio() -> None:
    """The app reads one line and quits: later writes must not fail on its closed pipe."""
    with contextlib.suppress(OSError):
        null = os.open(os.devnull, os.O_RDWR)
        for fd in (0, 1, 2):
            os.dup2(null, fd)


def main(argv: list[str]) -> int:
    """``--apply-update <plan.json>``."""
    if len(argv) != 1:
        _say({"ok": False, "error": "usage: --apply-update <plan.json>"})
        return 2
    try:
        plan = Plan.read(Path(argv[0]))
    except PlanError as exc:
        Log(Path(argv[0]).with_suffix(".log"))(f"not started: {exc}")
        _say({"ok": False, "error": str(exc)})
        return 2
    _say({"ok": True})
    _let_go_of_stdio()
    return run(plan, Log(plan.log))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
