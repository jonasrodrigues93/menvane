use std::fs::{self, OpenOptions};
use std::path::Path;

use chrono::{TimeZone, Utc};
use fs2::FileExt;
use menvane_domain::{
    Applicability, HandoffStatus, MemoryType, NormalizedEvent, NormalizedEventKind,
    ReinforcementSignal, Scope, TaskHandoff,
};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use menvane_store::{InjectionIdentity, SessionRepository};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn legacy_operational_tables_migrate_idempotently_with_markers() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let legacy = SessionRepository::new(home.join("index.sqlite"));
    legacy.initialize().unwrap();
    let first = legacy
        .ingest(
            &event(
                "first",
                "legacy-session",
                NormalizedEventKind::SessionStarted,
                Path::new("/tmp/menvane-test-project"),
            ),
            None,
        )
        .unwrap();
    legacy
        .ingest(
            &event(
                "second",
                "legacy-session",
                NormalizedEventKind::SessionEnded,
                Path::new("/tmp/menvane-test-project"),
            ),
            None,
        )
        .unwrap();
    let memory_id = Uuid::from_u128(7);
    legacy
        .record_access(memory_id, ReinforcementSignal::Retrieved)
        .unwrap();
    legacy
        .claim_injection(
            &InjectionIdentity {
                client: "legacy".to_owned(),
                conversation_key: "legacy-key".to_owned(),
                generation: 0,
                episode_id: None,
            },
            memory_id,
        )
        .unwrap();
    legacy
        .record_procedure_application(memory_id, first.session.id, true)
        .unwrap();
    legacy
        .record_import("legacy", "imported", "orphan", Some("payload"))
        .unwrap();
    drop(legacy);

    let state_path = home.join("state.sqlite");
    prepare_interrupted_migration(&home.join("index.sqlite"), &state_path);
    let menvane = Menvane::new(&home).unwrap();
    for table in operational_tables() {
        assert_eq!(
            row_count(&home.join("index.sqlite"), table),
            row_count(&state_path, table),
            "{table}"
        );
    }
    assert_eq!(row_count(&state_path, "operational_migration_markers"), 19);
    assert!(has_table(&home.join("index.sqlite"), "sessions"));
    assert_eq!(
        SessionRepository::new(&state_path)
            .events(first.session.id)
            .unwrap()
            .len(),
        2
    );
    drop(menvane);

    let reopened = Menvane::new(&home).unwrap();
    assert_eq!(row_count(&state_path, "operational_migration_markers"), 19);
    assert_eq!(
        SessionRepository::new(&state_path)
            .events(first.session.id)
            .unwrap()
            .len(),
        2
    );
    drop(reopened);
}

