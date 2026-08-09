use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use menvane_domain::{NormalizedEvent, NormalizedEventKind, ReinforcementSignal, SessionState};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
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
    UNIQUE(client, external_session_id, generation)
);
CREATE INDEX IF NOT EXISTS sessions_external ON sessions(client, external_session_id, generation DESC);
CREATE INDEX IF NOT EXISTS sessions_state_event ON sessions(state, last_event_at);
CREATE TABLE IF NOT EXISTS session_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);
CREATE INDEX IF NOT EXISTS session_events_session ON session_events(session_id, timestamp, event_id);
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
    session_key TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    injected_at TEXT NOT NULL,
    PRIMARY KEY(session_key, memory_id)
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
CREATE TABLE IF NOT EXISTS operational_migration_markers (
    migration TEXT NOT NULL,
    table_name TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    PRIMARY KEY(migration, table_name)
);
"#;

const OPERATIONAL_MIGRATION: &str = "index-to-state-v1";

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
    ("session_injections", "session_key, memory_id, injected_at"),
    (
        "procedure_applications",
        "memory_id, source_session, signal, created_at",
    ),
    (
        "orphan_sessions",
        "client, external_session_id, payload_json, created_at",
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
            let session = latest_session(&transaction, &event.client, &event.external_session_id)?
                .context("duplicate event refers to a missing session")?;
            transaction.commit()?;
            return Ok(IngestResult {
                session,
                inserted: false,
                should_finalize: false,
            });
        }
        let previous = latest_session(&transaction, &event.client, &event.external_session_id)?;
        let session = match previous {
            Some(previous) if previous.state != SessionState::Finalized => previous,
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
        enqueue_compile: bool,
    ) -> Result<()> {
        let connection = self.open()?;
        let now = Utc::now().to_rfc3339();
        connection.execute(
            "UPDATE sessions SET markdown_path=?1 WHERE id=?2",
            params![markdown_path.to_string_lossy(), session_id.to_string()],
        )?;
        connection.execute(
            "UPDATE jobs SET status='completed', updated_at=?1 WHERE job_type='finalize_session' AND dedupe_key=?2",
            params![now, session_id.to_string()],
        )?;
        if enqueue_compile {
            enqueue_job_connection(&connection, "compile_session", &session_id.to_string())?;
        }
        Ok(())
    }

    pub fn jobs(&self) -> Result<Vec<JobRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key FROM jobs ORDER BY created_at",
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
            ))
        })?;
        rows.map(|row| {
            let (id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key) = row?;
            Ok(JobRecord {
                id: Uuid::parse_str(&id)?,
                job_type,
                status,
                attempt_count,
                next_retry_at: DateTime::parse_from_rfc3339(&next_retry_at)?.with_timezone(&Utc),
                last_error,
                dedupe_key,
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

    pub fn claim_compile_job(&self) -> Result<Option<JobRecord>> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<String> = transaction
            .query_row(
                "SELECT id FROM jobs WHERE job_type='compile_session' AND status='pending' AND next_retry_at <= ?1 ORDER BY created_at LIMIT 1",
                [Utc::now().to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute(
            "UPDATE jobs SET status='running', attempt_count=attempt_count+1, updated_at=?1 WHERE id=?2",
            params![Utc::now().to_rfc3339(), id],
        )?;
        let job = transaction.query_row(
            "SELECT id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key FROM jobs WHERE id=?1",
            [&id],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, String>(4)?, row.get(5)?, row.get(6)?)),
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
        }))
    }

    pub fn finish_job(&self, id: Uuid, provider: Option<&str>, error: Option<&str>) -> Result<()> {
        let connection = self.open()?;
        let job = connection.query_row(
            "SELECT attempt_count FROM jobs WHERE id=?1",
            [id.to_string()],
            |row| row.get::<_, u32>(0),
        )?;
        let (status, next_retry_at) = if error.is_none() {
            ("completed", Utc::now())
        } else if job < 5 {
            let delay = 2_i64.pow(job.min(10));
            ("pending", Utc::now() + chrono::Duration::seconds(delay))
        } else {
            ("failed", Utc::now())
        };
        connection.execute(
            "UPDATE jobs SET status=?1, next_retry_at=?2, last_error=?3, provider=?4, updated_at=?5 WHERE id=?6",
            params![status, next_retry_at.to_rfc3339(), error, provider, Utc::now().to_rfc3339(), id.to_string()],
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

    pub fn claim_injection(&self, session_key: &str, memory_id: Uuid) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute(
            "INSERT OR IGNORE INTO session_injections(session_key, memory_id, injected_at) VALUES (?1, ?2, ?3)",
            params![session_key, memory_id.to_string(), Utc::now().to_rfc3339()],
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
            "UPDATE jobs SET status='pending', attempt_count=0, next_retry_at=?1, last_error=NULL, provider=NULL, updated_at=?1 WHERE job_type='compile_session' AND dedupe_key=?2",
            params![now, session_id],
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

fn create_session(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    project_id: Option<&str>,
    generation: u32,
) -> Result<SessionRecord> {
    let id = Uuid::now_v7();
    transaction.execute(
        "INSERT INTO sessions(id, client, external_session_id, project_id, generation, state, started_at, last_event_at, imported) VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?6, 0)",
        params![
            id.to_string(),
            event.client,
            event.external_session_id,
            project_id,
            generation,
            event.timestamp.to_rfc3339()
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
            "SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported FROM sessions WHERE client=?1 AND external_session_id=?2 ORDER BY generation DESC LIMIT 1",
            params![client, external_session_id],
            session_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn session_by_id(connection: &Connection, id: Uuid) -> Result<SessionRecord> {
    connection
        .query_row(
            "SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported FROM sessions WHERE id=?1",
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
            transaction.execute(
                &format!(
                    "INSERT OR IGNORE INTO {table}({columns}) SELECT {columns} FROM legacy.{table}"
                ),
                [],
            )?;
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

fn sql_conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
