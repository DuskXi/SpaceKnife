"""The parser vocabulary of this backend: file classes and what this platform does not record."""

from __future__ import annotations

import sys
from functools import cache

from spaceknife_protocol.filter import AgeField, AttrName, FilterVocab
from spaceknife_protocol.filter_syntax import default_vocab

from .. import classes as class_table

# Attributes the scanner does not collect on POSIX (hidden = dot files is collected everywhere).
_POSIX_MISSING_ATTRS: tuple[AttrName, ...] = (
    "archive",
    "system",
    "readonly",
    "compressed",
    "encrypted",
    "offline",
    "temporary",
    "notindexed",
    "sparse",
    "ads",
)


_current: tuple[int, FilterVocab] | None = None


def platform_vocab() -> FilterVocab:
    """The vocabulary with the active class table; the same object until the table changes (the
    parser caches its class index per vocabulary object)."""
    global _current
    v = class_table.version()
    if _current is None or _current[0] != v:
        _current = (v, _platform().model_copy(update={"classes": list(class_table.active().classes)}))
    return _current[1]


@cache
def _platform() -> FilterVocab:
    attrs: tuple[AttrName, ...]
    ages: tuple[AgeField, ...] = ()
    if sys.platform == "win32":
        attrs = ("ads",)  # alternate data streams are not collected yet
    else:
        attrs = _POSIX_MISSING_ATTRS
        if sys.platform.startswith("linux"):
            ages = ("create",)  # no birth time from stat() on Linux
    return default_vocab().model_copy(
        update={"unsupported_attrs": list(attrs), "unsupported_ages": list(ages)}
    )
