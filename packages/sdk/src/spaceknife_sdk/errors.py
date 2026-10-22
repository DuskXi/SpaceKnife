"""Errors a plugin raises (and the host maps to protocol error codes)."""

from __future__ import annotations

from typing import Any

from spaceknife_protocol.text import Text, localize


class PluginError(Exception):
    """A message for the user (``T(...)`` or a Chinese string): shown as is, as ``PLUGIN_ERROR``."""

    def __init__(self, text: Text | str, *, data: Any = None) -> None:
        self.text = text
        self.data = data
        super().__init__(text.zh if isinstance(text, Text) else text)

    def message(self) -> str:
        """The text in the language of the current request."""
        out = localize(self.text)
        return out if isinstance(out, str) else str(self.text)


class Cancelled(Exception):
    """The call was cancelled or ran out of time: ``ctx.check()`` raises it. Do not swallow it."""


class HostError(Exception):
    """The host refused or failed a request of the plugin (``code`` is a protocol ErrorCode)."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data


class ManifestError(ValueError):
    """``plugin.toml`` or a declaration is invalid; ``issues`` holds every problem found."""

    def __init__(self, issues: list[Issue]) -> None:
        self.issues = issues
        super().__init__("; ".join(i.text() for i in issues) or "invalid plugin")


class Issue:
    """One problem of a plugin, as ``plugin check`` reports it."""

    __slots__ = ("code", "file", "fix", "line", "message", "severity")

    def __init__(
        self,
        code: str,
        message: str,
        *,
        file: str = "",
        line: int = 0,
        fix: str = "",
        severity: str = "error",
    ) -> None:
        self.code = code
        self.message = message
        self.file = file
        self.line = line
        self.fix = fix
        self.severity = severity

    def text(self) -> str:
        where = (
            f"{self.file}:{self.line}: " if self.file and self.line else f"{self.file}: " if self.file else ""
        )
        fix = f" ({self.fix})" if self.fix else ""
        return f"{where}{self.code}: {self.message}{fix}"

    def as_dict(self) -> dict[str, Any]:
        return {
            "code": self.code,
            "message": self.message,
            "file": self.file,
            "line": self.line,
            "fix": self.fix,
            "severity": self.severity,
        }

    def __repr__(self) -> str:
        return f"Issue({self.text()!r})"


__all__ = ["Cancelled", "HostError", "Issue", "ManifestError", "PluginError"]
