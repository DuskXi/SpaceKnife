"""Exceptions raised by :class:`spaceknife.helper.HelperClient`.

``HelperError`` — an operation failed; ``code`` is the helper's error code (``needs_admin``,
``not_ntfs``, ``io_error``, ``invalid_request``, ``usn_inactive`` (no change journal on the
volume), ``usn_gone`` (the checkpoint is no longer in the journal) …). The helper is still usable
unless it is also a :class:`HelperUnavailable`.

``HelperUnavailable`` — there is no working helper; ``code`` says why: ``exited``, ``timeout``,
``auth``, ``protocol`` (it sent garbage and was disconnected), ``closed``, ``not_supported``.
Callers fall back to the hard scan.

``HelperDenied`` — the user declined the UAC prompt.
"""

from __future__ import annotations

from ..i18n import _


class HelperError(Exception):
    def __init__(self, message: str, code: str = "error") -> None:
        super().__init__(message)
        self.message = message
        self.code = code


class HelperUnavailable(HelperError):
    def __init__(self, message: str, code: str = "unavailable") -> None:
        super().__init__(message, code)


class HelperDenied(HelperUnavailable):
    def __init__(self, message: str | None = None) -> None:
        super().__init__(_("用户拒绝了管理员授权") if message is None else message, "denied")


__all__ = ["HelperDenied", "HelperError", "HelperUnavailable"]
