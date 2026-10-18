"""SpaceKnife native acceleration (optional; the backend falls back to pure Python without it).

The functions are documented in docs/native.md and typed in ``_native.pyi``.
"""

from typing import Any

from . import _native
from ._native import __version__


def __getattr__(name: str) -> Any:
    return getattr(_native, name)


__all__ = ["__version__"]
