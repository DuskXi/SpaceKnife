"""The plugin author docs (``docs/plugin-dev.md``) for ``spaceknife plugin docs``.

The file lives in the repository's ``docs/``; the frozen app carries a copy as
``spaceknife_sdk/docs/plugin-dev.md`` (``packaging/spaceknife-backend.spec``). Sections start with an
anchor line ``<a id="kind-report"></a>`` before their heading; ``plugin check`` names them.
"""

from __future__ import annotations

import re
from importlib import resources
from pathlib import Path

NAME = "plugin-dev.md"
ANCHOR = re.compile(r'^<a id="([a-z0-9_.-]+)"></a>\s*$')
HEADING = re.compile(r"^(#{1,6})\s+(.*)$")


def path() -> Path | None:
    """Where the docs are: the packaged copy, else the repository's docs/ above this package."""
    try:
        packaged = resources.files("spaceknife_sdk").joinpath("docs", NAME)
        if packaged.is_file():
            return Path(str(packaged))
    except (ModuleNotFoundError, OSError):
        pass
    here = Path(__file__).resolve()
    for folder in here.parents:
        candidate = folder / "docs" / NAME
        if candidate.is_file():
            return candidate
    return None


def text() -> str | None:
    p = path()
    return p.read_text(encoding="utf-8") if p is not None else None


def anchors(doc: str) -> list[tuple[str, str]]:
    """(anchor, heading) of every section, in order."""
    out: list[tuple[str, str]] = []
    lines = doc.splitlines()
    for i, line in enumerate(lines):
        m = ANCHOR.match(line)
        if m is None:
            continue
        title = next((h.group(2) for ln in lines[i + 1 : i + 4] if (h := HEADING.match(ln))), "")
        out.append((m.group(1), title))
    return out


def section(doc: str, anchor: str) -> str | None:
    """The section that starts at ``anchor``, up to the next heading of the same or a higher level."""
    lines = doc.splitlines(keepends=True)
    start = None
    for i, ln in enumerate(lines):
        m = ANCHOR.match(ln.strip())
        if m is not None and m.group(1) == anchor:
            start = i
            break
    if start is None:
        return None
    level = 7
    body: list[str] = []
    for i in range(start, len(lines)):
        ln = lines[i]
        h = HEADING.match(ln)
        if h is not None:
            if level == 7:
                level = len(h.group(1))
            elif len(h.group(1)) <= level:
                while body and ANCHOR.match(body[-1].strip()):
                    body.pop()
                break
        body.append(ln)
    return "".join(body).rstrip() + "\n"


__all__ = ["anchors", "path", "section", "text"]
