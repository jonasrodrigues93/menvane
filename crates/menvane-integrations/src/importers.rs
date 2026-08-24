use std::collections::HashMap;
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
        let mut codex_tool_calls = HashMap::new();
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
            let normalized = if self.client == "codex" {
                normalize_codex_record(&record, &mut codex_tool_calls)
                    .unwrap_or_else(|| normalize_record(&record))
            } else {
                normalize_record(&record)
            };
            if let Some((kind, input, output, tool, success, origin, role)) = normalized {
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
                    attributed_path: input.as_deref().and_then(imported_attributed_path),
                    bounded_input: input,
                    bounded_output: output,
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

#[derive(Default)]
struct PendingCodexToolCall {
    tool: Option<String>,
    input: Option<String>,
    success: Option<bool>,
}

fn normalize_codex_record(
    record: &Value,
    tool_calls: &mut HashMap<String, PendingCodexToolCall>,
) -> Option<Option<ImportedEvent>> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let Some(payload) = record.get("payload") else {
        return Some(None);
    };
    let Some(payload_type) = payload.get("type").and_then(Value::as_str) else {
        return Some(None);
    };
    match payload_type {
        "message" => {
            if payload.get("role").and_then(Value::as_str) != Some("user") {
                return Some(None);
            }
            Some(
                payload
                    .get("content")
                    .and_then(content_text)
                    .map(|content| {
                        (
                            NormalizedEventKind::UserPrompt,
                            Some(content),
                            None,
                            None,
                            None,
                            NormalizedEventOrigin::User,
                            NormalizedEventRole::UserPrompt,
                        )
                    }),
            )
        }
        "custom_tool_call" | "function_call" => {
            let call_id = find_string(payload, &["call_id", "id"]).map(str::to_owned);
            if let Some(call_id) = call_id {
                let status = payload.get("status").and_then(Value::as_str);
                tool_calls.insert(
                    call_id,
                    PendingCodexToolCall {
                        tool: payload
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        input: payload
                            .get("input")
                            .or_else(|| payload.get("arguments"))
                            .map(value_text),
                        success: match status {
                            Some("completed") => Some(true),
                            Some("failed" | "error") => Some(false),
                            _ => None,
                        },
                    },
                );
            }
            Some(None)
        }
        "custom_tool_call_output" | "function_call_output" => {
            let call_id = find_string(payload, &["call_id", "id"]).map(str::to_owned);
            let pending = call_id
                .as_ref()
                .and_then(|call_id| tool_calls.remove(call_id))
                .unwrap_or_default();
            let output = payload.get("output").map(value_text);
            let success = payload
                .get("is_error")
                .and_then(Value::as_bool)
                .map(|is_error| !is_error)
                .or(pending.success);
            Some(Some((
                NormalizedEventKind::ToolCompleted,
                pending.input.or(call_id),
                output,
                pending.tool.or_else(|| Some("tool".to_owned())),
                success,
                NormalizedEventOrigin::Tool,
                NormalizedEventRole::ToolActivity,
            )))
        }
        _ => Some(None),
    }
}

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
                for (part_index, (kind, input, output, tool, success, origin, role)) in
                    normalize_opencode_message(&message).into_iter().enumerate()
                {
                    let attributed_path = input.as_deref().and_then(imported_attributed_path);
                    events.push(NormalizedEvent {
                        event_id: hex::encode(Sha256::digest(
                            format!("opencode:{id}:{index}:{part_index}").as_bytes(),
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
                        attributed_path,
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

fn imported_attributed_path(input: &str) -> Option<String> {
    let input: Value = serde_json::from_str(input).ok()?;
    find_string(&input, &["filePath", "file_path", "path"])
        .map(str::to_owned)
        .or_else(|| {
            input
                .get("patchText")
                .and_then(Value::as_str)
                .and_then(|patch| {
                    patch.lines().find_map(|line| {
                        ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                            .into_iter()
                            .find_map(|prefix| line.strip_prefix(prefix).map(str::to_owned))
                    })
                })
        })
}

fn normalize_opencode_message(record: &Value) -> Vec<ImportedEvent> {
    let Some(parts) = record.get("parts").and_then(Value::as_array) else {
        return normalize_record(record).into_iter().collect();
    };
    let Some(role) = record
        .get("info")
        .and_then(|info| info.get("role"))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    if role.contains("user") {
        return content_text(&Value::Array(parts.clone()))
            .map(|content| {
                vec![(
                    NormalizedEventKind::UserPrompt,
                    Some(content),
                    None,
                    None,
                    None,
                    NormalizedEventOrigin::User,
                    NormalizedEventRole::UserPrompt,
                )]
            })
            .unwrap_or_default();
    }
    if role.contains("assistant") {
        return parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool"))
            .map(|part| {
                let state = part.get("state").unwrap_or(&Value::Null);
                let status = state.get("status").and_then(Value::as_str);
                (
                    NormalizedEventKind::ToolCompleted,
                    state.get("input").map(Value::to_string),
                    state.get("output").map(value_text),
                    part.get("tool").and_then(Value::as_str).map(str::to_owned),
                    match status {
                        Some("completed") => Some(true),
                        Some("error" | "failed") => Some(false),
                        _ => None,
                    },
                    NormalizedEventOrigin::Tool,
                    NormalizedEventRole::ToolActivity,
                )
            })
            .collect();
    }
    normalize_record(record).into_iter().collect()
}

fn normalize_record(record: &Value) -> Option<ImportedEvent> {
    let role = find_string(record, &["role", "type"])?;
    let content =
        find_value(record, &["content", "message", "text", "parts"]).and_then(content_text);
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

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn find_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    find_value(value, keys)?.as_str()
}

fn find_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let raw = find_value(value, &["timestamp", "created_at", "updated_at"])
        .or_else(|| {
            value
                .get("time")
                .and_then(|time| find_value(time, &["created", "updated"]))
        })
        .or_else(|| {
            value
                .get("info")
                .and_then(|info| info.get("time"))
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
        if let Some(found) = value.get("info").and_then(|info| info.get(key)) {
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
    use menvane_engine::{ImportOutcome, Menvane};
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
                "{{\"session_id\":\"external-1\",\"cwd\":{:?},\"role\":\"user\",\"content\":\"imported-session-prompt\"}}\nnot-json\n{{\"session_id\":\"external-1\",\"role\":\"tool\",\"tool_name\":\"test\",\"success\":true}}\n",
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
        let finalized = menvane
            .jobs()
            .unwrap()
            .into_iter()
            .find(|job| job.job_type == "finalize_session")
            .unwrap();
        let session_id = finalized.dedupe_key.parse().unwrap();
        let events = menvane.session_events(session_id).unwrap();
        assert_eq!(events.len(), scan.sessions[0].events.len());
        let sessions_before = menvane.sessions(100).unwrap();
        let handoff_before = menvane.current_handoff_items(None).unwrap();
        let memories_before = menvane.all_memories().unwrap();
        assert_eq!(
            menvane.import_session(scan.sessions[0].clone()).unwrap(),
            ImportOutcome::AlreadyImported
        );
        assert_eq!(menvane.sessions(100).unwrap(), sessions_before);
        assert_eq!(menvane.current_handoff_items(None).unwrap(), handoff_before);
        assert_eq!(menvane.all_memories().unwrap(), memories_before);
        assert_eq!(menvane.session_summary(session_id).unwrap(), None);
        assert!(
            events
                .iter()
                .any(|event| { event.bounded_input.as_deref() == Some("imported-session-prompt") })
        );
    }

    #[test]
    fn codex_custom_tool_calls_and_outputs_become_tool_evidence() {
        let temporary = TempDir::new().unwrap();
        let sessions = temporary.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("session.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-23T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-1\",\"cwd\":\"/tmp\"}}\n",
                "{\"timestamp\":\"2026-08-23T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"run tests\"}]}}\n",
                "{\"timestamp\":\"2026-08-23T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"call_id\":\"call-1\",\"status\":\"completed\",\"input\":\"{\\\"cmd\\\":\\\"cargo test\\\"}\"}}\n",
                "{\"timestamp\":\"2026-08-23T10:00:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"call-1\",\"output\":\"tests passed\"}}\n",
                "{\"timestamp\":\"2026-08-23T10:00:04Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n"
            ),
        )
        .unwrap();

        let scan = JsonlImporter::with_roots("codex", vec![sessions])
            .scan()
            .unwrap();
        let events = &scan.sessions[0].events;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == NormalizedEventKind::UserPrompt)
                .count(),
            1
        );
        let tool = events
            .iter()
            .find(|event| event.kind == NormalizedEventKind::ToolCompleted)
            .unwrap();
        assert_eq!(tool.tool_family.as_deref(), Some("exec"));
        assert_eq!(
            tool.bounded_input.as_deref(),
            Some("{\"cmd\":\"cargo test\"}")
        );
        assert_eq!(tool.bounded_output.as_deref(), Some("tests passed"));
        assert_eq!(tool.success, Some(true));
        assert!(events.iter().all(|event| {
            event.bounded_input.as_deref() != Some("done")
                && event.bounded_output.as_deref() != Some("done")
        }));
    }

    #[test]
    fn empty_skipped_import_can_be_repaired_once() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let timestamp = Utc::now();
        let empty = NormalizedSession {
            client: "opencode".to_owned(),
            external_session_id: "repairable".to_owned(),
            cwd: Some(project.to_string_lossy().into_owned()),
            events: vec![
                boundary_event(
                    "opencode",
                    "repairable",
                    project.to_string_lossy().as_ref(),
                    timestamp,
                    NormalizedEventKind::SessionStarted,
                    "import-start",
                ),
                boundary_event(
                    "opencode",
                    "repairable",
                    project.to_string_lossy().as_ref(),
                    timestamp,
                    NormalizedEventKind::SessionEnded,
                    "import-end",
                ),
            ],
            estimated_bytes: 0,
        };
        assert_eq!(
            menvane.import_session(empty.clone()).unwrap(),
            ImportOutcome::Imported
        );
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(menvane.process_next_job())
            .unwrap();
        let mut repaired = empty;
        repaired.events.insert(
            1,
            NormalizedEvent {
                event_id: "repaired-prompt".to_owned(),
                kind: NormalizedEventKind::UserPrompt,
                origin: NormalizedEventOrigin::User,
                role: NormalizedEventRole::UserPrompt,
                client: "opencode".to_owned(),
                external_session_id: "repairable".to_owned(),
                timestamp,
                cwd: project.to_string_lossy().into_owned(),
                project_id: None,
                tool_family: None,
                bounded_input: Some("recovered prompt".to_owned()),
                bounded_output: None,
                attributed_path: None,
                success: None,
                model: None,
                harness_injected: false,
            },
        );

        assert_eq!(
            menvane.import_session(repaired.clone()).unwrap(),
            ImportOutcome::Imported
        );
        assert_eq!(
            menvane.import_session(repaired).unwrap(),
            ImportOutcome::AlreadyImported
        );
        let latest = menvane.sessions(1).unwrap().remove(0);
        assert_eq!(latest.generation, 2);
        assert_eq!(latest.state, menvane_domain::SessionState::Finalized);
        assert!(
            menvane
                .session_events(latest.id)
                .unwrap()
                .iter()
                .any(|event| event.bounded_input.as_deref() == Some("recovered prompt"))
        );
    }

    #[test]
    fn successful_mutation_attributes_global_opencode_session_to_project() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("subwitcher");
        fs::create_dir_all(&project).unwrap();
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&project)
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        fs::write(project.join("project.md"), "# Subwitcher\n").unwrap();
        let timestamp = Utc::now();
        let mut session = NormalizedSession {
            client: "opencode".to_owned(),
            external_session_id: "global-project-work".to_owned(),
            cwd: Some(temporary.path().to_string_lossy().into_owned()),
            events: vec![
                boundary_event(
                    "opencode",
                    "global-project-work",
                    temporary.path().to_string_lossy().as_ref(),
                    timestamp,
                    NormalizedEventKind::SessionStarted,
                    "import-start",
                ),
                boundary_event(
                    "opencode",
                    "global-project-work",
                    temporary.path().to_string_lossy().as_ref(),
                    timestamp,
                    NormalizedEventKind::SessionEnded,
                    "import-end",
                ),
            ],
            estimated_bytes: 0,
        };
        session.events.insert(
            1,
            NormalizedEvent {
                event_id: "project-write".to_owned(),
                kind: NormalizedEventKind::ToolCompleted,
                origin: NormalizedEventOrigin::Tool,
                role: NormalizedEventRole::ToolActivity,
                client: "opencode".to_owned(),
                external_session_id: "global-project-work".to_owned(),
                timestamp,
                cwd: temporary.path().to_string_lossy().into_owned(),
                project_id: None,
                tool_family: Some("apply_patch".to_owned()),
                bounded_input: Some("updated project.md".to_owned()),
                bounded_output: Some("success".to_owned()),
                attributed_path: Some(project.join("project.md").to_string_lossy().into_owned()),
                success: Some(true),
                model: None,
                harness_injected: false,
            },
        );
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        assert_eq!(
            menvane.import_session(session).unwrap(),
            ImportOutcome::Imported
        );
        let imported = menvane.sessions(1).unwrap().remove(0);
        assert_eq!(imported.state, menvane_domain::SessionState::Finalized);
        assert!(imported.project_id.is_some());
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
    fn current_opencode_message_shape_becomes_a_prompt() {
        let message = serde_json::json!({
            "info": {
                "role": "user",
                "time": {"created": 1_786_656_868_285_i64}
            },
            "parts": [{"type": "text", "text": "execute task 001"}]
        });

        let result = normalize_record(&message).unwrap();

        assert_eq!(result.0, NormalizedEventKind::UserPrompt);
        assert_eq!(result.1.as_deref(), Some("execute task 001"));
        assert_eq!(
            find_timestamp(&message),
            DateTime::from_timestamp_millis(1_786_656_868_285)
        );
    }

    #[test]
    fn current_opencode_tool_parts_become_individual_events() {
        let message = serde_json::json!({
            "info": {"role": "assistant"},
            "parts": [
                {"type": "reasoning", "text": "private"},
                {"type": "tool", "tool": "read", "state": {
                    "status": "completed",
                    "input": {"filePath": "/tmp/one"},
                    "output": "first"
                }},
                {"type": "tool", "tool": "bash", "state": {
                    "status": "failed",
                    "input": {"command": "false"},
                    "output": "failed"
                }}
            ]
        });

        let events = normalize_opencode_message(&message);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].3.as_deref(), Some("read"));
        assert_eq!(events[0].4, Some(true));
        assert_eq!(events[1].3.as_deref(), Some("bash"));
        assert_eq!(events[1].4, Some(false));
        assert!(
            events
                .iter()
                .all(|event| event.2.as_deref() != Some("private"))
        );
    }

    #[test]
    fn opencode_apply_patch_exposes_its_attributed_path() {
        let input = serde_json::json!({
            "patchText": "*** Begin Patch\n*** Update File: /home/user/project/project.md\n@@\n-old\n+new\n*** End Patch"
        });
        assert_eq!(
            imported_attributed_path(&input.to_string()).as_deref(),
            Some("/home/user/project/project.md")
        );
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
