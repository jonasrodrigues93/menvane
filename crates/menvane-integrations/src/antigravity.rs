use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
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

#[derive(Debug, Clone)]
pub struct AntigravityPaths {
    pub mcp_configuration: PathBuf,
    pub hooks_configuration: PathBuf,
    pub brain_directories: Vec<PathBuf>,
}

impl AntigravityPaths {
    pub fn discover() -> Result<Self> {
        let config_dir = if let Some(directory) = std::env::var_os("ANTIGRAVITY_CONFIG_DIR") {
            PathBuf::from(directory)
        } else {
            let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
            home.join(".gemini/config")
        };
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        Ok(Self {
            mcp_configuration: config_dir.join("mcp_config.json"),
            hooks_configuration: config_dir.join("hooks.json"),
            brain_directories: vec![
                home.join(".gemini/antigravity-cli/brain"),
                home.join(".gemini/antigravity/brain"),
                home.join(".gemini/antigravity-ide/brain"),
            ],
        })
    }
}

pub struct AntigravityInstaller {
    paths: AntigravityPaths,
    executable: PathBuf,
}

impl AntigravityInstaller {
    pub fn new(paths: AntigravityPaths, executable: impl Into<PathBuf>) -> Self {
        Self {
            paths,
            executable: executable.into(),
        }
    }

    pub fn connect(&self) -> Result<bool> {
        let mut mcp = read_object(&self.paths.mcp_configuration)?;
        let mut hooks = read_object(&self.paths.hooks_configuration)?;
        let mcp_changed = install_mcp(&mut mcp, &self.executable)?;
        let hooks_changed = install_hooks(&mut hooks, &self.executable)?;
        if mcp_changed {
            backup(&self.paths.mcp_configuration)?;
            write_json(&self.paths.mcp_configuration, &mcp)?;
        }
        if hooks_changed {
            backup(&self.paths.hooks_configuration)?;
            write_json(&self.paths.hooks_configuration, &hooks)?;
        }
        Ok(mcp_changed || hooks_changed)
    }

    pub fn disconnect(&self) -> Result<bool> {
        let mut mcp = read_object(&self.paths.mcp_configuration)?;
        let mut hooks = read_object(&self.paths.hooks_configuration)?;
        let mcp_changed = remove_mcp(&mut mcp, &self.executable);
        let hooks_changed = remove_hooks(&mut hooks, &self.executable);
        if mcp_changed {
            backup(&self.paths.mcp_configuration)?;
            write_json(&self.paths.mcp_configuration, &mcp)?;
        }
        if hooks_changed {
            backup(&self.paths.hooks_configuration)?;
            write_json(&self.paths.hooks_configuration, &hooks)?;
        }
        Ok(mcp_changed || hooks_changed)
    }
}

