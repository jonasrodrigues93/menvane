use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use menvane_domain::{
    NormalizedEvent, NormalizedEventKind, NormalizedEventOrigin, NormalizedEventRole,
    NormalizedSession, SessionImporter,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub struct SessionScan {
    pub sessions: Vec<NormalizedSession>,
    pub invalid_records: usize,
    pub estimated_bytes: u64,
}

impl SessionScan {
    pub fn retain_since(&mut self, since: DateTime<Utc>) {
        self.sessions.retain(|session| {
            session
                .events
                .iter()
                .map(|event| event.timestamp)
                .max()
                .is_some_and(|timestamp| timestamp >= since)
        });
        self.estimated_bytes = self
            .sessions
            .iter()
            .map(|session| session.estimated_bytes)
            .sum();
    }
}

pub struct JsonlImporter {
    client: String,
    roots: Vec<PathBuf>,
    max_line_bytes: usize,
}

impl JsonlImporter {
    pub fn claude() -> Result<Self, String> {
        let root = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
            .ok_or_else(|| "HOME is not set".to_owned())?;
        Ok(Self {
            client: "claude-code".to_owned(),
            roots: vec![root.join("projects")],
            max_line_bytes: 1_048_576,
        })
    }

    pub fn codex() -> Result<Self, String> {
        let root = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
            .ok_or_else(|| "HOME is not set".to_owned())?;
        Ok(Self {
            client: "codex".to_owned(),
            roots: vec![root.join("sessions"), root.join("archived_sessions")],
            max_line_bytes: 1_048_576,
        })
    }

    pub fn with_roots(client: impl Into<String>, roots: Vec<PathBuf>) -> Self {
        Self {
            client: client.into(),
            roots,
            max_line_bytes: 1_048_576,
        }
    }

    pub fn scan(&self) -> Result<SessionScan, String> {
        let mut paths = Vec::new();
        for root in &self.roots {
            collect_jsonl(root, &mut paths).map_err(|error| error.to_string())?;
        }
        let mut sessions = Vec::new();
        let mut invalid_records = 0;
        let mut estimated_bytes = 0;
        for path in paths {
            let (session, invalid) = self.parse_file(&path)?;
            invalid_records += invalid;
            estimated_bytes += session.estimated_bytes;
            sessions.push(session);
        }
        Ok(SessionScan {
            sessions,
            invalid_records,
            estimated_bytes,
        })
    }

    fn parse_file(&self, path: &Path) -> Result<(NormalizedSession, usize), String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let estimated_bytes = file.metadata().map_err(|error| error.to_string())?.len();
        let mut external_session_id = None;
        let mut cwd = None;
        let mut events = Vec::new();
        let mut invalid = 0;
        for (line_number, line) in BufReader::new(file).split(b'\n').enumerate() {
            let line = match line {
                Ok(line) if line.len() <= self.max_line_bytes => line,
                _ => {
                    invalid += 1;
                    continue;
                }
            };
            let record: Value = match serde_json::from_slice(&line) {
                Ok(record) => record,
                Err(_) => {
                    invalid += 1;
                    continue;
                }
            };
            external_session_id = external_session_id.or_else(|| {
                find_string(&record, &["session_id", "sessionId", "id"]).map(str::to_owned)
            });
            cwd = cwd.or_else(|| find_string(&record, &["cwd"]).map(str::to_owned));
            if let Some((kind, input, output, tool, success, origin, role)) =
                normalize_record(&record)
            {
                let timestamp = find_string(&record, &["timestamp", "created_at"])
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);
                events.push(NormalizedEvent {
                    event_id: hex::encode(Sha256::digest(
                        format!(
                            "{}:{line_number}:{}",
                            path.display(),
                            String::from_utf8_lossy(&line)
                        )
                        .as_bytes(),
                    )),
                    kind,
                    origin,
                    role,
                    client: self.client.clone(),
                    external_session_id: String::new(),
                    timestamp,
                    cwd: cwd.clone().unwrap_or_default(),
                    project_id: None,
                    tool_family: tool,
                    bounded_input: input,
                    bounded_output: output,
                    attributed_path: None,
                    success,
                    model: find_string(&record, &["model"]).map(str::to_owned),
        harness_injected: false,
                });
            }
        }
        let external_session_id = external_session_id.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_owned()
        });
        let timestamp = events
            .first()
            .map(|event| event.timestamp)
            .unwrap_or_else(Utc::now);
        events.insert(
            0,
            boundary_event(
                &self.client,
                &external_session_id,
                cwd.as_deref().unwrap_or_default(),
                timestamp,
                NormalizedEventKind::SessionStarted,
                "import-start",
            ),
        );
        events.push(boundary_event(
            &self.client,
            &external_session_id,
            cwd.as_deref().unwrap_or_default(),
            events
                .last()
                .map(|event| event.timestamp)
                .unwrap_or(timestamp),
            NormalizedEventKind::SessionEnded,
            "import-end",
        ));
        for event in &mut events {
            event.external_session_id.clone_from(&external_session_id);
            event.cwd = cwd.clone().unwrap_or_default();
        }
        Ok((
            NormalizedSession {
                client: self.client.clone(),
                external_session_id,
                cwd,
                events,
                estimated_bytes,
            },
            invalid,
        ))
    }
}

