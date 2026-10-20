"""The partition layouts' geometry constants are written twice: in the design tokens (frontends) and
in layout/flame.py, layout/flame_polar.py (backend). They must agree."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from spaceknife.layout import flame, flame_polar

TOKENS = Path(__file__).resolve().parents[3] / "design" / "tokens" / "visual.tokens.json"


def value(group: dict[str, object], name: str) -> float:
    """A token's number: a dimension's value (CSS px) or a plain number."""
    token = group[name]
    assert isinstance(token, dict)
    v = token["$value"]
    if isinstance(v, dict):
        assert v["unit"] == "px"
        v = v["value"]
    assert isinstance(v, int | float)
    return v


@pytest.fixture(scope="module")
def visual() -> dict[str, dict[str, object]]:
    tokens: dict[str, dict[str, dict[str, object]]] = json.loads(TOKENS.read_text(encoding="utf-8"))
    return tokens["visual"]


@pytest.mark.parametrize(
    ("token", "constant"), [("rowMin", "ROW_MIN"), ("rowMax", "ROW_MAX"), ("gap", "GAP")]
)
def test_flame_graph_constants_match_the_tokens(
    visual: dict[str, dict[str, object]], token: str, constant: str
) -> None:
    assert value(visual["flame"], token) == getattr(flame, constant)


@pytest.mark.parametrize(
    ("token", "constant"),
    [
        ("margin", "MARGIN"),
        ("holeRatio", "HOLE_RATIO"),
        ("holeMin", "HOLE_MIN"),
        ("holeMaxRatio", "HOLE_MAX_RATIO"),
        ("ringMin", "RING_MIN"),
        ("gap", "GAP"),
    ],
)
def test_polar_flame_graph_constants_match_the_tokens(
    visual: dict[str, dict[str, object]], token: str, constant: str
) -> None:
    assert value(visual["flamePolar"], token) == getattr(flame_polar, constant)


def test_every_geometry_token_is_checked(visual: dict[str, dict[str, object]]) -> None:
    flame_names = {k for k in visual["flame"] if not k.startswith("$")}
    polar_names = {k for k in visual["flamePolar"] if not k.startswith("$")}
    assert flame_names == {"rowMin", "rowMax", "gap"}
    assert polar_names == {"margin", "holeRatio", "holeMin", "holeMaxRatio", "ringMin", "gap", "segments"}
    # segments: how finely frontends tessellate a sector; the backend never needs it
