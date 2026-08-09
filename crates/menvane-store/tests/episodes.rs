use std::path::Path;

use chrono::{Duration, TimeZone, Utc};
use menvane_domain::{
    EpisodeState, IntentClassificationSource, NormalizedEvent, NormalizedEventKind, PromptIntent,
    PromptIntentKind,
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
        6
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
    }
}

fn timestamp(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).single().unwrap() + Duration::seconds(seconds)
}
