"""Test plugin: a process that dies, a bug, a model of its own built wrongly, a message for the user."""

from __future__ import annotations

import os

from pydantic import Field

from spaceknife_sdk import Context, Payload, Plugin, PluginError, T

plugin = Plugin()


class Ok(Payload):
    ok: bool = True


@plugin.method
async def ping(ctx: Context) -> Ok:
    return Ok()


@plugin.method
async def die(ctx: Context) -> Ok:
    os._exit(3)


@plugin.method
async def boom(ctx: Context) -> Ok:
    raise ValueError("a bug")


class Short(Payload):
    text: str = Field(max_length=5)


@plugin.method
async def misbuilt(ctx: Context) -> Ok:
    Short(text="far too long")  # its own model: its bug, not the caller's params
    return Ok()


@plugin.method
async def refuse(ctx: Context) -> Ok:
    raise PluginError(T("不行", "No way"))
