use std::fs;

use chrono::{Duration, Utc};
use menvane_domain::{NormalizedEvent, NormalizedEventKind};
use menvane_engine::{Menvane, ScopeSelection};
use tempfile::TempDir;

#[test]
fn old_irrelevant_session_is_archived_but_remains_reindexable() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let timestamp = Utc::now() - Duration::days(180);
    menvane
        .ingest_event(event(
            &project,
            "start",
            NormalizedEventKind::SessionStarted,
            timestamp,
            None,
        ))
        .unwrap();
    menvane
        .ingest_event(event(
            &project,
            "prompt",
            NormalizedEventKind::UserPrompt,
            timestamp,
            Some("archivable-session-evidence"),
        ))
        .unwrap();
    menvane
        .ingest_event(event(
            &project,
            "end",
            NormalizedEventKind::SessionEnded,
            timestamp,
            None,
        ))
        .unwrap();
    assert_eq!(menvane.gc().unwrap(), 1);
    assert!(
        menvane
            .search_with_sessions(
                &project,
                "archivable-session-evidence",
                ScopeSelection::Project,
                10,
                true,
            )
            .unwrap()
            .is_empty()
    );
    let archive = home.join("memory/archive/sessions");
    assert_eq!(fs::read_dir(archive).unwrap().count(), 1);
    let (_, memories) = menvane.reindex().unwrap();
    assert_eq!(memories, 1);
}

fn event(
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    timestamp: chrono::DateTime<Utc>,
    input: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
        kind,
        client: "decay-test".to_owned(),
        external_session_id: "old-session".to_owned(),
        timestamp,
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: input.map(str::to_owned),
        bounded_output: None,
        attributed_path: None,
        success: None,
        model: None,
    }
}
