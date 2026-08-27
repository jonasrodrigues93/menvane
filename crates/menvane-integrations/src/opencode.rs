use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use menvane_engine::Menvane;
use serde_json::{Map, Value, json};

use crate::ClaudeHook;

#[derive(Clone)]
pub struct OpenCodePaths {
    pub configuration: PathBuf,
    pub plugin: PathBuf,
}

impl OpenCodePaths {
    pub fn discover() -> Result<Self> {
        let root = std::env::var_os("OPENCODE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("opencode"))
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".config/opencode"))
            })
            .context("HOME is not set")?;
        Ok(Self {
            configuration: root.join("opencode.json"),
            plugin: root.join("plugins/menvane.js"),
        })
    }
}

pub struct OpenCodeInstaller {
    paths: OpenCodePaths,
    executable: PathBuf,
}

impl OpenCodeInstaller {
    pub fn new(paths: OpenCodePaths, executable: impl Into<PathBuf>) -> Self {
        Self {
            paths,
            executable: executable.into(),
        }
    }

    pub fn connect(&self) -> Result<bool> {
        let mut configuration = read_object(&self.paths.configuration)?;
        let uri = format!("file://{}", self.paths.plugin.to_string_lossy());
        let plugins = configuration
            .entry("plugin")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("OpenCode plugin configuration must be an array")?;
        let mut changed = false;
        if !plugins.iter().any(|plugin| plugin.as_str() == Some(&uri)) {
            plugins.push(Value::String(uri));
            changed = true;
        }
        let mcp = configuration
            .entry("mcp")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("OpenCode mcp configuration must be an object")?;
        let expected = json!({
            "type": "local",
            "command": [self.executable.to_string_lossy(), "mcp"],
            "enabled": true
        });
        if mcp.get("menvane") != Some(&expected) {
            mcp.insert("menvane".to_owned(), expected);
            changed = true;
        }
        let plugin = plugin_source(&self.executable);
        if fs::read_to_string(&self.paths.plugin).ok().as_deref() != Some(&plugin) {
            write_text(&self.paths.plugin, &plugin)?;
            changed = true;
        }
        if changed {
            backup(&self.paths.configuration)?;
            write_json(&self.paths.configuration, &configuration)?;
        }
        Ok(changed)
    }

    pub fn disconnect(&self) -> Result<bool> {
        let mut configuration = read_object(&self.paths.configuration)?;
        let uri = format!("file://{}", self.paths.plugin.to_string_lossy());
        let mut changed = false;
        if let Some(plugins) = configuration
            .get_mut("plugin")
            .and_then(Value::as_array_mut)
        {
            let before = plugins.len();
            plugins.retain(|plugin| plugin.as_str() != Some(&uri));
            changed |= before != plugins.len();
        }
        if let Some(mcp) = configuration.get_mut("mcp").and_then(Value::as_object_mut) {
            let owned = mcp.get("menvane").is_some_and(|server| {
                server.get("command") == Some(&json!([self.executable.to_string_lossy(), "mcp"]))
            });
            if owned {
                mcp.remove("menvane");
                changed = true;
            }
        }
        if fs::read_to_string(&self.paths.plugin)
            .ok()
            .is_some_and(|source| source == plugin_source(&self.executable))
        {
            fs::remove_file(&self.paths.plugin)?;
            changed = true;
        }
        if changed {
            backup(&self.paths.configuration)?;
            write_json(&self.paths.configuration, &configuration)?;
        }
        Ok(changed)
    }
}

pub struct OpenCodeHook<'a> {
    shared: ClaudeHook<'a>,
}

impl<'a> OpenCodeHook<'a> {
    pub fn new(menvane: &'a Menvane, executable: impl Into<PathBuf>) -> Self {
        Self {
            shared: ClaudeHook::new(menvane, executable),
        }
    }

    pub fn handle(&self, event_name: &str, payload: Value) -> Result<Value> {
        self.shared.handle_client(event_name, payload, "opencode")
    }
}

