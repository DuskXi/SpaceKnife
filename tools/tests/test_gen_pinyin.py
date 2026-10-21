from __future__ import annotations

import gen_pinyin


def test_the_pinyin_table_is_up_to_date() -> None:
    assert gen_pinyin.main(["--check"]) == 0, "run: uv run python tools/gen_pinyin.py"


def test_every_command_name_is_in_the_table() -> None:
    import json

    table = json.loads(gen_pinyin.TABLE.read_text(encoding="utf-8"))
    names = gen_pinyin.names()
    assert sorted(table) == names
    assert len(names) > 40
    assert table["切换配色"] == ["qie", "huan", "pei", "se"]
