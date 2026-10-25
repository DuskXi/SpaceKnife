"""Acceptance probe for settings changed right after enabling (round 5's ``growthwatch-race``):
a ``start="startup"`` task starts the process as soon as the plugin is enabled, and loading this
module takes a while, so a setting changed right after enabling lands while the process starts."""

from __future__ import annotations

import time
from typing import Any

from spaceknife_sdk import Context, Payload, Plugin, T

time.sleep(0.5)  # a slow start (a packaged app's process starts slower than a development one)

plugin = Plugin()
plugin.setting("rules", T("规则", "Rules"), "none")
plugin.setting("size", T("阈值", "Threshold"), 1)
HEARD: list[dict[str, Any]] = []


class Now(Payload):
    rules: str
    size: int
    heard: list[dict[str, Any]]


@plugin.method
def now(ctx: Context) -> Now:
    return Now(rules=str(ctx.settings.get("rules")), size=int(ctx.settings.get("size")), heard=HEARD)


@plugin.on("settings")
def changed(ctx: Context, changed: list[str]) -> None:
    HEARD.append({"changed": changed, "rules": ctx.settings.get("rules"), "size": ctx.settings.get("size")})


@plugin.background("watch", T("盯着", "Watching"), start="startup")
def watch(ctx: Context) -> None:
    while True:
        ctx.check()
        time.sleep(0.05)