#[test]
fn reindex_replaces_only_the_derived_index() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let cwd = temporary.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let session = event(
        "session",
        "reindex-session",
        NormalizedEventKind::SessionStarted,
        &cwd,
    );
    menvane.ingest_event(session).unwrap();
    menvane
        .write(
            &cwd,
            WriteMemory {
                title: "Reindex marker".to_owned(),
                body: "reindex-preserves-derived-search".to_owned(),
                memory_type: MemoryType::Fact,
                scope: Scope::Global,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();

    menvane.reindex().unwrap();

    assert_eq!(row_count(&home.join("state.sqlite"), "sessions"), 1);
    assert_eq!(row_count(&home.join("state.sqlite"), "session_events"), 1);
    assert!(!has_table(&home.join("index.sqlite"), "sessions"));
    assert_eq!(
        menvane
            .search(
                &cwd,
                "reindex-preserves-derived-search",
                ScopeSelection::Global,
                10
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reindex_preserves_handoff_state_and_evidence() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let state = SessionRepository::new(home.join("state.sqlite"));
    let session = state
        .ingest(
            &event(
                "handoff-session",
                "handoff-reindex",
                NormalizedEventKind::SessionStarted,
                Path::new("/tmp/menvane-handoff-project"),
            ),
            None,
        )
        .unwrap()
        .session;
    state
        .ingest(
            &event(
                "handoff-prompt",
                "handoff-reindex",
                NormalizedEventKind::UserPrompt,
                Path::new("/tmp/menvane-handoff-project"),
            ),
            None,
        )
        .unwrap();
    let episode = state
        .create_episode(session.id, "handoff-prompt", "preserve handoff")
        .unwrap();
    let handoff = TaskHandoff {
        id: Uuid::now_v7(),
        project_id: None,
        conversation_key: episode.conversation_key.clone(),
        episode_id: episode.id,
        source_session_id: session.id,
        source_client: session.client.clone(),
        status: HandoffStatus::Ready,
        goal: "preserve handoff".to_owned(),
        current_state: "captured".to_owned(),
        completed_work: vec!["session captured".to_owned()],
        pending_work: vec!["resume".to_owned()],
        next_action: Some("continue".to_owned()),
        blockers: Vec::new(),
        changed_files: Vec::new(),
        decisions: Vec::new(),
        validation: Vec::new(),
        relevant_memory_ids: Vec::new(),
        source_event_ids: vec!["handoff-prompt".to_owned()],
        git_head: None,
        worktree_state_hash: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    state.create_or_update_handoff(&handoff).unwrap();
    drop(state);

    menvane.reindex().unwrap();

    let reopened = SessionRepository::new(home.join("state.sqlite"));
    reopened.initialize().unwrap();
    assert_eq!(reopened.handoff(handoff.id).unwrap(), handoff);
    assert_eq!(
        reopened.handoff_evidence(handoff.id).unwrap(),
        vec!["handoff-prompt"]
    );
}

#[test]
fn reindex_refuses_to_replace_an_index_used_by_the_daemon() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("daemon.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    let error = menvane.reindex().unwrap_err();

    assert!(
        error
            .to_string()
            .contains("cannot reindex while the Menvane daemon is running")
    );
}

#[test]
fn stale_running_jobs_are_recovered_and_released_atomically() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();
    repository
        .ingest(
            &event(
                "stale-start",
                "stale-session",
                NormalizedEventKind::SessionStarted,
                Path::new("/tmp/menvane-stale-project"),
            ),
            None,
        )
        .unwrap();
    repository
        .ingest(
            &event(
                "stale-end",
                "stale-session",
                NormalizedEventKind::SessionEnded,
                Path::new("/tmp/menvane-stale-project"),
            ),
            None,
        )
        .unwrap();
    let claimed_at = Utc::now();
    let first = repository
        .claim_job_at("crashed-worker", 30, claimed_at)
        .unwrap()
        .unwrap();
    assert_eq!(first.status, "running");
    assert_eq!(first.owner.as_deref(), Some("crashed-worker"));
    let recovered = repository
        .claim_job_at(
            "restarted-worker",
            30,
            claimed_at + chrono::Duration::seconds(31),
        )
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, first.id);
    assert_eq!(recovered.owner.as_deref(), Some("restarted-worker"));
    assert_eq!(
        recovered.lease_started_at,
        Some(claimed_at + chrono::Duration::seconds(31))
    );
}

#[test]
fn backup_restore_round_trips_index_and_state_databases() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let backup = temporary.path().join("backup");
    let menvane = Menvane::new(&home).unwrap();
    let cwd = temporary.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let original = event(
        "original",
        "backup-session",
        NormalizedEventKind::SessionStarted,
        &cwd,
    );
    menvane.ingest_event(original).unwrap();
    let retained = menvane
        .write(
            &cwd,
            WriteMemory {
                title: "Backup retained".to_owned(),
                body: "backup-retained-memory".to_owned(),
                memory_type: MemoryType::Fact,
                scope: Scope::Global,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap()
        .metadata
        .id;
    menvane.backup(&backup).unwrap();

    let manifest: Value =
        serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
    let files = manifest["files"].as_object().unwrap();
    assert!(files.contains_key("index.sqlite"));
    assert!(files.contains_key("state.sqlite"));

    menvane
        .ingest_event(event(
            "later",
            "later-session",
            NormalizedEventKind::SessionStarted,
            &cwd,
        ))
        .unwrap();
    menvane.restore(&backup).unwrap();

    assert_eq!(row_count(&home.join("state.sqlite"), "sessions"), 1);
    assert_eq!(row_count(&home.join("state.sqlite"), "session_events"), 1);
    assert_eq!(menvane.read(retained).unwrap().title, "Backup retained");
}

fn operational_tables() -> [&'static str; 19] {
    [
        "sessions",
        "session_events",
        "observations",
        "jobs",
        "imports",
        "access_events",
        "integration_state",
        "session_injections",
        "briefing_deliveries",
        "procedure_applications",
        "orphan_sessions",
        "conversations",
        "task_episodes",
        "prompt_intents",
        "prompt_intent_history",
        "handoffs",
        "handoff_versions",
        "handoff_evidence",
        "checkpoint_state",
    ]
}

fn row_count(path: &Path, table: &str) -> i64 {
    let connection = Connection::open(path).unwrap();
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn prepare_interrupted_migration(index: &Path, state: &Path) {
    SessionRepository::new(state).initialize().unwrap();
    let connection = Connection::open(state).unwrap();
    connection
        .execute(
            "ATTACH DATABASE ?1 AS legacy",
            [index.to_string_lossy().as_ref()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions(id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported) SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported FROM legacy.sessions",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO operational_migration_markers(migration, table_name, completed_at) VALUES ('index-to-state-v1', 'sessions', '2023-11-14T22:13:20Z')",
            [],
        )
        .unwrap();
    connection.execute_batch("DETACH DATABASE legacy").unwrap();
}

fn has_table(path: &Path, table: &str) -> bool {
    let connection = Connection::open(path).unwrap();
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap()
}

fn event(
    id: &str,
    external_session_id: &str,
    kind: NormalizedEventKind,
    cwd: &Path,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
        kind,
        client: "test-client".to_owned(),
        external_session_id: external_session_id.to_owned(),
        timestamp: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
        cwd: cwd.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: Some("deterministic evidence".to_owned()),
        bounded_output: None,
        attributed_path: None,
        success: None,
        model: None,
    }
}
