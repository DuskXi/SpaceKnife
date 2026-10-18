"""Optional native acceleration (packages/native, built with ``uv sync --group native``).

Every caller keeps a pure-Python path; this module only answers "is the fast path here?".
``SPACEKNIFE_NO_NATIVE=1`` forces the fallbacks (tests compare both paths).
"""

from __future__ import annotations

import os
from types import ModuleType

_module: ModuleType | None
if os.environ.get("SPACEKNIFE_NO_NATIVE") == "1":
    _module = None
else:
    try:
        import spaceknife_native as _module
    except ImportError:
        _module = None


def module() -> ModuleType | None:
    """The native module, or None when it is not installed / disabled."""
    return _module


def has(name: str) -> bool:
    """Whether the native module provides ``name`` (a function of docs/native.md)."""
    return _module is not None and hasattr(_module, name)


def version() -> str | None:
    return getattr(_module, "__version__", None) if _module is not None else None


__all__ = ["has", "module", "version"]
