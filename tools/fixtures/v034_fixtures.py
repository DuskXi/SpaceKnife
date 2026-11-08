"""Real SpaceKnife 0.3.4 data for the upgrade tests (`crates/sk-core/tests/data/v034`).

Every file it writes comes from the v0.3.4 Python backend itself: the scans are real scans of small
trees in a temporary folder, the snapshots are written by its `store.snapshot.save`, the data files
by its own server (settings, classes, folder classes, tags, palette recents) or its own modules
(update.json, volume_summaries.json). Run it with that backend, never in your own checkout:

    git worktree add --detach <tmp>/ref034 v0.3.4
    cd <tmp>/ref034 && uv sync
    .venv/bin/python <repo>/tools/fixtures/v034_fixtures.py <repo>/crates/sk-core/tests/data/v034 \
        --work <an empty temporary folder> [--v033 <a v0.3.3 checkout, for classes.json version 1>]

It writes `<out>/windows` on Windows and `<out>/posix` elsewhere (run it on both). The trees are
scanned where they are (a temporary folder), then every snapshot is loaded and saved again by
0.3.4 with its roots moved under a neutral folder (`C:\\Users\\lenny\\sk-fixture` /
`/home/lenny/sk-fixture`), so no fixture names the machine it was made on; the data files are made
afterwards against those neutral roots, so their keys (`root_key`) are 0.3.4's for them.

`<out>/<platform>/expected/*.json` hold what 0.3.4 reads back from each snapshot (every column,
the names as hex), `root_keys.json` the key 0.3.4 gives to a list of tricky roots, and
`root_keys_os.json` the key it gives to roots no UTF-8 text can hold (lone surrogates, bytes that are
not UTF-8). `--root-keys-only` writes just those two (no scans, no `--work`).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import subprocess
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

WINDOWS = sys.platform == "win32"
PLATFORM = "windows" if WINDOWS else "posix"
NEUTRAL = "C:\\Users\\lenny\\sk-fixture" if WINDOWS else "/home/lenny/sk-fixture"
TOKEN = "fixture-token-0123456789abcdef"
EPOCH = datetime(2026, 10, 26, 9, 0, 0, tzinfo=UTC)  # the auto-save stamps (0.3.4 was published then)


# --------------------------------------------------------------------------- the trees


def put(path: Path, size: int = 0, data: bytes | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data if data is not None else bytes((i * 7) & 0xFF for i in range(size)))


def put_raw(folder: Path, name: bytes, size: int) -> None:
    """A file whose name is raw bytes (POSIX: names that are not UTF-8)."""
    fd = os.open(os.path.join(os.fsencode(folder), name), os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o644)
    try:
        os.write(fd, b"r" * size)
    finally:
        os.close(fd)


def tree_plain(root: Path) -> None:
    put(root / "readme.txt", 120)
    put(root / "Makefile", 64)
    put(root / ".hidden", 3)
    put(root / "photos" / "IMG_0001.JPG", 4096)
    put(root / "photos" / "img_0002.jpeg", 2048)
    put(root / "photos" / "2026" / "trip.mp4", 9000)
    put(root / "photos" / "2026" / "notes.md", 512)
    put(root / "docs" / "報告.docx", 3000)
    put(root / "docs" / "emoji-🙂.txt", 10)
    put(root / "docs" / "archive.tar.gz", 7000)
    put(root / "docs" / ("long-" + "x" * 200 + ".log"), 1)
    put(root / "docs" / "empty.dat", 0)
    put(root / "code" / "src" / "main.rs", 800)
    put(root / "code" / "src" / "lib.RS", 300)
    put(root / "code" / "target" / "debug" / "app.exe", 5000)
    (root / "empty-folder").mkdir(parents=True)
    (root / "deep" / "a" / "b" / "c" / "d").mkdir(parents=True)
    put(root / "deep" / "a" / "b" / "c" / "d" / "leaf.bin", 1500)


def tree_twins(root: Path) -> None:
    """Names that differ only in case (POSIX file systems keep both)."""
    put(root / "README.md", 10)
    put(root / "Readme.md", 20)
    put(root / "readme.md", 30)
    put(root / "Docs" / "a.txt", 5)
    put(root / "docs" / "a.txt", 6)
    put(root / "docs" / "A.txt", 7)


def tree_odd(root: Path) -> list[str]:
    """Names a UTF-8 decoder cannot read: lone surrogates on Windows, bytes that are not UTF-8 on
    POSIX; and a few other odd ones."""
    root.mkdir(parents=True)
    made = []
    if WINDOWS:
        for name in ["lone-\udc80.txt", "high-\ud83d-end", "ext.\udcff", "\udfff", "pair-\U0001f642.txt"]:
            put(root / name, 11)
            made.append(name)
        (root / "dir-\ud800").mkdir()
        put(root / "dir-\ud800" / "inside.txt", 4)
    else:
        for name in [b"bad-\xff-name.txt", b"\xe9t\xe9.txt", b"ext.\xff", b"\x80", b"back\\slash.txt",
                     b"new\nline.txt", b'quote".txt']:
            put_raw(root, name, 13)
            made.append(os.fsdecode(name))
        os.mkdir(os.path.join(os.fsencode(root), b"dir-\xfe"))
        put_raw(root / os.fsdecode(b"dir-\xfe"), b"inside.txt", 4)
    put(root / "plain.txt", 9)
    put(root / "trailing space .txt", 2)
    return made


def tree_hardlinks(root: Path) -> None:
    put(root / "original.bin", 8192)
    os.link(root / "original.bin", root / "link-1.bin")
    put(root / "sub" / "own.bin", 100)
    os.link(root / "original.bin", root / "sub" / "link-2.bin")


def tree_denied(root: Path) -> Path:
    put(root / "open" / "ok.txt", 50)
    put(root / "locked" / "secret.txt", 70)
    put(root / "locked" / "inner" / "deep.txt", 70)
    put(root / "after.txt", 5)
    return root / "locked"


def deny(folder: Path) -> None:
    if WINDOWS:
        # Everyone may not list it (removed again by allow())
        subprocess.run(["icacls", str(folder), "/deny", "*S-1-1-0:(RD)"], check=True, capture_output=True)
    else:
        os.chmod(folder, 0)


def allow(folder: Path) -> None:
    if WINDOWS:
        subprocess.run(["icacls", str(folder), "/remove:d", "*S-1-1-0"], check=True, capture_output=True)
    else:
        os.chmod(folder, 0o755)


def tree_multi(a: Path, b: Path) -> None:
    put(a / "a1.txt", 100)
    put(a / "sub" / "a2.mp3", 2000)
    put(b / "b1.txt", 300)
    put(b / "nested" / "deeper" / "b2.iso", 6000)


def tree_monitored(root: Path) -> None:
    put(root / "stays.txt", 100)
    put(root / "gone.txt", 200)
    put(root / "grow.bin", 100)
    put(root / "old-folder" / "x.txt", 300)
    put(root / "old-folder" / "inner" / "y.txt", 400)
    put(root / "renamed-from" / "z.txt", 500)


def change_monitored(root: Path) -> None:
    (root / "gone.txt").unlink()
    shutil.rmtree(root / "old-folder")
    (root / "renamed-from").rename(root / "renamed-to")
    put(root / "new.txt", 600)
    put(root / "grow.bin", 5000)
    put(root / "new-folder" / "w.txt", 700)


def tree_stopped(root: Path) -> None:
    for i in range(24):
        for j in range(4):
            put(root / f"d{i:02}" / f"e{j}" / "f.txt", 10 + i + j)


# --------------------------------------------------------------------------- 0.3.4, as a server


async def call(conn: Any, spec: Any, params: Any) -> Any:
    return await conn.call(spec, params)


async def wait_status(conn: Any, scan_id: str, ok: Any, timeout: float = 60) -> Any:
    from spaceknife_protocol import scan as p
    from spaceknife_protocol.methods import SCAN_STATUS

    loop = asyncio.get_running_loop()
    end = loop.time() + timeout
    while loop.time() < end:
        st = await conn.call(SCAN_STATUS, p.ScanIdParams(scan_id=scan_id))
        if ok(st):
            return st
        await asyncio.sleep(0.05)
    raise TimeoutError(f"scan {scan_id} did not get there: {st}")


async def scan_all(work: Path, raw: Path) -> dict[str, list[str]]:
    """Phase A: real scans of the trees, each saved by hand to raw/<name>.sks."""
    from spaceknife.scan import scanner as scanner_mod
    from spaceknife.server import Server, ServerConfig
    from spaceknife_client.connection import Connection
    from spaceknife_protocol import scan as p
    from spaceknife_protocol import settings as ps
    from spaceknife_protocol.methods import SCAN_START, SETTINGS_UPDATE, SNAPSHOT_SAVE, TREE_FIND
    from spaceknife_protocol.session import ClientInfo

    trees = work / "trees"
    roots: dict[str, list[str]] = {}
    tree_plain(trees / "plain")
    roots["plain"] = [str(trees / "plain")]
    if not WINDOWS:
        tree_twins(trees / "twins")
        roots["twins"] = [str(trees / "twins")]
    tree_odd(trees / "odd")
    roots["odd"] = [str(trees / "odd")]
    tree_hardlinks(trees / "hardlinks")
    roots["hardlinks"] = [str(trees / "hardlinks")]
    locked = tree_denied(trees / "denied")
    roots["denied"] = [str(trees / "denied")]
    tree_multi(trees / "multi-a", trees / "multi-b")
    roots["multi"] = [str(trees / "multi-a"), str(trees / "multi-b")]
    tree_monitored(trees / "monitored")
    roots["monitored"] = [str(trees / "monitored")]
    tree_stopped(trees / "stopped")
    roots["stopped"] = [str(trees / "stopped")]

    os.environ["SPACEKNIFE_HOME"] = str(work / "home0")
    server = Server(ServerConfig(token=TOKEN, hello_timeout=5, ping_interval=None, update_checks=False))
    await server.start()
    conn = await Connection.open(server.url, TOKEN, ClientInfo(name="fixtures", version="0.0.0", kind="test"))
    try:
        for name, rs in roots.items():
            # live monitoring only where the tree is changed on purpose (a watcher would otherwise
            # rescan the locked folder once it is open again)
            await call(conn, SETTINGS_UPDATE, ps.SettingsUpdateParams(values={"scan.monitor": name == "monitored"}))
            if name == "denied":
                deny(locked)
            original_apply = scanner_mod.Scanner._apply
            original_init = scanner_mod.Scanner.__init__
            if name == "stopped":

                def apply_then_stop(self: Any, *results: Any) -> None:
                    original_apply(self, *results)
                    self.cancel()  # stop after the first results: the rest stays pending

                def small_slices(self: Any, *args: Any, **kwargs: Any) -> None:
                    # one folder per slice, so the first results are not the whole tree
                    original_init(self, *args, **{**kwargs, "budget_s": 0.0, "batch_tasks": 1})

                scanner_mod.Scanner._apply = apply_then_stop  # type: ignore[method-assign]
                scanner_mod.Scanner.__init__ = small_slices  # type: ignore[method-assign]
            try:
                started = await call(conn, SCAN_START, p.ScanStartParams(roots=rs, workers=0, auto_snapshot=False))
                want = "cancelled" if name == "stopped" else "done"
                st = await wait_status(conn, started.scan_id, lambda s: s.state in ("done", "cancelled", "failed"))
                assert st.state == want, (name, st.state)
                if name == "stopped":
                    assert st.pending_dirs > 0, st
                if name == "denied":
                    assert st.denied, st
                    await call(conn, SNAPSHOT_SAVE, p.SnapshotSaveParams(scan_id=started.scan_id, path=str(raw / f"{name}.sks")))
            finally:
                scanner_mod.Scanner._apply = original_apply  # type: ignore[method-assign]
                scanner_mod.Scanner.__init__ = original_init  # type: ignore[method-assign]
                if name == "denied":
                    allow(locked)
            if name == "denied":
                continue
            if name == "monitored":
                await wait_status(
                    conn, started.scan_id, lambda s: s.monitor is not None and s.monitor.state == "running"
                )
                await asyncio.sleep(0.5)
                change_monitored(Path(rs[0]))

                async def applied() -> bool:
                    new = await call(conn, TREE_FIND, p.TreeFindParams(scan_id=started.scan_id, path=str(Path(rs[0]) / "new-folder" / "w.txt")))
                    gone = await call(conn, TREE_FIND, p.TreeFindParams(scan_id=started.scan_id, path=str(Path(rs[0]) / "old-folder")))
                    moved = await call(conn, TREE_FIND, p.TreeFindParams(scan_id=started.scan_id, path=str(Path(rs[0]) / "renamed-to" / "z.txt")))
                    return new.node is not None and gone.node is None and moved.node is not None

                for _ in range(400):
                    if await applied():
                        break
                    await asyncio.sleep(0.05)
                else:
                    raise TimeoutError("the monitor did not apply the changes")
                await asyncio.sleep(0.5)
            await call(conn, SNAPSHOT_SAVE, p.SnapshotSaveParams(scan_id=started.scan_id, path=str(raw / f"{name}.sks")))
    finally:
        await conn.close()
        await server.close()
    return roots


# --------------------------------------------------------------------------- neutral roots


def neutral(root: str, trees: Path) -> str:
    rel = os.path.relpath(root, trees)
    return os.path.join(NEUTRAL, rel)


def moved(value: Any, old: list[str], new: list[str]) -> Any:
    """``value`` with every string under an old root moved under its new root."""
    if isinstance(value, str):
        for o, n in zip(old, new, strict=True):
            if value == o or value.startswith(o.rstrip("\\/") + os.sep):
                return n + value[len(o) :]
        return value
    if isinstance(value, list):
        return [moved(v, old, new) for v in value]
    if isinstance(value, dict):
        return {k: moved(v, old, new) for k, v in value.items()}
    return value


def anonymous(volume: dict[str, Any]) -> dict[str, Any]:
    """A volume as 0.3.4 described it, without what identifies the machine (serial, sizes, label,
    device)."""
    out = dict(volume)
    for key, value in {
        "serial": "1234ABCD" if WINDOWS else "",
        "label": "",
        "device": "C:" if WINDOWS else "/dev/sda1",
        "total": 256_000_000_000,
        "used": 128_000_000_000,
        "free": 128_000_000_000,
        "remote_path": "",
        "reserved": 0,
        "dev": 0 if WINDOWS else 2049,
    }.items():
        if key in out:
            out[key] = value
    return out


def rewrite(src: Path, dst: Path, trees: Path) -> list[str]:
    """Load a 0.3.4 snapshot and save it again (0.3.4's own writer) with its roots moved under the
    neutral folder: only the root nodes' names and the header's paths change."""
    import numpy as np
    from spaceknife.store import snapshot
    from spaceknife.store.nodes import NodeStore, encode_name

    store, header = snapshot.load(src)
    old = list(header.roots)
    new = [neutral(r, trees) for r in old]
    cols = store.export_columns()
    image = np.frombuffer(store.names_blob, dtype=np.uint8)
    off = cols["name_off"].astype(np.int64)
    ln = cols["name_len"].astype(np.int64)
    roots = list(store.roots())
    renamed = {node: encode_name(new[old.index(store.name(node))]) for node in roots}
    names = []
    for i in range(len(store)):
        names.append(renamed.get(i, image[off[i] : off[i] + ln[i]].tobytes()))
    lens = np.array([len(b) for b in names], dtype=np.uint16)
    blob = b"".join(b"\x00" + b for b in names)
    cols = dict(cols)
    cols["name_len"] = lens
    cols["name_off"] = (np.cumsum(lens.astype(np.int64) + 1) - lens).astype(np.uint32)
    out = NodeStore.from_columns(cols, blob, store.ext_table, separated=True)
    snapshot.save(
        out,
        dst,
        roots=new,
        volumes=[anonymous(v) for v in moved(list(header.volumes), old, new)],
        stats=moved(dict(header.stats), old, new),
        checkpoints=dict(header.checkpoints),
    )
    return new


def dump(path: Path) -> dict[str, Any]:
    """Everything 0.3.4 reads back from a snapshot (the names as hex: they need not be text)."""
    from spaceknife.store import snapshot

    store, header = snapshot.load(path)
    cols = store.export_columns()
    image = store.names_blob
    off, ln = cols["name_off"], cols["name_len"]
    return {
        "format": header.format,
        "app_version": header.app_version,
        "created_at": header.created_at.isoformat(),
        "roots": header.roots,
        "node_count": header.node_count,
        "ext_table_hex": [e.encode("utf-8", "surrogatepass").hex() for e in header.ext_table],
        "stats": header.stats,
        "volumes": header.volumes,
        "columns": {k: [int(x) for x in v] for k, v in cols.items() if k != "name_off"},
        "names_hex": [image[int(off[i]) : int(off[i]) + int(ln[i])].hex() for i in range(len(store))],
    }


# --------------------------------------------------------------------------- the data files


async def make_home(home: Path, snaps: Path, roots: dict[str, list[str]], v033: Path | None) -> dict[str, Any]:
    """Phase C: a 0.3.4 data folder (SPACEKNIFE_HOME layout) with the neutral roots."""
    from spaceknife import update as upd
    from spaceknife.server import Server, ServerConfig
    from spaceknife.server.volume_summaries import Kept, VolumeSummaries
    from spaceknife.store import snapshot
    from spaceknife_client.connection import Connection
    from spaceknife_protocol import classes as pc
    from spaceknife_protocol import palette as pp
    from spaceknife_protocol import scan as p
    from spaceknife_protocol import settings as ps
    from spaceknife_protocol import view as pv
    from spaceknife_protocol.base import Empty
    from spaceknife_protocol.methods import (
        CLASSES_FOLDER,
        CLASSES_GET,
        CLASSES_UPDATE,
        PALETTE_REMEMBER,
        SETTINGS_UPDATE,
        SNAPSHOT_LOAD,
        TREE_FIND,
        VIEW_OPEN,
        VIEW_SELECT,
        VIEW_TAG,
    )
    from spaceknife_protocol.session import ClientInfo

    os.environ["SPACEKNIFE_HOME"] = str(home)
    facts: dict[str, Any] = {"auto": {}}
    # the auto-save folders: one snapshot each, the plain tree's with older ones (weekly retention)
    for i, (name, rs) in enumerate(roots.items()):
        when = EPOCH + timedelta(minutes=i)
        dst = snapshot.auto_path(rs, when=when, base=home / "data" / "snapshots")
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(snaps / f"{name}.sks", dst)
        facts["auto"][name] = {"key": snapshot.root_key(rs), "file": dst.name, "roots": rs}
    plain_folder = home / "data" / "snapshots" / snapshot.root_key(roots["plain"])
    older = []
    for weeks in (1, 2, 3):
        when = EPOCH - timedelta(weeks=weeks)
        dst = snapshot.auto_path(roots["plain"], when=when, base=home / "data" / "snapshots")
        shutil.copyfile(snaps / "plain.sks", dst)
        older.append(dst.name)
    facts["older_plain"] = older

    server = Server(ServerConfig(token=TOKEN, hello_timeout=5, ping_interval=None, update_checks=False))
    await server.start()
    conn = await Connection.open(server.url, TOKEN, ClientInfo(name="fixtures", version="0.0.0", kind="test"))
    try:
        values: dict[str, Any] = {
            "appearance.theme": "dark",
            "appearance.accent": "#FF8800",
            "general.update_channel": "stable",
            "treemap.font_scale": 120,
            "treemap.flame_origin": "top",
            "scan.unit_base": "1024",
            "scan.workers": 2,
            "tags.name_red": "Delete me",
            "advanced.snapshot_keep": 7,
            "advanced.log_level": "DEBUG",
        }
        await call(conn, SETTINGS_UPDATE, ps.SettingsUpdateParams(values=values))
        facts["settings"] = values

        state = await call(conn, CLASSES_GET, Empty())
        edits = []
        for c in state.classes:
            name = "Movies" if c.id == "video" else c.name
            color = "#112233" if c.id == "video" else c.color
            edits.append(pc.ClassEdit(id=c.id, name=name, extensions=c.extensions, color=color,
                                      color_light="#AABBCC" if c.id == "video" else c.color_light))
        edits.insert(0, pc.ClassEdit(id=None, name="Fixture class", extensions=["fx1", "FX2", ".fx3"],
                                     color="#123456", color_light=""))
        out = await call(conn, CLASSES_UPDATE, pc.ClassesUpdateParams(classes=edits))
        facts["classes"] = [c.model_dump(mode="json") for c in out.classes]
        folder = os.path.join(roots["plain"][0], "photos")
        await call(conn, CLASSES_FOLDER, pc.ClassesFolderParams(path=folder, class_id="video"))
        other = os.path.join(roots["plain"][0], "code")
        await call(conn, CLASSES_FOLDER, pc.ClassesFolderParams(path=other, class_id=""))
        facts["folders"] = [[folder, "video"], [other, ""]]

        # tags and palette jumps: on the plain tree's snapshot, as the app would
        auto = plain_folder / facts["auto"]["plain"]["file"]
        loaded = await call(conn, SNAPSHOT_LOAD, p.SnapshotLoadParams(path=str(auto)))
        view = await call(conn, VIEW_OPEN, pv.ViewOpenParams(scan_id=loaded.scan_id, viewport=pv.Viewport(width=800, height=600)))
        tagged = {}
        for rel, color in [("photos", "red"), (os.path.join("docs", "報告.docx"), "yellow"), ("code", "blue")]:
            target = os.path.join(roots["plain"][0], rel)
            found = await call(conn, TREE_FIND, p.TreeFindParams(scan_id=loaded.scan_id, path=target))
            assert found.node is not None, target
            await call(conn, VIEW_SELECT, pv.ViewSelectParams(view_id=view.state.view_id, key=found.node.id))
            await call(conn, VIEW_TAG, pv.ViewTagParams(view_id=view.state.view_id, color=color))
            tagged[rel.replace(os.sep, "/")] = color
        facts["tags"] = tagged
        jumps = [os.path.join(roots["plain"][0], "docs"), os.path.join(roots["plain"][0], "photos", "2026")]
        for j in jumps:
            await call(conn, PALETTE_REMEMBER, pp.PaletteRememberParams(kind="jump", text=j, scan_id=loaded.scan_id))
        filters = ["*.mp4", ">1MB | :class:video"]
        for f in filters:
            await call(conn, PALETTE_REMEMBER, pp.PaletteRememberParams(kind="filter", text=f))
        facts["jumps"] = list(reversed(jumps))
        facts["filters"] = list(reversed(filters))
    finally:
        await conn.close()
        await server.close()

    # update.json: 0.3.4's own state, as a check of a (made-up, local) release list leaves it
    feed = [
        {"tag_name": "v0.3.4", "name": "SpaceKnife 0.3.4", "prerelease": True, "draft": False,
         "published_at": "2026-10-26T08:00:00Z", "html_url": "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.3.4",
         "body": "Notes of 0.3.4", "assets": [{"name": "SpaceKnife-0.3.4-Setup-x64.exe", "size": 123,
         "browser_download_url": "https://github.com/DuskXi/SpaceKnife/releases/download/v0.3.4/SpaceKnife-0.3.4-Setup-x64.exe"}]},
        {"tag_name": "v0.3.3", "name": "SpaceKnife 0.3.3", "prerelease": True, "draft": False,
         "published_at": "2026-10-21T08:00:00Z", "html_url": "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.3.3",
         "body": "Notes of 0.3.3", "assets": []},
        {"tag_name": "v0.4.0-beta.1", "name": "", "prerelease": True, "draft": True, "assets": []},
    ]
    releases = upd.trim(upd.parse_releases(feed))
    state_doc = upd.UpdateState(
        checked_at=datetime(2026, 11, 1, 12, 30, 15, 123456, tzinfo=UTC),
        etag='W/"0123abcd"',
        releases=releases,
        kept=upd.HISTORY_KEEP,
        dismissed=["0.3.3"],
        muted=["0.2"],
    )
    upd.save_state(state_doc, upd.state_file())
    facts["update"] = json.loads(state_doc.model_dump_json())

    kept_snapshot = str(Path(NEUTRAL) / "elsewhere" / "kept.sks")
    summaries = VolumeSummaries(home / "data" / "volume_summaries.json")
    at = datetime(2026, 10, 30, 18, 0, 0, tzinfo=UTC)
    mount = "C:\\" if WINDOWS else "/"
    entries = [
        Kept(mount=mount, identity="serial:1234ABCD" if WINDOWS else "ext4:256000000000", at=at,
             seen=datetime(2026, 11, 1, 9, 0, 0, tzinfo=UTC), files=1234, dirs=56, bytes_alloc=987654321,
             method="mft" if WINDOWS else "hard", snapshot=kept_snapshot,
             classes=[{"class_id": "video", "bytes": 1000, "files": 3,
                       "parts": [{"class_id": "image", "bytes": 10}]},
                      {"class_id": "other", "bytes": 500, "files": 7}],
             folders=[{"name": "Users", "bytes": 5000}, {"name": "data", "bytes": 300}]),
        Kept(mount=os.path.join(NEUTRAL, "usb") if not WINDOWS else "F:\\", identity="exfat:64000000000",
             at=at - timedelta(days=3), seen=at - timedelta(days=2), files=5, bytes_alloc=4096, snapshot=None),
    ]
    summaries.put(entries)
    facts["volume_summaries"] = json.loads((home / "data" / "volume_summaries.json").read_text("utf-8"))

    if v033 is not None:
        facts["classes_v1"] = classes_v1(home.parent / "classes_v1.json", v033)
    return facts


CLASSES_V1 = r"""
import json, sys
from pathlib import Path
from spaceknife import classes
from spaceknife_protocol.classes import ClassEdit
table = classes.active()
edits = [ClassEdit(id=c.id, name=("Films" if c.id == "video" else c.name), extensions=c.extensions,
                   color=("#445566" if c.id == "video" else c.color)) for c in table.classes]
edits.insert(0, ClassEdit(id=None, name="Old class", extensions=["old1"], color="#654321"))
classes.save(classes.build(edits), Path(sys.argv[1]))
"""


def classes_v1(dst: Path, v033: Path) -> Any:
    """classes.json version 1, written by 0.3.3's own classes.save (one colour for both themes)."""
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [str(v033 / "packages" / "backend" / "src"), str(v033 / "packages" / "protocol" / "src")]
    )
    env["SPACEKNIFE_LANGUAGE"] = "zh-CN"
    subprocess.run([sys.executable, "-c", CLASSES_V1, str(dst)], check=True, env=env)
    return json.loads(dst.read_text("utf-8"))


