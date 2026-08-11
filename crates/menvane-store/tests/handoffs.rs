use std::path::Path;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use menvane_domain::{
    HandoffStatus, HandoffValidation, IntentClassificationSource, NormalizedEvent,
    NormalizedEventKind, PromptIntent, PromptIntentKind, TaskHandoff,
};
use menvane_store::SessionRepository;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn handoff_lifecycle_is_idempotent_and_preserves_versions() {
    let (temporary, repository, session, episode) = setup();
    let original = handoff(&session, &episode, "handoff-1", "active", "initial state");
    let stored = repository.create_or_update_handoff(&original).unwrap();
    assert_eq!(stored, original);
    assert_eq!(
        repository.create_or_update_handoff(&original).unwrap(),
        original
    );

    let mut updated = original.clone();
    updated.current_state = "updated state".to_owned();
    updated.created_at = timestamp(99);
    updated.updated_at += ChronoDuration::seconds(1);
    let updated = repository.create_or_update_handoff(&updated).unwrap();
    assert_eq!(updated.created_at, original.created_at);
    assert_eq!(
        repository.handoff(original.id).unwrap().created_at,
        original.created_at
    );
    let versions = repository.handoff_versions(original.id).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].snapshot.current_state, "initial state");

    assert_eq!(
        repository.consume_handoff(original.id).unwrap().status,
        HandoffStatus::Consumed
    );
    assert_eq!(
        repository.complete_handoff(original.id).unwrap().status,
        HandoffStatus::Completed
    );
    let versions = repository.handoff_versions(original.id).unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].revision, 1);
    assert_eq!(versions[1].revision, 2);
    assert_eq!(versions[2].revision, 3);
    assert_eq!(versions[2].status, HandoffStatus::Consumed);
    assert!(
        repository
            .handoff_for_episode(episode.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repository.complete_handoff(original.id).unwrap().status,
        HandoffStatus::Completed
    );
    assert!(
        repository
            .newest_handoff_candidates(Some("project-a"), 10)
            .unwrap()
            .is_empty()
    );

    let replacement = handoff(&session, &episode, "handoff-2", "ready", "replacement");
    repository.create_or_update_handoff(&replacement).unwrap();
    assert_eq!(
        repository
            .handoff_for_episode(episode.id)
            .unwrap()
            .unwrap()
            .id,
        replacement.id
    );
    let newer = handoff(
        &session,
        &episode,
        "handoff-3",
        "active",
        "newer replacement",
    );
    let newer = repository.create_or_update_handoff(&newer).unwrap();
    assert_eq!(
        repository
            .handoff_for_episode(episode.id)
            .unwrap()
            .unwrap()
            .id,
        replacement.id
    );
    assert_eq!(
        repository.handoff(replacement.id).unwrap().status,
        HandoffStatus::Active
    );
    assert_eq!(
        repository
            .newest_handoff_candidates(Some("project-a"), 10)
            .unwrap(),
        vec![newer.clone()]
    );
    assert_eq!(
        repository.handoff_evidence(newer.id).unwrap(),
        vec!["prompt-a"]
    );
    assert_eq!(
        repository.consume_handoff(newer.id).unwrap().status,
        HandoffStatus::Consumed
    );
    assert_eq!(
        repository.stale_handoff(newer.id).unwrap().status,
        HandoffStatus::Stale
    );
    assert_eq!(
        repository.stale_handoff(newer.id).unwrap().status,
        HandoffStatus::Stale
    );
    assert!(
        repository
            .newest_handoff_candidates(Some("project-a"), 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .handoff_for_episode(episode.id)
            .unwrap()
            .is_none()
    );
    drop(temporary);
}

#[test]
fn full_delivery_claim_and_consumption_are_atomic_and_idempotent() {
    let (temporary, repository, session, episode) = setup();
    let original = handoff(&session, &episode, "atomic", "active", "state");
    repository.create_or_update_handoff(&original).unwrap();
    let identity = menvane_store::InjectionIdentity {
        client: "target".to_owned(),
        conversation_key: "target-conversation".to_owned(),
        generation: 4,
        episode_id: None,
    };
    assert!(repository.deliver_handoff(&identity, original.id).unwrap());
    assert_eq!(
        repository.handoff(original.id).unwrap().status,
        HandoffStatus::Consumed
    );
    assert_eq!(repository.handoff_versions(original.id).unwrap().len(), 1);
    assert!(!repository.deliver_handoff(&identity, original.id).unwrap());
    assert_eq!(repository.handoff_versions(original.id).unwrap().len(), 1);
    let connection = rusqlite::Connection::open(temporary.path().join("state.sqlite")).unwrap();
    let deliveries: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM handoff_deliveries WHERE delivery_kind='full'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(deliveries, 1);
}

#[test]
fn card_delivery_claim_does_not_consume_or_duplicate() {
    let (temporary, repository, session, episode) = setup();
    let original = handoff(&session, &episode, "card", "ready", "state");
    repository.create_or_update_handoff(&original).unwrap();
    let identity = menvane_store::InjectionIdentity {
        client: "target".to_owned(),
        conversation_key: "target-conversation".to_owned(),
        generation: 4,
        episode_id: None,
    };
    assert!(
        repository
            .claim_handoff_delivery(&identity, original.id, "card")
            .unwrap()
    );
    assert!(
        !repository
            .claim_handoff_delivery(&identity, original.id, "card")
            .unwrap()
    );
    assert_eq!(
        repository.handoff(original.id).unwrap().status,
        HandoffStatus::Ready
    );
    repository.stale_handoff(original.id).unwrap();
    assert!(!repository.deliver_handoff(&identity, original.id).unwrap());
    let connection = rusqlite::Connection::open(temporary.path().join("state.sqlite")).unwrap();
    let full_deliveries: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM handoff_deliveries WHERE delivery_kind='full'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(full_deliveries, 0);
}

#[test]
fn checkpoint_triggers_are_deduplicated_and_completed_jobs_requeue() {
    let (_temporary, repository, _session, episode) = setup();
    let now = timestamp(10);
    let first = repository
        .mark_handoff_dirty_at(episode.id, Duration::from_secs(10), now)
        .unwrap();
    let second = repository
        .mark_handoff_dirty_at(episode.id, Duration::from_secs(10), now)
        .unwrap();
    assert_eq!(first.updated_at, second.updated_at);
    assert_eq!(second.revision, first.revision + 1);
    assert_eq!(repository.jobs().unwrap().len(), 1);

    let job = repository
        .claim_job_at("worker", 30, now + ChronoDuration::seconds(11))
        .unwrap()
        .unwrap();
    repository.finish_job(job.id, "worker", None, None).unwrap();
    assert_eq!(repository.jobs().unwrap()[0].status, "completed");

    let requeued = repository
        .mark_handoff_dirty_at(
            episode.id,
            Duration::from_secs(1),
            now + ChronoDuration::seconds(20),
        )
        .unwrap();
    assert!(requeued.dirty);
    let jobs = repository.jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "pending");
    assert!(!repository.complete_checkpoint(episode.id).unwrap().dirty);
    assert!(!repository.complete_checkpoint(episode.id).unwrap().dirty);
}

