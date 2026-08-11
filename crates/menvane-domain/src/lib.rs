mod memory;
mod project;
mod retrieval;
mod session;

pub use memory::{Applicability, Memory, MemoryMetadata, MemoryStatus, MemoryType, Scope};
pub use project::{Project, ProjectTechnologies};
pub use retrieval::{
    EmbeddingError, EmbeddingProvider, JsonSchema, LlmError, LlmErrorKind, LlmProvider, LlmRequest,
    ProviderCapabilities, ProviderHealth, StructuredResponse,
};
pub use session::ReinforcementSignal;
pub use session::{
    ConsolidationResponse, EpisodeEvidencePacket, EpisodeState, EvidenceItem, EvidenceKind, Goal,
    GoalOperation, GoalOperationKind, GoalState, HandoffReplacement, HandoffStatus,
    HandoffValidation, IntentClassificationSource, MemoryOperation, NormalizedEvent,
    NormalizedEventKind, NormalizedEventOrigin, NormalizedEventRole, NormalizedSession,
    ProjectHandoff, PromptIntent, PromptIntentKind, SessionImporter, SessionState, TaskEpisode,
    TaskHandoff,
};

pub type EventOrigin = NormalizedEventOrigin;
pub type EventRole = NormalizedEventRole;
