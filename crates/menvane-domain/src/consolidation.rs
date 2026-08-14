use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::handoff::{
    HandoffItem, HandoffItemOperation, MAX_HANDOFF_ITEMS, MAX_HANDOFF_TEXT_CHARS, NewHandoffItem,
};
use crate::memory::{Applicability, KnowledgeType, MemoryStatus, Scope};
use crate::session::NormalizedEvent;
use crate::summary::{
    ContinuityDisposition, ContinuityItem, EpisodicSummary, MAX_SUMMARY_ITEMS,
    MAX_SUMMARY_TEXT_CHARS,
};

pub const MAX_KNOWLEDGE_OPERATIONS: usize = 10;
pub const MAX_RELATED_SUMMARIES: usize = 5;
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
    if packet.related_summaries.len() > MAX_RELATED_SUMMARIES {
        return Err(ConsolidationValidationError(
            "packet contains too many related summaries".into(),
        ));
    }
    let item_ids = packet
        .handoff_items
        .iter()
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    if item_ids.len() != packet.handoff_items.len() {
        return Err(ConsolidationValidationError(
            "packet contains repeated handoff items".into(),
        ));
    }
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
        validate_knowledge_promotion(packet, operation)?;
    }
    Ok(())
}

pub fn validate_knowledge_promotion(
    packet: &ConsolidationPacket,
    operation: &KnowledgeOperation,
) -> Result<(), ConsolidationValidationError> {
    if matches!(
        operation.operation,
        KnowledgeOperationKind::NoOp | KnowledgeOperationKind::Reinforce
    ) {
        return Ok(());
    }
    if operation.evidence_event_ids.is_empty() {
        return Err(ConsolidationValidationError(
            "knowledge promotion needs observable evidence".into(),
        ));
    }
    let observable = packet.events.iter().any(|event| {
        operation
            .evidence_event_ids
            .iter()
            .any(|id| id == &event.event_id)
            && (event.success == Some(true)
                || event
                    .bounded_output
                    .as_deref()
                    .is_some_and(|output| !output.trim().is_empty()))
    });
    if !observable {
        return Err(ConsolidationValidationError(
            "knowledge promotion needs observable evidence".into(),
        ));
    }
    if operation
        .title
        .as_deref()
        .is_none_or(|title| title.split_whitespace().count() < 1)
    {
        return Err(ConsolidationValidationError(
            "knowledge promotion needs a retrieval title".into(),
        ));
    }
    let text = knowledge_text(operation);
    if text.trim().is_empty() || text.split_whitespace().count() < 3 {
        return Err(ConsolidationValidationError(
            "knowledge promotion needs retrievable content".into(),
        ));
    }
    let lower = text.to_ascii_lowercase();
    let task_state = ["in progress", "current task", "implemented behavior"]
        .iter()
        .any(|term| lower.contains(term))
        || lower
            .split(|character: char| !character.is_alphanumeric())
            .any(|word| matches!(word, "pending" | "todo"));
    if task_state {
        return Err(ConsolidationValidationError(
            "task state or evident behavior cannot become knowledge".into(),
        ));
    }
    if operation.target_memory_ids.iter().any(|id| {
        packet
            .related_memories
            .iter()
            .any(|memory| memory.id == *id && memory.status == MemoryStatus::Forgotten)
    }) {
        return Err(ConsolidationValidationError(
            "forgotten knowledge cannot be promoted".into(),
        ));
    }
    if packet.related_memories.iter().any(|memory| {
        memory.status != MemoryStatus::Forgotten
            && memory.knowledge_type == operation.knowledge_type.unwrap_or(memory.knowledge_type)
            && normalize_knowledge_text(&memory.title)
                == normalize_knowledge_text(operation.title.as_deref().unwrap_or_default())
            && normalize_knowledge_text(&memory.body) == normalize_knowledge_text(&text)
    }) {
        return Err(ConsolidationValidationError(
            "duplicate knowledge cannot be promoted".into(),
        ));
    }
    Ok(())
}

