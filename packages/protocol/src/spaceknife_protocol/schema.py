"""JSON Schema export of protocol v1 (source for TypeScript codegen and the handshake hash)."""

from __future__ import annotations

import hashlib
import json
from typing import Any

from pydantic import BaseModel
from pydantic.json_schema import JsonSchemaMode, models_json_schema

from . import PROTOCOL_VERSION
from .base import Params
from .binary import BinaryFrameHeader
from .envelope import ErrorObject
from .methods import EVENTS, METHODS, NOTIFICATIONS


def _mode(model: type[BaseModel]) -> JsonSchemaMode:
    # Inbound models are described as the server validates them, outbound ones as they are emitted.
    return "validation" if issubclass(model, Params) else "serialization"


def export_schema() -> dict[str, Any]:
    """Return one JSON document describing every method, notification, event and frame header."""
    models: list[type[BaseModel]] = [ErrorObject, BinaryFrameHeader]
    for method in METHODS.values():
        models += [method.params, method.result]
    for note in NOTIFICATIONS.values():
        models.append(note.params)
    for event in EVENTS.values():
        models.append(event.payload)
    unique = list(dict.fromkeys(models))
    refs, top = models_json_schema([(m, _mode(m)) for m in unique], ref_template="#/$defs/{model}")

    def ref(model: type[BaseModel]) -> str:
        return str(refs[(model, _mode(model))]["$ref"])

    doc: dict[str, Any] = {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "SpaceKnife protocol",
        "protocol": PROTOCOL_VERSION,
        "methods": {
            name: {
                "params": ref(spec.params),
                "result": ref(spec.result),
                "requiresSession": spec.requires_session,
                "doc": spec.doc,
            }
            for name, spec in sorted(METHODS.items())
        },
        "notifications": {
            name: {"params": ref(spec.params), "doc": spec.doc}
            for name, spec in sorted(NOTIFICATIONS.items())
        },
        "events": {
            name: {"payload": ref(spec.payload), "doc": spec.doc} for name, spec in sorted(EVENTS.items())
        },
        "binaryFrameHeader": ref(BinaryFrameHeader),
        "errorObject": ref(ErrorObject),
        "$defs": top.get("$defs", {}),
    }
    return doc


def canonical_json(doc: dict[str, Any]) -> str:
    return json.dumps(doc, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def schema_hash() -> str:
    """Short stable fingerprint of the schema, exchanged in ``session.hello``."""
    return hashlib.sha256(canonical_json(export_schema()).encode("utf-8")).hexdigest()[:16]
