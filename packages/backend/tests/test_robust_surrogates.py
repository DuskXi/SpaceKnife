"""Undecodable file names (lone surrogates) must never break a scan, an event or an RPC reply."""

from __future__ import annotations

import asyncio
import json
import os
import sys
from pathlib import Path

import pytest

from spaceknife.events import emit, printable
from spaceknife_client.connection import Connection
from spaceknife_protocol import scan as p
from spaceknife_protocol.base import Empty
from spaceknife_protocol.envelope import SuccessResponse
from spaceknife_protocol.methods import EVENT_LOG, SCAN_LIST, SCAN_START, SCAN_STATUS, TREE_CHILDREN
from spaceknife_protocol.streams import TOPIC_LOG, LogRecordEvent, LogTopicParams

from .test_scan_server import isolated_home, wait_done

__all__ = ["isolated_home"]  # autouse fixture: never touch the real snapshot folder


def odd_name(root: Path) -> str:
    """Create a folder whose name is not valid Unicode; returns the name as Python sees it."""
    try:
        if sys.platform == "win32":
            name = "x\udc80y"  # unpaired UTF-16 surrogate, legal on NTFS
            os.mkdir(root / name)
        else:
            raw = b"x\xffy"  # not UTF-8: decoded with surrogateescape
            os.mkdir(os.fsencode(root) + b"/" + raw)
            name = os.fsdecode(raw)
    except (OSError, UnicodeError) as exc:  # e.g. APFS rejects non-UTF-8 names
        pytest.skip(f"file system refuses undecodable names: {exc}")
    (root / name / "inner.bin").write_bytes(b"z" * 300)
    (root / name / f"tail.{name}").write_bytes(b"z")  # an extension that is not valid Unicode either
    return name


def test_printable_escapes_only_undecodable_text() -> None:
    assert printable("C:\\普通\\path {x}") == "C:\\普通\\path {x}"
    assert printable("a\udc80b") == ("a\\udc80b" if sys.platform == "win32" else "a\\x80b")
    assert printable("a\ud800b") == "a\\ud800b"  # never produced by surrogateescape: escaped as is
    raw = b"q\xff".decode("utf-8", "surrogateescape")  # how POSIX names hold undecodable bytes
    assert printable(raw) == ("q\\udcff" if sys.platform == "win32" else "q\\xff")


async def test_scan_of_undecodable_names_over_the_protocol(tmp_path: Path, client: Connection) -> None:
    root = tmp_path / "root"
    root.mkdir()
    name = odd_name(root)
    (root / "plain.txt").write_bytes(b"x" * 100)
    logs: list[LogRecordEvent] = []
    client.on_event(EVENT_LOG, logs.append)
    await client.subscribe(TOPIC_LOG, LogTopicParams(min_level="INFO", backlog=0))

    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    status = await asyncio.wait_for(wait_done(client, started.scan_id), timeout=20)
    assert status.state == "done"
    assert (status.files, status.dirs) == (3, 2)
    assert status.snapshot is not None  # the auto snapshot holds the odd extension too
    again = await asyncio.wait_for(client.call(SCAN_STATUS, p.ScanIdParams(scan_id=started.scan_id)), 10)
    assert again.state == "done" and again.snapshot == status.snapshot
    listed = await asyncio.wait_for(client.call(SCAN_LIST, Empty()), 10)
    assert [s.scan_id for s in listed.scans] == [started.scan_id]

    # a reply carrying the name itself is still answered (the name is shown escaped)
    children = await asyncio.wait_for(
        client.call(TREE_CHILDREN, p.TreeChildrenParams(scan_id=started.scan_id, node=status.roots[0].node)),
        10,
    )
    assert sorted(c.name for c in children.items) == sorted(["plain.txt", printable(name)])

    emit("perm", f"无法访问 {root / name}", level="WARNING", scan_id=started.scan_id, path=str(root / name))
    for _ in range(100):
        if any("无法访问" in e.message for e in logs):
            break
        await asyncio.sleep(0.02)
    messages = [e.message for e in logs]
    assert any("开始扫描" in m for m in messages)
    assert any("硬扫描完成" in m for m in messages)
    denied = next(e for e in logs if "无法访问" in e.message)
    assert denied.message == f"无法访问 {printable(str(root / name))}"
    assert denied.extra["path"] == printable(str(root / name))


def test_snapshot_header_keeps_lone_surrogates(tmp_path: Path) -> None:
    from spaceknife.store import snapshot
    from spaceknife.store.nodes import NodeStore

    root = str(tmp_path / "r\udc81")
    store = NodeStore()
    store.add_root(root)
    store.ext_id("b\udc80n")
    path = tmp_path / "s.sks"
    snapshot.save(store, path, roots=[root], stats={"denied": ["\udc82"]})
    loaded, header = snapshot.load(path)
    assert header.ext_table == ["", "b\udc80n"] and loaded.ext_table == ["", "b\udc80n"]
    assert header.roots == [root] and header.stats["denied"] == ["\udc82"]
    assert loaded.name(1) == root


def test_session_frames_survive_lone_surrogates() -> None:
    from spaceknife.server.session import encode_message

    frame = encode_message(SuccessResponse(id=1, result={"name": "a\udc80b", "a\udc81": ["\udc82"]}))
    doc = json.loads(frame)
    assert doc["result"]["name"] == printable("a\udc80b")
    assert doc["result"][printable("a\udc81")] == [printable("\udc82")]
    assert (
        encode_message(SuccessResponse(id=2, result={"ok": "fine"}))
        == '{"jsonrpc":"2.0","id":2,"result":{"ok":"fine"}}'
    )
