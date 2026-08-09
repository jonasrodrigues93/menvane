use std::io::Read;
use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono::{Duration, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use menvane_integrations::{
    ClaudeHook, ClaudeInstaller, ClaudePaths, CodexHook, CodexInstaller, CodexPaths, JsonlImporter,
    McpServer, OpenCodeHook, OpenCodeImporter, OpenCodeInstaller, OpenCodePaths,
};
use menvane_server::{
    DEFAULT_ADDRESS, DEFAULT_PORT, daemon_running, home_from_environment, serve, start_daemon,
    stop_daemon,
};
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

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Daemon(DaemonArgs),
    Connect(ConnectArgs),
    Disconnect(ClientArgs),
    Hook(HookArgs),
    Provider(ProviderArgs),
    Import(ImportArgs),
    Backup(BackupArgs),
    Restore(RestoreArgs),
    Write(WriteArgs),
    Search(SearchArgs),
    Read(ReadArgs),
    Forget(ForgetArgs),
    Reindex,
    Doctor,
    Gc,
    Handoff(HandoffArgs),
    Mcp,
}

#[derive(Args)]
struct HandoffArgs {
    #[command(subcommand)]
    command: HandoffCommand,
}

#[derive(Subcommand)]
enum HandoffCommand {
    Inspect(HandoffInspectArgs),
}

#[derive(Args)]
struct HandoffInspectArgs {
    id: Uuid,
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

    #[test]
    fn parses_only_positive_day_windows() {
        assert_eq!(parse_days("7d"), Ok(7));
        assert!(parse_days("7").is_err());
        assert!(parse_days("7h").is_err());
        assert!(parse_days("0d").is_err());
    }

    #[test]
    fn parses_handoff_inspection_diagnostics() {
        assert!(
            Cli::try_parse_from([
                "menvane",
                "handoff",
                "inspect",
                "018f2c20-7a1e-7c3b-9f4a-1a2b3c4d5e6f"
            ])
            .is_ok()
        );
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
}

#[derive(Clone, Copy, ValueEnum)]
enum ConfigurableProvider {
    Openai,
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
    #[arg(long, default_value_t = 1.0)]
    confidence: f64,
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
    Fact,
    Decision,
    Procedure,
    Gotcha,
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
            ConnectClient::All => {
                let executable = std::env::current_exe()?;
                ClaudeInstaller::new(ClaudePaths::discover()?, &executable).connect()?;
                CodexInstaller::new(CodexPaths::discover()?, &executable).connect()?;
                OpenCodeInstaller::new(OpenCodePaths::discover()?, &executable).connect()?;
                for client in ["claude-code", "codex", "opencode"] {
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
            },
            ProviderCommand::Login(authentication) => match authentication.provider {
                ConfigurableProvider::Openai => {
                    menvane.login_openai().await?;
                    println!("OpenAI ChatGPT authorization completed");
                }
            },
            ProviderCommand::Logout(authentication) => match authentication.provider {
                ConfigurableProvider::Openai => {
                    menvane.logout_openai()?;
                    println!("OpenAI ChatGPT authorization removed");
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
                    memory_type: match arguments.r#type {
                        WritableType::Fact => MemoryType::Fact,
                        WritableType::Decision => MemoryType::Decision,
                        WritableType::Procedure => MemoryType::Procedure,
                        WritableType::Gotcha => MemoryType::Gotcha,
                    },
                    scope: match arguments.scope {
                        PhysicalScope::Global => Scope::Global,
                        PhysicalScope::Project => Scope::Project,
                    },
                    confidence: arguments.confidence,
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
                    result.memory_type,
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
        Command::Gc => {
            println!("archived {} sessions", menvane.gc()?);
        }
        Command::Handoff(arguments) => match arguments.command {
            HandoffCommand::Inspect(arguments) => {
                let detail = menvane
                    .handoff_detail(arguments.id)?
                    .ok_or_else(|| anyhow::anyhow!("handoff {} not found", arguments.id))?;
                println!("{}", serde_json::to_string_pretty(&detail)?);
            }
        },
        Command::Mcp => {
            let cwd = std::env::current_dir()?;
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            McpServer::new(&menvane, cwd).serve(stdin.lock(), stdout.lock())?;
        }
    }
    Ok(())
}
