use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use menvane_domain::{
    Applicability, EpisodeEvidencePacket, EvidenceItem, EvidenceKind, JsonSchema, LlmError,
    LlmProvider, LlmRequest, Memory, MemoryMetadata, MemoryStatus, MemoryType,
    ProviderCapabilities, ProviderHealth, Scope, StructuredResponse,
};
use menvane_engine::{
    CompilationInput, CompilationResult, MemoryCompiler, Menvane, RelatedMemory,
    RelatedMemoryProvenance, WriteMemory,
};
use menvane_store::{IndexStore, MarkdownStore};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

struct FixtureProvider {
    responses: Mutex<VecDeque<Value>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LlmProvider for FixtureProvider {
    async fn generate_structured(
        &self,
        _request: LlmRequest,
        _schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(StructuredResponse {
            value: self.responses.lock().unwrap().pop_front().unwrap(),
            provider: "fixture".to_owned(),
            model: "phase6".to_owned(),
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
        "phase6"
    }
}

#[tokio::test]
async fn equivalent_content_with_different_titles_reinforces_instead_of_duplicating() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let existing = write_global(&menvane, "Old title", "SQLite is derived.", 0.4);
    let result = compile(
        vec![related(&existing)],
        operation(
            "reinforce",
            &[existing.metadata.id],
            "New title",
            "SQLite is derived.",
            &[],
        ),
    )
    .await;
    menvane
        .apply_compilation_result(temporary.path(), result, None, Some(Uuid::from_u128(1)))
        .unwrap();
    let memories = menvane.all_memories().unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].metadata.confidence, 0.9);
    assert_eq!(memories[0].title, "Old title");
}

#[tokio::test]
async fn complementary_evidence_merges_without_losing_historical_targets() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let first = write_global_with_sources(
        &menvane,
        temporary.path().join("home").as_path(),
        "First",
        "Use WAL.",
        0.6,
        &[Uuid::from_u128(101)],
    );
    let second = write_global_with_sources(
        &menvane,
        temporary.path().join("home").as_path(),
        "Second",
        "Set a busy timeout.",
        0.7,
        &[Uuid::from_u128(102)],
    );
    let result = compile(
        vec![related(&first), related(&second)],
        operation(
            "merge",
            &[first.metadata.id, second.metadata.id],
            "SQLite concurrency safeguards",
            "Use WAL and set a busy timeout.",
            &[],
        ),
    )
    .await;
    menvane
        .apply_compilation_result(temporary.path(), result, None, Some(Uuid::from_u128(2)))
        .unwrap();
    let memories = menvane.all_memories().unwrap();
    assert_eq!(memories.len(), 2);
    assert!(memories.iter().any(|memory| {
        memory.metadata.status == MemoryStatus::Historical && memory.body == "Set a busy timeout."
    }));
    assert!(memories.iter().any(|memory| {
        memory.metadata.status == MemoryStatus::Active
            && memory.body.contains("WAL")
            && memory.body.contains("busy timeout")
            && memory.metadata.source_sessions == vec![Uuid::from_u128(101), Uuid::from_u128(102)]
    }));
}

#[tokio::test]
async fn contradictory_evidence_supersedes_the_correct_target() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let old = write_global(&menvane, "Old", "The service uses port 4100.", 0.8);
    let response = json!({
        "operations": [operation_value(
            "supersede", &[old.metadata.id], "Service port", "The service uses port 4200.", &["validation"]
        )]
    });
    let result = compile_response(vec![related(&old)], response)
        .await
        .unwrap();
    menvane
        .apply_compilation_result(temporary.path(), result, None, Some(Uuid::from_u128(3)))
        .unwrap();
    let memories = menvane.all_memories().unwrap();
    assert_eq!(memories.len(), 2);
    assert!(memories.iter().any(|memory| {
        memory.metadata.id == old.metadata.id && memory.metadata.status == MemoryStatus::Superseded
    }));
    assert!(memories.iter().any(|memory| {
        memory.metadata.status == MemoryStatus::Active && memory.body.contains("4200")
    }));
}

