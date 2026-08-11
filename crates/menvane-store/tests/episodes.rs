use std::path::Path;

use chrono::{Duration, TimeZone, Utc};
use menvane_domain::{
    EpisodeState, Goal, GoalOperation, GoalOperationKind, IntentClassificationSource,
    NormalizedEvent, NormalizedEventKind, PromptIntent, PromptIntentKind,
};
use menvane_store::{SessionRepository, conversation_key};
use rusqlite::Connection;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn migration_backfills_the_deterministic_conversation_identity() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("state.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                client TEXT NOT NULL,
                external_session_id TEXT NOT NULL,
                project_id TEXT,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                last_event_at TEXT NOT NULL,
                markdown_path TEXT,
                imported INTEGER NOT NULL DEFAULT 0,
                UNIQUE(client, external_session_id, generation)
            );
            CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
            INSERT INTO schema_migrations(version) VALUES (1), (2), (3);
            INSERT INTO sessions(id, client, external_session_id, generation, state, started_at, last_event_at)
            VALUES ('00000000-0000-7000-8000-000000000001', 'client:a', 'external:1', 1, 'open', '2023-11-14T22:13:20Z', '2023-11-14T22:13:20Z');",
        )
        .unwrap();

    let repository = SessionRepository::new(&path);
    repository.initialize().unwrap();

    let session = repository
        .session(Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap())
        .unwrap();
    assert_eq!(
        session.conversation_key,
        conversation_key("client:a", "external:1")
    );
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        12
    );
}

#[test]
fn recall_identity_matches_the_resolved_project_generation() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();
    repository
        .ingest(
            &event("a-start", "shared", NormalizedEventKind::SessionStarted, 0),
            Some("project-a"),
        )
        .unwrap();
    repository
        .ingest(
            &event("a-end", "shared", NormalizedEventKind::SessionEnded, 1),
            Some("project-a"),
        )
        .unwrap();
    repository
        .ingest(
            &event("b-start", "shared", NormalizedEventKind::SessionStarted, 2),
            Some("project-b"),
        )
        .unwrap();
    repository
        .ingest(
            &event("b-end", "shared", NormalizedEventKind::SessionEnded, 3),
            Some("project-b"),
        )
        .unwrap();
    repository
        .ingest(
            &event("g-start", "shared", NormalizedEventKind::SessionStarted, 4),
            None,
        )
        .unwrap();

    let project_a = repository
        .injection_identity("client", "shared", Some("project-a"))
        .unwrap();
    let project_b = repository
        .recall_context("client", "shared", Some("project-b"))
        .unwrap()
        .unwrap();
    let global = repository
        .injection_identity("client", "shared", None)
        .unwrap();

    assert_eq!(project_a.generation, 1);
    assert_eq!(project_a.episode_id, None);
    assert_eq!(project_b.session.generation, 2);
    assert_eq!(global.generation, 3);
}

#[test]
fn episodes_and_intents_are_idempotent_across_session_generations() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();

    let first_session = repository
        .ingest(
            &event(
                "start-1",
                "external",
                NormalizedEventKind::SessionStarted,
                0,
            ),
            Some("project-a"),
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event("prompt-1", "external", NormalizedEventKind::UserPrompt, 1),
            Some("project-a"),
        )
        .unwrap();
    let episode = repository
        .create_episode(first_session.id, "prompt-1", "Implement persistence")
        .unwrap();
    assert!(
        !repository
            .ingest(
                &event("prompt-1", "external", NormalizedEventKind::UserPrompt, 1),
                Some("project-a"),
            )
            .unwrap()
            .inserted
    );
    let intent = prompt_intent("prompt-1", episode.id, PromptIntentKind::RootGoal, 2);
    assert!(repository.record_prompt_intent(&intent).unwrap());
    assert!(!repository.record_prompt_intent(&intent).unwrap());

    repository
        .ingest(
            &event("end-1", "external", NormalizedEventKind::SessionEnded, 3),
            Some("project-a"),
        )
        .unwrap();
    let second_session = repository
        .ingest(
            &event(
                "start-2",
                "external",
                NormalizedEventKind::SessionStarted,
                4,
            ),
            Some("project-a"),
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event("prompt-2", "external", NormalizedEventKind::UserPrompt, 5),
            Some("project-a"),
        )
        .unwrap();
    assert_eq!(second_session.generation, 2);
    assert_eq!(
        first_session.conversation_key,
        second_session.conversation_key
    );

    let continued = PromptIntent {
        event_id: "prompt-2".to_owned(),
        episode_id: episode.id,
        kind: PromptIntentKind::FollowUp,
        confidence: 0.8,
        weight: 0.9,
        classifier_version: "deterministic-v1".to_owned(),
        source: IntentClassificationSource::Deterministic,
        classified_at: timestamp(6),
    };
    assert!(repository.record_prompt_intent(&continued).unwrap());
    assert_eq!(repository.prompt_intent("prompt-2").unwrap(), continued);
    assert_eq!(
        repository
            .list_active_episodes(&episode.conversation_key, Some("project-a"))
            .unwrap(),
        vec![episode]
    );
}

