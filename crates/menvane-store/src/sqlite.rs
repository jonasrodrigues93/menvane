use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use menvane_domain::{
    Applicability, EpisodicSummary, KnowledgeType, Memory, MemoryStatus, Project, RelatedSummary,
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::MarkdownStore;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_meta (
    version INTEGER PRIMARY KEY CHECK (version = 1)
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    identity TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    scope TEXT NOT NULL,
    project_id TEXT,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    applicability_json TEXT NOT NULL,
    tags_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    source_sessions_json TEXT NOT NULL DEFAULT '[]',
    supersedes_json TEXT NOT NULL DEFAULT '[]',
    last_verified_at TEXT,
    successes INTEGER,
    failures INTEGER,
    source_project_ids_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS memories_scope_project ON memories(scope, project_id);
CREATE INDEX IF NOT EXISTS memories_status ON memories(status);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    id UNINDEXED,
    title,
    body,
    tags,
    applicability,
    tokenize = 'unicode61'
);
CREATE TABLE IF NOT EXISTS memory_embeddings (
    memory_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    embedding BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(memory_id, provider, model)
);
CREATE TABLE IF NOT EXISTS session_summaries (
    session_id TEXT PRIMARY KEY,
    project_id TEXT,
    ended_at TEXT,
    summary_json TEXT NOT NULL,
    selection_text TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS session_summaries_project ON session_summaries(project_id, ended_at);
CREATE VIRTUAL TABLE IF NOT EXISTS session_summary_fts USING fts5(
    session_id UNINDEXED,
    selection_text,
    tokenize = 'unicode61'
);
"#;

#[derive(Debug, Clone, Copy)]
pub enum SearchScope<'a> {
    Auto(&'a str),
    Project(&'a str),
    Global,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: Uuid,
    pub knowledge_type: KnowledgeType,
    pub scope: String,
    pub title: String,
    pub status: String,
    pub applicability: Applicability,
    pub excerpt: String,
    pub score: f64,
    pub fts_rank: usize,
    pub age_days: f64,
    pub source_session_count: usize,
    pub supersession_count: usize,
}

#[derive(Debug, Clone)]
pub struct IndexStore {
    path: PathBuf,
}

pub const MAX_SUMMARY_SELECTION_SESSIONS: usize = 24;
pub const MAX_SUMMARY_SELECTION_BYTES: usize = 24_000;

impl IndexStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn initialize(&self) -> Result<()> {
        let connection = self.open()?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        reject_unversioned_database(&connection)?;
        connection.execute_batch(SCHEMA)?;
        ensure_memory_columns(&connection)?;
        connection.execute("INSERT OR IGNORE INTO schema_meta(version) VALUES (1)", [])?;
        Ok(())
    }

    pub fn upsert_project(&self, project: &Project, path: &Path) -> Result<()> {
        let connection = self.open_initialized()?;
        insert_project(&connection, project, path)
    }

    pub fn upsert_memory(&self, memory: &Memory, path: &Path) -> Result<()> {
        let mut connection = self.open_initialized()?;
        let transaction = connection.transaction()?;
        insert_memory(&transaction, memory, path)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_session_summary(
        &self,
        session_id: Uuid,
        project_id: Option<&str>,
        ended_at: Option<DateTime<Utc>>,
        summary: &EpisodicSummary,
    ) -> Result<()> {
        let mut connection = self.open_initialized()?;
        let transaction = connection.transaction()?;
        let summary_json = serde_json::to_string(summary)?;
        let selection_text = summary_selection_text(summary);
        transaction.execute(
            "DELETE FROM session_summary_fts WHERE session_id=?1",
            [session_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO session_summaries(session_id, project_id, ended_at, summary_json, selection_text, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id) DO UPDATE SET project_id=excluded.project_id, ended_at=excluded.ended_at, summary_json=excluded.summary_json, selection_text=excluded.selection_text, updated_at=excluded.updated_at",
            params![
                session_id.to_string(),
                project_id,
                ended_at.map(|value| value.to_rfc3339()),
                summary_json,
                selection_text,
                Utc::now().to_rfc3339(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_summary_fts(session_id, selection_text) VALUES (?1, ?2)",
            params![session_id.to_string(), summary_selection_text(summary)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn related_summaries(
        &self,
        project_id: Option<&str>,
        source_sessions: &[Uuid],
        query: &str,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<RelatedSummary>> {
        let connection = self.open_initialized()?;
        let limit = limit.min(MAX_SUMMARY_SELECTION_SESSIONS);
        let max_bytes = max_bytes.min(MAX_SUMMARY_SELECTION_BYTES);
        let fts_query = fts_query(query, false);
        let scope = project_id.map_or_else(
            || "s.project_id IS NULL".to_owned(),
            |_| "s.project_id = ?2".to_owned(),
        );
        let sql = if fts_query.is_empty() {
            format!(
                "SELECT s.session_id, s.ended_at, s.summary_json, 0 FROM session_summaries s WHERE 0 AND {scope} LIMIT ?3"
            )
        } else {
            format!(
                "SELECT s.session_id, s.ended_at, s.summary_json, bm25(session_summary_fts) FROM session_summary_fts JOIN session_summaries s ON s.session_id=session_summary_fts.session_id WHERE session_summary_fts MATCH ?1 AND {scope} ORDER BY bm25(session_summary_fts), s.ended_at DESC, s.session_id LIMIT ?3"
            )
        };
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![
                fts_query,
                project_id,
                i64::try_from(MAX_SUMMARY_SELECTION_SESSIONS)?
            ],
            |row| {
                let session_id: String = row.get(0)?;
                let ended_at: Option<String> = row.get(1)?;
                let summary_json: String = row.get(2)?;
                let rank: f64 = row.get(3)?;
                Ok((
                    Uuid::parse_str(&session_id).map_err(sql_error)?,
                    ended_at.as_deref().map(parse_timestamp).transpose()?,
                    serde_json::from_str::<EpisodicSummary>(&summary_json).map_err(sql_error)?,
                    rank,
                ))
            },
        )?;
        let mut candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for source_session in source_sessions {
            if candidates
                .iter()
                .any(|candidate| candidate.0 == *source_session)
            {
                continue;
            }
            if let Some(candidate) = connection
                .query_row(
                    "SELECT session_id, ended_at, summary_json, 0 FROM session_summaries WHERE session_id=?1 AND project_id IS ?2",
                    params![source_session.to_string(), project_id],
                    |row| {
                        let session_id: String = row.get(0)?;
                        Ok((
                            Uuid::parse_str(&session_id).map_err(sql_error)?,
                            row.get::<_, Option<String>>(1)?.as_deref().map(parse_timestamp).transpose()?,
                            serde_json::from_str::<EpisodicSummary>(&row.get::<_, String>(2)?).map_err(sql_error)?,
                            0.0,
                        ))
                    },
                )
                .optional()?
            {
                candidates.push(candidate);
            }
        }
        candidates.sort_by(|left, right| {
            let left_source = source_sessions.contains(&left.0);
            let right_source = source_sessions.contains(&right.0);
            right_source
                .cmp(&left_source)
                .then_with(|| left.3.total_cmp(&right.3))
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.0.cmp(&right.0))
        });
        let mut total = 0;
        let mut selected = Vec::new();
        for (session_id, ended_at, summary, _) in candidates.into_iter().take(limit) {
            let bytes = serde_json::to_vec(&summary)?.len();
            if selected.is_empty() && bytes > max_bytes {
                continue;
            }
            if total + bytes > max_bytes {
                break;
            }
            total += bytes;
            selected.push(RelatedSummary {
                session_id,
                ended_at,
                summary,
            });
        }
        Ok(selected)
    }

    pub fn read_memory(&self, markdown: &MarkdownStore, id: Uuid) -> Result<(Memory, PathBuf)> {
        let connection = self.open_initialized()?;
        let path: Option<String> = connection
            .query_row(
                "SELECT path FROM memories WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let path = path.with_context(|| format!("memory not found: {id}"))?;
        let path = PathBuf::from(path);
        Ok((markdown.parse_memory(&path)?, path))
    }

    pub fn search(
        &self,
        query: &str,
        scope: SearchScope<'_>,
        limit: usize,
        _include_sessions: bool,
        match_all_terms: bool,
    ) -> Result<Vec<SearchResult>> {
        let fts_query = fts_query(query, match_all_terms);
        if fts_query.is_empty() {
            bail!("search query must contain letters or numbers");
        }
        let connection = self.open_initialized()?;
        let (scope_sql, project_id) = match scope {
            SearchScope::Auto(project_id) => {
                ("(m.scope = 'global' OR m.project_id = ?2)", project_id)
            }
            SearchScope::Project(project_id) => ("m.project_id = ?2", project_id),
            SearchScope::Global => ("m.scope = 'global'", ""),
        };
        let sql = format!(
            "SELECT m.id, m.type, m.scope, m.title, m.status, m.applicability_json, m.source_sessions_json, m.supersedes_json, snippet(memory_fts, 2, '', '', ' ... ', 24), -bm25(memory_fts) AS score, MAX(0, julianday('now') - julianday(m.updated_at))
             FROM memory_fts
             JOIN memories m ON m.id = memory_fts.id
              WHERE memory_fts MATCH ?1 AND {scope_sql} AND m.status IN ('active', 'candidate')
              ORDER BY CASE m.status WHEN 'active' THEN 0 ELSE 1 END, score DESC
             LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![fts_query, project_id, i64::try_from(limit)?,],
            |row| {
                let id: String = row.get(0)?;
                Ok(SearchResult {
                    id: Uuid::parse_str(&id).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            id.len(),
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    knowledge_type: row.get::<_, String>(1)?.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    scope: row.get(2)?,
                    title: row.get(3)?,
                    status: row.get(4)?,
                    applicability: serde_json::from_str(&row.get::<_, String>(5)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    source_session_count: json_array_count(&row.get::<_, String>(6)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    supersession_count: json_array_count(&row.get::<_, String>(7)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    excerpt: row.get(8)?,
                    score: row.get(9)?,
                    fts_rank: 0,
                    age_days: row.get(10)?,
                })
            },
        )?;
        rows.enumerate()
            .map(|(index, row)| {
                let mut result = row?;
                result.fts_rank = index + 1;
                Ok(result)
            })
            .collect::<Result<Vec<_>, rusqlite::Error>>()
            .map_err(Into::into)
    }

    pub fn list(
        &self,
        scope: SearchScope<'_>,
        limit: usize,
        _include_sessions: bool,
    ) -> Result<Vec<SearchResult>> {
        let connection = self.open_initialized()?;
        let (scope_sql, project_id) = match scope {
            SearchScope::Auto(project_id) => ("(scope = 'global' OR project_id = ?1)", project_id),
            SearchScope::Project(project_id) => ("project_id = ?1", project_id),
            SearchScope::Global => ("scope = 'global'", ""),
        };
        let sql = format!(
            "SELECT id, type, scope, title, status, applicability_json, source_sessions_json, supersedes_json, substr(body, 1, 500), 0.0, MAX(0, julianday('now') - julianday(updated_at))
             FROM memories
             WHERE {scope_sql} AND status != 'forgotten'
             ORDER BY updated_at DESC
             LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![project_id, i64::try_from(limit)?], |row| {
            let id: String = row.get(0)?;
            Ok(SearchResult {
                id: Uuid::parse_str(&id).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        id.len(),
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                knowledge_type: row.get::<_, String>(1)?.parse().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                scope: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                applicability: serde_json::from_str(&row.get::<_, String>(5)?).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    },
                )?,
                source_session_count: json_array_count(&row.get::<_, String>(6)?).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    },
                )?,
                supersession_count: json_array_count(&row.get::<_, String>(7)?).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    },
                )?,
                excerpt: row.get(8)?,
                score: row.get(9)?,
                fts_rank: 0,
                age_days: row.get(10)?,
            })
        })?;
        rows.enumerate()
            .map(|(index, row)| {
                let mut result = row?;
                result.fts_rank = index + 1;
                Ok(result)
            })
            .collect::<Result<Vec<_>, rusqlite::Error>>()
            .map_err(Into::into)
    }

    pub fn memory_count(&self) -> Result<u64> {
        let connection = self.open_initialized()?;
        Ok(connection.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?)
    }

    pub fn project_count(&self) -> Result<u64> {
        let connection = self.open_initialized()?;
        Ok(connection.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?)
    }

    pub fn fts5_available(&self) -> Result<bool> {
        let connection = self.open_initialized()?;
        let result = connection.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS menvane_fts_check USING fts5(value)",
            [],
        );
        if result.is_ok() {
            connection.execute("DROP TABLE menvane_fts_check", [])?;
        }
        Ok(result.is_ok())
    }

    pub fn reindex(&self, markdown: &MarkdownStore) -> Result<(usize, usize)> {
        let temporary = self
            .path
            .with_extension(format!("reindex-{}", Uuid::now_v7()));
        let connection = Connection::open(&temporary)?;
        configure_connection(&connection, false)?;
        reject_unversioned_database(&connection)?;
        connection.execute_batch(SCHEMA)?;
        ensure_memory_columns(&connection)?;
        connection.execute("INSERT OR IGNORE INTO schema_meta(version) VALUES (1)", [])?;
        let project_files = markdown.project_files()?;
        for path in &project_files {
            let project = markdown.parse_project(path)?;
            insert_project(&connection, &project, path)?;
        }
        let memory_files = markdown.memory_files()?;
        for path in &memory_files {
            let memory = markdown.parse_memory(path)?;
            insert_memory(&connection, &memory, path)?;
        }
        for path in &markdown.session_files()? {
            let session = markdown.parse_session(path)?;
            if let Some(summary) = session.metadata.summary {
                insert_session_summary(
                    &connection,
                    session.metadata.id,
                    session.metadata.project_id.as_deref(),
                    session.metadata.ended_at,
                    &summary,
                )?;
            }
        }
        connection.execute_batch("PRAGMA optimize;")?;
        drop(connection);
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", self.path.display(), suffix));
            if sidecar.exists() {
                fs::remove_file(sidecar)?;
            }
        }
        fs::rename(&temporary, &self.path).with_context(|| {
            format!(
                "failed to replace {} with rebuilt index",
                self.path.display()
            )
        })?;
        self.initialize()?;
        Ok((project_files.len(), memory_files.len()))
    }

    pub fn backup(&self, destination: &Path) -> Result<()> {
        let source = self.open_initialized()?;
        let mut destination = Connection::open(destination)?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
        backup.run_to_completion(128, std::time::Duration::from_millis(10), None)?;
        Ok(())
    }

    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        configure_connection(&connection, true)?;
        Ok(connection)
    }

    fn open_initialized(&self) -> Result<Connection> {
        self.open()
    }
}

fn configure_connection(connection: &Connection, wal: bool) -> Result<()> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    if !wal {
        connection.execute_batch("PRAGMA journal_mode = DELETE;")?;
    }
    Ok(())
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
            bail!("unsupported index schema version")
        }
        return Ok(());
    }
    let user_table_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if user_table_count != 0 {
        bail!("unversioned index database requires recreation")
    }
    Ok(())
}

