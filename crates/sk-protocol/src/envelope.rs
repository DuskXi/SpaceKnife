//! Text-frame envelope: JSON-RPC 2.0 shaped requests, responses and notifications.
//!
//! Every text frame on the socket is exactly one of:
//!
//! * [`Request`]         — `{"jsonrpc": "2.0", "id": 1, "method": "...", "params": {...}}`
//! * [`Notification`]    — `{"jsonrpc": "2.0", "method": "...", "params": {...}}` (events, `$/cancel`)
//! * [`SuccessResponse`] — `{"jsonrpc": "2.0", "id": 1, "result": ...}`
//! * [`ErrorResponse`]   — `{"jsonrpc": "2.0", "id": 1 | null, "error": {"code", "message", "data"}}`
//!
//! The envelope is validated here; `params` / `result` stay raw JSON ([`RawValue`]) and are parsed
//! into the per-method types by the receiving side ([`crate::validate::parse_params`]), so a request
//! is parsed exactly once.
//!
//! Outbound, the `encode_*` functions serialise a typed payload straight into the frame text.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::errors::{ErrorCode, ProtocolError};

/// Ints must stay inside the IEEE-754 safe range so JavaScript clients never lose precision.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
pub const JSONRPC_VERSION: &str = "2.0";

// ------------------------------------------------------------------------ request id

/// A request id: a non-negative integer ≤ 2^53−1, or a string of 1–64 characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RequestId {
    Int(u64),
    Str(String),
}

impl From<u64> for RequestId {
    fn from(v: u64) -> Self {
        RequestId::Int(v)
    }
}

impl From<&str> for RequestId {
    fn from(v: &str) -> Self {
        RequestId::Str(v.to_owned())
    }
}

impl From<String> for RequestId {
    fn from(v: String) -> Self {
        RequestId::Str(v)
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Int(v) => write!(f, "{v}"),
            RequestId::Str(s) => f.write_str(s),
        }
    }
}

impl Serialize for RequestId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            RequestId::Int(v) => s.serialize_u64(*v),
            RequestId::Str(v) => s.serialize_str(v),
        }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = RequestId;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a non-negative safe integer or a string of 1-64 characters")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<RequestId, E> {
                if v > MAX_SAFE_INTEGER {
                    return Err(E::custom(format!(
                        "Input should be less than or equal to {MAX_SAFE_INTEGER}"
                    )));
                }
                Ok(RequestId::Int(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<RequestId, E> {
                if v < 0 {
                    return Err(E::custom("Input should be greater than or equal to 0"));
                }
                self.visit_u64(v as u64)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<RequestId, E> {
                let n = v.chars().count();
                if n == 0 {
                    return Err(E::custom("String should have at least 1 character"));
                }
                if n > 64 {
                    return Err(E::custom("String should have at most 64 characters"));
                }
                Ok(RequestId::Str(v.to_owned()))
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for RequestId {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "RequestId".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [
                { "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER },
                { "type": "string", "minLength": 1, "maxLength": 64 }
            ]
        })
    }
}

// ------------------------------------------------------------------------ messages

fn any_or_null(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{}, { "type": "null" }] })
}

/// `{"code", "message", "data"}` of an error response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    // Omitted from the frame when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "any_or_null")]
    pub data: Option<Value>,
}

impl ErrorObject {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code().into(),
            message: message.into(),
            data: None,
        }
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub id: RequestId,
    pub method: String,
    /// The raw params object (`None`: absent or `null`).
    pub params: Option<Box<RawValue>>,
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub method: String,
    pub params: Option<Box<RawValue>>,
}

#[derive(Clone, Debug)]
pub struct SuccessResponse {
    pub id: RequestId,
    pub result: Box<RawValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ErrorResponse {
    pub id: Option<RequestId>,
    pub error: ErrorObject,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Success(SuccessResponse),
    Error(ErrorResponse),
}

fn raw_eq(a: &Option<Box<RawValue>>, b: &Option<Box<RawValue>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.get() == b.get(),
        (None, None) => true,
        _ => false,
    }
}

impl PartialEq for Request {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id && self.method == o.method && raw_eq(&self.params, &o.params)
    }
}

impl PartialEq for Notification {
    fn eq(&self, o: &Self) -> bool {
        self.method == o.method && raw_eq(&self.params, &o.params)
    }
}

impl PartialEq for SuccessResponse {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id && self.result.get() == o.result.get()
    }
}

