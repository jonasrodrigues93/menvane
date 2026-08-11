use std::fs;
use std::sync::Arc;

use chrono::{Duration, Utc};
use menvane_domain::{
    NormalizedEvent, NormalizedEventKind, NormalizedEventOrigin, NormalizedEventRole,
};
use menvane_engine::{CaptureOutcome, Menvane, ScopeSelection};
use rusqlite::Connection;
use tempfile::TempDir;

mod common;

#[test]
fn injected_context_is_stripped_from_composed_user_prompts() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let mut prompt = event(&project, "composed", NormalizedEventKind::UserPrompt);
    prompt.bounded_input = Some(
        "Implement the export\nMENVANE MEMORY CONTEXT\nHistorical context only.\n[REQUIRED CONTEXT]\nTitle: x\nEND MENVANE MEMORY CONTEXT\n<available-skills>\n- browser-control\n</available-skills>\nContinue the export"
            .to_owned(),
    );
    assert_eq!(
        menvane.ingest_event(prompt.clone()).unwrap(),
        CaptureOutcome::Stored
    );
    let connection =
        Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let payload: String = connection
        .query_row(
            "SELECT payload_json FROM session_events WHERE event_id='composed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored: NormalizedEvent = serde_json::from_str(&payload).unwrap();
    assert!(stored.is_user_prompt());
    assert!(stored.is_consolidation_eligible());
    assert_eq!(
        stored.bounded_input.as_deref(),
        Some("Implement the export\nContinue the export")
    );
}

#[test]
fn injected_and_system_events_are_operational_evidence_only() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let system = NormalizedEvent {
        harness_injected: true,
        ..event(&project, "system-prompt", NormalizedEventKind::UserPrompt)
    };
    assert!(!system.is_user_prompt());
    assert!(system.is_operational());
    assert!(!system.is_durable());
    assert!(!system.is_consolidation_eligible());
    assert_eq!(
        menvane.ingest_event(system).unwrap(),
        CaptureOutcome::Stored
    );

    let mut skill = event(&project, "skill", NormalizedEventKind::ToolCompleted);
    skill.role = NormalizedEventRole::SystemPrompt;
    skill.origin = NormalizedEventOrigin::System;
    assert!(!skill.is_durable());
    assert!(!skill.is_consolidation_eligible());
    assert!(skill.is_operational());
}

#[test]
fn capture_is_bounded_idempotent_and_reopens_finalized_sessions() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let started = event(&project, "start", NormalizedEventKind::SessionStarted);
    assert_eq!(
        menvane.ingest_event(started).unwrap(),
        CaptureOutcome::Stored
    );

    let mut prompt = event(&project, "prompt", NormalizedEventKind::UserPrompt);
    prompt.bounded_input = Some("first-generation-goal".to_owned());
    menvane.ingest_event(prompt).unwrap();
    let mut tool = event(&project, "tool", NormalizedEventKind::ToolCompleted);
    tool.tool_family = Some("test".to_owned());
    tool.success = Some(false);
    tool.bounded_output = Some(format!(
        "api_key=very-secret {}",
        "bounded-output ".repeat(1_000)
    ));
    menvane.ingest_event(tool).unwrap();
    let mut ignored = event(&project, "ignored", NormalizedEventKind::ToolCompleted);
    ignored.attributed_path = Some(project.join(".env").to_string_lossy().into_owned());
    ignored.bounded_output = Some("must-not-persist".to_owned());
    assert_eq!(
        menvane.ingest_event(ignored).unwrap(),
        CaptureOutcome::Dropped
    );
    let ended = event(&project, "end", NormalizedEventKind::SessionEnded);
    assert_eq!(
        menvane.ingest_event(ended.clone()).unwrap(),
        CaptureOutcome::Stored
    );
    process_one(&menvane);
    assert_eq!(
        menvane.ingest_event(ended).unwrap(),
        CaptureOutcome::Duplicate
    );

    let first = menvane
        .search_with_sessions(
            &project,
            "first-generation-goal",
            ScopeSelection::Project,
            10,
            true,
        )
        .unwrap();
    assert_eq!(first.len(), 1);
    let first = menvane.read(first[0].id).unwrap();
    assert_eq!(first.metadata.generation, Some(1));
    assert!(!first.body.contains("very-secret"));
    assert!(!first.body.contains("must-not-persist"));
    assert!(first.body.contains("[REDACTED]"));
    assert!(first.body.len() < 4_000);

    let mut reopened = event(&project, "reopened", NormalizedEventKind::UserPrompt);
    reopened.bounded_input = Some("second-generation-goal".to_owned());
    menvane.ingest_event(reopened).unwrap();
    let mut second_end = event(&project, "second-end", NormalizedEventKind::SessionEnded);
    second_end.timestamp += Duration::seconds(1);
    menvane.ingest_event(second_end).unwrap();
    process_one(&menvane);
    let second = menvane
        .search_with_sessions(
            &project,
            "second-generation-goal",
            ScopeSelection::Project,
            10,
            true,
        )
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(
        menvane.read(second[0].id).unwrap().metadata.generation,
        Some(2)
    );
}

