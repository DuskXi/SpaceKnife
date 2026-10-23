"""File primitives of the cleanup executor (docs/dupes.md §16.9.3), one interface per system.

- :class:`PosixFs` (Linux, macOS): folder file descriptors (``O_PATH`` on Linux), every name opened
  and checked relative to one, never following the last component; a hard link is made from the
  keeper's descriptor (``linkat`` of ``/proc/self/fd/N``); names move with ``renameat2``
  (``RENAME_EXCHANGE`` swaps two names, ``RENAME_NOREPLACE`` never replaces one; macOS
  ``renameatx_np`` with ``RENAME_SWAP`` / ``RENAME_EXCL``, not verified); in-place block sharing is
  ``FIDEDUPERANGE``, which the kernel does only for byte-identical ranges.
- :class:`WinFs` (``winfs.py``): handles, renames and links by handle, never replacing.

**Sandbox** (I13): with ``allowed_roots`` (tests, the measuring tool, verification runs) every path a
primitive touches — the file, its folder, a temporary name, the recovery area — must lie below one
of them, else :class:`FsError` ``sandbox`` before any system call.

**Faults** (tests only): ``SPACEKNIFE_CLEANUP_FAULT=<phase>@<seq>:<kind>,...`` makes the primitive of
that phase fail for step ``seq`` (``*`` any step) with ``eio``, ``enospc``, ``eacces``, ``eperm``,
``exdev``, ``exists``, ``wrong_ident`` or ``exit`` (the process ends at once, as in a crash). It is
read only while ``SPACEKNIFE_CLEANUP_SANDBOX`` is set too.
"""

from __future__ import annotations

import contextlib
import ctypes
import errno
import os
import stat
import struct
import sys
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Any


class FsError(OSError):
    """A primitive failed: ``code`` is one of exists, missing, locked, denied, exdev, link_limit,
    enospc, eio, no_privilege, unsupported, readonly_fs, fs_limit, sandbox, wrong_ident, error."""

    def __init__(self, code: str, message: str = "") -> None:
        super().__init__(message or code)
        self.code = code


_ERRNO = {
    errno.EEXIST: "exists",
    errno.ENOTEMPTY: "exists",
    errno.ENOENT: "missing",
    errno.ENOTDIR: "missing",
    errno.EACCES: "denied",
    errno.EPERM: "denied",
    errno.EXDEV: "exdev",
    errno.EMLINK: "link_limit",
    errno.ENOSPC: "enospc",
    errno.EDQUOT: "enospc",
    errno.EIO: "eio",
    errno.EOPNOTSUPP: "unsupported",
    errno.EINVAL: "unsupported",
    errno.ENOSYS: "unsupported",
    errno.EBUSY: "locked",
    errno.ETXTBSY: "locked",
    errno.EROFS: "readonly_fs",
    errno.ELOOP: "missing",
}


def fs_error(exc: BaseException) -> FsError:
    if isinstance(exc, FsError):
        return exc
    code = getattr(exc, "code", None)
    if isinstance(code, str):  # winfs.WinFsError
        return FsError(code, str(exc))
    if isinstance(exc, OSError) and exc.errno in _ERRNO:
        return FsError(_ERRNO[exc.errno], str(exc))
    return FsError("error", str(exc))


@dataclass(frozen=True, slots=True)
class St:
    dev: int
    fid_lo: int
    fid_hi: int
    size: int
    mtime: int
    change: int
    nlink: int
    kind: str  # file | dir | link | other
    attrs: int = 0
    usn: int = 0
    uid: int = -1

    @property
    def ident(self) -> tuple[int, int, int]:
        return (self.dev, self.fid_lo, self.fid_hi)

    def stamps(self) -> tuple[int, int, int]:
        return (self.size, self.mtime, self.change)


# ------------------------------------------------------------------ sandbox and faults
def _norm(p: str) -> str:
    return os.path.normcase(os.path.abspath(p)).rstrip("\\/")


@dataclass(slots=True)
class Sandbox:
    roots: list[str]

    def allows(self, path: str) -> bool:
        if not path:
            return False
        keys = {_norm(path)}
        with contextlib.suppress(OSError, ValueError):
            parent, name = os.path.split(os.path.abspath(path))
            keys.add(_norm(os.path.join(os.path.realpath(parent), name)))
        for k in keys:
            if not any(k == r or k.startswith(r + os.sep) for r in (_norm(x) for x in self.roots)):
                return False
        return True

    def check(self, *paths: str) -> None:
        for p in paths:
            if not self.allows(p):
                raise FsError("sandbox", p)


