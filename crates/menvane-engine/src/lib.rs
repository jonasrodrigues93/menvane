mod project_resolver;
mod retriever;
mod technology_detector;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use menvane_domain::{Applicability, Memory, MemoryMetadata, MemoryType, Project, Scope};
use menvane_store::{IndexStore, MarkdownStore, SearchResult, mark_forgotten};
use uuid::Uuid;

pub use project_resolver::{ProjectResolution, ProjectResolver, normalize_git_remote};
pub use retriever::{RetrievalMode, RetrievalScope, Retriever};
pub use technology_detector::TechnologyDetector;

pub struct WriteMemory {
    pub title: String,
    pub body: String,
    pub memory_type: MemoryType,
    pub scope: Scope,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub applies_to: Applicability,
}

pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

pub struct DoctorCheck {
    pub name: &'static str,
    pub healthy: bool,
    pub detail: String,
}

impl DoctorReport {
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|check| check.healthy)
    }
}

pub struct Menvane {
    home: PathBuf,
    markdown: MarkdownStore,
    index: IndexStore,
}

impl Menvane {
    pub fn from_environment() -> Result<Self> {
        let home = match env::var_os("MENVANE_HOME") {
            Some(path) => PathBuf::from(path),
            None => env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set; set MENVANE_HOME explicitly")?
                .join(".menvane"),
        };
        Self::new(home)
    }

    pub fn new(home: impl Into<PathBuf>) -> Result<Self> {
        let home = home.into();
        let markdown = MarkdownStore::new(&home);
        markdown.initialize()?;
        let index = IndexStore::new(home.join("index.sqlite"));
        index.initialize()?;
        Ok(Self {
            home,
            markdown,
            index,
        })
    }

    pub fn write(&self, cwd: &Path, request: WriteMemory) -> Result<Memory> {
        if request.title.trim().is_empty() {
            bail!("memory title cannot be empty");
        }
        if !(0.0..=1.0).contains(&request.confidence) {
            bail!("confidence must be between 0 and 1");
        }
        if request.memory_type == MemoryType::Session {
            bail!("sessions are created by session capture, not manual writes");
        }
        let project = match request.scope {
            Scope::Project => Some(self.ensure_project(cwd)?),
            Scope::Global => None,
        };
        let project_id = project.as_ref().map(|project| project.id.clone());
        let memory = Memory {
            metadata: MemoryMetadata::new(
                request.memory_type,
                request.scope,
                project_id,
                request.confidence,
                request.tags,
                request.applies_to,
            ),
            title: request.title.trim().to_owned(),
            body: format_memory_body(request.memory_type, &request.body),
        };
        let path = self.markdown.write_memory(&memory, project.as_ref())?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown
            .commit(&format!("feat(memory): write {}", memory.metadata.id));
        Ok(memory)
    }

    pub fn search(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_with_sessions(cwd, query, scope, limit, false)
    }

    pub fn search_with_sessions(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
        include_sessions: bool,
    ) -> Result<Vec<SearchResult>> {
        let project = match scope {
            ScopeSelection::Global => None,
            ScopeSelection::Auto | ScopeSelection::Project => Some(self.ensure_project(cwd)?),
        };
        let retrieval_scope = match scope {
            ScopeSelection::Auto => RetrievalScope::Auto,
            ScopeSelection::Project => RetrievalScope::Project,
            ScopeSelection::Global => RetrievalScope::Global,
        };
        Retriever::new(&self.index).retrieve(
            query,
            project.as_ref(),
            retrieval_scope,
            RetrievalMode::Explicit,
            include_sessions,
            limit,
        )
    }

    pub fn recall(&self, cwd: &Path, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let project = self.ensure_project(cwd)?;
        Retriever::new(&self.index).retrieve(
            query,
            Some(&project),
            RetrievalScope::Auto,
            RetrievalMode::Automatic,
            false,
            limit,
        )
    }

    pub fn read(&self, id: Uuid) -> Result<Memory> {
        Ok(self.index.read_memory(&self.markdown, id)?.0)
    }