fn knowledge_text(operation: &KnowledgeOperation) -> String {
    match operation.content.as_ref() {
        Some(KnowledgeContent::Context(value)) => value.body.clone(),
        Some(KnowledgeContent::Playbook(value)) => format!(
            "{} {} {} {} {}",
            value.trigger,
            value.steps.join(" "),
            value.validation.join(" "),
            value.failure_handling,
            serde_json::to_string(&value.applicability).unwrap_or_default()
        ),
        None => String::new(),
    }
}

fn normalize_knowledge_text(value: &str) -> String {
    value
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn preserve_handoff_transitions(
    mut result: ConsolidationResult,
    packet: &ConsolidationPacket,
) -> Result<ConsolidationResult, ConsolidationValidationError> {
    let mut continuity = result.summary.continuity.clone();
    for operation in &result.handoff {
        let (item_id, disposition, front) = match operation {
            HandoffItemOperation::Resolve(value) => (
                value.item_id,
                ContinuityDisposition::Resolved,
                value.text.clone(),
            ),
            HandoffItemOperation::Discard(value) => (
                value.item_id,
                ContinuityDisposition::Discarded,
                value.text.clone(),
            ),
            HandoffItemOperation::Replace(value) => (
                value.item_id,
                ContinuityDisposition::Replaced,
                value.replacement.state.clone(),
            ),
            _ => continue,
        };
        if let Some(item) = continuity
            .iter_mut()
            .find(|item| item.item_id == Some(item_id))
        {
            item.front = front;
            item.disposition = disposition;
        } else {
            continuity.push(ContinuityItem {
                item_id: Some(item_id),
                front,
                disposition,
            });
        }
    }
    if continuity.len() > MAX_SUMMARY_ITEMS {
        return Err(ConsolidationValidationError(
            "summary contains too many continuity items".into(),
        ));
    }
    result.summary.continuity = continuity;
    validate_consolidation_result(packet, &result)?;
    Ok(result)
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
    if summary
        .continuity
        .iter()
        .any(|item| item.front.chars().count() > MAX_SUMMARY_TEXT_CHARS)
    {
        return Err(ConsolidationValidationError(
            "summary continuity text exceeds the limit".into(),
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
                    "continuity": {
                        "type": "array",
                        "maxItems": MAX_SUMMARY_ITEMS,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["item_id", "front", "disposition"],
                            "properties": {
                                "item_id": {"type": ["string", "null"]},
                                "front": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS},
                                "disposition": {"type": "string", "enum": ["continues", "resolved", "discarded", "replaced"]}
                            }
                        }
                    },
                    "candidate-learnings": {"type": "array", "maxItems": MAX_SUMMARY_ITEMS, "items": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}}
                }
            },
            "handoff": {"type": "array", "maxItems": 120, "items": handoff_operation_schema()},
            "knowledge": {"type": "array", "maxItems": MAX_KNOWLEDGE_OPERATIONS, "items": knowledge_operation_schema()}
        }
    })
}

fn handoff_operation_schema() -> serde_json::Value {
    let new_item = || {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["kind", "state", "next_step", "blocker"],
            "properties": {
                "kind": {"type": "string", "enum": ["in-progress", "open-question", "parked", "blocked"]},
                "state": {"type": "string", "maxLength": MAX_HANDOFF_TEXT_CHARS},
                "next_step": {"type": ["string", "null"], "maxLength": MAX_HANDOFF_TEXT_CHARS},
                "blocker": {"type": ["string", "null"], "maxLength": MAX_HANDOFF_TEXT_CHARS}
            }
        })
    };
    let evidence = serde_json::json!({"type": "array", "items": {"type": "string"}});
    serde_json::json!({
        "anyOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["keep"],
                "properties": {"keep": {"type": "object", "additionalProperties": false, "required": ["item_id"], "properties": {"item_id": {"type": "string"}}}}
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["update"],
                "properties": {"update": {"type": "object", "additionalProperties": false, "required": ["item_id", "kind", "state", "next_step", "blocker", "evidence_event_ids"], "properties": {"item_id": {"type": "string"}, "kind": {"type": "string", "enum": ["in-progress", "open-question", "parked", "blocked"]}, "state": {"type": "string", "maxLength": MAX_HANDOFF_TEXT_CHARS}, "next_step": {"type": ["string", "null"]}, "blocker": {"type": ["string", "null"]}, "evidence_event_ids": evidence}}}
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["resolve"],
                "properties": {"resolve": transition_schema()}
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["discard"],
                "properties": {"discard": transition_schema()}
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["replace"],
                "properties": {"replace": {"type": "object", "additionalProperties": false, "required": ["item_id", "replacement", "evidence_event_ids"], "properties": {"item_id": {"type": "string"}, "replacement": new_item(), "evidence_event_ids": evidence}}}
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["uncertain"],
                "properties": {"uncertain": {"type": "object", "additionalProperties": false, "required": ["item_id"], "properties": {"item_id": {"type": "string"}}}}
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["create"],
                "properties": {"create": {"type": "object", "additionalProperties": false, "required": ["item", "evidence_event_ids"], "properties": {"item": new_item(), "evidence_event_ids": evidence}}}
            }
        ]
    })
}