def sandbox_from_env() -> Sandbox | None:
    raw = os.environ.get("SPACEKNIFE_CLEANUP_SANDBOX", "")
    roots = [r for r in raw.split(os.pathsep) if r.strip()]
    return Sandbox(roots) if roots else None


@dataclass(slots=True)
class Faults:
    rules: list[tuple[str, str, str]] = field(default_factory=list)  # (phase, seq or "*", kind)
    fired: list[tuple[str, int, str]] = field(default_factory=list)

    @staticmethod
    def parse(text: str) -> Faults:
        f = Faults()
        for part in text.split(","):
            part = part.strip()
            if not part or ":" not in part:
                continue
            where, kind = part.rsplit(":", 1)
            phase, _, seq = where.partition("@")
            f.rules.append((phase, seq or "*", kind))
        return f

    def hit(self, phase: str, seq: int) -> str | None:
        for i, (ph, sq, kind) in enumerate(self.rules):
            if ph == phase and (sq == "*" or sq == str(seq)):
                if sq != "*":
                    self.rules.pop(i)  # a one-off fault
                self.fired.append((phase, seq, kind))
                return kind
        return None


def faults_from_env() -> Faults | None:
    if not os.environ.get("SPACEKNIFE_CLEANUP_SANDBOX"):
        return None  # never outside a sandbox
    text = os.environ.get("SPACEKNIFE_CLEANUP_FAULT", "")
    return Faults.parse(text) if text else None


_FAULT_ERRNO = {
    "eio": errno.EIO,
    "enospc": errno.ENOSPC,
    "eacces": errno.EACCES,
    "eperm": errno.EPERM,
    "exdev": errno.EXDEV,
    "exists": errno.EEXIST,
}


class FsOps:
    """The common part: the sandbox, the faults, and the step number the faults look at."""

    kind = "posix"
    swaps = True  # names are exchanged atomically (POSIX); Windows does two no-replace renames

    def __init__(self, sandbox: Sandbox | None = None, faults: Faults | None = None) -> None:
        self.sandbox = sandbox
        self.faults = faults
        self.seq = 0
        self._wrong = False

    def guard(self, *paths: str) -> None:
        if self.sandbox is not None:
            self.sandbox.check(*paths)

    def fault(self, phase: str) -> None:
        if self.faults is None:
            return
        kind = self.faults.hit(phase, self.seq)
        if kind is None:
            return
        if kind == "exit":
            os._exit(137)  # a crash between two steps (tests)
        if kind == "wrong_ident":
            self._wrong = True
            return
        raise fs_error(OSError(_FAULT_ERRNO.get(kind, errno.EIO), f"injected {kind} at {phase}"))

    def _maybe_wrong(self, st: St) -> St:
        if self._wrong:
            self._wrong = False
            return St(st.dev, st.fid_lo ^ 0x5A5A, st.fid_hi, st.size, st.mtime, st.change, st.nlink, st.kind)
        return st

    # every method below is per system
    def open_dir(self, path: str) -> int:
        raise NotImplementedError

    def close(self, h: int) -> None:
        raise NotImplementedError

    def fstat(self, h: int) -> St:
        raise NotImplementedError

    def stat_at(self, dh: int, name: str, *, follow: bool = False) -> St | None:
        raise NotImplementedError

    def open_entry(self, dh: int, name: str, *, purpose: str) -> int:
        raise NotImplementedError

    def make_hardlink(self, kh: int, kdh: int, kname: str, dh: int, tname: str) -> None:
        raise NotImplementedError

    def make_symlink(self, dh: int, dpath: str, tname: str, text: str) -> None:
        raise NotImplementedError

    def read_link(self, dh: int, name: str) -> str | None:
        raise NotImplementedError

    def exchange(self, dh: int, a: str, b: str) -> None:
        raise FsError("unsupported", "names cannot be swapped here")

    def rename_at(self, sdh: int, sname: str, ddh: int, dname: str) -> None:
        raise NotImplementedError

    def rename_h(self, h: int, ddh: int, dname: str) -> None:
        raise FsError("unsupported", "renames by handle are Windows'")

    def remove_own(self, dh: int, name: str, ident: tuple[int, int, int] | None) -> bool:
        raise NotImplementedError

    def mkdir_at(self, dh: int, name: str, mode: int = 0o700) -> None:
        raise NotImplementedError

    def read_at(self, h: int, n: int, off: int) -> bytes:
        raise NotImplementedError

    def dedupe(self, kh: int, ch: int, off: int, n: int) -> tuple[int, int]:
        raise FsError("unsupported", "in-place sharing is Linux'")

    def clone_range(self, ch: int, kh: int, off: int, n: int) -> None:
        raise FsError("unsupported", "block cloning by handle is Windows'")

    def fsync_dir(self, path: str) -> None:
        return None