#[test]
fn stale_checkpoint_completion_does_not_clear_newer_dirty_state() {
    let (_temporary, repository, _session, episode) = setup();
    let observed = repository
        .mark_handoff_dirty_at(episode.id, Duration::from_secs(1), timestamp(10))
        .unwrap();
    let newer = repository
        .mark_handoff_dirty_at(episode.id, Duration::from_secs(1), timestamp(10))
        .unwrap();

    let after_stale_completion = repository
        .complete_checkpoint_if_unchanged(episode.id, observed.updated_at, observed.revision)
        .unwrap();
    assert!(after_stale_completion.dirty);
    assert_eq!(after_stale_completion.revision, newer.revision);

    let completed = repository
        .complete_checkpoint_if_unchanged(episode.id, newer.updated_at, newer.revision)
        .unwrap();
    assert!(!completed.dirty);
    assert_eq!(
        repository
            .complete_checkpoint_if_unchanged(episode.id, newer.updated_at, newer.revision)
            .unwrap(),
        completed
    );
}

#[test]
fn evidence_can_cross_session_generations_within_one_episode() {
    let temporary = TempDir::new().unwrap();
    let repository = SessionRepository::new(temporary.path().join("state.sqlite"));
    repository.initialize().unwrap();
    let first = repository
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
            &event("prompt-a", "external", NormalizedEventKind::UserPrompt, 1),
            Some("project-a"),
        )
        .unwrap();
    let episode = repository
        .create_episode(first.id, "prompt-a", "cross generation handoff")
        .unwrap();
    repository
        .record_prompt_intent(&prompt_intent(
            "prompt-a",
            episode.id,
            PromptIntentKind::RootGoal,
            2,
        ))
        .unwrap();
    repository
        .ingest(
            &event("end-1", "external", NormalizedEventKind::SessionEnded, 3),
            Some("project-a"),
        )
        .unwrap();
    let second = repository
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
            &event("prompt-b", "external", NormalizedEventKind::UserPrompt, 5),
            Some("project-a"),
        )
        .unwrap();
    repository
        .record_prompt_intent(&prompt_intent(
            "prompt-b",
            episode.id,
            PromptIntentKind::FollowUp,
            6,
        ))
        .unwrap();

    let mut handoff = handoff(&first, &episode, "cross-generation", "ready", "state");
    handoff.source_session_id = second.id;
    handoff.source_event_ids = vec!["prompt-a".to_owned(), "prompt-b".to_owned()];
    handoff.validation = vec![HandoffValidation {
        event_id: "prompt-a".to_owned(),
        command: None,
        success: true,
        summary: "validated".to_owned(),
        timestamp: timestamp(7),
    }];
    repository.create_or_update_handoff(&handoff).unwrap();

    let evidence = repository.handoff_evidence_records(handoff.id).unwrap();
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[0].source_session_id, first.id);
    assert_eq!(evidence[1].source_session_id, second.id);
}

