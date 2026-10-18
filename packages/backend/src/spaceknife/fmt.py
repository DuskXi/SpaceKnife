"""Human-readable formatting shared by the CLI and (later) server-side labels, in the current
interface language (M7.5).

Units follow the platform convention (UI-SPEC G13): binary multiples on Windows and Linux,
decimal on macOS — always labelled KB/MB/GB/TB like the system file managers do.
"""

from __future__ import annotations

import sys
from datetime import UTC, datetime

from .i18n import _, tr

UNIT_BASE = 1000 if sys.platform == "darwin" else 1024
_UNITS = ("B", "KB", "MB", "GB", "TB", "PB")


def size(n: int | float, base: int = UNIT_BASE) -> str:
    value = float(abs(n))
    unit = 0
    while value >= base and unit < len(_UNITS) - 1:
        value /= base
        unit += 1
    sign = "-" if n < 0 else ""
    if unit == 0:
        return f"{sign}{int(value)} B"
    digits = 0 if value >= 100 else 1
    return f"{sign}{value:.{digits}f} {_UNITS[unit]}"


def count(n: int) -> str:
    return f"{n:,}"


def duration(seconds: float) -> str:
    if seconds < 1:
        return tr("{n} 毫秒", n=f"{seconds * 1000:.0f}")
    if seconds < 60:
        return tr("{n} 秒", n=f"{seconds:.1f}")
    minutes, secs = divmod(int(seconds), 60)
    return tr("{minutes} 分 {seconds} 秒", minutes=minutes, seconds=secs)


def age(ts: int, now: datetime | None = None) -> str:
    """Relative age of a unix timestamp in the current language, e.g. '3 天前'."""
    if ts <= 0:
        return _("未知")
    now = now or datetime.now(UTC)
    delta = (now - datetime.fromtimestamp(ts, UTC)).total_seconds()
    if delta < 60:
        return _("刚刚")
    n = int(delta // 60)
    if delta < 3600:
        return tr("{n} 分钟前", n=n)
    n = int(delta // 3600)
    if delta < 86400:
        return tr("{n} 小时前", n=n)
    n = int(delta // 86400)
    if delta < 86400 * 30:
        return tr("{n} 天前", n=n)
    n = int(delta // (86400 * 30))
    if delta < 86400 * 365:
        return tr("{n} 个月前", n=n)
    return tr("{n} 年前", n=int(delta // (86400 * 365)))
