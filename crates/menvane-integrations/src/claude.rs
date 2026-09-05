use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::Utc;
use menvane_domain::{
    NormalizedEvent, NormalizedEventKind, NormalizedEventOrigin, NormalizedEventRole,
};
use menvane_engine::Menvane;
use menvane_runtime::{DEFAULT_PORT, daemon_running, home_from_environment, start_daemon};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PreCompact",
    "Stop",
    "SessionEnd",
];

#[derive(Debug, Clone)]
pub struct ClaudePaths {
    pub settings: PathBuf,
    pub configuration: PathBuf,
}

impl ClaudePaths {
    pub fn discover() -> Result<Self> {
        if let Some(directory) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            let directory = PathBuf::from(directory);
            return Ok(Self {
                settings: directory.join("settings.json"),
                configuration: directory.join("claude.json"),
            });
        }
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        Ok(Self {
            settings: home.join(".claude/settings.json"),
            configuration: home.join(".claude.json"),
        })
    }
}

pub struct ClaudeInstaller {
    paths: ClaudePaths,
    executable: PathBuf,
}

impl ClaudeInstaller {
    pub fn new(paths: ClaudePaths, executable: impl Into<PathBuf>) -> Self {
        Self {
            paths,
            executable: executable.into(),
        }
    }

    pub fn connect(&self) -> Result<bool> {
        let mut settings = read_object(&self.paths.settings)?;
        let mut configuration = read_object(&self.paths.configuration)?;
        let commands = self.hook_commands();
        let settings_changed = install_hooks(&mut settings, &commands)?;
        let configuration_changed = install_mcp(&mut configuration, &self.executable)?;
        if settings_changed {
            backup(&self.paths.settings)?;
            write_json(&self.paths.settings, &settings)?;
        }
        if configuration_changed {
            backup(&self.paths.configuration)?;
            write_json(&self.paths.configuration, &configuration)?;
        }
        Ok(settings_changed || configuration_changed)
    }

    pub fn disconnect(&self) -> Result<bool> {
        let mut settings = read_object(&self.paths.settings)?;
        let mut configuration = read_object(&self.paths.configuration)?;
        let commands = self.hook_commands().into_values().collect::<HashSet<_>>();
        let settings_changed = remove_hooks(&mut settings, &commands);
        let configuration_changed = remove_mcp(&mut configuration, &self.executable);
        if settings_changed {
            backup(&self.paths.settings)?;
            write_json(&self.paths.settings, &settings)?;
        }
        if configuration_changed {
            backup(&self.paths.configuration)?;
            write_json(&self.paths.configuration, &configuration)?;
        }
        Ok(settings_changed || configuration_changed)
    }

    fn hook_commands(&self) -> Map<String, Value> {
        EVENTS
            .into_iter()
            .map(|event| {
                (
                    event.to_owned(),
                    Value::String(format!(
                        "{} hook claude {event}",
                        shell_quote(&self.executable)
                    )),
                )
            })
            .collect()
    }
}

pub struct ClaudeHook<'a> {
    menvane: &'a Menvane,
    executable: PathBuf,
}

impl<'a> ClaudeHook<'a> {
    pub fn new(menvane: &'a Menvane, executable: impl Into<PathBuf>) -> Self {
        Self {
            menvane,
            executable: executable.into(),
        }
    }

    pub fn handle(&self, event_name: &str, payload: Value) -> Result<Value> {
        self.handle_client(event_name, payload, "claude-code")
    }

    pub(crate) fn handle_client(
        &self,
        event_name: &str,
        payload: Value,
        client: &str,
    ) -> Result<Value> {
        if std::env::var("MENVANE_INTERNAL").as_deref() == Ok("1") {
            return Ok(json!({}));
        }
        let event = normalize_event(event_name, &payload, client)?;
        let session_id = event.external_session_id.clone();
        let cwd = event.cwd.clone();
        let sanitized_event = self.menvane.sanitize_event(event)?;
        let prompt = sanitized_event
            .as_ref()
            .and_then(|event| event.bounded_input.clone())
            .unwrap_or_default();
        if let Some(event) = sanitized_event {
            self.ensure_daemon()?;
            post_json("/api/v1/events", &serde_json::to_value(event)?)?;
        }
        let recall_kind = match event_name {
            "SessionStart" => Some("session-start"),
            "UserPromptSubmit" => Some("user-prompt"),
            _ => None,
        };
        let Some(recall_kind) = recall_kind else {
            return Ok(json!({}));
        };
        self.ensure_daemon()?;
        let response = post_json(
            "/api/v1/recall",
            &json!({
                "client": client,
                "cwd": cwd,
                "session_id": session_id,
                "kind": recall_kind,
                "prompt": prompt
            }),
        )?;
        let context = response
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": context
            }
        }))
    }

    fn ensure_daemon(&self) -> Result<()> {
        let home = home_from_environment()?;
        if daemon_running(&home) {
            return Ok(());
        }
        start_daemon(&home, &self.executable)?;
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(50));
            if TcpStream::connect(("127.0.0.1", DEFAULT_PORT)).is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!("Menvane daemon did not become ready")
    }
}