fn transition_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["item_id", "text", "evidence_event_ids"],
        "properties": {
            "item_id": {"type": "string"},
            "text": {"type": "string", "maxLength": MAX_HANDOFF_TEXT_CHARS},
            "evidence_event_ids": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn applicability_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["languages", "frameworks", "tools", "databases", "platforms"],
        "properties": {
            "languages": {"type": "array", "items": {"type": "string"}},
            "frameworks": {"type": "array", "items": {"type": "string"}},
            "tools": {"type": "array", "items": {"type": "string"}},
            "databases": {"type": "array", "items": {"type": "string"}},
            "platforms": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn knowledge_operation_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["operation", "target_memory_ids", "knowledge_type", "title", "scope", "scope_confidence", "applies_to", "content", "evidence_event_ids", "contradicting_event_ids"],
        "properties": {
            "operation": {"type": "string", "enum": ["create", "reinforce", "merge", "supersede", "no-op"]},
            "target_memory_ids": {"type": "array", "items": {"type": "string"}},
            "knowledge_type": {"type": ["string", "null"], "enum": ["context", "playbook", null]},
            "title": {"type": ["string", "null"], "maxLength": MAX_SUMMARY_TEXT_CHARS},
            "scope": {"type": ["string", "null"], "enum": ["global", "project", null]},
            "scope_confidence": {"type": ["number", "null"]},
            "applies_to": applicability_schema(),
            "content": {"anyOf": [{"type": "null"}, context_content_schema(), playbook_content_schema()]},
            "evidence_event_ids": {"type": "array", "items": {"type": "string"}},
            "contradicting_event_ids": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn context_content_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["context"],
        "properties": {"context": {"type": "object", "additionalProperties": false, "required": ["body"], "properties": {"body": {"type": "string", "maxLength": MAX_KNOWLEDGE_BODY_CHARS}}}}
    })
}

