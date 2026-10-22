"""How plugin code reaches its host: the same interface in a plugin process and inside the host.

A :class:`Link` sends requests to the host and waits for the answer (plugin code calls it from its
own thread or event loop, never from the host's), sends notifications (events, state, logs,
progress) and maps tree images. The process runtime implements it over the wire
(:mod:`._runtime`), the in-process runtime with direct calls (``spaceknife.plugins.runtime``).
"""

from __future__ import annotations

import contextlib
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Any, Protocol

import numpy as np


@dataclass(slots=True)
class Image:
    """Whole columns of one scan version, read-only."""

    scan_id: str
    version: int
    epoch: int
    n: int
    columns: dict[str, np.ndarray]
    key: str = ""
    keep: list[Any] = field(default_factory=list)  # mappings to keep alive while the arrays are used

    def close(self) -> None:
        self.columns = {}
        for m in self.keep:
            close = getattr(m, "close", None)
            if close is not None:
                # an array still pointing into it: freed with the last reference
                with contextlib.suppress(BufferError, OSError, ValueError):
                    close()
        self.keep = []


class Link(Protocol):
    def request(
        self,
        method: str,
        params: dict[str, Any],
        *,
        frames: Sequence[bytes] = (),
        timeout: float | None = None,
    ) -> tuple[Any, list[bytes]]:
        """Ask the host; returns (JSON value, binary frames). Raises HostError when refused."""
        ...

    def notify(self, type: str, payload: dict[str, Any]) -> None:
        """Fire and forget: ``event``, ``state``, ``log``, ``progress``, ``toast``."""
        ...

    def image(self, scan_id: str, columns: Sequence[str]) -> Image:
        """Whole columns of a scan (exported by the host on first use)."""
        ...

    def store_of(self, scan_id: str) -> Any:
        """The host's NodeStore (in-process built-in plugins only), else None."""
        ...


__all__ = ["Image", "Link"]
