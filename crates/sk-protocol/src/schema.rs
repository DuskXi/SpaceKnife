//! JSON Schema export of protocol v1 (source for the TypeScript codegen and the handshake hash).
//!
//! Every type is described as the server *deserialises* it: params types as they are validated,
//! and payload types with every field that has a default marked optional (the TypeScript
//! generator treats every field of an output type as present, since the server always sends all).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use schemars::generate::SchemaSettings;
use schemars::{Schema, SchemaGenerator};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::PROTOCOL_VERSION;
use crate::binary::BinaryFrameHeader;
use crate::envelope::ErrorObject;
use crate::methods::{EVENTS, METHODS, NOTIFICATIONS};

/// The generator every protocol schema is built with.
pub fn generator() -> SchemaGenerator {
    SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
}

fn reference(s: Schema) -> Value {
    s.as_object()
        .and_then(|o| o.get("$ref"))
        .cloned()
        .unwrap_or_else(|| s.to_value())
}

/// One JSON document describing every method, notification, event and frame header.
pub fn export_schema() -> Value {
    let mut g = generator();
    let error_object = reference(g.subschema_for::<ErrorObject>());
    let frame_header = reference(g.subschema_for::<BinaryFrameHeader>());
    let mut methods = BTreeMap::new();
    for m in METHODS {
        let params = reference((m.params)(&mut g));
        let result = reference((m.result)(&mut g));
        methods.insert(
            m.name,
            json!({ "params": params, "result": result, "requiresSession": m.requires_session, "doc": m.doc }),
        );
    }
    let mut notifications = BTreeMap::new();
    for n in NOTIFICATIONS {
        notifications.insert(
            n.name,
            json!({ "params": reference((n.params)(&mut g)), "doc": n.doc }),
        );
    }
    let mut events = BTreeMap::new();
    for e in EVENTS {
        events.insert(
            e.name,
            json!({ "payload": reference((e.payload)(&mut g)), "doc": e.doc }),
        );
    }
    let defs: Map<String, Value> = g.take_definitions(true);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "SpaceKnife protocol",
        "protocol": PROTOCOL_VERSION,
        "methods": methods,
        "notifications": notifications,
        "events": events,
        "binaryFrameHeader": frame_header,
        "errorObject": error_object,
        "$defs": defs,
    })
}

/// Compact JSON with object keys sorted (whatever map type serde_json was built with).
pub fn canonical_json(doc: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string"));
                    out.push(':');
                    write(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(doc, &mut out);
    out
}

static HASH: LazyLock<String> = LazyLock::new(|| {
    let digest = Sha256::digest(canonical_json(&export_schema()).as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
});

/// Short stable fingerprint of the schema (16 hex digits), exchanged in `session.hello`.
pub fn schema_hash() -> &'static str {
    &HASH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_deterministic_and_hashed() {
        assert_eq!(
            canonical_json(&export_schema()),
            canonical_json(&export_schema())
        );
        let h = schema_hash();
        assert_eq!(h.len(), 16);
        assert!(
            h.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn schema_covers_every_spec_and_refs_resolve() {
        let doc = export_schema();
        let names = |k: &str| {
            doc[k]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(names("methods").len(), METHODS.len());
        assert_eq!(names("notifications").len(), NOTIFICATIONS.len());
        assert_eq!(names("events").len(), EVENTS.len());
        let defs = doc["$defs"].as_object().unwrap();
        let mut refs: Vec<&Value> = Vec::new();
        for m in doc["methods"].as_object().unwrap().values() {
            refs.push(&m["params"]);
            refs.push(&m["result"]);
        }
        for e in doc["events"].as_object().unwrap().values() {
            refs.push(&e["payload"]);
        }
        refs.push(&doc["binaryFrameHeader"]);
        refs.push(&doc["errorObject"]);
        for r in refs {
            let r = r.as_str().unwrap_or_else(|| panic!("not a reference: {r}"));
            let name = r.strip_prefix("#/$defs/").unwrap();
            assert!(defs.contains_key(name), "{name}");
        }
    }
}
