"""example.devpack with the mock host: every declaration does what it says, without any code."""

from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import PurePath

import pytest

from spaceknife.extensions import EXTENSIONS
from spaceknife_sdk.testing import MockHost, make_tree

TREE = make_tree(
    {
        "web": {".turbo": {"cache.bin": 4000}, "app.ts": 300, "package.json": 90},
        "rust": {"target": {"debug": {"app": 9000, "deps": {"x.rlib": 5000}}}, "main.rs": 200},
        "c": {"main.o": 700, "main.c": 100, "build.log": 50},
    }
)


@pytest.fixture
async def host() -> AsyncIterator[MockHost]:
    async with MockHost.load(__file__) as h:
        h.scan(TREE)
        yield h


async def test_rules_explain_build_caches(host: MockHost) -> None:
    cards = await host.insights(node=host.find("web/.turbo"))
    assert [c.rule for c in cards] == ["devpack-turbo"]
    cards = await host.insights(node=host.find("rust/target/debug"))
    assert cards[0].rule == "devpack-cargo-target" and cards[0].command == "cargo clean"


async def test_the_class_the_folder_rule_and_the_filter(host: MockHost) -> None:
    assert await host.filter(":class:example.devpack.build_output") == [
        "c/main.o",
        "rust/target/debug/app",  # below target/debug: the folder rule
        "rust/target/debug/deps/x.rlib",
    ]
    preset = EXTENSIONS.get("command", "example.devpack.junk")  # the filter preset's command
    assert preset is not None and preset.impl.action.expr == ":class:example.devpack.build_output"
    assert preset.impl.keys == () and [b.key for b in _keys()] == ["Ctrl+Alt+J"]


def _keys() -> list:
    return [c.spec for c in EXTENSIONS.owned("example.devpack") if c.kind == "keybinding"]


async def test_template_colours_and_visualization(host: MockHost) -> None:
    out = await host.export("by_folder")
    assert isinstance(out, str)
    assert [PurePath(line.split("\t")[1]).name for line in out.splitlines()] == ["rust", "web", "c"]
    frame = await host.color("dev")
    assert frame.pal is not None and set(frame.pal.tolist()) <= {0, 1, 2, 3}
    frame = await host.visual("files_flame")  # checked against the row contract
    assert len(frame.draw) > 0


async def test_plugin_check(host: MockHost) -> None:
    await host.check(strict=True)