# --------------------------------------------------------------------------- root keys


def root_key_cases() -> list[dict[str, Any]]:
    from spaceknife.store import snapshot

    if WINDOWS:
        lists = [
            ["C:\\"], ["c:\\"], ["C:"], ["C:/"], ["D:\\Data"], ["d:/data/"], ["D:\\data\\\\sub\\.\\x\\..\\y"],
            ["C:\\Users\\lenny"], ["C:\\Users\\lenny", "D:\\"], ["D:\\", "C:\\Users\\lenny"],
            ["\\\\server\\share"], ["\\\\server\\share\\"], ["\\\\server\\share\\dir\\sub"], ["//server/share/dir"],
            ["\\\\?\\C:\\"], ["\\\\?\\UNC\\server\\share\\x"], ["\\\\.\\PhysicalDrive0"], ["C:foo"], ["\\rooted"],
            ["C:\\ÄÖÜ\\Straße"], ["C:\\STRASSE ẞ"], ["C:\\İstanbul"], ["C:\\ΟΔΥΣΣΕΥΣ"], ["C:\\ǅemal"],
            ["C:\\Ꭰ Cherokee"], ["C:\\Ა Mtavruli"], ["C:\\ＡＢＣ"], ["C:\\Ⅻ roman"], ["C:\\Ⓐ circled"],
            ["C:\\हिंदी"], ["C:\\cafe\u0301"], ["C:\\下载\\视频"], ["C:\\emoji 🙂 x"], ["C:\\٣٤ digits"],
            ["C:\\" + "a" * 40], ["C:\\___"], ["C:\\..."], ["C:\\a b-c.d_e"], ["C:\\𝐀𝐁 math"],
            ["C:\\data", "c:\\DATA"], ["C:\\x\\..\\.."], ["."], ["relative\\path"],
        ]
    else:
        lists = [
            ["/"], ["//"], ["///"], ["//a/b"], ["///a/b"], ["/home/lenny"], ["/home/lenny/"], ["/home//lenny/./x/../y"],
            ["/home/lenny", "/mnt/data"], ["/mnt/data", "/home/lenny"], ["/Volumes/Macintosh HD"], ["/HOME/Lenny"],
            ["/a/b\\c"], ["/ÄÖÜ/Straße"], ["/İstanbul"], ["/ΟΔΥΣΣΕΥΣ"], ["/हिंदी"], ["/cafe\u0301"], ["/下载/视频"],
            ["/emoji 🙂 x"], ["/Ⓐ circled"], ["/Ⅻ roman"], ["/" + "a" * 40], ["/___"], ["/..."], ["/a b-c.d_e"],
            ["/data", "/DATA"], ["/x/../.."], ["."], ["relative/path"], ["/𝐀𝐁 math"],
        ]
    return [{"roots": r, "key": snapshot.root_key(r)} for r in lists]