#[test]
fn episode_updates_and_provider_reassignment_preserve_history() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();
    let session = repository
        .ingest(
            &event("start", "external", NormalizedEventKind::SessionStarted, 0),
            Some("project-a"),
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event("prompt-a", "external", NormalizedEventKind::UserPrompt, 1),
            Some("project-a"),
        )
        .unwrap();
    repository
        .ingest(
            &event("prompt-b", "external", NormalizedEventKind::UserPrompt, 2),
            Some("project-a"),
        )
        .unwrap();
    let first = repository
        .create_episode(session.id, "prompt-a", "First task")
        .unwrap();
    let second = repository
        .create_episode(session.id, "prompt-b", "Second task")
        .unwrap();
    let original = prompt_intent("prompt-a", first.id, PromptIntentKind::RootGoal, 3);
    assert!(repository.record_prompt_intent(&original).unwrap());

    let reviewed = PromptIntent {
        event_id: "prompt-a".to_owned(),
        episode_id: second.id,
        kind: PromptIntentKind::NewGoal,
        confidence: 0.95,
        weight: 1.0,
        classifier_version: "provider-v1".to_owned(),
        source: IntentClassificationSource::ProviderReview,
        classified_at: timestamp(4),
    };
    assert!(repository.review_prompt_intent(&reviewed).unwrap());
    assert!(!repository.review_prompt_intent(&reviewed).unwrap());
    let history = repository.prompt_intent_history("prompt-a").unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].previous, original);
    assert_eq!(repository.prompt_intent("prompt-a").unwrap(), reviewed);

    let mut updated = second.clone();
    updated.goal = "Updated second task".to_owned();
    updated.state = EpisodeState::Dormant;
    updated.updated_at = timestamp(5);
    let updated = repository.update_episode(&updated).unwrap();
    assert_eq!(repository.episode(second.id).unwrap(), updated);
    assert!(
        repository
            .list_active_episodes(&updated.conversation_key, Some("project-a"))
            .unwrap()
            .iter()
            .all(|episode| episode.id != updated.id)
    );
}

#[test]
fn project_identity_blocks_session_reuse_and_episode_continuation() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();
    let first = repository
        .ingest(
            &event("start", "external", NormalizedEventKind::SessionStarted, 0),
            Some("project-a"),
        )
        .unwrap()
        .session;
    assert!(
        repository
            .ingest(
                &event("conflict", "external", NormalizedEventKind::UserPrompt, 1),
                Some("project-b"),
            )
            .is_err()
    );
    repository
        .ingest(
            &event("end", "external", NormalizedEventKind::SessionEnded, 2),
            Some("project-a"),
        )
        .unwrap();
    let second = repository
        .ingest(
            &event(
                "start-2",
                "external",
                NormalizedEventKind::SessionStarted,
                3,
            ),
            Some("project-b"),
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event("prompt-2", "external", NormalizedEventKind::UserPrompt, 4),
            Some("project-b"),
        )
        .unwrap();
    let episode = repository
        .create_episode(first.id, "start", "Project A task")
        .unwrap();
    let intent = prompt_intent("prompt-2", episode.id, PromptIntentKind::FollowUp, 5);
    assert!(repository.record_prompt_intent(&intent).is_err());
    assert_eq!(second.generation, 2);
}

