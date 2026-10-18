"""Elevated read-only scan helper (docs/native.md 3).

The backend never runs elevated. For an MFT quick scan it starts this package as a separate
process (``python -m spaceknife.helper``), behind the UAC prompt on Windows, and talks to it
over a local named pipe / unix socket with one-time token authentication. The helper only
performs a whitelist of read-only operations and exits when the backend disconnects, on
shutdown, or after 10 idle minutes.

Backend API: :class:`HelperClient` (one connection) and :class:`SharedHelper` (one helper
per backend, reused across scans); failures raise :class:`HelperError` and its subclasses.
"""

from __future__ import annotations

from .auth import is_elevated
from .client import HelperClient, SharedHelper
from .errors import HelperDenied, HelperError, HelperUnavailable

__all__ = ["HelperClient", "HelperDenied", "HelperError", "HelperUnavailable", "SharedHelper", "is_elevated"]
