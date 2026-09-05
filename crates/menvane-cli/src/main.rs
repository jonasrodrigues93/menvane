use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono::{Duration, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use menvane_domain::{Applicability, KnowledgeType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use menvane_integrations::{
    AntigravityHook, AntigravityInstaller, AntigravityPaths, ClaudeHook, ClaudeInstaller,
    ClaudePaths, CodexHook, CodexInstaller, CodexPaths, JsonlImporter, McpServer, OpenCodeHook,
    OpenCodeImporter, OpenCodeInstaller, OpenCodePaths,
};
use menvane_server::{
    DEFAULT_ADDRESS, DEFAULT_PORT, daemon_running, home_from_environment, serve, start_daemon,
    stop_daemon,
};
use menvane_setup::{Agent, SetupOptions};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "menvane",
    version,
    about = "Local persistent memory for agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn run_setup(arguments: &SetupArgs) -> Result<()> {
    if arguments.visual {
        if arguments.from.is_some() || arguments.non_interactive || arguments.output_json {
            bail!("--visual cannot be combined with manifest or non-interactive options");
        }
        let status = std::process::Command::new("menvane-setup").status()?;
        if !status.success() {
            bail!("visual setup exited with status {status}");
        }
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let options = if arguments.non_interactive {
        let mut manifest = String::new();
        match arguments.from.as_deref() {
            Some(path) if path.as_os_str() != "-" => {
                std::fs::File::open(path)?.read_to_string(&mut manifest)?;
            }
            _ => {
                io::stdin().read_to_string(&mut manifest)?;
            }
        }
        SetupOptions::from_toml(&manifest)?
    } else {
        interactive_setup()?
    };
    let report = menvane_setup::apply(&options, &executable)?;
    if arguments.output_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("setup complete");
        println!("home\t{}", report.home.display());
        println!("service\tenabled and started");
        for action in report.actions {
            println!("action\t{action}");
        }
    }
    Ok(())
}

fn interactive_setup() -> Result<SetupOptions> {
    let home = home_from_environment()?;
    println!("Menvane setup");
    println!("No daemon will start until the final confirmation succeeds.");
    let mut options = SetupOptions::new(home);
    options.provider = Some(prompt("Provider", "openai")?);
    options.model = Some(prompt("Model", "gpt-5.6-luna")?);
    options.reasoning_effort = Some(prompt("Reasoning effort", "medium")?);
    options.base_url = Some(prompt("Provider endpoint", "https://api.openai.com/v1")?);
    let api_key = rpassword::prompt_password("API key (leave empty to use api_key_env): ")?;
    if !api_key.trim().is_empty() {
        options.api_key = Some(api_key);
    }
    options.api_key_env = Some(prompt("API key environment variable", "OPENAI_API_KEY")?);
    if prompt_bool("Enable embeddings", false)? {
        options.embedding_provider = Some(prompt("Embedding provider", "openai-api")?);
        options.embedding_model = Some(prompt("Embedding model", "text-embedding-3-small")?);
        options.embedding_base_url =
            Some(prompt("Embedding endpoint", "https://api.openai.com/v1")?);
        let embedding_key = rpassword::prompt_password(
            "Embedding API key (leave empty to reuse the environment variable): ",
        )?;
        if !embedding_key.trim().is_empty() {
            options.embedding_api_key = Some(embedding_key);
        }
        options.embedding_api_key_env = Some(prompt(
            "Embedding API key environment variable",
            "OPENAI_API_KEY",
        )?);
        options.embedding_min_similarity = Some(
            prompt("Minimum embedding similarity", "0.78")?
                .parse()
                .map_err(|_| anyhow::anyhow!("minimum embedding similarity must be a number"))?,
        );
    }
    options.max_prompt_bytes = Some(prompt_u64("Maximum prompt bytes", 16_384)?);
    options.max_tool_input_bytes = Some(prompt_u64("Maximum tool input bytes", 4_096)?);
    options.max_tool_output_bytes = Some(prompt_u64("Maximum tool output bytes", 4_096)?);
    options.idle_finalize_seconds = Some(prompt_u64("Idle finalization seconds", 120)?);
    options.open_finalize_seconds = Some(prompt_u64("Open session timeout seconds", 1_800)?);
    options.lease_timeout_seconds = Some(prompt_u64("Job lease timeout seconds", 300)?);
    options.memory_lifetime_days = Some(prompt_u64("Memory lifetime days", 90)?);
    options.min_match_confidence = Some(prompt("Minimum match confidence", "0.45")?.parse()?);
    options.min_knowledge_confidence =
        Some(prompt("Minimum knowledge confidence", "0.55")?.parse()?);
    options.min_utility = Some(prompt("Minimum utility", "0.55")?.parse()?);
    options.max_cards = Some(prompt_u64("Maximum knowledge cards", 3)?);
    options.agents = selected_agents()?;
    println!("\nThe following changes will be applied:");
    println!("- configuration at {}", options.home.display());
    for agent in &options.agents {
        println!("- connect {}", agent.key());
    }
    println!("- enable and start menvane.service");
    if !prompt_bool("Apply and start the service now", false)? {
        bail!("setup cancelled; no changes were applied");
    }
    Ok(options)
}

fn selected_agents() -> Result<Vec<Agent>> {
    let mut selected = Vec::new();
    for (name, agent, detected) in detected_agents()? {
        println!(
            "{name}: {}",
            if detected { "detected" } else { "not detected" }
        );
        if prompt_bool(&format!("Connect {name}"), detected)? {
            selected.push(agent);
        }
    }
    Ok(selected)
}

fn detected_agents() -> Result<Vec<(&'static str, Agent, bool)>> {
    let claude = ClaudePaths::discover()?;
    let codex = CodexPaths::discover()?;
    let opencode = OpenCodePaths::discover()?;
    let antigravity = AntigravityPaths::discover()?;
    Ok(vec![
        (
            "Claude Code",
            Agent::Claude,
            claude.settings.exists() || claude.configuration.exists(),
        ),
        ("Codex", Agent::Codex, codex.configuration.exists()),
        (
            "OpenCode",
            Agent::Opencode,
            opencode.configuration.exists() || opencode.plugin.exists(),
        ),
        (
            "Antigravity",
            Agent::Antigravity,
            antigravity.mcp_configuration.exists() || antigravity.hooks_configuration.exists(),
        ),
    ])
}

fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    })
}

