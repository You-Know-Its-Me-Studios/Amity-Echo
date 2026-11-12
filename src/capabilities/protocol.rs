//! Stable, provider-independent wire contract for native Echo capabilities.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: &str = "echo.capabilities.v1";
pub const TRAINING_CONTRACT: &str = "echo.capability-training.v1";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequest {
    pub protocol: String,
    pub capability: Option<String>,
    pub action: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Available,
    Activated,
    Running,
    Completed,
    Paused,
    AwaitingApproval,
    AwaitingUserDecision,
    Unavailable,
    Unsupported,
    Failed,
    Cancelled,
}

impl CapabilityStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Activated => "activated",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Paused => "paused",
            Self::AwaitingApproval => "awaiting_approval",
            Self::AwaitingUserDecision => "awaiting_user_decision",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityResponse {
    pub protocol: String,
    pub capability: String,
    pub action: String,
    pub project_id: String,
    pub session_id: String,
    pub task_id: Option<String>,
    pub status: CapabilityStatus,
    pub message: String,
    pub data: Value,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

impl CapabilityResponse {
    pub fn new(status: CapabilityStatus, message: impl Into<String>, data: Value) -> Self {
        Self {
            protocol: PROTOCOL.into(), capability: String::new(), action: String::new(),
            project_id: String::new(), session_id: String::new(), task_id: None,
            status, message: message.into(), data, evidence_refs: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, refs: Vec<String>) -> Self {
        self.evidence_refs = refs;
        self
    }
}