fn json_array_count(value: &str) -> serde_json::Result<usize> {
    serde_json::from_str::<Vec<serde_json::Value>>(value).map(|values| values.len())
}

fn insert_project(connection: &Connection, project: &Project, path: &Path) -> Result<()> {
    connection.execute(
        "INSERT INTO projects(id, identity, name, path, metadata_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET identity=excluded.identity, name=excluded.name, path=excluded.path, metadata_json=excluded.metadata_json, updated_at=excluded.updated_at",
        params![
            project.id,
            project.identity,
            project.name,
            path.to_string_lossy(),
            serde_json::to_string(project)?,
            project.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn insert_memory(connection: &Connection, memory: &Memory, path: &Path) -> Result<()> {
    let metadata = &memory.metadata;
    connection.execute(
        "DELETE FROM memory_fts WHERE id = ?1",
        [metadata.id.to_string()],
    )?;
    connection.execute(
            "INSERT INTO memories(id, type, scope, project_id, title, status, path, body, applicability_json, tags_json, created_at, updated_at, source_sessions_json, supersedes_json, last_verified_at, successes, failures, source_project_ids_json)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
           ON CONFLICT(id) DO UPDATE SET type=excluded.type, scope=excluded.scope, project_id=excluded.project_id, title=excluded.title, status=excluded.status, path=excluded.path, body=excluded.body, applicability_json=excluded.applicability_json, tags_json=excluded.tags_json, updated_at=excluded.updated_at, source_sessions_json=excluded.source_sessions_json, supersedes_json=excluded.supersedes_json, last_verified_at=excluded.last_verified_at, successes=excluded.successes, failures=excluded.failures, source_project_ids_json=excluded.source_project_ids_json",
        params![
            metadata.id.to_string(),
            metadata.knowledge_type.to_string(),
            metadata.scope.to_string(),
            metadata.project_id,
            memory.title,
            metadata.status.to_string(),
            path.to_string_lossy(),
            memory.body,
            serde_json::to_string(&metadata.applies_to)?,
            serde_json::to_string(&metadata.tags)?,
            metadata.created_at.to_rfc3339(),
            metadata.updated_at.to_rfc3339(),
            serde_json::to_string(&metadata.source_sessions)?,
            serde_json::to_string(&metadata.supersedes)?,
            metadata.last_verified_at.map(|value| value.to_rfc3339()),
            metadata.successes,
            metadata.failures,
            serde_json::to_string(&metadata.source_project_ids)?,
        ],
    )?;
    connection.execute(
        "INSERT INTO memory_fts(id, title, body, tags, applicability) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            metadata.id.to_string(),
            memory.title,
            memory.body,
            metadata.tags.join(" "),
            [
                metadata.applies_to.languages.join(" "),
                metadata.applies_to.frameworks.join(" "),
                metadata.applies_to.tools.join(" "),
                metadata.applies_to.databases.join(" "),
                metadata.applies_to.platforms.join(" "),
            ]
            .join(" "),
        ],
    )?;
    Ok(())
}

fn ensure_memory_columns(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(memories)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, definition) in [
        ("last_verified_at", "TEXT"),
        ("successes", "INTEGER"),
        ("failures", "INTEGER"),
        ("source_project_ids_json", "TEXT NOT NULL DEFAULT '[]'"),
    ] {
        if !columns.iter().any(|column| column == name) {
            connection.execute(
                &format!("ALTER TABLE memories ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn insert_session_summary(
    connection: &Connection,
    session_id: Uuid,
    project_id: Option<&str>,
    ended_at: Option<DateTime<Utc>>,
    summary: &EpisodicSummary,
) -> Result<()> {
    connection.execute(
        "DELETE FROM session_summary_fts WHERE session_id=?1",
        [session_id.to_string()],
    )?;
    let selection_text = summary_selection_text(summary);
    connection.execute(
        "INSERT INTO session_summaries(session_id, project_id, ended_at, summary_json, selection_text, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id) DO UPDATE SET project_id=excluded.project_id, ended_at=excluded.ended_at, summary_json=excluded.summary_json, selection_text=excluded.selection_text, updated_at=excluded.updated_at",
        params![
            session_id.to_string(),
            project_id,
            ended_at.map(|value| value.to_rfc3339()),
            serde_json::to_string(summary)?,
            selection_text,
            Utc::now().to_rfc3339(),
        ],
    )?;
    connection.execute(
        "INSERT INTO session_summary_fts(session_id, selection_text) VALUES (?1, ?2)",
        params![session_id.to_string(), summary_selection_text(summary)],
    )?;
    Ok(())
}

fn summary_selection_text(summary: &EpisodicSummary) -> String {
    summary
        .intentions
        .iter()
        .chain(&summary.actions)
        .chain(std::iter::once(&summary.result))
        .chain(summary.continuity.iter().map(|item| &item.front))
        .chain(&summary.candidate_learnings)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_timestamp(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(sql_error)
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

fn fts_query(query: &str, match_all_terms: bool) -> String {
    query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(if match_all_terms { " AND " } else { " OR " })
}

pub fn mark_forgotten(memory: &mut Memory) {
    memory.metadata.status = MemoryStatus::Forgotten;
    memory.metadata.updated_at = chrono::Utc::now();
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use menvane_domain::{
        Applicability, EpisodicSummary, KnowledgeType, MemoryMetadata, MemoryStatus, Scope,
        SummaryOutcome,
    };
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn forgotten_memories_are_not_searchable() {
        let temporary = TempDir::new().unwrap();
        let markdown = MarkdownStore::new(temporary.path());
        markdown.initialize().unwrap();
        let index = IndexStore::new(temporary.path().join("index.sqlite"));
        index.initialize().unwrap();
        let mut memory = Memory {
            metadata: MemoryMetadata::new(
                KnowledgeType::Context,
                Scope::Global,
                None,
                Vec::new(),
                Applicability::default(),
                MemoryStatus::Active,
            ),
            title: "Durable rust context".to_owned(),
            body: "SQLite is derived.".to_owned(),
        };
        let path = markdown.write_memory(&memory, None).unwrap();
        index.upsert_memory(&memory, &path).unwrap();
        assert_eq!(
            index
                .search("durable", SearchScope::Global, 10, false, true)
                .unwrap()
                .len(),
            1
        );
        mark_forgotten(&mut memory);
        markdown.update_memory(&path, &memory).unwrap();
        index.upsert_memory(&memory, &path).unwrap();
        assert!(
            index
                .search("durable", SearchScope::Global, 10, false, true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_result_exposes_durable_provenance_counts() {
        let temporary = TempDir::new().unwrap();
        let markdown = MarkdownStore::new(temporary.path());
        markdown.initialize().unwrap();
        let index = IndexStore::new(temporary.path().join("index.sqlite"));
        index.initialize().unwrap();
        let mut memory = Memory {
            metadata: MemoryMetadata::new(
                KnowledgeType::Context,
                Scope::Global,
                None,
                Vec::new(),
                Applicability::default(),
                MemoryStatus::Active,
            ),
            title: "Provenance marker".to_owned(),
            body: "Durable provenance search content".to_owned(),
        };
        memory.metadata.source_sessions = vec![Uuid::now_v7(), Uuid::now_v7()];
        memory.metadata.supersedes = vec![Uuid::now_v7()];
        let path = markdown.write_memory(&memory, None).unwrap();
        index.upsert_memory(&memory, &path).unwrap();
        let result = index
            .search("provenance", SearchScope::Global, 10, false, true)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(result.source_session_count, 2);
        assert_eq!(result.supersession_count, 1);
    }

    #[test]
    fn summary_selection_prioritizes_sources_and_is_bounded() {
        let temporary = TempDir::new().unwrap();
        let index = IndexStore::new(temporary.path().join("index.sqlite"));
        index.initialize().unwrap();
        let project = "project";
        let source = Uuid::from_u128(1);
        let unrelated = Uuid::from_u128(2);
        let summary = |text: &str| EpisodicSummary {
            intentions: vec![text.to_owned()],
            actions: Vec::new(),
            outcome: SummaryOutcome::Advanced,
            result: text.to_owned(),
            continuity: Vec::new(),
            candidate_learnings: Vec::new(),
        };
        index
            .upsert_session_summary(
                source,
                Some(project),
                Some(chrono::Utc.timestamp_opt(1, 0).single().unwrap()),
                &summary("source session unrelated wording"),
            )
            .unwrap();
        index
            .upsert_session_summary(
                unrelated,
                Some(project),
                Some(chrono::Utc.timestamp_opt(2, 0).single().unwrap()),
                &summary("export schema error"),
            )
            .unwrap();
        let selected = index
            .related_summaries(Some(project), &[source], "export schema error", 2, 10_000)
            .unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].session_id, source);
        assert_eq!(
            index
                .related_summaries(Some(project), &[source], "export", 2, 1)
                .unwrap()
                .len(),
            0
        );
    }
}
