use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub structured_output: bool,
    pub json_schema: bool,
    pub embeddings: bool,
}

pub trait EmbeddingProvider: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
    fn name(&self) -> &str;
    fn model(&self) -> &str;
}

#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("embedding provider is unavailable: {0}")]
    Unavailable(String),
    #[error("embedding request failed: {0}")]
    Request(String),
    #[error("embedding response is invalid: {0}")]
    InvalidResponse(String),
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system: String,
    pub prompt: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct JsonSchema(pub Value);

#[derive(Debug, Clone)]
pub struct StructuredResponse {
    pub value: Value,
    pub provider: String,
    pub model: String,
    pub usage: Option<ResponseUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub credits: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderHealth {
    Ready,
    BinaryMissing,
    NotAuthenticated,
    ModelUnavailable,
    MissingApiKey,
    Incompatible,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorKind {
    Unavailable,
    Authentication,
    RateLimited,
    Network,
    UnsupportedCapability,
    InvalidInput,
    InvalidSchema,
    Internal,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct LlmError {
    pub kind: LlmErrorKind,
    pub message: String,
}

impl LlmError {
    pub fn fallback_allowed(&self) -> bool {
        matches!(
            self.kind,
            LlmErrorKind::Unavailable
                | LlmErrorKind::Authentication
                | LlmErrorKind::RateLimited
                | LlmErrorKind::Network
                | LlmErrorKind::UnsupportedCapability
        )
    }
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError>;

    async fn health(&self) -> ProviderHealth;
    fn capabilities(&self) -> ProviderCapabilities;
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
}