fn prompt_u64(label: &str, default: u64) -> Result<u64> {
    prompt(label, &default.to_string())?
        .parse()
        .map_err(Into::into)
}

fn prompt_bool(label: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        print!("{label} [{hint}]: ");
        io::stdout().flush()?;
        let mut value = String::new();
        io::stdin().read_line(&mut value)?;
        match value.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please answer yes or no."),
        }
    }
}

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Daemon(DaemonArgs),
    Connect(ConnectArgs),
    Disconnect(ClientArgs),
    Hook(HookArgs),
    Provider(ProviderArgs),
    Import(ImportArgs),
    Jobs(JobsArgs),
    Backup(BackupArgs),
    Restore(RestoreArgs),
    Write(WriteArgs),
    Search(SearchArgs),
    Read(ReadArgs),
    Forget(ForgetArgs),
    Reindex,
    Doctor,
    Handoff(HandoffArgs),
    Mcp,
    Setup(SetupArgs),
}

#[derive(Args)]
struct SetupArgs {
    #[arg(long)]
    visual: bool,
    #[arg(long, value_name = "FILE")]
    from: Option<PathBuf>,
    #[arg(long)]
    non_interactive: bool,
    #[arg(long)]
    output_json: bool,
}

#[derive(Args)]
struct HandoffArgs {
    #[command(subcommand)]
    command: HandoffCommand,
}

#[derive(Subcommand)]
enum HandoffCommand {
    /// Inspect the single current project handoff summary and its provenance.
    Inspect,
}

