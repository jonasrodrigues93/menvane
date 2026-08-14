use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use menvane_domain::{
    Applicability, HandoffItem, HandoffItemKind, HandoffItemSource, JsonSchema, KnowledgeType,
    LlmError, LlmErrorKind, LlmProvider, LlmRequest, MemoryStatus, NormalizedEvent,
    NormalizedEventKind, ProviderCapabilities, ProviderHealth, ResponseUsage, Scope,
    StructuredResponse,
};
use menvane_engine::{CaptureOutcome, Menvane, ScopeSelection, WriteMemory};
use menvane_store::{MarkdownStore, SessionRepository};
use tempfile::TempDir;
use uuid::Uuid;

struct FakeLlmProvider {
    responses: Mutex<Vec<Result<serde_json::Value, LlmError>>>,
    calls: Mutex<Vec<LlmRequest>>,
}

impl FakeLlmProvider {
    fn new(responses: Vec<Result<serde_json::Value, LlmError>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl LlmProvider for FakeLlmProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        _schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        self.calls.lock().unwrap().push(request);
        match self.responses.lock().unwrap().remove(0) {
            Ok(value) => Ok(StructuredResponse {
                value,
                provider: "fake".to_owned(),
                model: "deterministic".to_owned(),
                usage: Some(ResponseUsage {
                    input_tokens: Some(11),
                    output_tokens: Some(7),
                    credits: Some(0.25),
                }),
            }),
            Err(error) => Err(error),
        }
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

#[test]
fn context_and_playbook_round_trip_through_markdown_and_search() {
    let (temporary, project, menvane) = setup_project();
    let context = menvane
        .write(
            &project,
            WriteMemory {
                title: "Derived index rebuild".to_owned(),
                body: "The index is derived from canonical Markdown.".to_owned(),
                knowledge_type: KnowledgeType::Context,
                scope: Scope::Project,
                tags: vec!["sqlite".to_owned()],
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    let playbook = menvane
        .write(
            &project,
            WriteMemory {
                title: "Rebuild the local index".to_owned(),
                body: "Trigger: when the index is missing\n\n1. Run reindex\n2. Verify search"
                    .to_owned(),
                knowledge_type: KnowledgeType::Playbook,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();

    assert_eq!(menvane.read(context.metadata.id).unwrap(), context);
    assert_eq!(menvane.read(playbook.metadata.id).unwrap(), playbook);
    assert_eq!(menvane.all_memories().unwrap().len(), 2);
    assert_eq!(
        menvane
            .search(&project, "canonical Markdown", ScopeSelection::Project, 10)
            .unwrap()[0]
            .knowledge_type,
        KnowledgeType::Context
    );
    assert_eq!(
        menvane
            .search(&project, "reindex search", ScopeSelection::Project, 10)
            .unwrap()[0]
            .knowledge_type,
        KnowledgeType::Playbook
    );

    let markdown = MarkdownStore::new(menvane.home());
    assert_eq!(markdown.memory_files().unwrap().len(), 2);
    drop(temporary);
}

#[test]
fn inferred_context_and_playbook_cross_the_promotion_barrier() {
    let (_temporary, project, _provider, menvane) =
        setup_provider(vec![Ok(promotion_result()), Ok(promotion_result())]);
    let timestamp = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    menvane
        .ingest_event(event(
            &project,
            "prompt",
            NormalizedEventKind::UserPrompt,
            timestamp,
            Some("deploy through the remote runner"),
            None,
        ))
        .unwrap();
    let mut tool = event(
        &project,
        "tool",
        NormalizedEventKind::ToolCompleted,
        timestamp + chrono::Duration::seconds(1),
        Some("deploy"),
        Some("deployment verified through the remote runner"),
    );
    tool.success = Some(true);
    menvane.ingest_event(tool).unwrap();
    menvane
        .ingest_event(event(
            &project,
            "end",
            NormalizedEventKind::SessionEnded,
            timestamp + chrono::Duration::seconds(2),
            None,
            None,
        ))
        .unwrap();
    process_next_job(&menvane);
    process_next_job(&menvane);

    let memories = menvane.all_memories().unwrap();
    assert_eq!(memories.len(), 2);
    assert_eq!(
        memories
            .iter()
            .find(|memory| memory.metadata.knowledge_type == KnowledgeType::Context)
            .unwrap()
            .metadata
            .status,
        MemoryStatus::Active
    );
    assert_eq!(
        memories
            .iter()
            .find(|memory| memory.metadata.knowledge_type == KnowledgeType::Playbook)
            .unwrap()
            .metadata
            .status,
        MemoryStatus::Candidate
    );
}

#[test]
fn playbook_application_is_independent_and_idempotent() {
    let (_temporary, project, provider, menvane) =
        setup_provider(vec![Ok(promotion_result()), Ok(promotion_result())]);
    ingest_promotion_session(&menvane, &project);
    process_next_job(&menvane);
    process_next_job(&menvane);
    assert_eq!(provider.call_count(), 1);
    let playbook = menvane
        .all_memories()
        .unwrap()
        .into_iter()
        .find(|memory| memory.metadata.knowledge_type == KnowledgeType::Playbook)
        .unwrap();

    assert!(
        menvane
            .apply_playbook(playbook.metadata.id, Uuid::from_u128(1), true)
            .unwrap()
    );
    assert!(
        !menvane
            .apply_playbook(playbook.metadata.id, Uuid::from_u128(1), true)
            .unwrap()
    );
    assert!(
        menvane
            .apply_playbook(playbook.metadata.id, Uuid::from_u128(2), false)
            .unwrap()
    );
    assert!(
        menvane
            .apply_playbook(playbook.metadata.id, Uuid::from_u128(3), true)
            .unwrap()
    );

    let applied = menvane
        .read_without_recording(playbook.metadata.id)
        .unwrap();
    assert_eq!(applied.metadata.status, MemoryStatus::Active);
    assert_eq!(applied.metadata.successes, Some(2));
    assert_eq!(applied.metadata.failures, Some(1));
}

#[test]
fn forgotten_memory_is_not_recreated_by_an_explicit_duplicate_write() {
    let (_temporary, project, menvane) = setup_project();
    let memory = menvane
        .write(
            &project,
            WriteMemory {
                title: "External deployment constraint".to_owned(),
                body: "The deployment requires an external approval window.".to_owned(),
                knowledge_type: KnowledgeType::Context,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    menvane.forget(memory.metadata.id).unwrap();
    assert!(
        menvane
            .write(
                &project,
                WriteMemory {
                    title: memory.title,
                    body: memory.body,
                    knowledge_type: KnowledgeType::Context,
                    scope: Scope::Project,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .is_err()
    );
    assert_eq!(menvane.all_memories().unwrap().len(), 1);
}

#[test]
fn normalized_session_capture_is_sanitized_ordered_and_provider_independent() {
    let (_temporary, project, menvane) = setup_project();
    let timestamp = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();

    assert_eq!(
        menvane
            .ingest_event(event(
                &project,
                "prompt",
                NormalizedEventKind::UserPrompt,
                timestamp,
                Some("Implement the export"),
                None,
            ))
            .unwrap(),
        CaptureOutcome::Stored
    );
    let mut tool = event(
        &project,
        "tool",
        NormalizedEventKind::ToolCompleted,
        timestamp + chrono::Duration::seconds(1),
        Some("cargo test"),
        Some("Authorization: Bearer secret\napi_key=private\ntests passed"),
    );
    tool.tool_family = Some("shell".to_owned());
    tool.success = Some(true);
    assert_eq!(menvane.ingest_event(tool).unwrap(), CaptureOutcome::Stored);
    assert_eq!(
        menvane
            .ingest_event(event(
                &project,
                "end",
                NormalizedEventKind::SessionEnded,
                timestamp + chrono::Duration::seconds(2),
                None,
                None,
            ))
            .unwrap(),
        CaptureOutcome::Stored
    );

    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    let session = repository
        .latest_session("test-client", "external-session")
        .unwrap()
        .unwrap();
    assert_eq!(repository.events(session.id).unwrap().len(), 3);
    assert_eq!(repository.events(session.id).unwrap()[0].event_id, "prompt");
    assert_eq!(
        menvane
            .jobs()
            .unwrap()
            .iter()
            .filter(|job| job.job_type == "finalize_session")
            .count(),
        1
    );
    assert!(menvane.configured_provider().is_ok());
    assert!(
        !menvane
            .session_briefing_for_client(&project, "client", "new-session")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn consolidation_preserves_chronology_and_records_execution() {
    let (_temporary, project, provider, menvane) = setup_provider(vec![Ok(valid_result())]);
    ingest_meaningful_session(&menvane, &project);
    process_next_job(&menvane);
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    let session = repository
        .latest_session("test-client", "external-session")
        .unwrap()
        .unwrap();
    let path = session.markdown_path.unwrap();
    let before = fs::read_to_string(&path).unwrap();
    let before_chronology = chronology_bytes(&before);
    assert_eq!(
        session.summary_status,
        menvane_domain::SummaryStatus::Pending
    );

    process_next_job(&menvane);

    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(chronology_bytes(&after), before_chronology);
    assert!(after.contains("## Episodic summary"));
    assert_eq!(
        repository
            .consolidation_result(session.id)
            .unwrap()
            .unwrap()
            .execution
            .provider,
        "fake"
    );
    assert_eq!(provider.call_count(), 1);
    assert_eq!(
        repository.session(session.id).unwrap().summary_status,
        menvane_domain::SummaryStatus::Ready
    );
    assert!(menvane.all_memories().unwrap().is_empty());
}

#[test]
fn unavailable_provider_keeps_pending_session_and_retryable_job() {
    let (_temporary, project, provider, menvane) = setup_provider(vec![Err(LlmError {
        kind: LlmErrorKind::Unavailable,
        message: "offline".to_owned(),
    })]);
    ingest_meaningful_session(&menvane, &project);
    process_next_job(&menvane);
    process_next_job(&menvane);

    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    let session = repository
        .latest_session("test-client", "external-session")
        .unwrap()
        .unwrap();
    let consolidation_job = repository
        .jobs()
        .unwrap()
        .into_iter()
        .find(|job| job.job_type == "consolidate_session")
        .unwrap();
    assert_eq!(
        session.summary_status,
        menvane_domain::SummaryStatus::Pending
    );
    assert_eq!(consolidation_job.status, "pending");
    assert!(consolidation_job.last_error.unwrap().contains("offline"));
    assert_eq!(provider.call_count(), 1);
    assert!(menvane.all_memories().unwrap().is_empty());
}

#[test]
fn consolidation_merge_and_supersede_apply_lifecycle_operations() {
    let (_temporary, project, provider, menvane) = setup_provider(Vec::new());
    let first = menvane
        .write(
            &project,
            WriteMemory {
                title: "Export path".to_owned(),
                body: "The export path uses the remote runner.".to_owned(),
                knowledge_type: KnowledgeType::Context,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    let second = menvane
        .write(
            &project,
            WriteMemory {
                title: "Export verification".to_owned(),
                body: "Export verification uses the remote runner output.".to_owned(),
                knowledge_type: KnowledgeType::Context,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    let third = menvane
        .write(
            &project,
            WriteMemory {
                title: "Old export rule".to_owned(),
                body: "The old export rule uses a local runner.".to_owned(),
                knowledge_type: KnowledgeType::Context,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    provider
        .responses
        .lock()
        .unwrap()
        .push(Ok(merge_and_supersede_result(
            first.metadata.id,
            second.metadata.id,
            third.metadata.id,
        )));
    ingest_promotion_session(&menvane, &project);
    process_next_job(&menvane);
    process_next_job(&menvane);

    let memories = menvane.all_memories().unwrap();
    assert_eq!(
        memories
            .iter()
            .find(|memory| memory.metadata.id == first.metadata.id)
            .unwrap()
            .body,
        "Merged export guidance"
    );
    assert_eq!(
        memories
            .iter()
            .find(|memory| memory.metadata.id == second.metadata.id)
            .unwrap()
            .metadata
            .status,
        MemoryStatus::Superseded
    );
    assert_eq!(
        memories
            .iter()
            .find(|memory| memory.metadata.id == third.metadata.id)
            .unwrap()
            .metadata
            .status,
        MemoryStatus::Superseded
    );
    assert!(memories.iter().any(|memory| {
        memory.title == "Replacement export rule"
            && memory.metadata.supersedes == vec![third.metadata.id]
    }));
}

#[test]
fn invalid_output_is_repaired_once_and_then_rolls_back_without_changes() {
    let (_temporary, project, provider, menvane) = setup_provider(vec![
        Ok(serde_json::json!({"invalid": true})),
        Ok(serde_json::json!({"still": "invalid"})),
    ]);
    ingest_meaningful_session(&menvane, &project);
    process_next_job(&menvane);
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    let session = repository
        .latest_session("test-client", "external-session")
        .unwrap()
        .unwrap();
    let path = session.markdown_path.unwrap();
    let before = fs::read_to_string(&path).unwrap();

    process_next_job(&menvane);

    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(after, before);
    assert!(
        repository
            .consolidation_result(session.id)
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .current_handoff(session.project_id.as_deref())
            .unwrap()
            .is_empty()
    );
    assert_eq!(menvane.all_memories().unwrap().len(), 0);
    assert_eq!(
        repository.session(session.id).unwrap().summary_status,
        menvane_domain::SummaryStatus::Pending
    );
    assert_eq!(provider.call_count(), 2);
}

#[test]
fn operational_session_is_skipped_without_provider_call() {
    let (_temporary, project, provider, menvane) = setup_provider(Vec::new());
    let timestamp = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    menvane
        .ingest_event(event(
            &project,
            "start",
            NormalizedEventKind::SessionStarted,
            timestamp,
            None,
            None,
        ))
        .unwrap();
    menvane
        .ingest_event(event(
            &project,
            "end",
            NormalizedEventKind::SessionEnded,
            timestamp + chrono::Duration::seconds(1),
            None,
            None,
        ))
        .unwrap();
    process_next_job(&menvane);
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    let session = repository
        .latest_session("test-client", "external-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        session.summary_status,
        menvane_domain::SummaryStatus::Skipped
    );
    assert_eq!(provider.call_count(), 0);
    assert_eq!(repository.jobs().unwrap().len(), 1);
}

#[test]
fn current_handoff_items_are_project_scoped_and_rendered_deterministically() {
    let (_temporary, project, menvane) = setup_project();
    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let session_id = Uuid::from_u128(7);
    let first = handoff_item(
        Uuid::from_u128(2),
        &project_id,
        HandoffItemKind::Blocked,
        "Export is blocked on the schema",
        Some("confirm the schema".to_owned()),
        Some("schema review".to_owned()),
        session_id,
    );
    let second = handoff_item(
        Uuid::from_u128(1),
        &project_id,
        HandoffItemKind::InProgress,
        "Export implementation is underway",
        Some("run the export tests".to_owned()),
        None,
        session_id,
    );
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    repository.upsert_handoff_item(&first).unwrap();
    repository.upsert_handoff_item(&second).unwrap();

    let items = menvane.current_handoff_items(Some(&project_id)).unwrap();
    assert_eq!(items, vec![second.clone(), first.clone()]);
    let rendered = menvane.render_current_handoff(Some(&project_id)).unwrap();
    assert!(
        rendered.find("Export implementation").unwrap()
            < rendered.find("Export is blocked").unwrap()
    );
    assert!(rendered.contains("Next: run the export tests"));
    assert!(rendered.contains("Blocked by: schema review"));
    assert!(
        menvane
            .current_handoff_items(Some("unrelated-project"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn handoff_delivery_is_claimed_by_session_and_rendered_content() {
    let (_temporary, project, menvane) = setup_project();
    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    let item = handoff_item(
        Uuid::from_u128(9),
        &project_id,
        HandoffItemKind::InProgress,
        "Export remains open",
        None,
        None,
        Uuid::from_u128(8),
    );
    repository.upsert_handoff_item(&item).unwrap();

    let first = menvane
        .session_briefing_for_client(&project, "test-client", "session")
        .unwrap();
    assert!(!first.is_empty());
    assert!(
        menvane
            .session_briefing_for_client(&project, "test-client", "session")
            .unwrap()
            .is_empty()
    );

    let mut changed = item;
    changed.state = "Export is blocked".to_owned();
    repository.upsert_handoff_item(&changed).unwrap();
    let second = menvane
        .session_briefing_for_client(&project, "test-client", "session")
        .unwrap();
    assert!(!second.is_empty());
    assert!(second.contains("Export is blocked"));
}

#[test]
fn unrelated_project_changes_do_not_change_handoff_content() {
    let (_temporary, project, menvane) = setup_project();
    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    repository
        .upsert_handoff_item(&handoff_item(
            Uuid::from_u128(10),
            &project_id,
            HandoffItemKind::OpenQuestion,
            "Confirm export schema",
            None,
            None,
            Uuid::from_u128(11),
        ))
        .unwrap();
    let before = menvane.render_current_handoff(Some(&project_id)).unwrap();
    fs::write(project.join("unrelated.txt"), "unrelated").unwrap();
    let after = menvane.render_current_handoff(Some(&project_id)).unwrap();
    assert_eq!(before, after);
}

#[test]
fn consolidation_applies_resolve_discard_and_uncertain_deterministically() {
    let (_temporary, project, provider, menvane) = setup_provider(vec![
        Ok(transition_result("resolve", Uuid::from_u128(21))),
        Ok(transition_result("resolve", Uuid::from_u128(21))),
    ]);
    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    repository
        .upsert_handoff_item(&handoff_item(
            Uuid::from_u128(21),
            &project_id,
            HandoffItemKind::InProgress,
            "resolve this front",
            None,
            None,
            Uuid::from_u128(20),
        ))
        .unwrap();
    ingest_meaningful_session(&menvane, &project);
    process_next_job(&menvane);
    process_next_job(&menvane);

    assert!(
        repository
            .current_handoff(Some(&project_id))
            .unwrap()
            .is_empty()
    );
    let session = repository
        .latest_session("test-client", "external-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        repository
            .consolidation_result(session.id)
            .unwrap()
            .unwrap()
            .result
            .summary
            .continuity[0]
            .disposition,
        menvane_domain::ContinuityDisposition::Resolved
    );
    assert_eq!(provider.call_count(), 1);
}

#[test]
fn reindex_rebuilds_knowledge_without_touching_session_state() {
    let (_temporary, project, menvane) = setup_project();
    let memory = menvane
        .write(
            &project,
            WriteMemory {
                title: "Reindex marker".to_owned(),
                body: "reindex preserves canonical context".to_owned(),
                knowledge_type: KnowledgeType::Context,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    let playbook = menvane
        .write(
            &project,
            WriteMemory {
                title: "Reindex playbook marker".to_owned(),
                body: "Use the marker and verify the result after reindex.".to_owned(),
                knowledge_type: KnowledgeType::Playbook,
                scope: Scope::Project,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    menvane
        .apply_playbook(playbook.metadata.id, Uuid::from_u128(100), true)
        .unwrap();
    let before_reindex = menvane
        .read_without_recording(playbook.metadata.id)
        .unwrap();
    menvane
        .ingest_event(event(
            &project,
            "session",
            NormalizedEventKind::SessionStarted,
            Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            None,
            None,
        ))
        .unwrap();

    menvane.reindex().unwrap();

    let repository = SessionRepository::new(menvane.home().join("state.sqlite"));
    assert_eq!(
        repository
            .latest_session("test-client", "external-session")
            .unwrap()
            .unwrap()
            .state,
        menvane_domain::SessionState::Open
    );
    assert_eq!(menvane.read(memory.metadata.id).unwrap(), memory);
    assert_eq!(
        menvane
            .read_without_recording(playbook.metadata.id)
            .unwrap(),
        before_reindex
    );
    assert_eq!(
        menvane
            .search(&project, "canonical context", ScopeSelection::Project, 10)
            .unwrap()
            .len(),
        1
    );
}

fn setup_project() -> (TempDir, PathBuf, Menvane) {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let status = std::process::Command::new("git")
        .args(["-C", project.to_str().unwrap(), "init", "--quiet"])
        .status()
        .unwrap();
    assert!(status.success());
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    (temporary, project, menvane)
}

fn setup_provider(
    responses: Vec<Result<serde_json::Value, LlmError>>,
) -> (TempDir, PathBuf, Arc<FakeLlmProvider>, Menvane) {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let status = std::process::Command::new("git")
        .args(["-C", project.to_str().unwrap(), "init", "--quiet"])
        .status()
        .unwrap();
    assert!(status.success());
    let provider = Arc::new(FakeLlmProvider::new(responses));
    let menvane =
        Menvane::new_with_provider(temporary.path().join("home"), provider.clone()).unwrap();
    (temporary, project, provider, menvane)
}

fn ingest_meaningful_session(menvane: &Menvane, project: &Path) {
    let timestamp = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    menvane
        .ingest_event(event(
            project,
            "prompt",
            NormalizedEventKind::UserPrompt,
            timestamp,
            Some("continue the export"),
            None,
        ))
        .unwrap();
    menvane
        .ingest_event(event(
            project,
            "end",
            NormalizedEventKind::SessionEnded,
            timestamp + chrono::Duration::seconds(1),
            None,
            None,
        ))
        .unwrap();
}

fn ingest_promotion_session(menvane: &Menvane, project: &Path) {
    let timestamp = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    menvane
        .ingest_event(event(
            project,
            "prompt",
            NormalizedEventKind::UserPrompt,
            timestamp,
            Some("continue the export"),
            None,
        ))
        .unwrap();
    let mut tool = event(
        project,
        "tool",
        NormalizedEventKind::ToolCompleted,
        timestamp + chrono::Duration::seconds(1),
        Some("deploy"),
        Some("deployment verified"),
    );
    tool.success = Some(true);
    menvane.ingest_event(tool).unwrap();
    menvane
        .ingest_event(event(
            project,
            "end",
            NormalizedEventKind::SessionEnded,
            timestamp + chrono::Duration::seconds(2),
            None,
            None,
        ))
        .unwrap();
}

fn valid_result() -> serde_json::Value {
    serde_json::json!({
        "summary": {
            "intentions": ["continue the export"],
            "actions": [],
            "outcome": "inconclusive",
            "result": "The export remains open.",
            "continuity": [],
            "candidate-learnings": []
        },
        "handoff": [],
        "knowledge": []
    })
}

fn promotion_result() -> serde_json::Value {
    let mut result = valid_result();
    result["knowledge"] = serde_json::json!([
        {
            "operation": "create",
            "target_memory_ids": [],
            "knowledge_type": "context",
            "title": "Remote deployment approval",
            "scope": "project",
            "scope_confidence": 0.95,
            "applies_to": {},
            "content": {"context": {"body": "Remote deployments require an external approval window before verification."}},
            "evidence_event_ids": ["tool"],
            "contradicting_event_ids": []
        },
        {
            "operation": "create",
            "target_memory_ids": [],
            "knowledge_type": "playbook",
            "title": "Verify remote deployment",
            "scope": "project",
            "scope_confidence": 0.95,
            "applies_to": {},
            "content": {"playbook": {"trigger": "When deploying remotely", "applicability": {}, "steps": ["Request approval", "Deploy remotely"], "validation": ["Confirm deployment output"], "failure_handling": "Stop and request approval again."}},
            "evidence_event_ids": ["tool"],
            "contradicting_event_ids": []
        }
    ]);
    result
}

fn merge_and_supersede_result(first: Uuid, second: Uuid, third: Uuid) -> serde_json::Value {
    let mut result = valid_result();
    result["knowledge"] = serde_json::json!([
        {
            "operation": "merge",
            "target_memory_ids": [first, second],
            "knowledge_type": "context",
            "title": "Merged export guidance",
            "scope": "project",
            "scope_confidence": 0.95,
            "applies_to": {},
            "content": {"context": {"body": "Merged export guidance"}},
            "evidence_event_ids": ["tool"],
            "contradicting_event_ids": []
        },
        {
            "operation": "supersede",
            "target_memory_ids": [third],
            "knowledge_type": "context",
            "title": "Replacement export rule",
            "scope": "project",
            "scope_confidence": 0.95,
            "applies_to": {},
            "content": {"context": {"body": "The replacement export rule uses the remote runner."}},
            "evidence_event_ids": ["tool"],
            "contradicting_event_ids": []
        }
    ]);
    result
}

fn transition_result(operation: &str, item_id: Uuid) -> serde_json::Value {
    let mut result = valid_result();
    let operation = match operation {
        "resolve" => serde_json::json!({
            "resolve": {
                "item_id": item_id,
                "text": "The front was resolved.",
                "evidence_event_ids": ["prompt"]
            }
        }),
        "discard" => serde_json::json!({
            "discard": {
                "item_id": item_id,
                "text": "The front was discarded.",
                "evidence_event_ids": ["prompt"]
            }
        }),
        "uncertain" => serde_json::json!({"uncertain": {"item_id": item_id}}),
        _ => unreachable!(),
    };
    result["handoff"] = serde_json::json!([operation]);
    result
}

fn chronology_bytes(markdown: &str) -> Vec<u8> {
    let body = markdown.split_once("\n---\n").unwrap().1;
    body.split_once("\n## Episodic summary\n")
        .map_or(body, |value| value.0)
        .as_bytes()
        .to_vec()
}

fn process_next_job(menvane: &Menvane) {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(menvane.process_next_job())
        .unwrap();
}

fn event(
    project: &Path,
    event_id: &str,
    kind: NormalizedEventKind,
    timestamp: chrono::DateTime<Utc>,
    input: Option<&str>,
    output: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: event_id.to_owned(),
        kind,
        origin: Default::default(),
        role: Default::default(),
        client: "test-client".to_owned(),
        external_session_id: "external-session".to_owned(),
        timestamp,
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: input.map(str::to_owned),
        bounded_output: output.map(str::to_owned),
        attributed_path: None,
        success: None,
        model: None,
        harness_injected: false,
    }
}

fn handoff_item(
    id: Uuid,
    project_id: &str,
    kind: HandoffItemKind,
    state: &str,
    next_step: Option<String>,
    blocker: Option<String>,
    session_id: Uuid,
) -> HandoffItem {
    let timestamp = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    HandoffItem {
        id,
        project_id: Some(project_id.to_owned()),
        kind,
        state: state.to_owned(),
        next_step,
        blocker,
        low_confidence: false,
        last_confirmed_at: timestamp,
        sources: vec![HandoffItemSource {
            session_id,
            event_ids: vec!["prompt".to_owned()],
        }],
        created_at: timestamp,
        updated_at: timestamp,
    }
}
