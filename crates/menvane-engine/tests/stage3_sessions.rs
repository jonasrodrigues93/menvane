use std::fs;
use std::sync::Arc;

use chrono::{Duration, Utc};
use menvane_domain::{NormalizedEvent, NormalizedEventKind};
use menvane_engine::{CaptureOutcome, Menvane, ScopeSelection};
use tempfile::TempDir;

#[test]
fn capture_is_bounded_idempotent_and_reopens_finalized_sessions() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
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
        CaptureOutcome::Finalized
    );
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
fn concurrent_events_and_idle_finalization_are_safe() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
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
    assert_eq!(
        menvane
            .jobs()
            .unwrap()
            .iter()
            .filter(|job| job.job_type == "compile_session")
            .count(),
        1
    );
}

fn event(project: &std::path::Path, id: &str, kind: NormalizedEventKind) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
        kind,
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
    }
}
