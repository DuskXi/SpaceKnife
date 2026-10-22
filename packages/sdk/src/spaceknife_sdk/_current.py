"""The context of the call being handled (report builders turn node ids into references with it)."""

from __future__ import annotations

from contextvars import ContextVar
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .context import Context

CURRENT: ContextVar[Context | None] = ContextVar("spaceknife_sdk_context", default=None)


def current_context() -> Context | None:
    return CURRENT.get()
