//! Session handshake models (`session.hello`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

str_enum! {
    pub enum ClientKind {
        Electron = "electron",
        Qt = "qt",
        Winui = "winui",
        Cli = "cli",
        Test = "test",
        Other = "other",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
    pub kind: ClientKind,
}

impl Validate for ClientInfo {
    fn validate(&self) -> ValidationResult {
        v::str_len("name", &self.name, 1, 64)?;
        v::str_len("version", &self.version, 1, 32)
    }
}

/// First request on every connection; anything else before it closes the socket.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelloParams {
    /// Per-launch secret handed to the client out of band
    pub token: String,
    /// Protocol version the client speaks, e.g. '1.0'
    pub protocol: String,
    pub client: ClientInfo,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl Validate for HelloParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("token", &self.token, 16, 256)?;
        v::pattern("protocol", v::is_version(&self.protocol), r"^\d+\.\d+$")?;
        self.client.validate().map_err(|e| e.at("client"))?;
        v::list_len("capabilities", &self.capabilities, 0, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// The runtime the backend runs on (the Python backend sent its Python version).
    pub python: String,
    pub platform: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Limits {
    pub max_inbound_message_bytes: i64,
    pub max_outbound_chunk_bytes: i64,
    pub hello_timeout_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResult {
    pub protocol: String,
    pub server: ServerInfo,
    pub session_id: String,
    /// Intersection of client and server capabilities
    pub capabilities: Vec<String>,
    pub schema_hash: String,
    pub limits: Limits,
}
