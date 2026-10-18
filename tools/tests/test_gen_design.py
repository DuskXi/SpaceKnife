from __future__ import annotations

import json
from pathlib import Path

import pytest

import gen_design as g


def write(tmp: Path, name: str, data: dict[str, object]) -> None:
    (tmp / name).write_text(json.dumps(data), encoding="utf-8")


def test_repository_artefacts_are_up_to_date() -> None:
    assert g.stale(g.artefacts()) == [], "run: uv run python tools/gen_design.py"


def test_alias_resolution_and_type_inheritance(tmp_path: Path) -> None:
    write(
        tmp_path,
        "a.tokens.json",
        {
            "color": {
                "$type": "color",
                "base": {"$value": "#112233"},
                "alias": {"$value": "{color.base}"},
                "chain": {"$value": "{color.alias}"},
            },
            "size": {"$type": "dimension", "x": {"$value": {"value": 4, "unit": "px"}}},
        },
    )
    tokens = {t.dotted: t for t in g.load_tokens(tmp_path)}
    assert tokens["color.chain"].value == "#112233"
    assert tokens["color.chain"].type == "color"
    assert tokens["size.x"].value == 4


@pytest.mark.parametrize(
    ("data", "message"),
    [
        ({"c": {"$type": "color", "a": {"$value": "{c.b}"}, "b": {"$value": "{c.a}"}}}, "cycle"),
        ({"c": {"$type": "color", "a": {"$value": "{c.nope}"}}}, "unknown alias"),
        ({"c": {"$type": "color", "a": {"$value": "red"}}}, "#RRGGBB"),
        ({"c": {"a": {"$value": 1}}}, r"no \$type"),
        ({"c": {"$type": "gradient", "a": {"$value": 1}}}, "unsupported"),
        ({"c": {"$type": "dimension", "a": {"$value": {"value": 1, "unit": "rem"}}}}, "unit"),
    ],
)
def test_invalid_tokens_are_rejected(tmp_path: Path, data: dict[str, object], message: str) -> None:
    write(tmp_path, "x.tokens.json", data)
    with pytest.raises(g.TokenError, match=message):
        g.load_tokens(tmp_path)


def test_duplicate_top_level_group_is_rejected(tmp_path: Path) -> None:
    write(tmp_path, "a.tokens.json", {"c": {"$type": "number", "a": {"$value": 1}}})
    write(tmp_path, "b.tokens.json", {"c": {"$type": "number", "b": {"$value": 2}}})
    with pytest.raises(g.TokenError, match="defined twice"):
        g.load_tokens(tmp_path)


def test_naming() -> None:
    path = ("color", "text", "onAccent")
    assert g.kebab(path) == "color-text-on-accent"
    assert g.camel(path) == "colorTextOnAccent"
    assert g.pascal(("treemap", "level", "0", "headerBg")) == "TreemapLevel0HeaderBg"
    assert g.argb("#112233") == "#FF112233"
    assert g.argb("#11223344") == "#44112233"


def test_generated_python_module_matches_tokens() -> None:
    from spaceknife_client.tokens import TOKENS

    assert TOKENS["color.bg.window"] == "#08090B"
    assert TOKENS["treemap.class.video"] == "#E07A1F"
    assert TOKENS["layout.toolbar"] == 48
    assert TOKENS["font.family.ui"][0] == "Geist"