#[tokio::test]
async fn supersede_without_contradicting_evidence_retries_once_then_fails() {
    let old_id = Uuid::from_u128(200);
    let calls = Arc::new(AtomicUsize::new(0));
    let response = json!({
        "operations": [operation_value(
            "supersede", &[old_id], "Service port", "The service uses port 4200.", &[]
        )]
    });
    let result = MemoryCompiler::new(Arc::new(FixtureProvider {
        responses: Mutex::new(VecDeque::from([response.clone(), response])),
        calls: calls.clone(),
    }))
    .compile(CompilationInput {
        evidence: packet(),
        existing_related_memories: vec![RelatedMemory {
            id: old_id,
            memory_type: MemoryType::Fact,
            scope: Scope::Global,
            status: MemoryStatus::Active,
            confidence: 0.8,
            applicability: Applicability {
                databases: vec!["sqlite".to_owned()],
                ..Applicability::default()
            },
            title: "Old service port".to_owned(),
            body: "The service uses port 4100.".to_owned(),
            provenance: RelatedMemoryProvenance {
                source_session_count: 0,
                supersession_count: 0,
            },
        }],
        technology_profile: json!({ "databases": ["sqlite"] }),
        source_session: None,
        source_episode: Some(Uuid::from_u128(9)),
    })
    .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn same_title_with_unrelated_content_does_not_supersede() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let old = write_global(&menvane, "Service port", "The service uses port 4100.", 0.8);
    let result = compile(
        vec![related(&old)],
        operation(
            "create",
            &[],
            "Service port",
            "The service uses port 4200.",
            &[],
        ),
    )
    .await;
    menvane
        .apply_compilation_result(temporary.path(), result, None, Some(Uuid::from_u128(4)))
        .unwrap();
    let memories = menvane.all_memories().unwrap();
    assert_eq!(memories.len(), 2);
    assert!(
        memories
            .iter()
            .all(|memory| memory.metadata.status == MemoryStatus::Active)
    );
}

#[tokio::test]
async fn forgotten_memory_requires_an_explicit_target_operation() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let forgotten = menvane
        .forget(
            write_global(
                &menvane,
                "Retired endpoint",
                "Do not recreate the retired endpoint.",
                0.8,
            )
            .metadata
            .id,
        )
        .unwrap();
    let result = compile_response(
        vec![related(&forgotten)],
        operation_value(
            "create",
            &[],
            "Retired endpoint",
            "Do not recreate the retired endpoint.",
            &[],
        ),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn partial_operation_retry_has_no_duplicate_effects() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let response = operation("create", &[], "Retry-safe fact", "Retry-safe content.", &[]);
    let first = compile_response(Vec::new(), json!({ "operations": [response.clone()] })).await;
    let second = compile_response(Vec::new(), json!({ "operations": [response] })).await;
    let first = first.unwrap();
    let second = second.unwrap();
    menvane
        .apply_compilation_result(temporary.path(), first, None, Some(Uuid::from_u128(5)))
        .unwrap();
    rusqlite::Connection::open(temporary.path().join("home/state.sqlite"))
        .unwrap()
        .execute("DELETE FROM compilation_operations", [])
        .unwrap();
    menvane
        .apply_compilation_result(temporary.path(), second, None, Some(Uuid::from_u128(5)))
        .unwrap();
    assert_eq!(menvane.all_memories().unwrap().len(), 1);
}

#[tokio::test]
async fn zero_operation_output_is_valid() {
    let result = compile_response(Vec::new(), json!({ "operations": [] })).await;
    assert!(result.unwrap().operations.is_empty());
}