#[test]
fn trivial_sessions_are_not_queued_for_compilation() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let started = event(&project, "start", NormalizedEventKind::SessionStarted);
    assert_eq!(
        menvane.ingest_event(started).unwrap(),
        CaptureOutcome::Stored
    );
    let ended = event(&project, "end", NormalizedEventKind::SessionEnded);
    assert_eq!(menvane.ingest_event(ended).unwrap(), CaptureOutcome::Stored);
    process_one(&menvane);
    let jobs = menvane.jobs().unwrap();
    assert_eq!(
        jobs.iter()
            .filter(|job| job.job_type == "compile_session")
            .count(),
        0
    );
}

#[test]
fn concurrent_events_and_idle_finalization_are_safe() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Arc::new(Menvane::new(temporary.path().join("home")).unwrap());
    let mut handles = Vec::new();
    for index in 0..20 {
        let menvane = Arc::clone(&menvane);
        let project = project.clone();
        handles.push(std::thread::spawn(move || {
            let mut event = event(
                &project,
                &format!("concurrent-{index}"),
                NormalizedEventKind::ToolCompleted,
            );
            event.tool_family = Some(format!("tool-{index}"));
            menvane.ingest_event(event).unwrap();
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let mut stopped = event(&project, "stopped", NormalizedEventKind::TurnStopped);
    stopped.timestamp = Utc::now() - Duration::seconds(121);
    menvane.ingest_event(stopped).unwrap();
    assert_eq!(menvane.finalize_idle_sessions().unwrap(), 1);
    process_one(&menvane);
    assert_eq!(
        menvane
            .jobs()
            .unwrap()
            .iter()
            .filter(|job| job.job_type == "compile_session")
            .count(),
        0
    );
}

#[test]
fn session_outside_git_is_finalized_as_global() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("notes");
    fs::create_dir_all(&directory).unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let mut prompt = event(&directory, "global-prompt", NormalizedEventKind::UserPrompt);
    prompt.bounded_input = Some("global-session-evidence".to_owned());
    menvane.ingest_event(prompt).unwrap();
    menvane
        .ingest_event(event(
            &directory,
            "global-end",
            NormalizedEventKind::SessionEnded,
        ))
        .unwrap();
    process_one(&menvane);

    let sessions = menvane
        .search_with_sessions(
            &directory,
            "global-session-evidence",
            ScopeSelection::Auto,
            10,
            true,
        )
        .unwrap();
    assert_eq!(sessions.len(), 1);
    let session = menvane.read(sessions[0].id).unwrap();
    assert_eq!(session.metadata.scope, menvane_domain::Scope::Global);
    assert!(session.metadata.project_id.is_none());
    assert!(menvane.all_projects().unwrap().is_empty());
}

#[tokio::test]
async fn pending_finalization_is_completed_by_a_restarted_worker() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let mut prompt = event(&project, "restart-prompt", NormalizedEventKind::UserPrompt);
    prompt.bounded_input = Some("restart-finalization-evidence".to_owned());
    menvane.ingest_event(prompt).unwrap();
    menvane
        .ingest_event(event(
            &project,
            "restart-end",
            NormalizedEventKind::SessionEnded,
        ))
        .unwrap();
    drop(menvane);

    let restarted = Menvane::new(&home).unwrap();
    assert!(
        restarted
            .search_with_sessions(
                &project,
                "restart-finalization-evidence",
                ScopeSelection::Project,
                10,
                true,
            )
            .unwrap()
            .is_empty()
    );
    assert!(restarted.process_next_job().await.unwrap());
    assert_eq!(
        restarted
            .search_with_sessions(
                &project,
                "restart-finalization-evidence",
                ScopeSelection::Project,
                10,
                true,
            )
            .unwrap()
            .len(),
        1
    );
    assert!(
        restarted
            .jobs()
            .unwrap()
            .iter()
            .any(|job| job.job_type == "finalize_session" && job.status == "completed")
    );
}

#[tokio::test]
async fn repeated_finalization_claims_do_not_duplicate_session_memory() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let mut prompt = event(
        &project,
        "duplicate-prompt",
        NormalizedEventKind::UserPrompt,
    );
    prompt.bounded_input = Some("duplicate-finalization-evidence".to_owned());
    menvane.ingest_event(prompt).unwrap();
    menvane
        .ingest_event(event(
            &project,
            "duplicate-end",
            NormalizedEventKind::SessionEnded,
        ))
        .unwrap();
    assert!(menvane.process_next_job().await.unwrap());
    let session_id = menvane
        .search_with_sessions(
            &project,
            "duplicate-finalization-evidence",
            ScopeSelection::Project,
            10,
            true,
        )
        .unwrap()[0]
        .id;
    let connection = Connection::open(home.join("state.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE jobs SET status='pending', owner=NULL, lease_started_at=NULL, lease_until=NULL WHERE job_type='finalize_session' AND dedupe_key=?1",
            [session_id.to_string()],
        )
        .unwrap();
    assert!(menvane.process_next_job().await.unwrap());
    assert_eq!(
        menvane
            .all_memories()
            .unwrap()
            .into_iter()
            .filter(|memory| memory.metadata.id == session_id)
            .count(),
        1
    );
}

fn event(project: &std::path::Path, id: &str, kind: NormalizedEventKind) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
        kind,
        origin: Default::default(),
        role: Default::default(),
        client: "test-client".to_owned(),
        external_session_id: "external-session".to_owned(),
        timestamp: Utc::now(),
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: None,
        bounded_output: None,
        attributed_path: None,
        success: None,
        model: Some("test-model".to_owned()),
        harness_injected: false,
    }
}

fn process_one(menvane: &Menvane) {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(menvane.process_next_job())
        .unwrap();
}
