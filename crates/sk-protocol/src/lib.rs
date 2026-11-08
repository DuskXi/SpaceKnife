//! SpaceKnife WebSocket protocol v1.
//!
//! Single source of truth for every message exchanged between the backend and the frontends; see
//! `docs/protocol.md` for the wire-level description.
//!
//! * [`envelope`] — JSON-RPC 2.0 text frames: [`parse_message`], typed `encode_*` helpers.
//! * [`binary`] — the binary frame codec ([`binary::FrameBuilder`], [`binary::decode_frame`]).
//! * [`methods`] — method / notification / event names, marker types and the registry.
//! * per-area message types: [`session`], [`system`], [`streams`], [`scan`], [`io`], [`machine`],
//!   [`filter`], [`view`], [`palette`], [`settings`], [`ops`], [`compare`], [`classes`], [`export`],
//!   [`update`].
//! * [`filter_syntax`] — the filter language parser / normaliser.
//! * [`validate`] — params validation ([`Validate`], [`parse_params`], [`ParamsError`]).
//! * [`schema`] / [`ts`] — JSON Schema export, schema hash, TypeScript generation.
//!
//! Params types (client → server) reject unknown fields; payload types (server → client) ignore
//! them and always serialise every field (`None` as `null`).
#![allow(clippy::derivable_impls)]

mod macros;

pub mod base;
pub mod binary;
pub mod classes;
pub mod compare;
pub mod envelope;
pub mod errors;
pub mod export;
pub mod filter;
pub mod filter_syntax;
pub mod io;
pub mod machine;
pub mod methods;
pub mod ops;
pub mod palette;
pub mod scan;
pub mod schema;
pub mod session;
pub mod settings;
pub mod streams;
pub mod system;
pub mod ts;
pub mod update;
pub mod validate;
pub mod view;

pub use base::{DateTime, Empty, EmptyResult};
pub use envelope::{
    ErrorObject, ErrorResponse, MAX_SAFE_INTEGER, Message, Notification, Request, RequestId,
    SuccessResponse, dump_message, encode_error, encode_notification, encode_request,
    encode_response, parse_message,
};
pub use errors::{CloseCode, ErrorCode, ProtocolError};
pub use validate::{ParamsError, Validate, parse_params, parse_params_str};

pub const PROTOCOL_VERSION: &str = "1.1";
pub const SUBPROTOCOL: &str = "spaceknife.v1";
pub const WS_PATH: &str = "/v1";

// Capabilities a server may advertise; the handshake result carries the intersection.
pub const CAP_BINARY_FRAMES: &str = "binary-frames.v1";
pub const CAP_SUBSCRIPTIONS: &str = "subscriptions";
pub const CAP_CANCEL: &str = "cancel";
pub const CAP_LOG_STREAM: &str = "log-stream";
/// Visualizations: the `visual` view setting, `visual.list`, FrameInfo.visual/layout/geometry/root,
/// the `branch` colour mode and column. Each non-treemap layout is negotiated on its own.
pub const CAP_VISUALS: &str = "visuals.v1";
pub const CAP_LAYOUT_FLAME: &str = "layout.flame.v1";
pub const CAP_LAYOUT_POLAR: &str = "layout.polar.v1";
/// Update checks against the GitHub releases: update.* and the update.changed event.
pub const CAP_UPDATES: &str = "updates.v1";
pub const ALL_CAPABILITIES: [&str; 8] = [
    CAP_BINARY_FRAMES,
    CAP_SUBSCRIPTIONS,
    CAP_CANCEL,
    CAP_LOG_STREAM,
    CAP_VISUALS,
    CAP_LAYOUT_FLAME,
    CAP_LAYOUT_POLAR,
    CAP_UPDATES,
];

/// Major component of a `"<major>.<minor>"` version string (`None` if it does not parse).
pub fn protocol_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}
