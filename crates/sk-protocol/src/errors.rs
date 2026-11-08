//! Error codes (JSON-RPC 2.0 + SpaceKnife extensions, docs/protocol.md §5) and WebSocket close codes.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! int_codes {
    ($(#[$meta:meta])* $vis:vis enum $name:ident : $repr:ty { $($(#[$vm:meta])* $variant:ident = $val:expr),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr($repr)]
        $vis enum $name { $($(#[$vm])* $variant = $val),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub const fn code(self) -> $repr {
                self as $repr
            }

            pub fn from_code(code: $repr) -> Option<Self> {
                match code {
                    $(x if x == $val => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl From<$name> for $repr {
            fn from(v: $name) -> $repr {
                v as $repr
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                (*self as $repr).serialize(s)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let v = <$repr>::deserialize(d)?;
                Self::from_code(v).ok_or_else(|| serde::de::Error::custom(format!("unknown code {v}")))
            }
        }
    };
}

int_codes! {
    /// JSON-RPC error codes.
    pub enum ErrorCode: i32 {
        // JSON-RPC 2.0 standard codes
        ParseError = -32700,
        InvalidRequest = -32600,
        MethodNotFound = -32601,
        InvalidParams = -32602,
        InternalError = -32603,
        // SpaceKnife server errors (-32000 .. -32099)
        Unauthorized = -32001,
        NotHandshaken = -32002,
        ProtocolMismatch = -32003,
        UnknownTopic = -32004,
        UnknownSubscription = -32005,
        ShuttingDown = -32006,
        UnknownScan = -32007,
        ScanFailed = -32008,
        SnapshotError = -32009,
        UnknownNode = -32010,
        ScanBusy = -32011,
        UnknownView = -32012,
        /// The settings were changed by another window: retry with the new version.
        SettingsConflict = -32013,
        /// An item of the basket the user confirmed left it or changed since (another session
        /// added to it): nothing ran, ask again with what it holds now (basket-confirm.v1).
        BasketChanged = -32014,
        /// Same value LSP uses for a request cancelled by the client.
        RequestCancelled = -32800,
    }
}

impl ErrorCode {
    /// The upper-case name used in docs/protocol.md (`INVALID_PARAMS`).
    pub const fn name(self) -> &'static str {
        match self {
            ErrorCode::ParseError => "PARSE_ERROR",
            ErrorCode::InvalidRequest => "INVALID_REQUEST",
            ErrorCode::MethodNotFound => "METHOD_NOT_FOUND",
            ErrorCode::InvalidParams => "INVALID_PARAMS",
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::NotHandshaken => "NOT_HANDSHAKEN",
            ErrorCode::ProtocolMismatch => "PROTOCOL_MISMATCH",
            ErrorCode::UnknownTopic => "UNKNOWN_TOPIC",
            ErrorCode::UnknownSubscription => "UNKNOWN_SUBSCRIPTION",
            ErrorCode::ShuttingDown => "SHUTTING_DOWN",
            ErrorCode::UnknownScan => "UNKNOWN_SCAN",
            ErrorCode::ScanFailed => "SCAN_FAILED",
            ErrorCode::SnapshotError => "SNAPSHOT_ERROR",
            ErrorCode::UnknownNode => "UNKNOWN_NODE",
            ErrorCode::ScanBusy => "SCAN_BUSY",
            ErrorCode::UnknownView => "UNKNOWN_VIEW",
            ErrorCode::SettingsConflict => "SETTINGS_CONFLICT",
            ErrorCode::BasketChanged => "BASKET_CHANGED",
            ErrorCode::RequestCancelled => "REQUEST_CANCELLED",
        }
    }
}

int_codes! {
    /// Application close codes (4000-4999 are reserved for applications by RFC 6455).
    pub enum CloseCode: u16 {
        Normal = 1000,
        GoingAway = 1001,
        ProtocolError = 4400,
        Unauthorized = 4401,
        HandshakeTimeout = 4408,
        VersionMismatch = 4426,
    }
}

/// Raised by codecs when a message or frame does not follow the protocol.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolError(pub String);

impl ProtocolError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_protocol_document() {
        assert_eq!(ErrorCode::InvalidParams.code(), -32602);
        assert_eq!(ErrorCode::SettingsConflict.code(), -32013);
        assert_eq!(ErrorCode::RequestCancelled.code(), -32800);
        assert_eq!(
            serde_json::to_string(&ErrorCode::ParseError).unwrap(),
            "-32700"
        );
        assert_eq!(
            serde_json::from_str::<ErrorCode>("-32010").unwrap(),
            ErrorCode::UnknownNode
        );
        assert_eq!(CloseCode::VersionMismatch.code(), 4426);
        for c in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_code(c.code()), Some(*c));
        }
    }
}
