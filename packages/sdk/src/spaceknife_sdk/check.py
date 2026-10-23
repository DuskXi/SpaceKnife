"""``spaceknife plugin check`` from Python (docs/plugin-dev.md 「检查」).

>>> from spaceknife_sdk.check import check
>>> problems = check("path/to/plugin", strict=True)     # [] when everything is fine
>>> for p in problems: print(p.text())

The checks live with the host (they use its rule compiler, template parser, filter parser, icon
cleaner and a :class:`~spaceknife_sdk.testing.MockHost` for the contracts), so this needs the
SpaceKnife backend installed, as :mod:`spaceknife_sdk.testing` does.
"""

from __future__ import annotations

import os
from typing import Any

from .errors import Issue


def check(
    folder: str | os.PathLike[str],
    *,
    strict: bool = False,
    contracts: bool = True,
    process: bool = True,
) -> list[Issue]:
    """The problems of the plugin in ``folder``: errors (with ``strict``, warnings too). ``contracts``
    runs its code on synthetic trees (in a plugin process unless ``process=False``)."""
    from pathlib import Path

    from spaceknife.plugins.check import check_plugin

    return check_plugin(Path(folder), contracts=contracts, process=process).errors(strict)


def report(folder: str | os.PathLike[str], *, strict: bool = False, **kw: Any) -> dict[str, Any]:
    """Everything ``plugin check --json`` prints: ``{plugin, ok, issues: [{code, message, file,
    line, fix, severity, doc}]}``."""
    from pathlib import Path

    from spaceknife.plugins.check import check_plugin

    return check_plugin(Path(folder), **kw).as_dict(strict)


__all__ = ["check", "report"]
