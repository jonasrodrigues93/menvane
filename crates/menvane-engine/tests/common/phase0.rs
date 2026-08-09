use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use menvane_domain::{
    JsonSchema, LlmError, LlmProvider, LlmRequest, NormalizedSession, ProviderCapabilities,
    ProviderHealth, StructuredResponse,
};
use menvane_engine::CompilationInput;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct Corpus {
    pub schema_version: u32,
    pub fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
pub struct Fixture {
    pub id: String,
    pub categories: Vec<String>,
    pub session: NormalizedSession,
    pub expected: ExpectedOutputs,
    pub compiler_response: Value,
}

#[derive(Debug, Deserialize)]
pub struct ExpectedOutputs {
    pub episodes: Value,
    pub intents: Value,
    pub handoffs: Value,
    pub evidence: Value,
    pub memory_operations: Value,
    pub durable_memories: Vec<ExpectedMemory>,
    pub recall: Vec<RecallExpectation>,
}

#[derive(Debug, Deserialize)]
pub struct ExpectedMemory {
    pub memory_type: String,
    pub title: String,
    pub scope: String,
    pub confidence: f64,
    pub body: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecallExpectation {
    pub query: String,
    pub expected_titles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderCall {
    pub fixture_id: String,
    pub input_bytes: usize,
}

pub struct FixtureProvider {
    responses: HashMap<String, Value>,
    calls: Mutex<Vec<ProviderCall>>,
}

impl FixtureProvider {
    pub fn new(corpus: &Corpus) -> Self {
        Self {
            responses: corpus
                .fixtures
                .iter()
                .map(|fixture| (fixture.id.clone(), fixture.compiler_response.clone()))
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<ProviderCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for FixtureProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        _schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        let input: Value = serde_json::from_str(&request.prompt).map_err(|error| LlmError {
            kind: menvane_domain::LlmErrorKind::Internal,
            message: error.to_string(),
        })?;
        let fixture_id = input
            .pointer("/technology_profile/fixture_id")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError {
                kind: menvane_domain::LlmErrorKind::InvalidInput,
                message: "fixture_id is missing from the evaluation input".to_owned(),
            })?;
        let response = self
            .responses
            .get(fixture_id)
            .cloned()
            .ok_or_else(|| LlmError {
                kind: menvane_domain::LlmErrorKind::InvalidInput,
                message: format!("unknown fixture: {fixture_id}"),
            })?;
        self.calls.lock().unwrap().push(ProviderCall {
            fixture_id: fixture_id.to_owned(),
            input_bytes: request.prompt.len(),
        });
        Ok(StructuredResponse {
            value: response,
            provider: "fixture".to_owned(),
            model: "phase-0-deterministic".to_owned(),
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
        "fixture"
    }

    fn model(&self) -> &str {
        "phase-0-deterministic"
    }
}

pub fn load_corpus() -> Corpus {
    serde_json::from_str(include_str!("../fixtures/phase0/corpus.json")).unwrap()
}

pub fn compilation_input(fixture: &Fixture) -> CompilationInput {
    let prompts = fixture
        .session
        .events
        .iter()
        .filter(|event| event.kind == menvane_domain::NormalizedEventKind::UserPrompt)
        .filter_map(|event| event.bounded_input.clone())
        .collect();
    let tool_events = fixture
        .session
        .events
        .iter()
        .filter(|event| event.kind == menvane_domain::NormalizedEventKind::ToolCompleted)
        .map(|event| {
            json!({
                "tool": event.tool_family,
                "input": event.bounded_input,
                "output": event.bounded_output,
                "success": event.success,
                "path": event.attributed_path,
            })
            .to_string()
        })
        .collect();
    let errors = fixture
        .session
        .events
        .iter()
        .filter(|event| event.success == Some(false))
        .filter_map(|event| event.bounded_output.clone())
        .collect();
    let validation_results = fixture
        .session
        .events
        .iter()
        .filter(|event| event.success == Some(true))
        .filter_map(|event| event.tool_family.clone())
        .collect();
    CompilationInput {
        session_summary: format!("fixture:{}", fixture.id),
        important_prompts: prompts,
        important_tool_events: tool_events,
        errors,
        decisions: Vec::new(),
        validation_results,
        existing_related_memories: Vec::new(),
        technology_profile: json!({ "fixture_id": fixture.id }),
        source_session: None,
    }
}
