"""Test plugin: every attempt must be stopped (it has no permissions)."""

from __future__ import annotations

import os
import tempfile
from collections.abc import Callable

from spaceknife_sdk import Context, Params, Payload, Plugin

plugin = Plugin()


class Try(Params):
    what: str
    target: str = ""


class Tried(Payload):
    blocked: bool
    error: str = ""


def _net(p: Try) -> object:
    import socket

    return socket.create_connection(("192.0.2.1", 9), timeout=0.5)


def _resolve(p: Try) -> object:
    import socket

    return socket.getaddrinfo("example.com", 80)


def _write(p: Try) -> object:
    with open(p.target, "w") as fh:
        return fh.write("x")


def _delete(p: Try) -> object:
    os.remove(p.target)
    return True


def _spawn(p: Try) -> object:
    import subprocess

    return subprocess.run(["echo", "hi"], check=False)


def _ctypes(p: Try) -> object:
    import ctypes

    return ctypes.CDLL(None)


def _read(p: Try) -> object:
    with open(p.target, "rb") as fh:
        return fh.read(1)


def _temp(p: Try) -> object:
    with tempfile.NamedTemporaryFile("w") as fh:
        return fh.write("allowed")


ACTIONS: dict[str, Callable[[Try], object]] = {
    "net": _net,
    "resolve": _resolve,
    "write": _write,
    "delete": _delete,
    "spawn": _spawn,
    "ctypes": _ctypes,
    "read": _read,
    "temp": _temp,
}


@plugin.method
async def attempt(ctx: Context, p: Try) -> Tried:
    try:
        if p.what == "tree":
            ctx.tree.query()
        elif p.what == "execute":
            ctx.call("basket.execute", {"mode": "trash"})
        elif p.what == "settings":
            ctx.call("settings.update", {"values": {}})
        elif p.what == "basket":
            ctx.call("basket.add", {"scan_id": "x", "nodes": [1]})
        elif p.what == "files":
            ctx.files.read(p.target)
        else:
            ACTIONS[p.what](p)
    except Exception as exc:  # what stopped it
        return Tried(blocked=True, error=f"{type(exc).__name__}: {exc}")
    return Tried(blocked=False)
