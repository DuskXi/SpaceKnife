//! System-level methods: info, ping, shutdown.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::DateTime;
use crate::session::ServerInfo;
use crate::validate::{self as v, Validate, ValidationResult};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PingParams {
    #[serde(default)]
    pub payload: Option<String>,
}

impl Validate for PingParams {
    fn validate(&self) -> ValidationResult {
        v::opt_str_len("payload", &self.payload, 0, 1024)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PingResult {
    #[serde(default)]
    pub payload: Option<String>,
    pub server_time: DateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlatformInfo {
    pub system: String,
    pub release: String,
    pub machine: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SystemInfo {
    pub server: ServerInfo,
    pub pid: i64,
    pub started_at: DateTime,
    pub platform: PlatformInfo,
    pub sessions: i64,
    /// macOS: whether SpaceKnife has Full Disk Access (no consent prompt per protected place); None elsewhere or when it cannot be told
    #[serde(default)]
    pub full_disk_access: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShutdownResult {
    pub accepted: bool,
}