#[test]
fn related_memory_retrieval_is_bounded_and_keeps_lifecycle_records() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let home = temporary.path().join("home");
    for status in [
        MemoryStatus::Active,
        MemoryStatus::Candidate,
        MemoryStatus::NeedsValidation,
        MemoryStatus::Superseded,
        MemoryStatus::Historical,
        MemoryStatus::Forgotten,
    ] {
        let memory = Memory {
            metadata: MemoryMetadata::new(
                MemoryType::Fact,
                Scope::Global,
                None,
                0.5,
                Vec::new(),
                Applicability {
                    databases: vec!["sqlite".to_owned()],
                    ..Applicability::default()
                },
            ),
            title: status.to_string(),
            body: "SQLite lifecycle evidence.".to_owned(),
        };
        let mut memory = memory;
        memory.metadata.status = status;
        persist_memory(&home, &memory);
    }
    let related = menvane
        .related_memories(
            temporary.path(),
            &packet(),
            &json!({ "databases": ["sqlite"] }),
            None,
        )
        .unwrap();
    assert!(
        related
            .iter()
            .any(|memory| memory.status == MemoryStatus::Forgotten)
    );
    let statuses = related
        .iter()
        .map(|memory| memory.status.to_string())
        .collect::<HashSet<_>>();
    assert_eq!(
        statuses,
        [
            "active",
            "candidate",
            "needs-validation",
            "superseded",
            "historical",
            "forgotten",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    let session = Memory {
        metadata: MemoryMetadata::new(
            MemoryType::Session,
            Scope::Global,
            None,
            0.5,
            Vec::new(),
            Applicability::default(),
        ),
        title: "Session lifecycle evidence".to_owned(),
        body: "SQLite lifecycle evidence.".to_owned(),
    };
    persist_memory(&home, &session);
    let related = menvane
        .related_memories(
            temporary.path(),
            &packet(),
            &json!({ "databases": ["sqlite"] }),
            None,
        )
        .unwrap();
    assert!(
        related
            .iter()
            .all(|memory| memory.memory_type != MemoryType::Session)
    );
    assert!(related.len() <= menvane_engine::RELATED_MEMORY_LIMIT);
    assert!(
        serde_json::to_vec(&related).unwrap().len() <= menvane_engine::RELATED_MEMORY_BUDGET_BYTES
    );
}

async fn compile(related: Vec<RelatedMemory>, operation: Value) -> CompilationResult {
    compile_response(related, json!({ "operations": [operation] }))
        .await
        .unwrap()
}

async fn compile_response(
    related: Vec<RelatedMemory>,
    response: Value,
) -> Result<CompilationResult, LlmError> {
    MemoryCompiler::new(Arc::new(FixtureProvider {
        responses: Mutex::new(VecDeque::from([response.clone(), response])),
        calls: Arc::new(AtomicUsize::new(0)),
    }))
    .compile(CompilationInput {
        evidence: packet(),
        existing_related_memories: related,
        technology_profile: json!({ "databases": ["sqlite"] }),
        source_session: None,
        source_episode: Some(Uuid::from_u128(9)),
    })
    .await
}

fn operation(
    kind: &str,
    targets: &[Uuid],
    title: &str,
    statement: &str,
    contradicting: &[&str],
) -> Value {
    operation_value(kind, targets, title, statement, contradicting)
}

fn operation_value(
    kind: &str,
    targets: &[Uuid],
    title: &str,
    statement: &str,
    contradicting: &[&str],
) -> Value {
    json!({
        "operation": kind,
        "target_memory_ids": targets,
        "type": "fact",
        "title": title,
        "scope_candidate": "global",
        "scope_confidence": 0.95,
        "scope_reason": "Observed directly in the evidence",
        "confidence_signal": 0.9,
        "applicability": { "languages": [], "frameworks": [], "tools": [], "databases": ["sqlite"], "platforms": [] },
        "content": { "statement": statement },
        "evidence_event_ids": ["validation"],
        "contradicting_event_ids": contradicting
    })
}

fn related(memory: &Memory) -> RelatedMemory {
    RelatedMemory {
        id: memory.metadata.id,
        memory_type: memory.metadata.memory_type,
        scope: memory.metadata.scope,
        status: memory.metadata.status.clone(),
        confidence: memory.metadata.confidence,
        applicability: memory.metadata.applies_to.clone(),
        title: memory.title.clone(),
        body: memory.body.clone(),
        provenance: RelatedMemoryProvenance {
            source_session_count: memory.metadata.source_sessions.len(),
            supersession_count: memory.metadata.supersedes.len(),
        },
    }
}

fn write_global(menvane: &Menvane, title: &str, body: &str, confidence: f64) -> Memory {
    menvane
        .write(
            std::path::Path::new("/tmp"),
            WriteMemory {
                title: title.to_owned(),
                body: body.to_owned(),
                memory_type: MemoryType::Fact,
                scope: Scope::Global,
                confidence,
                tags: Vec::new(),
                applies_to: Applicability {
                    databases: vec!["sqlite".to_owned()],
                    ..Applicability::default()
                },
            },
        )
        .unwrap()
}

fn write_global_with_sources(
    menvane: &Menvane,
    home: &std::path::Path,
    title: &str,
    body: &str,
    confidence: f64,
    source_sessions: &[Uuid],
) -> Memory {
    let mut memory = write_global(menvane, title, body, confidence);
    memory.metadata.source_sessions = source_sessions.to_vec();
    persist_memory(home, &memory);
    memory
}

fn persist_memory(home: &std::path::Path, memory: &Memory) {
    let markdown = MarkdownStore::new(home);
    let index = IndexStore::new(home.join("index.sqlite"));
    let path = match index.read_memory(&markdown, memory.metadata.id) {
        Ok((_, path)) => {
            markdown.update_memory(&path, memory).unwrap();
            path
        }
        Err(_) => markdown.write_memory(memory, None).unwrap(),
    };
    index.upsert_memory(memory, &path).unwrap();
}

fn packet() -> EpisodeEvidencePacket {
    EpisodeEvidencePacket {
        episode_id: Uuid::from_u128(9),
        goal: EvidenceItem {
            event_id: "goal".to_owned(),
            kind: EvidenceKind::Goal,
            timestamp: chrono::Utc::now(),
            content: "Validate SQLite lifecycle evidence".to_owned(),
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
            content: "validation passed".to_owned(),
            importance: 1.0,
        }],
        files: Vec::new(),
        compaction_context: Vec::new(),
        unresolved_questions: Vec::new(),
    }
}
