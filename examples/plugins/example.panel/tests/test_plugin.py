"""example.panel: the panel's method, and its mock data still matches what the method returns."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from pathlib import Path

import pytest

from spaceknife_sdk.testing import MockHost, make_tree

FOLDER = Path(__file__).resolve().parents[1]


@pytest.fixture
async def host() -> AsyncIterator[MockHost]:
    async with MockHost.load(__file__) as h:
        h.scan(
            make_tree(
                {
                    "old.txt": (1000, "2019-03-01"),
                    "older.txt": (500, "2019-12-31"),
                    "sub": {"new.bin": (7000, "2022-06-01")},
                }
            )
        )
        yield h


async def test_histogram(host: MockHost) -> None:
    got = await host.call("histogram")
    years = {y["year"]: y for y in got["years"]}
    assert sorted(years) == [2019, 2020, 2021, 2022]  # the years between are there, empty
    assert years[2019]["files"] == 2 and years[2020]["files"] == 0
    assert years[2022]["bytes"] >= 7000
    sub = await host.call("histogram", under=host.find("sub"))
    assert [y["year"] for y in sub["years"]] == [2022]


def test_mock_data_matches_the_method() -> None:
    import importlib.util
    import sys

    from spaceknife_sdk.manifest import load_manifest
    from spaceknife_sdk.plugin import loaded

    spec = importlib.util.spec_from_file_location("example_panel_plugin", FOLDER / "plugin.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        with loaded(FOLDER, load_manifest(FOLDER)):
            spec.loader.exec_module(module)
    finally:
        sys.modules.pop(spec.name, None)
    fixtures = json.loads((FOLDER / "web" / "fixtures.json").read_text(encoding="utf-8"))
    module.Histogram.model_validate(fixtures["calls"]["histogram"])  # the panel's mock data is real data


async def test_plugin_check(host: MockHost) -> None:
    await host.check(strict=True)