fn to_raw<T: Serialize + ?Sized>(v: &T) -> Box<RawValue> {
    serde_json::value::to_raw_value(v).expect("protocol values always serialise")
}

impl Request {
    pub fn new<P: Serialize + ?Sized>(
        id: impl Into<RequestId>,
        method: impl Into<String>,
        params: Option<&P>,
    ) -> Self {
        Self {
            id: id.into(),
            method: method.into(),
            params: params.map(to_raw),
        }
    }

    /// Deserialise and validate the params into a method's params type.
    pub fn parse_params<P: serde::de::DeserializeOwned + crate::Validate>(
        &self,
    ) -> Result<P, crate::ParamsError> {
        crate::validate::parse_params(self.params.as_deref())
    }
}

impl Notification {
    pub fn new<P: Serialize + ?Sized>(method: impl Into<String>, params: Option<&P>) -> Self {
        Self {
            method: method.into(),
            params: params.map(to_raw),
        }
    }

    pub fn parse_params<P: serde::de::DeserializeOwned + crate::Validate>(
        &self,
    ) -> Result<P, crate::ParamsError> {
        crate::validate::parse_params(self.params.as_deref())
    }
}

impl SuccessResponse {
    pub fn new<R: Serialize + ?Sized>(id: impl Into<RequestId>, result: &R) -> Self {
        Self {
            id: id.into(),
            result: to_raw(result),
        }
    }
}

impl Serialize for Request {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Request", 4)?;
        st.serialize_field("jsonrpc", JSONRPC_VERSION)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("method", &self.method)?;
        if let Some(p) = &self.params {
            st.serialize_field("params", p)?;
        }
        st.end()
    }
}

impl Serialize for Notification {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Notification", 3)?;
        st.serialize_field("jsonrpc", JSONRPC_VERSION)?;
        st.serialize_field("method", &self.method)?;
        if let Some(p) = &self.params {
            st.serialize_field("params", p)?;
        }
        st.end()
    }
}

impl Serialize for SuccessResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("SuccessResponse", 3)?;
        st.serialize_field("jsonrpc", JSONRPC_VERSION)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("result", &self.result)?;
        st.end()
    }
}

impl Serialize for ErrorResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("ErrorResponse", 3)?;
        st.serialize_field("jsonrpc", JSONRPC_VERSION)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("error", &self.error)?;
        st.end()
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Message::Request(m) => m.serialize(s),
            Message::Notification(m) => m.serialize(s),
            Message::Success(m) => m.serialize(s),
            Message::Error(m) => m.serialize(s),
        }
    }
}

// ------------------------------------------------------------------------ parsing

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(d).map(Some)
}

/// Every member any message may have; `null` and absent are told apart.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire<'a> {
    #[serde(default, borrow, deserialize_with = "present")]
    jsonrpc: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    id: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    method: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    params: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    result: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    error: Option<&'a RawValue>,
}

fn err(msg: impl Into<String>) -> ProtocolError {
    ProtocolError(msg.into())
}

fn extra(field: &str) -> ProtocolError {
    err(format!("Extra inputs are not permitted at {field}"))
}

fn parse_id(raw: &RawValue) -> Result<RequestId, ProtocolError> {
    serde_json::from_str(raw.get()).map_err(|e| err(format!("{e} at id")))
}

fn parse_method(raw: &RawValue) -> Result<String, ProtocolError> {
    let m: String = serde_json::from_str(raw.get())
        .map_err(|_| err("Input should be a valid string at method"))?;
    let n = m.chars().count();
    if n == 0 {
        return Err(err("String should have at least 1 character at method"));
    }
    if n > 128 {
        return Err(err("String should have at most 128 characters at method"));
    }
    Ok(m)
}

fn parse_params_member(raw: Option<&RawValue>) -> Result<Option<Box<RawValue>>, ProtocolError> {
    match raw {
        None => Ok(None),
        Some(r) => {
            let text = r.get();
            if text == "null" {
                Ok(None)
            } else if text.starts_with('{') {
                Ok(Some(r.to_owned()))
            } else {
                Err(err("Input should be a valid dictionary at params"))
            }
        }
    }
}

