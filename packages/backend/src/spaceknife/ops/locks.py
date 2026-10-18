"""Which processes hold files open (docs/manage.md §2.4, UI-W5-37).

* Windows: Restart Manager (``RmStartSession`` / ``RmRegisterResources`` / ``RmGetList``), the
  friendly name is ``strAppName`` ("Microsoft Word", "Python"). A folder registers its files and
  subfolders (at most :data:`MAX_FILES` per call). One session answers "is anything held at all";
  only when something is, sessions on halves of the set find which paths (a bisection: a few
  sessions for a few locked items among thousands).
* Linux: the links in ``/proc/<pid>/fd`` (processes we may not inspect are skipped). An open file
  does not block deleting it there, but the space only comes back once it is closed.
* macOS: ``lsof -Fpcn`` when it is installed (best effort, not tested on a Mac), else nothing.

Synchronous and possibly slow (Restart Manager takes about 0.1–0.2 s per session): call it from a
worker thread.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from collections.abc import Iterable, Sequence

from loguru import logger

MAX_FILES = 20_000  # names registered per call at most (a folder's walk stops there)
PER_FOLDER = 4_000  # ... and per folder
LSOF_TIMEOUT = 20.0


def find_lockers(paths: list[str]) -> dict[str, list[str]]:
    """Path → friendly names of the processes using it (a folder: anything inside it).

    Only paths that are in use appear in the result. Never raises: when detection itself fails
    the answer is "nothing known to be in use" (and a debug log line).
    """
    if not paths:
        return {}
    try:
        if sys.platform == "win32":
            return _windows(paths)
        if sys.platform == "darwin":
            return _darwin(paths)
        return _linux(paths)
    except Exception as exc:  # detection is advisory: never fail an operation because of it
        logger.debug("lock detection failed: {}", exc)
        return {}


# --------------------------------------------------------------------------- Windows


def _walk_files(path: str, budget: int) -> list[str]:
    """``path`` itself, or for a folder the files inside it (not following links), at most
    ``budget``. Restart Manager takes files only: a folder among them fails the whole query."""
    if not os.path.isdir(path):
        return [path]
    if os.path.islink(path) or _is_junction(path):
        return []
    names: list[str] = []
    stack = [path]
    while stack and len(names) < budget:
        folder = stack.pop()
        try:
            with os.scandir(folder) as it:
                for entry in it:
                    if entry.is_dir(follow_symlinks=False):
                        if not entry.is_junction():
                            stack.append(entry.path)
                        continue
                    names.append(entry.path)
                    if len(names) >= budget:
                        break
        except OSError:
            continue
    return names


def _is_junction(path: str) -> bool:
    try:
        return os.path.isjunction(path)
    except OSError:
        return False


def _windows(paths: list[str]) -> dict[str, list[str]]:
    from . import _win

    budget = MAX_FILES
    names: dict[str, list[str]] = {}
    for p in paths:
        if budget <= 0:
            break
        got = _walk_files(p, min(PER_FOLDER, budget)) if os.path.lexists(p) else []
        if got:
            names[p] = got
            budget -= len(got)
    if not names:
        return {}

    def held(group: Sequence[str]) -> list[str] | None:
        """Who holds any file of ``group`` open; None when Restart Manager refuses the query."""
        files = [f for p in group for f in names[p]]
        try:
            return _unique(name for _, name in _win.rm_lockers(files))
        except OSError as exc:
            if len(group) == 1:
                logger.debug("lock detection skipped {}: {}", group[0], exc)
            return None

    out: dict[str, list[str]] = {}
    pending: list[list[str]] = [list(names)]
    while pending:
        group = pending.pop()
        who = held(group)
        if who is not None and not who:
            continue
        if len(group) == 1:
            if who:
                out[group[0]] = who
            continue
        half = len(group) // 2  # someone holds something here (or one name spoils the query)
        pending.extend((group[:half], group[half:]))
    return {p: out[p] for p in paths if p in out}


# --------------------------------------------------------------------------- Linux


def _resolved(path: str) -> str:
    """Parent folders resolved, the last component kept (a link is itself, not its target)."""
    parent, name = os.path.split(os.path.abspath(path))
    return os.path.join(os.path.realpath(parent), name)


def _process_name(pid: str, proc: str = "/proc") -> str:
    try:
        exe = os.readlink(f"{proc}/{pid}/exe")
        if exe.endswith(" (deleted)"):
            exe = exe[: -len(" (deleted)")]
        if exe:
            return os.path.basename(exe)
    except OSError:
        pass
    try:
        with open(f"{proc}/{pid}/comm", encoding="utf-8", errors="replace") as fh:
            return fh.read().strip() or f"PID {pid}"
    except OSError:
        return f"PID {pid}"


def _linux(paths: list[str], proc: str = "/proc") -> dict[str, list[str]]:
    wanted: dict[str, list[str]] = {}  # resolved path -> the paths asked for
    for p in paths:
        wanted.setdefault(_resolved(p), []).append(p)
    found: dict[str, list[str]] = {}
    try:
        pids = sorted((d for d in os.listdir(proc) if d.isdigit()), key=int)
    except OSError:
        return {}
    for pid in pids:
        fd_dir = f"{proc}/{pid}/fd"
        try:
            fds = os.listdir(fd_dir)
        except OSError:  # gone, or not ours to inspect
            continue
        name: str | None = None
        for fd in fds:
            try:
                target = os.readlink(f"{fd_dir}/{fd}")
            except OSError:
                continue
            if not target.startswith("/"):  # socket:[…], pipe:[…], anon_inode:…
                continue
            if target.endswith(" (deleted)"):
                continue
            hit = target
            while True:
                for asked in wanted.get(hit, ()):
                    if name is None:
                        name = _process_name(pid, proc)
                    names = found.setdefault(asked, [])
                    if name not in names:
                        names.append(name)
                up = os.path.dirname(hit)
                if up == hit:
                    break
                hit = up
    return {p: found[p] for p in paths if p in found}


# --------------------------------------------------------------------------- macOS (untested)


def _darwin(paths: list[str]) -> dict[str, list[str]]:
    lsof = shutil.which("lsof") or ("/usr/sbin/lsof" if os.path.exists("/usr/sbin/lsof") else None)
    if lsof is None:
        return {}
    args = [lsof, "-w", "-Fpcn", "+c", "0"]
    for p in paths:
        args += ["+D", p] if os.path.isdir(p) and not os.path.islink(p) else ["--", p]
    try:
        run = subprocess.run(args, capture_output=True, text=True, timeout=LSOF_TIMEOUT, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return {}
    wanted = {_resolved(p): p for p in paths}
    found: dict[str, list[str]] = {}
    command = ""
    for line in run.stdout.splitlines():
        tag, value = line[:1], line[1:]
        if tag == "p":
            command = ""
        elif tag == "c":
            command = value
        elif tag == "n" and value.startswith("/"):
            hit = value
            while True:
                asked = wanted.get(hit)
                if asked is not None and command and command not in found.setdefault(asked, []):
                    found[asked].append(command)
                up = os.path.dirname(hit)
                if up == hit:
                    break
                hit = up
    return {p: found[p] for p in paths if found.get(p)}


def _unique(names: Iterable[str]) -> list[str]:
    out: list[str] = []
    for n in names:
        if n not in out:
            out.append(n)
    return out


__all__ = ["MAX_FILES", "find_lockers"]
