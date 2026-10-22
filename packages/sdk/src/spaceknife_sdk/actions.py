"""Actions: what a command, a report button or a table row does when used.

Written as one key (``{ open = "report:dupes" }``, ``{ cleanup = "extra_copies" }``) in TOML and
Python alike. :func:`normalize_action` checks the short form and gives ``{"kind": …, …}`` with local
names; the host resolves those to wire names (:func:`resolve_action`) when it registers the plugin.
"""

from __future__ import annotations

from typing import Any

from pydantic import TypeAdapter, ValidationError

from spaceknife_protocol.plugins import (
    ACTION_KINDS,
    Action,
    full_id,
    valid_local_name,
    wire_name,
)

NODE_OPS = ("reveal", "open", "terminal", "copy_path", "select", "zoom", "basket")
TOGGLES = ("show_free", "show_unknown", "show_trash", "sort_by_size")
VIEW_KEYS = (
    "detail",
    "min_element",
    "sort_by_size",
    "show_free",
    "show_unknown",
    "show_trash",
    "visual",
    "color_mode",
    "filter_mode",
    "direction",
)
_ACTION: TypeAdapter[Action] = TypeAdapter(Action)


class ActionError(ValueError):
    pass


def normalize_action(raw: Any) -> dict[str, Any]:
    """The short form (one key naming the kind) or the full form (``kind``) -> the full form."""
    if not isinstance(raw, dict) or not raw:
        raise ActionError("an action is a table with one key: call, open, command, view, …")
    if "kind" in raw:
        kind = raw["kind"]
        if kind not in ACTION_KINDS:
            raise ActionError(f"unknown action kind {kind!r}")
        out = dict(raw)
    else:
        keys = [k for k in raw if k in ACTION_KINDS]
        if len(keys) != 1:
            raise ActionError(f"an action names exactly one of {', '.join(ACTION_KINDS)}; got {sorted(raw)}")
        kind = keys[0]
        value = raw[kind]
        rest = {k: v for k, v in raw.items() if k != kind}
        out = _short(kind, value, rest)
    _check(out)
    return out


def _short(kind: str, value: Any, rest: dict[str, Any]) -> dict[str, Any]:
    def extra(*allowed: str) -> None:
        bad = sorted(set(rest) - set(allowed))
        if bad:
            raise ActionError(f"{kind} action: unknown keys {bad}")

    if kind == "call":
        extra("args")
        return {"kind": "call", "method": value, "args": dict(rest.get("args") or {})}
    if kind == "open":
        extra()
        return {"kind": "open", "target": value}
    if kind == "command":
        extra()
        return {"kind": "command", "command": value}
    if kind == "view":
        extra()
        return {"kind": "view", "settings": dict(value or {})}
    if kind == "cycle":
        extra("step")
        return {"kind": "cycle", "what": value, "step": int(rest.get("step", 1))}
    if kind == "toggle":
        extra()
        return {"kind": "toggle", "name": value}
    if kind == "filter":
        extra()
        return {"kind": "filter", "expr": value}
    if kind == "setting":
        extra("value", "toggle")
        return {
            "kind": "setting",
            "key": value,
            "value": rest.get("value"),
            "toggle": bool(rest.get("toggle")),
        }
    if kind == "cleanup":
        extra()
        return {"kind": "cleanup", "cleanup": value}
    if kind == "node":
        extra()
        return {"kind": "node", "op": value}
    if kind == "url":
        extra()
        return {"kind": "url", "url": value}
    extra()
    return {"kind": "builtin", "name": value}


def _check(a: dict[str, Any]) -> None:
    kind = a["kind"]
    if kind == "call" and not (isinstance(a.get("method"), str) and a["method"]):
        raise ActionError("call: the method's name")
    if kind == "open":
        target = a.get("target")
        if not isinstance(target, str) or not (
            target.startswith(("report:", "panel:")) or target == "settings" or target.startswith("settings:")
        ):
            raise ActionError("open: report:<name>, panel:<name> or settings")
    if kind == "view":
        bad = sorted(set(a.get("settings", {})) - set(VIEW_KEYS))
        if bad:
            raise ActionError(f"view: unknown view settings {bad}")
    if kind == "cycle" and a.get("what") not in ("visual", "color"):
        raise ActionError("cycle: visual or color")
    if kind == "toggle" and a.get("name") not in TOGGLES:
        raise ActionError(f"toggle: one of {', '.join(TOGGLES)}")
    if kind == "node" and a.get("op") not in NODE_OPS:
        raise ActionError(f"node: one of {', '.join(NODE_OPS)}")
    if kind == "url" and not str(a.get("url", "")).startswith("https://"):
        raise ActionError("url: only https:// addresses")
    if kind == "setting" and a.get("value") is None and not a.get("toggle"):
        raise ActionError("setting: a value, or toggle = true")


def _local_ref(ref: str, plugin: str) -> str:
    """A local name -> its full id; a dotted name is already a full id."""
    return full_id(plugin, ref) if valid_local_name(ref) else ref


def resolve_action(norm: dict[str, Any], plugin: str) -> Action:
    """Turn a normalized action's local names into wire names for the frontends."""
    a = dict(norm)
    kind = a["kind"]
    if kind == "call":
        method = a["method"]
        a["method"] = wire_name(plugin, method) if valid_local_name(method) else method
    elif kind == "open":
        target = a["target"]
        if target == "settings":
            a["target"] = f"settings:{plugin}"
        else:
            what, _, name = target.partition(":")
            if what in ("report", "panel"):
                a["target"] = f"{what}:{_local_ref(name, plugin)}"
    elif kind == "cleanup":
        name = a["cleanup"]
        a["cleanup"] = _local_ref(name, plugin)
        owner, _, local = a["cleanup"].rpartition(".")
        a["method"] = wire_name(owner, local)
    elif kind == "command":
        a["command"] = _local_ref(a["command"], plugin)
    elif kind == "setting":
        key = a["key"]
        a["key"] = f"plugin.{plugin}.{key}" if valid_local_name(key) else key
    try:
        return _ACTION.validate_python(a)
    except ValidationError as exc:
        raise ActionError(str(exc)) from None


__all__ = ["NODE_OPS", "TOGGLES", "VIEW_KEYS", "ActionError", "normalize_action", "resolve_action"]
