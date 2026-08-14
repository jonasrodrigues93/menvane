use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use menvane_domain::{
    ConsolidationExecution, ConsolidationResult, EpisodicSummary, HandoffItem, HandoffItemKind,
    HandoffItemSource, NormalizedEvent, NormalizedEventKind, ReinforcementSignal, SessionState,
    SummaryStatus,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_HANDOFF_LIST_LIMIT: usize = 100;
pub const MAX_HANDOFF_ITEM_BYTES: usize = 2_000;
pub const MAX_HANDOFF_SOURCE_EVENTS: usize = 128;
pub const MAX_HANDOFF_TOTAL_BYTES: usize = 32_768;
pub const MAX_CHECKPOINT_DEBOUNCE_SECONDS: i64 = 86_400;
pub const GLOBAL_HANDOFF_KEY: &str = "__global__";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_meta (
    version INTEGER PRIMARY KEY CHECK (version = 1)
);
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
    summary_status TEXT NOT NULL DEFAULT 'pending',
    summary_json TEXT,
    UNIQUE(client, external_session_id, generation)
);
CREATE INDEX IF NOT EXISTS sessions_external ON sessions(client, external_session_id, generation DESC);
CREATE INDEX IF NOT EXISTS sessions_state_event ON sessions(state, last_event_at);
CREATE TABLE IF NOT EXISTS session_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    kind TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    payload_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS session_events_session ON session_events(session_id, timestamp, event_id);
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
    owner TEXT,
    lease_started_at TEXT,
    lease_until TEXT,
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
CREATE TABLE IF NOT EXISTS orphan_sessions (
    client TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(client, external_session_id)
);
CREATE TABLE IF NOT EXISTS integration_state (
    client TEXT PRIMARY KEY,
    connected INTEGER NOT NULL,
    mcp_registered INTEGER NOT NULL,
    hook_status TEXT NOT NULL,
    last_event_at TEXT,
    details_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS access_events (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    signal TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS application_events (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    successful INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(memory_id, session_id)
);
CREATE TABLE IF NOT EXISTS consolidation_results (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id),
    result_json TEXT NOT NULL,
    execution_json TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS handoff_items (
    id TEXT PRIMARY KEY,
    project_key TEXT NOT NULL,
    project_id TEXT,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    next_step TEXT,
    blocker TEXT,
    low_confidence INTEGER NOT NULL,
    last_confirmed_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS handoff_items_project ON handoff_items(project_key, id);
CREATE TABLE IF NOT EXISTS handoff_item_sources (
    item_id TEXT NOT NULL REFERENCES handoff_items(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    event_ids_json TEXT NOT NULL,
    PRIMARY KEY(item_id, session_id)
);
CREATE TABLE IF NOT EXISTS delivery_claims (
    client TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    content_kind TEXT NOT NULL,
    content_id TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    PRIMARY KEY(client, external_session_id, generation, content_kind, content_id)
);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub summary_status: SummaryStatus,
}

#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub session_id: Uuid,
    pub event: NormalizedEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionIdentity {
    pub client: String,
    pub external_session_id: String,
    pub generation: u32,
}

#[derive(Debug, Clone)]
pub struct RecallContext {
    pub session: SessionRecord,
    pub handoff: Vec<HandoffItem>,
}

#[derive(Debug, Clone)]
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
    pub payload_json: String,
}

#[derive(Debug, Clone)]
pub struct IntegrationRecord {
    pub client: String,
    pub connected: bool,
    pub mcp_registered: bool,
    pub hook_status: String,
    pub last_event_at: Option<DateTime<Utc>>,
    pub details_json: String,
}

#[derive(Debug, Clone)]
pub struct OrphanRecord {
    pub client: String,
    pub external_session_id: String,
    pub payload_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationMarker {
    pub session_id: Uuid,
    pub result: ConsolidationResult,
    pub execution: ConsolidationExecution,
    pub applied_at: DateTime<Utc>,
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
        let connection = self.open()?;
        reject_unversioned_database(&connection)?;
        connection.execute_batch(SCHEMA)?;
        connection.execute("INSERT OR IGNORE INTO schema_meta(version) VALUES (1)", [])?;
        Ok(())
    }

    pub fn ingest(
        &self,
        event: &NormalizedEvent,
        project_id: Option<&str>,
    ) -> Result<IngestResult> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(session_id) = event_session_id(&transaction, &event.event_id)? {
            let session = session_by_id(&transaction, session_id)?;
            transaction.commit()?;
            return Ok(IngestResult {
                session,
                inserted: false,
                should_finalize: false,
            });
        }
        let previous = latest_session_tx(&transaction, &event.client, &event.external_session_id)?;
        let session = match previous {
            Some(previous) if previous.state != SessionState::Finalized => {
                if previous.project_id.as_deref() != project_id && previous.project_id.is_some() {
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
            params![event.event_id, session.id.to_string(), event_kind(event.kind), event.timestamp.to_rfc3339(), serde_json::to_string(event)?],
        )?;
        transaction.execute(
            "INSERT INTO integration_state(client, connected, mcp_registered, hook_status, last_event_at, details_json) VALUES (?1, 1, 0, 'event received', ?2, '{}') ON CONFLICT(client) DO UPDATE SET last_event_at=excluded.last_event_at",
            params![event.client, event.timestamp.to_rfc3339()],
        )?;
        let state = state_after_event(session.state, event.kind);
        let ended_at = (state == SessionState::Finalized).then(|| event.timestamp.to_rfc3339());
        transaction.execute(
            "UPDATE sessions SET state=?1, last_event_at=?2, ended_at=COALESCE(?3, ended_at), project_id=COALESCE(project_id, ?4) WHERE id=?5",
            params![session_state(state), event.timestamp.to_rfc3339(), ended_at, project_id, session.id.to_string()],
        )?;
        if state == SessionState::Finalized {
            enqueue_job_tx(
                &transaction,
                "finalize_session",
                &session.id.to_string(),
                "{}",
            )?;
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
        let mut statement = connection.prepare("SELECT payload_json FROM session_events WHERE session_id=?1 ORDER BY timestamp, event_id")?;
        let rows = statement.query_map([session_id.to_string()], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn session_events(&self, session_id: Uuid) -> Result<Vec<SessionEvent>> {
        self.events(session_id).map(|events| {
            events
                .into_iter()
                .map(|event| SessionEvent { session_id, event })
                .collect()
        })
    }

    pub fn finalize_idle_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<SessionRecord>> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statement = transaction.prepare("SELECT id FROM sessions WHERE state='idle' AND last_event_at <= ?1 ORDER BY last_event_at")?;
            statement
                .query_map([cutoff.to_rfc3339()], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut sessions = Vec::new();
        for id in ids {
            transaction.execute("UPDATE sessions SET state='finalized', ended_at=last_event_at WHERE id=?1 AND state='idle'", [&id])?;
            enqueue_job_tx(&transaction, "finalize_session", &id, "{}")?;
            sessions.push(session_by_id(&transaction, Uuid::parse_str(&id)?)?);
        }
        transaction.commit()?;
        Ok(sessions)
    }

    pub fn mark_finalized(
        &self,
        session_id: Uuid,
        markdown_path: &Path,
        job_id: Uuid,
        owner: &str,
        summary_status_value: SummaryStatus,
    ) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE sessions SET markdown_path=?1, summary_status=?2, summary_json=NULL WHERE id=?3",
            params![
                markdown_path.to_string_lossy(),
                summary_status(summary_status_value),
                session_id.to_string()
            ],
        )?;
        transaction.execute("UPDATE jobs SET status='completed', owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE id=?2 AND status='running' AND owner=?3", params![now, job_id.to_string(), owner])?;
        if summary_status_value == SummaryStatus::Pending {
            transaction.execute("INSERT OR IGNORE INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at) VALUES (?1, 'consolidate_session', ?2, 'pending', '{}', ?3, ?3, ?3)", params![Uuid::now_v7().to_string(), session_id.to_string(), now])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn session(&self, id: Uuid) -> Result<SessionRecord> {
        let connection = self.open()?;
        session_by_id(&connection, id)
    }

    pub fn find_session(&self, id: Uuid) -> Result<Option<SessionRecord>> {
        let connection = self.open()?;
        connection
            .query_row("SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, summary_status FROM sessions WHERE id=?1", [id.to_string()], session_from_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, summary_status FROM sessions ORDER BY last_event_at DESC, id DESC LIMIT ?1")?;
        let rows = statement.query_map([i64::try_from(limit)?], session_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn session_summary(&self, session_id: Uuid) -> Result<Option<EpisodicSummary>> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT summary_json FROM sessions WHERE id=?1",
                [session_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    pub fn latest_session(
        &self,
        client: &str,
        external_session_id: &str,
    ) -> Result<Option<SessionRecord>> {
        let connection = self.open()?;
        latest_session_tx(&connection, client, external_session_id)
    }

    pub fn recall_context(
        &self,
        client: &str,
        external_session_id: &str,
        project_id: Option<&str>,
    ) -> Result<Option<RecallContext>> {
        let connection = self.open()?;
        let Some(session) =
            latest_session_for_project(&connection, client, external_session_id, project_id)?
        else {
            return Ok(None);
        };
        Ok(Some(RecallContext {
            handoff: current_handoff_connection(&connection, project_id)?,
            session,
        }))
    }

    pub fn injection_identity(
        &self,
        client: &str,
        external_session_id: &str,
        project_id: Option<&str>,
    ) -> Result<InjectionIdentity> {
        let session = self.latest_session(client, external_session_id)?;
        Ok(session
            .filter(|value| value.project_id.as_deref() == project_id)
            .map(|value| InjectionIdentity {
                client: value.client,
                external_session_id: value.external_session_id,
                generation: value.generation,
            })
            .unwrap_or_else(|| InjectionIdentity {
                client: client.to_owned(),
                external_session_id: external_session_id.to_owned(),
                generation: 0,
            }))
    }

    pub fn jobs(&self) -> Result<Vec<JobRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key, owner, lease_started_at, lease_until, payload_json FROM jobs ORDER BY created_at")?;
        let rows = statement.query_map([], job_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn enqueue_job(
        &self,
        job_type: &str,
        dedupe_key: &str,
        payload_json: &str,
    ) -> Result<Uuid> {
        let connection = self.open()?;
        enqueue_job_connection(&connection, job_type, dedupe_key, payload_json)
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
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now_text = now.to_rfc3339();
        let lease_until = now + chrono::Duration::seconds(i64::try_from(lease_timeout_seconds)?);
        transaction.execute("UPDATE jobs SET status='pending', owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?1 WHERE status='running' AND lease_until <= ?1", [&now_text])?;
        let id: Option<String> = transaction.query_row("SELECT id FROM jobs WHERE status='pending' AND next_retry_at <= ?1 ORDER BY created_at LIMIT 1", [&now_text], |row| row.get(0)).optional()?;
        let Some(id) = id else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute("UPDATE jobs SET status='running', owner=?1, lease_started_at=?2, lease_until=?3, attempt_count=attempt_count+1, updated_at=?2 WHERE id=?4", params![owner, now_text, lease_until.to_rfc3339(), id])?;
        let job = transaction.query_row("SELECT id, job_type, status, attempt_count, next_retry_at, last_error, dedupe_key, owner, lease_started_at, lease_until, payload_json FROM jobs WHERE id=?1", [&id], job_from_row)?;
        transaction.commit()?;
        Ok(Some(job))
    }

    pub fn finish_job(
        &self,
        id: Uuid,
        owner: &str,
        provider: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let connection = self.open()?;
        let attempt: Option<u32> = connection
            .query_row(
                "SELECT attempt_count FROM jobs WHERE id=?1 AND status='running' AND owner=?2",
                params![id.to_string(), owner],
                |row| row.get(0),
            )
            .optional()?;
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let now = Utc::now();
        let (status, retry) = match error {
            None => ("completed", now),
            Some(_) if attempt < 5 => (
                "pending",
                now + chrono::Duration::seconds(2_i64.pow(attempt.min(10))),
            ),
            Some(_) => ("failed", now),
        };
        connection.execute("UPDATE jobs SET status=?1, next_retry_at=?2, last_error=?3, provider=?4, owner=NULL, lease_started_at=NULL, lease_until=NULL, updated_at=?5 WHERE id=?6 AND owner=?7", params![status, retry.to_rfc3339(), error, provider, now.to_rfc3339(), id.to_string(), owner])?;
        Ok(())
    }

    pub fn set_session_summary(
        &self,
        session_id: Uuid,
        status: SummaryStatus,
        summary_json: Option<&str>,
    ) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "UPDATE sessions SET summary_status=?1, summary_json=?2 WHERE id=?3",
            params![summary_status(status), summary_json, session_id.to_string()],
        )?;
        Ok(())
    }

    pub fn record_consolidation(
        &self,
        session_id: Uuid,
        result: &ConsolidationResult,
        execution: &ConsolidationExecution,
    ) -> Result<bool> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute("INSERT OR IGNORE INTO consolidation_results(session_id, result_json, execution_json, applied_at) VALUES (?1, ?2, ?3, ?4)", params![session_id.to_string(), serde_json::to_string(result)?, serde_json::to_string(execution)?, Utc::now().to_rfc3339()])?;
        if inserted == 1 {
            transaction.execute(
                "UPDATE sessions SET summary_status=?1, summary_json=?2 WHERE id=?3",
                params![
                    summary_status(SummaryStatus::Ready),
                    serde_json::to_string(&result.summary)?,
                    session_id.to_string()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(inserted == 1)
    }

    pub fn consolidation_result(&self, session_id: Uuid) -> Result<Option<ConsolidationMarker>> {
        let connection = self.open()?;
        connection.query_row("SELECT session_id, result_json, execution_json, applied_at FROM consolidation_results WHERE session_id=?1", [session_id.to_string()], |row| {
            let id: String = row.get(0)?;
            Ok(ConsolidationMarker { session_id: Uuid::parse_str(&id).map_err(sql_error)?, result: serde_json::from_str(&row.get::<_, String>(1)?).map_err(sql_error)?, execution: serde_json::from_str(&row.get::<_, String>(2)?).map_err(sql_error)?, applied_at: parse_timestamp(&row.get::<_, String>(3)?).map_err(sql_error)? })
        }).optional().map_err(Into::into)
    }

    pub fn upsert_handoff_item(&self, item: &HandoffItem) -> Result<()> {
        validate_handoff_item(item)?;
        if serde_json::to_vec(item)?.len() > MAX_HANDOFF_TOTAL_BYTES {
            bail!("handoff item exceeds {MAX_HANDOFF_TOTAL_BYTES} bytes")
        }
        let connection = self.open()?;
        let key = project_key(item.project_id.as_deref());
        connection.execute("INSERT INTO handoff_items(id, project_key, project_id, kind, state, next_step, blocker, low_confidence, last_confirmed_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT(id) DO UPDATE SET project_key=excluded.project_key, project_id=excluded.project_id, kind=excluded.kind, state=excluded.state, next_step=excluded.next_step, blocker=excluded.blocker, low_confidence=excluded.low_confidence, last_confirmed_at=excluded.last_confirmed_at, updated_at=excluded.updated_at", params![item.id.to_string(), key, item.project_id, handoff_kind(item.kind), item.state, item.next_step, item.blocker, item.low_confidence, item.last_confirmed_at.to_rfc3339(), item.created_at.to_rfc3339(), item.updated_at.to_rfc3339()])?;
        connection.execute(
            "DELETE FROM handoff_item_sources WHERE item_id=?1",
            [item.id.to_string()],
        )?;
        for source in &item.sources {
            connection.execute("INSERT INTO handoff_item_sources(item_id, session_id, event_ids_json) VALUES (?1, ?2, ?3)", params![item.id.to_string(), source.session_id.to_string(), serde_json::to_string(&source.event_ids)?])?;
        }
        Ok(())
    }

    pub fn current_handoff(&self, project_id: Option<&str>) -> Result<Vec<HandoffItem>> {
        let connection = self.open()?;
        current_handoff_connection(&connection, project_id)
    }

    pub fn remove_handoff_item(&self, item_id: Uuid) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "DELETE FROM handoff_items WHERE id=?1",
            [item_id.to_string()],
        )?;
        Ok(())
    }

    pub fn claim_delivery(
        &self,
        identity: &InjectionIdentity,
        content_kind: &str,
        content_id: &str,
    ) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute("INSERT OR IGNORE INTO delivery_claims(client, external_session_id, generation, content_kind, content_id, claimed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![identity.client, identity.external_session_id, identity.generation, content_kind, content_id, Utc::now().to_rfc3339()])? == 1)
    }

    pub fn integrations(&self) -> Result<Vec<IntegrationRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT client, connected, mcp_registered, hook_status, last_event_at, details_json FROM integration_state ORDER BY client")?;
        let rows = statement.query_map([], |row| {
            Ok(IntegrationRecord {
                client: row.get(0)?,
                connected: row.get(1)?,
                mcp_registered: row.get(2)?,
                hook_status: row.get(3)?,
                last_event_at: parse_optional_timestamp(row.get(4)?).map_err(sql_error)?,
                details_json: row.get(5)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn set_integration_connected(&self, client: &str, connected: bool) -> Result<()> {
        let connection = self.open()?;
        connection.execute("INSERT INTO integration_state(client, connected, mcp_registered, hook_status, details_json) VALUES (?1, ?2, ?2, ?3, '{}') ON CONFLICT(client) DO UPDATE SET connected=excluded.connected, mcp_registered=excluded.mcp_registered, hook_status=excluded.hook_status", params![client, connected, if connected { "installed" } else { "removed" }])?;
        Ok(())
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
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn clear_orphan(&self, client: &str, external_session_id: &str) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "DELETE FROM orphan_sessions WHERE client=?1 AND external_session_id=?2",
            params![client, external_session_id],
        )?;
        connection.execute(
            "DELETE FROM imports WHERE client=?1 AND external_session_id=?2",
            params![client, external_session_id],
        )?;
        Ok(())
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

    pub fn record_import(
        &self,
        client: &str,
        external_session_id: &str,
        status: &str,
        orphan_payload: Option<&str>,
    ) -> Result<()> {
        let connection = self.open()?;
        let now = Utc::now().to_rfc3339();
        connection.execute("INSERT INTO imports(id, client, external_session_id, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(client, external_session_id) DO UPDATE SET status=excluded.status", params![Uuid::now_v7().to_string(), client, external_session_id, status, now])?;
        if let Some(payload) = orphan_payload {
            connection.execute("INSERT INTO orphan_sessions(client, external_session_id, payload_json, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(client, external_session_id) DO UPDATE SET payload_json=excluded.payload_json, created_at=excluded.created_at", params![client, external_session_id, payload, now])?;
        }
        Ok(())
    }

    pub fn mark_latest_session_imported(
        &self,
        client: &str,
        external_session_id: &str,
    ) -> Result<()> {
        let connection = self.open()?;
        connection.execute("UPDATE sessions SET imported=1 WHERE id=(SELECT id FROM sessions WHERE client=?1 AND external_session_id=?2 ORDER BY generation DESC LIMIT 1)", params![client, external_session_id])?;
        Ok(())
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

    pub fn record_application(
        &self,
        memory_id: Uuid,
        session_id: Uuid,
        successful: bool,
    ) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute("INSERT OR IGNORE INTO application_events(id, memory_id, session_id, successful, created_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![Uuid::now_v7().to_string(), memory_id.to_string(), session_id.to_string(), successful, Utc::now().to_rfc3339()])? == 1)
    }

    pub fn access_counts(&self, memory_id: Uuid) -> Result<Vec<(String, u64)>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT signal, COUNT(*) FROM access_events WHERE memory_id=?1 GROUP BY signal ORDER BY signal")?;
        let rows = statement.query_map([memory_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn meaningful_access(&self, memory_id: Uuid) -> Result<(u64, Option<DateTime<Utc>>)> {
        let connection = self.open()?;
        let (count, latest): (u64, Option<String>) = connection.query_row(
            "SELECT COUNT(*), MAX(created_at) FROM access_events WHERE memory_id=?1 AND signal IN ('explicitly_read', 'successfully_applied')",
            [memory_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((count, latest.as_deref().map(parse_timestamp).transpose()?))
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

    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        reject_unversioned_database(&connection)?;
        connection.execute_batch(SCHEMA)?;
        connection.execute("INSERT OR IGNORE INTO schema_meta(version) VALUES (1)", [])?;
        Ok(connection)
    }
}

fn reject_unversioned_database(connection: &Connection) -> Result<()> {
    let has_schema_meta = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_meta'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if has_schema_meta {
        let version: Option<i64> = connection
            .query_row("SELECT version FROM schema_meta LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        if version != Some(1) {
            bail!("unsupported state schema version")
        }
        return Ok(());
    }
    let user_table_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if user_table_count != 0 {
        bail!("unversioned state database requires recreation")
    }
    Ok(())
}

fn create_session(
    connection: &rusqlite::Transaction<'_>,
    event: &NormalizedEvent,
    project_id: Option<&str>,
    generation: u32,
) -> Result<SessionRecord> {
    let id = Uuid::now_v7();
    let timestamp = event.timestamp.to_rfc3339();
    connection.execute("INSERT INTO sessions(id, client, external_session_id, project_id, generation, state, started_at, last_event_at) VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?6)", params![id.to_string(), event.client, event.external_session_id, project_id, generation, timestamp])?;
    session_by_id(connection, id)
}

fn latest_session_tx(
    connection: &Connection,
    client: &str,
    external_session_id: &str,
) -> Result<Option<SessionRecord>> {
    connection.query_row("SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, summary_status FROM sessions WHERE client=?1 AND external_session_id=?2 ORDER BY generation DESC LIMIT 1", params![client, external_session_id], session_from_row).optional().map_err(Into::into)
}

fn latest_session_for_project(
    connection: &Connection,
    client: &str,
    external_session_id: &str,
    project_id: Option<&str>,
) -> Result<Option<SessionRecord>> {
    connection.query_row("SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, summary_status FROM sessions WHERE client=?1 AND external_session_id=?2 AND project_id IS ?3 ORDER BY generation DESC LIMIT 1", params![client, external_session_id, project_id], session_from_row).optional().map_err(Into::into)
}

fn session_by_id(connection: &Connection, id: Uuid) -> Result<SessionRecord> {
    connection.query_row("SELECT id, client, external_session_id, project_id, generation, state, started_at, ended_at, last_event_at, markdown_path, imported, summary_status FROM sessions WHERE id=?1", [id.to_string()], session_from_row).map_err(Into::into)
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        client: row.get(1)?,
        external_session_id: row.get(2)?,
        project_id: row.get(3)?,
        generation: row.get(4)?,
        state: parse_session_state(&row.get::<_, String>(5)?)?,
        started_at: parse_timestamp(&row.get::<_, String>(6)?)?,
        ended_at: parse_optional_timestamp(row.get(7)?)?,
        last_event_at: parse_timestamp(&row.get::<_, String>(8)?)?,
        markdown_path: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
        imported: row.get(10)?,
        summary_status: parse_summary_status(&row.get::<_, String>(11)?)?,
    })
}

fn event_session_id(
    connection: &rusqlite::Transaction<'_>,
    event_id: &str,
) -> Result<Option<Uuid>> {
    let value: Option<String> = connection
        .query_row(
            "SELECT session_id FROM session_events WHERE event_id=?1",
            [event_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value
        .map(|id| Uuid::parse_str(&id).map_err(anyhow::Error::new))
        .transpose()
}

fn current_handoff_connection(
    connection: &Connection,
    project_id: Option<&str>,
) -> Result<Vec<HandoffItem>> {
    let key = project_key(project_id);
    let mut statement = connection.prepare("SELECT id, project_id, kind, state, next_step, blocker, low_confidence, last_confirmed_at, created_at, updated_at FROM handoff_items WHERE project_key=?1 ORDER BY updated_at DESC, id LIMIT ?2")?;
    let rows = statement.query_map(
        params![key, i64::try_from(MAX_HANDOFF_LIST_LIMIT)?],
        |row| handoff_item_from_row(connection, row),
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn handoff_item_from_row(
    connection: &Connection,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<HandoffItem> {
    let id = parse_uuid(row.get::<_, String>(0)?)?;
    let mut source_statement = connection.prepare("SELECT session_id, event_ids_json FROM handoff_item_sources WHERE item_id=?1 ORDER BY session_id").map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let sources = source_statement
        .query_map([id.to_string()], |source| {
            Ok(HandoffItemSource {
                session_id: parse_uuid(source.get::<_, String>(0)?)?,
                event_ids: serde_json::from_str(&source.get::<_, String>(1)?).map_err(sql_error)?,
            })
        })
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(HandoffItem {
        id,
        project_id: row.get(1)?,
        kind: parse_handoff_kind(&row.get::<_, String>(2)?)?,
        state: row.get(3)?,
        next_step: row.get(4)?,
        blocker: row.get(5)?,
        low_confidence: row.get(6)?,
        last_confirmed_at: parse_timestamp(&row.get::<_, String>(7)?)?,
        sources,
        created_at: parse_timestamp(&row.get::<_, String>(8)?)?,
        updated_at: parse_timestamp(&row.get::<_, String>(9)?)?,
    })
}

fn enqueue_job_tx(
    connection: &rusqlite::Transaction<'_>,
    job_type: &str,
    dedupe_key: &str,
    payload: &str,
) -> Result<Uuid> {
    enqueue_job_connection(connection, job_type, dedupe_key, payload)
}

fn enqueue_job_connection(
    connection: &Connection,
    job_type: &str,
    dedupe_key: &str,
    payload: &str,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let now = Utc::now().to_rfc3339();
    connection.execute("INSERT OR IGNORE INTO jobs(id, job_type, dedupe_key, status, payload_json, next_retry_at, created_at, updated_at) VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?5, ?5)", params![id.to_string(), job_type, dedupe_key, payload, now])?;
    let value: String = connection.query_row(
        "SELECT id FROM jobs WHERE job_type=?1 AND dedupe_key=?2",
        params![job_type, dedupe_key],
        |row| row.get::<_, String>(0),
    )?;
    Ok(Uuid::parse_str(&value)?)
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    Ok(JobRecord {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        job_type: row.get(1)?,
        status: row.get(2)?,
        attempt_count: row.get(3)?,
        next_retry_at: parse_timestamp(&row.get::<_, String>(4)?)?,
        last_error: row.get(5)?,
        dedupe_key: row.get(6)?,
        owner: row.get(7)?,
        lease_started_at: parse_optional_timestamp(row.get(8)?)?,
        lease_until: parse_optional_timestamp(row.get(9)?)?,
        payload_json: row.get(10)?,
    })
}

fn validate_handoff_item(item: &HandoffItem) -> Result<()> {
    if item.state.trim().is_empty() {
        bail!("handoff item state cannot be empty")
    }
    if item.sources.len() > MAX_HANDOFF_SOURCE_EVENTS {
        bail!("handoff item has too many sources")
    }
    for source in &item.sources {
        if source.event_ids.len() > MAX_HANDOFF_SOURCE_EVENTS {
            bail!("handoff source has too many events")
        }
    }
    Ok(())
}

fn project_key(project_id: Option<&str>) -> String {
    project_id.unwrap_or(GLOBAL_HANDOFF_KEY).to_owned()
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
fn state_after_event(state: SessionState, kind: NormalizedEventKind) -> SessionState {
    match kind {
        NormalizedEventKind::SessionEnded => SessionState::Finalized,
        NormalizedEventKind::TurnStopped => SessionState::Idle,
        _ if state == SessionState::Idle => SessionState::Open,
        _ => state,
    }
}
fn session_state(state: SessionState) -> &'static str {
    match state {
        SessionState::Open => "open",
        SessionState::Idle => "idle",
        SessionState::Finalized => "finalized",
    }
}
fn parse_session_state(value: &str) -> rusqlite::Result<SessionState> {
    match value {
        "open" => Ok(SessionState::Open),
        "idle" => Ok(SessionState::Idle),
        "finalized" => Ok(SessionState::Finalized),
        _ => Err(sql_error(anyhow::anyhow!("invalid session state: {value}"))),
    }
}
fn summary_status(status: SummaryStatus) -> &'static str {
    match status {
        SummaryStatus::Pending => "pending",
        SummaryStatus::Ready => "ready",
        SummaryStatus::Skipped => "skipped",
    }
}
fn parse_summary_status(value: &str) -> rusqlite::Result<SummaryStatus> {
    match value {
        "pending" => Ok(SummaryStatus::Pending),
        "ready" => Ok(SummaryStatus::Ready),
        "skipped" => Ok(SummaryStatus::Skipped),
        _ => Err(sql_error(anyhow::anyhow!(
            "invalid summary status: {value}"
        ))),
    }
}
fn handoff_kind(kind: HandoffItemKind) -> &'static str {
    match kind {
        HandoffItemKind::InProgress => "in-progress",
        HandoffItemKind::OpenQuestion => "open-question",
        HandoffItemKind::Parked => "parked",
        HandoffItemKind::Blocked => "blocked",
    }
}
fn parse_handoff_kind(value: &str) -> rusqlite::Result<HandoffItemKind> {
    match value {
        "in-progress" => Ok(HandoffItemKind::InProgress),
        "open-question" => Ok(HandoffItemKind::OpenQuestion),
        "parked" => Ok(HandoffItemKind::Parked),
        "blocked" => Ok(HandoffItemKind::Blocked),
        _ => Err(sql_error(anyhow::anyhow!(
            "invalid handoff item kind: {value}"
        ))),
    }
}
fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(sql_error)
}
fn parse_timestamp(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(sql_error)
}
fn parse_optional_timestamp(value: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value.as_deref().map(parse_timestamp).transpose()
}
fn sql_error(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

pub fn conversation_key(client: &str, external_session_id: &str) -> String {
    format!("{client}:{external_session_id}")
}
