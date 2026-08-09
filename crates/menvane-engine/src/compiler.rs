use std::sync::Arc;
use std::time::Duration;

use menvane_domain::{
    Applicability, EpisodeEvidencePacket, JsonSchema, LlmError, LlmErrorKind, LlmProvider,
    LlmRequest, MemoryType, Scope,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CompilationInput {
    pub evidence: EpisodeEvidencePacket,
    pub existing_related_memories: Vec<String>,
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

pub struct CompilationResult {
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
        let request = LlmRequest {
            system: compiler_system_prompt().to_owned(),
            prompt: serde_json::to_string_pretty(&json!({
                "episode_evidence": input.evidence,
                "existing_related_memories": input.existing_related_memories,
                "technology_profile": input.technology_profile
            }))
            .map_err(internal)?,
            timeout: Duration::from_secs(120),
        };
        let schema = JsonSchema(compiler_schema());
        let mut last_error = None;
        for _ in 0..2 {
            let response = self
                .provider
                .generate_structured(request.clone(), schema.clone())
                .await?;
            match parse_response(response.value) {
                Ok(memories) => {
                    return Ok(CompilationResult {
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
    memories: Vec<Candidate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    #[serde(rename = "type")]
    memory_type: String,
    title: String,
    scope_candidate: String,
    scope_confidence: f64,
    confidence: f64,
    #[serde(default)]
    applies_to: Applicability,
    content: Value,
    #[serde(default)]
    evidence: Vec<String>,
}

fn parse_response(value: Value) -> Result<Vec<CompiledMemory>, LlmError> {
    let response: CompilerResponse = serde_json::from_value(value)
        .map_err(|error| invalid_schema(format!("compiler schema mismatch: {error}")))?;
    response
        .memories
        .into_iter()
        .map(|candidate| {
            let memory_type = candidate
                .memory_type
                .parse::<MemoryType>()
                .map_err(|error| invalid_schema(error.to_string()))?;
            if memory_type == MemoryType::Session {
                return Err(invalid_schema("compiler cannot create session memories"));
            }
            if !(0.0..=1.0).contains(&candidate.confidence)
                || !(0.0..=1.0).contains(&candidate.scope_confidence)
            {
                return Err(invalid_schema("confidence values must be between 0 and 1"));
            }
            let scope =
                if candidate.scope_candidate == "global" && candidate.scope_confidence >= 0.8 {
                    Scope::Global
                } else if candidate.scope_candidate == "project"
                    || candidate.scope_candidate == "global"
                {
                    Scope::Project
                } else {
                    return Err(invalid_schema("scope_candidate must be project or global"));
                };
            let body = content_markdown(memory_type, &candidate.content)?;
            Ok(CompiledMemory {
                memory_type,
                title: candidate.title,
                scope,
                scope_confidence: candidate.scope_confidence,
                confidence: candidate.confidence,
                applies_to: candidate.applies_to,
                body,
                evidence: candidate.evidence,
            })
        })
        .collect()
}

fn content_markdown(memory_type: MemoryType, content: &Value) -> Result<String, LlmError> {
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
        "required": ["memories"],
        "properties": {
            "memories": {
                "type": "array",
                "items": {
                    "anyOf": [
                        { "$ref": "#/$defs/fact" },
                        { "$ref": "#/$defs/decision" },
                        { "$ref": "#/$defs/gotcha" },
                        { "$ref": "#/$defs/procedure" }
                    ]
                }
            }
        },
        "$defs": {
            "applies_to": {
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
            "fact": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "title", "scope_candidate", "scope_confidence", "confidence", "applies_to", "content", "evidence"],
                "properties": {
                    "type": { "type": "string", "enum": ["fact"] },
                    "title": { "type": "string" },
                    "scope_candidate": { "type": "string", "enum": ["project", "global"] },
                    "scope_confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "applies_to": { "$ref": "#/$defs/applies_to" },
                    "content": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["statement"],
                        "properties": {
                            "statement": { "type": "string" }
                        }
                    },
                    "evidence": { "type": "array", "items": { "type": "string" } }
                }
            },
            "decision": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "title", "scope_candidate", "scope_confidence", "confidence", "applies_to", "content", "evidence"],
                "properties": {
                    "type": { "type": "string", "enum": ["decision"] },
                    "title": { "type": "string" },
                    "scope_candidate": { "type": "string", "enum": ["project", "global"] },
                    "scope_confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "applies_to": { "$ref": "#/$defs/applies_to" },
                    "content": {
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
                    "evidence": { "type": "array", "items": { "type": "string" } }
                }
            },
            "gotcha": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "title", "scope_candidate", "scope_confidence", "confidence", "applies_to", "content", "evidence"],
                "properties": {
                    "type": { "type": "string", "enum": ["gotcha"] },
                    "title": { "type": "string" },
                    "scope_candidate": { "type": "string", "enum": ["project", "global"] },
                    "scope_confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "applies_to": { "$ref": "#/$defs/applies_to" },
                    "content": {
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
                    "evidence": { "type": "array", "items": { "type": "string" } }
                }
            },
            "procedure": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "title", "scope_candidate", "scope_confidence", "confidence", "applies_to", "content", "evidence"],
                "properties": {
                    "type": { "type": "string", "enum": ["procedure"] },
                    "title": { "type": "string" },
                    "scope_candidate": { "type": "string", "enum": ["project", "global"] },
                    "scope_confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "applies_to": { "$ref": "#/$defs/applies_to" },
                    "content": {
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
                    "evidence": { "type": "array", "items": { "type": "string" } }
                }
            }
        }
    })
}

