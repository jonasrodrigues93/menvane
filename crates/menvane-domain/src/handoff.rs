use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_HANDOFF_ITEMS: usize = 100;
pub const MAX_HANDOFF_TEXT_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HandoffItemKind {
    InProgress,
    OpenQuestion,
    Parked,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffItemSource {
    pub session_id: Uuid,
    pub event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffItem {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub kind: HandoffItemKind,
    pub state: String,
    pub next_step: Option<String>,
    pub blocker: Option<String>,
    pub low_confidence: bool,
    pub last_confirmed_at: DateTime<Utc>,
    pub sources: Vec<HandoffItemSource>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewHandoffItem {
    pub kind: HandoffItemKind,
    pub state: String,
    pub next_step: Option<String>,
    pub blocker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffUpdate {
    pub item_id: Uuid,
    pub kind: HandoffItemKind,
    pub state: String,
    pub next_step: Option<String>,
    pub blocker: Option<String>,
    pub evidence_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffTransition {
    pub item_id: Uuid,
    pub text: String,
    pub evidence_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffReplacement {
    pub item_id: Uuid,
    pub replacement: NewHandoffItem,
    pub evidence_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffCreation {
    pub item: NewHandoffItem,
    pub evidence_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HandoffItemOperation {
    Keep { item_id: Uuid },
    Update(HandoffUpdate),
    Resolve(HandoffTransition),
    Discard(HandoffTransition),
    Replace(HandoffReplacement),
    Uncertain { item_id: Uuid },
    Create(HandoffCreation),
}
