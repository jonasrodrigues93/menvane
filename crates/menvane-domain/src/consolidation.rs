use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::handoff::{
    HandoffItem, HandoffItemOperation, MAX_HANDOFF_ITEMS, MAX_HANDOFF_TEXT_CHARS, NewHandoffItem,
};
use crate::memory::{Applicability, KnowledgeType, MemoryStatus, Scope};
use crate::session::NormalizedEvent;
use crate::summary::{EpisodicSummary, MAX_SUMMARY_ITEMS, MAX_SUMMARY_TEXT_CHARS};

pub const MAX_KNOWLEDGE_OPERATIONS: usize = 10;
pub const MAX_RELATED_SUMMARIES: usize = 5;
pub const MAX_RELATED_MEMORIES: usize = 50;
pub const MAX_KNOWLEDGE_BODY_CHARS: usize = 8_000;
pub const GLOBAL_SCOPE_CONFIDENCE_THRESHOLD: f64 = 0.9;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedSummary {
    pub session_id: Uuid,
    pub ended_at: Option<DateTime<Utc>>,
    pub summary: EpisodicSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedMemory {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub knowledge_type: KnowledgeType,
    pub scope: Scope,
    pub status: MemoryStatus,
    pub title: String,
    pub body: String,
    pub source_sessions: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationPacket {
    pub session_id: Uuid,
    pub events: Vec<NormalizedEvent>,
    pub handoff_items: Vec<HandoffItem>,
    pub related_summaries: Vec<RelatedSummary>,
    pub related_memories: Vec<RelatedMemory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnowledgeOperationKind {
    Create,
    Reinforce,
    Merge,
    Supersede,
    NoOp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybookContent {
    pub trigger: String,
    pub applicability: Applicability,
    pub steps: Vec<String>,
    pub validation: Vec<String>,
    pub failure_handling: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextContent {
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnowledgeContent {
    Context(ContextContent),
    Playbook(PlaybookContent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeOperation {
    pub operation: KnowledgeOperationKind,
    pub target_memory_ids: Vec<Uuid>,
    pub knowledge_type: Option<KnowledgeType>,
    pub title: Option<String>,
    pub scope: Option<Scope>,
    pub scope_confidence: Option<f64>,
    pub applies_to: Applicability,
    pub content: Option<KnowledgeContent>,
    pub evidence_event_ids: Vec<String>,
    pub contradicting_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationResult {
    pub summary: EpisodicSummary,
    pub handoff: Vec<HandoffItemOperation>,
    pub knowledge: Vec<KnowledgeOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationExecution {
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
    pub attempts: u32,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub credits: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationValidationError(pub String);

impl std::fmt::Display for ConsolidationValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConsolidationValidationError {}

pub fn validate_consolidation_result(
    packet: &ConsolidationPacket,
    result: &ConsolidationResult,
) -> Result<(), ConsolidationValidationError> {
    validate_summary(&result.summary)?;
    if result.handoff.len() > packet.handoff_items.len() + 20 {
        return Err(ConsolidationValidationError(
            "too many handoff operations".into(),
        ));
    }
    if result.knowledge.len() > MAX_KNOWLEDGE_OPERATIONS {
        return Err(ConsolidationValidationError(
            "too many knowledge operations".into(),
        ));
    }
    let item_ids = packet
        .handoff_items
        .iter()
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    if packet.handoff_items.len() > MAX_HANDOFF_ITEMS {
        return Err(ConsolidationValidationError(
            "handoff contains too many items".into(),
        ));
    }
    let mut destinations = HashSet::new();
    for operation in &result.handoff {
        let item_id = match operation {
            HandoffItemOperation::Keep { item_id }
            | HandoffItemOperation::Uncertain { item_id } => *item_id,
            HandoffItemOperation::Update(value) => value.item_id,
            HandoffItemOperation::Resolve(value) | HandoffItemOperation::Discard(value) => {
                value.item_id
            }
            HandoffItemOperation::Replace(value) => value.item_id,
            HandoffItemOperation::Create(_) => continue,
        };
        if !item_ids.contains(&item_id) || !destinations.insert(item_id) {
            return Err(ConsolidationValidationError(
                "handoff operation references an invalid or repeated item".into(),
            ));
        }
        validate_handoff_operation(operation, packet)?;
    }
    if destinations.len() != item_ids.len() {
        return Err(ConsolidationValidationError(
            "every previous handoff item needs an operation".into(),
        ));
    }
    let event_ids = packet
        .events
        .iter()
        .map(|event| event.event_id.as_str())
        .collect::<HashSet<_>>();
    let memory_ids = packet
        .related_memories
        .iter()
        .map(|memory| memory.id)
        .collect::<HashSet<_>>();
    let summary_item_ids = packet
        .handoff_items
        .iter()
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    if result.summary.continuity.iter().any(|item| {
        item.item_id
            .is_some_and(|id| !summary_item_ids.contains(&id))
    }) {
        return Err(ConsolidationValidationError(
            "summary references an item outside the packet".into(),
        ));
    }
    for operation in &result.knowledge {
        if operation
            .target_memory_ids
            .iter()
            .any(|id| !memory_ids.contains(id))
        {
            return Err(ConsolidationValidationError(
                "knowledge operation references a memory outside the packet".into(),
            ));
        }
        if operation
            .evidence_event_ids
            .iter()
            .chain(&operation.contradicting_event_ids)
            .any(|id| !event_ids.contains(id.as_str()))
        {
            return Err(ConsolidationValidationError(
                "knowledge operation references an event outside the packet".into(),
            ));
        }
        validate_knowledge_operation(operation)?;
    }
    Ok(())
}

fn validate_summary(summary: &EpisodicSummary) -> Result<(), ConsolidationValidationError> {
    if summary.intentions.len() > MAX_SUMMARY_ITEMS
        || summary.actions.len() > MAX_SUMMARY_ITEMS
        || summary.continuity.len() > MAX_SUMMARY_ITEMS
        || summary.candidate_learnings.len() > MAX_SUMMARY_ITEMS
    {
        return Err(ConsolidationValidationError(
            "summary contains too many items".into(),
        ));
    }
    let mut strings = summary
        .intentions
        .iter()
        .chain(&summary.actions)
        .chain(&summary.candidate_learnings)
        .chain(std::iter::once(&summary.result));
    if strings.any(|value| value.chars().count() > MAX_SUMMARY_TEXT_CHARS) {
        return Err(ConsolidationValidationError(
            "summary text exceeds the limit".into(),
        ));
    }
    Ok(())
}

fn validate_handoff_operation(
    operation: &HandoffItemOperation,
    packet: &ConsolidationPacket,
) -> Result<(), ConsolidationValidationError> {
    match operation {
        HandoffItemOperation::Update(value) => validate_new_handoff(&NewHandoffItem {
            kind: value.kind,
            state: value.state.clone(),
            next_step: value.next_step.clone(),
            blocker: value.blocker.clone(),
        })?,
        HandoffItemOperation::Replace(value) => validate_new_handoff(&value.replacement)?,
        HandoffItemOperation::Create(value) => validate_new_handoff(&value.item)?,
        HandoffItemOperation::Resolve(value) | HandoffItemOperation::Discard(value) => {
            validate_text(&value.text, "handoff transition")?
        }
        HandoffItemOperation::Keep { .. } | HandoffItemOperation::Uncertain { .. } => {}
    }
    let events = packet
        .events
        .iter()
        .map(|event| event.event_id.as_str())
        .collect::<HashSet<_>>();
    let ids = match operation {
        HandoffItemOperation::Update(value) => &value.evidence_event_ids,
        HandoffItemOperation::Resolve(value) | HandoffItemOperation::Discard(value) => {
            &value.evidence_event_ids
        }
        HandoffItemOperation::Replace(value) => &value.evidence_event_ids,
        HandoffItemOperation::Create(value) => &value.evidence_event_ids,
        HandoffItemOperation::Keep { .. } | HandoffItemOperation::Uncertain { .. } => return Ok(()),
    };
    if ids.iter().any(|id| !events.contains(id.as_str())) {
        return Err(ConsolidationValidationError(
            "handoff operation references an event outside the packet".into(),
        ));
    }
    Ok(())
}

fn validate_new_handoff(item: &NewHandoffItem) -> Result<(), ConsolidationValidationError> {
    validate_text(&item.state, "handoff state")?;
    if item.state.trim().is_empty() {
        return Err(ConsolidationValidationError(
            "handoff state cannot be empty".into(),
        ));
    }
    if let Some(value) = item.next_step.as_deref() {
        validate_text(value, "handoff next step")?;
    }
    if let Some(value) = item.blocker.as_deref() {
        validate_text(value, "handoff blocker")?;
    }
    Ok(())
}

fn validate_text(value: &str, field: &str) -> Result<(), ConsolidationValidationError> {
    if value.chars().count() > MAX_HANDOFF_TEXT_CHARS {
        return Err(ConsolidationValidationError(format!(
            "{field} exceeds the limit"
        )));
    }
    Ok(())
}

fn validate_knowledge_operation(
    operation: &KnowledgeOperation,
) -> Result<(), ConsolidationValidationError> {
    match operation.operation {
        KnowledgeOperationKind::NoOp => return Ok(()),
        KnowledgeOperationKind::Reinforce => {
            if operation.target_memory_ids.is_empty() {
                return Err(ConsolidationValidationError(
                    "reinforce needs a target".into(),
                ));
            }
        }
        KnowledgeOperationKind::Create => {
            if !operation.target_memory_ids.is_empty() {
                return Err(ConsolidationValidationError(
                    "create cannot target an existing memory".into(),
                ));
            }
        }
        KnowledgeOperationKind::Merge => {
            if operation.target_memory_ids.len() < 2 {
                return Err(ConsolidationValidationError(
                    "merge needs at least two targets".into(),
                ));
            }
        }
        KnowledgeOperationKind::Supersede => {
            if operation.target_memory_ids.is_empty() {
                return Err(ConsolidationValidationError(
                    "supersede needs a target".into(),
                ));
            }
        }
    }
    if !matches!(
        operation.operation,
        KnowledgeOperationKind::Reinforce | KnowledgeOperationKind::NoOp
    ) && (operation.title.as_deref().is_none_or(str::is_empty)
        || operation.content.is_none()
        || operation.knowledge_type.is_none()
        || operation.scope.is_none())
    {
        return Err(ConsolidationValidationError(
            "knowledge creation needs type, title, scope, and content".into(),
        ));
    }
    if operation.scope == Some(Scope::Global)
        && operation.scope_confidence.unwrap_or(0.0) < GLOBAL_SCOPE_CONFIDENCE_THRESHOLD
    {
        return Err(ConsolidationValidationError(
            "global knowledge needs high scope confidence".into(),
        ));
    }
    if operation.knowledge_type == Some(KnowledgeType::Context)
        && !matches!(operation.content, Some(KnowledgeContent::Context(_)))
    {
        return Err(ConsolidationValidationError(
            "context operation needs context content".into(),
        ));
    }
    if operation.knowledge_type == Some(KnowledgeType::Playbook)
        && !matches!(operation.content, Some(KnowledgeContent::Playbook(_)))
    {
        return Err(ConsolidationValidationError(
            "playbook operation needs playbook content".into(),
        ));
    }
    match operation.content.as_ref() {
        Some(KnowledgeContent::Context(value))
            if value.body.chars().count() > MAX_KNOWLEDGE_BODY_CHARS =>
        {
            return Err(ConsolidationValidationError(
                "context content exceeds the limit".into(),
            ));
        }
        Some(KnowledgeContent::Playbook(value)) => {
            if value.trigger.chars().count() > MAX_SUMMARY_TEXT_CHARS
                || value.failure_handling.chars().count() > MAX_SUMMARY_TEXT_CHARS
                || value.steps.len() > MAX_SUMMARY_ITEMS
                || value.validation.len() > MAX_SUMMARY_ITEMS
                || value
                    .steps
                    .iter()
                    .any(|step| step.chars().count() > MAX_SUMMARY_TEXT_CHARS)
                || value
                    .validation
                    .iter()
                    .any(|step| step.chars().count() > MAX_SUMMARY_TEXT_CHARS)
            {
                return Err(ConsolidationValidationError(
                    "playbook content exceeds the limit".into(),
                ));
            }
        }
        None | Some(KnowledgeContent::Context(_)) => {}
    }
    Ok(())
}

pub fn consolidation_result_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "handoff", "knowledge"],
        "properties": {
            "summary": {
                "type": "object",
                "additionalProperties": false,
                "required": ["intentions", "actions", "outcome", "result", "continuity", "candidate-learnings"],
                "properties": {
                    "intentions": {"type": "array", "maxItems": MAX_SUMMARY_ITEMS, "items": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}},
                    "actions": {"type": "array", "maxItems": MAX_SUMMARY_ITEMS, "items": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}},
                    "outcome": {"type": "string", "enum": ["completed", "advanced", "blocked", "abandoned", "inconclusive"]},
                    "result": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS},
                    "continuity": {"type": "array", "maxItems": MAX_SUMMARY_ITEMS},
                    "candidate-learnings": {"type": "array", "maxItems": MAX_SUMMARY_ITEMS, "items": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}}
                }
            },
            "handoff": {"type": "array", "maxItems": 120},
            "knowledge": {"type": "array", "maxItems": MAX_KNOWLEDGE_OPERATIONS}
        }
    })
}
