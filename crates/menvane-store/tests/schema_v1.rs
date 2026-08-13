use std::collections::BTreeSet;

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