# ------------------------------------------------------------------ POSIX
def _kind(mode: int) -> str:
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "link"
    return "other"


def _st(s: os.stat_result) -> St:
    ino = int(s.st_ino)
    return St(
        dev=int(s.st_dev) & ((1 << 64) - 1),
        fid_lo=ino & ((1 << 64) - 1),
        fid_hi=ino >> 64,
        size=int(s.st_size),
        mtime=int(s.st_mtime_ns),
        change=int(s.st_ctime_ns),
        nlink=int(s.st_nlink),
        kind=_kind(s.st_mode),
        attrs=int(getattr(s, "st_flags", 0)),
        uid=int(getattr(s, "st_uid", -1)),
    )


_O_PATH = getattr(os, "O_PATH", 0)
_O_NOATIME = getattr(os, "O_NOATIME", 0)
_O_CLOEXEC = getattr(os, "O_CLOEXEC", 0)
_O_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
_O_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)
RENAME_NOREPLACE = 1
RENAME_EXCHANGE = 2
FIDEDUPERANGE = 0xC0189436
FILE_DEDUPE_RANGE_SAME = 0
FILE_DEDUPE_RANGE_DIFFERS = 1

_libc: list[Any] = []


def _c() -> Any:
    if not _libc:
        _libc.append(ctypes.CDLL(None, use_errno=True))
    return _libc[0]


