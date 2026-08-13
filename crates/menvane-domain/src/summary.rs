use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_SUMMARY_ITEMS: usize = 20;
pub const MAX_SUMMARY_TEXT_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SummaryOutcome {
    Completed,
    Advanced,
    Blocked,
    Abandoned,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContinuityDisposition {
    Continues,
    Resolved,
    Discarded,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<Uuid>,
    pub front: String,
    pub disposition: ContinuityDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "kebab-case")]
pub struct EpisodicSummary {
    pub intentions: Vec<String>,
    pub actions: Vec<String>,
    pub outcome: SummaryOutcome,
    pub result: String,
    pub continuity: Vec<ContinuityItem>,
    pub candidate_learnings: Vec<String>,
}