fn normalize_event(event_name: &str, payload: &Value, client: &str) -> Result<NormalizedEvent> {
    let kind = match event_name {
        "SessionStart" => NormalizedEventKind::SessionStarted,
        "UserPromptSubmit" => NormalizedEventKind::UserPrompt,
        "PostToolUse" => NormalizedEventKind::ToolCompleted,
        "PreCompact" | "PostCompact" => NormalizedEventKind::ContextCompacted,
        "Stop" => NormalizedEventKind::TurnStopped,
        "SessionEnd" => NormalizedEventKind::SessionEnded,
        _ => anyhow::bail!("unsupported Claude hook event: {event_name}"),
    };
    let external_session_id = required_string(payload, "session_id")?;
    let cwd = required_string(payload, "cwd")?;
    let bounded_input = match kind {
        NormalizedEventKind::UserPrompt => payload
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_owned),
        NormalizedEventKind::ContextCompacted => [
            "compaction_summary",
            "compactionSummary",
            "summary",
            "content",
        ]
        .iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str).map(str::to_owned)),
        NormalizedEventKind::ToolCompleted => payload.get("tool_input").map(Value::to_string),
        _ => None,
    };
    let bounded_output = (kind == NormalizedEventKind::ToolCompleted)
        .then(|| payload.get("tool_response").map(Value::to_string))
        .flatten();
    let attributed_path = payload
        .get("tool_input")
        .and_then(find_attributed_path)
        .map(str::to_owned);
    let success = (kind == NormalizedEventKind::ToolCompleted).then(|| {
        payload
            .get("tool_response")
            .and_then(|response| response.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                payload
                    .get("tool_response")
                    .is_some_and(|response| response.get("error").is_none())
            })
    });
    let canonical = serde_json::to_vec(payload)?;
    let event_id = hex::encode(Sha256::digest(
        [event_name.as_bytes(), canonical.as_slice()].concat(),
    ));
    let (origin, role) = event_metadata(kind, payload);
    Ok(NormalizedEvent {
        event_id,
        kind,
        origin,
        role,
        client: client.to_owned(),
        external_session_id,
        timestamp: Utc::now(),
        cwd,
        project_id: None,
        tool_family: payload
            .get("tool_name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        bounded_input,
        bounded_output,
        attributed_path,
        success,
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        harness_injected: false,
    })
}

fn event_metadata(
    kind: NormalizedEventKind,
    payload: &Value,
) -> (NormalizedEventOrigin, NormalizedEventRole) {
    if kind == NormalizedEventKind::ContextCompacted {
        return (
            NormalizedEventOrigin::Compaction,
            NormalizedEventRole::CompactionSummary,
        );
    }
    if kind == NormalizedEventKind::ToolCompleted {
        return (
            NormalizedEventOrigin::Tool,
            NormalizedEventRole::ToolActivity,
        );
    }
    if kind != NormalizedEventKind::UserPrompt {
        return (
            NormalizedEventOrigin::System,
            NormalizedEventRole::Lifecycle,
        );
    }
    let source = ["origin", "role", "source", "input_type"]
        .iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if payload.get("is_injected").and_then(Value::as_bool) == Some(true)
        || payload.get("injected").and_then(Value::as_bool) == Some(true)
        || payload.get("system_prompt").is_some()
        || payload.get("system").is_some()
        || source.contains("system")
    {
        return (
            NormalizedEventOrigin::System,
            NormalizedEventRole::SystemPrompt,
        );
    }
    if payload.get("agent_instructions").is_some()
        || payload.get("instructions").is_some()
        || source.contains("agent")
        || source.contains("inject")
    {
        return (
            NormalizedEventOrigin::Agent,
            NormalizedEventRole::AgentInstruction,
        );
    }
    if payload.get("tool_metadata").is_some() || source.contains("tool") {
        return (
            NormalizedEventOrigin::Tool,
            NormalizedEventRole::ToolMetadata,
        );
    }
    (NormalizedEventOrigin::User, NormalizedEventRole::UserPrompt)
}

fn find_attributed_path(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    for key in ["file_path", "path"] {
        if let Some(path) = object.get(key).and_then(Value::as_str) {
            return Some(path);
        }
    }
    None
}

fn required_string(payload: &Value, key: &str) -> Result<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("Claude hook payload is missing {key}"))
}

fn post_json(path: &str, body: &Value) -> Result<Value> {
    let body = serde_json::to_vec(body)?;
    let mut stream = TcpStream::connect(("127.0.0.1", DEFAULT_PORT))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{DEFAULT_PORT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("daemon returned an invalid HTTP response")?;
    let headers = String::from_utf8_lossy(&response[..separator]);
    if !headers.starts_with("HTTP/1.1 200") {
        anyhow::bail!("daemon request failed: {headers}");
    }
    Ok(serde_json::from_slice(&response[separator + 4..])?)
}

fn install_hooks(settings: &mut Map<String, Value>, commands: &Map<String, Value>) -> Result<bool> {
    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude hooks configuration must be an object")?;
    let mut changed = false;
    for (event, command) in commands {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("Claude {event} hooks must be an array"))?;
        let exists = groups.iter().any(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| {
                    handlers
                        .iter()
                        .any(|handler| handler.get("command") == Some(command))
                })
        });
        if !exists {
            groups.push(json!({
                "hooks": [{ "type": "command", "command": command, "timeout": 5 }]
            }));
            changed = true;
        }
    }
    Ok(changed)
}

