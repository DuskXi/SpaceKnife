"""Test plugin: slow calls (cooperative ones, and one that never looks at cancellation)."""

from __future__ import annotations

import asyncio
import time

from spaceknife_sdk import Context, Params, Payload, Plugin

plugin = Plugin()


class Wait(Params):
    seconds: float = 5.0
    tag: str = ""


class Done(Payload):
    tag: str
    cancelled: bool = False


@plugin.method(timeout=0.5)
async def short(ctx: Context, p: Wait) -> Done:
    deadline = time.monotonic() + p.seconds
    while time.monotonic() < deadline:
        ctx.check()
        await asyncio.sleep(0.02)
    return Done(tag=p.tag)


@plugin.method(latest=True)
async def latest(ctx: Context, p: Wait) -> Done:
    deadline = time.monotonic() + p.seconds
    while time.monotonic() < deadline:
        ctx.check()
        await asyncio.sleep(0.02)
    return Done(tag=p.tag)


@plugin.method(timeout=0.3)
def stuck(ctx: Context, p: Wait) -> Done:
    time.sleep(p.seconds)  # never checks: the host has to give up on it
    return Done(tag=p.tag)


@plugin.method(timeout=0.3)
async def blocking(ctx: Context, p: Wait) -> Done:
    time.sleep(p.seconds)  # noqa: ASYNC251 — never awaits: it holds the plugin's event loop
    return Done(tag=p.tag)


@plugin.method
async def quick(ctx: Context) -> Done:
    return Done(tag="quick")
