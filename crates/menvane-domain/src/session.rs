use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::summary::EpisodicSummary;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NormalizedEventOrigin {
    #[default]
    User,
    System,
    Agent,
    Compaction,
    Tool,
    Importer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NormalizedEventRole {
    #[default]
    UserPrompt,
    SystemPrompt,
    AgentInstruction,
    CompactionSummary,
    ToolMetadata,
    ToolActivity,
    Lifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    Open,
    Idle,
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SummaryStatus {
    Pending,
    Ready,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetadata {
    pub id: Uuid,
    pub client: String,
    pub external_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub imported: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation: u32,
    pub summary_status: SummaryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<EpisodicSummary>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinforcementSignal {
    Retrieved,
    Injected,
    McpRead,
    SuccessfullyApplied,
    FailedApplication,
}

impl ReinforcementSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retrieved => "retrieved",
            Self::Injected => "injected",
            Self::McpRead => "mcp_read",
            Self::SuccessfullyApplied => "successfully_applied",
            Self::FailedApplication => "failed_application",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedEvent {
    pub event_id: String,
    pub kind: NormalizedEventKind,
    #[serde(default)]
    pub origin: NormalizedEventOrigin,
    #[serde(default)]
    pub role: NormalizedEventRole,
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub harness_injected: bool,
}

impl NormalizedEvent {
    pub fn is_user_prompt(&self) -> bool {
        !self.harness_injected
            && self.kind == NormalizedEventKind::UserPrompt
            && self.origin == NormalizedEventOrigin::User
            && self.role == NormalizedEventRole::UserPrompt
    }

    pub fn is_injected_content(&self) -> bool {
        self.harness_injected
            || matches!(
                self.origin,
                NormalizedEventOrigin::System
                    | NormalizedEventOrigin::Agent
                    | NormalizedEventOrigin::Compaction
            )
            || matches!(
                self.role,
                NormalizedEventRole::SystemPrompt
                    | NormalizedEventRole::AgentInstruction
                    | NormalizedEventRole::CompactionSummary
                    | NormalizedEventRole::ToolMetadata
            )
    }

    pub fn is_operational(&self) -> bool {
        self.is_injected_content() || self.role == NormalizedEventRole::Lifecycle
    }

    pub fn is_durable(&self) -> bool {
        !self.is_injected_content()
    }

    pub fn is_consolidation_eligible(&self) -> bool {
        self.is_durable()
            && matches!(
                self.kind,
                NormalizedEventKind::UserPrompt | NormalizedEventKind::ToolCompleted
            )
    }

    pub fn is_allowed_evidence(&self) -> bool {
        self.is_consolidation_eligible()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSession {
    pub client: String,
    pub external_session_id: String,
    pub cwd: Option<String>,
    pub events: Vec<NormalizedEvent>,
    pub estimated_bytes: u64,
}

pub trait SessionImporter {
    fn discover(&self) -> Result<Vec<NormalizedSession>, String>;
}
