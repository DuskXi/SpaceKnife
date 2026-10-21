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
    assert TOKENS["motion.duration.swap"] == 200
    assert TOKENS["visual.flamePolar.holeRatio"] == 0.24


def _luminance(color: str) -> float:
    def channel(c: float) -> float:
        return c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4

    r, g_, b = (channel(int(color[i : i + 2], 16) / 255) for i in (1, 3, 5))
    return 0.2126 * r + 0.7152 * g_ + 0.0722 * b


def _contrast(a: str, b: str) -> float:
    hi, lo = sorted((_luminance(a), _luminance(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def test_partition_fills_have_readable_labels() -> None:
    tokens = {t.dotted: t.value for t in g.load_tokens()}
    branch = [tokens[f"visual.branch.{slot}.{tier}"] for slot in range(1, 9) for tier in range(1, 5)]
    assert len(set(branch)) == 32
    rings = [tokens[f"visual.ring.{tier}"] for tier in range(1, 5)]
    light, dark = tokens["treemap.label.light"], tokens["treemap.label.dark"]
    # Labels take white or near-black, whichever contrasts more (UI-TM-19).
    for fill in [*branch, tokens["visual.branch.neutral"], *rings]:
        assert max(_contrast(fill, light), _contrast(fill, dark)) >= 4.5, fill
    assert tokens["visual.center.bg"] == tokens["treemap.level.0.headerBg"]


def test_visualization_icons() -> None:
    _, icons = g.load_icons()
    assert {"vizTreemap", "vizFlame", "vizFlamePolar"} <= set(icons)
    assert (g.BUILD / "icons" / "viz-flame-polar.svg").exists()
