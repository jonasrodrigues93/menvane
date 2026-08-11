use std::fs;

use chrono::{Duration, Utc};
use menvane_domain::{NormalizedEvent, NormalizedEventKind};
use menvane_engine::Menvane;
use rusqlite::Connection;
use tempfile::TempDir;

mod common;

#[test]
fn ingestion_never_creates_deterministic_goals_episodes_or_intents() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();

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
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Add a bounded cache for project metadata."),
    );
    ingest(
        &menvane,
        &project,
        "constraint",
        NormalizedEventKind::UserPrompt,
        Some("Also keep the cache disabled by default."),
    );
    ingest(
        &menvane,
        &project,
        "correction",
        NormalizedEventKind::UserPrompt,
        Some("Correction: the cache uses a bounded size."),
    );
    ingest(
        &menvane,
        &project,
        "new-goal",
        NormalizedEventKind::UserPrompt,
        Some("Now review the dashboard colors."),
    );

    let connection = Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let intents: u64 = connection
        .query_row("SELECT COUNT(*) FROM prompt_intents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(intents, 0, "intent classification is retired");
    let episodes: u64 = connection
        .query_row("SELECT COUNT(*) FROM task_episodes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(episodes, 0, "deterministic episodes are retired");
    let goals: u64 = connection
        .query_row("SELECT COUNT(*) FROM goals", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        goals, 0,
        "no goal is created without a consolidation result"
    );
}

#[test]
fn sessions_still_capture_order_and_reopen_finalized_generations() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
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
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("first-generation-goal"),
    );
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    ingest(
        &menvane,
        &project,
        "reopen-prompt",
        NormalizedEventKind::UserPrompt,
        Some("second-generation-goal"),
    );
    ingest(
        &menvane,
        &project,
        "second-end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    let connection =
        rusqlite::Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let generations: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE client='test-client' AND external_session_id='external-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generations, 2);
}

fn ingest(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    prompt: Option<&str>,
) {
    menvane
        .ingest_event(event(project, id, kind, prompt))
        .unwrap();
}

fn event(
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    prompt: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
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
    }
}
