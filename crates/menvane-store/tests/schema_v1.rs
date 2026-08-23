use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use menvane_store::{IndexStore, MarkdownStore, SessionRepository};
use rusqlite::Connection;
use tempfile::TempDir;

#[test]
fn clean_home_has_only_version_one_tables() {
    let home = TempDir::new().unwrap();
    MarkdownStore::new(home.path()).initialize().unwrap();
    IndexStore::new(home.path().join("index.sqlite"))
        .initialize()
        .unwrap();
    SessionRepository::new(home.path().join("state.sqlite"))
        .initialize()
        .unwrap();

    assert_eq!(
        tables(&home.path().join("index.sqlite")),
        BTreeSet::from(
            [
                "memories",
                "memory_embeddings",
                "memory_fts",
                "memory_fts_config",
                "memory_fts_content",
                "memory_fts_data",
                "memory_fts_docsize",
                "memory_fts_idx",
                "projects",
                "schema_meta",
                "session_summaries",
                "session_summary_fts",
                "session_summary_fts_config",
                "session_summary_fts_content",
                "session_summary_fts_data",
                "session_summary_fts_docsize",
                "session_summary_fts_idx",
            ]
            .map(str::to_owned)
        )
    );
    assert_eq!(
        tables(&home.path().join("state.sqlite")),
        BTreeSet::from(
            [
                "access_events",
                "application_events",
                "consolidation_results",
                "delivery_claims",
                "handoff_item_sources",
                "handoff_items",
                "imports",
                "integration_state",
                "jobs",
                "orphan_sessions",
                "schema_meta",
                "session_events",
                "sessions",
            ]
            .map(str::to_owned)
        )
    );
}

#[test]
fn repeated_initialization_preserves_rows_and_integrity() {
    let home = TempDir::new().unwrap();
    MarkdownStore::new(home.path()).initialize().unwrap();
    let index = IndexStore::new(home.path().join("index.sqlite"));
    let state = SessionRepository::new(home.path().join("state.sqlite"));
    index.initialize().unwrap();
    state.initialize().unwrap();
    state
        .enqueue_job("finalize_session", "session-1", "{}")
        .unwrap();
    index.initialize().unwrap();
    state.initialize().unwrap();
    assert_eq!(state.jobs().unwrap().len(), 1);
    assert_integrity(&home.path().join("index.sqlite"));
    assert_integrity(&home.path().join("state.sqlite"));
}

#[test]
fn retryable_jobs_remain_pending_and_failed_consolidations_can_be_requeued() {
    let home = TempDir::new().unwrap();
    let state = SessionRepository::new(home.path().join("state.sqlite"));
    state.initialize().unwrap();
    state
        .enqueue_job("consolidate_session", "session-retryable", "{}")
        .unwrap();

    let mut now = Utc::now();
    for _ in 0..6 {
        let job = state.claim_job_at("test-owner", 300, now).unwrap().unwrap();
        state
            .finish_job(job.id, "test-owner", None, Some("offline"), true)
            .unwrap();
        now = state.jobs().unwrap()[0].next_retry_at + Duration::seconds(1);
    }
    let job = state.jobs().unwrap().pop().unwrap();
    assert_eq!(job.status, "pending");
    assert_eq!(job.attempt_count, 6);

    state
        .enqueue_job("consolidate_session", "session-failed", "{}")
        .unwrap();
    now = Utc::now();
    for _ in 0..5 {
        let job = state.claim_job_at("test-owner", 300, now).unwrap().unwrap();
        state
            .finish_job(job.id, "test-owner", None, Some("invalid"), false)
            .unwrap();
        now = state
            .jobs()
            .unwrap()
            .into_iter()
            .find(|value| value.dedupe_key == "session-failed")
            .unwrap()
            .next_retry_at
            + Duration::seconds(1);
    }
    assert_eq!(
        state
            .jobs()
            .unwrap()
            .into_iter()
            .find(|value| value.dedupe_key == "session-failed")
            .unwrap()
            .status,
        "failed"
    );
    assert_eq!(state.retry_failed_consolidations().unwrap(), 1);
    let requeued = state
        .jobs()
        .unwrap()
        .into_iter()
        .find(|value| value.dedupe_key == "session-failed")
        .unwrap();
    assert_eq!(requeued.status, "pending");
    assert_eq!(requeued.attempt_count, 0);
}

#[test]
fn unversioned_operational_database_is_rejected() {
    let home = TempDir::new().unwrap();
    let path = home.path().join("state.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("CREATE TABLE sessions (id TEXT PRIMARY KEY)", [])
        .unwrap();
    drop(connection);
    let error = SessionRepository::new(path).initialize().unwrap_err();
    assert!(error.to_string().contains("requires recreation"));
}

fn tables(path: &std::path::Path) -> BTreeSet<String> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<BTreeSet<String>, _>>()
        .unwrap()
}

fn assert_integrity(path: &std::path::Path) {
    let connection = Connection::open(path).unwrap();
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |_| Ok(()))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(foreign_keys.is_empty());
}
