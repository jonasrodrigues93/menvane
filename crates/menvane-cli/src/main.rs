use std::io::Read;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use menvane_integrations::{
    ClaudeHook, ClaudeInstaller, ClaudePaths, CodexHook, CodexInstaller, CodexPaths, McpServer,
    OpenCodeHook, OpenCodeInstaller, OpenCodePaths,
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
    about = "Local persistent memory for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Daemon(DaemonArgs),
    Connect(ClientArgs),
    Disconnect(ClientArgs),
    Hook(HookArgs),
    Provider(ProviderArgs),
    Write(WriteArgs),
    Search(SearchArgs),
    Read(ReadArgs),
    Forget(ForgetArgs),
    Reindex,
    Doctor,
    Mcp,
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
}

#[derive(Args)]
struct ClientArgs {
    #[arg(value_enum)]
    client: Client,
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
            Client::Claude => {
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
            Client::Codex => {
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
            Client::Opencode => {
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
        },
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
        Command::Mcp => {
            let cwd = std::env::current_dir()?;
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            McpServer::new(&menvane, cwd).serve(stdin.lock(), stdout.lock())?;
        }
    }
    Ok(())
}
