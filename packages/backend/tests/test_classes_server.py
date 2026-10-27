"""classes.* over the real WebSocket (W7 「文件类别」): edit, recolour, :class: filters, persistence."""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path

import pytest
from pydantic import ValidationError

from spaceknife import classes
from spaceknife.classes import DEFAULT_CLASSES, build
from spaceknife.server import Server
from spaceknife.view.state import F_MATCH
from spaceknife_client.connection import Connection, RemoteError
from spaceknife_client.views import ViewClient
from spaceknife_protocol import scan as p
from spaceknife_protocol import view as pv
from spaceknife_protocol.base import Empty
from spaceknife_protocol.classes import ClassEdit, ClassesUpdateParams
from spaceknife_protocol.methods import (
    CLASSES_GET,
    CLASSES_RESET,
    CLASSES_UPDATE,
    FILTER_VOCAB,
    SCAN_START,
    TREE_SUMMARY,
    VIEW_FILTER,
    VIEW_OPEN,
)

from .test_scan_server import wait_done


@pytest.fixture(autouse=True)
def isolated(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))
    yield
    classes.set_active(DEFAULT_CLASSES)  # the table is process-wide: never leak an edit


def edits(state: object, extra: list[ClassEdit]) -> list[ClassEdit]:
    rows = [ClassEdit(id=c.id, name=c.name, extensions=c.extensions, color=c.color) for c in state.classes]  # type: ignore[attr-defined]
    return [*rows[:-1], *extra, rows[-1]]


async def test_new_class_colours_and_filters(tmp_path: Path, server: Server, client: Connection) -> None:
    root = tmp_path / "root"
    root.mkdir()
    (root / "pakchunk0-Windows.pak").write_bytes(b"m" * 5000)
    (root / "movie.mp4").write_bytes(b"v" * 3000)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    await wait_done(client, started.scan_id)
    vc = ViewClient(client)
    update, frame = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=started.scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    view_id = update.state.view_id
    assert frame is not None
    row = frame.names.index("pakchunk0-Windows.pak")
    assert frame.columns["cls"][row] == DEFAULT_CLASSES.other_index

    state = await client.call(CLASSES_GET, Empty())
    assert not state.custom
    new = ClassEdit(name="游戏资源", extensions=["PAK; .ucas"], color="#c77dff")
    state = await client.call(CLASSES_UPDATE, ClassesUpdateParams(classes=edits(state, [new])))
    assert state.custom
    game = next(c for c in state.classes if c.name == "游戏资源")
    assert game.extensions == ["pak", "ucas"] and game.color == "#C77DFF"
    assert state.classes[-1].id == "other"
    saved = json.loads((tmp_path / "home" / "config" / "classes.json").read_text(encoding="utf-8"))
    assert any(c["name"] == "游戏资源" for c in saved["classes"])

    vocab = await client.call(FILTER_VOCAB, Empty())
    assert any(c.name == "游戏资源" for c in vocab.classes)
    update, frame = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text=":class:游戏资源"))
    assert frame is not None
    row = frame.names.index("pakchunk0-Windows.pak")
    assert frame.columns["cls"][row] == [c.id for c in state.classes].index(game.id)
    assert frame.columns["flags"][row] & F_MATCH
    assert not frame.columns["flags"][frame.names.index("movie.mp4")] & F_MATCH

    state = await client.call(CLASSES_RESET, Empty())
    assert not state.custom and not (tmp_path / "home" / "config" / "classes.json").exists()


async def test_summary_gives_each_top_file_its_class(tmp_path: Path, client: Connection) -> None:
    # ts is video or code by size (classes.STREAM_SPLITS): the list's colours come from the backend
    root = tmp_path / "root"
    root.mkdir()
    (root / "show.ts").write_bytes(b"G" * (188 * 5600))  # 1.0 MiB of whole 188-byte packets
    (root / "app.ts").write_bytes(b"x" * 3000)
    (root / "movie.mp4").write_bytes(b"v" * 3000)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    status = await wait_done(client, started.scan_id)
    res = await client.call(
        TREE_SUMMARY, p.TreeSummaryParams(scan_id=started.scan_id, node=status.roots[0].node, top=10)
    )
    assert {f.node.name: f.class_id for f in res.top_files} == {
        "show.ts": "video",
        "app.ts": "code",
        "movie.mp4": "video",
    }


