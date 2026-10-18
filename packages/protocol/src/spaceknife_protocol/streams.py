"""Cross-cutting stream machinery: cancellation, subscriptions, progress and the log topic."""

from __future__ import annotations

from datetime import datetime
from typing import Any, Literal

from pydantic import Field

from .base import Params, Payload
from .envelope import RequestId

LogLevel = Literal["TRACE", "DEBUG", "INFO", "SUCCESS", "WARNING", "ERROR", "CRITICAL"]

TOPIC_LOG = "log"


class CancelParams(Params):
    """``$/cancel`` notification: cancel the in-flight request with this id."""

    id: RequestId


class SubscribeParams(Params):
    topic: str = Field(min_length=1, max_length=64)
    params: dict[str, Any] | None = None
    rate_hz: float | None = Field(default=None, gt=0, le=120, description="Upper bound on event rate")


class SubscribeResult(Payload):
    sub: str


class UnsubscribeParams(Params):
    sub: str = Field(min_length=1, max_length=64)


class ProgressEvent(Payload):
    """``$/progress``: LSP-style progress for long-running requests."""

    token: str
    value: dict[str, Any]


class LogTopicParams(Params):
    """Params for subscribing to the ``log`` topic."""

    min_level: LogLevel = "INFO"
    backlog: int = Field(default=200, ge=0, le=5000, description="Recent records replayed on subscribe")


class LogRecordEvent(Payload):
    """``log.record``: one backend log record, pushed to ``log`` subscribers (W8 log panel)."""

    sub: str
    seq: int
    time: datetime
    level: LogLevel
    source: str
    message: str
    extra: dict[str, str] = Field(default_factory=dict)
