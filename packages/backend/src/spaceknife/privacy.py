"""macOS privacy protection (TCC) a disk scan runs into: Full Disk Access, and the folders that ask.

Without Full Disk Access macOS guards several places with a consent of their own (Desktop,
Documents, Downloads, iCloud Drive, other cloud providers, the Photos library, other apps' data),
so a scan of the home folder or of the disk raises one prompt after another, and a worker thread
waits on each. Full Disk Access grants them all at once and survives updates of a signed app.

:func:`full_disk_access` tells whether this process has it, by reading a file only Full Disk
Access may read (the user's TCC database: refused without a prompt). The check runs in the backend,
which macOS attributes to SpaceKnife (the responsible app), so it answers for the app.
:func:`protected_folders` lists the places to leave out when the user prefers not to grant it.
"""

from __future__ import annotations

import os
import sys

# System Settings › Privacy & Security › Full Disk Access (macOS 13+; older versions open the pane)
FULL_DISK_ACCESS_SETTINGS = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"

# home-relative places behind a consent prompt of their own (macOS 14 and 15)
PROMPTING = (
    "Desktop",
    "Documents",
    "Downloads",
    "Library/Mobile Documents",  # iCloud Drive
    "Library/CloudStorage",  # File Providers: OneDrive, Google Drive, Dropbox…
    "Pictures/Photos Library.photoslibrary",
    "Library/Containers",  # other apps' data (macOS 14+)
    "Library/Group Containers",
)

# files only Full Disk Access may read, the first that exists decides
_PROBES = (
    "Library/Application Support/com.apple.TCC/TCC.db",
    "Library/Safari/Bookmarks.plist",
    "Library/Mail",
)


def full_disk_access(home: str | None = None) -> bool | None:
    """macOS: does this process have Full Disk Access? None elsewhere, or when no probe exists."""
    if sys.platform != "darwin":
        return None
    base = home or os.path.expanduser("~")
    for rel in _PROBES:
        path = os.path.join(base, rel)
        try:
            if os.path.isdir(path):
                os.listdir(path)
            else:
                with open(path, "rb"):
                    pass
        except PermissionError:
            return False
        except OSError:
            continue  # missing: the next probe
        return True
    return None


def protected_folders(home: str | None = None) -> tuple[str, ...]:
    """macOS: this user's places that ask for consent one by one without Full Disk Access (the
    ones that exist; looking at a folder itself asks nothing, listing it does)."""
    if sys.platform != "darwin":
        return ()
    base = home or os.path.expanduser("~")
    return tuple(p for p in (os.path.join(base, rel) for rel in PROMPTING) if os.path.lexists(p))


def inside(path: str, root: str) -> bool:
    """``path`` lies strictly below ``root`` (POSIX spelling)."""
    base = root.rstrip("/") or "/"
    return path != base and path.startswith(base if base == "/" else base + "/")


__all__ = ["FULL_DISK_ACCESS_SETTINGS", "PROMPTING", "full_disk_access", "inside", "protected_folders"]
