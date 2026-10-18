from __future__ import annotations

import re

from spaceknife_protocol.methods import EVENTS, METHODS, NOTIFICATIONS
from spaceknife_protocol.schema import canonical_json, export_schema, schema_hash


def test_schema_is_deterministic_and_hashed() -> None:
    assert canonical_json(export_schema()) == canonical_json(export_schema())
    assert re.fullmatch(r"[0-9a-f]{16}", schema_hash())


def test_schema_covers_every_spec_and_refs_resolve() -> None:
    doc = export_schema()
    assert set(doc["methods"]) == set(METHODS)
    assert set(doc["notifications"]) == set(NOTIFICATIONS)
    assert set(doc["events"]) == set(EVENTS)
    defs = doc["$defs"]
    refs = [m["params"] for m in doc["methods"].values()] + [m["result"] for m in doc["methods"].values()]
    refs += [e["payload"] for e in doc["events"].values()] + [doc["binaryFrameHeader"], doc["errorObject"]]
    for ref in refs:
        assert ref.startswith("#/$defs/")
        assert ref.removeprefix("#/$defs/") in defs
