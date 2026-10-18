"""The parser vocabulary of this backend: file classes and what this platform does not record."""

from __future__ import annotations

import sys
from functools import cache

from spaceknife_protocol.filter import AgeField, AttrName, FilterVocab
from spaceknife_protocol.filter_syntax import default_vocab

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


@cache
def platform_vocab() -> FilterVocab:
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
