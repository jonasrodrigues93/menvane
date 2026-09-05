use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use menvane_engine::Menvane;
use menvane_integrations::{
    AntigravityInstaller, AntigravityPaths, ClaudeInstaller, ClaudePaths, CodexInstaller,
    CodexPaths, OpenCodeInstaller, OpenCodePaths,
};
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

pub const SETUP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    Claude,
    Codex,
    Opencode,
    Antigravity,
}

impl Agent {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Opencode, Self::Antigravity];

    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Antigravity => "antigravity",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupOptions {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub home: PathBuf,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub embedding_provider: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_base_url: Option<String>,
    pub embedding_api_key: Option<String>,
    pub embedding_api_key_env: Option<String>,
    pub embedding_min_similarity: Option<f64>,
    pub max_prompt_bytes: Option<u64>,
    pub max_tool_input_bytes: Option<u64>,
    pub max_tool_output_bytes: Option<u64>,
    pub idle_finalize_seconds: Option<u64>,
    pub open_finalize_seconds: Option<u64>,
    pub lease_timeout_seconds: Option<u64>,
    pub memory_lifetime_days: Option<u64>,
    pub min_match_confidence: Option<f64>,
    pub min_knowledge_confidence: Option<f64>,
    pub min_utility: Option<f64>,
    pub max_cards: Option<u64>,
    #[serde(default)]
    pub agents: Vec<Agent>,
}

fn default_schema_version() -> u32 {
    SETUP_SCHEMA_VERSION
}

impl SetupOptions {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            schema_version: SETUP_SCHEMA_VERSION,
            home: home.into(),
            provider: None,
            model: None,
            reasoning_effort: None,
            base_url: None,
            api_key: None,
            api_key_env: None,
            embedding_provider: None,
            embedding_model: None,
            embedding_base_url: None,
            embedding_api_key: None,
            embedding_api_key_env: None,
            embedding_min_similarity: None,
            max_prompt_bytes: None,
            max_tool_input_bytes: None,
            max_tool_output_bytes: None,
            idle_finalize_seconds: None,
            open_finalize_seconds: None,
            lease_timeout_seconds: None,
            memory_lifetime_days: None,
            min_match_confidence: None,
            min_knowledge_confidence: None,
            min_utility: None,
            max_cards: None,
            agents: Vec::new(),
        }
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let options: Self = toml::from_str(text).context("invalid setup manifest")?;
        if options.schema_version != SETUP_SCHEMA_VERSION {
            bail!(
                "unsupported setup manifest schema {}",
                options.schema_version
            );
        }
        Ok(options)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupPlan {
    pub schema_version: u32,
    pub home: PathBuf,
    pub configuration: String,
    pub actions: Vec<String>,
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupReport {
    pub home: PathBuf,
    pub actions: Vec<String>,
    pub service_enabled: bool,
}

pub fn prepare(options: &SetupOptions, executable: &Path) -> Result<SetupPlan> {
    validate_options(options)?;
    let configuration_path = options.home.join("config.toml");
    let source = fs::read_to_string(&configuration_path)
        .unwrap_or_else(|_| menvane_store::default_config_text().to_owned());
    let mut configuration: Table = toml::from_str(&source).context("invalid config.toml")?;
    patch_configuration(&mut configuration, options)?;
    let configuration = toml::to_string_pretty(&configuration)?;
    let mut actions = vec![format!("configure {}", options.home.display())];
    for agent in &options.agents {
        actions.push(format!("connect {}", agent.key()));
    }
    actions.push(format!("enable and start {}", executable.display()));
    Ok(SetupPlan {
        schema_version: SETUP_SCHEMA_VERSION,
        home: options.home.clone(),
        configuration,
        actions,
        agents: options.agents.clone(),
    })
}

pub fn apply(options: &SetupOptions, executable: &Path) -> Result<SetupReport> {
    let plan = prepare(options, executable)?;
    let snapshots = Snapshots::capture(&plan, options)?;
    let result = apply_plan(&plan, options, executable);
    if result.is_err() {
        snapshots.restore()?;
    }
    result
}

fn apply_plan(plan: &SetupPlan, options: &SetupOptions, executable: &Path) -> Result<SetupReport> {
    fs::create_dir_all(&plan.home)?;
    let configuration_path = plan.home.join("config.toml");
    backup_file(&configuration_path)?;
    atomic_write(&configuration_path, plan.configuration.as_bytes())?;
    for agent in &plan.agents {
        connect_agent(*agent, executable)?;
    }
    let menvane = Menvane::new(&plan.home)?;
    for agent in &plan.agents {
        menvane.set_integration_connected(agent.key(), true)?;
    }
    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", "--now", "menvane.service"])?;
    Ok(SetupReport {
        home: options.home.clone(),
        actions: plan.actions.clone(),
        service_enabled: true,
    })
}

fn connect_agent(agent: Agent, executable: &Path) -> Result<()> {
    match agent {
        Agent::Claude => ClaudeInstaller::new(ClaudePaths::discover()?, executable).connect(),
        Agent::Codex => CodexInstaller::new(CodexPaths::discover()?, executable).connect(),
        Agent::Opencode => OpenCodeInstaller::new(OpenCodePaths::discover()?, executable).connect(),
        Agent::Antigravity => {
            AntigravityInstaller::new(AntigravityPaths::discover()?, executable).connect()
        }
    }
    .map(|_| ())
}

fn run_systemctl(arguments: &[&str]) -> Result<()> {
    let status = Command::new("systemctl").args(arguments).status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!(
            "systemctl {} failed with status {status}",
            arguments.join(" ")
        ),
        Err(error) => Err(error).context("systemctl --user is required to start Menvane"),
    }
}

