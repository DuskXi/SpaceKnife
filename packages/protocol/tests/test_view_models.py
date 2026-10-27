from __future__ import annotations

import pytest
from pydantic import ValidationError

from spaceknife_protocol import (
    ALL_CAPABILITIES,
    CAP_LAYOUT_FLAME,
    CAP_LAYOUT_POLAR,
    CAP_VISUALS,
    PROTOCOL_VERSION,
    protocol_major,
)
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import METHODS, VISUAL_LIST


def _info(**extra: object) -> pv.FrameInfo:
    return pv.FrameInfo.model_validate(
        {
            "view_id": "v1",
            "seq": 1,
            "width": 800,
            "height": 600,
            "dpr": 1.0,
            "rows": 0,
            "breadcrumb": [],
            "scanned": 0,
            "layout_ms": 0.1,
            **extra,
        }
    )


def test_visual_capabilities_and_minor_version() -> None:
    assert {CAP_VISUALS, CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR} <= set(ALL_CAPABILITIES)
    assert (CAP_VISUALS, CAP_LAYOUT_FLAME, CAP_LAYOUT_POLAR) == (
        "visuals.v1",
        "layout.flame.v1",
        "layout.polar.v1",
    )
    assert protocol_major(PROTOCOL_VERSION) == 1
    assert METHODS["visual.list"] is VISUAL_LIST


def test_frame_info_defaults_describe_a_treemap_frame() -> None:
    info = _info()
    assert (info.visual, info.layout, info.geometry, info.root) == ("treemap", pv.DRAWLIST_LAYOUT, None, None)


def test_geometry_is_a_tagged_union() -> None:
    rect = {"x": 0, "y": 560, "w": 800, "h": 38}
    flame = _info(
        visual="flame",
        layout=pv.FLAME_LAYOUT,
        geometry={"kind": "flame", "origin": "bottom", "row_h": 40, "rows": 5, "gap": 2, "root": rect},
        root={"key": 7, "node": 7, "kind": 1, "size": 123},
    )
    assert isinstance(flame.geometry, pv.FlameGeometry)
    assert flame.geometry.root == pv.PixelRect(**rect)
    assert flame.root is not None
    assert (flame.root.tone, flame.root.branch, flame.root.delta) == (4, 0, None)

    polar = _info(
        visual="flame_polar",
        layout=pv.POLAR_LAYOUT,
        geometry={
            "kind": "polar",
            "cx": 400,
            "cy": 300,
            "radius": 292,
            "hole": 70,
            "ring_w": 44,
            "rings": 5,
            "gap": 2,
        },
    )
    assert isinstance(polar.geometry, pv.PolarGeometry)
    assert polar.geometry.unit == pv.ANGLE_UNIT == 1 << 20
    # Every angle of a turn is exact in an f32 (24-bit mantissa).
    assert pv.ANGLE_UNIT < 1 << 24

    again = pv.FrameInfo.model_validate_json(polar.model_dump_json())
    assert again == polar
    with pytest.raises(ValidationError):
        _info(geometry={"kind": "spiral"})


@pytest.mark.parametrize("visual", [*pv.BUILTIN_VISUALS, "plugin.heat-map", "a1"])
def test_visual_ids_accepted(visual: str) -> None:
    assert pv.ViewSettingsParams(visual=visual).visual == visual


@pytest.mark.parametrize("visual", ["", "Flame", "1flame", "flame_", "flame__polar", "a" * 49, "x/y"])
def test_visual_ids_rejected(visual: str) -> None:
    with pytest.raises(ValidationError):
        pv.ViewSettingsParams(visual=visual)


def test_branch_colour_mode_and_visual_list() -> None:
    assert pv.ViewSettingsParams(color_mode="branch").color_mode == "branch"
    result = pv.VisualListResult.model_validate(
        {
            "visuals": [
                {
                    "id": "flame_polar",
                    "layout": pv.POLAR_LAYOUT,
                    "name": "Polar flame graph",
                    "icon": "viz-flame-polar",
                    "order": 2,
                    "compare": True,
                    "free_space": True,
                    "default_color": "branch",
                    "available": False,
                    "reason": "This window can't show this visualization",
                }
            ]
        }
    )
    assert result.visuals[0].default_color == "branch"
    assert not result.visuals[0].available