#[derive(Args)]
struct BackupArgs {
    output: PathBuf,
}

#[derive(Args)]
struct RestoreArgs {
    source: PathBuf,
    #[arg(long, help = "Confirm replacement of current Menvane state")]
    confirm: bool,
}

#[derive(Args)]
struct ImportArgs {
    #[arg(value_enum)]
    client: Client,
    #[arg(value_name = "DAYS", value_parser = parse_days)]
    days: Option<i64>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long, default_value = "http://127.0.0.1:4096")]
    url: String,
}

#[derive(Args)]
struct JobsArgs {
    #[command(subcommand)]
    command: JobsCommand,
}

#[derive(Subcommand)]
enum JobsCommand {
    Retry,
}

fn parse_days(value: &str) -> Result<i64, String> {
    let days = value
        .strip_suffix('d')
        .ok_or_else(|| "the time window must use days, for example 7d".to_owned())?
        .parse::<i64>()
        .map_err(|_| {
            "the time window must be a positive number of days, for example 7d".to_owned()
        })?;
    if days <= 0 {
        return Err("the time window must be a positive number of days, for example 7d".to_owned());
    }
    Ok(days)
}

#[cfg(test)]
mod tests {
    use super::{Cli, parse_days};
    use clap::Parser;
    use jsonschema::validator_for;
    use serde_json::Value;

    #[test]
    fn parses_only_positive_day_windows() {
        assert_eq!(parse_days("7d"), Ok(7));
        assert!(parse_days("7").is_err());
        assert!(parse_days("7h").is_err());
        assert!(parse_days("0d").is_err());
    }

    #[test]
    fn parses_handoff_inspection_diagnostics() {
        assert!(Cli::try_parse_from(["menvane", "handoff", "inspect"]).is_ok());
    }

    #[test]
    fn parses_github_copilot_provider_commands() {
        assert!(
            Cli::try_parse_from([
                "menvane",
                "provider",
                "configure",
                "github-copilot",
                "--model",
                "gpt-4.1",
                "--client-id",
                "client-id"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["menvane", "provider", "login", "github-copilot"]).is_ok());
    }

    #[test]
    fn parses_antigravity_commands() {
        assert!(Cli::try_parse_from(["menvane", "connect", "antigravity"]).is_ok());
        assert!(Cli::try_parse_from(["menvane", "disconnect", "antigravity"]).is_ok());
        assert!(Cli::try_parse_from(["menvane", "hook", "antigravity", "PreInvocation"]).is_ok());
        assert!(Cli::try_parse_from(["menvane", "import", "antigravity", "7d"]).is_ok());
    }

    #[test]
    fn handoff_inspect_contract_is_versioned() {
        let value: Value = serde_json::json!({
            "project_id": "project",
            "text": "# Current Handoff\n",
            "items": []
        });
        let schema: Value = serde_json::from_str(include_str!(
            "../../../contracts/v1/cli-handoff-inspect.schema.json"
        ))
        .unwrap();
        assert!(validator_for(&schema).unwrap().is_valid(&value));
    }
}

#[derive(Args)]
struct ProviderArgs {
    #[command(subcommand)]
    command: ProviderCommand,
}

#[derive(Subcommand)]
enum ProviderCommand {
    Status,
    Test,
    Configure(ProviderConfigureArgs),
    Login(ProviderAuthArgs),
    Logout(ProviderAuthArgs),
}

#[derive(Args)]
struct ProviderAuthArgs {
    #[arg(value_enum)]
    provider: ConfigurableProvider,
}

#[derive(Args)]
struct ProviderConfigureArgs {
    #[arg(value_enum)]
    provider: ConfigurableProvider,
    #[arg(long)]
    model: String,
    #[arg(long, value_enum, default_value = "medium")]
    reasoning_effort: ReasoningEffort,
    #[arg(long)]
    client_id: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ConfigurableProvider {
    Openai,
    GithubCopilot,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ReasoningEffort {
    fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }
}

#[derive(Args)]
struct ClientArgs {
    #[arg(value_enum)]
    client: Client,
}

#[derive(Args)]
struct ConnectArgs {
    #[arg(value_enum)]
    client: ConnectClient,
}

#[derive(Clone, Copy, ValueEnum)]
enum ConnectClient {
    Claude,
    Codex,
    Opencode,
    Antigravity,
    All,
}

#[derive(Args)]
struct HookArgs {
    #[arg(value_enum)]
    client: Client,
    event: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum Client {
    Claude,
    Codex,
    Opencode,
    Antigravity,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, default_value = DEFAULT_ADDRESS)]
    address: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

#[derive(Args)]
struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Subcommand)]
enum DaemonCommand {
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Args)]
struct WriteArgs {
    #[arg(long)]
    title: String,
    #[arg(long, help = "Markdown content below the title")]
    content: String,
    #[arg(long, value_enum)]
    r#type: WritableType,
    #[arg(long, value_enum, default_value = "project")]
    scope: PhysicalScope,
    #[arg(long, value_delimiter = ',')]
    tags: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    languages: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    frameworks: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    tools: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    databases: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    platforms: Vec<String>,
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
}

