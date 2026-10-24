"""User-facing events for the log panel (W8, UI-W8-22…25).

An event is an ordinary loguru record bound with ``event`` (its source: scan, perm, monitor,
delete, snapshot, export, system) and, when it concerns one, the ``scan_id`` and absolute ``path``
it can be located at. The full log keeps going to the log file; frontends show the records that
carry ``event`` plus anything at warning level or above. Messages are the final, localised text.
"""

from __future__ import annotations

import contextlib
import sys
from typing import Literal

from loguru import logger

from .i18n import _

Source = Literal["scan", "perm", "monitor", "delete", "snapshot", "export", "system"]
_DENIED_ZH = "拒绝"  # in a Chinese system's "access denied" message  # i18n: ignore
_STOPS = "。. "  # full stops stripped from an OS message  # i18n: ignore


def emit(
    source: Source,
    message: str,
    *,
    level: Literal["INFO", "WARNING", "ERROR"] = "INFO",
    scan_id: str | None = None,
    path: str | None = None,
) -> None:
    # no format arguments: loguru leaves braces in paths alone
    logger.bind(event=source, scan_id=scan_id or "", path=printable(path or "")).log(
        level, printable(message)
    )


def printable(text: str) -> str:
    """``text`` with undecodable file-name parts escaped (``\\xff`` on POSIX, ``\\udc80`` on Windows).

    Names keep lone surrogates (surrogateescape / surrogatepass) so they round-trip to the OS
    exactly, but such text cannot be encoded as UTF-8: logs and the wire get this form instead.
    """
    if text.isascii():
        return text
    try:
        text.encode("utf-8")
    except UnicodeEncodeError:
        pass
    else:
        return text
    if sys.platform != "win32":  # the original bytes, as the scanner read them
        with contextlib.suppress(UnicodeEncodeError):
            return text.encode("utf-8", "surrogateescape").decode("utf-8", "backslashreplace")
    return text.encode("utf-8", "backslashreplace").decode("utf-8")


def denied_reason(message: str) -> str:
    """OS error text -> the wording used in the log panel."""
    from .scan import worker  # the walkers' own listing errors (both use the same texts)

    own = {
        worker.CASE_ERROR: _("同一文件夹里另有一项与它只差大小写，按名字打开的是那一项"),
        worker.DUP_ERROR: _("同一文件夹里另有一项在 Windows 上显示为同一个名字，按名字分不开"),
        worker.BADNAME_ERROR: _("名字里有 Windows 的名字里不能有的字符，按名字打不开"),
        worker.CAP_ERROR: _("只能按文件 ID 打开的文件夹一次要读的项太多，这次没有读"),
        worker.INCOMPLETE_ERROR: _("按文件 ID 打开时没有列全"),
    }.get(message)
    if own is not None:
        return own
    low = message.lower()
    if "denied" in low or _DENIED_ZH in message or "permission" in low or "eacces" in low or "eperm" in low:
        return _("拒绝访问")
    return message.rstrip(_STOPS) or _("无法读取")


__all__ = ["Source", "denied_reason", "emit", "printable"]
