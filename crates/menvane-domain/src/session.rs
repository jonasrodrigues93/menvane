use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
pub enum NormalizedEventOrigin {
    User,
    System,
    Agent,
    Compaction,
    Tool,
    Importer,
}

impl Default for NormalizedEventOrigin {
    fn default() -> Self {
        Self::User
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NormalizedEventRole {
    UserPrompt,
    SystemPrompt,
    AgentInstruction,
    CompactionSummary,
    ToolMetadata,
    ToolActivity,
    Lifecycle,
}

impl Default for NormalizedEventRole {
    fn default() -> Self {
        Self::UserPrompt
    }
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
pub enum PromptIntentKind {
    RootGoal,
    NewGoal,
    Refinement,
    Constraint,
    Correction,
    FollowUp,
    Operational,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EpisodeState {
    Active,
    Dormant,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntentClassificationSource {
    Deterministic,
    ProviderReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEpisode {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub conversation_key: String,
    pub root_event_id: String,
    pub goal: String,
    pub ordinal: u32,
    pub state: EpisodeState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptIntent {
    pub event_id: String,
    pub episode_id: Uuid,
    pub kind: PromptIntentKind,
    pub confidence: f64,
    pub weight: f64,
    pub classifier_version: String,
    pub source: IntentClassificationSource,
    pub classified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HandoffStatus {
    Active,
    Ready,
    Consumed,
    Completed,
    Stale,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffValidation {
    pub event_id: String,
    pub command: Option<String>,
    pub success: bool,
    pub summary: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskHandoff {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub conversation_key: String,
    pub episode_id: Uuid,
    pub source_session_id: Uuid,
    pub source_client: String,
    pub status: HandoffStatus,
    pub goal: String,
    pub current_state: String,
    pub completed_work: Vec<String>,
    pub pending_work: Vec<String>,
    pub next_action: Option<String>,
    pub blockers: Vec<String>,
    pub changed_files: Vec<String>,
    pub decisions: Vec<String>,
    pub validation: Vec<HandoffValidation>,
    pub relevant_memory_ids: Vec<Uuid>,
    pub source_event_ids: Vec<String>,
    pub git_head: Option<String>,
    pub worktree_state_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceKind {
    Goal,
    Prompt,
    Action,
    Decision,
    Discovery,
    Error,
    Validation,
    CompactionContext,
    UnresolvedQuestion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub event_id: String,
    pub kind: EvidenceKind,
    pub timestamp: DateTime<Utc>,
    pub content: String,
    pub importance: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeEvidencePacket {
    pub episode_id: Uuid,
    pub goal: EvidenceItem,
    pub prompts: Vec<EvidenceItem>,
    pub actions: Vec<EvidenceItem>,
    pub decisions: Vec<EvidenceItem>,
    pub discoveries: Vec<EvidenceItem>,
    pub errors: Vec<EvidenceItem>,
    pub validations: Vec<EvidenceItem>,
    pub files: Vec<String>,
    pub compaction_context: Vec<EvidenceItem>,
    pub unresolved_questions: Vec<EvidenceItem>,
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
}

impl NormalizedEvent {
    pub fn is_user_prompt(&self) -> bool {
        self.kind == NormalizedEventKind::UserPrompt
            && self.origin == NormalizedEventOrigin::User
            && self.role == NormalizedEventRole::UserPrompt
    }

    pub fn is_allowed_evidence(&self) -> bool {
        if matches!(
            self.origin,
            NormalizedEventOrigin::System
                | NormalizedEventOrigin::Agent
                | NormalizedEventOrigin::Compaction
        ) {
            return false;
        }
        !matches!(
            self.role,
            NormalizedEventRole::ToolMetadata | NormalizedEventRole::CompactionSummary
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_and_intent_vocabulary_is_stable() {
        assert_eq!(
            serde_json::to_string(&PromptIntentKind::RootGoal).unwrap(),
            "\"root-goal\""
        );
        assert_eq!(
            serde_json::from_str::<EpisodeState>("\"completed\"").unwrap(),
            EpisodeState::Completed
        );
        assert_eq!(
            serde_json::from_str::<HandoffStatus>("\"consumed\"").unwrap(),
            HandoffStatus::Consumed
        );
        assert_eq!(
            serde_json::to_string(&EvidenceKind::CompactionContext).unwrap(),
            "\"compaction-context\""
        );
    }

    #[test]
    fn legacy_normalized_events_use_compatible_capture_defaults() {
        let event: NormalizedEvent = serde_json::from_value(serde_json::json!({
            "event_id": "legacy",
            "kind": "user-prompt",
            "client": "claude-code",
            "external_session_id": "session",
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/tmp"
        }))
        .unwrap();
        assert!(event.is_user_prompt());
        assert_eq!(event.origin, NormalizedEventOrigin::User);
        assert_eq!(event.role, NormalizedEventRole::UserPrompt);
    }
}
