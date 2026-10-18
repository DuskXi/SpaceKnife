"""One-time token and peer checks.

The backend writes a 32-byte random token (hex) to a temp file only the current user can read
(Windows: protected DACL, see ``_win32.owner_only_sddl``; POSIX: 0600, O_EXCL) and passes the
*path* on the helper's command line. The helper reads and deletes it and proves itself with the
token in its first frame; the backend also checks, from the OS, that the peer is the process it
launched. The helper in turn checks that the pipe/socket server is the backend pid it was given.
"""

from __future__ import annotations

import contextlib
import hmac
import os
import re
import secrets
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

if sys.platform == "win32":
    from . import _win32

TOKEN_BYTES = 32
TOKEN_RE = re.compile(r"[0-9a-f]{64}")
TOKEN_FILE_RE = re.compile(r"spaceknife-helper-[0-9a-f]{16}\.token")
_FILE_ATTRIBUTE_REPARSE_POINT = 0x400


def new_token() -> str:
    return secrets.token_hex(TOKEN_BYTES)


def write_token_file(token: str, directory: str | None = None) -> Path:
    """Create a fresh token file readable only by this user; returns its path."""
    path = Path(directory or tempfile.gettempdir()) / f"spaceknife-helper-{secrets.token_hex(8)}.token"
    data = token.encode("ascii")
    if sys.platform == "win32":
        _win32.write_private_file(str(path), data)
    else:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            os.write(fd, data)
        finally:
            os.close(fd)
    return path


def read_token_file(path: str) -> str:
    """Helper side: read the token and delete the file. Refuses anything but a token file."""
    p = Path(path)
    if not p.is_absolute() or TOKEN_FILE_RE.fullmatch(p.name) is None:
        raise ValueError("令牌文件路径无效")
    st = os.lstat(p)
    if not stat.S_ISREG(st.st_mode) or getattr(st, "st_file_attributes", 0) & _FILE_ATTRIBUTE_REPARSE_POINT:
        raise ValueError("令牌文件不是普通文件")
    try:
        fd = os.open(p, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
        try:
            data = os.read(fd, 256)
        finally:
            os.close(fd)
    finally:
        with contextlib.suppress(OSError):  # the backend removes it as well once connected
            os.remove(p)
    token = data.decode("ascii", "replace").strip()
    if TOKEN_RE.fullmatch(token) is None:
        raise ValueError("令牌格式无效")
    return token


def tokens_match(a: str, b: str) -> bool:
    return hmac.compare_digest(a.encode("ascii", "replace"), b.encode("ascii", "replace"))


def is_elevated() -> bool:
    """Whether this process has administrator rights (Windows: BUILTIN\\Administrators enabled)."""
    if sys.platform == "win32":
        return _win32.is_admin()
    return os.geteuid() == 0


def parent_pid(pid: int) -> int | None:
    if sys.platform == "win32":
        return _win32.parent_pid(pid)
    if sys.platform.startswith("linux"):
        try:
            stat_line = Path(f"/proc/{pid}/stat").read_text()
        except OSError:
            return None
        return int(stat_line.rsplit(")", 1)[1].split()[1])  # field 4, after "(comm)"
    try:
        out = subprocess.run(
            ["/bin/ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True, check=False
        )
        return int(out.stdout.strip())
    except (OSError, ValueError):
        return None


def peer_is_launched(peer_pid: int, launched_pid: int) -> bool:
    """Whether the connected peer is the process we started.

    On Windows a virtual environment's ``Scripts\\python.exe`` is a launcher that runs the real
    interpreter as its child, so the child of the launched process counts as well. Nowhere else.
    """
    if peer_pid == launched_pid:
        return True
    return sys.platform == "win32" and parent_pid(peer_pid) == launched_pid


__all__ = [
    "is_elevated",
    "new_token",
    "parent_pid",
    "peer_is_launched",
    "read_token_file",
    "tokens_match",
    "write_token_file",
]
