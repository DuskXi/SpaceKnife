"""Human-readable formatting shared by the CLI and (later) server-side labels.

Units follow the platform convention (UI-SPEC G13): binary multiples on Windows and Linux,
decimal on macOS — always labelled KB/MB/GB/TB like the system file managers do.
"""

from __future__ import annotations

import sys
from datetime import UTC, datetime

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
        return f"{seconds * 1000:.0f} 毫秒"
    if seconds < 60:
        return f"{seconds:.1f} 秒"
    minutes, secs = divmod(int(seconds), 60)
    return f"{minutes} 分 {secs} 秒"


def age(ts: int, now: datetime | None = None) -> str:
    """Relative age of a unix timestamp in Chinese, e.g. '3 天前'."""
    if ts <= 0:
        return "未知"
    now = now or datetime.now(UTC)
    delta = (now - datetime.fromtimestamp(ts, UTC)).total_seconds()
    if delta < 60:
        return "刚刚"
    for limit, div, unit in (
        (3600, 60, "分钟"),
        (86400, 3600, "小时"),
        (86400 * 30, 86400, "天"),
        (86400 * 365, 86400 * 30, "个月"),
    ):
        if delta < limit:
            return f"{int(delta // div)} {unit}前"
    return f"{int(delta // (86400 * 365))} 年前"
