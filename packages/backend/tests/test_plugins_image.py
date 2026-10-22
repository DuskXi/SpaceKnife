"""Tree images (read-only shared memory), host-side queries, node id epochs, output limits."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from spaceknife.plugins.image import Images, export
from spaceknife.plugins.query import run
from spaceknife.plugins.runtime.base import RuntimeCallError
from spaceknife.server.rpc import RpcError
from spaceknife_protocol.binary import decode_frame
from spaceknife_protocol.errors import ErrorCode
from spaceknife_sdk.image import map_image
from spaceknife_sdk.testing import MockHost, make_tree

from .plugin_helpers import mock
from .store_builder import build, find

pytestmark = pytest.mark.timeout(120)


@pytest.fixture(autouse=True)
def _home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))


def _store() -> object:
    return build({"a.log": 500, "b.txt": 10, "sub": {"c.log": 200, "d.bin": 1000}})


def test_an_image_maps_read_only() -> None:
    store = _store()
    e = export("scan-1", store, ["size", "parent", "kind"], lambda: np.zeros(len(store), dtype=np.uint8))  # type: ignore[arg-type]
    try:
        image = map_image(e.desc)
        assert image.n == len(store)  # type: ignore[arg-type]
        assert image.columns["size"].tolist() == store.cols["size"][: len(store)].tolist()  # type: ignore[attr-defined]
        assert not image.columns["size"].flags.writeable
        with pytest.raises(ValueError, match="read-only"):
            image.columns["size"][1] = 7
        image.close()
    finally:
        e.release()


def test_image_cache_counts_users() -> None:
    store = _store()
    images = Images()
    e = export("scan-1", store, ["size"])  # type: ignore[arg-type]
    images.add(e, "p.one")
    assert images.find("scan-1", store.version, {"size"}, running=False) is e  # type: ignore[attr-defined]
    assert images.find("scan-1", store.version + 1, {"size"}, running=False) is None  # type: ignore[attr-defined]
    assert images.bytes() >= 8 * len(store)  # type: ignore[arg-type]
    images.release_user("p.one")
    assert images.bytes() == 0
    images.close()


def test_a_wider_image_of_the_same_version_is_kept() -> None:
    """A plugin reading one column after another gets a new image each time (same tree version,
    more columns): the one just added stays, the older ones of that plugin go."""
    store = _store()
    images = Images()
    made = []
    for cols in (["kind"], ["alloc", "kind"], ["alloc", "kind", "size"], ["alloc", "kind", "mtime", "size"]):
        e = export("scan-1", store, cols)  # type: ignore[arg-type]
        images.add(e, "p.one")
        made.append(e)
        # what the plugin maps next is still there, with the values
        image = map_image(e.desc)
        assert image.columns["kind"].tolist() == store.cols["kind"][: len(store)].tolist()  # type: ignore[attr-defined]
        image.close()
    kept = images.find("scan-1", store.version, {"size", "mtime"}, running=False)  # type: ignore[attr-defined]
    assert kept is made[-1]
    assert images.bytes() <= made[-1].size + made[-2].size
    # another plugin's image is not dropped for this one's
    other = export("scan-1", store, ["size"])  # type: ignore[arg-type]
    images.add(other, "p.two")
    images.add(export("scan-1", store, ["parent"]), "p.one")  # type: ignore[arg-type]
    assert images.find("scan-1", store.version, {"size"}, running=False) is not None  # type: ignore[attr-defined]
    images.close()


async def test_a_plugin_reads_columns_one_after_another(tmp_path: Path) -> None:
    """Through a real plugin process: each new column comes back with the tree's values."""
    folder = tmp_path / "t.cols"
    folder.mkdir()
    (folder / "plugin.toml").write_text(
        'id = "t.cols"\nversion = "0.1.0"\nname = "cols"\nentry = "plugin.py"\npermissions = ["tree.read"]\n',
        encoding="utf-8",
    )
    (folder / "plugin.py").write_text(
        "from spaceknife_sdk import Context, Payload, Plugin\n"
        "plugin = Plugin()\n"
        "class Out(Payload):\n"
        "    files: int\n"
        "    alloc: int\n"
        "    size: int\n"
        "@plugin.method\n"
        "async def cols(ctx: Context) -> Out:\n"
        "    t = ctx.tree\n"
        "    files = t.col('kind') == 0\n"
        "    alloc = int(t.col('alloc')[files].sum())\n"
        "    size = int(t.col('size')[files].sum())\n"
        "    return Out(files=int(files.sum()), alloc=alloc, size=size)\n",
        encoding="utf-8",
    )
    host = MockHost.load(folder, process=True, grant="requested")
    host.scan(make_tree({"a.bin": 5000, "sub": {"b.bin": 3000}}))
    async with host:
        out = await host.call("cols")
    assert out["files"] == 2
    assert out["size"] == 8000
    assert out["alloc"] >= 8000


