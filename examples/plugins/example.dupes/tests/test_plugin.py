"""example.dupes with the mock host: the groups, the report, the cleanup, the field and the set."""

from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import PurePath

import pytest

from spaceknife_sdk.testing import MockHost, make_tree

SAME = b"x" * 5000
TREE = make_tree(
    {
        "a.bin": SAME,
        "b.bin": SAME,
        "c.bin": b"y" * 5000,  # same size, other contents
        "sub": {"d.bin": SAME, "e.bin": b"z" * 300},
        "small.txt": b"x",
    }
)


@pytest.fixture
async def host() -> AsyncIterator[MockHost]:
    async with MockHost.load(__file__) as h:
        h.scan(TREE)
        await h.set_setting("min_size", 1000)
        yield h


async def test_find_groups_same_contents(host: MockHost) -> None:
    found = await host.call("find")
    assert [len(g["nodes"]) for g in found["groups"]] == [3]
    assert found["wasted"] == 2 * 5000
    assert host.events["progress"][-1] == {"done": 4, "total": 4}
    assert host.state("status")["groups"] == 1


async def test_report_and_cleanup(host: MockHost) -> None:
    text = host.text(await host.report("dupes"))
    assert "a.bin" in text and "[把多余的副本加入删除篮]" in text
    proposed = await host.cleanup("extra")
    assert sorted(PurePath(p.path).name for p in proposed) == ["b.bin", "d.bin"]
    assert all(p.reason.startswith("与 ") for p in proposed)


async def test_keep_only_this_one(host: MockHost) -> None:
    b = host.find("b.bin")
    # greyed in the menu until a search found copies, and always on folders and unique files
    assert await host.command_available("keep_this", node=b) == "先查找一次重复文件"
    await host.call("find")
    assert await host.command_available("keep_this", node=b) is None
    assert await host.command_available("keep_this", node=host.find("c.bin")) == "没有内容和它相同的文件"
    assert await host.command_available("keep_this", node=host.find("sub")) == "只用于文件"
    # run on b.bin as its context menu does: the cleanup gets keep = "node", b.bin stays
    proposed = await host.command("keep_this", node=b)
    assert sorted(PurePath(p.path).name for p in proposed) == ["a.bin", "d.bin"]
    assert all("b.bin" in p.reason for p in proposed)


async def test_field_and_node_set(host: MockHost) -> None:
    # they only read the last search (a field has 1 s): nothing before one ran
    assert await host.fields(node=host.find("sub/d.bin")) == []
    assert await host.filter(":set:dupes") == []
    await host.call("find")
    await host.settled()
    fields = await host.fields(node=host.find("sub/d.bin"))
    assert [f.value for f in fields] == [2]
    assert await host.fields(node=host.find("c.bin")) == []
    assert await host.filter(":set:dupes") == ["a.bin", "b.bin", "sub/d.bin"]


async def test_the_background_search(host: MockHost) -> None:
    # the command starts the task and says so; the task's progress and end are the host's to show
    toast = await host.command("search")
    assert "后台" in toast["text"]
    task = await host.task("search", key=host.scan_id or "")  # the command's (one per scan): until done
    assert task.state == "done" and task.done == task.total == 4
    assert host.state("status")["groups"] == 1
    await host.settled()
    assert await host.filter(":set:dupes") == ["a.bin", "b.bin", "sub/d.bin"]


async def test_in_a_plugin_process() -> None:
    async with MockHost.load(__file__, process=True) as h:  # as the app runs it
        h.scan(TREE)
        await h.set_setting("min_size", 1000)
        assert len((await h.call("find"))["groups"]) == 1
        assert (await h.task("search")).state == "done"
        await h.check(strict=True)