#[test]
fn evidence_relationships_and_bounds_are_checked_before_persistence() {
    let (_temporary, repository, session, episode) = setup();
    let mut invalid_event = handoff(&session, &episode, "invalid-event", "active", "state");
    invalid_event.source_event_ids = vec!["missing".to_owned()];
    assert!(repository.create_or_update_handoff(&invalid_event).is_err());
    assert!(repository.handoff(invalid_event.id).is_err());

    repository
        .ingest(
            &event(
                "prompt-other",
                "external",
                NormalizedEventKind::UserPrompt,
                2,
            ),
            Some("project-a"),
        )
        .unwrap();
    let other_episode = repository
        .create_episode(session.id, "prompt-other", "other episode")
        .unwrap();
    repository
        .record_prompt_intent(&prompt_intent(
            "prompt-other",
            other_episode.id,
            PromptIntentKind::RootGoal,
            3,
        ))
        .unwrap();
    let mut conflicting_prompt =
        handoff(&session, &episode, "conflicting-prompt", "active", "state");
    conflicting_prompt.source_event_ids = vec!["prompt-other".to_owned()];
    repository
        .create_or_update_handoff(&conflicting_prompt)
        .unwrap();

    let mut oversized = handoff(&session, &episode, "oversized", "active", "state");
    oversized.goal = "x".repeat(2_049);
    assert!(repository.create_or_update_handoff(&oversized).is_err());

    let mut diff = handoff(&session, &episode, "diff", "active", "diff --git a/a b/b");
    assert!(repository.create_or_update_handoff(&diff).is_err());
    diff.current_state = "valid".to_owned();
    diff.pending_work = (0..33).map(|value| value.to_string()).collect();
    assert!(repository.create_or_update_handoff(&diff).is_err());

    assert!(
        repository
            .list_handoffs(Some("project-a"), None, 101)
            .is_err()
    );
}

