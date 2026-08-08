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
