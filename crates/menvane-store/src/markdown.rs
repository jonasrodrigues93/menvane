use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use menvane_domain::{Memory, MemoryMetadata, Project, Scope};
use serde::de::DeserializeOwned;
use uuid::Uuid;

static GIT_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub struct ParsedMarkdown<T> {
    pub metadata: T,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct MarkdownStore {
    home: PathBuf,
    memory_root: PathBuf,
}

impl MarkdownStore {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let memory_root = home.join("memory");
        Self { home, memory_root }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn memory_root(&self) -> &Path {
        &self.memory_root
    }

    pub fn initialize(&self) -> Result<()> {
        for path in [
            self.home.join("logs"),
            self.home.join("spool"),
            self.memory_root.join("archive/sessions"),
        ] {
            fs::create_dir_all(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
        }
        for directory in ["facts", "decisions", "procedures", "gotchas"] {
            fs::create_dir_all(self.memory_root.join("global").join(directory))?;
        }
        let config = self.home.join("config.toml");
        if !config.exists() {
            self.atomic_write(&config, default_config().as_bytes())?;
        }
        self.initialize_git();
        Ok(())
    }

    pub fn project_directory(&self, project: &Project) -> PathBuf {
        let slug = slugify(&project.name);
        self.memory_root
            .join("projects")
            .join(format!("{}--{}", slug, &project.id[..12]))
    }

    pub fn write_project(&self, project: &Project) -> Result<PathBuf> {
        let directory = self.project_directory(project);
        for name in ["facts", "decisions", "procedures", "gotchas", "sessions"] {
            fs::create_dir_all(directory.join(name))?;
        }
        let path = directory.join("project.md");
        let markdown = serialize_frontmatter(project, "")?;
        self.atomic_write(&path, markdown.as_bytes())?;
        Ok(path)
    }

    pub fn write_memory(&self, memory: &Memory, project: Option<&Project>) -> Result<PathBuf> {
        let base = match memory.metadata.scope {
            Scope::Global => self.memory_root.join("global"),
            Scope::Project => {
                let project = project.context("project memory requires project metadata")?;
                self.project_directory(project)
            }
        };
        let directory = base.join(memory.metadata.memory_type.directory_name());
        fs::create_dir_all(&directory)?;
        let filename = format!("{}--{}.md", slugify(&memory.title), memory.metadata.id);
        let path = directory.join(filename);
        let body = format!("# {}\n\n{}\n", memory.title.trim(), memory.body.trim());
        let markdown = serialize_frontmatter(&memory.metadata, &body)?;
        self.atomic_write(&path, markdown.as_bytes())?;
        Ok(path)
    }

    pub fn update_memory(&self, path: &Path, memory: &Memory) -> Result<()> {
        let body = format!("# {}\n\n{}\n", memory.title.trim(), memory.body.trim());
        let markdown = serialize_frontmatter(&memory.metadata, &body)?;
        self.atomic_write(path, markdown.as_bytes())
    }

    pub fn parse_memory(&self, path: &Path) -> Result<Memory> {
        let parsed: ParsedMarkdown<MemoryMetadata> = parse_frontmatter(path)?;
        let (title, body) = parse_title(&parsed.body)?;
        Ok(Memory {
            metadata: parsed.metadata,
            title,
            body,
        })
    }

    pub fn parse_project(&self, path: &Path) -> Result<Project> {
        Ok(parse_frontmatter(path)?.metadata)
    }

    pub fn project_files(&self) -> Result<Vec<PathBuf>> {
        collect_named_files(&self.memory_root.join("projects"), "project.md")
    }

    pub fn memory_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = collect_markdown_files(&self.memory_root.join("global"))?;
        files.extend(collect_markdown_files(&self.memory_root.join("projects"))?);
        files.extend(collect_markdown_files(
            &self.memory_root.join("archive/sessions"),
        )?);
        files.retain(|path| path.file_name().is_some_and(|name| name != "project.md"));
        files.sort();
        Ok(files)
    }

    pub fn archive_session(&self, path: &Path) -> Result<PathBuf> {
        let filename = path.file_name().context("session path has no filename")?;
        let destination = self.memory_root.join("archive/sessions").join(filename);
        fs::create_dir_all(destination.parent().unwrap())?;
        fs::rename(path, &destination)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        File::open(destination.parent().unwrap())?.sync_all()?;
        Ok(destination)
    }

    pub fn commit(&self, message: &str) {
        if !self.memory_root.join(".git").exists() {
            return;
        }
        let Ok(_guard) = GIT_WRITE_LOCK.lock() else {
            return;
        };
        let _ = Command::new("git")
            .args(["add", "."])
            .current_dir(&self.memory_root)
            .status();
        let _ = Command::new("git")
            .args([
                "-c",
                "user.name=Menvane",
                "-c",
                "user.email=menvane@localhost",
                "commit",
                "--quiet",
                "-m",
                message,
            ])
            .current_dir(&self.memory_root)
            .status();
    }

    fn initialize_git(&self) {
        if self.memory_root.join(".git").exists() {
            return;
        }
        let _ = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&self.memory_root)
            .status();
    }

    fn atomic_write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .with_context(|| format!("path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".{}.tmp", Uuid::now_v7()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

pub fn parse_frontmatter<T: DeserializeOwned>(path: &Path) -> Result<ParsedMarkdown<T>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let rest = content
        .strip_prefix("---\n")
        .with_context(|| format!("missing YAML frontmatter in {}", path.display()))?;
    let (yaml, body) = rest
        .split_once("\n---\n")
        .with_context(|| format!("unterminated YAML frontmatter in {}", path.display()))?;
    let metadata = serde_yaml::from_str(yaml)
        .with_context(|| format!("invalid YAML frontmatter in {}", path.display()))?;
    Ok(ParsedMarkdown {
        metadata,
        body: body.trim().to_owned(),
    })
}

fn serialize_frontmatter<T: serde::Serialize>(metadata: &T, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(metadata)?;
    Ok(format!("---\n{}---\n{}", yaml, body))
}

fn parse_title(markdown: &str) -> Result<(String, String)> {
    let mut lines = markdown.lines();
    let heading = lines.next().context("memory body is empty")?;
    let title = heading
        .strip_prefix("# ")
        .context("memory body must start with a level-one heading")?
        .trim()
        .to_owned();
    if title.is_empty() {
        bail!("memory title cannot be empty");
    }
    Ok((
        title,
        lines.collect::<Vec<_>>().join("\n").trim().to_owned(),
    ))
}

fn collect_markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    collect_files(root, &|path| {
        path.extension().is_some_and(|extension| extension == "md")
    })
}

fn collect_named_files(root: &Path, name: &str) -> Result<Vec<PathBuf>> {
    collect_files(root, &|path| {
        path.file_name().is_some_and(|filename| filename == name)
    })
}

fn collect_files(root: &Path, predicate: &dyn Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                if path.file_name().is_none_or(|name| name != ".git") {
                    pending.push(path);
                }
            } else if predicate(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "memory".to_owned()
    } else {
        slug
    }
}

