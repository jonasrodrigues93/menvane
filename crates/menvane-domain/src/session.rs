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
    }
}