/// Parse one text frame into an envelope.
///
/// Fails with [`ProtocolError`] if the frame is not valid JSON or not a valid envelope.
pub fn parse_message(raw: &str) -> Result<Message, ProtocolError> {
    if !raw.trim_start().starts_with('{') {
        // Also rejects arrays, which serde would otherwise read as a struct.
        return Err(err(match serde_json::from_str::<de::IgnoredAny>(raw) {
            Ok(_) => "Unable to extract tag using discriminator".to_owned(),
            Err(e) => format!("Invalid JSON: {e}"),
        }));
    }
    let w: Wire<'_> = serde_json::from_str(raw).map_err(|e| {
        if e.is_data() {
            err(e.to_string())
        } else {
            err(format!("Invalid JSON: {e}"))
        }
    })?;
    if let Some(v) = w.jsonrpc
        && v.get() != "\"2.0\""
    {
        return Err(err("Input should be '2.0' at jsonrpc"));
    }
    if let Some(method) = w.method {
        if w.result.is_some() {
            return Err(extra("result"));
        }
        if w.error.is_some() {
            return Err(extra("error"));
        }
        let method = parse_method(method)?;
        let params = parse_params_member(w.params)?;
        return Ok(match w.id {
            Some(id) => Message::Request(Request {
                id: parse_id(id)?,
                method,
                params,
            }),
            None => Message::Notification(Notification { method, params }),
        });
    }
    if w.params.is_some() {
        return Err(extra("params"));
    }
    if let Some(error) = w.error {
        if w.result.is_some() {
            return Err(extra("result"));
        }
        let id = match w.id {
            None => return Err(err("Field required at id")),
            Some(id) if id.get() == "null" => None,
            Some(id) => Some(parse_id(id)?),
        };
        let error: ErrorObject =
            serde_json::from_str(error.get()).map_err(|e| err(format!("{e} at error")))?;
        return Ok(Message::Error(ErrorResponse { id, error }));
    }
    if let Some(result) = w.result {
        let id = match w.id {
            None => return Err(err("Field required at id")),
            Some(id) => parse_id(id)?,
        };
        return Ok(Message::Success(SuccessResponse {
            id,
            result: result.to_owned(),
        }));
    }
    Err(err("Unable to extract tag using discriminator"))
}

/// [`parse_message`] for a frame received as bytes (must be UTF-8).
pub fn parse_message_bytes(raw: &[u8]) -> Result<Message, ProtocolError> {
    let text = std::str::from_utf8(raw).map_err(|e| err(format!("Invalid JSON: {e}")))?;
    parse_message(text)
}

// ------------------------------------------------------------------------ encoding

/// Serialise an envelope to a compact JSON text frame.
///
/// Optional members are omitted rather than sent as `null` (JSON-RPC forbids `"params": null`),
/// except where the spec requires them: `result` is always present on success and `id` is always
/// present (possibly `null`) on errors.
pub fn dump_message(message: &Message) -> String {
    serde_json::to_string(message).expect("envelopes always serialise")
}

#[derive(Serialize)]
struct OutResponse<'a, R: ?Sized> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    result: &'a R,
}

#[derive(Serialize)]
struct OutNotification<'a, P: ?Sized> {
    jsonrpc: &'static str,
    method: &'a str,
    params: &'a P,
}

#[derive(Serialize)]
struct OutRequest<'a, P: ?Sized> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a P>,
}

#[derive(Serialize)]
struct OutErrorObject<'a> {
    code: i64,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a Value>,
}

#[derive(Serialize)]
struct OutError<'a> {
    jsonrpc: &'static str,
    id: Option<&'a RequestId>,
    error: OutErrorObject<'a>,
}

/// `{"jsonrpc":"2.0","id":…,"result":…}` straight from a typed result.
pub fn encode_response<R: Serialize + ?Sized>(id: &RequestId, result: &R) -> String {
    serde_json::to_string(&OutResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result,
    })
    .expect("results always serialise")
}

/// `{"jsonrpc":"2.0","method":…,"params":…}` (an event, or a client notification).
pub fn encode_notification<P: Serialize + ?Sized>(method: &str, params: &P) -> String {
    serde_json::to_string(&OutNotification {
        jsonrpc: JSONRPC_VERSION,
        method,
        params,
    })
    .expect("events always serialise")
}

/// `{"jsonrpc":"2.0","id":…,"method":…,"params":…}` (params omitted when `None`).
pub fn encode_request<P: Serialize + ?Sized>(
    id: &RequestId,
    method: &str,
    params: Option<&P>,
) -> String {
    serde_json::to_string(&OutRequest {
        jsonrpc: JSONRPC_VERSION,
        id,
        method,
        params,
    })
    .expect("params always serialise")
}

