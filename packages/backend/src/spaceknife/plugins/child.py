"""The plugin process: ``python -m spaceknife.plugins.child --pipe … --token-file … --server-pid …``.

Connects to the backend (only to an address it could have made; the peer must be that backend),
proves itself with the one-time token, takes ``init``, limits itself and installs the audit hook,
and only then imports the plugin's code. A frozen backend runs this as ``--plugin-host``.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

from .. import __version__


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="spaceknife-plugin-host")
    ap.add_argument("--pipe", required=True)
    ap.add_argument("--token-file", required=True)
    ap.add_argument("--server-pid", type=int, required=True)
    args = ap.parse_args(argv)

    from spaceknife_sdk import wire
    from spaceknife_sdk._audit import Guard, install
    from spaceknife_sdk._runtime import ChildServer, describe_safely, load_plugin, state_from_init
    from spaceknife_sdk.errors import ManifestError
    from spaceknife_sdk.manifest import load_manifest

    from ..helper.auth import read_token_file
    from ..helper.transport import PLUGIN_PREFIX, connect
    from .limits import limit_self

    token = read_token_file(args.token_file)
    channel = connect(args.pipe, 10.0, PLUGIN_PREFIX)
    if channel.peer_pid != args.server_pid:
        channel.close()
        return 2

    def read_frame() -> tuple[int, bytes]:
        kind, body = channel.read_frame(None, 60.0, wire.MAX_BINARY, wire.MAX_JSON)
        return kind, bytes(body)

    def write(parts: list[bytes]) -> None:
        channel.send(parts, 60.0)

    write(wire.frame_json(wire.Hello(token=token, pid=os.getpid(), sdk=__version__)))
    kind, body = read_frame()
    init = json.loads(body)
    if kind != wire.KIND_JSON or init.get("type") != "init":
        return 3
    missing = limit_self(int(init.get("memory_mb", 1024)), server_pid=args.server_pid)
    state = state_from_init(init)
    server = ChildServer(read_frame, write)
    folder = Path(str(init["dir"]))
    guard = Guard(
        plugin_dir=str(folder),
        storage=str(state.plugin.storage),
        granted=state.granted,
        roots=state.roots,
        net_hosts=state.net_hosts,
        programs=state.exec,
    )
    server.guard = guard
    sys.path[:] = [p for p in sys.path if p]  # no current folder
    try:
        manifest = load_manifest(folder)
        install(guard)
        for msg in missing:
            server.send({"type": "log", "level": "WARNING", "message": f"limit not set: {msg}"})
        plugin = load_plugin(folder, manifest)
    except ManifestError as exc:
        server.send(
            {
                "type": "ready",
                "error": {
                    "code": "invalid",
                    "message": str(exc),
                    "issues": [i.as_dict() for i in exc.issues],
                },
            }
        )
        server.close_output()
        return 1
    except BaseException as exc:  # the plugin's import failed
        import traceback

        server.send(
            {
                "type": "ready",
                "error": {
                    "code": "import",
                    "message": f"{type(exc).__name__}: {exc}",
                    "trace": traceback.format_exc()[-4000:],
                },
            }
        )
        server.close_output()
        return 1
    desc, err = describe_safely(plugin)
    if desc is None:
        server.send({"type": "ready", "error": err})
        server.close_output()
        return 1
    state.catalog = dict(desc.get("catalog") or {})
    server.start(plugin, state)
    server.send({"type": "ready", "description": desc})
    return server.serve()


if __name__ == "__main__":
    sys.exit(main())