#[derive(Args)]
struct SearchArgs {
    query: String,
    #[arg(long, value_enum, default_value = "auto")]
    scope: SearchScopeArg,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
}

#[derive(Args)]
struct ReadArgs {
    id: Uuid,
}

#[derive(Args)]
struct ForgetArgs {
    id: Uuid,
    #[arg(long)]
    reason: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum WritableType {
    Memory,
    Playbook,
}

#[derive(Clone, Copy, ValueEnum)]
enum PhysicalScope {
    Global,
    Project,
}

#[derive(Clone, Copy, ValueEnum)]
enum SearchScopeArg {
    Auto,
    Project,
    Global,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Setup(arguments) = &cli.command {
        run_setup(arguments)?;
        return Ok(());
    }
    let menvane = Menvane::from_environment()?;
    match cli.command {
        Command::Serve(arguments) => {
            serve(menvane, &arguments.address, arguments.port).await?;
        }
        Command::Daemon(arguments) => {
            let home = home_from_environment()?;
            match arguments.command {
                DaemonCommand::Start => {
                    let pid = start_daemon(&home, &std::env::current_exe()?)?;
                    println!("started daemon process {pid}");
                }
                DaemonCommand::Stop => {
                    stop_daemon(&home)?;
                    println!("stopped daemon");
                }
                DaemonCommand::Restart => {
                    if daemon_running(&home) {
                        stop_daemon(&home)?;
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                    let pid = start_daemon(&home, &std::env::current_exe()?)?;
                    println!("restarted daemon process {pid}");
                }
                DaemonCommand::Status => {
                    if daemon_running(&home) {
                        println!("running");
                    } else {
                        bail!("daemon is not running");
                    }
                }
            }
        }
        Command::Connect(arguments) => match arguments.client {
            ConnectClient::Claude => {
                let installer =
                    ClaudeInstaller::new(ClaudePaths::discover()?, std::env::current_exe()?);
                let changed = installer.connect()?;
                menvane.set_integration_connected("claude-code", true)?;
                println!(
                    "Claude Code integration {}",
                    if changed {
                        "connected"
                    } else {
                        "already connected"
                    }
                );
            }
            ConnectClient::Codex => {
                let installer =
                    CodexInstaller::new(CodexPaths::discover()?, std::env::current_exe()?);
                let changed = installer.connect()?;
                menvane.set_integration_connected("codex", true)?;
                println!(
                    "Codex integration {}",
                    if changed {
                        "connected"
                    } else {
                        "already connected"
                    }
                );
            }
            ConnectClient::Opencode => {
                let installer =
                    OpenCodeInstaller::new(OpenCodePaths::discover()?, std::env::current_exe()?);
                let changed = installer.connect()?;
                menvane.set_integration_connected("opencode", true)?;
                println!(
                    "OpenCode integration {}",
                    if changed {
                        "connected"
                    } else {
                        "already connected"
                    }
                );
            }
            ConnectClient::Antigravity => {
                let installer = AntigravityInstaller::new(
                    AntigravityPaths::discover()?,
                    std::env::current_exe()?,
                );
                let changed = installer.connect()?;
                menvane.set_integration_connected("antigravity", true)?;
                println!(
                    "Antigravity integration {}",
                    if changed {
                        "connected"
                    } else {
                        "already connected"
                    }
                );
            }
            ConnectClient::All => {
                let executable = std::env::current_exe()?;
                ClaudeInstaller::new(ClaudePaths::discover()?, &executable).connect()?;
                CodexInstaller::new(CodexPaths::discover()?, &executable).connect()?;
                OpenCodeInstaller::new(OpenCodePaths::discover()?, &executable).connect()?;
                AntigravityInstaller::new(AntigravityPaths::discover()?, &executable).connect()?;
                for client in ["claude-code", "codex", "opencode", "antigravity"] {
                    menvane.set_integration_connected(client, true)?;
                }
                println!("all integrations connected");
            }
        },
        Command::Disconnect(arguments) => match arguments.client {
            Client::Claude => {
                let installer =
                    ClaudeInstaller::new(ClaudePaths::discover()?, std::env::current_exe()?);
                let changed = installer.disconnect()?;
                menvane.set_integration_connected("claude-code", false)?;
                println!(
                    "Claude Code integration {}",
                    if changed {
                        "disconnected"
                    } else {
                        "not connected"
                    }
                );
            }
            Client::Codex => {
                let installer =
                    CodexInstaller::new(CodexPaths::discover()?, std::env::current_exe()?);
                let changed = installer.disconnect()?;
                menvane.set_integration_connected("codex", false)?;
                println!(
                    "Codex integration {}",
                    if changed {
                        "disconnected"
                    } else {
                        "not connected"
                    }
                );
            }
            Client::Opencode => {
                let installer =
                    OpenCodeInstaller::new(OpenCodePaths::discover()?, std::env::current_exe()?);
                let changed = installer.disconnect()?;
                menvane.set_integration_connected("opencode", false)?;
                println!(
                    "OpenCode integration {}",
                    if changed {
                        "disconnected"
                    } else {
                        "not connected"
                    }
                );
            }
            Client::Antigravity => {
                let installer = AntigravityInstaller::new(
                    AntigravityPaths::discover()?,
                    std::env::current_exe()?,
                );
                let changed = installer.disconnect()?;
                menvane.set_integration_connected("antigravity", false)?;
                println!(
                    "Antigravity integration {}",
                    if changed {
                        "disconnected"
                    } else {
                        "not connected"
                    }
                );
            }
        },
        Command::Hook(arguments) => match arguments.client {
            Client::Claude => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                let payload = serde_json::from_str(&input)?;
                let output = ClaudeHook::new(&menvane, std::env::current_exe()?)
                    .handle(&arguments.event, payload)?;
                println!("{}", serde_json::to_string(&output)?);
            }
            Client::Codex => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                let payload = serde_json::from_str(&input)?;
                let output = CodexHook::new(&menvane, std::env::current_exe()?)
                    .handle(&arguments.event, payload)?;
                println!("{}", serde_json::to_string(&output)?);
            }
            Client::Opencode => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                let payload = serde_json::from_str(&input)?;
                let output = OpenCodeHook::new(&menvane, std::env::current_exe()?)
                    .handle(&arguments.event, payload)?;
                println!("{}", serde_json::to_string(&output)?);
            }
            Client::Antigravity => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                let payload = serde_json::from_str(&input)?;
                let output = AntigravityHook::new(&menvane, std::env::current_exe()?)
                    .handle(&arguments.event, payload)?;
                println!("{}", serde_json::to_string(&output)?);
            }
        },
        Command::Provider(arguments) => match arguments.command {
            ProviderCommand::Status => {
                let (provider, model, health) = menvane.provider_health().await?;
                println!("provider\t{provider}");
                println!("model\t{model}");
                println!("health\t{health:?}");
            }
            ProviderCommand::Test => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&menvane.provider_test().await?)?
                );
            }
            ProviderCommand::Configure(configuration) => match configuration.provider {
                ConfigurableProvider::Openai => {
                    menvane.configure_openai(
                        &configuration.model,
                        Some(configuration.reasoning_effort.as_str()),
                    )?;
                    println!(
                        "configured OpenAI model {}; restart the daemon to apply",
                        configuration.model
                    );
                }
                ConfigurableProvider::GithubCopilot => {
                    let client_id = configuration.client_id.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("GitHub Copilot configuration requires --client-id")
                    })?;
                    menvane.configure_github_copilot(
                        &configuration.model,
                        Some(configuration.reasoning_effort.as_str()),
                        client_id,
                    )?;
                    println!(
                        "configured GitHub Copilot model {}; restart the daemon to apply",
                        configuration.model
                    );
                }
            },
            ProviderCommand::Login(authentication) => match authentication.provider {
                ConfigurableProvider::Openai => {
                    menvane.login_openai().await?;
                    println!("OpenAI ChatGPT authorization completed");
                }
                ConfigurableProvider::GithubCopilot => {
                    menvane.login_github_copilot().await?;
                    println!("GitHub Copilot authorization completed");
                }
            },
            ProviderCommand::Logout(authentication) => match authentication.provider {
                ConfigurableProvider::Openai => {
                    menvane.logout_openai()?;
                    println!("OpenAI ChatGPT authorization removed");
                }
                ConfigurableProvider::GithubCopilot => {
                    menvane.logout_github_copilot()?;
                    println!("GitHub Copilot authorization removed");
                }
            },
        },
        Command::Import(arguments) => {
            let scan = match arguments.client {
                Client::Claude => JsonlImporter::claude()
                    .map_err(anyhow::Error::msg)?
                    .scan()
                    .map_err(anyhow::Error::msg)?,
                Client::Codex => JsonlImporter::codex()
                    .map_err(anyhow::Error::msg)?
                    .scan()
                    .map_err(anyhow::Error::msg)?,
                Client::Opencode => OpenCodeImporter::new(arguments.url)
                    .scan()
                    .await
                    .map_err(anyhow::Error::msg)?,
                Client::Antigravity => JsonlImporter::antigravity()
                    .map_err(anyhow::Error::msg)?
                    .scan()
                    .map_err(anyhow::Error::msg)?,
            };
            let mut scan = scan;
            if let Some(days) = arguments.days {
                let window = Duration::try_days(days)
                    .ok_or_else(|| anyhow::anyhow!("the time window is too large"))?;
                scan.retain_since(Utc::now() - window);
            }
            if arguments.dry_run {
                println!("sessions discovered\t{}", scan.sessions.len());
                println!("invalid\t{}", scan.invalid_records);
                println!("estimated bytes\t{}", scan.estimated_bytes);
            } else {
                let mut imported = 0;
                let mut existing = 0;
                let mut orphans = 0;
                for session in scan.sessions {
                    match menvane.import_session(session)? {
                        menvane_engine::ImportOutcome::Imported => imported += 1,
                        menvane_engine::ImportOutcome::AlreadyImported => existing += 1,
                        menvane_engine::ImportOutcome::Orphan => orphans += 1,
                    }
                }
                println!("imported\t{imported}");
                println!("already imported\t{existing}");
                println!("orphans\t{orphans}");
                println!("invalid\t{}", scan.invalid_records);
            }
        }
        Command::Jobs(arguments) => match arguments.command {
            JobsCommand::Retry => {
                let count = menvane.retry_failed_consolidations()?;
                println!("requeued {count} failed consolidation jobs");
            }
        },
        Command::Backup(arguments) => {
            menvane.backup(&arguments.output)?;
            println!("backup created at {}", arguments.output.display());
        }
        Command::Restore(arguments) => {
            if !arguments.confirm {
                bail!("restore requires --confirm because it replaces current state");
            }
            menvane.restore(&arguments.source)?;
            println!("restored backup from {}", arguments.source.display());
        }
        Command::Write(arguments) => {
            let memory = menvane.write(
                &arguments.cwd,
                WriteMemory {
                    title: arguments.title,
                    body: arguments.content,
                    knowledge_type: match arguments.r#type {
                        WritableType::Memory => KnowledgeType::Memory,
                        WritableType::Playbook => KnowledgeType::Playbook,
                    },
                    scope: match arguments.scope {
                        PhysicalScope::Global => Scope::Global,
                        PhysicalScope::Project => Scope::Project,
                    },
                    tags: arguments.tags,
                    applies_to: Applicability {
                        languages: arguments.languages,
                        frameworks: arguments.frameworks,
                        tools: arguments.tools,
                        databases: arguments.databases,
                        platforms: arguments.platforms,
                    },
                },
            )?;
            println!("{}", memory.metadata.id);
        }
        Command::Search(arguments) => {
            let scope = match arguments.scope {
                SearchScopeArg::Auto => ScopeSelection::Auto,
                SearchScopeArg::Project => ScopeSelection::Project,
                SearchScopeArg::Global => ScopeSelection::Global,
            };
            for result in
                menvane.search(&arguments.cwd, &arguments.query, scope, arguments.limit)?
            {
                println!(
                    "{}\t{}\t{}\t{}\t{:.3}\t{}",
                    result.id,
                    result.knowledge_type,
                    result.scope,
                    result.status,
                    result.score,
                    result.title
                );
                if !result.excerpt.is_empty() {
                    println!("  {}", result.excerpt.replace('\n', " "));
                }
            }
        }
        Command::Read(arguments) => {
            let memory = menvane.read(arguments.id)?;
            println!("---\n{}---", serde_yaml::to_string(&memory.metadata)?);
            println!("# {}\n\n{}", memory.title, memory.body);
        }
        Command::Forget(arguments) => {
            let memory = menvane.forget(arguments.id)?;
            if let Some(reason) = arguments.reason {
                println!("forgot {}: {}", memory.metadata.id, reason);
            } else {
                println!("forgot {}", memory.metadata.id);
            }
        }
        Command::Reindex => {
            let (projects, memories) = menvane.reindex()?;
            println!("reindexed {projects} projects and {memories} memories");
        }
        Command::Doctor => {
            let report = menvane.doctor();
            let provider = menvane.provider_health().await;
            for check in &report.checks {
                let status = if check.healthy { "ok" } else { "failed" };
                println!("{status}\t{}\t{}", check.name, check.detail);
            }
            let provider_healthy = match provider {
                Ok((name, model, health)) => {
                    println!(
                        "{}\tLLM provider\t{name}/{model}: {health:?}",
                        if health == menvane_domain::ProviderHealth::Ready {
                            "ok"
                        } else {
                            "failed"
                        }
                    );
                    health == menvane_domain::ProviderHealth::Ready
                }
                Err(error) => {
                    println!("failed\tLLM provider\t{error}");
                    false
                }
            };
            if !report.healthy() || !provider_healthy {
                bail!("one or more doctor checks failed");
            }
        }
        Command::Handoff(arguments) => match arguments.command {
            HandoffCommand::Inspect => {
                let cwd = std::env::current_dir()?;
                let project_id = menvane.ensure_project(&cwd)?.map(|project| project.id);
                let items = menvane.current_handoff_items(project_id.as_deref())?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "project_id": project_id,
                        "text": menvane.render_current_handoff(project_id.as_deref())?,
                        "items": items,
                    }))?
                );
            }
        },
        Command::Mcp => {
            let cwd = std::env::current_dir()?;
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            McpServer::new(&menvane, cwd).serve(stdin.lock(), stdout.lock())?;
        }
        Command::Setup(_) => unreachable!(),
    }
    Ok(())
}