fn install_mcp(configuration: &mut Map<String, Value>, executable: &Path) -> Result<bool> {
    let servers = configuration
        .entry("mcpServers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("mcpServers must be a JSON object")?;
    let expected = json!({
        "command": executable.to_string_lossy(),
        "args": ["mcp"]
    });
    if servers.get("menvane") != Some(&expected) {
        servers.insert("menvane".to_owned(), expected);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn remove_mcp(configuration: &mut Map<String, Value>, executable: &Path) -> bool {
    let Some(servers) = configuration
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let target = executable.to_string_lossy();
    let is_menvane = |value: &Value| {
        value
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command == target || command.contains("menvane"))
    };
    if servers.get("menvane").is_some_and(is_menvane) {
        servers.remove("menvane");
        true
    } else {
        false
    }
}

fn install_hooks(configuration: &mut Map<String, Value>, executable: &Path) -> Result<bool> {
    let exe = shell_quote(executable);
    let expected = json!({
        "PreInvocation": [
            {
                "type": "command",
                "command": format!("{exe} hook antigravity PreInvocation")
            }
        ],
        "PostToolUse": [
            {
                "matcher": "*",
                "hooks": [
                    {
                        "type": "command",
                        "command": format!("{exe} hook antigravity PostToolUse")
                    }
                ]
            }
        ],
        "Stop": [
            {
                "type": "command",
                "command": format!("{exe} hook antigravity Stop")
            }
        ]
    });
    if configuration.get("menvane") != Some(&expected) {
        configuration.insert("menvane".to_owned(), expected);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn remove_hooks(configuration: &mut Map<String, Value>, executable: &Path) -> bool {
    let target = executable.to_string_lossy();
    if let Some(entry) = configuration.get("menvane") {
        let text = entry.to_string();
        if text.contains(&*target) || text.contains("menvane hook antigravity") {
            configuration.remove("menvane");
            return true;
        }
    }
    false
}

pub struct AntigravityHook<'a> {
    menvane: &'a Menvane,
    executable: PathBuf,
}

impl<'a> AntigravityHook<'a> {
    pub fn new(menvane: &'a Menvane, executable: impl Into<PathBuf>) -> Self {
        Self {
            menvane,
            executable: executable.into(),
        }
    }

    pub fn handle(&self, event_name: &str, payload: Value) -> Result<Value> {
        if std::env::var("MENVANE_INTERNAL").as_deref() == Ok("1") {
            return Ok(json!({}));
        }
        let event = normalize_antigravity_event(event_name, &payload)?;
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
        if event_name == "PreInvocation" || event_name == "SessionStart" {
            self.ensure_daemon()?;
            let kind = if event_name == "SessionStart" {
                "session-start"
            } else {
                "user-prompt"
            };
            let response = post_json(
                "/api/v1/recall",
                &json!({
                    "client": "antigravity",
                    "cwd": cwd,
                    "session_id": session_id,
                    "kind": kind,
                    "prompt": prompt
                }),
            )?;
            let context = response
                .get("context")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !context.is_empty() {
                return Ok(json!({
                    "injectSteps": [
                        {
                            "ephemeralMessage": context
                        }
                    ]
                }));
            }
            return Ok(json!({ "injectSteps": [] }));
        }
        if event_name == "Stop" {
            return Ok(json!({ "decision": "allow" }));
        }
        Ok(json!({}))
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

fn normalize_antigravity_event(event_name: &str, payload: &Value) -> Result<NormalizedEvent> {
    let kind = match event_name {
        "SessionStart" => NormalizedEventKind::SessionStarted,
        "PreInvocation" | "UserPromptSubmit" => NormalizedEventKind::UserPrompt,
        "PostToolUse" => NormalizedEventKind::ToolCompleted,
        "Stop" => NormalizedEventKind::TurnStopped,
        "SessionEnd" => NormalizedEventKind::SessionEnded,
        _ => anyhow::bail!("unsupported Antigravity hook event: {event_name}"),
    };

    let external_session_id = payload
        .get("conversationId")
        .or_else(|| payload.get("conversation_id"))
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown-session".to_owned());

    let cwd = payload
        .get("workspacePaths")
        .and_then(Value::as_array)
        .and_then(|paths| paths.first())
        .and_then(Value::as_str)
        .or_else(|| payload.get("cwd").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    let (bounded_input, bounded_output, tool_family, success, attributed_path) = match kind {
        NormalizedEventKind::UserPrompt => {
            let prompt = payload
                .get("userMessage")
                .or_else(|| payload.get("prompt"))
                .or_else(|| payload.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| read_last_user_prompt(payload));
            (prompt, None, None, None, None)
        }
        NormalizedEventKind::ToolCompleted => {
            let tool = payload
                .get("toolCall")
                .or_else(|| payload.get("tool_call"))
                .or_else(|| payload.get("tool"));
            let name = tool
                .and_then(|t| t.get("name").or_else(|| t.get("tool_name")))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let input = tool
                .and_then(|t| t.get("args").or_else(|| t.get("arguments")))
                .map(Value::to_string);
            let output = payload
                .get("output")
                .or_else(|| payload.get("result"))
                .map(Value::to_string);
            let is_err = payload.get("error").is_some_and(|e| !e.is_null());
            let path = input.as_deref().and_then(find_attributed_path);
            (input, output, name, Some(!is_err), path)
        }
        _ => (None, None, None, None, None),
    };

    let canonical = serde_json::to_vec(payload)?;
    let event_id = hex::encode(Sha256::digest(
        [event_name.as_bytes(), canonical.as_slice()].concat(),
    ));

    let (origin, role) = match kind {
        NormalizedEventKind::UserPrompt => {
            (NormalizedEventOrigin::User, NormalizedEventRole::UserPrompt)
        }
        NormalizedEventKind::ToolCompleted => (
            NormalizedEventOrigin::Tool,
            NormalizedEventRole::ToolActivity,
        ),
        _ => (
            NormalizedEventOrigin::System,
            NormalizedEventRole::Lifecycle,
        ),
    };

    Ok(NormalizedEvent {
        event_id,
        kind,
        origin,
        role,
        client: "antigravity".to_owned(),
        external_session_id,
        timestamp: Utc::now(),
        cwd,
        project_id: None,
        tool_family,
        bounded_input,
        bounded_output,
        attributed_path,
        success,
        model: payload
            .get("modelName")
            .or_else(|| payload.get("model"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        harness_injected: false,
    })
}

fn read_last_user_prompt(payload: &Value) -> Option<String> {
    let transcript_path = payload.get("transcriptPath").and_then(Value::as_str)?;
    let file = File::open(transcript_path).ok()?;
    let reader = BufReader::new(file);
    let mut last_prompt = None;
    for line in reader.lines().map_while(Result::ok) {
        if let Ok(item) = serde_json::from_str::<Value>(&line)
            && item.get("type").and_then(Value::as_str) == Some("USER_INPUT")
            && let Some(content) = item.get("content").and_then(Value::as_str)
        {
            last_prompt = Some(content.to_owned());
        }
    }
    last_prompt
}

fn find_attributed_path(input: &str) -> Option<String> {
    let value: Value = serde_json::from_str(input).ok()?;
    for key in [
        "target_file",
        "TargetFile",
        "file_path",
        "filePath",
        "path",
        "Path",
        "AbsolutePath",
        "absolute_path",
    ] {
        if let Some(path) = value.get(key).and_then(Value::as_str) {
            return Some(path.to_owned());
        }
    }
    None
}

fn post_json(path: &str, payload: &Value) -> Result<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", DEFAULT_PORT))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let body = serde_json::to_vec(payload)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{DEFAULT_PORT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .context("invalid HTTP response")?;
    Ok(serde_json::from_str(body)?)
}

fn read_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&content)?;
    value
        .as_object()
        .cloned()
        .context("configuration file must contain a JSON object")
}

fn backup(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let parent = path.parent().context("path has no parent")?;
    let stem = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("config");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let destination = parent.join(format!("{stem}.bak.{timestamp}"));
    fs::copy(path, destination)?;
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
    fn antigravity_connect_and_disconnect_preserve_unowned_configuration() {
        let temporary = TempDir::new().unwrap();
        let paths = AntigravityPaths {
            mcp_configuration: temporary.path().join("config/mcp_config.json"),
            hooks_configuration: temporary.path().join("config/hooks.json"),
            brain_directories: vec![temporary.path().join("brain")],
        };
        fs::create_dir_all(paths.mcp_configuration.parent().unwrap()).unwrap();
        fs::write(
            &paths.mcp_configuration,
            r#"{"mcpServers":{"existing":{"command":"node","args":["server.js"]}}}"#,
        )
        .unwrap();
        fs::write(
            &paths.hooks_configuration,
            r#"{"custom-hook":{"PreInvocation":[{"type":"command","command":"echo hi"}]}}"#,
        )
        .unwrap();

        let installer = AntigravityInstaller::new(paths.clone(), "/usr/local/bin/menvane");
        assert!(installer.connect().unwrap());
        assert!(!installer.connect().unwrap());

        let mcp = read_object(&paths.mcp_configuration).unwrap();
        assert!(mcp["mcpServers"]["existing"].is_object());
        assert!(mcp["mcpServers"]["menvane"].is_object());

        let hooks = read_object(&paths.hooks_configuration).unwrap();
        assert!(hooks["custom-hook"].is_object());
        assert!(hooks["menvane"].is_object());

        assert!(installer.disconnect().unwrap());
        let mcp = read_object(&paths.mcp_configuration).unwrap();
        assert!(mcp["mcpServers"]["existing"].is_object());
        assert!(mcp["mcpServers"].get("menvane").is_none());

        let hooks = read_object(&paths.hooks_configuration).unwrap();
        assert!(hooks["custom-hook"].is_object());
        assert!(hooks.get("menvane").is_none());
    }

    #[test]
    fn normalizes_antigravity_prompt_and_tool_events() {
        let prompt_event = normalize_antigravity_event(
            "PreInvocation",
            &json!({
                "conversationId": "c-1234",
                "workspacePaths": ["/workspace/project"],
                "userMessage": "implement feature x",
                "modelName": "gemini-2.5-pro"
            }),
        )
        .unwrap();
        assert_eq!(prompt_event.client, "antigravity");
        assert_eq!(prompt_event.external_session_id, "c-1234");
        assert_eq!(prompt_event.cwd, "/workspace/project");
        assert_eq!(prompt_event.kind, NormalizedEventKind::UserPrompt);
        assert_eq!(
            prompt_event.bounded_input.as_deref(),
            Some("implement feature x")
        );
        assert_eq!(prompt_event.model.as_deref(), Some("gemini-2.5-pro"));

        let tool_event = normalize_antigravity_event(
            "PostToolUse",
            &json!({
                "conversationId": "c-1234",
                "workspacePaths": ["/workspace/project"],
                "toolCall": {
                    "name": "run_command",
                    "args": {
                        "CommandLine": "cargo test"
                    }
                },
                "output": "test result: ok"
            }),
        )
        .unwrap();
        assert_eq!(tool_event.client, "antigravity");
        assert_eq!(tool_event.kind, NormalizedEventKind::ToolCompleted);
        assert_eq!(tool_event.tool_family.as_deref(), Some("run_command"));
        assert!(tool_event.success.unwrap_or(false));
    }
}
