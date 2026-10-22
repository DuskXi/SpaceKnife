"""The plugin process's audit hook: no network, writes, programs or native code without permission.

This is **not** a security sandbox (Python's own documentation says so for audit hooks): it stops
mistakes and casual misuse. The real boundary is the user confirming the plugin with its permissions,
the hash pinning that makes any change ask again, and the process with its limits.
"""

from __future__ import annotations

import fnmatch
import os
import sys
import sysconfig
import threading
from collections.abc import Iterable
from typing import Any

WRITE_FLAGS = os.O_WRONLY | os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_TRUNC
_FS_EVENTS = frozenset(
    {
        "os.remove",
        "os.rename",
        "os.replace",
        "os.rmdir",
        "os.mkdir",
        "os.chmod",
        "os.chown",
        "os.utime",
        "os.symlink",
        "os.link",
        "os.truncate",
        "shutil.rmtree",
        "shutil.move",
        "shutil.copyfile",
        "shutil.copytree",
    }
)
_EXEC_EVENTS = frozenset(
    {
        "subprocess.Popen",
        "os.system",
        "os.exec",
        "os.spawn",
        "os.posix_spawn",
        "os.startfile",
        "os.fork",
        "os.forkpty",
    }
)
_NATIVE = ("ctypes", "_ctypes", "cffi", "_cffi_backend")


class Guard:
    """The rules for one plugin process."""

    def __init__(
        self,
        *,
        plugin_dir: str,
        storage: str,
        granted: Iterable[str],
        roots: Iterable[str] = (),
        net_hosts: Iterable[str] = (),
        programs: Iterable[str] = (),
        extra_read: Iterable[str] = (),
        extra_write: Iterable[str] = (),
    ) -> None:
        self.granted = set(granted)
        self.roots = [self._norm(r) for r in roots]
        self.net_hosts = [h.lower() for h in net_hosts]
        self.programs = set(programs)
        self.write_ok = [self._norm(p) for p in (storage, *self._temp(), *extra_write)]
        self.read_ok = [
            self._norm(p) for p in (plugin_dir, storage, *self._python(), *self._temp(), *extra_read)
        ]
        self.blocked: list[str] = []
        self.enabled = True
        self.host_threads: set[int] = set()  # the channel's own threads (Windows pipes use ctypes)
        if "net" in self.granted:
            self.read_ok += [self._norm(p) for p in _net_files()]
        self.read_ok += [self._norm(p) for p in _system_data()]

    def allow_thread(self) -> None:
        """The calling thread is the host channel's: its native calls are allowed."""
        self.host_threads.add(threading.get_ident())

    @staticmethod
    def _norm(path: str) -> str:
        return os.path.normcase(os.path.realpath(path))

    @staticmethod
    def _temp() -> list[str]:
        import tempfile

        return [tempfile.gettempdir()]

    @staticmethod
    def _python() -> list[str]:
        out = {sys.prefix, sys.base_prefix, sys.exec_prefix, os.path.dirname(sys.executable)}
        for key in ("stdlib", "platstdlib", "purelib", "platlib"):
            with _suppress():
                out.add(sysconfig.get_path(key))
        out.update(p for p in sys.path if p and os.path.isdir(p))
        return [p for p in out if p]

    def set_roots(self, roots: Iterable[str]) -> None:
        self.roots = [self._norm(r) for r in roots]

    @staticmethod
    def _inside(path: str, bases: list[str]) -> bool:
        return any(path == b or path.startswith(b.rstrip("\\/") + os.sep) for b in bases)

    def _deny(self, what: str) -> None:
        self.blocked.append(what)
        raise PermissionError(f"blocked by the plugin sandbox: {what}")

    def check_open(self, path: Any, mode: Any, flags: Any) -> None:
        if path is None or isinstance(path, int):
            return
        try:
            p = self._norm(os.fsdecode(path))
        except (TypeError, ValueError):
            return
        writing = False
        if isinstance(mode, str):
            writing = any(c in mode for c in "wax+")
        elif isinstance(flags, int):
            writing = bool(flags & WRITE_FLAGS)
        if writing:
            if not self._inside(p, self.write_ok):
                self._deny(f"write {p}")
            return
        if self._inside(p, self.read_ok):
            return
        if "files.read" in self.granted and self._inside(p, self.roots):
            return
        if p.startswith(("/dev/shm", "/proc/self")) or os.path.basename(p) in ("/dev/null", "nul"):
            return
        self._deny(f"read {p}")

    def __call__(self, event: str, args: tuple[Any, ...]) -> None:
        if not self.enabled:
            return
        if event == "open":
            self.check_open(
                args[0] if args else None,
                args[1] if len(args) > 1 else None,
                args[2] if len(args) > 2 else None,
            )
        elif event in _FS_EVENTS:
            for a in args[:2]:
                if isinstance(a, str | bytes | os.PathLike):
                    p = self._norm(os.fsdecode(a))
                    if not self._inside(p, self.write_ok):
                        self._deny(f"{event} {p}")
        elif event.startswith("socket."):
            self._socket(event, args)
        elif event in _EXEC_EVENTS or event.startswith(("os.exec", "os.spawn")):
            self._exec(event, args)
        elif event == "import" and args and str(args[0]).split(".")[0] in _NATIVE:
            if threading.get_ident() not in self.host_threads:
                self._deny(f"import {args[0]}")
        elif event.startswith("ctypes.") and threading.get_ident() not in self.host_threads:
            self._deny(event)

    def _socket(self, event: str, args: tuple[Any, ...]) -> None:
        if event in ("socket.bind", "socket.listen") and "net" not in self.granted:
            self._deny(event)
        if event in ("socket.connect", "socket.sendto", "socket.sendmsg"):
            if "net" not in self.granted:
                addr = args[1] if len(args) > 1 else None
                if isinstance(addr, str | bytes):  # AF_UNIX: the host's own channel is opened before
                    self._deny(f"{event} {addr!r}")
                self._deny(event)
        elif event == "socket.getaddrinfo":
            host = str(args[0]) if args and args[0] is not None else ""
            if "net" not in self.granted:
                self._deny(f"resolve {host}")
            if (
                host
                and "*" not in self.net_hosts
                and not any(fnmatch.fnmatch(host.lower(), h) for h in self.net_hosts)
            ):
                self._deny(f"resolve {host} (not in net_hosts)")

    def _exec(self, event: str, args: tuple[Any, ...]) -> None:
        if "exec" not in self.granted:
            self._deny(event)
        prog = args[0] if args else None
        if isinstance(prog, list | tuple):
            prog = prog[0] if prog else None
        name = os.path.basename(os.fsdecode(prog)) if isinstance(prog, str | bytes | os.PathLike) else ""
        stem = name[:-4] if name.lower().endswith(".exe") else name
        if name not in self.programs and stem not in self.programs:
            self._deny(f"{event} {name}")