#[test]
fn bounded_handoff_surfaces_cover_all_project_session_and_detail() {
    let (_temporary, repository, session, episode) = setup();
    let handoff = handoff(&session, &episode, "surface", "ready", "state");
    repository.create_or_update_handoff(&handoff).unwrap();

    assert_eq!(
        repository
            .all_handoffs(None, 10)
            .unwrap()
            .first()
            .unwrap()
            .id,
        handoff.id
    );
    assert_eq!(
        repository
            .project_handoffs("project-a", None, 10)
            .unwrap()
            .first()
            .unwrap()
            .id,
        handoff.id
    );
    assert_eq!(
        repository
            .session_handoffs(session.id, None, 10)
            .unwrap()
            .first()
            .unwrap()
            .id,
        handoff.id
    );
    let detail = repository.handoff_detail(handoff.id).unwrap().unwrap();
    assert_eq!(detail.handoff.id, handoff.id);
    assert!(detail.versions.is_empty());
    assert_eq!(detail.evidence[0].event_id, "prompt-a");
    assert!(repository.handoff_detail(Uuid::now_v7()).unwrap().is_none());
}

#[test]
fn state_reopen_preserves_handoffs_versions_and_evidence() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("state.sqlite");
    let repository = SessionRepository::new(&path);
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
    let episode = repository
        .create_episode(session.id, "prompt-a", "persist state")
        .unwrap();
    let handoff = handoff(&session, &episode, "persisted", "ready", "state");
    repository.create_or_update_handoff(&handoff).unwrap();
    repository
        .mark_handoff_dirty_at(episode.id, Duration::from_secs(1), timestamp(2))
        .unwrap();
    drop(repository);

    let reopened = SessionRepository::new(&path);
    reopened.initialize().unwrap();
    assert_eq!(reopened.handoff(handoff.id).unwrap(), handoff);
    assert_eq!(
        reopened.handoff_evidence(handoff.id).unwrap(),
        vec!["prompt-a"]
    );
    assert!(reopened.checkpoint_state(episode.id).unwrap().dirty);
}

