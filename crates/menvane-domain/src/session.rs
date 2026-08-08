use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NormalizedEventKind {
    SessionStarted,
    UserPrompt,
    ToolCompleted,
    ContextCompacted,
    TurnStopped,
    SessionEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    Open,
    Idle,
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinforcementSignal {
    Retrieved,
    Injected,
    ExplicitlyRead,
    SuccessfullyApplied,
    FailedApplication,
}

impl ReinforcementSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retrieved => "retrieved",
            Self::Injected => "injected",
            Self::ExplicitlyRead => "explicitly_read",
            Self::SuccessfullyApplied => "successfully_applied",
            Self::FailedApplication => "failed_application",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub event_id: String,
    pub kind: NormalizedEventKind,
    pub client: String,
    pub external_session_id: String,
    pub timestamp: DateTime<Utc>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