fn compiler_system_prompt() -> &'static str {
    "Consolidate only durable, reusable knowledge supported by the episode evidence packet. Return zero memories when evidence is temporary or insufficient. Use only fact, decision, procedure, or gotcha. Preserve only event IDs supplied by evidence when reporting evidence. Prefer project scope whenever global validity is uncertain. Never include private reasoning or unsupported claims. Procedure content must include trigger, preconditions, steps, decision_points, validation, failure_handling, and expected_outcome. Leave all applicability dimensions (languages, frameworks, tools, databases, platforms) empty when the memory is not tied to specific technologies."
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

    #[tokio::test]
    async fn accepts_zero_memories_without_forcing_creation() {
        let compiler = MemoryCompiler::new(Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([json!({ "memories": [] })])),
            requests: Mutex::new(Vec::new()),
        }));
        let result = compiler.compile(input()).await.unwrap();
        assert!(result.memories.is_empty());
    }

    #[tokio::test]
    async fn retries_one_schema_mismatch_and_builds_procedure_markdown() {
        let valid = json!({
            "memories": [{
                "type": "procedure",
                "title": "Validate migrations",
                "scope_candidate": "global",
                "scope_confidence": 0.6,
                "confidence": 0.9,
                "applies_to": { "languages": [], "frameworks": [], "tools": [], "databases": ["sqlite"], "platforms": [] },
                "content": {
                    "trigger": "Before applying a migration",
                    "preconditions": ["A backup exists"],
                    "steps": ["Run migration", "Inspect schema"],
                    "decision_points": ["Rollback when validation fails"],
                    "validation": ["Schema matches expectation"],
                    "failure_handling": ["Restore the backup"],
                    "expected_outcome": "Migration is verified"
                },
                "evidence": ["test passed"]
            }]
        });
        let compiler = MemoryCompiler::new(Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([json!({ "unexpected": [] }), valid])),
            requests: Mutex::new(Vec::new()),
        }));
        let result = compiler.compile(input()).await.unwrap();
        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].scope, Scope::Project);
        assert!(result.memories[0].body.contains("1. Run migration"));
        assert!(result.memories[0].body.contains("## Failure handling"));
    }

    #[tokio::test]
    async fn accepts_non_technical_fact_with_empty_applicability() {
        let valid = json!({
            "memories": [{
                "type": "fact",
                "title": "Meeting notes belong in durable memory",
                "scope_candidate": "project",
                "scope_confidence": 0.95,
                "confidence": 0.8,
                "applies_to": { "languages": [], "frameworks": [], "tools": [], "databases": [], "platforms": [] },
                "content": { "statement": "Action items discussed in the weekly review are stored as durable project facts." },
                "evidence": ["reviewed during session"]
            }]
        });
        let compiler = MemoryCompiler::new(Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([valid])),
            requests: Mutex::new(Vec::new()),
        }));
        let result = compiler.compile(input()).await.unwrap();
        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].memory_type, MemoryType::Fact);
        assert!(result.memories[0].body.contains("Action items discussed"));
    }

    #[test]
    fn compiler_schema_is_structurally_strict() {
        let schema = compiler_schema();
        assert_schema_is_strict(&schema);
    }

    #[tokio::test]
    async fn compiler_receives_only_packet_evidence() {
        let provider = Arc::new(FakeProvider {
            responses: Mutex::new(VecDeque::from([json!({ "memories": [] })])),
            requests: Mutex::new(Vec::new()),
        });
        let compiler = MemoryCompiler::new(provider.clone());
        compiler.compile(input()).await.unwrap();
        let request = provider.requests.lock().unwrap().pop().unwrap();
        assert!(request.get("episode_evidence").is_some());
        assert!(request.get("important_prompts").is_none());
        assert_eq!(request["episode_evidence"]["goal"]["event_id"], "goal");
    }

    fn assert_schema_is_strict(schema: &Value) {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            if schema.get("type").and_then(Value::as_str) == Some("object") {
                assert!(
                    schema.get("additionalProperties").and_then(Value::as_bool) == Some(false),
                    "object schema must set additionalProperties: false"
                );
                let required: std::collections::HashSet<_> = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|values| values.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                for key in properties.keys() {
                    assert!(
                        required.contains(key.as_str()),
                        "required must include every property; missing '{key}'"
                    );
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
            for branch in any_of {
                assert_schema_is_strict(branch);
            }
        }
        if let Some(defs) = schema.get("$defs").and_then(Value::as_object) {
            for def in defs.values() {
                assert_schema_is_strict(def);
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
