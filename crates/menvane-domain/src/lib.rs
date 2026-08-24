mod consolidation;
mod handoff;
mod memory;
mod project;
mod retrieval;
mod session;
mod summary;

pub use consolidation::{
    ConsolidationExecution, ConsolidationPacket, ConsolidationResult, ConsolidationValidationError,
    KnowledgeContent, KnowledgeOperation, KnowledgeOperationKind, MemoryContent, PlaybookContent,
    RelatedMemory, RelatedSummary, consolidation_result_schema, preserve_handoff_transitions,
    validate_consolidation_result, validate_knowledge_promotion,
};
pub use handoff::{
    HandoffCreation, HandoffItem, HandoffItemKind, HandoffItemOperation, HandoffItemSource,
    HandoffReplacement, HandoffTransition, HandoffUpdate, NewHandoffItem,
};
pub use memory::{
    Applicability, KnowledgeMetadata, KnowledgeRecord, KnowledgeType, MemoryStatus,
    ParseKnowledgeTypeError, Scope,
};
pub use project::{Project, ProjectTechnologies};
pub use retrieval::{
    EmbeddingError, EmbeddingProvider, JsonSchema, LlmError, LlmErrorKind, LlmProvider, LlmRequest,
    ProviderCapabilities, ProviderHealth, ResponseUsage, StructuredResponse,
};
pub use session::ReinforcementSignal;
pub use session::{
    NormalizedEvent, NormalizedEventKind, NormalizedEventOrigin, NormalizedEventRole,
    NormalizedSession, SessionImporter, SessionMetadata, SessionState, SummaryStatus,
};
pub use summary::{ContinuityDisposition, ContinuityItem, EpisodicSummary, SummaryOutcome};

pub type EventOrigin = NormalizedEventOrigin;
pub type EventRole = NormalizedEventRole;
