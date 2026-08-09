use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use menvane_domain::{
    Applicability, EpisodeEvidencePacket, JsonSchema, LlmError, LlmErrorKind, LlmProvider,
    LlmRequest, MemoryStatus, MemoryType, Scope,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const RELATED_MEMORY_LIMIT: usize = 24;
pub const RELATED_MEMORY_BUDGET_BYTES: usize = 16_384;
pub const GLOBAL_SCOPE_CONFIDENCE_THRESHOLD: f64 = 0.8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedMemory {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub scope: Scope,
    pub status: MemoryStatus,
    pub confidence: f64,
    pub applicability: Applicability,
    pub title: String,
    pub body: String,
    pub provenance: RelatedMemoryProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedMemoryProvenance {
    pub source_session_count: usize,
    pub supersession_count: usize,
}

#[derive(Debug, Clone)]
pub struct CompilationInput {
    pub evidence: EpisodeEvidencePacket,
    pub existing_related_memories: Vec<RelatedMemory>,
    pub technology_profile: Value,
    pub source_session: Option<Uuid>,
    pub source_episode: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CompiledMemory {
    pub memory_type: MemoryType,
    pub title: String,
    pub scope: Scope,
    pub scope_confidence: f64,
    pub confidence: f64,
    pub applies_to: Applicability,
    pub body: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompiledOperation {
    pub operation: String,
    pub target_memory_ids: Vec<Uuid>,
    pub memory_type: MemoryType,
    pub title: String,
    pub scope: Scope,
    pub scope_confidence: f64,
    pub scope_reason: String,
    pub confidence_signal: f64,
    pub applies_to: Applicability,
    pub content: Value,
    pub evidence_event_ids: Vec<String>,
    pub contradicting_event_ids: Vec<String>,
}

pub struct CompilationResult {
    pub operations: Vec<CompiledOperation>,
    pub memories: Vec<CompiledMemory>,
    pub provider: String,
    pub model: String,
}

pub struct MemoryCompiler {
    provider: Arc<dyn LlmProvider>,
}

impl MemoryCompiler {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self { provider }
    }

    pub async fn compile(&self, input: CompilationInput) -> Result<CompilationResult, LlmError> {
        if !self.provider.capabilities().structured_output
            || !self.provider.capabilities().json_schema
        {
            return Err(LlmError {
                kind: LlmErrorKind::UnsupportedCapability,
                message: "memory compilation requires JSON Schema structured output".to_owned(),
            });
        }
        let prompt = serde_json::to_string_pretty(&json!({
            "episode_evidence": input.evidence,
            "existing_related_memories": input.existing_related_memories,
            "technology_profile": input.technology_profile
        }))
        .map_err(internal)?;
        let request = LlmRequest {
            system: compiler_system_prompt().to_owned(),
            prompt,
            timeout: Duration::from_secs(120),
        };
        let schema = JsonSchema(compiler_schema());
        let mut last_error = None;
        for attempt in 0..2 {
            let request = if attempt == 0 {
                request.clone()
            } else {
                LlmRequest {
                    system: format!(
                        "{} Return a corrected response after repairing the previous validation error.",
                        request.system
                    ),
                    ..request.clone()
                }
            };
            let response = self
                .provider
                .generate_structured(request, schema.clone())
                .await?;
            match parse_response(response.value, &input) {
                Ok(operations) => {
                    let memories = operations
                        .iter()
                        .filter(|operation| operation.operation == "create")
                        .map(compiled_memory)
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(CompilationResult {
                        operations,
                        memories,
                        provider: response.provider,
                        model: response.model,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| invalid_schema("compiler response was invalid")))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerResponse {
    operations: Vec<Operation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    operation: String,
    target_memory_ids: Vec<Uuid>,
    #[serde(rename = "type")]
    memory_type: String,
    title: String,
    scope_candidate: String,
    scope_confidence: f64,
    scope_reason: String,
    confidence_signal: f64,
    applicability: Applicability,
    content: Value,
    evidence_event_ids: Vec<String>,
    contradicting_event_ids: Vec<String>,
}

fn parse_response(
    value: Value,
    input: &CompilationInput,
) -> Result<Vec<CompiledOperation>, LlmError> {
    let response: CompilerResponse = serde_json::from_value(value)
        .map_err(|error| invalid_schema(format!("compiler schema mismatch: {error}")))?;
    let evidence_ids = evidence_ids(&input.evidence);
    let related = input
        .existing_related_memories
        .iter()
        .map(|memory| (memory.id, memory))
        .collect::<std::collections::HashMap<_, _>>();
    response
        .operations
        .into_iter()
        .map(|operation| validate_operation(operation, &evidence_ids, &related))
        .collect()
}

fn validate_operation(
    operation: Operation,
    evidence_ids: &HashSet<String>,
    related: &std::collections::HashMap<Uuid, &RelatedMemory>,
) -> Result<CompiledOperation, LlmError> {
    if !matches!(
        operation.operation.as_str(),
        "create" | "reinforce" | "merge" | "supersede" | "no-op"
    ) {
        return Err(invalid_schema(
            "operation must be create, reinforce, merge, supersede, or no-op",
        ));
    }
    let memory_type = operation
        .memory_type
        .parse::<MemoryType>()
        .map_err(|error| invalid_schema(error.to_string()))?;
    if memory_type == MemoryType::Session {
        return Err(invalid_schema("compiler cannot create session memories"));
    }
    if operation.title.trim().is_empty() {
        return Err(invalid_schema("operation title cannot be empty"));
    }
    if !(0.0..=1.0).contains(&operation.confidence_signal)
        || !(0.0..=1.0).contains(&operation.scope_confidence)
    {
        return Err(invalid_schema("confidence values must be between 0 and 1"));
    }
    let scope = match operation.scope_candidate.as_str() {
        "global" if operation.scope_confidence >= GLOBAL_SCOPE_CONFIDENCE_THRESHOLD => {
            Scope::Global
        }
        "project" | "global" => Scope::Project,
        _ => return Err(invalid_schema("scope_candidate must be project or global")),
    };
    if scope == Scope::Global && operation.scope_reason.trim().is_empty() {
        return Err(invalid_schema("global scope requires a scope reason"));
    }
    if operation
        .evidence_event_ids
        .iter()
        .chain(operation.contradicting_event_ids.iter())
        .any(|event_id| !evidence_ids.contains(event_id))
    {
        return Err(invalid_schema(
            "operation references an event outside the episode evidence",
        ));
    }
    if matches!(
        operation.operation.as_str(),
        "create" | "reinforce" | "merge" | "supersede"
    ) && operation.evidence_event_ids.is_empty()
    {
        return Err(invalid_schema("durable operations require source evidence"));
    }
    let mut targets = Vec::new();
    for target_id in &operation.target_memory_ids {
        if targets
            .iter()
            .any(|target: &&RelatedMemory| target.id == *target_id)
        {
            return Err(invalid_schema("target_memory_ids must be unique"));
        }
        let target = related
            .get(target_id)
            .ok_or_else(|| invalid_schema(format!("target memory does not exist: {target_id}")))?;
        targets.push(target);
    }
    match operation.operation.as_str() {
        "create" => {
            if !(targets.is_empty()
                || targets
                    .iter()
                    .all(|target| target.status == MemoryStatus::Forgotten))
            {
                return Err(invalid_schema("create targets must be empty or forgotten"));
            }
            if targets
                .iter()
                .any(|target| target.memory_type != memory_type || target.scope != scope)
            {
                return Err(invalid_schema(
                    "create target type or scope does not match operation type",
                ));
            }
            if targets.is_empty() {
                let body = content_markdown(memory_type, &operation.content)?;
                if related.values().any(|target| {
                    target.status == MemoryStatus::Forgotten
                        && target.memory_type == memory_type
                        && normalize_content(&target.body) == normalize_content(&body)
                }) {
                    return Err(invalid_schema(
                        "forgotten related memory requires an explicit target operation",
                    ));
                }
            }
        }
        "reinforce" => {
            if targets.len() != 1 || !eligible_target(targets[0]) {
                return Err(invalid_schema("reinforce requires one active target"));
            }
            if targets[0].memory_type != memory_type || targets[0].scope != scope {
                return Err(invalid_schema(
                    "reinforce target type does not match operation type",
                ));
            }
        }
        "merge" => {
            if targets.len() < 2 || targets.iter().any(|target| !eligible_target(target)) {
                return Err(invalid_schema("merge requires at least two active targets"));
            }
            if targets
                .iter()
                .any(|target| target.memory_type != memory_type || target.scope != scope)
            {
                return Err(invalid_schema(
                    "merge target types do not match operation type",
                ));
            }
        }
        "supersede" => {
            if targets.is_empty() || targets.iter().any(|target| !eligible_target(target)) {
                return Err(invalid_schema("supersede requires an active target"));
            }
            if targets
                .iter()
                .any(|target| target.memory_type != memory_type || target.scope != scope)
            {
                return Err(invalid_schema(
                    "supersede target types do not match operation type",
                ));
            }
        }
        "no-op" => {}
        _ => unreachable!(),
    }
    if !operation.contradicting_event_ids.is_empty()
        && matches!(operation.operation.as_str(), "create" | "reinforce")
    {
        return Err(invalid_schema(
            "contradicting evidence requires supersede, merge, or no-op",
        ));
    }
    if operation.operation == "supersede" && operation.contradicting_event_ids.is_empty() {
        return Err(invalid_schema("supersede requires contradicting evidence"));
    }
    if !operation.contradicting_event_ids.is_empty()
        && operation.operation == "no-op"
        && targets.iter().any(|target| {
            !matches!(
                target.status,
                MemoryStatus::Superseded | MemoryStatus::Historical | MemoryStatus::Forgotten
            )
        })
    {
        return Err(invalid_schema(
            "no-op contradiction must target preserved historical knowledge",
        ));
    }
    if operation.operation != "no-op" {
        content_markdown(memory_type, &operation.content)?;
    }
    Ok(CompiledOperation {
        operation: operation.operation,
        target_memory_ids: operation.target_memory_ids,
        memory_type,
        title: operation.title.trim().to_owned(),
        scope,
        scope_confidence: operation.scope_confidence,
        scope_reason: operation.scope_reason.trim().to_owned(),
        confidence_signal: operation.confidence_signal,
        applies_to: operation.applicability,
        content: operation.content,
        evidence_event_ids: operation.evidence_event_ids,
        contradicting_event_ids: operation.contradicting_event_ids,
    })
}

fn eligible_target(memory: &RelatedMemory) -> bool {
    matches!(
        memory.status,
        MemoryStatus::Active | MemoryStatus::Candidate | MemoryStatus::NeedsValidation
    )
}

fn normalize_content(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn compiled_memory(operation: &CompiledOperation) -> Result<CompiledMemory, LlmError> {
    Ok(CompiledMemory {
        memory_type: operation.memory_type,
        title: operation.title.clone(),
        scope: operation.scope,
        scope_confidence: operation.scope_confidence,
        confidence: operation.confidence_signal,
        applies_to: operation.applies_to.clone(),
        body: content_markdown(operation.memory_type, &operation.content)?,
        evidence: operation.evidence_event_ids.clone(),
    })
}

fn evidence_ids(packet: &EpisodeEvidencePacket) -> HashSet<String> {
    let mut ids = HashSet::from([packet.goal.event_id.clone()]);
    for item in packet_items(packet) {
        ids.insert(item.event_id.clone());
    }
    ids
}

fn packet_items(
    packet: &EpisodeEvidencePacket,
) -> impl Iterator<Item = &menvane_domain::EvidenceItem> {
    packet
        .prompts
        .iter()
        .chain(packet.actions.iter())
        .chain(packet.decisions.iter())
        .chain(packet.discoveries.iter())
        .chain(packet.errors.iter())
        .chain(packet.validations.iter())
        .chain(packet.compaction_context.iter())
        .chain(packet.unresolved_questions.iter())
}

pub(crate) fn content_markdown(
    memory_type: MemoryType,
    content: &Value,
) -> Result<String, LlmError> {
    let string = |key: &str| {
        content
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| invalid_schema(format!("{memory_type} content requires {key}")))
    };
    let list = |key: &str| -> Result<String, LlmError> {
        let values = content
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_schema(format!("{memory_type} content requires {key}[]")))?;
        Ok(values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(|value| format!("- {value}"))
                    .ok_or_else(|| invalid_schema(format!("{key} values must be strings")))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("\n"))
    };
    match memory_type {
        MemoryType::Fact => string("statement"),
        MemoryType::Decision => Ok(format!(
            "## Decision\n\n{}\n\n## Reason\n\n{}\n\n## Alternatives\n\n{}\n\n## Consequences\n\n{}",
            string("decision")?,
            string("reason")?,
            list("alternatives")?,
            list("consequences")?
        )),
        MemoryType::Gotcha => Ok(format!(
            "## Problem\n\n{}\n\n## Cause\n\n{}\n\n## Resolution\n\n{}\n\n## Avoidance\n\n{}",
            string("problem")?,
            string("cause")?,
            string("resolution")?,
            string("avoidance")?
        )),
        MemoryType::Procedure => Ok(format!(
            "## Trigger\n\n{}\n\n## Preconditions\n\n{}\n\n## Procedure\n\n{}\n\n## Decision points\n\n{}\n\n## Validation\n\n{}\n\n## Failure handling\n\n{}\n\n## Expected outcome\n\n{}",
            string("trigger")?,
            list("preconditions")?,
            numbered(content, "steps")?,
            list("decision_points")?,
            list("validation")?,
            list("failure_handling")?,
            string("expected_outcome")?
        )),
        MemoryType::Session => Err(invalid_schema("compiler cannot create sessions")),
    }
}

fn numbered(content: &Value, key: &str) -> Result<String, LlmError> {
    let values = content
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_schema(format!("procedure content requires {key}[]")))?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(|value| format!("{}. {value}", index + 1))
                .ok_or_else(|| invalid_schema(format!("{key} values must be strings")))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join("\n"))
}

fn compiler_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["operations"],
        "properties": {
            "operations": {
                "type": "array",
                "items": { "$ref": "#/$defs/operation" }
            }
        },
        "$defs": {
            "applicability": {
                "type": "object",
                "additionalProperties": false,
                "required": ["languages", "frameworks", "tools", "databases", "platforms"],
                "properties": {
                    "languages": { "type": "array", "items": { "type": "string" } },
                    "frameworks": { "type": "array", "items": { "type": "string" } },
                    "tools": { "type": "array", "items": { "type": "string" } },
                    "databases": { "type": "array", "items": { "type": "string" } },
                    "platforms": { "type": "array", "items": { "type": "string" } }
                }
            },
            "empty_content": {
                "type": "object",
                "additionalProperties": false,
                "required": [],
                "properties": {}
            },
            "fact_content": {
                "type": "object",
                "additionalProperties": false,
                "required": ["statement"],
                "properties": { "statement": { "type": "string" } }
            },
            "decision_content": {
                "type": "object",
                "additionalProperties": false,
                "required": ["decision", "reason", "alternatives", "consequences"],
                "properties": {
                    "decision": { "type": "string" },
                    "reason": { "type": "string" },
                    "alternatives": { "type": "array", "items": { "type": "string" } },
                    "consequences": { "type": "array", "items": { "type": "string" } }
                }
            },
            "gotcha_content": {
                "type": "object",
                "additionalProperties": false,
                "required": ["problem", "cause", "resolution", "avoidance"],
                "properties": {
                    "problem": { "type": "string" },
                    "cause": { "type": "string" },
                    "resolution": { "type": "string" },
                    "avoidance": { "type": "string" }
                }
            },
            "procedure_content": {
                "type": "object",
                "additionalProperties": false,
                "required": ["trigger", "preconditions", "steps", "decision_points", "validation", "failure_handling", "expected_outcome"],
                "properties": {
                    "trigger": { "type": "string" },
                    "preconditions": { "type": "array", "items": { "type": "string" } },
                    "steps": { "type": "array", "items": { "type": "string" } },
                    "decision_points": { "type": "array", "items": { "type": "string" } },
                    "validation": { "type": "array", "items": { "type": "string" } },
                    "failure_handling": { "type": "array", "items": { "type": "string" } },
                    "expected_outcome": { "type": "string" }
                }
            },
            "operation": {
                "type": "object",
                "additionalProperties": false,
                "required": ["operation", "target_memory_ids", "type", "title", "scope_candidate", "scope_confidence", "scope_reason", "confidence_signal", "applicability", "content", "evidence_event_ids", "contradicting_event_ids"],
                "properties": {
                    "operation": { "type": "string", "enum": ["create", "reinforce", "merge", "supersede", "no-op"] },
                    "target_memory_ids": { "type": "array", "items": { "type": "string", "format": "uuid" } },
                    "type": { "type": "string", "enum": ["fact", "decision", "procedure", "gotcha"] },
                    "title": { "type": "string" },
                    "scope_candidate": { "type": "string", "enum": ["project", "global"] },
                    "scope_confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "scope_reason": { "type": "string" },
                    "confidence_signal": { "type": "number", "minimum": 0, "maximum": 1 },
                    "applicability": { "$ref": "#/$defs/applicability" },
                    "content": { "anyOf": [
                        { "$ref": "#/$defs/empty_content" },
                        { "$ref": "#/$defs/fact_content" },
                        { "$ref": "#/$defs/decision_content" },
                        { "$ref": "#/$defs/gotcha_content" },
                        { "$ref": "#/$defs/procedure_content" }
                    ] },
                    "evidence_event_ids": { "type": "array", "items": { "type": "string" } },
                    "contradicting_event_ids": { "type": "array", "items": { "type": "string" } }
                }
            }
        }
    })
}

fn compiler_system_prompt() -> &'static str {
    "Consolidate durable reusable knowledge against existing related memories. Return zero operations when evidence is insufficient. Use only fact, decision, procedure, or gotcha. Every create, reinforce, merge, and supersede must cite supplied event IDs. Use target IDs only from supplied related memories. Use supersede for contradictions and preserve prior knowledge. Do not silently recreate forgotten knowledge. Global scope requires strong evidence and a reason; otherwise use project scope. Never write Markdown directly; return structured content only."
}

fn invalid_schema(message: impl ToString) -> LlmError {
    LlmError {
        kind: LlmErrorKind::InvalidSchema,
        message: message.to_string(),
    }
}

fn internal(message: impl ToString) -> LlmError {
    LlmError {
        kind: LlmErrorKind::Internal,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use menvane_domain::{
        EvidenceItem, EvidenceKind, ProviderCapabilities, ProviderHealth, StructuredResponse,
    };

    use super::*;

    struct FakeProvider {
        responses: Mutex<VecDeque<Value>>,
        requests: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn generate_structured(
            &self,
            request: LlmRequest,
            _schema: JsonSchema,
        ) -> Result<StructuredResponse, LlmError> {
            self.requests
                .lock()
                .unwrap()
                .push(serde_json::from_str(&request.prompt).unwrap());
            Ok(StructuredResponse {
                value: self.responses.lock().unwrap().pop_front().unwrap(),
                provider: "fake".to_owned(),
                model: "deterministic".to_owned(),
            })
        }

        async fn health(&self) -> ProviderHealth {
            ProviderHealth::Ready
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                structured_output: true,
                json_schema: true,
                embeddings: false,
            }
        }

        fn name(&self) -> &'static str {
            "fake"
        }

        fn model(&self) -> &str {
            "deterministic"
        }
    }

    fn operation(content: Value) -> Value {
        json!({
            "operation": "create",
            "target_memory_ids": [],
            "type": "fact",
            "title": "Migration validation",
            "scope_candidate": "project",
            "scope_confidence": 0.95,
            "scope_reason": "Observed in this project",
            "confidence_signal": 0.9,
            "applicability": { "languages": [], "frameworks": [], "tools": [], "databases": [], "platforms": [] },
            "content": content,
            "evidence_event_ids": ["validation"],
            "contradicting_event_ids": []
        })
    }

    #[tokio::test]
    async fn accepts_zero_operations_without_forcing_creation() {
        let compiler = MemoryCompiler::new(Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([json!({ "operations": [] })])),
            requests: Mutex::new(Vec::new()),
        }));
        let result = compiler.compile(input()).await.unwrap();
        assert!(result.operations.is_empty());
    }

    #[tokio::test]
    async fn retries_one_schema_mismatch_and_builds_fact_markdown() {
        let valid = json!({ "operations": [operation(json!({ "statement": "tests passed" }))] });
        let compiler = MemoryCompiler::new(Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([json!({ "unexpected": [] }), valid])),
            requests: Mutex::new(Vec::new()),
        }));
        let result = compiler.compile(input()).await.unwrap();
        assert_eq!(result.operations.len(), 1);
        assert_eq!(result.memories[0].scope, Scope::Project);
        assert_eq!(result.memories[0].body, "tests passed");
    }

    #[test]
    fn compiler_schema_is_structurally_strict() {
        assert_schema_is_strict(&compiler_schema());
    }

    #[tokio::test]
    async fn compiler_receives_packet_and_related_memories() {
        let provider = Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([json!({ "operations": [] })])),
            requests: Mutex::new(Vec::new()),
        });
        MemoryCompiler::new(provider.clone())
            .compile(input())
            .await
            .unwrap();
        let request = provider.requests.lock().unwrap().pop().unwrap();
        assert!(request.get("episode_evidence").is_some());
        assert!(request.get("existing_related_memories").is_some());
        assert_eq!(request["episode_evidence"]["goal"]["event_id"], "goal");
    }

    fn assert_schema_is_strict(schema: &Value) {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            if schema.get("type").and_then(Value::as_str) == Some("object") {
                assert_eq!(
                    schema.get("additionalProperties").and_then(Value::as_bool),
                    Some(false)
                );
                let required: std::collections::HashSet<_> = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|values| values.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                for key in properties.keys() {
                    assert!(required.contains(key.as_str()));
                }
            }
            for property in properties.values() {
                assert_schema_is_strict(property);
            }
        }
        if let Some(items) = schema.get("items") {
            assert_schema_is_strict(items);
        }
        if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
            for value in any_of {
                assert_schema_is_strict(value);
            }
        }
        if let Some(defs) = schema.get("$defs").and_then(Value::as_object) {
            for value in defs.values() {
                assert_schema_is_strict(value);
            }
        }
    }

    fn input() -> CompilationInput {
        CompilationInput {
            evidence: EpisodeEvidencePacket {
                episode_id: Uuid::from_u128(1),
                goal: EvidenceItem {
                    event_id: "goal".to_owned(),
                    kind: EvidenceKind::Goal,
                    timestamp: chrono::Utc::now(),
                    content: "A migration was validated.".to_owned(),
                    importance: 1.0,
                },
                prompts: Vec::new(),
                actions: Vec::new(),
                decisions: Vec::new(),
                discoveries: Vec::new(),
                errors: Vec::new(),
                validations: vec![EvidenceItem {
                    event_id: "validation".to_owned(),
                    kind: EvidenceKind::Validation,
                    timestamp: chrono::Utc::now(),
                    content: "tests passed".to_owned(),
                    importance: 1.0,
                }],
                files: Vec::new(),
                compaction_context: Vec::new(),
                unresolved_questions: Vec::new(),
            },
            existing_related_memories: Vec::new(),
            technology_profile: json!({ "databases": ["sqlite"] }),
            source_session: None,
            source_episode: None,
        }
    }
}
