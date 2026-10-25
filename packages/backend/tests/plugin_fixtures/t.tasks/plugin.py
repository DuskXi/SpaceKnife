"""Test plugin: background tasks (progress, cancel, time limit, a restart after a crash, one that
starts with the process), plain-def calls that outlive their cancel, and paced file reads."""

from __future__ import annotations

import asyncio
import os
import time

from pydantic import Field

from spaceknife_sdk import Context, Params, Payload, Plugin, T

plugin = Plugin()
SEEN: dict[str, object] = {"counted": 0, "langs": [], "startup": 0, "forever": 0}


class Count(Params):
    n: int = 5
    pause: float = 0.02


class Wait(Params):
    seconds: float = 1.0


class Done(Payload):
    tag: str = ""


class Seen(Payload):
    counted: int = 0
    langs: list[str] = Field(default_factory=list)
    startup: int = 0
    forever: int = 0
    pid: int = 0


class Hashed(Payload):
    digest: str
    progress: list[list[int]]
    many: list[str | None]


@plugin.background("count", T("数数", "Counting"))
def count(ctx: Context, p: Count) -> None:
    for i in range(p.n):
        ctx.check()
        ctx.progress(i + 1, p.n, T("第 {i} 个", "Number {i}").format(i=i + 1))
        time.sleep(p.pause)
    SEEN["counted"] = p.n


@plugin.background("forever", T("一直运行", "Runs forever"), restart=True)
def forever(ctx: Context) -> None:
    SEEN["forever"] = int(SEEN["forever"]) + 1  # type: ignore[call-overload]
    while True:
        ctx.check()
        time.sleep(0.02)


@plugin.background("startup", T("启动就运行", "Runs from the start"), start="startup")
async def startup(ctx: Context) -> None:
    SEEN["startup"] = int(SEEN["startup"]) + 1  # type: ignore[call-overload]
    await asyncio.Event().wait()  # until it is cancelled (an await: cancelling stops it at once)


@plugin.background("lang", T("跟着语言", "Follows the language"))
def lang(ctx: Context) -> None:
    langs: list[str] = []
    SEEN["langs"] = langs
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        ctx.check()
        if not langs or langs[-1] != ctx.lang:
            langs.append(ctx.lang)
            ctx.progress(len(langs), 0, T("中文", "English"))
        if len(langs) >= 2:
            return
        time.sleep(0.02)


@plugin.background("short", T("有时限", "Has a time limit"), timeout=0.3)
def short(ctx: Context) -> None:
    while True:
        ctx.check()
        time.sleep(0.02)


@plugin.method
def seen(ctx: Context) -> Seen:
    return Seen.model_validate({**SEEN, "pid": os.getpid()})


@plugin.method
def start_count(ctx: Context, p: Count) -> Done:
    info = ctx.tasks.start("count", p, key="k")
    again = ctx.tasks.start("count", p, key="k")  # the same one: one per name and key
    assert again.id == info.id
    return Done(tag=info.id)


@plugin.method
async def die(ctx: Context) -> Done:
    os._exit(3)


@plugin.method(timeout=0.05)
def slow(ctx: Context, p: Wait) -> Done:
    time.sleep(p.seconds)  # never checks: cancelled at 50 ms, it goes on on a spare thread
    return Done(tag="slow")


@plugin.method
def quick(ctx: Context) -> Done:
    return Done(tag="quick")


@plugin.method
def hashing(ctx: Context) -> Hashed:
    big = next(n for n in ctx.tree.query(kind="file", columns=("bytes",), sort="-bytes", limit=1).ids)
    steps: list[list[int]] = []
    digest = ctx.files.hash(int(big), progress=lambda done, total: steps.append([done, total]))
    files = [int(i) for i in ctx.tree.query(kind="file", columns=()).ids]
    return Hashed(digest=digest, progress=steps, many=ctx.files.hash_many(files, progress=True))