impl SessionImporter for JsonlImporter {
    fn discover(&self) -> Result<Vec<NormalizedSession>, String> {
        self.scan().map(|scan| scan.sessions)
    }
}

pub struct OpenCodeImporter {
    base_url: String,
}

type ImportedEvent = (
    NormalizedEventKind,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<bool>,
    NormalizedEventOrigin,
    NormalizedEventRole,
);

impl OpenCodeImporter {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    pub async fn scan(&self) -> Result<SessionScan, String> {
        let client = reqwest::Client::new();
        let summaries: Vec<Value> = client
            .get(format!("{}/session", self.base_url.trim_end_matches('/')))
            .send()
            .await
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())?;
        let mut sessions = Vec::new();
        for summary in summaries {
            let Some(id) = find_string(&summary, &["id"]) else {
                continue;
            };
            let messages: Vec<Value> = client
                .get(format!(
                    "{}/session/{id}/message",
                    self.base_url.trim_end_matches('/')
                ))
                .send()
                .await
                .map_err(|error| error.to_string())?
                .json()
                .await
                .map_err(|error| error.to_string())?;
            let mut events = Vec::new();
            let cwd = find_string(&summary, &["directory", "cwd"]).map(str::to_owned);
            let session_timestamp = find_timestamp(&summary).unwrap_or_else(Utc::now);
            for (index, message) in messages.into_iter().enumerate() {
                if let Some((kind, input, output, tool, success, origin, role)) =
                    normalize_record(&message)
                {
                    events.push(NormalizedEvent {
                        event_id: hex::encode(Sha256::digest(
                            format!("opencode:{id}:{index}").as_bytes(),
                        )),
                        kind,
                        origin,
                        role,
                        client: "opencode".to_owned(),
                        external_session_id: id.to_owned(),
                        timestamp: find_timestamp(&message).unwrap_or(session_timestamp),
                        cwd: cwd.clone().unwrap_or_default(),
                        project_id: None,
                        tool_family: tool,
                        bounded_input: input,
                        bounded_output: output,
                        attributed_path: None,
                        success,
                        model: None,
        harness_injected: false,
                    });
                }
            }
            events.insert(
                0,
                boundary_event(
                    "opencode",
                    id,
                    cwd.as_deref().unwrap_or_default(),
                    session_timestamp,
                    NormalizedEventKind::SessionStarted,
                    "import-start",
                ),
            );
            events.push(boundary_event(
                "opencode",
                id,
                cwd.as_deref().unwrap_or_default(),
                session_timestamp,
                NormalizedEventKind::SessionEnded,
                "import-end",
            ));
            sessions.push(NormalizedSession {
                client: "opencode".to_owned(),
                external_session_id: id.to_owned(),
                cwd,
                events,
                estimated_bytes: 0,
            });
        }
        Ok(SessionScan {
            sessions,
            invalid_records: 0,
            estimated_bytes: 0,
        })
    }
}

