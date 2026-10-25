"""zoneinfo is on the plugins' allow-list: its time zones are there wherever plugins run and are
tested, Windows included (no system database there: the SDK depends on tzdata on Windows)."""

from __future__ import annotations

from datetime import UTC, datetime
from zoneinfo import ZoneInfo, available_timezones


def test_time_zones_are_there() -> None:
    assert {"Europe/London", "Asia/Shanghai", "America/New_York"} <= available_timezones()
    when = datetime(2026, 7, 1, 12, tzinfo=UTC)
    assert when.astimezone(ZoneInfo("Asia/Shanghai")).hour == 20
    assert when.astimezone(ZoneInfo("Europe/London")).hour == 13
