"""The TypeScript generator: the core types stay current, and a plugin's own schema becomes its
panels' ``web/sk-plugin.d.ts`` (docs/plugins.md 「生成的类型」)."""

from __future__ import annotations

import gen_ts_protocol as g

SCHEMA = {
    "plugin": "example.dupes",
    "methods": {
        "find": {
            "params": "#/$defs/ExampleDupesFindParams",
            "result": "#/$defs/ExampleDupesFindResult",
            "doc": "Groups of files with the same content.",
        },
        "ping": {"result": "#/$defs/ExampleDupesPong"},
    },
    "events": {"progress": {"payload": "#/$defs/ExampleDupesProgress"}},
    "states": {"status": {"payload": "#/$defs/ExampleDupesStatus"}},
    "$defs": {
        "ExampleDupesFindParams": {
            "properties": {"min_size": {"type": "integer", "default": 1048576}},
            "type": "object",
        },
        "ExampleDupesFindResult": {
            "properties": {
                "groups": {"items": {"items": {"type": "integer"}, "type": "array"}, "type": "array"}
            },
            "required": ["groups"],
            "type": "object",
        },
        "ExampleDupesPong": {"properties": {"ok": {"type": "boolean", "default": True}}, "type": "object"},
        "ExampleDupesProgress": {
            "properties": {"done": {"type": "integer"}, "total": {"type": "integer"}},
            "required": ["done", "total"],
            "type": "object",
        },
        "ExampleDupesStatus": {
            "properties": {"busy": {"type": "boolean", "default": False}},
            "type": "object",
        },
    },
}


def test_core_types_are_current() -> None:
    assert g.main.__name__ == "main"
    schema = __import__("json").loads(g.SCHEMA.read_text(encoding="utf-8"))
    assert g.OUT.read_text(encoding="utf-8") == g.generate(schema), (
        "run: uv run python tools/gen_ts_protocol.py"
    )


def test_a_plugin_schema_becomes_its_panel_types() -> None:
    text = g.generate_plugin(SCHEMA)
    assert text.startswith(g.PLUGIN_HEADER)
    assert 'import type { Sk } from "./_sk/sk";' in text
    # params are inputs (optional fields), results outputs (every field present)
    assert "export interface ExampleDupesFindParams {\n  min_size?: number;\n}" in text
    assert "export interface ExampleDupesPong {\n  ok: boolean;\n}" in text
    assert "  find: { params: ExampleDupesFindParams; result: ExampleDupesFindResult };" in text
    assert "  /** Groups of files with the same content. */\n" in text
    assert "  ping: { params: Record<string, never>; result: ExampleDupesPong };" in text
    assert "export interface Events {\n  progress: ExampleDupesProgress;\n}" in text
    assert "export interface States {\n  status: ExampleDupesStatus;\n}" in text
    assert text.endswith("declare const sk: Sk<Methods, Events, States>;\nexport default sk;\n")


def test_a_plugin_without_code_gets_empty_maps() -> None:
    text = g.generate_plugin({"plugin": "t.decl"})
    for name in ("Methods", "Events", "States"):
        assert f"export interface {name} {{\n}}" in text