fn normalize_record(record: &Value) -> Option<ImportedEvent> {
    let role = find_string(record, &["role", "type"])?;
    let content = find_value(record, &["content", "message", "text"]).and_then(content_text);
    let blocks = find_value(record, &["content"])
        .or_else(|| {
            record
                .get("message")
                .and_then(|message| message.get("content"))
        })
        .and_then(Value::as_array);
    if blocks.is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
    }) {
        let block = blocks?
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))?;
        Some((
            NormalizedEventKind::ToolCompleted,
            block.get("tool_use_id").map(Value::to_string),
            block.get("content").and_then(content_text),
            Some("tool".to_owned()),
            Some(block.get("is_error").and_then(Value::as_bool) != Some(true)),
            NormalizedEventOrigin::Tool,
            NormalizedEventRole::ToolActivity,
        ))
    } else if blocks.is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
    }) {
        let block = blocks?
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))?;
        Some((
            NormalizedEventKind::ToolCompleted,
            block.get("input").map(Value::to_string),
            None,
            find_string(block, &["name"]).map(str::to_owned),
            None,
            NormalizedEventOrigin::Tool,
            NormalizedEventRole::ToolActivity,
        ))
    } else if role.contains("user") {
        Some((
            NormalizedEventKind::UserPrompt,
            content,
            None,
            None,
            None,
            NormalizedEventOrigin::User,
            NormalizedEventRole::UserPrompt,
        ))
    } else if role.contains("system") {
        Some((
            NormalizedEventKind::UserPrompt,
            content,
            None,
            None,
            None,
            NormalizedEventOrigin::System,
            NormalizedEventRole::SystemPrompt,
        ))
    } else if role.contains("assistant") {
        Some((
            NormalizedEventKind::UserPrompt,
            content,
            None,
            None,
            None,
            NormalizedEventOrigin::Agent,
            NormalizedEventRole::AgentInstruction,
        ))
    } else if role.contains("tool") || record.get("tool_name").is_some() {
        Some((
            NormalizedEventKind::ToolCompleted,
            record.get("tool_input").map(Value::to_string),
            record
                .get("tool_response")
                .or_else(|| record.get("result"))
                .map(Value::to_string),
            find_string(record, &["tool_name", "name"]).map(str::to_owned),
            record.get("success").and_then(Value::as_bool),
            NormalizedEventOrigin::Tool,
            NormalizedEventRole::ToolActivity,
        ))
    } else {
        None
    }
}

fn content_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    let blocks = value.as_array()?;
    let text = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn find_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    find_value(value, keys)?.as_str()
}

fn find_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let raw = find_value(value, &["timestamp", "created_at", "updated_at"]).or_else(|| {
        value
            .get("time")
            .and_then(|time| find_value(time, &["created", "updated"]))
    })?;
    if let Some(number) = raw.as_i64() {
        return DateTime::from_timestamp_millis(number);
    }
    raw.as_str()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn find_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        if let Some(found) = value.get(key) {
            return Some(found);
        }
        if let Some(found) = value.get("payload").and_then(|payload| payload.get(key)) {
            return Some(found);
        }
        if let Some(found) = value.get("message").and_then(|message| message.get(key)) {
            return Some(found);
        }
    }
    None
}

fn boundary_event(
    client: &str,
    session: &str,
    cwd: &str,
    timestamp: DateTime<Utc>,
    kind: NormalizedEventKind,
    suffix: &str,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: hex::encode(Sha256::digest(
            format!("{client}:{session}:{suffix}").as_bytes(),
        )),
        kind,
        origin: NormalizedEventOrigin::Importer,
        role: NormalizedEventRole::Lifecycle,
        client: client.to_owned(),
        external_session_id: session.to_owned(),
        timestamp,
        cwd: cwd.to_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: None,
        bounded_output: None,
        attributed_path: None,
        success: None,
        model: None,
        harness_injected: false,
    }
}

