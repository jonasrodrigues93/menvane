use std::fs;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use menvane_domain::{
    JsonSchema, LlmError, LlmErrorKind, LlmProvider, LlmRequest, NormalizedEvent,
    NormalizedEventKind, ProviderCapabilities, ProviderHealth, StructuredResponse,
};
use menvane_engine::Menvane;
use menvane_store::SessionRepository;
use rusqlite::Connection;
use tempfile::TempDir;

mod common;

#[test]
fn consolidation_creates_and_replaces_one_handoff_per_project() {
    let (temporary, project, menvane, _provider) = setup();
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "prompt-a",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest_tool(&menvane, &project, "tool-a", "bash", true, "src/export.rs");
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));
    assert!(process_one(&menvane));

    let handoff = current_handoff(&temporary, &project, &menvane);
    assert!(handoff.contains("consolidated summary"));
    assert_newer_handoff_count(&temporary, 1);

    ingest(
        &menvane,
        &project,
        "prompt-b",
        NormalizedEventKind::UserPrompt,
        Some("Continue work."),
    );
    ingest(
        &menvane,
        &project,
        "end-b",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));
    assert!(process_one(&menvane));
    assert_newer_handoff_count(&temporary, 1);
    assert!(current_handoff(&temporary, &project, &menvane).contains("consolidated summary"));
}

#[test]
fn provider_failure_preserves_the_last_valid_handoff() {
    let (temporary, project, menvane, provider) = setup();
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "prompt-a",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));
    assert!(process_one(&menvane));
    let before = current_handoff(&temporary, &project, &menvane);
    assert!(!before.is_empty());

    *provider.fail.lock().unwrap() = true;
    ingest(
        &menvane,
        &project,
        "prompt-b",
        NormalizedEventKind::UserPrompt,
        Some("Continue work."),
    );
    ingest(
        &menvane,
        &project,
        "end-b",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));
    assert!(process_one(&menvane));
    assert_eq!(current_handoff(&temporary, &project, &menvane), before);
    assert_newer_handoff_count(&temporary, 1);
}

#[test]
fn oversized_handoff_summary_is_never_persisted() {
    let (temporary, project, menvane, provider) = setup();
    *provider.summary_override.lock().unwrap() = Some("x".repeat(6_000));
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "prompt-a",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));
    assert!(process_one(&menvane));
    assert_newer_handoff_count(&temporary, 0);
}

#[test]
fn delivery_injects_the_single_summary_once_per_generation() {
    let (temporary, project, menvane, _provider) = setup();
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "prompt-a",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));
    assert!(process_one(&menvane));

    let first = menvane
        .session_briefing_for_client(&project, "client", "shared-session")
        .unwrap();
    assert!(first.contains("[PROJECT HANDOFF]"));
    assert!(first.contains("consolidated summary"));
    let repeated = menvane
        .session_briefing_for_client(&project, "client", "shared-session")
        .unwrap();
    assert!(repeated.is_empty());

    let prompt = menvane
        .prompt_context_for_client(&project, "client-2", "other-session", "continue the work")
        .unwrap()
        .0;
    assert!(prompt.contains("[PROJECT HANDOFF]"));
    drop(temporary);
}

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Menvane,
    Arc<ConsolidationProvider>,
) {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let provider = Arc::new(ConsolidationProvider::default());
    let menvane =
        Menvane::new_with_provider(temporary.path().join("home"), provider.clone()).unwrap();
    (temporary, project, menvane, provider)
}

#[derive(Default)]
struct ConsolidationProvider {
    fail: Arc<Mutex<bool>>,
    summary_override: Arc<Mutex<Option<String>>>,
    calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl LlmProvider for ConsolidationProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        _schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        if *self.fail.lock().unwrap() {
            return Err(LlmError {
                kind: LlmErrorKind::Unavailable,
                message: "offline".to_owned(),
            });
        }
        *self.calls.lock().unwrap() += 1;
        let input: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        let session_id = input["session"]["session_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        let events = input["session"]["events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let ids = events
            .iter()
            .filter_map(|event| event["event_id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        let default_summary = format!("consolidated summary for {} events", events.len());
        let summary = self
            .summary_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(default_summary);
        Ok(StructuredResponse {
            value: serde_json::json!({
                "goals": [],
                "memories": [],
                "handoff": {
                    "summary": summary,
                    "source_session_ids": [session_id],
                    "evidence_event_ids": ids
                }
            }),
            provider: "test-consolidation".to_owned(),
            model: "test".to_owned(),
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
        "test-consolidation"
    }

    fn model(&self) -> &str {
        "test"
    }
}

fn current_handoff(temporary: &TempDir, project: &std::path::Path, menvane: &Menvane) -> String {
    let project_id = menvane.ensure_project(project).unwrap().unwrap().id;
    let repository = SessionRepository::new(temporary.path().join("home/state.sqlite"));
    repository
        .current_project_handoff(Some(&project_id))
        .unwrap()
        .map(|handoff| handoff.summary)
        .unwrap_or_default()
}

fn assert_newer_handoff_count(temporary: &TempDir, expected: u64) {
    let connection = Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let count: u64 = connection
        .query_row("SELECT COUNT(*) FROM project_handoffs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, expected);
}

fn process_one(menvane: &Menvane) -> bool {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(menvane.process_next_job())
        .unwrap()
}

fn ingest(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    prompt: Option<&str>,
) {
    menvane
        .ingest_event(NormalizedEvent {
            event_id: id.to_owned(),
            kind,
            origin: Default::default(),
            role: Default::default(),
            client: "test-client".to_owned(),
            external_session_id: "external-session".to_owned(),
            timestamp: Utc::now() + Duration::milliseconds(id.len() as i64),
            cwd: project.to_string_lossy().into_owned(),
            project_id: None,
            tool_family: None,
            bounded_input: prompt.map(str::to_owned),
            bounded_output: None,
            attributed_path: None,
            success: None,
            model: None,
            harness_injected: false,
        })
        .unwrap();
}

fn ingest_tool(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    family: &str,
    success: bool,
    path: &str,
) {
    let mut event = NormalizedEvent {
        event_id: id.to_owned(),
        kind: NormalizedEventKind::ToolCompleted,
        origin: Default::default(),
        role: Default::default(),
        client: "test-client".to_owned(),
        external_session_id: "external-session".to_owned(),
        timestamp: Utc::now() + Duration::milliseconds(id.len() as i64),
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: Some(family.to_owned()),
        bounded_input: Some(family.to_owned()),
        bounded_output: Some("tests passed".to_owned()),
        attributed_path: Some(path.to_owned()),
        success: Some(success),
        model: None,
        harness_injected: false,
    };
    event.timestamp += Duration::milliseconds(1);
    menvane.ingest_event(event).unwrap();
}
