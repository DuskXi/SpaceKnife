//! Cross-cutting stream machinery: cancellation, subscriptions, progress and the log topic.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::base::DateTime;
use crate::envelope::RequestId;
use crate::macros::str_enum;
use crate::validate::{self as v, ParamsError, Validate, ValidationResult};

pub const TOPIC_LOG: &str = "log";

str_enum! {
    pub enum LogLevel {
        Trace = "TRACE",
        Debug = "DEBUG",
        Info = "INFO",
        Success = "SUCCESS",
        Warning = "WARNING",
        Error = "ERROR",
        Critical = "CRITICAL",
    }
}

/// `$/cancel` notification: cancel the in-flight request with this id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelParams {
    pub id: RequestId,
}

impl Validate for CancelParams {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeParams {
    pub topic: String,
    #[serde(default)]
    pub params: Option<Map<String, Value>>,
    /// Upper bound on event rate
    #[serde(default)]
    pub rate_hz: Option<f64>,
}

impl Validate for SubscribeParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("topic", &self.topic, 1, 64)?;
        if let Some(r) = self.rate_hz {
            v::float_gt_le("rate_hz", r, 0.0, 120.0)?;
        }
        Ok(())
    }
}

impl SubscribeParams {
    /// The topic's own params (`None` → `{}`), parsed and validated.
    pub fn topic_params<P: serde::de::DeserializeOwned + Validate>(
        &self,
    ) -> Result<P, ParamsError> {
        let value = Value::Object(self.params.clone().unwrap_or_default());
        let p: P = serde_json::from_value(value).map_err(|e| ParamsError::from(e).at("params"))?;
        p.validate().map_err(|e| e.at("params"))?;
        Ok(p)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    pub sub: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeParams {
    pub sub: String,
}

impl Validate for UnsubscribeParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("sub", &self.sub, 1, 64)
    }
}

/// `$/progress`: LSP-style progress for long-running requests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressEvent {
    pub token: String,
    pub value: Map<String, Value>,
}

fn default_min_level() -> LogLevel {
    LogLevel::Info
}

fn default_backlog() -> u32 {
    200
}

/// Params for subscribing to the `log` topic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogTopicParams {
    #[serde(default = "default_min_level")]
    pub min_level: LogLevel,
    /// Recent records replayed on subscribe
    #[serde(default = "default_backlog")]
    pub backlog: u32,
}

impl Default for LogTopicParams {
    fn default() -> Self {
        Self {
            min_level: default_min_level(),
            backlog: default_backlog(),
        }
    }
}

impl Validate for LogTopicParams {
    fn validate(&self) -> ValidationResult {
        v::int_range("backlog", self.backlog, 0, 5000)
    }
}

/// `log.record`: one backend log record, pushed to `log` subscribers (W8 log panel).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LogRecordEvent {
    pub sub: String,
    pub seq: i64,
    pub time: DateTime,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}