class PosixFs(FsOps):
    kind = "posix"

    def __init__(self, sandbox: Sandbox | None = None, faults: Faults | None = None) -> None:
        super().__init__(sandbox, faults)
        self._paths: dict[int, str] = {}  # descriptor -> path (sandbox checks, messages)
        self.has_proc = os.path.isdir("/proc/self/fd")

    def _path(self, dh: int, name: str = "") -> str:
        base = self._paths.get(dh, "")
        return os.path.join(base, name) if name else base

    def open_dir(self, path: str) -> int:
        self.guard(path)
        self.fault("open_dir")
        try:
            flags = (_O_PATH or os.O_RDONLY) | _O_DIRECTORY | _O_NOFOLLOW | _O_CLOEXEC
            fd = os.open(path, flags)
        except OSError as exc:
            raise fs_error(exc) from exc
        self._paths[fd] = path
        return fd

    def close(self, h: int) -> None:
        self._paths.pop(h, None)
        with contextlib.suppress(OSError):
            os.close(h)

    def fstat(self, h: int) -> St:
        try:
            return self._maybe_wrong(_st(os.fstat(h)))
        except OSError as exc:
            raise fs_error(exc) from exc

    def stat_at(self, dh: int, name: str, *, follow: bool = False) -> St | None:
        self.guard(self._path(dh, name))
        try:
            return self._maybe_wrong(_st(os.stat(name, dir_fd=dh, follow_symlinks=follow)))
        except FileNotFoundError:
            return None
        except OSError as exc:
            if exc.errno in (errno.ENOENT, errno.ENOTDIR, errno.ELOOP) and follow:
                return None
            raise fs_error(exc) from exc

    def open_entry(self, dh: int, name: str, *, purpose: str) -> int:
        """A descriptor of ``name`` in ``dh`` (not followed). ``purpose``: copy, keeper, keeper_link,
        meta (a path descriptor), data (read), data_rw (in-place sharing)."""
        self.guard(self._path(dh, name))
        self.fault(f"open_{purpose}")
        if purpose in ("data", "data_rw"):
            flags = (os.O_RDWR if purpose == "data_rw" else os.O_RDONLY) | _O_NOFOLLOW | _O_CLOEXEC
            try:
                fd = os.open(name, flags | _O_NOATIME, dir_fd=dh)
            except PermissionError:
                fd = -1
            except OSError as exc:
                raise fs_error(exc) from exc
            if fd < 0:
                try:
                    fd = os.open(name, flags, dir_fd=dh)  # O_NOATIME needs the owner: read anyway
                except OSError as exc:
                    raise fs_error(exc) from exc
        else:
            flags = (_O_PATH or os.O_RDONLY) | _O_NOFOLLOW | _O_CLOEXEC
            try:
                fd = os.open(name, flags, dir_fd=dh)
            except OSError as exc:
                raise fs_error(exc) from exc
        self._paths[fd] = self._path(dh, name)
        return fd

    def make_hardlink(self, kh: int, kdh: int, kname: str, dh: int, tname: str) -> None:
        self.guard(self._path(dh, tname), self._path(kdh, kname))
        self.fault("make")
        try:
            if self.has_proc and sys.platform.startswith("linux"):
                os.link(f"/proc/self/fd/{kh}", tname, dst_dir_fd=dh, follow_symlinks=True)
            else:
                os.link(kname, tname, src_dir_fd=kdh, dst_dir_fd=dh, follow_symlinks=False)
        except OSError as exc:
            raise fs_error(exc) from exc

    def make_symlink(self, dh: int, dpath: str, tname: str, text: str) -> None:
        self.guard(self._path(dh, tname))
        self.fault("make")
        try:
            os.symlink(text, tname, dir_fd=dh)
        except OSError as exc:
            raise fs_error(exc) from exc

    def read_link(self, dh: int, name: str) -> str | None:
        self.guard(self._path(dh, name))
        try:
            return os.readlink(name, dir_fd=dh)
        except OSError:
            return None

    def _renameat2(self, sdh: int, sname: str, ddh: int, dname: str, flags: int) -> None:
        if sys.platform == "darwin":  # pragma: no cover - macOS, not verified
            fn = _c().renameatx_np
            fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
            mac = {RENAME_EXCHANGE: 0x2, RENAME_NOREPLACE: 0x4}[flags]  # RENAME_SWAP, RENAME_EXCL
            rc = fn(sdh, os.fsencode(sname), ddh, os.fsencode(dname), mac)
        else:
            fn = _c().renameat2
            fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
            rc = fn(sdh, os.fsencode(sname), ddh, os.fsencode(dname), flags)
        if rc != 0:
            err = ctypes.get_errno()
            raise fs_error(OSError(err, os.strerror(err), dname))

    def exchange(self, dh: int, a: str, b: str) -> None:
        self.guard(self._path(dh, a), self._path(dh, b))
        self.fault("exchange")
        self._renameat2(dh, a, dh, b, RENAME_EXCHANGE)

    def rename_at(self, sdh: int, sname: str, ddh: int, dname: str) -> None:
        self.guard(self._path(sdh, sname), self._path(ddh, dname))
        self.fault("rename")
        self._renameat2(sdh, sname, ddh, dname, RENAME_NOREPLACE)

    def remove_own(self, dh: int, name: str, ident: tuple[int, int, int] | None) -> bool:
        """Remove a name this run made, after checking it is still ours (``ident``: what it must be,
        None: any symbolic link we made, checked by the caller)."""
        self.guard(self._path(dh, name))
        self.fault("remove")
        st = self.stat_at(dh, name)
        if st is None:
            return True
        if ident is not None and st.ident != ident:
            return False
        try:
            os.unlink(name, dir_fd=dh)
        except FileNotFoundError:
            return True
        except OSError as exc:
            raise fs_error(exc) from exc
        return True

    def mkdir_at(self, dh: int, name: str, mode: int = 0o700) -> None:
        self.guard(self._path(dh, name))
        try:
            os.mkdir(name, mode, dir_fd=dh)
        except FileExistsError:
            pass
        except OSError as exc:
            raise fs_error(exc) from exc

    def read_at(self, h: int, n: int, off: int) -> bytes:
        pread: Any = getattr(os, "pread")  # noqa: B009 (no os.pread in the Windows stubs)
        try:
            data: bytes = pread(h, n, off)
            return data
        except OSError as exc:
            raise fs_error(exc) from exc

    def dedupe(self, kh: int, ch: int, off: int, n: int) -> tuple[int, int]:
        """FIDEDUPERANGE of ``n`` bytes at ``off`` of the keeper onto the copy: (bytes shared, status)."""
        self.fault("dedupe")
        import fcntl

        ioctl: Any = getattr(fcntl, "ioctl")  # noqa: B009 (no fcntl.ioctl in the Windows stubs)
        # struct file_dedupe_range (24 bytes) + one file_dedupe_range_info (32 bytes)
        buf = bytearray(struct.pack("<QQHHI", off, n, 1, 0, 0) + struct.pack("<qQQiI", ch, off, 0, 0, 0))
        try:
            ioctl(kh, FIDEDUPERANGE, buf, True)
        except OSError as exc:
            raise fs_error(exc) from exc
        deduped = struct.unpack_from("<Q", buf, 40)[0]
        status = struct.unpack_from("<i", buf, 48)[0]
        if status < 0:
            raise fs_error(OSError(-status, os.strerror(-status)))
        return int(deduped), int(status)

    def fsync_dir(self, path: str) -> None:
        with contextlib.suppress(OSError):
            fd = os.open(path, os.O_RDONLY | _O_DIRECTORY | _O_CLOEXEC)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)