async def test_invalid_tables_are_refused(client: Connection) -> None:
    state = await client.call(CLASSES_GET, Empty())
    video = state.classes[0]
    dup = ClassEdit(name=video.name, extensions=["xyz"])
    with pytest.raises(RemoteError, match="重复"):
        await client.call(CLASSES_UPDATE, ClassesUpdateParams(classes=edits(state, [dup])))
    without_other = [ClassEdit(id=c.id, name=c.name, extensions=c.extensions) for c in state.classes[:-1]]
    with pytest.raises(RemoteError, match="其他"):
        await client.call(CLASSES_UPDATE, ClassesUpdateParams(classes=without_other))
    assert not (await client.call(CLASSES_GET, Empty())).custom


def test_build_rules() -> None:
    rows = [ClassEdit(id=c.id, name=c.name, extensions=c.extensions) for c in DEFAULT_CLASSES.classes]
    table = build(rows)
    assert table.classes == DEFAULT_CLASSES.classes and not table.custom
    # renamed built-in keeps SpaceSniffer's names as aliases; other stays last whatever the order
    rows[0] = ClassEdit(id="video", name="影片", extensions=["mp4", ".MKV", "mp4"])
    table = build([rows[-1], *rows[:-1]])
    assert table.ids[-1] == "other" and table.by_id["video"].extensions == ["mp4", "mkv"]
    assert "Video/Movies" in table.by_id["video"].aliases
    with pytest.raises(ValueError, match="不能填扩展名"):
        build([*rows[:-1], ClassEdit(id="other", name="其他", extensions=["x"])])
    with pytest.raises(ValueError, match="不能有"):
        build([ClassEdit(name="a:b"), rows[-1]])


def test_each_theme_keeps_its_colour(tmp_path: Path) -> None:
    # W7 待定 22: a colour picked in the light theme leaves the dark one as it was; both are saved
    # (classes.json version 2), and "" stays the theme's own colour
    rows = [ClassEdit(id=c.id, name=c.name, extensions=c.extensions) for c in DEFAULT_CLASSES.classes]
    rows[0] = ClassEdit(id="video", name=rows[0].name, extensions=rows[0].extensions, color_light="#abcdef")
    rows[1] = ClassEdit(id="image", name=rows[1].name, extensions=rows[1].extensions, color="#123456")
    table = build(rows)
    assert (table.by_id["video"].color, table.by_id["video"].color_light) == ("", "#ABCDEF")
    assert table.custom
    path = tmp_path / "classes.json"
    classes.save(table, path)
    assert json.loads(path.read_text(encoding="utf-8"))["version"] == 2
    back = classes.load(path)
    assert (back.by_id["video"].color, back.by_id["video"].color_light) == ("", "#ABCDEF")
    assert (back.by_id["image"].color, back.by_id["image"].color_light) == ("#123456", "")
    with pytest.raises(ValidationError):
        ClassEdit(name="x", color_light="red")


@pytest.mark.parametrize("version", [1, None])
def test_colours_from_before_themes_stay_in_both(tmp_path: Path, version: int | None) -> None:
    # a file written when there was only the dark theme: the user's colours are kept in the light
    # theme too, not dropped for its own (classes.json version 1, or no version at all)
    rows = [ClassEdit(id=c.id, name=c.name, extensions=c.extensions) for c in DEFAULT_CLASSES.classes]
    rows[1] = ClassEdit(id="image", name=rows[1].name, extensions=rows[1].extensions, color="#123456")
    path = tmp_path / "classes.json"
    classes.save(build(rows), path)
    doc = json.loads(path.read_text(encoding="utf-8"))
    for c in doc["classes"]:
        c.pop("color_light")
    if version is None:
        doc.pop("version")
    else:
        doc["version"] = version
    path.write_text(json.dumps(doc), encoding="utf-8")
    old = classes.load(path)
    assert (old.by_id["image"].color, old.by_id["image"].color_light) == ("#123456", "#123456")
    assert (old.by_id["video"].color, old.by_id["video"].color_light) == ("", "")
    # saved again it is a version 2 file, and reads back the same
    classes.save(old, path)
    assert classes.load(path).by_id["image"].color_light == "#123456"