fn backup_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .context("backup path has no file name")?
        .to_string_lossy();
    let destination = path.with_file_name(format!("{name}.menvane-backup-{timestamp}"));
    fs::copy(path, destination)?;
    Ok(())
}

fn validate_options(options: &SetupOptions) -> Result<()> {
    if options.home.as_os_str().is_empty() {
        bail!("MENVANE_HOME cannot be empty");
    }
    if options.schema_version != SETUP_SCHEMA_VERSION {
        bail!(
            "unsupported setup manifest schema {}",
            options.schema_version
        );
    }
    if let Some(provider) = &options.provider
        && !matches!(
            provider.as_str(),
            "openai" | "openai-api" | "openrouter" | "codex" | "github-copilot"
        )
    {
        bail!("unsupported provider {provider}");
    }
    if let Some(model) = &options.model
        && (model.trim().is_empty() || model.contains('\0'))
    {
        bail!("model cannot be empty or contain NUL");
    }
    if let Some(effort) = &options.reasoning_effort
        && !matches!(
            effort.as_str(),
            "minimal" | "low" | "medium" | "high" | "xhigh"
        )
    {
        bail!("reasoning effort must be minimal, low, medium, high, or xhigh");
    }
    for (name, value) in [
        ("max_prompt_bytes", options.max_prompt_bytes),
        ("max_tool_input_bytes", options.max_tool_input_bytes),
        ("max_tool_output_bytes", options.max_tool_output_bytes),
        ("idle_finalize_seconds", options.idle_finalize_seconds),
        ("open_finalize_seconds", options.open_finalize_seconds),
        ("lease_timeout_seconds", options.lease_timeout_seconds),
        ("memory_lifetime_days", options.memory_lifetime_days),
    ] {
        if value.is_some_and(|value| value == 0 || value > i64::MAX as u64) {
            bail!("{name} must be between one and {}", i64::MAX);
        }
    }
    for (name, value) in [
        ("min_match_confidence", options.min_match_confidence),
        ("min_knowledge_confidence", options.min_knowledge_confidence),
        ("min_utility", options.min_utility),
        ("embedding_min_similarity", options.embedding_min_similarity),
    ] {
        if value.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
            bail!("{name} must be between zero and one");
        }
    }
    if options
        .max_cards
        .is_some_and(|value| !(1..=3).contains(&value))
    {
        bail!("max_cards must be between one and three");
    }
    for value in [
        options.api_key.as_deref(),
        options.embedding_api_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.contains('\0') || value.contains('\n') || value.contains('\r') {
            bail!("API keys cannot contain control characters");
        }
    }
    Ok(())
}

fn patch_configuration(configuration: &mut Table, options: &SetupOptions) -> Result<()> {
    set_string(
        configuration,
        "llm",
        "provider",
        options.provider.as_deref(),
    )?;
    set_string(configuration, "llm", "model", options.model.as_deref())?;
    set_string(
        configuration,
        "llm",
        "reasoning_effort",
        options.reasoning_effort.as_deref(),
    )?;
    set_string(
        configuration,
        "llm",
        "base_url",
        options.base_url.as_deref(),
    )?;
    set_string(configuration, "llm", "api_key", options.api_key.as_deref())?;
    set_string(
        configuration,
        "llm",
        "api_key_env",
        options.api_key_env.as_deref(),
    )?;
    set_string(
        configuration,
        "embeddings",
        "provider",
        options.embedding_provider.as_deref(),
    )?;
    set_string(
        configuration,
        "embeddings",
        "model",
        options.embedding_model.as_deref(),
    )?;
    set_string(
        configuration,
        "embeddings",
        "base_url",
        options.embedding_base_url.as_deref(),
    )?;
    set_string(
        configuration,
        "embeddings",
        "api_key",
        options.embedding_api_key.as_deref(),
    )?;
    set_string(
        configuration,
        "embeddings",
        "api_key_env",
        options.embedding_api_key_env.as_deref(),
    )?;
    set_float(
        configuration,
        "embeddings",
        "min_similarity",
        options.embedding_min_similarity,
    )?;
    set_integer(
        configuration,
        "capture",
        "max_prompt_bytes",
        options.max_prompt_bytes,
    )?;
    set_integer(
        configuration,
        "capture",
        "max_tool_input_bytes",
        options.max_tool_input_bytes,
    )?;
    set_integer(
        configuration,
        "capture",
        "max_tool_output_bytes",
        options.max_tool_output_bytes,
    )?;
    set_integer(
        configuration,
        "sessions",
        "idle_finalize_seconds",
        options.idle_finalize_seconds,
    )?;
    set_integer(
        configuration,
        "sessions",
        "open_finalize_seconds",
        options.open_finalize_seconds,
    )?;
    set_integer(
        configuration,
        "jobs",
        "lease_timeout_seconds",
        options.lease_timeout_seconds,
    )?;
    set_integer(
        configuration,
        "decay",
        "memory_lifetime_days",
        options.memory_lifetime_days,
    )?;
    set_float(
        configuration,
        "recall",
        "min_match_confidence",
        options.min_match_confidence,
    )?;
    set_float(
        configuration,
        "recall",
        "min_knowledge_confidence",
        options.min_knowledge_confidence,
    )?;
    set_float(configuration, "recall", "min_utility", options.min_utility)?;
    set_integer(configuration, "recall", "max_cards", options.max_cards)?;
    Ok(())
}