fn collect_jsonl(root: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            paths.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use menvane_engine::{ImportOutcome, Menvane, ScopeSelection};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn malformed_records_are_skipped_and_reimport_is_idempotent() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        let sessions = temporary.path().join("sessions");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("session.jsonl"),
            format!(
                "{{\"session_id\":\"external-1\",\"cwd\":{:?},\"role\":\"user\",\"content\":\"imported-session-goal\"}}\nnot-json\n{{\"session_id\":\"external-1\",\"role\":\"tool\",\"tool_name\":\"test\",\"success\":true}}\n",
                project.to_string_lossy()
            ),
        )
        .unwrap();
        let scan = JsonlImporter::with_roots("claude-code", vec![sessions])
            .scan()
            .unwrap();
        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.invalid_records, 1);
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        assert_eq!(
            menvane.import_session(scan.sessions[0].clone()).unwrap(),
            ImportOutcome::Imported
        );
        assert_eq!(
            menvane.import_session(scan.sessions[0].clone()).unwrap(),
            ImportOutcome::AlreadyImported
        );
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(menvane.process_next_job())
            .unwrap();
        let results = menvane
            .search_with_sessions(
                &project,
                "imported-session-goal",
                ScopeSelection::Project,
                10,
                true,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            menvane.read(results[0].id).unwrap().metadata.imported,
            Some(true)
        );
    }

    #[test]
    fn unknown_project_is_orphaned_without_guessing() {
        let temporary = TempDir::new().unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let session = NormalizedSession {
            client: "codex".to_owned(),
            external_session_id: "orphan-1".to_owned(),
            cwd: None,
            events: Vec::new(),
            estimated_bytes: 0,
        };
        assert_eq!(
            menvane.import_session(session.clone()).unwrap(),
            ImportOutcome::Orphan
        );
        assert_eq!(
            menvane.import_session(session).unwrap(),
            ImportOutcome::AlreadyImported
        );
    }

    #[test]
    fn claude_message_blocks_become_prompts_and_tools() {
        let prompt: Value = serde_json::json!({
            "type": "user",
            "sessionId": "session-1",
            "cwd": "/tmp",
            "message": {"role": "user", "content": [{"type": "text", "text": "remember this"}]}
        });
        let tool: Value = serde_json::json!({
            "type": "assistant",
            "sessionId": "session-1",
            "message": {"role": "assistant", "content": [{"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}}]}
        });
        let result = normalize_record(&prompt).unwrap();
        assert_eq!(result.0, NormalizedEventKind::UserPrompt);
        assert_eq!(result.1.as_deref(), Some("remember this"));
        let result = normalize_record(&tool).unwrap();
        assert_eq!(result.0, NormalizedEventKind::ToolCompleted);
        assert_eq!(result.3.as_deref(), Some("Bash"));
        assert_eq!(result.1.as_deref(), Some("{\"command\":\"cargo test\"}"));
        assert_eq!(result.5, NormalizedEventOrigin::Tool);
        assert_eq!(result.6, NormalizedEventRole::ToolActivity);
    }

    #[test]
    fn imported_system_and_assistant_messages_are_not_user_prompts() {
        let system = normalize_record(&serde_json::json!({
            "role": "system",
            "content": "<recommended_plugins>plugin metadata</recommended_plugins>"
        }))
        .unwrap();
        assert_eq!(system.5, NormalizedEventOrigin::System);
        assert_eq!(system.6, NormalizedEventRole::SystemPrompt);
        assert_ne!(system.0, NormalizedEventKind::ToolCompleted);

        let assistant = normalize_record(&serde_json::json!({
            "role": "assistant",
            "content": "agent instructions"
        }))
        .unwrap();
        assert_eq!(assistant.5, NormalizedEventOrigin::Agent);
        assert_eq!(assistant.6, NormalizedEventRole::AgentInstruction);
    }

    #[test]
    fn session_scan_filters_by_latest_activity() {
        let old = Utc::now() - chrono::Duration::days(8);
        let recent = Utc::now() - chrono::Duration::days(2);
        let event = |timestamp: DateTime<Utc>| NormalizedEvent {
            event_id: timestamp.to_rfc3339(),
            kind: NormalizedEventKind::UserPrompt,
            origin: Default::default(),
            role: Default::default(),
            client: "claude-code".to_owned(),
            external_session_id: timestamp.to_rfc3339(),
            timestamp,
            cwd: "/tmp".to_owned(),
            project_id: None,
            tool_family: None,
            bounded_input: None,
            bounded_output: None,
            attributed_path: None,
            success: None,
            model: None,
        harness_injected: false,
        };
        let mut scan = SessionScan {
            sessions: vec![
                NormalizedSession {
                    client: "claude-code".to_owned(),
                    external_session_id: "old".to_owned(),
                    cwd: Some("/tmp".to_owned()),
                    events: vec![event(old)],
                    estimated_bytes: 10,
                },
                NormalizedSession {
                    client: "claude-code".to_owned(),
                    external_session_id: "recent".to_owned(),
                    cwd: Some("/tmp".to_owned()),
                    events: vec![event(recent)],
                    estimated_bytes: 20,
                },
            ],
            invalid_records: 0,
            estimated_bytes: 30,
        };

        scan.retain_since(Utc::now() - chrono::Duration::days(7));

        assert_eq!(scan.sessions.len(), 1);
        assert_eq!(scan.sessions[0].external_session_id, "recent");
        assert_eq!(scan.estimated_bytes, 20);
    }
}