def os_root_key_cases() -> list[dict[str, Any]]:
    """Roots no UTF-8 text can hold, as the system names them, and the key 0.3.4 gave them.

    0.3.4 hashed the text Python made of the path: on Windows the UTF-16 units (a valid pair one
    character, a lone surrogate one code point of its own), on POSIX `os.fsdecode` (each byte that
    is not UTF-8 as U+DC80..U+DCFF). `roots_hex` is the path as the system gives it: UTF-16LE
    units on Windows, the bytes on POSIX."""
    from spaceknife.store import snapshot

    if WINDOWS:
        lists: list[list[bytes]] = [
            [u.encode("utf-16-le", "surrogatepass") for u in r]
            for r in [
                ["C:\\Users\\lenny\\lone-\udc80"],
                ["C:\\Users\\lenny\\\ud800high"],
                ["C:\\Users\\lenny\\end-\ud83d"],
                ["C:\\\udfff"],
                ["C:\\\udc80\\"],
                ["C:\\data\\x\udfff\\..\\y\udc80"],
                ["D:\\ÄBC\udcffÖ\\İx"],
                ["C:\\pair-\U0001f642-\udc80"],
                ["C:\\b\udc80", "C:\\a"],
                ["C:\\a", "c:\\B\ud800"],
                ["\\\\server\\share\\\udc80dir"],
                ["\\\\server\\\udc80share"],
                ["C:\\" + "x\udc80" * 20],
                ["C:/mixed/Sep\udc80/"],
            ]
        ]
        decode = lambda raw: raw.decode("utf-16-le", "surrogatepass")  # noqa: E731
    else:
        lists = [
            [b"/home/lenny/bad-\xff-name"],
            [b"/home/lenny/\xe9t\xe9"],
            [b"/\x80"],
            [b"/\xff/"],
            [b"/home/lenny/caf\xc3\xa9-\xff"],
            [b"/x/\xed\xa0\x80y"],
            [b"/x/\xf0\x9f\x98"],
            [b"/x/\xf0\x9f\x98a"],
            [b"/x/\xc0\xafz"],
            [b"/x/\xf4\x90\x80\x80"],
            [b"/x/\xe0\x80\x80"],
            [b"/a/\xff/../b\xfe"],
            [b"/m\xfcnchen", b"/a"],
            [b"/a", b"/\xff", b"/\xc3\xbf"],
            [b"//\xffshare/x"],
            [b"/home/lenny/\xff\xfe" + "下载".encode()],
            [b"/" + b"y\xff" * 20],
        ]
        decode = os.fsdecode
    return [
        {"roots_hex": [raw.hex() for raw in r], "key": snapshot.root_key([decode(raw) for raw in r])}
        for r in lists
    ]