    pub fn forget(&self, id: Uuid) -> Result<Memory> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, id)?;
        mark_forgotten(&mut memory);
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown.commit(&format!("chore(memory): forget {id}"));
        Ok(memory)
    }

    pub fn reindex(&self) -> Result<(usize, usize)> {
        self.index.reindex(&self.markdown)
    }

    pub fn doctor(&self) -> DoctorReport {
        let mut checks = Vec::new();
        let writable_probe = self.home.join(format!(".doctor-{}", Uuid::now_v7()));
        let writable = fs::write(&writable_probe, b"ok")
            .and_then(|_| fs::remove_file(&writable_probe))
            .map(|_| "writable".to_owned())
            .map_err(|error| error.to_string());
        checks.push(check("home writable", writable));
        checks.push(check(
            "SQLite",
            self.index
                .memory_count()
                .map(|count| format!("{count} memories")),
        ));
        checks.push(check(
            "FTS5",
            self.index.fts5_available().and_then(|available| {
                available
                    .then_some("available".to_owned())
                    .context("unavailable")
            }),
        ));
        let markdown_counts = self.markdown.project_files().and_then(|projects| {
            self.markdown
                .memory_files()
                .map(|memories| (projects.len() as u64, memories.len() as u64))
        });
        let index_counts = self.index.project_count().and_then(|projects| {
            self.index
                .memory_count()
                .map(|memories| (projects, memories))
        });
        let consistency = markdown_counts.and_then(|markdown| {
            index_counts.and_then(|index| {
                if markdown == index {
                    Ok(format!("{} projects, {} memories", markdown.0, markdown.1))
                } else {
                    bail!(
                        "Markdown has {} projects/{} memories; index has {} projects/{} memories",
                        markdown.0,
                        markdown.1,
                        index.0,
                        index.1
                    )
                }
            })
        });
        checks.push(check("Markdown/index consistency", consistency));
        let git = Command::new("git")
            .arg("--version")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .map_err(|error| error.to_string());
        checks.push(check("Git", git));
        DoctorReport { checks }
    }

    pub fn ensure_project(&self, cwd: &Path) -> Result<Project> {
        let resolution = ProjectResolver::resolve(cwd)?;
        let technologies = TechnologyDetector::detect(&resolution.root)?;
        let probe = Project {
            id: resolution.id.clone(),
            identity: resolution.identity.clone(),
            name: resolution.name.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            known_paths: vec![resolution.root.to_string_lossy().into_owned()],
            technologies: technologies.clone(),
        };
        let path = self.markdown.project_directory(&probe).join("project.md");
        let mut project = if path.exists() {
            self.markdown.parse_project(&path)?
        } else {
            probe
        };
        let known_path = resolution.root.to_string_lossy().into_owned();
        let mut changed = false;
        if !project.known_paths.contains(&known_path) {
            project.known_paths.push(known_path);
            project.known_paths.sort();
            changed = true;
        }
        if project.technologies != technologies {
            project.technologies = technologies;
            changed = true;
        }
        if !path.exists() || changed {
            project.updated_at = Utc::now();
            let path = self.markdown.write_project(&project)?;
            self.index.upsert_project(&project, &path)?;
            self.markdown
                .commit(&format!("chore(project): update {}", project.id));
        } else {
            self.index.upsert_project(&project, &path)?;
        }
        Ok(project)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ScopeSelection {
    Auto,
    Project,
    Global,
}

fn check(name: &'static str, result: Result<String, impl ToString>) -> DoctorCheck {
    match result {
        Ok(detail) => DoctorCheck {
            name,
            healthy: true,
            detail,
        },
        Err(error) => DoctorCheck {
            name,
            healthy: false,
            detail: error.to_string(),
        },
    }
}

fn format_memory_body(memory_type: MemoryType, body: &str) -> String {
    let body = body.trim();
    match memory_type {
        MemoryType::Fact => body.to_owned(),
        MemoryType::Decision if body.contains("## Decision") => body.to_owned(),
        MemoryType::Decision => {
            format!("## Decision\n\n{body}\n\n## Reason\n\n## Alternatives\n\n## Consequences")
        }
        MemoryType::Gotcha if body.contains("## Problem") => body.to_owned(),
        MemoryType::Gotcha => {
            format!("## Problem\n\n{body}\n\n## Cause\n\n## Resolution\n\n## Avoidance")
        }
        MemoryType::Procedure if body.contains("## Procedure") => body.to_owned(),
        MemoryType::Procedure => format!(
            "## Trigger\n\n## Preconditions\n\n## Procedure\n\n{body}\n\n## Decision points\n\n## Validation\n\n## Failure handling\n\n## Expected outcome"
        ),
        MemoryType::Session => body.to_owned(),
    }
}
