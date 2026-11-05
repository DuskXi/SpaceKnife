//! Disk load: the per-disk I/O governor's state, the W2 load curve and the `io` topic.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::macros::str_enum;
use crate::validate::{self as v, Validate, ValidationResult};

pub const TOPIC_IO: &str = "io";

str_enum! {
    pub enum LoadLevel {
        Max = "max",
        Balanced = "balanced",
        Quiet = "quiet",
        Idle = "idle",
        Custom = "custom",
    }
}

/// One second of the W2 load curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoadPoint {
    /// Unix time of the end of this second
    pub t: f64,
    /// SpaceKnife's own share of the disk load, %
    pub self_pct: f64,
    /// Whole-disk load, %
    pub total_pct: f64,
    /// Task Manager 'active time' of the whole disk, %
    #[serde(default)]
    pub active_pct: f64,
    #[serde(default)]
    pub yielding: bool,
    #[serde(default)]
    pub paused: bool,
}

/// Disk-load state of one physical disk (W2 curve, status bar UI-MAIN-50).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IoStatus {
    pub disk: String,
    pub media: String,
    pub level: LoadLevel,
    /// Limit on SpaceKnife's own load, % (100 = not limited)
    pub limit: f64,
    pub self_pct: f64,
    pub total_pct: f64,
    pub others_pct: f64,
    /// Raw busy time (Task Manager 'active time'), %
    pub active_pct: f64,
    /// Allowed parallel batches
    pub concurrency: i64,
    /// Batches in flight now
    pub inflight: i64,
    /// Share of the allowed parallelism in use, 0..1
    pub duty: f64,
    pub auto: bool,
    pub yielding: bool,
    pub paused: bool,
    /// False: no load data for this disk (open loop)
    pub measured: bool,
    /// Workers should read at OS low I/O priority
    pub low_io_priority: bool,
    #[serde(default)]
    pub q_eff: Option<f64>,
    /// Bytes read per second on the whole disk, last sample
    #[serde(default)]
    pub read_bps: Option<f64>,
    /// Mean time per read on the whole disk, ms
    #[serde(default)]
    pub read_latency_ms: Option<f64>,
    #[serde(default)]
    pub history: Vec<LoadPoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IoStatusList {
    pub disks: Vec<IoStatus>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IoTopicParams {
    /// One disk id; None = every disk
    #[serde(default)]
    pub disk: Option<String>,
}

impl Validate for IoTopicParams {
    fn validate(&self) -> ValidationResult {
        v::opt_str_len("disk", &self.disk, 0, 256)
    }
}

/// `io.load`: a disk's load state, twice a second while someone listens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IoStatusEvent {
    pub sub: String,
    pub status: IoStatus,
}