fn set_string(
    configuration: &mut Table,
    section: &str,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    if let Some(value) = value {
        section_table(configuration, section)?
            .insert(key.to_owned(), Value::String(value.trim().to_owned()));
    }
    Ok(())
}

fn set_integer(
    configuration: &mut Table,
    section: &str,
    key: &str,
    value: Option<u64>,
) -> Result<()> {
    if let Some(value) = value {
        section_table(configuration, section)?.insert(key.to_owned(), Value::Integer(value as i64));
    }
    Ok(())
}

fn set_float(
    configuration: &mut Table,
    section: &str,
    key: &str,
    value: Option<f64>,
) -> Result<()> {
    if let Some(value) = value {
        section_table(configuration, section)?.insert(key.to_owned(), Value::Float(value));
    }
    Ok(())
}

fn section_table<'a>(configuration: &'a mut Table, section: &str) -> Result<&'a mut Table> {
    configuration
        .entry(section.to_owned())
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .with_context(|| format!("{section} configuration must be a table"))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.as_file().write_all(contents)?;
    temporary.as_file().sync_all()?;
    let permissions = temporary.as_file().metadata()?.permissions();
    let mut permissions = permissions;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o600);
        fs::set_permissions(temporary.path(), permissions)?;
    }
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct Snapshots {
    files: Vec<(PathBuf, Option<Vec<u8>>)>,
}

impl Snapshots {
    fn capture(plan: &SetupPlan, options: &SetupOptions) -> Result<Self> {
        let mut paths = vec![
            plan.home.join("config.toml"),
            plan.home.join("index.sqlite"),
            plan.home.join("state.sqlite"),
        ];
        for agent in &options.agents {
            paths.extend(agent_paths(*agent)?);
        }
        paths.sort();
        paths.dedup();
        Ok(Self {
            files: paths
                .into_iter()
                .map(|path| {
                    let contents = fs::read(&path).ok();
                    (path, contents)
                })
                .collect(),
        })
    }

    fn restore(&self) -> Result<()> {
        for (path, contents) in &self.files {
            match contents {
                Some(contents) => atomic_write(path, contents)?,
                None => {
                    if path.exists() {
                        fs::remove_file(path)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn agent_paths(agent: Agent) -> Result<Vec<PathBuf>> {
    Ok(match agent {
        Agent::Claude => {
            let paths = ClaudePaths::discover()?;
            vec![paths.settings, paths.configuration]
        }
        Agent::Codex => vec![CodexPaths::discover()?.configuration],
        Agent::Opencode => {
            let paths = OpenCodePaths::discover()?;
            vec![paths.configuration, paths.plugin]
        }
        Agent::Antigravity => {
            let paths = AntigravityPaths::discover()?;
            vec![paths.mcp_configuration, paths.hooks_configuration]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn prepare_preserves_unknown_configuration() {
        let temporary = TempDir::new().unwrap();
        let home = temporary.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "[custom]\nvalue = \"kept\"\n\n[llm]\nmodel = \"old\"\n",
        )
        .unwrap();
        let mut options = SetupOptions::new(&home);
        options.model = Some("new-model".to_owned());
        let plan = prepare(&options, Path::new("/usr/bin/menvane")).unwrap();
        assert!(plan.configuration.contains("value = \"kept\""));
        assert!(plan.configuration.contains("model = \"new-model\""));
    }

    #[test]
    fn invalid_manifest_is_rejected_before_writing() {
        let temporary = TempDir::new().unwrap();
        let mut options = SetupOptions::new(temporary.path().join("home"));
        options.max_cards = Some(4);
        assert!(prepare(&options, Path::new("menvane")).is_err());
        assert!(!options.home.exists());
    }
}