fn plugin_source(executable: &Path) -> String {
    let executable = serde_json::to_string(&executable.to_string_lossy()).unwrap_or_default();
    format!(
        r#"const executable = {executable}

async function invoke(event, payload) {{
  const process = Bun.spawn([executable, "hook", "opencode", event], {{ stdin: "pipe", stdout: "pipe", stderr: "inherit", env: processEnv() }})
  process.stdin.write(JSON.stringify(payload))
  process.stdin.end()
  const output = await new Response(process.stdout).text()
  if ((await process.exited) !== 0) return {{}}
  return output.trim() ? JSON.parse(output) : {{}}
}}

function processEnv() {{ return {{ ...globalThis.process?.env }} }}

export const Menvane = async ({{ directory }}) => {{
  return {{
  event: async ({{ event }}) => {{
    const map = {{ "session.created": "SessionStart", "session.idle": "Stop", "session.compacted": "PostCompact", "session.deleted": "SessionEnd", "tool.completed": "PostToolUse" }}
    const name = map[event.type]
    const properties = event.properties || {{}}
    const sessionID = properties.sessionID || properties.session_id || properties.info?.id
    if (name && sessionID) await invoke(name, {{ ...properties, session_id: sessionID, cwd: properties.cwd || properties.info?.directory || directory, hook_event_name: name }})
  }},
  "chat.message": async (input, output) => {{
    const prompt = output.parts?.filter(part => part.type === "text").map(part => part.text || "").join("\n").trim() || ""
    if (!prompt) return
    const response = await invoke("UserPromptSubmit", {{ session_id: input.sessionID, cwd: directory, prompt, hook_event_name: "UserPromptSubmit" }})
    const context = response.hookSpecificOutput?.additionalContext
    if (context && Array.isArray(output.parts)) output.parts.push({{ type: "text", text: `\n\n${{context}}` }})
  }},
  "experimental.chat.system.transform": async (input, output) => {{
    if (!input.sessionID) return
    const response = await invoke("SessionStart", {{ session_id: input.sessionID, cwd: directory, hook_event_name: "SessionStart", source: "startup" }})
    const context = response.hookSpecificOutput?.additionalContext
    if (context && !output.system.includes(context)) output.system.push(context)
  }},
  "tool.execute.after": async (input, output) => {{ await invoke("PostToolUse", {{ session_id: input.sessionID, cwd: directory, tool_name: input.tool, tool_input: input.args, tool_response: output, hook_event_name: "PostToolUse" }}) }}
  }}
}}
"#
    )
}

fn read_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    serde_json::from_slice::<Value>(&fs::read(path)?)?
        .as_object()
        .cloned()
        .context("OpenCode configuration root must be an object")
}

fn write_json(path: &Path, value: &Map<String, Value>) -> Result<()> {
    write_text(path, &format!("{}\n", serde_json::to_string_pretty(value)?))
}

fn write_text(path: &Path, value: &str) -> Result<()> {
    let parent = path.parent().context("OpenCode path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".menvane-opencode-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn backup(path: &Path) -> Result<()> {
    if path.exists() {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        fs::copy(
            path,
            path.with_extension(format!("json.menvane-backup-{timestamp}")),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn opencode_install_is_minimal_idempotent_and_preserving() {
        let temporary = TempDir::new().unwrap();
        let paths = OpenCodePaths {
            configuration: temporary.path().join("opencode.json"),
            plugin: temporary.path().join("plugins/menvane.js"),
        };
        fs::write(
            &paths.configuration,
            r#"{"theme":"existing","plugin":["other"]}"#,
        )
        .unwrap();
        let installer = OpenCodeInstaller::new(paths.clone(), "/opt/menvane");
        assert!(installer.connect().unwrap());
        assert!(!installer.connect().unwrap());
        let connected = read_object(&paths.configuration).unwrap();
        assert_eq!(connected["theme"], "existing");
        assert!(
            connected["plugin"]
                .as_array()
                .unwrap()
                .contains(&json!("other"))
        );
        let source = fs::read_to_string(&paths.plugin).unwrap();
        assert!(!source.contains("rank"));
        assert!(!source.contains("consolidat"));
        assert!(source.contains("output.parts?.filter"));
        assert!(source.contains("properties.info?.id"));
        assert!(source.contains("experimental.chat.system.transform"));
        assert!(source.contains("const response = await invoke(\"UserPromptSubmit\""));
        assert!(source.contains("response.hookSpecificOutput?.additionalContext"));
        assert!(source.contains("output.parts.push"));
        assert!(source.contains("output.system.push"));
        assert!(!source.contains("output.options.system"));
        assert!(installer.disconnect().unwrap());
        let disconnected = read_object(&paths.configuration).unwrap();
        assert!(
            disconnected["plugin"]
                .as_array()
                .unwrap()
                .contains(&json!("other"))
        );
        assert!(!paths.plugin.exists());
    }
}