/// `{"jsonrpc":"2.0","id":…|null,"error":{"code","message"[,"data"]}}`.
pub fn encode_error(
    id: Option<&RequestId>,
    code: ErrorCode,
    message: &str,
    data: Option<&Value>,
) -> String {
    encode_error_code(id, code.code().into(), message, data)
}

/// [`encode_error`] with a raw code.
pub fn encode_error_code(
    id: Option<&RequestId>,
    code: i64,
    message: &str,
    data: Option<&Value>,
) -> String {
    serde_json::to_string(&OutError {
        jsonrpc: JSONRPC_VERSION,
        id,
        error: OutErrorObject {
            code,
            message,
            data,
        },
    })
    .expect("errors always serialise")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_each_kind() {
        assert!(matches!(
            parse_message(r#"{"jsonrpc":"2.0","id":1,"method":"system.ping"}"#),
            Ok(Message::Request(_))
        ));
        assert!(matches!(
            parse_message(r#"{"jsonrpc":"2.0","method":"$/cancel","params":{"id":1}}"#),
            Ok(Message::Notification(_))
        ));
        assert!(matches!(
            parse_message(r#"{"jsonrpc":"2.0","id":"a","result":null}"#),
            Ok(Message::Success(_))
        ));
        let e =
            parse_message(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}"#)
                .unwrap();
        match e {
            Message::Error(e) => assert_eq!(e.id, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_frames() {
        let too_big = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"x"}}"#,
            MAX_SAFE_INTEGER + 1
        );
        for raw in [
            "not json",
            "[]",
            r#"{"foo": 1}"#,
            r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":""}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"x","extra":true}"#,
            too_big.as_str(),
            r#"{"jsonrpc":"2.0","id":-1,"method":"x"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"x","params":[1,2]}"#,
        ] {
            assert!(parse_message(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn dump_omits_absent_params_but_keeps_required_nulls() {
        let req = Message::Request(Request::new::<Value>(1, "system.ping", None));
        let v: Value = serde_json::from_str(&dump_message(&req)).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc": "2.0", "id": 1, "method": "system.ping"})
        );

        let ok = Message::Success(SuccessResponse::new(1, &Value::Null));
        let v: Value = serde_json::from_str(&dump_message(&ok)).unwrap();
        assert_eq!(v, json!({"jsonrpc": "2.0", "id": 1, "result": null}));

        let e = Message::Error(ErrorResponse {
            id: None,
            error: ErrorObject {
                code: -32600,
                message: "x".into(),
                data: None,
            },
        });
        let v: Value = serde_json::from_str(&dump_message(&e)).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "x"}})
        );

        let e = encode_error(None, ErrorCode::InvalidRequest, "x", None);
        assert_eq!(serde_json::from_str::<Value>(&e).unwrap(), v);
        let r = encode_request::<Value>(&RequestId::Int(1), "system.ping", None);
        assert_eq!(r, r#"{"jsonrpc":"2.0","id":1,"method":"system.ping"}"#);
        assert_eq!(
            encode_response(&"a".into(), &json!({"x": 1})),
            r#"{"jsonrpc":"2.0","id":"a","result":{"x":1}}"#
        );
        assert_eq!(
            encode_notification("e", &json!({})),
            r#"{"jsonrpc":"2.0","method":"e","params":{}}"#
        );
    }

    #[test]
    fn roundtrip() {
        let msg = Message::Notification(Notification::new(
            "log.record",
            Some(&json!({"sub": "s1", "message": "中文日志"})),
        ));
        assert_eq!(parse_message(&dump_message(&msg)).unwrap(), msg);
        let msg = Message::Request(Request::new("abc", "x", Some(&json!({"a": [1, 2]}))));
        assert_eq!(parse_message(&dump_message(&msg)).unwrap(), msg);
    }

    #[test]
    fn missing_jsonrpc_defaults_and_null_params_are_absent() {
        match parse_message(r#"{"id":7,"method":"m","params":null}"#).unwrap() {
            Message::Request(r) => {
                assert_eq!(r.id, RequestId::Int(7));
                assert!(r.params.is_none());
            }
            other => panic!("{other:?}"),
        }
        assert!(parse_message(r#"{"jsonrpc":"2.0","result":1}"#).is_err()); // success without id
        assert!(
            parse_message(r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1,"message":""}}"#)
                .is_err()
        );
        assert!(parse_message(&format!(r#"{{"id":"{}","method":"m"}}"#, "x".repeat(65))).is_err());
    }
}