fn playbook_content_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["playbook"],
        "properties": {"playbook": {"type": "object", "additionalProperties": false, "required": ["trigger", "applicability", "steps", "validation", "failure_handling"], "properties": {"trigger": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}, "applicability": applicability_schema(), "steps": {"type": "array", "items": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}}, "validation": {"type": "array", "items": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}}, "failure_handling": {"type": "string", "maxLength": MAX_SUMMARY_TEXT_CHARS}}}}
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::handoff::HandoffTransition;

    fn item(id: Uuid) -> HandoffItem {
        let timestamp = Utc.timestamp_opt(1, 0).single().unwrap();
        HandoffItem {
            id,
            project_id: Some("project".to_owned()),
            kind: crate::handoff::HandoffItemKind::InProgress,
            state: "open".to_owned(),
            next_step: None,
            blocker: None,
            low_confidence: false,
            last_confirmed_at: timestamp,
            sources: Vec::new(),
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    fn summary() -> EpisodicSummary {
        EpisodicSummary {
            intentions: Vec::new(),
            actions: Vec::new(),
            outcome: crate::summary::SummaryOutcome::Inconclusive,
            result: "result".to_owned(),
            continuity: Vec::new(),
            candidate_learnings: Vec::new(),
        }
    }

    #[test]
    fn transitions_are_preserved_in_summary_continuity() {
        let id = Uuid::from_u128(1);
        let packet = ConsolidationPacket {
            session_id: Uuid::from_u128(2),
            events: Vec::new(),
            handoff_items: vec![item(id)],
            related_summaries: Vec::new(),
            related_memories: Vec::new(),
        };
        for operation in [
            HandoffItemOperation::Resolve(HandoffTransition {
                item_id: id,
                text: "resolved".to_owned(),
                evidence_event_ids: Vec::new(),
            }),
            HandoffItemOperation::Discard(HandoffTransition {
                item_id: id,
                text: "discarded".to_owned(),
                evidence_event_ids: Vec::new(),
            }),
        ] {
            let result = preserve_handoff_transitions(
                ConsolidationResult {
                    summary: summary(),
                    handoff: vec![operation],
                    knowledge: Vec::new(),
                },
                &packet,
            )
            .unwrap();
            assert_eq!(result.summary.continuity.len(), 1);
        }
    }

    #[test]
    fn every_previous_item_requires_one_operation() {
        let id = Uuid::from_u128(1);
        let packet = ConsolidationPacket {
            session_id: Uuid::from_u128(2),
            events: Vec::new(),
            handoff_items: vec![item(id)],
            related_summaries: Vec::new(),
            related_memories: Vec::new(),
        };
        let error = validate_consolidation_result(
            &packet,
            &ConsolidationResult {
                summary: summary(),
                handoff: Vec::new(),
                knowledge: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("every previous handoff item"));
    }

    #[test]
    fn consolidation_schema_declares_items_for_operational_arrays() {
        let schema = consolidation_result_schema();
        assert!(schema["properties"]["summary"]["properties"]["continuity"]["items"].is_object());
        assert!(schema["properties"]["handoff"]["items"].is_object());
        assert!(schema["properties"]["knowledge"]["items"].is_object());
    }

    fn promotion_packet() -> ConsolidationPacket {
        let timestamp = Utc.timestamp_opt(1, 0).single().unwrap();
        ConsolidationPacket {
            session_id: Uuid::from_u128(2),
            events: vec![NormalizedEvent {
                event_id: "tool".to_owned(),
                kind: crate::session::NormalizedEventKind::ToolCompleted,
                origin: crate::session::NormalizedEventOrigin::Tool,
                role: crate::session::NormalizedEventRole::ToolActivity,
                client: "test".to_owned(),
                external_session_id: "session".to_owned(),
                timestamp,
                cwd: "/project".to_owned(),
                project_id: None,
                tool_family: None,
                bounded_input: None,
                bounded_output: Some("deployment verified".to_owned()),
                attributed_path: None,
                success: Some(true),
                model: None,
                harness_injected: false,
            }],
            handoff_items: Vec::new(),
            related_summaries: Vec::new(),
            related_memories: Vec::new(),
        }
    }

    fn promotion_operation(body: &str) -> KnowledgeOperation {
        KnowledgeOperation {
            operation: KnowledgeOperationKind::Create,
            target_memory_ids: Vec::new(),
            knowledge_type: Some(KnowledgeType::Context),
            title: Some("External deployment constraint".to_owned()),
            scope: Some(Scope::Project),
            scope_confidence: Some(0.95),
            applies_to: Applicability::default(),
            content: Some(KnowledgeContent::Context(ContextContent {
                body: body.to_owned(),
            })),
            evidence_event_ids: vec!["tool".to_owned()],
            contradicting_event_ids: Vec::new(),
        }
    }

    #[test]
    fn promotion_barrier_accepts_reusable_content_without_task_state_words() {
        let packet = promotion_packet();
        let operation = promotion_operation(
            "When appending to the deployment log, keep entries single line to limit spending.",
        );
        validate_knowledge_promotion(&packet, &operation).unwrap();
    }

    #[test]
    fn promotion_barrier_rejects_temporary_task_state() {
        let packet = promotion_packet();
        for body in [
            "The export fix is pending review tomorrow.",
            "The remaining todo is wiring the exporter.",
            "The exporter refactor is in progress.",
            "The current task blocks the exporter release.",
            "The implemented behavior lives in the exporter module.",
        ] {
            assert!(validate_knowledge_promotion(&packet, &promotion_operation(body)).is_err());
        }
    }
}
