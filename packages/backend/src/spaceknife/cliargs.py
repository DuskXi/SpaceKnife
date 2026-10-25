"""Arguments of ``spaceknife plugin …`` as they were typed, where Windows PowerShell 5.1 mangled them.

Windows PowerShell 5.1 writes an argument with whitespace outside its own quotes wrapped in double
quotes, and any other argument as it is; it never escapes the double quotes inside (PowerShell 7.3+
does). The C runtime that splits the command line for the program reads those inner quotes as
quoting: they are dropped, and an argument may split in two. ``--setting 'rules=[{"name": "a b"}]'``
arrives as ``rules=[{name: a`` and ``b}]``, ``--step 'eval:f("a b")'`` as ``eval:f(a b)``.

:func:`as_typed` reads such a command line again (``GetCommandLineW``) the way PowerShell 5.1 wrote
it and gives the arguments as they were typed. It changes nothing when the command line has no
inner quotes, when it escapes them (``\\"``: other shells, PowerShell 7, or 5.1 written with
``\\"``), or when its reading does not match what the program received.
"""

from __future__ import annotations

import json
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

WHITE = " \t"


def needs_quotes(arg: str) -> bool:
    """Whether Windows PowerShell 5.1 wraps ``arg`` in double quotes: it has whitespace outside the
    quoted parts of it (a quote after a backslash does not count)."""
    inside = after_backslash = False
    for c in arg:
        if c == '"' and not after_backslash:
            inside = not inside
        elif c.isspace() and not inside:
            return True
        after_backslash = c == "\\"
    return False


def _quotes(text: str) -> int:
    """The double quotes in ``text`` that no backslash escapes."""
    return sum(1 for k, c in enumerate(text) if c == '"' and (k == 0 or text[k - 1] != "\\"))


def ps51_split(cmdline: str) -> list[str]:
    """A command line split the way Windows PowerShell 5.1 joined it: an argument it wrapped (one
    with whitespace outside its own quotes, :func:`needs_quotes`) runs from a double quote to the
    first double quote before whitespace (or the end) that leaves an even number of quotes inside;
    any other argument runs to the first whitespace outside its quotes. The quotes inside are kept.
    PowerShell's own writing is ambiguous only for arguments nobody types (``a" "b c``)."""
    out: list[str] = []
    i, n = 0, len(cmdline)
    while i < n:
        while i < n and cmdline[i] in WHITE:
            i += 1
        if i >= n:
            break
        if cmdline[i] == '"':
            wrapped = None
            for j in range(i + 1, n):
                if cmdline[j] == '"' and (j + 1 == n or cmdline[j + 1] in WHITE):
                    inner = cmdline[i + 1 : j]
                    if _quotes(inner) % 2:
                        continue  # a quote of the argument's own before whitespace: not its end
                    wrapped = j if needs_quotes(inner) else None  # "x": an argument with its quotes
                    break
            if wrapped is not None:
                out.append(cmdline[i + 1 : wrapped])
                i = wrapped + 1
                continue
        j, inside = i, False
        while j < n and (inside or cmdline[j] not in WHITE):
            if cmdline[j] == '"' and (j == i or cmdline[j - 1] != "\\"):
                inside = not inside
            j += 1
        out.append(cmdline[i:j])
        i = j
    return out


def crt_split(cmdline: str) -> list[str]:
    """A command line split as the C runtime splits it for a program's ``argv`` (backslashes before
    a quote, ``""`` inside quotes)."""
    return [value for _start, value in crt_spans(cmdline)]


def crt_spans(cmdline: str) -> list[tuple[int, str]]:
    """``crt_split`` with where each argument starts in the line."""
    out: list[tuple[int, str]] = []
    i, n = 0, len(cmdline)
    while i < n:
        while i < n and cmdline[i] in WHITE:
            i += 1
        if i >= n:
            break
        start = i
        arg: list[str] = []
        quoted = False
        while i < n and (quoted or cmdline[i] not in WHITE):
            c = cmdline[i]
            if c == "\\":
                k = i
                while k < n and cmdline[k] == "\\":
                    k += 1
                slashes = k - i
                if k < n and cmdline[k] == '"':
                    arg.append("\\" * (slashes // 2))
                    if slashes % 2:
                        arg.append('"')
                        i = k + 1
                    else:
                        i = k  # the quote itself is read next
                    continue
                arg.append("\\" * slashes)
                i = k
                continue
            if c == '"':
                if quoted and i + 1 < n and cmdline[i + 1] == '"':
                    arg.append('"')  # "" inside quotes: a quote, still quoted
                    i += 2
                    continue
                quoted = not quoted
                i += 1
                continue
            arg.append(c)
            i += 1
        out.append((start, "".join(arg)))
    return out


def command_line() -> str | None:
    """This process's command line as the system has it (Windows); None elsewhere."""
    if sys.platform != "win32":
        return None
    try:
        import ctypes

        get = ctypes.windll.kernel32.GetCommandLineW
        get.restype = ctypes.c_wchar_p
        return str(get() or "")
    except (AttributeError, OSError):
        return None


def as_typed(argv: list[str], cmdline: str | None = None) -> list[str]:
    """``argv`` with the arguments Windows PowerShell 5.1 mangled read as they were typed (see
    the module's docstring); ``argv`` itself when there is nothing to read again."""
    line = command_line() if cmdline is None else cmdline
    if not line or '"' not in line or '\\"' in line:
        return argv  # no quotes, or quotes escaped: what arrived is what was meant
    tail = len(argv) - 1
    spans = crt_spans(line)
    if tail <= 0 or len(spans) < tail:
        return argv
    head = len(spans) - tail  # the program, a launcher's script, -m module: not PowerShell's
    if [value for _start, value in spans[head:]] != argv[1:]:
        return argv  # this line, read our way, is not what the program got: leave it
    typed = ps51_split(line[spans[head][0] :])
    return argv if typed == argv[1:] else [argv[0], *typed]


def json_value(name: str, raw: str, warn: Callable[[str], None] | None = None) -> Any:
    """A ``name=value`` option's value: ``@file.json`` reads a file, JSON when it parses (``3``,
    ``true``, ``["a", "b"]``, ``{"k": 1}``), else the text itself; ``warn`` hears when a value
    looks like JSON and is not."""
    if raw.startswith("@") and len(raw) > 1:
        path = Path(raw[1:]).expanduser()
        try:
            text = path.read_text(encoding="utf-8-sig")
        except OSError as exc:
            raise ValueError(f"{name}: cannot read {path}: {exc.strerror or exc}") from None
        try:
            return json.loads(text)
        except ValueError as exc:
            raise ValueError(f"{name}: {path} is not JSON ({exc})") from None
    try:
        return json.loads(raw)
    except ValueError as exc:
        if raw.lstrip()[:1] in ("[", "{") and warn is not None:
            warn(f"{name}: not JSON ({exc}), taken as text; a file is safest: {name}=@file.json")
    return raw


__all__ = ["as_typed", "command_line", "crt_spans", "crt_split", "json_value", "needs_quotes", "ps51_split"]