#[test]
fn migration_twelve_converts_compile_jobs_and_disables_episodic_checkpoints() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("state.sqlite");
    let repository = SessionRepository::new(&path);
    repository.initialize().unwrap();
    let session = repository
        .ingest(
            &event(
                "m12-start",
                "external",
                NormalizedEventKind::SessionStarted,
                0,
            ),
            Some("project-a"),
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event("m12-prompt", "external", NormalizedEventKind::UserPrompt, 1),
            Some("project-a"),
        )
        .unwrap();
    let episode = repository
        .create_episode(session.id, "m12-prompt", "Legacy task")
        .unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version=12", [])
        .unwrap();
    connection
        .execute("DROP TABLE project_handoffs", [])
        .unwrap();
    connection
        .execute("DROP TABLE goal_event_links", [])
        .unwrap();
    connection.execute("DROP TABLE goals", []).unwrap();
    let now = "2026-01-01T00:00:00Z";
    for (id, session_part, episode_part) in [
        ("job-a", "first-session", "episode-1"),
        ("job-b", "first-session", "episode-2"),
        ("job-c", "second-session", "episode-3"),
    ] {
        connection
            .execute(
                "INSERT INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at)
                 VALUES (?1, 'compile_session', ?2, 'pending', '{}', ?3, ?3, ?3)",
                rusqlite::params![
                    format!("{id}-{episode_part}"),
                    format!("{session_part}:{episode_part}"),
                    now
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at)
             VALUES ('checkpoint-job', 'checkpoint_handoff', ?1, 'pending', '{}', ?2, ?2, ?2)",
            rusqlite::params![episode.id.to_string(), now],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO checkpoint_state(episode_id, dirty, debounce_until, last_checkpoint_at, revision, updated_at)
             VALUES (?1, 1, ?2, NULL, 1, ?3)",
            rusqlite::params![episode.id.to_string(), now, now],
        )
        .unwrap();
    drop(connection);

    let reopened = SessionRepository::new(&path);
    reopened.initialize().unwrap();
    let connection = Connection::open(&path).unwrap();
    let migration: i64 = connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migration, 12);
    let consolidate: Vec<String> = connection
        .prepare(
            "SELECT dedupe_key FROM jobs WHERE job_type='consolidate_session' ORDER BY dedupe_key",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(consolidate, vec!["first-session", "second-session"]);
    let compile_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE job_type='compile_session' AND status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(compile_count, 0);
    let checkpoint: String = connection
        .query_row(
            "SELECT status FROM jobs WHERE job_type='checkpoint_handoff' AND dedupe_key=?1",
            [episode.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(checkpoint, "completed");
    let dirty: i64 = connection
        .query_row(
            "SELECT dirty FROM checkpoint_state WHERE episode_id=?1",
            [episode.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dirty, 0);
    assert!(
        connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='project_handoffs'",
                [],
                |_| Ok(()),
            )
            .is_ok()
    );
}

#[test]
fn goal_operations_are_applied_idempotently() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();
    let session = repository
        .ingest(
            &event("start", "external", NormalizedEventKind::SessionStarted, 0),
            Some("project-a"),
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event("prompt", "external", NormalizedEventKind::UserPrompt, 1),
            Some("project-a"),
        )
        .unwrap();
    let operation = GoalOperation {
        kind: GoalOperationKind::Create,
        goal_id: None,
        summary: Some("Implement the export".to_owned()),
        event_ids: vec!["prompt".to_owned()],
    };
    let first = repository
        .apply_goal_operations(
            session.id,
            Some("project-a"),
            &session.conversation_key,
            &[operation.clone()],
        )
        .unwrap();
    let second = repository
        .apply_goal_operations(
            session.id,
            Some("project-a"),
            &session.conversation_key,
            &[operation.clone()],
        )
        .unwrap();
    assert_eq!(first.len(), 1);
    assert!(second.is_empty());
    let active = repository.active_goals(Some("project-a")).unwrap();
    assert_eq!(active.len(), 1);
    let goal: Goal = active.into_iter().next().unwrap();
    assert_eq!(goal.id, first[0]);
    assert_eq!(goal.summary, "Implement the export");
    assert_eq!(goal.state, menvane_domain::GoalState::Active);
}

fn prompt_intent(
    event_id: &str,
    episode_id: Uuid,
    kind: PromptIntentKind,
    seconds: i64,
) -> PromptIntent {
    PromptIntent {
        event_id: event_id.to_owned(),
        episode_id,
        kind,
        confidence: 0.7,
        weight: 0.8,
        classifier_version: "deterministic-v1".to_owned(),
        source: IntentClassificationSource::Deterministic,
        classified_at: timestamp(seconds),
    }
}

fn event(
    event_id: &str,
    external_session_id: &str,
    kind: NormalizedEventKind,
    seconds: i64,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: event_id.to_owned(),
        kind,
        origin: Default::default(),
        role: Default::default(),
        client: "client".to_owned(),
        external_session_id: external_session_id.to_owned(),
        timestamp: timestamp(seconds),
        cwd: Path::new("/tmp/project").to_string_lossy().into_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: Some("prompt".to_owned()),
        bounded_output: None,
        attributed_path: None,
        success: None,
        model: None,
        harness_injected: false,
    }
}

fn timestamp(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).single().unwrap() + Duration::seconds(seconds)
}
