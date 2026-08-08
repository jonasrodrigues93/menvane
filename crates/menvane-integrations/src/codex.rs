use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use menvane_engine::Menvane;
use serde_json::Value;

use crate::ClaudeHook;

const EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "Stop",
    "SessionEnd",
];

#[derive(Clone)]
pub struct CodexPaths {
    pub configuration: PathBuf,
}

impl CodexPaths {
    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
            .context("HOME is not set")?;
        Ok(Self {
            configuration: home.join("config.toml"),
        })
    }
}

pub struct CodexInstaller {
    paths: CodexPaths,
    executable: PathBuf,
}

impl CodexInstaller {
    pub fn new(paths: CodexPaths, executable: impl Into<PathBuf>) -> Self {
        Self {
            paths,
            executable: executable.into(),
        }
    }

    pub fn connect(&self) -> Result<bool> {
        let mut configuration = read_configuration(&self.paths.configuration)?;
        let changed = install(&mut configuration, &self.executable)?;
        if changed {
            backup(&self.paths.configuration)?;
            write_configuration(&self.paths.configuration, &configuration)?;
        }
        Ok(changed)
    }

    pub fn disconnect(&self) -> Result<bool> {
        let mut configuration = read_configuration(&self.paths.configuration)?;
        let changed = remove(&mut configuration, &self.executable);
        if changed {
            backup(&self.paths.configuration)?;
            write_configuration(&self.paths.configuration, &configuration)?;
        }
        Ok(changed)
    }
}

pub struct CodexHook<'a> {
    shared: ClaudeHook<'a>,
}

impl<'a> CodexHook<'a> {
    pub fn new(menvane: &'a Menvane, executable: impl Into<PathBuf>) -> Self {
        Self {
            shared: ClaudeHook::new(menvane, executable),
        }
    }

    pub fn handle(&self, event_name: &str, payload: Value) -> Result<Value> {
        self.shared.handle_client(event_name, payload, "codex")
    }
}

fn install(configuration: &mut toml::Table, executable: &Path) -> Result<bool> {
    let mut changed = false;
    let features = table_entry(configuration, "features")?;
    if features.get("hooks") != Some(&toml::Value::Boolean(true)) {
        features.insert("hooks".to_owned(), toml::Value::Boolean(true));
        changed = true;
    }
    let servers = table_entry(configuration, "mcp_servers")?;
    let expected_server: toml::Value = toml::from_str(&format!(
        "command = {:?}\nargs = [\"mcp\"]\nenabled = true\nrequired = false\n",
        executable.to_string_lossy()
    ))?;
    if servers.get("menvane") != Some(&expected_server) {
        servers.insert("menvane".to_owned(), expected_server);
        changed = true;
    }
    let hooks = table_entry(configuration, "hooks")?;
    for event in EVENTS {
        let command = format!("'{}' hook codex {event}", executable.to_string_lossy());
        let groups = hooks
            .entry(event)
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| format!("Codex {event} hooks must be an array"))?;
        let exists = groups.iter().any(|group| {
            group
                .get("hooks")
                .and_then(toml::Value::as_array)
                .is_some_and(|handlers| {
                    handlers.iter().any(|handler| {
                        handler.get("command").and_then(toml::Value::as_str) == Some(&command)
                    })
                })
        });
        if !exists {
            groups.push(toml::Value::Table(toml::Table::from_iter([(
                "hooks".to_owned(),
                toml::Value::Array(vec![toml::Value::Table(toml::Table::from_iter([
                    ("type".to_owned(), toml::Value::String("command".to_owned())),
                    ("command".to_owned(), toml::Value::String(command)),
                    ("timeout".to_owned(), toml::Value::Integer(3)),
                    (
                        "additionalContextLimit".to_owned(),
                        toml::Value::Integer(6_000),
                    ),
                ]))]),
            )])));
            changed = true;
        }
    }
    Ok(changed)
}

fn remove(configuration: &mut toml::Table, executable: &Path) -> bool {
    let mut changed = false;
    if let Some(servers) = configuration
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
    {
        let owned = servers.get("menvane").is_some_and(|server| {
            server.get("command").and_then(toml::Value::as_str)
                == Some(&*executable.to_string_lossy())
                && server
                    .get("args")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|args| args.len() == 1 && args[0].as_str() == Some("mcp"))
        });
        if owned {
            servers.remove("menvane");
            changed = true;
        }
    }
    if let Some(hooks) = configuration
        .get_mut("hooks")
        .and_then(toml::Value::as_table_mut)
    {
        for event in EVENTS {
            let command = format!("'{}' hook codex {event}", executable.to_string_lossy());
            if let Some(groups) = hooks.get_mut(event).and_then(toml::Value::as_array_mut) {
                for group in groups.iter_mut() {
                    if let Some(handlers) =
                        group.get_mut("hooks").and_then(toml::Value::as_array_mut)
                    {
                        let before = handlers.len();
                        handlers.retain(|handler| {
                            handler.get("command").and_then(toml::Value::as_str) != Some(&command)
                        });
                        changed |= before != handlers.len();
                    }
                }
                groups.retain(|group| {
                    group
                        .get("hooks")
                        .and_then(toml::Value::as_array)
                        .is_none_or(|handlers| !handlers.is_empty())
                });
            }
        }
    }
    changed
}

fn table_entry<'a>(configuration: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table> {
    configuration
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .with_context(|| format!("Codex {key} configuration must be a table"))
}

fn read_configuration(path: &Path) -> Result<toml::Table> {
    if !path.exists() {
        return Ok(toml::Table::new());
    }
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}

fn write_configuration(path: &Path, configuration: &toml::Table) -> Result<()> {
    let parent = path.parent().context("Codex config has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".menvane-codex-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(toml::to_string_pretty(configuration)?.as_bytes())?;
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
            path.with_extension(format!("toml.menvane-backup-{timestamp}")),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn codex_install_preserves_configuration_and_is_idempotent() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        fs::write(
            &path,
            "model = \"existing\"\n[mcp_servers.other]\ncommand = \"other\"\n",
        )
        .unwrap();
        let installer = CodexInstaller::new(
            CodexPaths {
                configuration: path.clone(),
            },
            "/opt/menvane",
        );
        assert!(installer.connect().unwrap());
        assert!(!installer.connect().unwrap());
        let connected = read_configuration(&path).unwrap();
        assert_eq!(connected["model"].as_str(), Some("existing"));
        assert!(connected["mcp_servers"]["other"].is_table());
        assert!(connected["mcp_servers"]["menvane"].is_table());
        assert!(installer.disconnect().unwrap());
        let disconnected = read_configuration(&path).unwrap();
        assert!(disconnected["mcp_servers"]["other"].is_table());
        assert!(disconnected["mcp_servers"].get("menvane").is_none());
    }
}