fn remove_hooks(settings: &mut Map<String, Value>, commands: &HashSet<Value>) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for event in EVENTS {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let previous = handlers.len();
                handlers.retain(|handler| {
                    !handler
                        .get("command")
                        .is_some_and(|command| commands.contains(command))
                });
                changed |= previous != handlers.len();
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|handlers| !handlers.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
    changed
}

fn install_mcp(configuration: &mut Map<String, Value>, executable: &Path) -> Result<bool> {
    let servers = configuration
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude mcpServers configuration must be an object")?;
    let expected = json!({
        "type": "stdio",
        "command": executable.to_string_lossy(),
        "args": ["mcp"]
    });
    if servers.get("menvane") == Some(&expected) {
        Ok(false)
    } else {
        servers.insert("menvane".to_owned(), expected);
        Ok(true)
    }
}

fn remove_mcp(configuration: &mut Map<String, Value>, executable: &Path) -> bool {
    let Some(servers) = configuration
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let owned = servers.get("menvane").is_some_and(|server| {
        server.get("command").and_then(Value::as_str) == Some(&*executable.to_string_lossy())
            && server.get("args") == Some(&json!(["mcp"]))
    });
    if owned {
        servers.remove("menvane");
    }
    owned
}

fn read_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    serde_json::from_slice::<Value>(&fs::read(path)?)?
        .as_object()
        .cloned()
        .context("Claude configuration root must be a JSON object")
}

fn backup(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Claude configuration path has no filename")?;
    fs::copy(
        path,
        path.with_file_name(format!("{filename}.menvane-backup-{timestamp}")),
    )?;
    Ok(())
}

fn write_json(path: &Path, object: &Map<String, Value>) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".menvane-config-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, object)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn connect_and_disconnect_preserve_unowned_configuration() {
        let temporary = TempDir::new().unwrap();
        let paths = ClaudePaths {
            settings: temporary.path().join(".claude/settings.json"),
            configuration: temporary.path().join(".claude.json"),
        };
        fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.settings,
            r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"other-hook"}]}]}}"#,
        )
        .unwrap();
        fs::write(
            &paths.configuration,
            r#"{"mcpServers":{"other":{"type":"stdio","command":"other"}},"projects":{"x":{}}}"#,
        )
        .unwrap();
        let installer = ClaudeInstaller::new(paths.clone(), "/opt/menvane");
        assert!(installer.connect().unwrap());
        assert!(!installer.connect().unwrap());
        let settings = read_object(&paths.settings).unwrap();
        assert_eq!(settings["theme"], "dark");
        let configuration = read_object(&paths.configuration).unwrap();
        assert!(configuration["mcpServers"]["other"].is_object());
        assert!(configuration["mcpServers"]["menvane"].is_object());
        assert!(installer.disconnect().unwrap());
        let settings = read_object(&paths.settings).unwrap();
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "other-hook"
        );
        let configuration = read_object(&paths.configuration).unwrap();
        assert!(configuration["mcpServers"]["other"].is_object());
        assert!(configuration["mcpServers"].get("menvane").is_none());
        assert_eq!(configuration["projects"], json!({ "x": {} }));
    }

    #[test]
    fn normalization_marks_injected_and_compaction_content() {
        let system = normalize_event(
            "UserPromptSubmit",
            &json!({
                "session_id": "session",
                "cwd": "/tmp",
                "prompt": "system instructions",
                "role": "system"
            }),
            "claude-code",
        )
        .unwrap();
        assert!(!system.is_user_prompt());
        assert_eq!(system.origin, NormalizedEventOrigin::System);
        assert_eq!(system.role, NormalizedEventRole::SystemPrompt);

        let tool_metadata = normalize_event(
            "UserPromptSubmit",
            &json!({
                "session_id": "session",
                "cwd": "/tmp",
                "prompt": "tool metadata",
                "role": "tool"
            }),
            "claude-code",
        )
        .unwrap();
        assert!(!tool_metadata.is_user_prompt());
        assert_eq!(tool_metadata.origin, NormalizedEventOrigin::Tool);
        assert_eq!(tool_metadata.role, NormalizedEventRole::ToolMetadata);

        let compacted = normalize_event(
            "PostCompact",
            &json!({
                "session_id": "session",
                "cwd": "/tmp",
                "summary": "compacted context"
            }),
            "claude-code",
        )
        .unwrap();
        assert_eq!(compacted.origin, NormalizedEventOrigin::Compaction);
        assert_eq!(compacted.role, NormalizedEventRole::CompactionSummary);
        assert_eq!(
            compacted.bounded_input.as_deref(),
            Some("compacted context")
        );
    }
}
