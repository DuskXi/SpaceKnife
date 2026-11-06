//! Base types shared by every message.
//!
//! Two flavours exist on purpose, as in the Python models:
//!
//! * *Params* — payloads the server receives. Unknown fields are rejected
//!   (`#[serde(deny_unknown_fields)]`) so typos and version skew surface immediately; constraints are
//!   checked by [`Validate`](crate::Validate).
//! * *Payloads* — results and events the clients receive. Unknown fields are ignored so an older
//!   client keeps working when a newer server adds optional fields. Every field is always
//!   serialised (`None` as `null`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::validate::Validate;

/// Timestamps on the wire: ISO 8601 / RFC 3339 in UTC (`2025-01-02T03:04:05.123456Z`).
pub type DateTime = chrono::DateTime<chrono::Utc>;

/// Params for methods that take no arguments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

impl Validate for Empty {}

/// Result for methods that return nothing but an acknowledgement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmptyResult {}

pub(crate) fn yes() -> bool {
    true
}
