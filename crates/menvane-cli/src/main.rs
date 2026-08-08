use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use menvane_integrations::McpServer;
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
    Write(WriteArgs),
    Search(SearchArgs),
    Read(ReadArgs),
    Forget(ForgetArgs),
    Reindex,
    Doctor,
    Mcp,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let menvane = Menvane::from_environment()?;
    match cli.command {
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
            for check in &report.checks {
                let status = if check.healthy { "ok" } else { "failed" };
                println!("{status}\t{}\t{}", check.name, check.detail);
            }
            if !report.healthy() {
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
