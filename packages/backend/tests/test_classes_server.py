"""classes.* over the real WebSocket (W7 「文件类别」): edit, recolour, :class: filters, persistence."""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path

import pytest

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
    (root / "model-step-12000.safetensors").write_bytes(b"m" * 5000)
    (root / "movie.mp4").write_bytes(b"v" * 3000)
    started = await client.call(SCAN_START, p.ScanStartParams(roots=[str(root)], workers=0))
    await wait_done(client, started.scan_id)
    vc = ViewClient(client)
    update, frame = await vc.call(
        VIEW_OPEN, pv.ViewOpenParams(scan_id=started.scan_id, viewport=pv.Viewport(width=800, height=600))
    )
    view_id = update.state.view_id
    assert frame is not None
    row = frame.names.index("model-step-12000.safetensors")
    assert frame.columns["cls"][row] == DEFAULT_CLASSES.other_index

    state = await client.call(CLASSES_GET, Empty())
    assert not state.custom
    new = ClassEdit(name="模型", extensions=["SafeTensors; .gguf"], color="#c77dff")
    state = await client.call(CLASSES_UPDATE, ClassesUpdateParams(classes=edits(state, [new])))
    assert state.custom
    model = next(c for c in state.classes if c.name == "模型")
    assert model.extensions == ["safetensors", "gguf"] and model.color == "#C77DFF"
    assert state.classes[-1].id == "other"
    saved = json.loads((tmp_path / "home" / "config" / "classes.json").read_text(encoding="utf-8"))
    assert any(c["name"] == "模型" for c in saved["classes"])

    vocab = await client.call(FILTER_VOCAB, Empty())
    assert any(c.name == "模型" for c in vocab.classes)
    update, frame = await vc.call(VIEW_FILTER, pv.ViewFilterParams(view_id=view_id, text=":class:模型"))
    assert frame is not None
    row = frame.names.index("model-step-12000.safetensors")
    assert frame.columns["cls"][row] == [c.id for c in state.classes].index(model.id)
    assert frame.columns["flags"][row] & F_MATCH
    assert not frame.columns["flags"][frame.names.index("movie.mp4")] & F_MATCH

    state = await client.call(CLASSES_RESET, Empty())
    assert not state.custom and not (tmp_path / "home" / "config" / "classes.json").exists()


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