def _net_files() -> list[str]:
    """What resolving names and checking certificates read."""
    out = ["/etc/hosts", "/etc/resolv.conf", "/etc/nsswitch.conf", "/etc/gai.conf", "/etc/host.conf"]
    with _suppress():
        import ssl

        paths = ssl.get_default_verify_paths()
        out += [p for p in (paths.cafile, paths.capath, paths.openssl_cafile, paths.openssl_capath) if p]
    out += ["/etc/ssl", "/etc/pki", "/usr/lib/ssl", "/usr/local/etc/openssl"]
    return out


def _system_data() -> list[str]:
    """Data the standard library reads by itself (time zones)."""
    out = ["/etc/localtime", "/usr/share/zoneinfo", "/usr/lib/zoneinfo"]
    with _suppress():
        import zoneinfo

        out += list(zoneinfo.TZPATH)
    return out


class _suppress:
    def __enter__(self) -> None:
        return None

    def __exit__(self, *exc: object) -> bool:
        return True


_installed: Guard | None = None


def install(guard: Guard) -> Guard:
    """Install the hook once for this process (audit hooks cannot be removed)."""
    global _installed
    if _installed is None:
        sys.addaudithook(guard)
        _installed = guard
    return _installed


def installed() -> Guard | None:
    return _installed


__all__ = ["Guard", "install", "installed"]