def test_queries() -> None:
    store = _store()
    value, frames = run(
        store, None, {"where": "*.log", "columns": ["bytes", "name", "path"], "sort": "-bytes"}
    )  # type: ignore[arg-type]
    _, cols = decode_frame(frames[0])
    assert value == {"total": 2, "count": 2}
    assert cols["bytes"].tolist() == [500, 200]
    blob = bytes(cols["name"])
    assert blob == b"a.logc.log" and cols["name_len"].tolist() == [5, 5]
    sub = find(store, "sub")  # type: ignore[arg-type]
    value, frames = run(store, None, {"under": sub, "kind": "file", "columns": ["size"], "sort": "size"})  # type: ignore[arg-type]
    assert decode_frame(frames[0])[1]["size"].tolist() == [200, 1000]
    value, frames = run(store, None, {"kind": "dir", "min_size": 1000, "columns": ["bytes"]})  # type: ignore[arg-type]
    assert decode_frame(frames[0])[1]["bytes"].tolist() == [1710, 1200]
    with pytest.raises(RuntimeCallError) as err:
        run(store, None, {"where": ">>1", "columns": []})  # type: ignore[arg-type]
    assert err.value.code == ErrorCode.INVALID_PARAMS
    with pytest.raises(RuntimeCallError):
        run(store, None, {"columns": ["nope"]})  # type: ignore[arg-type]


async def test_stale_ids_after_the_tree_is_reorganised() -> None:
    async with mock("t.full") as host:
        hp = await host._ready()
        rec = hp.records["t.full"]
        store = host.store
        value, _ = await hp.api.handle(
            rec, "host.node", {"scan_id": host.scan_id, "epoch": 0, "nodes": [1]}, []
        )
        assert value["nodes"][0]["id"] == 1
        store.epoch += 1  # what adopting a rebuilt tree does
        with pytest.raises(RuntimeCallError) as err:
            await hp.api.handle(rec, "host.node", {"scan_id": host.scan_id, "epoch": 0, "nodes": [1]}, [])
        assert err.value.code == ErrorCode.STALE_TREE
        # a cleanup's proposals with an old epoch are found again by path
        node = host.find("a.log")
        res = await hp.api.cleanup_result(
            rec,
            "logs",
            {"scan_id": host.scan_id},
            [
                {
                    "node": {"scan_id": host.scan_id, "node": 999, "epoch": 0, "path": store.path(node)},
                    "reason": "x",
                }
            ],
        )
        assert [i["node"]["node"] for i in res["items"]] == [node]


async def test_outputs_past_their_limits() -> None:
    async with mock("t.big") as host:
        rep = await host.report("long")
        table = rep.blocks[0]
        assert len(table.rows) == 2000  # type: ignore[union-attr]
        assert rep.blocks[-1].type == "callout"
        rep = await host.report("many")
        assert len(rep.blocks) == 201
    async with mock("t.big") as host:
        hp = await host._ready()
        route = hp.route("plugin.t.big.huge")
        assert route is not None
        with pytest.raises(RpcError) as err:
            await route.handler(host.session, route.spec.params.model_validate({}))  # type: ignore[arg-type]
        assert err.value.code == ErrorCode.PLUGIN_LIMIT