fn default_config() -> &'static str {
    "[capture]\nmax_prompt_bytes = 16384\nmax_tool_output_bytes = 4096\nmax_tool_input_bytes = 4096\nignore_paths = [\".env\", \".env.*\", \"**/secrets/**\", \"**/.ssh/**\"]\n\n[sessions]\nidle_finalize_seconds = 120\n\n[llm]\nprovider = \"openai\"\nmodel = \"gpt-5.6-luna\"\nreasoning_effort = \"medium\"\noauth_issuer = \"https://auth.openai.com\"\noauth_endpoint = \"https://chatgpt.com/backend-api/codex/responses\"\n"
}

#[cfg(test)]
mod tests {
    use menvane_domain::{Applicability, MemoryMetadata, MemoryType, Scope};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn memory_round_trip_preserves_metadata_and_body() {
        let temporary = TempDir::new().unwrap();
        let store = MarkdownStore::new(temporary.path());
        store.initialize().unwrap();
        let memory = Memory {
            metadata: MemoryMetadata::new(
                MemoryType::Fact,
                Scope::Global,
                None,
                0.9,
                vec!["rust".to_owned()],
                Applicability::default(),
            ),
            title: "Prefer explicit errors".to_owned(),
            body: "Return actionable failures.".to_owned(),
        };
        let path = store.write_memory(&memory, None).unwrap();
        assert_eq!(store.parse_memory(&path).unwrap(), memory);
    }
}
