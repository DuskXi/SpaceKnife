"""What an installed SpaceKnife 0.3.4 finds in a release, by 0.3.4's own code (tools/compat034.mjs
runs this; read that file first).

    <v0.3.4 tree>/.venv/bin/python -I tools/compat034_check.py --version 0.3.5 (--dir RELEASE | --github)

It needs the Python environment of a v0.3.4 source tree (``git archive v0.3.4``, ``uv sync``):
everything it decides comes from that tree's ``spaceknife.update`` — the release list as 0.3.4
reads it (``fetch_releases``), the release it offers (``choose``, preview channel: 0.3.4's
default), the file for each kind of install (``pick_asset``, its ``asset_templates``) and the
checksum it would check that file against (``fetch_checksums``: the SHA256SUMS*.txt it reads
first). ``--dir``: the files of a release that is not published yet (the Release workflow's
assembled folder), served on 127.0.0.1 as GitHub's API lists a release, with v0.3.4 before it;
``--github``: the published releases. Prints one JSON object; the caller judges it.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from spaceknife import update  # v0.3.4's, from its environment

AGENT = "SpaceKnife-compat034"
OLD = "0.3.4"

# v0.3.4 tools/release.py INSTALLS: (kind, sys.platform, platform.machine()) of every kind of
# install a release serves, as 0.3.4 names them
INSTALLS = (
    ("nsis", "win32", "AMD64"),
    ("portable", "win32", "AMD64"),
    ("portable", "win32", "ARM64"),
    ("portable", "linux", "x86_64"),
    ("portable", "linux", "aarch64"),
    ("portable", "darwin", "arm64"),
    ("portable", "darwin", "x86_64"),
    ("appimage", "linux", "x86_64"),
    ("deb", "linux", "x86_64"),
    ("rpm", "linux", "x86_64"),
    ("dmg", "darwin", "arm64"),
    ("dmg", "darwin", "x86_64"),
)


def serve(folder: Path, version: str) -> tuple[ThreadingHTTPServer, str]:
    """The folder's files as the one asset list of release ``version`` (and v0.3.4 before it, both
    pre-releases as on GitHub), on 127.0.0.1."""
    files = {p.name: p for p in sorted(folder.iterdir()) if p.is_file()}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            base = f"http://127.0.0.1:{self.server.server_address[1]}"
            if self.path == "/releases":

                def release(v: str, names: list[str]) -> dict[str, Any]:
                    return {
                        "tag_name": f"v{v}",
                        "name": f"SpaceKnife {v}",
                        "draft": False,
                        "prerelease": True,
                        "html_url": f"{base}/tag/v{v}",
                        "published_at": "2026-11-08T00:00:00Z",
                        "body": "",
                        "assets": [
                            {"name": n, "size": files[n].stat().st_size, "browser_download_url": f"{base}/files/{n}"}
                            for n in names
                        ],
                    }

                body = json.dumps([release(version, list(files)), release(OLD, [])]).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            name = self.path.removeprefix("/files/")
            # only the checksum lists are ever read here: nothing big is served
            if name not in files or not update.is_checksum_file(name):
                self.send_error(404)
                return
            data = files[name].read_bytes()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, format: str, *args: Any) -> None:
            pass

    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    http.daemon_threads = True
    threading.Thread(target=http.serve_forever, daemon=True).start()
    return http, f"http://127.0.0.1:{http.server_address[1]}/releases"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)
    where = ap.add_mutually_exclusive_group(required=True)
    where.add_argument("--dir", type=Path)
    where.add_argument("--github", action="store_true")
    args = ap.parse_args(argv)
    os.environ.setdefault("NO_PROXY", "127.0.0.1,localhost")
    http = None
    if args.dir:
        http, feed = serve(args.dir.resolve(), args.version)
    else:
        feed = update.RELEASES_URL
    try:
        fetched = update.fetch_releases(feed, None, AGENT)
        releases = fetched.releases or []
        offered = update.choose(releases, "preview")
        stable = update.choose(releases, "stable")
        target = next((r for r in releases if r.version == args.version), None)
        out: dict[str, Any] = {
            "feed": "github" if args.github else "local",
            "offered": offered.version if offered else None,
            "stable": stable.version if stable else None,
            "found": target is not None,
            "assets": [a.name for a in target.assets] if target else [],
            "installs": [],
        }
        for kind, system, machine in INSTALLS:
            row: dict[str, Any] = {"kind": kind, "system": system, "machine": machine}
            template = update.asset_template(kind, system, machine)  # type: ignore[arg-type]
            row["template"] = template
            asset = update.pick_asset(target, kind, system, machine) if target else None  # type: ignore[arg-type]
            row["asset"] = asset.name if asset else None
            if asset is not None:
                row["sums_order"] = [s.name for s in update.checksum_files(target, asset)]  # type: ignore[arg-type]
                try:
                    row["sha256"] = update.fetch_checksums(target, asset, AGENT)  # type: ignore[arg-type]
                except update.UpdateError as e:
                    row["error"] = str(e)
            out["installs"].append(row)
    finally:
        if http is not None:
            http.shutdown()
    json.dump(out, sys.stdout, indent=1)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
