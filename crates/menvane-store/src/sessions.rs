use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use menvane_domain::{
    EpisodeState, HandoffStatus, IntentClassificationSource, NormalizedEvent, NormalizedEventKind,
    PromptIntent, PromptIntentKind, ReinforcementSignal, SessionState, TaskEpisode, TaskHandoff,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SESSION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
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
    conversation_key TEXT NOT NULL DEFAULT '',
    UNIQUE(client, external_session_id, generation)
);
CREATE INDEX IF NOT EXISTS sessions_external ON sessions(client, external_session_id, generation DESC);
CREATE INDEX IF NOT EXISTS sessions_state_event ON sessions(state, last_event_at);
CREATE TABLE IF NOT EXISTS conversations (
    conversation_key TEXT PRIMARY KEY,
    client TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(client, external_session_id)
);
CREATE TABLE IF NOT EXISTS session_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);
CREATE INDEX IF NOT EXISTS session_events_session ON session_events(session_id, timestamp, event_id);
CREATE TABLE IF NOT EXISTS event_episode_links (
    event_id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL,
    linked_at TEXT NOT NULL,
    FOREIGN KEY(event_id) REFERENCES session_events(event_id),
    FOREIGN KEY(episode_id) REFERENCES task_episodes(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS event_episode_links_episode ON event_episode_links(episode_id, event_id);
CREATE TABLE IF NOT EXISTS observations (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    event_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    job_type TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT NOT NULL,
    last_error TEXT,
    provider TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(job_type, dedupe_key)
);
CREATE INDEX IF NOT EXISTS jobs_ready ON jobs(status, next_retry_at);
CREATE TABLE IF NOT EXISTS imports (
    id TEXT PRIMARY KEY,
    client TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(client, external_session_id)
);
CREATE TABLE IF NOT EXISTS access_events (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    signal TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS integration_state (
    client TEXT PRIMARY KEY,
    connected INTEGER NOT NULL,
    mcp_registered INTEGER NOT NULL,
    hook_status TEXT NOT NULL,
    last_event_at TEXT,
    details_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS session_injections (
    client TEXT NOT NULL,
    conversation_key TEXT NOT NULL,
    generation INTEGER NOT NULL,
    episode_id TEXT NOT NULL DEFAULT '',
    memory_id TEXT NOT NULL,
    injected_at TEXT NOT NULL,
    PRIMARY KEY(client, conversation_key, generation, episode_id, memory_id)
);
CREATE TABLE IF NOT EXISTS briefing_deliveries (
    client TEXT NOT NULL,
    conversation_key TEXT NOT NULL,
    generation INTEGER NOT NULL,
    episode_id TEXT NOT NULL DEFAULT '',
    delivered_at TEXT NOT NULL,
    PRIMARY KEY(client, conversation_key, generation, episode_id)
);
CREATE TABLE IF NOT EXISTS handoff_deliveries (
    client TEXT NOT NULL,
    conversation_key TEXT NOT NULL,
    generation INTEGER NOT NULL,
    handoff_id TEXT NOT NULL,
    delivery_kind TEXT NOT NULL CHECK(delivery_kind IN ('full', 'card')),
    delivered_at TEXT NOT NULL,
    PRIMARY KEY(client, conversation_key, generation, handoff_id, delivery_kind),
    FOREIGN KEY(handoff_id) REFERENCES handoffs(id)
);
CREATE TABLE IF NOT EXISTS procedure_applications (
    memory_id TEXT NOT NULL,
    source_session TEXT NOT NULL,
    signal TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(memory_id, source_session, signal)
);
CREATE TABLE IF NOT EXISTS orphan_sessions (
    client TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(client, external_session_id)
);
CREATE TABLE IF NOT EXISTS task_episodes (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    conversation_key TEXT NOT NULL,
    root_event_id TEXT NOT NULL,
    goal TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal > 0),
    state TEXT NOT NULL CHECK(state IN ('active', 'dormant', 'completed')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(root_event_id) REFERENCES session_events(event_id)
);
CREATE INDEX IF NOT EXISTS task_episodes_active ON task_episodes(conversation_key, project_id, state, ordinal);
CREATE TABLE IF NOT EXISTS prompt_intents (
    event_id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL,
    conversation_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    confidence REAL NOT NULL,
    weight REAL NOT NULL,
    classifier_version TEXT NOT NULL,
    source TEXT NOT NULL,
    classified_at TEXT NOT NULL,
    FOREIGN KEY(event_id) REFERENCES session_events(event_id),
    FOREIGN KEY(episode_id) REFERENCES task_episodes(id)
);
CREATE INDEX IF NOT EXISTS prompt_intents_episode ON prompt_intents(episode_id, classified_at, event_id);
CREATE TABLE IF NOT EXISTS prompt_intent_history (
    event_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    conversation_key TEXT NOT NULL,
    episode_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    confidence REAL NOT NULL,
    weight REAL NOT NULL,
    classifier_version TEXT NOT NULL,
    source TEXT NOT NULL,
    classified_at TEXT NOT NULL,
    replaced_at TEXT NOT NULL,
    PRIMARY KEY(event_id, revision),
    FOREIGN KEY(event_id) REFERENCES session_events(event_id)
);
CREATE INDEX IF NOT EXISTS prompt_intent_history_event ON prompt_intent_history(event_id, revision);
CREATE TABLE IF NOT EXISTS handoffs (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    conversation_key TEXT NOT NULL,
    episode_id TEXT NOT NULL,
    source_session_id TEXT NOT NULL,
    source_client TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active', 'ready', 'consumed', 'completed', 'stale', 'superseded')),
    goal TEXT NOT NULL,
    current_state TEXT NOT NULL,
    completed_work_json TEXT NOT NULL,
    pending_work_json TEXT NOT NULL,
    next_action TEXT,
    blockers_json TEXT NOT NULL,
    changed_files_json TEXT NOT NULL,
    decisions_json TEXT NOT NULL,
    validation_json TEXT NOT NULL,
    relevant_memory_ids_json TEXT NOT NULL,
    source_event_ids_json TEXT NOT NULL,
    git_head TEXT,
    worktree_state_hash TEXT,
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(episode_id) REFERENCES task_episodes(id),
    FOREIGN KEY(source_session_id) REFERENCES sessions(id)
);
CREATE UNIQUE INDEX IF NOT EXISTS handoffs_current_episode
    ON handoffs(episode_id)
    WHERE status IN ('active', 'ready', 'consumed');
CREATE INDEX IF NOT EXISTS handoffs_project_status ON handoffs(project_id, status, updated_at DESC);
CREATE TABLE IF NOT EXISTS handoff_versions (
    handoff_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(handoff_id, revision),
    FOREIGN KEY(handoff_id) REFERENCES handoffs(id)
);
CREATE INDEX IF NOT EXISTS handoff_versions_handoff ON handoff_versions(handoff_id, revision);
CREATE TABLE IF NOT EXISTS handoff_evidence (
    handoff_id TEXT NOT NULL,
    source_session_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    PRIMARY KEY(handoff_id, event_id),
    FOREIGN KEY(handoff_id) REFERENCES handoffs(id),
    FOREIGN KEY(source_session_id) REFERENCES sessions(id),
    FOREIGN KEY(event_id) REFERENCES session_events(event_id)
);
CREATE INDEX IF NOT EXISTS handoff_evidence_event ON handoff_evidence(event_id);
CREATE TABLE IF NOT EXISTS checkpoint_state (
    episode_id TEXT PRIMARY KEY,
    dirty INTEGER NOT NULL CHECK(dirty IN (0, 1)),
    debounce_until TEXT,
    last_checkpoint_at TEXT,
    revision INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(episode_id) REFERENCES task_episodes(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS operational_migration_markers (
    migration TEXT NOT NULL,
    table_name TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    PRIMARY KEY(migration, table_name)
);
"#;

const OPERATIONAL_MIGRATION: &str = "index-to-state-v1";
pub const MAX_HANDOFF_LIST_LIMIT: usize = 100;
pub const MAX_HANDOFF_TEXT_BYTES: usize = 4_096;
pub const MAX_HANDOFF_GOAL_BYTES: usize = 2_048;
pub const MAX_HANDOFF_LIST_ITEMS: usize = 32;
pub const MAX_HANDOFF_ITEM_BYTES: usize = 1_024;
pub const MAX_HANDOFF_VALIDATIONS: usize = 32;
pub const MAX_HANDOFF_MEMORY_IDS: usize = 64;
pub const MAX_HANDOFF_SOURCE_EVENTS: usize = 128;
pub const MAX_HANDOFF_CHANGED_FILES: usize = 128;
pub const MAX_HANDOFF_TOTAL_BYTES: usize = 32_768;
pub const MAX_HANDOFF_VERSIONS: usize = 100;
pub const MAX_CHECKPOINT_DEBOUNCE_SECONDS: i64 = 86_400;

const OPERATIONAL_TABLES: &[(&str, &str)] = &[
    (
        "sessions",
        "id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported",
    ),
    (
        "session_events",
        "event_id, session_id, kind, timestamp, payload_json",
    ),
    (
        "observations",
        "id, session_id, event_id, kind, content, created_at",
    ),
    (
        "jobs",
        "id, job_type, dedupe_key, status, payload_json, attempt_count, next_retry_at, last_error, provider, created_at, updated_at",
    ),
    (
        "imports",
        "id, client, external_session_id, status, created_at",
    ),
    ("access_events", "id, memory_id, signal, created_at"),
    (
        "integration_state",
        "client, connected, mcp_registered, hook_status, last_event_at, details_json",
    ),
    (
        "session_injections",
        "client, conversation_key, generation, episode_id, memory_id, injected_at",
    ),
    (
        "briefing_deliveries",
        "client, conversation_key, generation, episode_id, delivered_at",
    ),
    (
        "procedure_applications",
        "memory_id, source_session, signal, created_at",
    ),
    (
        "orphan_sessions",
        "client, external_session_id, payload_json, created_at",
    ),
    (
        "conversations",
        "conversation_key, client, external_session_id, created_at, updated_at",
    ),
    (
        "task_episodes",
        "id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at",
    ),
    ("event_episode_links", "event_id, episode_id, linked_at"),
    (
        "prompt_intents",
        "event_id, episode_id, conversation_key, kind, confidence, weight, classifier_version, source, classified_at",
    ),
    (
        "prompt_intent_history",
        "event_id, revision, conversation_key, episode_id, kind, confidence, weight, classifier_version, source, classified_at, replaced_at",
    ),
    (
        "handoffs",
        "id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, revision, created_at, updated_at",
    ),
    (
        "handoff_deliveries",
        "client, conversation_key, generation, handoff_id, delivery_kind, delivered_at",
    ),
    (
        "handoff_versions",
        "handoff_id, revision, status, snapshot_json, created_at",
    ),
    (
        "handoff_evidence",
        "handoff_id, source_session_id, event_id, ordinal",
    ),
    (
        "checkpoint_state",
        "episode_id, dirty, debounce_until, last_checkpoint_at, revision, updated_at",
    ),
];

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: Uuid,
    pub client: String,
    pub external_session_id: String,
    pub project_id: Option<String>,
    pub generation: u32,
    pub state: SessionState,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_event_at: DateTime<Utc>,
    pub markdown_path: Option<PathBuf>,
    pub imported: bool,
    pub conversation_key: String,
}

#[derive(Debug, Clone)]
pub struct EpisodeEvent {
    pub event: NormalizedEvent,
    pub session_id: Uuid,
    pub generation: u32,
    pub client: String,
    pub external_session_id: String,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptIntentHistory {
    pub event_id: String,
    pub revision: u32,
    pub conversation_key: String,
    pub previous: PromptIntent,
    pub replaced_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RecallContext {
    pub session: SessionRecord,
    pub active_episode: Option<TaskEpisode>,
    pub active_corrections: Vec<String>,
    pub active_constraints: Vec<String>,
    pub conversation_root_goal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionIdentity {
    pub client: String,
    pub conversation_key: String,
    pub generation: u32,
    pub episode_id: Option<Uuid>,
}

pub struct IngestResult {
    pub session: SessionRecord,
    pub inserted: bool,
    pub should_finalize: bool,
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: Uuid,
    pub job_type: String,
    pub status: String,
    pub attempt_count: u32,
    pub next_retry_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub dedupe_key: String,
    pub owner: Option<String>,
    pub lease_started_at: Option<DateTime<Utc>>,
    pub lease_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointState {
    pub episode_id: Uuid,
    pub dirty: bool,
    pub debounce_until: Option<DateTime<Utc>>,
    pub last_checkpoint_at: Option<DateTime<Utc>>,
    pub revision: u32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffEvidence {
    pub handoff_id: Uuid,
    pub source_session_id: Uuid,
    pub event_id: String,
    pub ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HandoffVersion {
    pub handoff_id: Uuid,
    pub revision: u32,
    pub status: HandoffStatus,
    pub snapshot: TaskHandoff,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HandoffDetail {
    pub handoff: TaskHandoff,
    pub versions: Vec<HandoffVersion>,
    pub evidence: Vec<HandoffEvidence>,
}

pub struct IntegrationRecord {
    pub client: String,
    pub connected: bool,
    pub mcp_registered: bool,
    pub hook_status: String,
    pub last_event_at: Option<DateTime<Utc>>,
}

pub struct OrphanRecord {
    pub client: String,
    pub external_session_id: String,
    pub payload_json: String,
}

#[derive(Debug, Clone)]
pub struct SessionRepository {
    path: PathBuf,
}

impl SessionRepository {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn initialize(&self) -> Result<()> {
        self.initialize_with_legacy(None)
    }

    pub fn initialize_with_legacy(&self, legacy_index: Option<&Path>) -> Result<()> {
        let mut connection = self.open()?;
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        connection.execute_batch(SESSION_SCHEMA)?;
        apply_migrations(&connection)?;
        if let Some(legacy_index) = legacy_index
            && legacy_index != self.path.as_path()
            && legacy_index.exists()
        {
            migrate_legacy_operational_tables(&mut connection, legacy_index)?;
        }
        ensure_conversation_keys(&connection)?;
        Ok(())
    }

    pub fn ingest(
        &self,
        event: &NormalizedEvent,
        project_id: Option<&str>,
    ) -> Result<IngestResult> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if event_exists(&transaction, &event.event_id)? {
            let session_id: String = transaction.query_row(
                "SELECT session_id FROM session_events WHERE event_id=?1",
                [&event.event_id],
                |row| row.get(0),
            )?;
            let session = session_by_id(&transaction, Uuid::parse_str(&session_id)?)?;
            transaction.commit()?;
            return Ok(IngestResult {
                session,
                inserted: false,
                should_finalize: false,
            });
        }
        let previous = latest_session(&transaction, &event.client, &event.external_session_id)?;
        let session = match previous {
            Some(previous) if previous.state != SessionState::Finalized => {
                if previous
                    .project_id
                    .as_deref()
                    .is_some_and(|previous| Some(previous) != project_id)
                {
                    bail!(
                        "active session {} cannot be reused across project identities",
                        previous.id
                    );
                }
                previous
            }
            Some(previous) => {
                create_session(&transaction, event, project_id, previous.generation + 1)?
            }
            None => create_session(&transaction, event, project_id, 1)?,
        };
        transaction.execute(
            "INSERT INTO session_events(event_id, session_id, kind, timestamp, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.event_id,
                session.id.to_string(),
                event_kind(event.kind),
                event.timestamp.to_rfc3339(),
                serde_json::to_string(event)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO integration_state(client, connected, mcp_registered, hook_status, last_event_at, details_json) VALUES (?1, 1, 0, 'event received', ?2, '{}') ON CONFLICT(client) DO UPDATE SET last_event_at=excluded.last_event_at",
            params![event.client, event.timestamp.to_rfc3339()],
        )?;
        if let Some(content) = observation_content(event) {
            transaction.execute(
                "INSERT INTO observations(id, session_id, event_id, kind, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Uuid::now_v7().to_string(),
                    session.id.to_string(),
                    event.event_id,
                    event_kind(event.kind),
                    content,
                    event.timestamp.to_rfc3339(),
                ],
            )?;
        }
        let state = state_after_event(session.state, event.kind);
        let ended_at = (state == SessionState::Finalized).then(|| event.timestamp.to_rfc3339());
        transaction.execute(
            "UPDATE sessions SET state=?1, last_event_at=?2, ended_at=COALESCE(?3, ended_at), project_id=COALESCE(project_id, ?4) WHERE id=?5",
            params![
                session_state(state),
                event.timestamp.to_rfc3339(),
                ended_at,
                project_id,
                session.id.to_string()
            ],
        )?;
        if state == SessionState::Finalized {
            enqueue_job(&transaction, "finalize_session", &session.id.to_string())?;
        }
        let session = session_by_id(&transaction, session.id)?;
        transaction.commit()?;
        Ok(IngestResult {
            session,
            inserted: true,
            should_finalize: state == SessionState::Finalized,
        })
    }

    pub fn events(&self, session_id: Uuid) -> Result<Vec<NormalizedEvent>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM session_events WHERE session_id=?1 ORDER BY timestamp, event_id",
        )?;
        let rows = statement.query_map([session_id.to_string()], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn associate_event_with_active_episode(&self, event_id: &str) -> Result<Option<Uuid>> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event: Option<(String, Option<String>, String)> = transaction
            .query_row(
                "SELECT s.conversation_key, s.project_id, s.id FROM session_events e JOIN sessions s ON s.id=e.session_id WHERE e.event_id=?1",
                [event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((conversation_key, project_id, _session_id)) = event else {
            return Ok(None);
        };
        let episode: Option<String> = transaction
            .query_row(
                "SELECT id FROM task_episodes WHERE conversation_key=?1 AND project_id IS ?2 AND state='active' ORDER BY ordinal DESC, id DESC LIMIT 1",
                params![conversation_key, project_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(episode_id) = episode else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute(
            "INSERT OR IGNORE INTO event_episode_links(event_id, episode_id, linked_at) VALUES (?1, ?2, ?3)",
            params![event_id, episode_id, Utc::now().to_rfc3339()],
        )?;
        let linked: String = transaction.query_row(
            "SELECT episode_id FROM event_episode_links WHERE event_id=?1",
            [event_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some(Uuid::parse_str(&linked)?))
    }

    pub fn event_episode(&self, event_id: &str) -> Result<Option<Uuid>> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT episode_id FROM event_episode_links WHERE event_id=?1",
                [event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| Uuid::parse_str(&value))
            .transpose()
            .map_err(Into::into)
    }

    pub fn episode_events(&self, episode_id: Uuid) -> Result<Vec<EpisodeEvent>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT e.payload_json, s.id, s.generation, s.client, s.external_session_id, s.project_id FROM event_episode_links l JOIN session_events e ON e.event_id=l.event_id JOIN sessions s ON s.id=e.session_id WHERE l.episode_id=?1 ORDER BY e.timestamp, e.event_id",
        )?;
        let rows = statement.query_map([episode_id.to_string()], |row| {
            let payload: String = row.get(0)?;
            let session_id: String = row.get(1)?;
            let event = serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((
                event,
                session_id,
                row.get::<_, u32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (event, session_id, generation, client, external_session_id, project_id) = row?;
            Ok(EpisodeEvent {
                event,
                session_id: Uuid::parse_str(&session_id).map_err(sql_conversion_error)?,
                generation,
                client,
                external_session_id,
                project_id,
            })
        })
        .collect::<Result<Vec<_>, rusqlite::Error>>()
        .map_err(Into::into)
    }

    pub fn episode_prompt_intents(&self, episode_id: Uuid) -> Result<Vec<PromptIntent>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT i.event_id, i.episode_id, i.kind, i.confidence, i.weight, i.classifier_version, i.source, i.classified_at FROM prompt_intents i WHERE i.episode_id=?1 ORDER BY i.classified_at, i.event_id",
        )?;
        let rows = statement.query_map([episode_id.to_string()], prompt_intent_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn episode_events_for_session(&self, session_id: Uuid) -> Result<Vec<Uuid>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT l.episode_id FROM event_episode_links l JOIN session_events e ON e.event_id=l.event_id WHERE e.session_id=?1 ORDER BY l.episode_id",
        )?;
        let rows = statement.query_map([session_id.to_string()], |row| {
            let episode_id: String = row.get(0)?;
            Uuid::parse_str(&episode_id).map_err(sql_conversion_error)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn finalize_idle_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<SessionRecord>> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statement = transaction.prepare(
                "SELECT id FROM sessions WHERE state='idle' AND last_event_at <= ?1 ORDER BY last_event_at",
            )?;
            statement
                .query_map([cutoff.to_rfc3339()], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut sessions = Vec::new();
        for id in ids {
            transaction.execute(
                "UPDATE sessions SET state='finalized', ended_at=last_event_at WHERE id=?1 AND state='idle'",
                [&id],
            )?;
            enqueue_job(&transaction, "finalize_session", &id)?;
            sessions.push(session_by_id(&transaction, Uuid::parse_str(&id)?)?);
        }
        transaction.commit()?;
        Ok(sessions)
    }

    pub fn mark_finalized(
        &self,
        session_id: Uuid,
        markdown_path: &Path,
        compile_episodes: &[Uuid],
        job_id: Uuid,
        owner: &str,
    ) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE sessions SET markdown_path=?1 WHERE id=?2",
            params![markdown_path.to_string_lossy(), session_id.to_string()],
        )?;
        transaction.execute(
            "UPDATE jobs SET status='completed', owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE id=?2 AND status='running' AND owner=?3",
            params![now, job_id.to_string(), owner],
        )?;
        for episode_id in compile_episodes {
            enqueue_job(
                &transaction,
                "compile_session",
                &format!("{session_id}:{episode_id}"),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn session(&self, id: Uuid) -> Result<SessionRecord> {
        let connection = self.open()?;
        session_by_id(&connection, id)
    }

    pub fn latest_session(
        &self,
        client: &str,
        external_session_id: &str,
    ) -> Result<Option<SessionRecord>> {
        let connection = self.open()?;
        latest_session_connection(&connection, client, external_session_id)
    }

    pub fn recall_context(
        &self,
        client: &str,
        external_session_id: &str,
        project_id: Option<&str>,
    ) -> Result<Option<RecallContext>> {
        let connection = self.open()?;
        let Some(session) = latest_session_for_project_connection(
            &connection,
            client,
            external_session_id,
            project_id,
        )?
        else {
            return Ok(None);
        };
        let active_episode =
            active_episode_connection(&connection, &session.conversation_key, project_id)?;
        let Some(active_episode) = active_episode else {
            return Ok(Some(RecallContext {
                session,
                active_episode: None,
                active_corrections: Vec::new(),
                active_constraints: Vec::new(),
                conversation_root_goal: None,
            }));
        };
        let prompts = episode_prompt_texts(&connection, active_episode.id)?;
        let root_event_id =
            conversation_root_event_id(&connection, &session.conversation_key, project_id)?;
        let root_goal = root_event_id
            .as_deref()
            .map(|event_id| event_prompt_text(&connection, event_id))
            .transpose()?
            .flatten();
        Ok(Some(RecallContext {
            session,
            active_episode: Some(active_episode),
            active_corrections: prompts
                .iter()
                .filter(|(_, kind, _)| kind == "correction")
                .map(|(_, _, prompt)| prompt.clone())
                .collect(),
            active_constraints: prompts
                .iter()
                .filter(|(_, kind, _)| kind == "constraint")
                .map(|(_, _, prompt)| prompt.clone())
                .collect(),
            conversation_root_goal: root_goal,
        }))
    }

    pub fn injection_identity(
        &self,
        client: &str,
        external_session_id: &str,
        project_id: Option<&str>,
    ) -> Result<InjectionIdentity> {
        let connection = self.open()?;
        let Some(session) = latest_session_for_project_connection(
            &connection,
            client,
            external_session_id,
            project_id,
        )?
        else {
            return Ok(InjectionIdentity {
                client: client.to_owned(),
                conversation_key: conversation_key(client, external_session_id),
                generation: 0,
                episode_id: None,
            });
        };
        let episode =
            active_episode_connection(&connection, &session.conversation_key, project_id)?;
        Ok(InjectionIdentity {
            client: client.to_owned(),
            conversation_key: session.conversation_key,
            generation: session.generation,
            episode_id: episode.map(|value| value.id),
        })
    }

    pub fn create_episode(
        &self,
        session_id: Uuid,
        root_event_id: &str,
        goal: &str,
    ) -> Result<TaskEpisode> {
        if goal.trim().is_empty() {
            bail!("episode goal cannot be empty");
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = session_by_id(&transaction, session_id)?;
        let root_session: Option<String> = transaction
            .query_row(
                "SELECT session_id FROM session_events WHERE event_id=?1",
                [root_event_id],
                |row| row.get(0),
            )
            .optional()?;
        if root_session.as_deref() != Some(&session.id.to_string()) {
            bail!("episode root event does not belong to its session");
        }
        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM task_episodes WHERE root_event_id=?1",
                [root_event_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let episode = episode_by_id(&transaction, Uuid::parse_str(&id)?)?;
            transaction.commit()?;
            return Ok(episode);
        }
        let ordinal: u32 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM task_episodes WHERE conversation_key=?1 AND project_id IS ?2",
            params![session.conversation_key, session.project_id],
            |row| row.get(0),
        )?;
        let episode = TaskEpisode {
            id: Uuid::now_v7(),
            project_id: session.project_id,
            conversation_key: session.conversation_key,
            root_event_id: root_event_id.to_owned(),
            goal: goal.trim().to_owned(),
            ordinal,
            state: EpisodeState::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        insert_episode(&transaction, &episode)?;
        transaction.commit()?;
        Ok(episode)
    }

    pub fn episode(&self, id: Uuid) -> Result<TaskEpisode> {
        let connection = self.open()?;
        episode_by_id(&connection, id)
    }

    pub fn episode_optional(&self, id: Uuid) -> Result<Option<TaskEpisode>> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at FROM task_episodes WHERE id=?1",
                [id.to_string()],
                episode_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn episode_for_root_event(&self, root_event_id: &str) -> Result<Option<TaskEpisode>> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at FROM task_episodes WHERE root_event_id=?1",
                [root_event_id],
                episode_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn update_episode(&self, episode: &TaskEpisode) -> Result<TaskEpisode> {
        if episode.goal.trim().is_empty() {
            bail!("episode goal cannot be empty");
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = episode_by_id(&transaction, episode.id)?;
        if current.project_id != episode.project_id
            || current.conversation_key != episode.conversation_key
            || current.root_event_id != episode.root_event_id
            || current.ordinal != episode.ordinal
            || current.created_at != episode.created_at
        {
            bail!("episode identity cannot be changed");
        }
        transaction.execute(
            "UPDATE task_episodes SET goal=?1, ordinal=?2, state=?3, updated_at=?4 WHERE id=?5",
            params![
                episode.goal.trim(),
                episode.ordinal,
                episode_state(episode.state),
                episode.updated_at.to_rfc3339(),
                episode.id.to_string()
            ],
        )?;
        let updated = episode_by_id(&transaction, episode.id)?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn list_active_episodes(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<TaskEpisode>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at
             FROM task_episodes
             WHERE conversation_key=?1 AND state='active' AND project_id IS ?2
             ORDER BY ordinal, id",
        )?;
        let rows = statement.query_map(params![conversation_key, project_id], episode_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_active_episodes_for_session(&self, session_id: Uuid) -> Result<Vec<TaskEpisode>> {
        let session = self.session(session_id)?;
        self.list_active_episodes(&session.conversation_key, session.project_id.as_deref())
    }

    pub fn list_episodes(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<TaskEpisode>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at
             FROM task_episodes
             WHERE conversation_key=?1 AND project_id IS ?2
             ORDER BY ordinal, id",
        )?;
        let rows = statement.query_map(params![conversation_key, project_id], episode_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn create_or_update_handoff(&self, handoff: &TaskHandoff) -> Result<TaskHandoff> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let handoff = validate_handoff(&transaction, handoff)?;
        let current_id: Option<String> = transaction
            .query_row(
                "SELECT id FROM handoffs WHERE episode_id=?1 AND status IN ('active', 'ready', 'consumed')",
                [handoff.episode_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let existing = handoff_by_id_optional(&transaction, handoff.id)?;
        if let Some(current_id) = current_id {
            let current = handoff_by_id(&transaction, Uuid::parse_str(&current_id)?)?;
            if current.id == handoff.id {
                let mut next = handoff.clone();
                next.created_at = current.created_at;
                if current.status == HandoffStatus::Consumed
                    && same_handoff_content(&current, &next)
                {
                    transaction.commit()?;
                    return Ok(current);
                }
                if current == next {
                    transaction.commit()?;
                    return Ok(current);
                }
                if !is_current_handoff_status(current.status) {
                    bail!("terminal handoff cannot be updated");
                }
                store_handoff_revision(&transaction, &current)?;
                update_handoff(
                    &transaction,
                    &next,
                    current_revision(&transaction, current.id)? + 1,
                )?;
                replace_handoff_evidence(&transaction, &next)?;
                transaction.commit()?;
                return Ok(next);
            }
            if existing.is_some() {
                bail!("handoff id is already used by another snapshot");
            }
            store_handoff_revision(&transaction, &current)?;
            update_handoff_status(
                &transaction,
                current.id,
                HandoffStatus::Superseded,
                current_revision(&transaction, current.id)? + 1,
            )?;
        } else if let Some(existing) = existing {
            if existing == handoff {
                transaction.commit()?;
                return Ok(existing);
            }
            bail!("terminal handoff cannot be updated");
        }
        insert_handoff(&transaction, &handoff)?;
        replace_handoff_evidence(&transaction, &handoff)?;
        transaction.commit()?;
        Ok(handoff)
    }

    pub fn handoff(&self, id: Uuid) -> Result<TaskHandoff> {
        let connection = self.open()?;
        handoff_by_id(&connection, id)
    }

    pub fn handoff_detail(&self, id: Uuid) -> Result<Option<HandoffDetail>> {
        let Some(handoff) = self.handoff_optional(id)? else {
            return Ok(None);
        };
        Ok(Some(HandoffDetail {
            versions: self
                .handoff_versions(id)?
                .into_iter()
                .take(MAX_HANDOFF_VERSIONS)
                .collect(),
            evidence: self
                .handoff_evidence_records(id)?
                .into_iter()
                .take(MAX_HANDOFF_SOURCE_EVENTS)
                .collect(),
            handoff,
        }))
    }

    pub fn handoff_optional(&self, id: Uuid) -> Result<Option<TaskHandoff>> {
        let connection = self.open()?;
        handoff_by_id_optional(&connection, id)
    }

    pub fn handoff_for_episode(&self, episode_id: Uuid) -> Result<Option<TaskHandoff>> {
        let connection = self.open()?;
        connection
            .query_row(
                CURRENT_HANDOFF_BY_EPISODE_SELECT,
                [episode_id.to_string()],
                handoff_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_handoffs(
        &self,
        project_id: Option<&str>,
        status: Option<HandoffStatus>,
        limit: usize,
    ) -> Result<Vec<TaskHandoff>> {
        validate_handoff_limit(limit)?;
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at
             FROM handoffs
             WHERE project_id IS ?1 AND (?2 IS NULL OR status=?2)
             ORDER BY updated_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                project_id,
                status.map(handoff_status),
                i64::try_from(limit)?
            ],
            handoff_from_row,
        )?;
        rows.take(limit)
            .map(|row| row.map_err(Into::into))
            .collect()
    }

    pub fn all_handoffs(
        &self,
        status: Option<HandoffStatus>,
        limit: usize,
    ) -> Result<Vec<TaskHandoff>> {
        validate_handoff_limit(limit)?;
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at
             FROM handoffs
             WHERE (?1 IS NULL OR status=?1)
             ORDER BY updated_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![status.map(handoff_status), i64::try_from(limit)?],
            handoff_from_row,
        )?;
        rows.take(limit)
            .map(|row| row.map_err(Into::into))
            .collect()
    }

    pub fn project_handoffs(
        &self,
        project_id: &str,
        status: Option<HandoffStatus>,
        limit: usize,
    ) -> Result<Vec<TaskHandoff>> {
        if project_id.trim().is_empty() || project_id.contains('\0') {
            bail!("project id cannot be empty or contain NUL");
        }
        self.list_handoffs(Some(project_id), status, limit)
    }

    pub fn session_handoffs(
        &self,
        session_id: Uuid,
        status: Option<HandoffStatus>,
        limit: usize,
    ) -> Result<Vec<TaskHandoff>> {
        validate_handoff_limit(limit)?;
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at
             FROM handoffs
             WHERE (source_session_id=?1 OR EXISTS (SELECT 1 FROM handoff_evidence e WHERE e.handoff_id=handoffs.id AND e.source_session_id=?1))
               AND (?2 IS NULL OR status=?2)
             ORDER BY updated_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                session_id.to_string(),
                status.map(handoff_status),
                i64::try_from(limit)?
            ],
            handoff_from_row,
        )?;
        rows.take(limit)
            .map(|row| row.map_err(Into::into))
            .collect()
    }

    pub fn newest_handoff_candidates(
        &self,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskHandoff>> {
        let connection = self.open()?;
        list_handoffs_for_candidates(&connection, project_id, None, limit)
    }

    pub fn newest_handoff_candidates_for_conversation(
        &self,
        project_id: Option<&str>,
        conversation_key: &str,
        limit: usize,
    ) -> Result<Vec<TaskHandoff>> {
        if conversation_key.trim().is_empty() {
            bail!("conversation key cannot be empty");
        }
        let connection = self.open()?;
        list_handoffs_for_candidates(&connection, project_id, Some(conversation_key), limit)
    }

    pub fn handoff_versions(&self, id: Uuid) -> Result<Vec<HandoffVersion>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT handoff_id, revision, status, snapshot_json, created_at FROM handoff_versions WHERE handoff_id=?1 ORDER BY revision DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![id.to_string(), MAX_HANDOFF_VERSIONS], |row| {
            let handoff_id: String = row.get(0)?;
            let status: String = row.get(2)?;
            let snapshot: String = row.get(3)?;
            let created_at: String = row.get(4)?;
            Ok(HandoffVersion {
                handoff_id: Uuid::parse_str(&handoff_id).map_err(sql_conversion_error)?,
                revision: row.get(1)?,
                status: parse_handoff_status(&status).map_err(sql_conversion_error)?,
                snapshot: serde_json::from_str(&snapshot).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                created_at: parse_timestamp(&created_at).map_err(sql_conversion_error)?,
            })
        })?;
        let mut versions = rows
            .map(|row| row.map_err(Into::into))
            .collect::<Result<Vec<_>>>()?;
        versions.reverse();
        Ok(versions)
    }

    pub fn handoff_evidence(&self, id: Uuid) -> Result<Vec<String>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT event_id FROM handoff_evidence WHERE handoff_id=?1 ORDER BY ordinal, event_id LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![id.to_string(), MAX_HANDOFF_SOURCE_EVENTS], |row| {
                row.get(0)
            })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn handoff_evidence_records(&self, id: Uuid) -> Result<Vec<HandoffEvidence>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT handoff_id, source_session_id, event_id, ordinal FROM handoff_evidence WHERE handoff_id=?1 ORDER BY ordinal, event_id LIMIT ?2",
        )?;
        let rows =
            statement.query_map(params![id.to_string(), MAX_HANDOFF_SOURCE_EVENTS], |row| {
                let handoff_id: String = row.get(0)?;
                let source_session_id: String = row.get(1)?;
                Ok(HandoffEvidence {
                    handoff_id: Uuid::parse_str(&handoff_id).map_err(sql_conversion_error)?,
                    source_session_id: Uuid::parse_str(&source_session_id)
                        .map_err(sql_conversion_error)?,
                    event_id: row.get(2)?,
                    ordinal: row.get(3)?,
                })
            })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn mark_handoff_dirty(
        &self,
        episode_id: Uuid,
        debounce: Duration,
    ) -> Result<CheckpointState> {
        self.mark_handoff_dirty_at(episode_id, debounce, Utc::now())
    }

    pub fn mark_dirty(&self, episode_id: Uuid, debounce: Duration) -> Result<CheckpointState> {
        self.mark_handoff_dirty(episode_id, debounce)
    }

    pub fn mark_handoff_dirty_at(
        &self,
        episode_id: Uuid,
        debounce: Duration,
        now: DateTime<Utc>,
    ) -> Result<CheckpointState> {
        let debounce_seconds = i64::try_from(debounce.as_secs())?;
        if debounce.subsec_nanos() != 0 || debounce_seconds > MAX_CHECKPOINT_DEBOUNCE_SECONDS {
            bail!("checkpoint debounce exceeds the supported bound");
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        episode_by_id(&transaction, episode_id)?;
        let now_text = now.to_rfc3339();
        let debounce_until = (now + chrono::Duration::seconds(debounce_seconds)).to_rfc3339();
        transaction.execute(
            "INSERT INTO checkpoint_state(episode_id, dirty, debounce_until, last_checkpoint_at, revision, updated_at)
             VALUES (?1, 1, ?2, NULL, 1, ?3)
             ON CONFLICT(episode_id) DO UPDATE SET dirty=1, debounce_until=excluded.debounce_until, revision=checkpoint_state.revision + 1, updated_at=excluded.updated_at",
            params![episode_id.to_string(), debounce_until, now_text],
        )?;
        enqueue_checkpoint_job(&transaction, episode_id, &debounce_until, &now_text)?;
        let state = checkpoint_state_connection(&transaction, episode_id)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn checkpoint_state(&self, episode_id: Uuid) -> Result<CheckpointState> {
        let connection = self.open()?;
        checkpoint_state_connection(&connection, episode_id)
    }

    pub fn complete_checkpoint(&self, episode_id: Uuid) -> Result<CheckpointState> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        episode_by_id(&transaction, episode_id)?;
        let current = checkpoint_state_connection(&transaction, episode_id)?;
        if !current.dirty {
            transaction.commit()?;
            return Ok(current);
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE checkpoint_state SET dirty=0, debounce_until=NULL, last_checkpoint_at=?1, revision=revision + 1, updated_at=?1 WHERE episode_id=?2 AND revision=?3 AND updated_at=?4",
            params![now, episode_id.to_string(), current.revision, current.updated_at.to_rfc3339()],
        )?;
        let state = checkpoint_state_connection(&transaction, episode_id)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn complete_checkpoint_if_unchanged(
        &self,
        episode_id: Uuid,
        observed_updated_at: DateTime<Utc>,
        observed_revision: u32,
    ) -> Result<CheckpointState> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        episode_by_id(&transaction, episode_id)?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE checkpoint_state SET dirty=0, debounce_until=NULL, last_checkpoint_at=?1, revision=revision + 1, updated_at=?1 WHERE episode_id=?2 AND dirty=1 AND revision=?3 AND updated_at=?4",
            params![
                now,
                episode_id.to_string(),
                observed_revision,
                observed_updated_at.to_rfc3339()
            ],
        )?;
        let state = checkpoint_state_connection(&transaction, episode_id)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn requeue_checkpoint_job(&self, episode_id: Uuid) -> Result<()> {
        let connection = self.open()?;
        let now = Utc::now().to_rfc3339();
        connection.execute(
            "UPDATE jobs SET status='pending', next_retry_at=?1, last_error=NULL, provider=NULL, owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE job_type='checkpoint_handoff' AND dedupe_key=?2 AND status IN ('completed', 'failed')",
            params![now, episode_id.to_string()],
        )?;
        Ok(())
    }

    pub fn prepare_checkpoint_flush(&self) -> Result<()> {
        let connection = self.open()?;
        let now = Utc::now().to_rfc3339();
        connection.execute(
            "UPDATE jobs SET status='pending', next_retry_at=?1, owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE job_type='checkpoint_handoff' AND status IN ('pending', 'running')",
            [&now],
        )?;
        connection.execute(
            "UPDATE jobs SET status='pending', attempt_count=0, next_retry_at=?1, last_error=NULL, provider=NULL, owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE job_type='checkpoint_handoff' AND status IN ('completed', 'failed') AND EXISTS (SELECT 1 FROM checkpoint_state WHERE dirty=1 AND episode_id=jobs.dedupe_key)",
            [&now],
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at) SELECT lower(hex(randomblob(16))), 'checkpoint_handoff', episode_id, 'pending', json_object('episode_id', episode_id), ?1, ?1, ?1 FROM checkpoint_state WHERE dirty=1",
            [&now],
        )?;
        Ok(())
    }

    pub fn consume_handoff(&self, id: Uuid) -> Result<TaskHandoff> {
        self.update_handoff_lifecycle(id, HandoffStatus::Consumed)
    }

    pub fn complete_handoff(&self, id: Uuid) -> Result<TaskHandoff> {
        self.update_handoff_lifecycle(id, HandoffStatus::Completed)
    }

    pub fn stale_handoff(&self, id: Uuid) -> Result<TaskHandoff> {
        self.update_handoff_lifecycle(id, HandoffStatus::Stale)
    }

    pub fn supersede_handoff(&self, id: Uuid) -> Result<TaskHandoff> {
        self.update_handoff_lifecycle(id, HandoffStatus::Superseded)
    }

    fn update_handoff_lifecycle(
        &self,
        id: Uuid,
        next_status: HandoffStatus,
    ) -> Result<TaskHandoff> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = handoff_by_id(&transaction, id)?;
        if current.status == next_status
            || !is_current_handoff_status(current.status)
            || (next_status == HandoffStatus::Consumed
                && current.status != HandoffStatus::Active
                && current.status != HandoffStatus::Ready)
        {
            transaction.commit()?;
            return Ok(current);
        }
        store_handoff_revision(&transaction, &current)?;
        update_handoff_status(
            &transaction,
            id,
            next_status,
            current_revision(&transaction, id)? + 1,
        )?;
        let updated = handoff_by_id(&transaction, id)?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn record_prompt_intent(&self, intent: &PromptIntent) -> Result<bool> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation_key = validate_intent(&transaction, intent)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO prompt_intents(event_id, episode_id, conversation_key, kind, confidence, weight, classifier_version, source, classified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                intent.event_id,
                intent.episode_id.to_string(),
                conversation_key,
                prompt_intent_kind(intent.kind),
                intent.confidence,
                intent.weight,
                intent.classifier_version,
                classification_source(intent.source),
                intent.classified_at.to_rfc3339(),
            ],
        )?;
        transaction.commit()?;
        Ok(inserted == 1)
    }

    pub fn prompt_intent(&self, event_id: &str) -> Result<PromptIntent> {
        let connection = self.open()?;
        prompt_intent_by_event(&connection, event_id)
    }

    pub fn list_prompt_intents(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<PromptIntent>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT i.event_id, i.episode_id, i.kind, i.confidence, i.weight, i.classifier_version, i.source, i.classified_at
             FROM prompt_intents i
             JOIN task_episodes e ON e.id=i.episode_id
             WHERE i.conversation_key=?1 AND e.project_id IS ?2
             ORDER BY i.classified_at, i.event_id",
        )?;
        let rows = statement.query_map(
            params![conversation_key, project_id],
            prompt_intent_from_row,
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn review_prompt_intent(&self, intent: &PromptIntent) -> Result<bool> {
        if intent.source != IntentClassificationSource::ProviderReview {
            bail!("prompt intent review must use provider-review source");
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation_key = validate_intent(&transaction, intent)?;
        let current = prompt_intent_by_event(&transaction, &intent.event_id)?;
        if same_classification(&current, intent) {
            transaction.commit()?;
            return Ok(false);
        }
        let revision: u32 = transaction.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM prompt_intent_history WHERE event_id=?1",
            [&intent.event_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO prompt_intent_history(event_id, revision, conversation_key, episode_id, kind, confidence, weight, classifier_version, source, classified_at, replaced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                current.event_id,
                revision,
                current_conversation_key(&transaction, &current.event_id)?,
                current.episode_id.to_string(),
                prompt_intent_kind(current.kind),
                current.confidence,
                current.weight,
                current.classifier_version,
                classification_source(current.source),
                current.classified_at.to_rfc3339(),
                intent.classified_at.to_rfc3339(),
            ],
        )?;
        transaction.execute(
            "UPDATE prompt_intents SET episode_id=?1, conversation_key=?2, kind=?3, confidence=?4, weight=?5, classifier_version=?6, source=?7, classified_at=?8 WHERE event_id=?9",
            params![
                intent.episode_id.to_string(),
                conversation_key,
                prompt_intent_kind(intent.kind),
                intent.confidence,
                intent.weight,
                intent.classifier_version,
                classification_source(intent.source),
                intent.classified_at.to_rfc3339(),
                intent.event_id,
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn prompt_intent_history(&self, event_id: &str) -> Result<Vec<PromptIntentHistory>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT event_id, revision, conversation_key, episode_id, kind, confidence, weight, classifier_version, source, classified_at, replaced_at
             FROM prompt_intent_history WHERE event_id=?1 ORDER BY revision",
        )?;
        let rows = statement.query_map([event_id], history_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn jobs(&self) -> Result<Vec<JobRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key, owner, lease_started_at, lease_until FROM jobs ORDER BY created_at",
        )?;
        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let next_retry_at: String = row.get(4)?;
            Ok((
                id,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                next_retry_at,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                job_type,
                status,
                attempt_count,
                next_retry_at,
                last_error,
                dedupe_key,
                owner,
                lease_started_at,
                lease_until,
            ) = row?;
            Ok(JobRecord {
                id: Uuid::parse_str(&id)?,
                job_type,
                status,
                attempt_count,
                next_retry_at: DateTime::parse_from_rfc3339(&next_retry_at)?.with_timezone(&Utc),
                last_error,
                dedupe_key,
                owner,
                lease_started_at: parse_optional_timestamp(lease_started_at)?,
                lease_until: parse_optional_timestamp(lease_until)?,
            })
        })
        .collect()
    }

    pub fn health(&self) -> Result<String> {
        let connection = self.open()?;
        let sessions: u64 =
            connection.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        let jobs: u64 = connection.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))?;
        Ok(format!("{sessions} sessions, {jobs} jobs"))
    }

    pub fn backup(&self, destination: &Path) -> Result<()> {
        let source = self.open()?;
        let mut destination = Connection::open(destination)?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
        backup.run_to_completion(128, Duration::from_millis(10), None)?;
        Ok(())
    }

    pub fn claim_job(&self, owner: &str, lease_timeout_seconds: u64) -> Result<Option<JobRecord>> {
        self.claim_job_at(owner, lease_timeout_seconds, Utc::now())
    }

    pub fn claim_job_at(
        &self,
        owner: &str,
        lease_timeout_seconds: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<JobRecord>> {
        self.claim_job_at_with_type(owner, lease_timeout_seconds, now, None)
    }

    pub fn claim_job_of_type(
        &self,
        owner: &str,
        lease_timeout_seconds: u64,
        job_type: &str,
    ) -> Result<Option<JobRecord>> {
        self.claim_job_at_with_type(owner, lease_timeout_seconds, Utc::now(), Some(job_type))
    }

    fn claim_job_at_with_type(
        &self,
        owner: &str,
        lease_timeout_seconds: u64,
        now: DateTime<Utc>,
        job_type: Option<&str>,
    ) -> Result<Option<JobRecord>> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now_text = now.to_rfc3339();
        let lease_until = now + chrono::Duration::seconds(i64::try_from(lease_timeout_seconds)?);
        transaction.execute(
            "UPDATE jobs SET status='pending', owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE status='running' AND (lease_until IS NULL OR lease_until <= ?1)",
            [&now_text],
        )?;
        let id: Option<String> = if let Some(job_type) = job_type {
            transaction
                .query_row(
                    "SELECT id FROM jobs WHERE status='pending' AND job_type=?2 AND next_retry_at <= ?1 ORDER BY created_at LIMIT 1",
                    params![now_text, job_type],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            transaction
                .query_row(
                    "SELECT id FROM jobs WHERE status='pending' AND next_retry_at <= ?1 ORDER BY CASE WHEN job_type='finalize_session' THEN 0 ELSE 1 END, created_at LIMIT 1",
                    [&now_text],
                    |row| row.get(0),
                )
                .optional()?
        };
        let Some(id) = id else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute(
            "UPDATE jobs SET status='running', owner=?1, lease_started_at=?2, lease_until=?3, attempt_count=attempt_count+1, updated_at=?2 WHERE id=?4 AND status='pending'",
            params![owner, now_text, lease_until.to_rfc3339(), id],
        )?;
        let job = transaction.query_row(
            "SELECT id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key, owner, lease_started_at, lease_until FROM jobs WHERE id=?1",
            [&id],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            )),
        )?;
        transaction.commit()?;
        Ok(Some(JobRecord {
            id: Uuid::parse_str(&job.0)?,
            job_type: job.1,
            status: job.2,
            attempt_count: job.3,
            next_retry_at: DateTime::parse_from_rfc3339(&job.4)?.with_timezone(&Utc),
            last_error: job.5,
            dedupe_key: job.6,
            owner: job.7,
            lease_started_at: parse_optional_timestamp(job.8)?,
            lease_until: parse_optional_timestamp(job.9)?,
        }))
    }

    pub fn finish_job(
        &self,
        id: Uuid,
        owner: &str,
        provider: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let connection = self.open()?;
        let job: Option<u32> = connection
            .query_row(
                "SELECT attempt_count FROM jobs WHERE id=?1 AND status='running' AND owner=?2",
                params![id.to_string(), owner],
                |row| row.get(0),
            )
            .optional()?;
        let Some(job) = job else {
            return Ok(());
        };
        let (status, next_retry_at) = if error.is_none() {
            ("completed", Utc::now())
        } else if job < 5 {
            let delay = 2_i64.pow(job.min(10));
            ("pending", Utc::now() + chrono::Duration::seconds(delay))
        } else {
            ("failed", Utc::now())
        };
        connection.execute(
            "UPDATE jobs SET status=?1, next_retry_at=?2, last_error=?3, provider=?4, owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?5 WHERE id=?6 AND status='running' AND owner=?7",
            params![status, next_retry_at.to_rfc3339(), error, provider, Utc::now().to_rfc3339(), id.to_string(), owner],
        )?;
        Ok(())
    }

    pub fn integrations(&self) -> Result<Vec<IntegrationRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT client, connected, mcp_registered, hook_status, last_event_at FROM integration_state ORDER BY client")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (client, connected, mcp_registered, hook_status, last_event_at) = row?;
            Ok(IntegrationRecord {
                client,
                connected,
                mcp_registered,
                hook_status,
                last_event_at: last_event_at
                    .map(|value| {
                        DateTime::parse_from_rfc3339(&value).map(|value| value.with_timezone(&Utc))
                    })
                    .transpose()?,
            })
        })
        .collect()
    }

    pub fn orphans(&self) -> Result<Vec<OrphanRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT client, external_session_id, payload_json FROM orphan_sessions ORDER BY created_at")?;
        let rows = statement.query_map([], |row| {
            Ok(OrphanRecord {
                client: row.get(0)?,
                external_session_id: row.get(1)?,
                payload_json: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn clear_orphan(&self, client: &str, external_session_id: &str) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM orphan_sessions WHERE client=?1 AND external_session_id=?2",
            params![client, external_session_id],
        )?;
        transaction.execute(
            "DELETE FROM imports WHERE client=?1 AND external_session_id=?2 AND status='orphan'",
            params![client, external_session_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_injection(&self, identity: &InjectionIdentity, memory_id: Uuid) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute(
            "INSERT OR IGNORE INTO session_injections(client, conversation_key, generation, episode_id, memory_id, injected_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                identity.client,
                identity.conversation_key,
                identity.generation,
                identity.episode_id.map_or_else(String::new, |value| value.to_string()),
                memory_id.to_string(),
                Utc::now().to_rfc3339()
            ],
        )? == 1)
    }

    pub fn claim_briefing(&self, identity: &InjectionIdentity) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute(
            "INSERT OR IGNORE INTO briefing_deliveries(client, conversation_key, generation, episode_id, delivered_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                identity.client,
                identity.conversation_key,
                identity.generation,
                identity.episode_id.map_or_else(String::new, |value| value.to_string()),
                Utc::now().to_rfc3339()
            ],
        )? == 1)
    }

    pub fn briefing_was_delivered(&self, identity: &InjectionIdentity) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM briefing_deliveries WHERE client=?1 AND conversation_key=?2 AND generation=?3 AND episode_id=?4",
                params![
                    identity.client,
                    identity.conversation_key,
                    identity.generation,
                    identity.episode_id.map_or_else(String::new, |value| value.to_string())
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn deliver_handoff(&self, identity: &InjectionIdentity, handoff_id: Uuid) -> Result<bool> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = handoff_by_id(&transaction, handoff_id)?;
        if !is_current_handoff_status(current.status) {
            transaction.commit()?;
            return Ok(false);
        }
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO handoff_deliveries(client, conversation_key, generation, handoff_id, delivery_kind, delivered_at) VALUES (?1, ?2, ?3, ?4, 'full', ?5)",
            params![
                identity.client,
                identity.conversation_key,
                identity.generation,
                handoff_id.to_string(),
                Utc::now().to_rfc3339()
            ],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        if current.status != HandoffStatus::Consumed {
            store_handoff_revision(&transaction, &current)?;
            update_handoff_status(
                &transaction,
                handoff_id,
                HandoffStatus::Consumed,
                current_revision(&transaction, handoff_id)? + 1,
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn claim_handoff_delivery(
        &self,
        identity: &InjectionIdentity,
        handoff_id: Uuid,
        delivery_kind: &str,
    ) -> Result<bool> {
        if !matches!(delivery_kind, "full" | "card") {
            bail!("unsupported handoff delivery kind: {delivery_kind}");
        }
        let connection = self.open()?;
        Ok(connection.execute(
            "INSERT OR IGNORE INTO handoff_deliveries(client, conversation_key, generation, handoff_id, delivery_kind, delivered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                identity.client,
                identity.conversation_key,
                identity.generation,
                handoff_id.to_string(),
                delivery_kind,
                Utc::now().to_rfc3339()
            ],
        )? == 1)
    }

    pub fn set_integration_connected(&self, client: &str, connected: bool) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "INSERT INTO integration_state(client, connected, mcp_registered, hook_status, details_json) VALUES (?1, ?2, ?2, ?3, '{}') ON CONFLICT(client) DO UPDATE SET connected=excluded.connected, mcp_registered=excluded.mcp_registered, hook_status=excluded.hook_status",
            params![client, connected, if connected { "installed" } else { "removed" }],
        )?;
        Ok(())
    }

    pub fn record_procedure_application(
        &self,
        memory_id: Uuid,
        source_session: Uuid,
        success: bool,
    ) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute(
            "INSERT OR IGNORE INTO procedure_applications(memory_id, source_session, signal, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                memory_id.to_string(),
                source_session.to_string(),
                if success { "success" } else { "failure" },
                Utc::now().to_rfc3339()
            ],
        )? == 1)
    }

    pub fn record_access(&self, memory_id: Uuid, signal: ReinforcementSignal) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "INSERT INTO access_events(id, memory_id, signal, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                Uuid::now_v7().to_string(),
                memory_id.to_string(),
                signal.as_str(),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn meaningful_access(&self, memory_id: Uuid) -> Result<(u64, Option<DateTime<Utc>>)> {
        let connection = self.open()?;
        let (count, latest): (u64, Option<String>) = connection.query_row(
            "SELECT COUNT(*), MAX(created_at) FROM access_events WHERE memory_id=?1 AND signal IN ('explicitly_read', 'successfully_applied')",
            [memory_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((
            count,
            latest
                .map(|value| {
                    DateTime::parse_from_rfc3339(&value).map(|value| value.with_timezone(&Utc))
                })
                .transpose()?,
        ))
    }

    pub fn import_exists(&self, client: &str, external_session_id: &str) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM imports WHERE client=?1 AND external_session_id=?2",
                params![client, external_session_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn requeue_import_compilation(
        &self,
        client: &str,
        external_session_id: &str,
    ) -> Result<()> {
        let connection = self.open()?;
        let session_id: Option<String> = connection
            .query_row(
                "SELECT id FROM sessions WHERE client=?1 AND external_session_id=?2 AND markdown_path IS NOT NULL ORDER BY generation DESC LIMIT 1",
                params![client, external_session_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        connection.execute(
            "UPDATE jobs SET status='pending', attempt_count=0, next_retry_at=?1, last_error=NULL, provider=NULL, updated_at=?1 WHERE job_type='compile_session' AND (dedupe_key=?2 OR EXISTS (SELECT 1 FROM event_episode_links l JOIN session_events e ON e.event_id=l.event_id JOIN sessions s ON s.id=e.session_id WHERE s.client=?3 AND s.external_session_id=?4 AND (jobs.dedupe_key=l.episode_id OR jobs.dedupe_key LIKE '%:' || l.episode_id)))",
            params![now, session_id, client, external_session_id],
        )?;
        Ok(())
    }

    pub fn record_import(
        &self,
        client: &str,
        external_session_id: &str,
        status: &str,
        orphan_payload: Option<&str>,
    ) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "INSERT OR IGNORE INTO imports(id, client, external_session_id, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![Uuid::now_v7().to_string(), client, external_session_id, status, Utc::now().to_rfc3339()],
        )?;
        if let Some(payload) = orphan_payload {
            connection.execute(
                "INSERT OR REPLACE INTO orphan_sessions(client, external_session_id, payload_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![client, external_session_id, payload, Utc::now().to_rfc3339()],
            )?;
        }
        Ok(())
    }

    pub fn mark_latest_session_imported(
        &self,
        client: &str,
        external_session_id: &str,
    ) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "UPDATE sessions SET imported=1 WHERE id=(SELECT id FROM sessions WHERE client=?1 AND external_session_id=?2 ORDER BY generation DESC LIMIT 1)",
            params![client, external_session_id],
        )?;
        Ok(())
    }

    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(connection)
    }
}

const CURRENT_HANDOFF_BY_EPISODE_SELECT: &str = "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at FROM handoffs WHERE episode_id=?1 AND status IN ('active', 'ready', 'consumed')";

fn list_handoffs_for_candidates(
    connection: &Connection,
    project_id: Option<&str>,
    conversation_key: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskHandoff>> {
    validate_handoff_limit(limit)?;
    let limit = i64::try_from(limit)?;
    let mut statement = if conversation_key.is_some() {
        connection.prepare(
            "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at FROM handoffs WHERE project_id IS ?1 AND conversation_key=?2 AND status IN ('active', 'ready', 'consumed') ORDER BY updated_at DESC, id DESC LIMIT ?3",
        )?
    } else {
        connection.prepare(
            "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at FROM handoffs WHERE project_id IS ?1 AND status IN ('active', 'ready', 'consumed') ORDER BY updated_at DESC, id DESC LIMIT ?2",
        )?
    };
    let rows = if let Some(conversation_key) = conversation_key {
        statement.query_map(
            params![project_id, conversation_key, limit],
            handoff_from_row,
        )?
    } else {
        statement.query_map(params![project_id, limit], handoff_from_row)?
    };
    rows.map(|row| row.map_err(Into::into)).collect()
}

fn handoff_by_id(connection: &Connection, id: Uuid) -> Result<TaskHandoff> {
    handoff_by_id_optional(connection, id)?
        .ok_or_else(|| anyhow::anyhow!("handoff not found: {id}"))
}

fn handoff_by_id_optional(connection: &Connection, id: Uuid) -> Result<Option<TaskHandoff>> {
    connection
        .query_row(
            "SELECT id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, created_at, updated_at FROM handoffs WHERE id=?1",
            [id.to_string()],
            handoff_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn handoff_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskHandoff> {
    let parse_json = |index: usize| -> rusqlite::Result<String> { row.get(index) };
    let id: String = row.get(0)?;
    let episode_id: String = row.get(3)?;
    let source_session_id: String = row.get(4)?;
    let status: String = row.get(6)?;
    let created_at: String = row.get(20)?;
    let updated_at: String = row.get(21)?;
    Ok(TaskHandoff {
        id: Uuid::parse_str(&id).map_err(sql_conversion_error)?,
        project_id: row.get(1)?,
        conversation_key: row.get(2)?,
        episode_id: Uuid::parse_str(&episode_id).map_err(sql_conversion_error)?,
        source_session_id: Uuid::parse_str(&source_session_id).map_err(sql_conversion_error)?,
        source_client: row.get(5)?,
        status: parse_handoff_status(&status).map_err(sql_conversion_error)?,
        goal: row.get(7)?,
        current_state: row.get(8)?,
        completed_work: parse_json_array(&parse_json(9)?).map_err(sql_conversion_error)?,
        pending_work: parse_json_array(&parse_json(10)?).map_err(sql_conversion_error)?,
        next_action: row.get(11)?,
        blockers: parse_json_array(&parse_json(12)?).map_err(sql_conversion_error)?,
        changed_files: parse_json_array(&parse_json(13)?).map_err(sql_conversion_error)?,
        decisions: parse_json_array(&parse_json(14)?).map_err(sql_conversion_error)?,
        validation: serde_json::from_str(&parse_json(15)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                15,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        relevant_memory_ids: parse_json_array(&parse_json(16)?)
            .map_err(sql_conversion_error)?
            .into_iter()
            .map(|value| Uuid::parse_str(&value).map_err(sql_conversion_error))
            .collect::<rusqlite::Result<Vec<_>>>()?,
        source_event_ids: parse_json_array(&parse_json(17)?).map_err(sql_conversion_error)?,
        git_head: row.get(18)?,
        worktree_state_hash: row.get(19)?,
        created_at: parse_timestamp(&created_at).map_err(sql_conversion_error)?,
        updated_at: parse_timestamp(&updated_at).map_err(sql_conversion_error)?,
    })
}

fn parse_json_array(value: &str) -> std::io::Result<Vec<String>> {
    serde_json::from_str(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

fn validate_handoff_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_HANDOFF_LIST_LIMIT {
        bail!("handoff list limit must be between 1 and {MAX_HANDOFF_LIST_LIMIT}");
    }
    Ok(())
}

fn validate_handoff(transaction: &Transaction<'_>, input: &TaskHandoff) -> Result<TaskHandoff> {
    if !matches!(input.status, HandoffStatus::Active | HandoffStatus::Ready) {
        bail!("handoff creation and update require active or ready status");
    }
    let mut handoff = input.clone();
    handoff.project_id = handoff
        .project_id
        .map(|value| bounded_identifier(&value, "project id", MAX_HANDOFF_ITEM_BYTES))
        .transpose()?;
    handoff.conversation_key = bounded_identifier(
        &handoff.conversation_key,
        "conversation key",
        MAX_HANDOFF_ITEM_BYTES,
    )?;
    handoff.source_client = bounded_identifier(
        &handoff.source_client,
        "source client",
        MAX_HANDOFF_ITEM_BYTES,
    )?;
    handoff.goal = bounded_text(&handoff.goal, "goal", MAX_HANDOFF_GOAL_BYTES)?;
    handoff.current_state = bounded_text(
        &handoff.current_state,
        "current state",
        MAX_HANDOFF_TEXT_BYTES,
    )?;
    handoff.next_action = handoff
        .next_action
        .as_deref()
        .map(|value| bounded_text(value, "next action", MAX_HANDOFF_TEXT_BYTES))
        .transpose()?;
    handoff.git_head = handoff
        .git_head
        .as_deref()
        .map(|value| bounded_identifier(value, "git head", MAX_HANDOFF_ITEM_BYTES))
        .transpose()?;
    handoff.worktree_state_hash = handoff
        .worktree_state_hash
        .as_deref()
        .map(|value| bounded_identifier(value, "worktree state hash", MAX_HANDOFF_ITEM_BYTES))
        .transpose()?;
    handoff.completed_work = bounded_list(handoff.completed_work, "completed work")?;
    handoff.pending_work = bounded_list(handoff.pending_work, "pending work")?;
    handoff.blockers = bounded_list(handoff.blockers, "blockers")?;
    handoff.changed_files = bounded_list_with_limit(
        handoff.changed_files,
        "changed files",
        MAX_HANDOFF_CHANGED_FILES,
    )?;
    handoff.decisions = bounded_list(handoff.decisions, "decisions")?;
    if handoff.validation.len() > MAX_HANDOFF_VALIDATIONS {
        bail!("validation list exceeds {MAX_HANDOFF_VALIDATIONS} items");
    }
    for validation in &mut handoff.validation {
        validation.event_id = bounded_identifier(
            &validation.event_id,
            "validation event id",
            MAX_HANDOFF_ITEM_BYTES,
        )?;
        validation.command = validation
            .command
            .as_deref()
            .map(|value| bounded_text(value, "validation command", MAX_HANDOFF_ITEM_BYTES))
            .transpose()?;
        validation.summary = bounded_text(
            &validation.summary,
            "validation summary",
            MAX_HANDOFF_TEXT_BYTES,
        )?;
    }
    if handoff.relevant_memory_ids.len() > MAX_HANDOFF_MEMORY_IDS {
        bail!("relevant memory list exceeds {MAX_HANDOFF_MEMORY_IDS} items");
    }
    if handoff.source_event_ids.is_empty() {
        bail!("handoff must reference at least one source event");
    }
    if handoff.source_event_ids.len() > MAX_HANDOFF_SOURCE_EVENTS {
        bail!("source event list exceeds {MAX_HANDOFF_SOURCE_EVENTS} items");
    }
    for event_id in &mut handoff.source_event_ids {
        *event_id = bounded_identifier(event_id, "source event id", MAX_HANDOFF_ITEM_BYTES)?;
    }
    if handoff
        .source_event_ids
        .iter()
        .enumerate()
        .any(|(index, value)| handoff.source_event_ids[index + 1..].contains(value))
    {
        bail!("source event references must be unique");
    }
    if serde_json::to_vec(&handoff)?.len() > MAX_HANDOFF_TOTAL_BYTES {
        bail!("handoff exceeds {MAX_HANDOFF_TOTAL_BYTES} bytes");
    }
    validate_handoff_relationships(transaction, &handoff)?;
    Ok(handoff)
}

fn bounded_list(values: Vec<String>, name: &str) -> Result<Vec<String>> {
    bounded_list_with_limit(values, name, MAX_HANDOFF_LIST_ITEMS)
}

fn bounded_list_with_limit(values: Vec<String>, name: &str, limit: usize) -> Result<Vec<String>> {
    if values.len() > limit {
        bail!("{name} list exceeds {limit} items");
    }
    values
        .into_iter()
        .map(|value| bounded_text(&value, name, MAX_HANDOFF_ITEM_BYTES))
        .collect()
}

fn bounded_identifier(value: &str, name: &str, limit: usize) -> Result<String> {
    let value = sanitize_text(value);
    if value.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    if value.len() > limit {
        bail!("{name} exceeds {limit} bytes");
    }
    Ok(value)
}

fn bounded_text(value: &str, name: &str, limit: usize) -> Result<String> {
    let value = sanitize_text(value);
    if value.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    if value.len() > limit {
        bail!("{name} exceeds {limit} bytes");
    }
    if contains_unbounded_evidence(&value) {
        bail!("{name} contains a diff or tool dump");
    }
    Ok(value)
}

fn sanitize_text(value: &str) -> String {
    let normalized: String = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect();
    normalized
        .lines()
        .map(|line| {
            let lowercase = line.to_ascii_lowercase();
            [
                "authorization:",
                "api_key=",
                "api-key=",
                "access_token=",
                "access-token=",
                "password=",
                "secret=",
            ]
            .iter()
            .find_map(|marker| {
                lowercase
                    .find(marker)
                    .map(|index| format!("{}[REDACTED]", &line[..index + marker.len()]))
            })
            .unwrap_or_else(|| line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn contains_unbounded_evidence(value: &str) -> bool {
    let lowercase = value.to_ascii_lowercase();
    [
        "diff --git",
        "@@ -",
        "--- a/",
        "+++ b/",
        "*** begin patch",
        "tool_input",
        "tool_output",
        "tool_result",
        "<tool_result>",
    ]
    .iter()
    .any(|marker| lowercase.contains(marker))
}

fn validate_handoff_relationships(
    transaction: &Transaction<'_>,
    handoff: &TaskHandoff,
) -> Result<()> {
    let episode: (Option<String>, String) = transaction.query_row(
        "SELECT project_id, conversation_key FROM task_episodes WHERE id=?1",
        [handoff.episode_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if episode.0 != handoff.project_id || episode.1 != handoff.conversation_key {
        bail!("handoff episode project or conversation does not match");
    }
    let source: (String, Option<String>, String) = transaction.query_row(
        "SELECT client, project_id, conversation_key FROM sessions WHERE id=?1",
        [handoff.source_session_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if source.0 != handoff.source_client
        || source.1 != handoff.project_id
        || source.2 != handoff.conversation_key
    {
        bail!("handoff source session does not match its episode");
    }
    for event_id in &handoff.source_event_ids {
        validate_handoff_event(transaction, handoff, event_id)?;
    }
    for validation in &handoff.validation {
        validate_handoff_event(transaction, handoff, &validation.event_id)?;
    }
    Ok(())
}

fn validate_handoff_event(
    transaction: &Transaction<'_>,
    handoff: &TaskHandoff,
    event_id: &str,
) -> Result<String> {
    let (source_session_id, project_id, conversation_key): (String, Option<String>, String) =
        transaction.query_row(
            "SELECT e.session_id, s.project_id, s.conversation_key FROM session_events e JOIN sessions s ON s.id=e.session_id WHERE e.event_id=?1",
            [event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    if project_id != handoff.project_id || conversation_key != handoff.conversation_key {
        bail!("handoff evidence event does not match its conversation or project");
    }
    let assigned_episode: Option<String> = transaction
        .query_row(
            "SELECT episode_id FROM event_episode_links WHERE event_id=?1",
            [event_id],
            |row| row.get(0),
        )
        .optional()?
        .or(transaction
            .query_row(
                "SELECT episode_id FROM prompt_intents WHERE event_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .optional()?);
    let episode_id = handoff.episode_id.to_string();
    if assigned_episode
        .as_deref()
        .is_some_and(|assigned| assigned != episode_id.as_str())
    {
        bail!("handoff evidence prompt belongs to another episode");
    }
    Ok(source_session_id)
}

fn insert_handoff(transaction: &Transaction<'_>, handoff: &TaskHandoff) -> Result<()> {
    transaction.execute(
        "INSERT INTO handoffs(id, project_id, conversation_key, episode_id, source_session_id, source_client, status, goal, current_state, completed_work_json, pending_work_json, next_action, blockers_json, changed_files_json, decisions_json, validation_json, relevant_memory_ids_json, source_event_ids_json, git_head, worktree_state_hash, revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, 1, ?21, ?22)",
        params![
            handoff.id.to_string(),
            handoff.project_id,
            handoff.conversation_key,
            handoff.episode_id.to_string(),
            handoff.source_session_id.to_string(),
            handoff.source_client,
            handoff_status(handoff.status),
            handoff.goal,
            handoff.current_state,
            serde_json::to_string(&handoff.completed_work)?,
            serde_json::to_string(&handoff.pending_work)?,
            handoff.next_action,
            serde_json::to_string(&handoff.blockers)?,
            serde_json::to_string(&handoff.changed_files)?,
            serde_json::to_string(&handoff.decisions)?,
            serde_json::to_string(&handoff.validation)?,
            serde_json::to_string(&handoff.relevant_memory_ids.iter().map(Uuid::to_string).collect::<Vec<_>>())?,
            serde_json::to_string(&handoff.source_event_ids)?,
            handoff.git_head,
            handoff.worktree_state_hash,
            handoff.created_at.to_rfc3339(),
            handoff.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn update_handoff(
    transaction: &Transaction<'_>,
    handoff: &TaskHandoff,
    revision: u32,
) -> Result<()> {
    transaction.execute(
        "UPDATE handoffs SET project_id=?1, conversation_key=?2, episode_id=?3, source_session_id=?4, source_client=?5, status=?6, goal=?7, current_state=?8, completed_work_json=?9, pending_work_json=?10, next_action=?11, blockers_json=?12, changed_files_json=?13, decisions_json=?14, validation_json=?15, relevant_memory_ids_json=?16, source_event_ids_json=?17, git_head=?18, worktree_state_hash=?19, revision=?20, updated_at=?21 WHERE id=?22",
        params![
            handoff.project_id,
            handoff.conversation_key,
            handoff.episode_id.to_string(),
            handoff.source_session_id.to_string(),
            handoff.source_client,
            handoff_status(handoff.status),
            handoff.goal,
            handoff.current_state,
            serde_json::to_string(&handoff.completed_work)?,
            serde_json::to_string(&handoff.pending_work)?,
            handoff.next_action,
            serde_json::to_string(&handoff.blockers)?,
            serde_json::to_string(&handoff.changed_files)?,
            serde_json::to_string(&handoff.decisions)?,
            serde_json::to_string(&handoff.validation)?,
            serde_json::to_string(&handoff.relevant_memory_ids.iter().map(Uuid::to_string).collect::<Vec<_>>())?,
            serde_json::to_string(&handoff.source_event_ids)?,
            handoff.git_head,
            handoff.worktree_state_hash,
            revision,
            handoff.updated_at.to_rfc3339(),
            handoff.id.to_string(),
        ],
    )?;
    Ok(())
}

fn update_handoff_status(
    transaction: &Transaction<'_>,
    id: Uuid,
    status: HandoffStatus,
    revision: u32,
) -> Result<()> {
    transaction.execute(
        "UPDATE handoffs SET status=?1, revision=?2, updated_at=?3 WHERE id=?4",
        params![
            handoff_status(status),
            revision,
            Utc::now().to_rfc3339(),
            id.to_string()
        ],
    )?;
    Ok(())
}

fn current_revision(transaction: &Transaction<'_>, id: Uuid) -> Result<u32> {
    Ok(transaction.query_row(
        "SELECT revision FROM handoffs WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?)
}

fn store_handoff_revision(transaction: &Transaction<'_>, handoff: &TaskHandoff) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO handoff_versions(handoff_id, revision, status, snapshot_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            handoff.id.to_string(),
            current_revision(transaction, handoff.id)?,
            handoff_status(handoff.status),
            serde_json::to_string(handoff)?,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn replace_handoff_evidence(transaction: &Transaction<'_>, handoff: &TaskHandoff) -> Result<()> {
    transaction.execute(
        "DELETE FROM handoff_evidence WHERE handoff_id=?1",
        [handoff.id.to_string()],
    )?;
    for (ordinal, event_id) in handoff.source_event_ids.iter().enumerate() {
        let source_session_id: String = transaction.query_row(
            "SELECT session_id FROM session_events WHERE event_id=?1",
            [event_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO handoff_evidence(handoff_id, source_session_id, event_id, ordinal) VALUES (?1, ?2, ?3, ?4)",
            params![handoff.id.to_string(), source_session_id, event_id, ordinal],
        )?;
    }
    Ok(())
}

fn enqueue_checkpoint_job(
    transaction: &Transaction<'_>,
    episode_id: Uuid,
    debounce_until: &str,
    now: &str,
) -> Result<()> {
    let dedupe_key = episode_id.to_string();
    transaction.execute(
        "INSERT OR IGNORE INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at) VALUES (?1, 'checkpoint_handoff', ?2, 'pending', ?3, ?4, ?5, ?5)",
        params![
            Uuid::now_v7().to_string(),
            dedupe_key,
            serde_json::json!({"episode_id": episode_id}).to_string(),
            debounce_until,
            now,
        ],
    )?;
    transaction.execute(
        "UPDATE jobs SET status='pending', attempt_count=0, next_retry_at=?1, last_error=NULL, provider=NULL, owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?2 WHERE job_type='checkpoint_handoff' AND dedupe_key=?3 AND status IN ('completed', 'failed')",
        params![debounce_until, now, dedupe_key],
    )?;
    transaction.execute(
        "UPDATE jobs SET next_retry_at=CASE WHEN next_retry_at < ?1 THEN ?1 ELSE next_retry_at END, updated_at=?2 WHERE job_type='checkpoint_handoff' AND dedupe_key=?3 AND status='pending'",
        params![debounce_until, now, dedupe_key],
    )?;
    Ok(())
}

fn checkpoint_state_connection(
    connection: &Connection,
    episode_id: Uuid,
) -> Result<CheckpointState> {
    connection
        .query_row(
            "SELECT episode_id, dirty, debounce_until, last_checkpoint_at, revision, updated_at FROM checkpoint_state WHERE episode_id=?1",
            [episode_id.to_string()],
            |row| {
                let id: String = row.get(0)?;
                let debounce_until: Option<String> = row.get(2)?;
                let last_checkpoint_at: Option<String> = row.get(3)?;
                let updated_at: String = row.get(5)?;
                Ok(CheckpointState {
                    episode_id: Uuid::parse_str(&id).map_err(sql_conversion_error)?,
                    dirty: row.get(1)?,
                    debounce_until: debounce_until
                        .map(|value| parse_timestamp(&value).map_err(sql_conversion_error))
                        .transpose()?,
                    last_checkpoint_at: last_checkpoint_at
                        .map(|value| parse_timestamp(&value).map_err(sql_conversion_error))
                        .transpose()?,
                    revision: row.get(4)?,
                    updated_at: parse_timestamp(&updated_at).map_err(sql_conversion_error)?,
                })
            },
        )
        .map_err(Into::into)
}

fn is_current_handoff_status(status: HandoffStatus) -> bool {
    matches!(
        status,
        HandoffStatus::Active | HandoffStatus::Ready | HandoffStatus::Consumed
    )
}

fn same_handoff_content(left: &TaskHandoff, right: &TaskHandoff) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.status = HandoffStatus::Active;
    right.status = HandoffStatus::Active;
    left.created_at = right.created_at;
    left.updated_at = right.updated_at;
    left == right
}

fn handoff_status(status: HandoffStatus) -> &'static str {
    match status {
        HandoffStatus::Active => "active",
        HandoffStatus::Ready => "ready",
        HandoffStatus::Consumed => "consumed",
        HandoffStatus::Completed => "completed",
        HandoffStatus::Stale => "stale",
        HandoffStatus::Superseded => "superseded",
    }
}

fn parse_handoff_status(value: &str) -> std::io::Result<HandoffStatus> {
    match value {
        "active" => Ok(HandoffStatus::Active),
        "ready" => Ok(HandoffStatus::Ready),
        "consumed" => Ok(HandoffStatus::Consumed),
        "completed" => Ok(HandoffStatus::Completed),
        "stale" => Ok(HandoffStatus::Stale),
        "superseded" => Ok(HandoffStatus::Superseded),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid handoff status: {value}"),
        )),
    }
}

fn create_session(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    project_id: Option<&str>,
    generation: u32,
) -> Result<SessionRecord> {
    let id = Uuid::now_v7();
    let conversation_key = conversation_key(&event.client, &event.external_session_id);
    ensure_conversation(
        transaction,
        &conversation_key,
        &event.client,
        &event.external_session_id,
        &event.timestamp,
    )?;
    transaction.execute(
        "INSERT INTO sessions(id, client, external_session_id, project_id, generation, state, started_at, last_event_at, imported, conversation_key) VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?6, 0, ?7)",
        params![
            id.to_string(),
            event.client,
            event.external_session_id,
            project_id,
            generation,
            event.timestamp.to_rfc3339(),
            conversation_key,
        ],
    )?;
    session_by_id(transaction, id)
}

fn latest_session(
    transaction: &Transaction<'_>,
    client: &str,
    external_session_id: &str,
) -> Result<Option<SessionRecord>> {
    transaction
        .query_row(
            "SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, conversation_key FROM sessions WHERE client=?1 AND external_session_id=?2 ORDER BY generation DESC LIMIT 1",
            params![client, external_session_id],
            session_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn latest_session_connection(
    connection: &Connection,
    client: &str,
    external_session_id: &str,
) -> Result<Option<SessionRecord>> {
    connection
        .query_row(
            "SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, conversation_key FROM sessions WHERE client=?1 AND external_session_id=?2 ORDER BY generation DESC LIMIT 1",
            params![client, external_session_id],
            session_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn latest_session_for_project_connection(
    connection: &Connection,
    client: &str,
    external_session_id: &str,
    project_id: Option<&str>,
) -> Result<Option<SessionRecord>> {
    connection
        .query_row(
            "SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, conversation_key FROM sessions WHERE client=?1 AND external_session_id=?2 AND project_id IS ?3 ORDER BY generation DESC LIMIT 1",
            params![client, external_session_id, project_id],
            session_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn active_episode_connection(
    connection: &Connection,
    conversation_key: &str,
    project_id: Option<&str>,
) -> Result<Option<TaskEpisode>> {
    connection
        .query_row(
            "SELECT id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at FROM task_episodes WHERE conversation_key=?1 AND state='active' AND project_id IS ?2 ORDER BY ordinal DESC, id DESC LIMIT 1",
            params![conversation_key, project_id],
            episode_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn conversation_root_event_id(
    connection: &Connection,
    conversation_key: &str,
    project_id: Option<&str>,
) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT root_event_id FROM task_episodes WHERE conversation_key=?1 AND project_id IS ?2 ORDER BY ordinal, id LIMIT 1",
            params![conversation_key, project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn episode_prompt_texts(
    connection: &Connection,
    episode_id: Uuid,
) -> Result<Vec<(String, String, String)>> {
    let mut statement = connection.prepare(
        "SELECT i.event_id, i.kind, e.payload_json FROM prompt_intents i JOIN session_events e ON e.event_id=i.event_id WHERE i.episode_id=?1 AND i.kind IN ('correction', 'constraint') ORDER BY i.classified_at, i.event_id",
    )?;
    let rows = statement.query_map([episode_id.to_string()], |row| {
        let event_id: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let payload: String = row.get(2)?;
        Ok((event_id, kind, payload))
    })?;
    let prompts = rows
        .map(|row| {
            let (event_id, kind, payload) = row?;
            let event: NormalizedEvent = serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok::<_, rusqlite::Error>((event_id, kind, event.bounded_input.unwrap_or_default()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(prompts
        .into_iter()
        .filter(|(_, _, text)| !text.trim().is_empty())
        .collect())
}

fn event_prompt_text(connection: &Connection, event_id: &str) -> Result<Option<String>> {
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload_json FROM session_events WHERE event_id=?1",
            [event_id],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .map(|payload| {
            let event: NormalizedEvent = serde_json::from_str(&payload)?;
            Ok(event.bounded_input.filter(|text| !text.trim().is_empty()))
        })
        .transpose()
        .map(|value| value.flatten())
}

fn session_by_id(connection: &Connection, id: Uuid) -> Result<SessionRecord> {
    connection
        .query_row(
            "SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, conversation_key FROM sessions WHERE id=?1",
            [id.to_string()],
            session_from_row,
        )
        .map_err(Into::into)
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let id: String = row.get(0)?;
    let state: String = row.get(5)?;
    let started_at: String = row.get(6)?;
    let ended_at: Option<String> = row.get(7)?;
    let last_event_at: String = row.get(8)?;
    let conversation_key: String = row.get(11)?;
    Ok(SessionRecord {
        id: Uuid::parse_str(&id).map_err(sql_conversion_error)?,
        client: row.get(1)?,
        external_session_id: row.get(2)?,
        project_id: row.get(3)?,
        generation: row.get(4)?,
        state: parse_session_state(&state).map_err(sql_conversion_error)?,
        started_at: parse_timestamp(&started_at).map_err(sql_conversion_error)?,
        ended_at: ended_at
            .map(|value| parse_timestamp(&value))
            .transpose()
            .map_err(sql_conversion_error)?,
        last_event_at: parse_timestamp(&last_event_at).map_err(sql_conversion_error)?,
        markdown_path: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
        imported: row.get(10)?,
        conversation_key,
    })
}

fn event_exists(transaction: &Transaction<'_>, event_id: &str) -> Result<bool> {
    Ok(transaction
        .query_row(
            "SELECT 1 FROM session_events WHERE event_id=?1",
            [event_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn enqueue_job(transaction: &Transaction<'_>, job_type: &str, dedupe_key: &str) -> Result<()> {
    enqueue_job_connection(transaction, job_type, dedupe_key)
}

fn enqueue_job_connection(connection: &Connection, job_type: &str, dedupe_key: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    connection.execute(
        "INSERT OR IGNORE INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at) VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?5, ?5)",
        params![
            Uuid::now_v7().to_string(),
            job_type,
            dedupe_key,
            serde_json::json!({ "id": dedupe_key }).to_string(),
            now
        ],
    )?;
    Ok(())
}

fn observation_content(event: &NormalizedEvent) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(input) = &event.bounded_input {
        parts.push(input.as_str());
    }
    if let Some(output) = &event.bounded_output {
        parts.push(output.as_str());
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn apply_migrations(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);",
    )?;
    let current: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if current < 2 {
        migrate_to_version_2(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (2)", [])?;
    }
    if current < 3 {
        migrate_to_version_3(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (3)", [])?;
    }
    if current < 4 {
        migrate_to_version_4(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (4)", [])?;
    }
    if current < 5 {
        migrate_to_version_5(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (5)", [])?;
    }
    if current < 6 {
        migrate_to_version_6(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (6)", [])?;
    }
    if current < 7 {
        migrate_to_version_7(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (7)", [])?;
    }
    if current < 8 {
        migrate_to_version_8(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (8)", [])?;
    }
    if current < 9 {
        migrate_to_version_9(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (9)", [])?;
    }
    if current < 10 {
        migrate_to_version_10(connection)?;
        connection.execute("INSERT INTO schema_migrations(version) VALUES (10)", [])?;
    }
    Ok(())
}

fn migrate_legacy_operational_tables(
    connection: &mut Connection,
    legacy_index: &Path,
) -> Result<()> {
    let legacy_path = legacy_index.to_string_lossy().into_owned();
    connection.execute("ATTACH DATABASE ?1 AS legacy", [&legacy_path])?;
    let result = migrate_attached_operational_tables(connection);
    if let Err(error) = result {
        let _ = connection.execute_batch("DETACH DATABASE legacy");
        return Err(error);
    }
    connection.execute_batch("DETACH DATABASE legacy")?;
    Ok(())
}

fn migrate_attached_operational_tables(connection: &mut Connection) -> Result<()> {
    let migration_complete: i64 = connection.query_row(
        "SELECT COUNT(*) FROM operational_migration_markers WHERE migration=?1",
        [OPERATIONAL_MIGRATION],
        |row| row.get(0),
    )?;
    if migration_complete == i64::try_from(OPERATIONAL_TABLES.len())? {
        return Ok(());
    }

    for (table, columns) in OPERATIONAL_TABLES {
        let marked: Option<()> = connection
            .query_row(
                "SELECT 1 FROM operational_migration_markers WHERE migration=?1 AND table_name=?2",
                params![OPERATIONAL_MIGRATION, table],
                |_| Ok(()),
            )
            .optional()?;
        if marked.is_some() {
            continue;
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM legacy.sqlite_master WHERE type='table' AND name=?1)",
            [*table],
            |row| row.get(0),
        )?;
        if exists {
            if *table == "session_injections" {
                let has_client: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM legacy.pragma_table_info('session_injections') WHERE name='client')",
                    [],
                    |row| row.get(0),
                )?;
                if has_client {
                    transaction.execute(
                        "INSERT OR IGNORE INTO session_injections(client, conversation_key, generation, episode_id, memory_id, injected_at) SELECT client, conversation_key, generation, episode_id, memory_id, injected_at FROM legacy.session_injections",
                        [],
                    )?;
                } else {
                    transaction.execute(
                        "INSERT OR IGNORE INTO session_injections(client, conversation_key, generation, episode_id, memory_id, injected_at) SELECT 'legacy', session_key, 0, '', memory_id, injected_at FROM legacy.session_injections",
                        [],
                    )?;
                }
            } else if *table == "checkpoint_state" {
                let has_revision: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM legacy.pragma_table_info('checkpoint_state') WHERE name='revision')",
                    [],
                    |row| row.get(0),
                )?;
                if has_revision {
                    transaction.execute(
                        &format!(
                            "INSERT OR IGNORE INTO {table}({columns}) SELECT {columns} FROM legacy.{table}"
                        ),
                        [],
                    )?;
                } else {
                    transaction.execute(
                        "INSERT OR IGNORE INTO checkpoint_state(episode_id, dirty, debounce_until, last_checkpoint_at, revision, updated_at) SELECT episode_id, dirty, debounce_until, last_checkpoint_at, 0, updated_at FROM legacy.checkpoint_state",
                        [],
                    )?;
                }
            } else {
                transaction.execute(
                    &format!(
                        "INSERT OR IGNORE INTO {table}({columns}) SELECT {columns} FROM legacy.{table}"
                    ),
                    [],
                )?;
            }
        }
        transaction.execute(
            "INSERT INTO operational_migration_markers(migration, table_name, completed_at) VALUES (?1, ?2, ?3)",
            params![OPERATIONAL_MIGRATION, table, Utc::now().to_rfc3339()],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

fn migrate_to_version_2(connection: &Connection) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    connection.execute(
        "UPDATE jobs
         SET status = 'pending',
             attempt_count = 0,
             last_error = NULL,
             provider = NULL,
             next_retry_at = ?1,
             updated_at = ?1
         WHERE job_type = 'compile_session'
           AND status = 'failed'
           AND last_error LIKE '%invalid_json_schema%'",
        params![now],
    )?;
    Ok(())
}

fn migrate_to_version_3(connection: &Connection) -> Result<()> {
    for column in ["owner", "lease_started_at", "lease_until"] {
        let exists: Option<()> = connection
            .query_row(
                "SELECT 1 FROM pragma_table_info('jobs') WHERE name=?1",
                [column],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_none() {
            connection.execute(&format!("ALTER TABLE jobs ADD COLUMN {column} TEXT"), [])?;
        }
    }
    connection.execute(
        "UPDATE jobs
         SET status='pending', owner=NULL, lease_started_at=NULL, lease_until=NULL
         WHERE status='running'",
        [],
    )?;
    Ok(())
}

fn migrate_to_version_4(connection: &Connection) -> Result<()> {
    let exists: Option<()> = connection
        .query_row(
            "SELECT 1 FROM pragma_table_info('sessions') WHERE name='conversation_key'",
            [],
            |_| Ok(()),
        )
        .optional()?;
    if exists.is_none() {
        connection.execute(
            "ALTER TABLE sessions ADD COLUMN conversation_key TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    ensure_conversation_keys(connection)
}

fn migrate_to_version_5(connection: &Connection) -> Result<()> {
    let has_client = connection
        .query_row(
            "SELECT 1 FROM pragma_table_info('session_injections') WHERE name='client'",
            [],
            |_| Ok(()),
        )
        .optional()?;
    if has_client.is_some() {
        return Ok(());
    }
    connection.execute_batch(
        "ALTER TABLE session_injections RENAME TO session_injections_legacy;
         CREATE TABLE session_injections (
             client TEXT NOT NULL,
             conversation_key TEXT NOT NULL,
             generation INTEGER NOT NULL,
             episode_id TEXT NOT NULL DEFAULT '',
             memory_id TEXT NOT NULL,
             injected_at TEXT NOT NULL,
             PRIMARY KEY(client, conversation_key, generation, episode_id, memory_id)
         );
         INSERT OR IGNORE INTO session_injections(client, conversation_key, generation, episode_id, memory_id, injected_at)
         SELECT 'legacy', session_key, 0, '', memory_id, injected_at FROM session_injections_legacy;
         DROP TABLE session_injections_legacy;",
    )?;
    Ok(())
}

fn migrate_to_version_6(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS briefing_deliveries (
             client TEXT NOT NULL,
             conversation_key TEXT NOT NULL,
             generation INTEGER NOT NULL,
             episode_id TEXT NOT NULL DEFAULT '',
             delivered_at TEXT NOT NULL,
             PRIMARY KEY(client, conversation_key, generation, episode_id)
         );",
    )?;
    Ok(())
}

fn migrate_to_version_7(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS handoffs (
             id TEXT PRIMARY KEY,
             project_id TEXT,
             conversation_key TEXT NOT NULL,
             episode_id TEXT NOT NULL,
             source_session_id TEXT NOT NULL,
             source_client TEXT NOT NULL,
             status TEXT NOT NULL CHECK(status IN ('active', 'ready', 'consumed', 'completed', 'stale', 'superseded')),
             goal TEXT NOT NULL,
             current_state TEXT NOT NULL,
             completed_work_json TEXT NOT NULL,
             pending_work_json TEXT NOT NULL,
             next_action TEXT,
             blockers_json TEXT NOT NULL,
             changed_files_json TEXT NOT NULL,
             decisions_json TEXT NOT NULL,
             validation_json TEXT NOT NULL,
             relevant_memory_ids_json TEXT NOT NULL,
             source_event_ids_json TEXT NOT NULL,
             git_head TEXT,
             worktree_state_hash TEXT,
             revision INTEGER NOT NULL CHECK(revision > 0),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             FOREIGN KEY(episode_id) REFERENCES task_episodes(id),
             FOREIGN KEY(source_session_id) REFERENCES sessions(id)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS handoffs_current_episode ON handoffs(episode_id) WHERE status IN ('active', 'ready', 'consumed');
         CREATE INDEX IF NOT EXISTS handoffs_project_status ON handoffs(project_id, status, updated_at DESC);
         CREATE TABLE IF NOT EXISTS handoff_versions (
             handoff_id TEXT NOT NULL,
             revision INTEGER NOT NULL,
             status TEXT NOT NULL,
             snapshot_json TEXT NOT NULL,
             created_at TEXT NOT NULL,
             PRIMARY KEY(handoff_id, revision),
             FOREIGN KEY(handoff_id) REFERENCES handoffs(id)
         );
         CREATE INDEX IF NOT EXISTS handoff_versions_handoff ON handoff_versions(handoff_id, revision);
         CREATE TABLE IF NOT EXISTS handoff_evidence (
             handoff_id TEXT NOT NULL,
             source_session_id TEXT NOT NULL,
             event_id TEXT NOT NULL,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             PRIMARY KEY(handoff_id, event_id),
             FOREIGN KEY(handoff_id) REFERENCES handoffs(id),
             FOREIGN KEY(source_session_id) REFERENCES sessions(id),
             FOREIGN KEY(event_id) REFERENCES session_events(event_id)
         );
         CREATE INDEX IF NOT EXISTS handoff_evidence_event ON handoff_evidence(event_id);
         CREATE TABLE IF NOT EXISTS checkpoint_state (
             episode_id TEXT PRIMARY KEY,
             dirty INTEGER NOT NULL CHECK(dirty IN (0, 1)),
             debounce_until TEXT,
             last_checkpoint_at TEXT,
             revision INTEGER NOT NULL DEFAULT 0,
             updated_at TEXT NOT NULL,
             FOREIGN KEY(episode_id) REFERENCES task_episodes(id) ON DELETE CASCADE
         );",
    )?;
    Ok(())
}

fn migrate_to_version_8(connection: &Connection) -> Result<()> {
    let exists: Option<()> = connection
        .query_row(
            "SELECT 1 FROM pragma_table_info('checkpoint_state') WHERE name='revision'",
            [],
            |_| Ok(()),
        )
        .optional()?;
    if exists.is_none() {
        connection.execute(
            "ALTER TABLE checkpoint_state ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

fn migrate_to_version_9(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS event_episode_links (
             event_id TEXT PRIMARY KEY,
             episode_id TEXT NOT NULL,
             linked_at TEXT NOT NULL,
             FOREIGN KEY(event_id) REFERENCES session_events(event_id),
             FOREIGN KEY(episode_id) REFERENCES task_episodes(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS event_episode_links_episode ON event_episode_links(episode_id, event_id);",
    )?;
    Ok(())
}

fn migrate_to_version_10(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS handoff_deliveries (
             client TEXT NOT NULL,
             conversation_key TEXT NOT NULL,
             generation INTEGER NOT NULL,
             handoff_id TEXT NOT NULL,
             delivery_kind TEXT NOT NULL CHECK(delivery_kind IN ('full', 'card')),
             delivered_at TEXT NOT NULL,
             PRIMARY KEY(client, conversation_key, generation, handoff_id, delivery_kind),
             FOREIGN KEY(handoff_id) REFERENCES handoffs(id)
         );",
    )?;
    Ok(())
}

fn ensure_conversation_keys(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT client, external_session_id, MIN(started_at), MAX(last_event_at)
         FROM sessions WHERE conversation_key='' OR conversation_key IS NULL
         GROUP BY client, external_session_id",
    )?;
    let sessions = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (client, external_session_id, created_at, updated_at) in sessions {
        let key = conversation_key(&client, &external_session_id);
        connection.execute(
            "INSERT OR IGNORE INTO conversations(conversation_key, client, external_session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![key, client, external_session_id, created_at],
        )?;
        connection.execute(
            "UPDATE conversations SET updated_at=?1 WHERE conversation_key=?2",
            params![updated_at, key],
        )?;
        connection.execute(
            "UPDATE sessions SET conversation_key=?1 WHERE client=?2 AND external_session_id=?3 AND (conversation_key='' OR conversation_key IS NULL)",
            params![key, client, external_session_id],
        )?;
    }
    Ok(())
}

fn ensure_conversation(
    connection: &Connection,
    key: &str,
    client: &str,
    external_session_id: &str,
    timestamp: &DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "INSERT INTO conversations(conversation_key, client, external_session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(conversation_key) DO UPDATE SET updated_at=excluded.updated_at",
        params![
            key,
            client,
            external_session_id,
            timestamp.to_rfc3339()
        ],
    )?;
    Ok(())
}

fn insert_episode(connection: &Connection, episode: &TaskEpisode) -> Result<()> {
    connection.execute(
        "INSERT INTO task_episodes(id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            episode.id.to_string(),
            episode.project_id,
            episode.conversation_key,
            episode.root_event_id,
            episode.goal,
            episode.ordinal,
            episode_state(episode.state),
            episode.created_at.to_rfc3339(),
            episode.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn episode_by_id(connection: &Connection, id: Uuid) -> Result<TaskEpisode> {
    connection
        .query_row(
            "SELECT id, project_id, conversation_key, root_event_id, goal, ordinal, state, created_at, updated_at FROM task_episodes WHERE id=?1",
            [id.to_string()],
            episode_from_row,
        )
        .map_err(Into::into)
}

fn episode_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEpisode> {
    let id: String = row.get(0)?;
    let state: String = row.get(6)?;
    let created_at: String = row.get(7)?;
    let updated_at: String = row.get(8)?;
    Ok(TaskEpisode {
        id: Uuid::parse_str(&id).map_err(sql_conversion_error)?,
        project_id: row.get(1)?,
        conversation_key: row.get(2)?,
        root_event_id: row.get(3)?,
        goal: row.get(4)?,
        ordinal: row.get(5)?,
        state: parse_episode_state(&state).map_err(sql_conversion_error)?,
        created_at: parse_timestamp(&created_at).map_err(sql_conversion_error)?,
        updated_at: parse_timestamp(&updated_at).map_err(sql_conversion_error)?,
    })
}

fn prompt_intent_by_event(connection: &Connection, event_id: &str) -> Result<PromptIntent> {
    connection
        .query_row(
            "SELECT event_id, episode_id, kind, confidence, weight, classifier_version, source, classified_at FROM prompt_intents WHERE event_id=?1",
            [event_id],
            prompt_intent_from_row,
        )
        .map_err(Into::into)
}

fn prompt_intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PromptIntent> {
    let episode_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let source: String = row.get(6)?;
    let classified_at: String = row.get(7)?;
    Ok(PromptIntent {
        event_id: row.get(0)?,
        episode_id: Uuid::parse_str(&episode_id).map_err(sql_conversion_error)?,
        kind: parse_prompt_intent_kind(&kind).map_err(sql_conversion_error)?,
        confidence: row.get(3)?,
        weight: row.get(4)?,
        classifier_version: row.get(5)?,
        source: parse_classification_source(&source).map_err(sql_conversion_error)?,
        classified_at: parse_timestamp(&classified_at).map_err(sql_conversion_error)?,
    })
}

fn history_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PromptIntentHistory> {
    let intent = PromptIntent {
        event_id: row.get(0)?,
        episode_id: Uuid::parse_str(&row.get::<_, String>(3)?).map_err(sql_conversion_error)?,
        kind: parse_prompt_intent_kind(&row.get::<_, String>(4)?).map_err(sql_conversion_error)?,
        confidence: row.get(5)?,
        weight: row.get(6)?,
        classifier_version: row.get(7)?,
        source: parse_classification_source(&row.get::<_, String>(8)?)
            .map_err(sql_conversion_error)?,
        classified_at: parse_timestamp(&row.get::<_, String>(9)?).map_err(sql_conversion_error)?,
    };
    Ok(PromptIntentHistory {
        event_id: intent.event_id.clone(),
        revision: row.get(1)?,
        conversation_key: row.get(2)?,
        previous: intent,
        replaced_at: parse_timestamp(&row.get::<_, String>(10)?).map_err(sql_conversion_error)?,
    })
}

fn current_conversation_key(connection: &Connection, event_id: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT conversation_key FROM prompt_intents WHERE event_id=?1",
            [event_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn validate_intent(connection: &Connection, intent: &PromptIntent) -> Result<String> {
    if !(0.0..=1.0).contains(&intent.confidence) || !(0.0..=1.0).contains(&intent.weight) {
        bail!("prompt intent confidence and weight must be between 0 and 1");
    }
    let session: (String, Option<String>) = connection.query_row(
        "SELECT conversation_key, project_id FROM sessions WHERE id=(SELECT session_id FROM session_events WHERE event_id=?1)",
        [&intent.event_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let episode: (String, Option<String>) = connection.query_row(
        "SELECT conversation_key, project_id FROM task_episodes WHERE id=?1",
        [intent.episode_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if session != episode {
        bail!("prompt intent episode does not match its event conversation or project");
    }
    Ok(session.0)
}

fn same_classification(current: &PromptIntent, next: &PromptIntent) -> bool {
    current.event_id == next.event_id
        && current.episode_id == next.episode_id
        && current.kind == next.kind
        && current.confidence == next.confidence
        && current.weight == next.weight
        && current.classifier_version == next.classifier_version
        && current.source == next.source
}

pub fn conversation_key(client: &str, external_session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"menvane-conversation-v1\0");
    for value in [client, external_session_id] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let digest = hasher.finalize();
    let encoded = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("v1:{encoded}")
}

fn state_after_event(current: SessionState, event: NormalizedEventKind) -> SessionState {
    match event {
        NormalizedEventKind::SessionEnded => SessionState::Finalized,
        NormalizedEventKind::TurnStopped => SessionState::Idle,
        _ => match current {
            SessionState::Idle => SessionState::Open,
            state => state,
        },
    }
}

fn event_kind(kind: NormalizedEventKind) -> &'static str {
    match kind {
        NormalizedEventKind::SessionStarted => "session-started",
        NormalizedEventKind::UserPrompt => "user-prompt",
        NormalizedEventKind::ToolCompleted => "tool-completed",
        NormalizedEventKind::ContextCompacted => "context-compacted",
        NormalizedEventKind::TurnStopped => "turn-stopped",
        NormalizedEventKind::SessionEnded => "session-ended",
    }
}

fn session_state(state: SessionState) -> &'static str {
    match state {
        SessionState::Open => "open",
        SessionState::Idle => "idle",
        SessionState::Finalized => "finalized",
    }
}

fn episode_state(state: EpisodeState) -> &'static str {
    match state {
        EpisodeState::Active => "active",
        EpisodeState::Dormant => "dormant",
        EpisodeState::Completed => "completed",
    }
}

fn parse_episode_state(value: &str) -> std::io::Result<EpisodeState> {
    match value {
        "active" => Ok(EpisodeState::Active),
        "dormant" => Ok(EpisodeState::Dormant),
        "completed" => Ok(EpisodeState::Completed),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid episode state: {value}"),
        )),
    }
}

fn prompt_intent_kind(kind: PromptIntentKind) -> &'static str {
    match kind {
        PromptIntentKind::RootGoal => "root-goal",
        PromptIntentKind::NewGoal => "new-goal",
        PromptIntentKind::Refinement => "refinement",
        PromptIntentKind::Constraint => "constraint",
        PromptIntentKind::Correction => "correction",
        PromptIntentKind::FollowUp => "follow-up",
        PromptIntentKind::Operational => "operational",
    }
}

fn parse_prompt_intent_kind(value: &str) -> std::io::Result<PromptIntentKind> {
    match value {
        "root-goal" => Ok(PromptIntentKind::RootGoal),
        "new-goal" => Ok(PromptIntentKind::NewGoal),
        "refinement" => Ok(PromptIntentKind::Refinement),
        "constraint" => Ok(PromptIntentKind::Constraint),
        "correction" => Ok(PromptIntentKind::Correction),
        "follow-up" => Ok(PromptIntentKind::FollowUp),
        "operational" => Ok(PromptIntentKind::Operational),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid prompt intent kind: {value}"),
        )),
    }
}

fn classification_source(source: IntentClassificationSource) -> &'static str {
    match source {
        IntentClassificationSource::Deterministic => "deterministic",
        IntentClassificationSource::ProviderReview => "provider-review",
    }
}

fn parse_classification_source(value: &str) -> std::io::Result<IntentClassificationSource> {
    match value {
        "deterministic" => Ok(IntentClassificationSource::Deterministic),
        "provider-review" => Ok(IntentClassificationSource::ProviderReview),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid classification source: {value}"),
        )),
    }
}

fn parse_session_state(value: &str) -> std::io::Result<SessionState> {
    match value {
        "open" => Ok(SessionState::Open),
        "idle" => Ok(SessionState::Idle),
        "finalized" => Ok(SessionState::Finalized),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid session state: {value}"),
        )),
    }
}

fn parse_timestamp(value: &str) -> std::result::Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).map(|timestamp| timestamp.with_timezone(&Utc))
}

fn parse_optional_timestamp(
    value: Option<String>,
) -> std::result::Result<Option<DateTime<Utc>>, chrono::ParseError> {
    value.map(|value| parse_timestamp(&value)).transpose()
}

fn sql_conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
