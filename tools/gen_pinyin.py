"""Write the pinyin of every palette command name: packages/backend/src/spaceknife/palette/pinyin.json.

The palette finds a command by its name, the name's full pinyin and its pinyin initials
(``palette/commands.py``). Working the pinyin out when the backend starts meant importing pypinyin:
about 50 MB of dictionaries kept for good, for a hundred or so fixed names. It is worked out here
instead, for the names on every platform; the tests run ``--check``.

Usage::

    uv run python tools/gen_pinyin.py          # rewrite the table
    uv run python tools/gen_pinyin.py --check  # exit 1 if it is out of date
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from pypinyin import lazy_pinyin

import gen_docs  # the registries as built on each platform (tools/ is on the path)
from spaceknife.palette import commands

TABLE = Path(__file__).resolve().parents[1] / "packages/backend/src/spaceknife/palette/pinyin.json"


def names() -> list[str]:
    """Every command name on every platform."""
    return sorted({cmd.name for variants in gen_docs.command_variants() for cmd in variants})


def render() -> str:
    rows = [
        f" {json.dumps(name, ensure_ascii=False)}: "
        f"{json.dumps(lazy_pinyin(name, errors=commands.each_char), ensure_ascii=False)}"
        for name in names()
    ]
    return "{\n" + ",\n".join(rows) + "\n}\n"  # one name a line


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="exit 1 if the table is out of date")
    args = ap.parse_args(argv)
    text = render()
    if args.check:
        if not TABLE.exists() or TABLE.read_text(encoding="utf-8") != text:
            print(f"{TABLE.name} is out of date: run uv run python tools/gen_pinyin.py", file=sys.stderr)
            return 1
        return 0
    TABLE.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {TABLE.relative_to(Path.cwd()) if TABLE.is_relative_to(Path.cwd()) else TABLE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
