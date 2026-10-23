"""A plugin's own protocol schema: its methods, events, states and streams (``plugin build``).

The same shape as the core schema (``spaceknife schema``), for one plugin: every model under
``$defs`` with the plugin's id as a PascalCase prefix (``ExampleDupesFindParams``), so it can sit
beside the core schema and other plugins' without a clash, and the methods, events and states by
their local names pointing at them. ``web/sk-plugin.d.ts`` is generated from it
(:func:`spaceknife_sdk.tsgen.generate_plugin`); ``generated/schema.json`` keeps it.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Mapping
from typing import Any

from .plugin import Description

SCHEMA_VERSION = 1


def prefix_of(plugin: str) -> str:
    """``example.dupes`` -> ``ExampleDupes``; ``acme.zip-source`` -> ``AcmeZipSource``."""
    return "".join(part[:1].upper() + part[1:] for part in re.split(r"[.\-_]", plugin) if part)


class _Defs:
    """Collects models, renaming each with the prefix (a second model of the same name that differs
    gets a number)."""

    def __init__(self, prefix: str) -> None:
        self.prefix = prefix
        self.defs: dict[str, Any] = {}

    def _rename(self, node: Any, names: Mapping[str, str]) -> Any:
        if isinstance(node, dict):
            out: dict[str, Any] = {}
            for k, v in node.items():
                if k == "$ref" and isinstance(v, str) and v.startswith("#/$defs/"):
                    out[k] = "#/$defs/" + names.get(v[8:], self.prefix + v[8:])
                elif k == "$defs":
                    continue
                else:
                    out[k] = self._rename(v, names)
            return out
        if isinstance(node, list):
            return [self._rename(v, names) for v in node]
        return node

    def _put(self, name: str, body: Any) -> str:
        want = self.prefix + name
        final, n = want, 2
        while final in self.defs and self.defs[final] != body:
            final, n = f"{want}{n}", n + 1
        self.defs[final] = body
        return final

    def add(self, schema: Mapping[str, Any], fallback: str) -> str:
        """Hoist a model's schema (and its nested ``$defs``); returns the ``$ref`` to it."""
        nested: dict[str, Any] = dict(schema.get("$defs") or {})
        names: dict[str, str] = {}
        for sub in nested:  # nested models first, so references inside resolve to the final names
            names[sub] = self.prefix + sub
        for sub, body in nested.items():
            names[sub] = self._put(sub, self._rename(body, names))
        title = str(schema.get("title") or fallback)
        body = self._rename({k: v for k, v in schema.items() if k != "$defs"}, names)
        return "#/$defs/" + self._put(re.sub(r"[^A-Za-z0-9_]", "", title) or fallback, body)


def export_plugin_schema(description: Description) -> dict[str, Any]:
    """The plugin's schema (JSON-ready, keys sorted when dumped)."""
    defs = _Defs(prefix_of(description.id))
    methods: dict[str, Any] = {}
    for m in description.methods:
        if not m.public or m.kind != "method":  # reports and cleanups: the host calls them
            continue
        entry: dict[str, Any] = {"kind": m.kind}
        if m.params:
            entry["params"] = defs.add(m.params, f"{m.name.title()}Params")
        if m.result:
            entry["result"] = defs.add(m.result, f"{m.name.title()}Result")
        if m.doc:
            entry["doc"] = m.doc
        methods[m.name] = entry
    events = {e.name: {"payload": defs.add(e.schema_, e.name.title())} for e in description.events}
    states = {
        s.name: {"payload": defs.add(s.schema_, s.name.title()), "default": s.default}
        for s in description.states
    }
    streams = {
        s.name: ({"params": defs.add(s.params, f"{s.name.title()}Params")} if s.params else {})
        for s in description.streams
    }
    out: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "plugin": description.id,
        "version": description.version,
        "api": description.api,
        "methods": methods,
        "events": events,
        "states": states,
        "streams": streams,
        "$defs": defs.defs,
    }
    out["schema_hash"] = schema_hash(out)
    return out


def schema_hash(schema: Mapping[str, Any]) -> str:
    body = {k: v for k, v in schema.items() if k not in ("schema_hash", "version")}
    text = json.dumps(body, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def dumps(schema: Mapping[str, Any]) -> str:
    """The text of ``generated/schema.json``."""
    return json.dumps(schema, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


__all__ = ["SCHEMA_VERSION", "dumps", "export_plugin_schema", "prefix_of", "schema_hash"]