# ------------------------------------------------------------------ Windows
class WinFs(FsOps):  # pragma: no cover - Windows
    kind = "windows"
    swaps = False

    def __init__(self, sandbox: Sandbox | None = None, faults: Faults | None = None) -> None:
        super().__init__(sandbox, faults)
        from . import winfs

        self.w = winfs
        self._paths: dict[int, str] = {}

    def _path(self, dh: int, name: str = "") -> str:
        base = self._paths.get(dh, "")
        return base.rstrip("\\") + "\\" + name if name else base

    def _st(self, h: int) -> St:
        s = self.w.stat_handle(h)
        kind = "dir" if s.directory else ("link" if s.attrs & self.w.ATTR_REPARSE else "file")
        return self._maybe_wrong(
            St(s.dev, s.fid_lo, s.fid_hi, s.size, s.mtime, s.change, s.nlink, kind, s.attrs, self.w.usn_of(h))
        )

    def open_dir(self, path: str) -> int:
        self.guard(path)
        self.fault("open_dir")
        try:
            h = self.w.open_dir(path)
        except OSError as exc:
            raise fs_error(exc) from exc
        self._paths[h] = path
        return h

    def close(self, h: int) -> None:
        self._paths.pop(h, None)
        self.w.close(h)

    def fstat(self, h: int) -> St:
        try:
            return self._st(h)
        except OSError as exc:
            raise fs_error(exc) from exc

    def stat_at(self, dh: int, name: str, *, follow: bool = False) -> St | None:
        self.guard(self._path(dh, name))
        w = self.w
        try:
            if follow:
                h = w.open_path(
                    self._path(dh, name), w.FILE_READ_ATTRIBUTES | w.SYNCHRONIZE, w.SHARE_ALL, follow=True
                )
            else:
                h = w.open_at(dh, name, w.FILE_READ_ATTRIBUTES, w.SHARE_ALL)
        except OSError as exc:
            e = fs_error(exc)
            if e.code == "missing":
                return None
            if e.code == "unsupported":  # a folder asked as a file
                try:
                    h = w.open_at(dh, name, w.FILE_READ_ATTRIBUTES, w.SHARE_ALL, directory=True)
                except OSError:
                    raise e from exc
            else:
                raise e from exc
        try:
            return self._st(h)
        finally:
            w.close(h)

    def open_entry(self, dh: int, name: str, *, purpose: str) -> int:
        w = self.w
        self.guard(self._path(dh, name))
        self.fault(f"open_{purpose}")
        read = w.FILE_READ_ATTRIBUTES
        access, share = {
            # the copy: deny others write, delete and rename while we work on it
            "copy": (w.DELETE | read, w.SHARE_READ),
            # the keeper of a hard link: others may not write (we open it again through the link)
            "keeper_link": (read | w.FILE_WRITE_ATTRIBUTES, w.SHARE_READ | w.SHARE_DELETE),
            "keeper": (read, w.SHARE_READ),
            "meta": (read, w.SHARE_ALL),
            "own": (w.DELETE | read, w.SHARE_READ | w.SHARE_DELETE),
            "data": (w.FILE_READ_DATA | read | w.FILE_WRITE_ATTRIBUTES, w.SHARE_READ),
            "data_rw": (w.FILE_READ_DATA | w.FILE_WRITE_DATA | read | w.FILE_WRITE_ATTRIBUTES, w.SHARE_READ),
        }[purpose]
        try:
            h: int = w.open_at(dh, name, access, share)
        except OSError as exc:
            raise fs_error(exc) from exc
        self._paths[h] = self._path(dh, name)
        return h

    def make_hardlink(self, kh: int, kdh: int, kname: str, dh: int, tname: str) -> None:
        self.guard(self._path(dh, tname))
        self.fault("make")
        try:
            self.w.link_handle(kh, dh, tname)
        except OSError as exc:
            raise fs_error(exc) from exc

    def make_symlink(self, dh: int, dpath: str, tname: str, text: str) -> None:
        self.guard(self._path(dh, tname))
        self.fault("make")
        from .facts import system_facts

        try:
            self.w.create_symlink(
                dpath.rstrip("\\") + "\\" + tname, text, unprivileged=system_facts().developer_mode
            )
        except OSError as exc:
            raise fs_error(exc) from exc

    def read_link(self, dh: int, name: str) -> str | None:
        w = self.w
        try:
            h = w.open_at(dh, name, w.FILE_READ_ATTRIBUTES, w.SHARE_ALL)
        except OSError:
            return None
        try:
            got = w.read_symlink(h)
            return got[0] if got else None
        finally:
            w.close(h)

    def rename_h(self, h: int, ddh: int, dname: str) -> None:
        self.guard(self._path(ddh, dname))
        self.fault("rename")
        try:
            self.w.rename_handle(h, ddh, dname)
        except OSError as exc:
            raise fs_error(exc) from exc

    def rename_at(self, sdh: int, sname: str, ddh: int, dname: str) -> None:
        h = self.open_entry(sdh, sname, purpose="own")
        try:
            self.rename_h(h, ddh, dname)
        finally:
            self.close(h)

    def remove_own(self, dh: int, name: str, ident: tuple[int, int, int] | None) -> bool:
        self.guard(self._path(dh, name))
        self.fault("remove")
        w = self.w
        try:
            h = w.open_at(dh, name, w.DELETE | w.FILE_READ_ATTRIBUTES, w.SHARE_READ | w.SHARE_DELETE)
        except OSError as exc:
            e = fs_error(exc)
            if e.code == "missing":
                return True
            raise e from exc
        try:
            st = self._st(h)
            if ident is not None and st.ident != ident:
                return False
            w.delete_handle(h)
            return True
        except OSError as exc:
            raise fs_error(exc) from exc
        finally:
            w.close(h)

    def mkdir_at(self, dh: int, name: str, mode: int = 0o700) -> None:
        self.guard(self._path(dh, name))
        try:
            os.mkdir(self._path(dh, name))
        except FileExistsError:
            pass
        except OSError as exc:
            raise fs_error(exc) from exc

    def read_at(self, h: int, n: int, off: int) -> bytes:
        try:
            return self.w.read_at(h, n, off)
        except OSError as exc:
            raise fs_error(exc) from exc

    def clone_range(self, ch: int, kh: int, off: int, n: int) -> None:
        self.fault("clone")
        try:
            self.w.duplicate_extents(ch, kh, off, off, n)
        except OSError as exc:
            raise fs_error(exc) from exc

    def fsync_dir(self, path: str) -> None:
        return None


def make_fs(sandbox: Sandbox | None = None, faults: Faults | None = None) -> FsOps:
    if sys.platform == "win32":
        return WinFs(sandbox, faults)
    return PosixFs(sandbox, faults)


def exchange_supported(fs: FsOps, folder_fd: int) -> bool:
    """Whether names swap atomically here (asked once per volume: a swap of two names that do not
    exist fails with ENOENT where the call is supported, EINVAL / ENOSYS where it is not)."""
    if not isinstance(fs, PosixFs):
        return False
    try:
        fs._renameat2(
            folder_fd, ".sk-probe-a-nonexistent", folder_fd, ".sk-probe-b-nonexistent", RENAME_EXCHANGE
        )
    except FsError as exc:
        return exc.code == "missing"
    return True


def common_parts(a: Sequence[str], b: Sequence[str]) -> int:
    n = 0
    for x, y in zip(a, b, strict=False):
        if x != y:
            break
        n += 1
    return n