def write_root_keys(target: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    (target / "root_keys.json").write_text(
        json.dumps(root_key_cases(), ensure_ascii=True, indent=1) + "\n", encoding="utf-8", newline="\n"
    )
    (target / "root_keys_os.json").write_text(
        json.dumps(os_root_key_cases(), ensure_ascii=True, indent=1) + "\n", encoding="utf-8", newline="\n"
    )


# --------------------------------------------------------------------------- main


async def amain(out: Path, work: Path, v033: Path | None) -> None:
    os.environ["SPACEKNIFE_LANGUAGE"] = "en"
    target = out / PLATFORM
    if target.exists():
        shutil.rmtree(target)
    raw = work / "raw"
    raw.mkdir(parents=True, exist_ok=True)
    roots = await scan_all(work, raw)
    snaps = target / "snapshots"
    snaps.mkdir(parents=True)
    neutral_roots = {}
    for name in roots:
        neutral_roots[name] = rewrite(raw / f"{name}.sks", snaps / f"{name}.sks", work / "trees")
    expected = target / "expected"
    expected.mkdir()
    for name in roots:
        (expected / f"{name}.json").write_text(
            json.dumps(dump(snaps / f"{name}.sks"), ensure_ascii=True, indent=1, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="\n",
        )
    facts = await make_home(target / "home", snaps, neutral_roots, v033)
    facts["roots"] = neutral_roots
    (target / "facts.json").write_text(
        json.dumps(facts, ensure_ascii=True, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n"
    )
    write_root_keys(target)
    # nothing of the machine it ran on: no temporary folder, no user name, also not inside the
    # snapshots' compressed names or in the dumps' hex
    from spaceknife.store import snapshot

    leaks = [str(work), os.path.expanduser("~")]
    for f in target.rglob("*"):
        if not f.is_file():
            continue
        data = f.read_bytes()
        if f.suffix == ".sks":
            store, header = snapshot.load(f)
            data += b"".join(store.name_bytes(i) for i in range(len(store)))
            data += json.dumps(header.model_dump(mode="json")).encode("utf-8", "surrogatepass")
        elif f.parent.name == "expected":
            data += b"".join(bytes.fromhex(h) for h in json.loads(data)["names_hex"])
        for leak in leaks:
            for form in {leak, leak.replace("\\", "\\\\"), leak.replace("\\", "/")}:
                if form.encode("utf-8") in data:
                    raise SystemExit(f"{f} names {form!r}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=Path)
    ap.add_argument("--work", type=Path)
    ap.add_argument("--v033", type=Path)
    ap.add_argument(
        "--root-keys-only", action="store_true", help="only (re)write root_keys.json and root_keys_os.json"
    )
    args = ap.parse_args()
    if args.root_keys_only:
        write_root_keys(args.out.resolve() / PLATFORM)
        return
    if args.work is None:
        raise SystemExit("--work is required")
    args.work.mkdir(parents=True, exist_ok=True)
    if any(args.work.iterdir()):
        raise SystemExit("--work must be an empty folder")
    asyncio.run(amain(args.out.resolve(), args.work.resolve(), args.v033))
    for item in args.work.iterdir():  # the trees and the first home (kept when something failed)
        shutil.rmtree(item) if item.is_dir() else item.unlink()


if __name__ == "__main__":
    main()