#[test]
fn current_handoff_identity_is_project_scoped_and_global_is_explicitly_unique() {
    let (_temporary, repository, session, episode) = setup();
    let first = handoff(&session, &episode, "project-first", "ready", "first");
    repository.create_or_update_handoff(&first).unwrap();
    repository
        .ingest(
            &event(
                "project-prompt-2",
                "external",
                NormalizedEventKind::UserPrompt,
                2,
            ),
            Some("project-a"),
        )
        .unwrap();
    let second_episode = repository
        .create_episode(session.id, "project-prompt-2", "second episode")
        .unwrap();
    let second = handoff(
        &session,
        &second_episode,
        "project-second",
        "ready",
        "second",
    );
    let stored = repository.create_or_update_handoff(&second).unwrap();
    assert_eq!(stored.id, first.id);
    assert_eq!(
        repository
            .project_handoffs("project-a", None, 10)
            .unwrap()
            .len(),
        1
    );

    let global_session = repository
        .ingest(
            &event(
                "global-start",
                "global-external",
                NormalizedEventKind::SessionStarted,
                3,
            ),
            None,
        )
        .unwrap()
        .session;
    repository
        .ingest(
            &event(
                "global-prompt",
                "global-external",
                NormalizedEventKind::UserPrompt,
                4,
            ),
            None,
        )
        .unwrap();
    let global_episode = repository
        .create_episode(global_session.id, "global-prompt", "global episode")
        .unwrap();
    let mut global = handoff(
        &global_session,
        &global_episode,
        "global",
        "ready",
        "global",
    );
    global.source_event_ids = vec!["global-prompt".to_owned()];
    repository.create_or_update_handoff(&global).unwrap();
    let mut global_update = global.clone();
    global_update.id = Uuid::now_v7();
    global_update.current_state = "global update".to_owned();
    assert_eq!(
        repository
            .create_or_update_handoff(&global_update)
            .unwrap()
            .id,
        global.id
    );
    assert_eq!(
        repository.current_handoff(None).unwrap().unwrap().id,
        global.id
    );
    assert_eq!(
        repository
            .current_handoff(Some("project-a"))
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
}

#[test]
fn migration_quarantines_contaminated_and_deduplicates_episode_handoffs() {
    let (temporary, repository, session, episode) = setup();
    let first = handoff(&session, &episode, "legacy-first", "ready", "first");
    repository.create_or_update_handoff(&first).unwrap();
    repository
        .ingest(
            &event(
                "legacy-prompt-2",
                "external",
                NormalizedEventKind::UserPrompt,
                2,
            ),
            Some("project-a"),
        )
        .unwrap();
    let second_episode = repository
        .create_episode(session.id, "legacy-prompt-2", "legacy second")
        .unwrap();
    let connection = rusqlite::Connection::open(temporary.path().join("state.sqlite")).unwrap();
    connection
        .execute("DROP INDEX handoffs_current_project", [])
        .unwrap();
    let second_id = Uuid::now_v7();
    connection
        .execute(
            "INSERT INTO handoffs(id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, revision, created_at, updated_at)
             SELECT ?1, project_id, conversation_key, ?2, source_session_id, source_client, status, goal, 'newer state', completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, revision, created_at, ?3 FROM handoffs WHERE id=?4",
            rusqlite::params![
                second_id.to_string(),
                second_episode.id.to_string(),
                timestamp(99).to_rfc3339(),
                first.id.to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE handoffs SET goal='AGENTS.md contaminated' WHERE id=?1",
            [first.id.to_string()],
        )
        .unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version=11", [])
        .unwrap();
    drop(connection);
    let reopened = SessionRepository::new(temporary.path().join("state.sqlite"));
    reopened.initialize().unwrap();
    assert_eq!(
        reopened
            .current_handoff(Some("project-a"))
            .unwrap()
            .unwrap()
            .id,
        second_id
    );
    assert_eq!(
        reopened.handoff(first.id).unwrap().status,
        HandoffStatus::Superseded
    );
}

fn setup() -> (
    TempDir,
    SessionRepository,
    menvane_store::SessionRecord,
    menvane_domain::TaskEpisode,
) {
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
    let episode = repository
        .create_episode(session.id, "prompt-a", "persist handoff")
        .unwrap();
    (temporary, repository, session, episode)
}

fn handoff(
    session: &menvane_store::SessionRecord,
    episode: &menvane_domain::TaskEpisode,
    id: &str,
    status: &str,
    current_state: &str,
) -> TaskHandoff {
    TaskHandoff {
        id: Uuid::now_v7(),
        project_id: episode.project_id.clone(),
        conversation_key: episode.conversation_key.clone(),
        episode_id: episode.id,
        source_session_id: session.id,
        source_client: session.client.clone(),
        status: match status {
            "active" => HandoffStatus::Active,
            "ready" => HandoffStatus::Ready,
            _ => panic!("unsupported test status"),
        },
        goal: format!("{id} goal"),
        current_state: current_state.to_owned(),
        completed_work: vec!["captured evidence".to_owned()],
        pending_work: vec!["continue work".to_owned()],
        next_action: Some("inspect state".to_owned()),
        blockers: Vec::new(),
        changed_files: vec!["src/lib.rs".to_owned()],
        decisions: vec!["state-only".to_owned()],
        validation: Vec::new(),
        relevant_memory_ids: vec![Uuid::now_v7()],
        source_event_ids: vec!["prompt-a".to_owned()],
        git_head: Some("abc123".to_owned()),
        worktree_state_hash: Some("hash123".to_owned()),
        created_at: timestamp(2),
        updated_at: timestamp(2),
    }
}

fn event(
    id: &str,
    external_session_id: &str,
    kind: NormalizedEventKind,
    seconds: i64,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
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
    }
}

fn timestamp(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).single().unwrap() + ChronoDuration::seconds(seconds)
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
        confidence: 0.9,
        weight: 1.0,
        classifier_version: "test-v1".to_owned(),
        source: IntentClassificationSource::Deterministic,
        classified_at: timestamp(seconds),
    }
}
