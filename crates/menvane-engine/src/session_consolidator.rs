use std::sync::Arc;
use std::time::{Duration, Instant};

use menvane_domain::{
    ConsolidationExecution, ConsolidationResult, JsonSchema, LlmError, LlmErrorKind, LlmProvider,
    LlmRequest, ResponseUsage, consolidation_result_schema, preserve_handoff_transitions,
};
use serde_json::Value;
use uuid::Uuid;

pub const MAX_HANDOFF_SUMMARY_BYTES: usize = 2_000;

pub use menvane_domain::ConsolidationPacket;

#[derive(Debug, Clone)]
pub struct ConsolidationOutcome {
    pub response: ConsolidationResult,
    pub provider: String,
    pub model: String,
    pub execution: ConsolidationExecution,
}

pub struct SessionConsolidator {
    provider: Arc<dyn LlmProvider>,
    prompt: String,
}

impl SessionConsolidator {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            provider,
            prompt: CONSOLIDATION_SYSTEM_PROMPT.to_owned(),
        }
    }

    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    pub async fn consolidate(
        &self,
        packet: &ConsolidationPacket,
    ) -> Result<ConsolidationOutcome, LlmError> {
        if !self.provider.capabilities().structured_output
            || !self.provider.capabilities().json_schema
        {
            return Err(LlmError {
                kind: LlmErrorKind::UnsupportedCapability,
                message: "session consolidation requires JSON Schema structured output".to_owned(),
            });
        }
        let prompt = serde_json::to_string_pretty(packet).map_err(internal)?;
        let started = Instant::now();
        let mut last_error = None;
        for attempt in 1..=2 {
            let system = if attempt == 1 {
                self.prompt.clone()
            } else {
                format!(
                    "{} Repair the previous response: {}",
                    self.prompt,
                    last_error
                        .as_ref()
                        .map_or("invalid structured output", |error: &LlmError| error
                            .message
                            .as_str())
                )
            };
            let response = match self
                .provider
                .generate_structured(
                    LlmRequest {
                        system,
                        prompt: prompt.clone(),
                        timeout: Duration::from_secs(180),
                    },
                    JsonSchema(consolidation_result_schema()),
                )
                .await
            {
                Ok(response) => response,
                Err(error) if error.kind == LlmErrorKind::InvalidSchema && attempt == 1 => {
                    last_error = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            };
            match parse_response(response.value, packet) {
                Ok(result) => {
                    let execution = execution(
                        &response.provider,
                        &response.model,
                        started.elapsed(),
                        attempt,
                        &prompt,
                        &result,
                        response.usage.as_ref(),
                    );
                    return Ok(ConsolidationOutcome {
                        response: result,
                        provider: response.provider,
                        model: response.model,
                        execution,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| invalid_schema("consolidation response was invalid")))
    }
}

fn parse_response(
    value: Value,
    packet: &ConsolidationPacket,
) -> Result<ConsolidationResult, LlmError> {
    let result: ConsolidationResult = serde_json::from_value(value)
        .map_err(|error| invalid_schema(format!("consolidation schema mismatch: {error}")))?;
    preserve_handoff_transitions(result, packet).map_err(|error| invalid_schema(error.to_string()))
}

fn execution(
    provider: &str,
    model: &str,
    elapsed: Duration,
    attempts: u32,
    input: &str,
    result: &ConsolidationResult,
    usage: Option<&ResponseUsage>,
) -> ConsolidationExecution {
    let output = serde_json::to_vec(result).map_or(0, |value| value.len());
    ConsolidationExecution {
        provider: provider.to_owned(),
        model: model.to_owned(),
        latency_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
        attempts,
        input_bytes: input.len() as u64,
        output_bytes: output as u64,
        input_tokens: usage.and_then(|value| value.input_tokens),
        output_tokens: usage.and_then(|value| value.output_tokens),
        credits: usage.and_then(|value| value.credits),
    }
}

pub const CONSOLIDATION_SYSTEM_PROMPT: &str = "Summarize the chronological session into intentions, actions, outcome, result, continuity, and candidate learnings. Apply an explicit operation to every supplied handoff item. Create only reusable context or playbook knowledge supported by supplied evidence. Return structured JSON only.";

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

#[allow(dead_code)]
fn _session_id(packet: &ConsolidationPacket) -> Uuid {
    packet.session_id
}
