use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectResolution {
    pub id: String,
    pub identity: String,
    pub name: String,
    pub root: PathBuf,
}

pub struct ProjectResolver;

#[derive(Deserialize)]
struct ProjectOverride {
    project: String,
}

impl ProjectResolver {
    pub fn resolve(cwd: &Path) -> Result<Option<ProjectResolution>> {
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("cannot resolve working directory {}", cwd.display()))?;
        let Some(root) = git_output(&cwd, &["rev-parse", "--show-toplevel"]) else {
            return Ok(None);
        };
        if let Some((root, project)) = find_override(&cwd)? {
            return Ok(Some(resolution(
                format!("override:{project}"),
                project,
                root,
            )));
        }
        let root = PathBuf::from(root).canonicalize()?;
        let identity = canonical_git_identity(&root)?;
        let name = identity
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("project")
            .to_owned();
        Ok(Some(resolution(identity, name, root)))
    }
}

pub fn normalize_git_remote(remote: &str) -> Result<String> {
    let remote = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    if remote.is_empty() {
        bail!("Git remote is empty");
    }
    let without_protocol = remote
        .split_once("://")
        .map(|(_, remainder)| remainder)
        .unwrap_or(remote);
    let (host_part, path) = if !remote.contains("://") {
        if let Some((host, path)) = without_protocol.split_once(':') {
            if !host.contains('/') {
                (host, path)
            } else {
                split_host_path(without_protocol)?
            }
        } else {
            split_host_path(without_protocol)?
        }
    } else {
        split_host_path(without_protocol)?
    };
    let host = host_part
        .rsplit('@')
        .next()
        .context("Git remote host is missing")?
        .to_ascii_lowercase();
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        bail!("Git remote must contain a host and repository path");
    }
    Ok(format!("{host}/{path}"))
}

fn split_host_path(remote: &str) -> Result<(&str, &str)> {
    remote
        .split_once('/')
        .context("Git remote must contain a host and repository path")
}

fn find_override(cwd: &Path) -> Result<Option<(PathBuf, String)>> {
    for directory in cwd.ancestors() {
        let path = directory.join(".menvane.toml");
        if path.exists() {
            let configuration: ProjectOverride = toml::from_str(&fs::read_to_string(&path)?)?;
            let project = configuration.project.trim();
            if project.is_empty() {
                bail!("project override in {} cannot be empty", path.display());
            }
            return Ok(Some((directory.to_path_buf(), project.to_owned())));
        }
    }
    Ok(None)
}

fn canonical_git_identity(root: &Path) -> Result<String> {
    let remote = git_output(root, &["remote", "get-url", "origin"]).or_else(|| {
        git_output(root, &["remote"]).and_then(|remotes| {
            remotes
                .lines()
                .next()
                .and_then(|name| git_output(root, &["remote", "get-url", name]))
        })
    });
    if let Some(remote) = remote
        && let Ok(identity) = normalize_git_remote(&remote)
    {
        return Ok(identity);
    }
    let common = git_output(root, &["rev-parse", "--git-common-dir"])
        .context("Git repository has no common directory")?;
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    }
    .canonicalize()?;
    Ok(format!("git-common-dir:{}", common.display()))
}

fn git_output(cwd: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|output| !output.is_empty())
}

fn resolution(identity: String, name: String, root: PathBuf) -> ProjectResolution {
    let id = hex::encode(Sha256::digest(identity.as_bytes()));
    ProjectResolution {
        id,
        identity,
        name,
        root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_github_remotes_normalize_identically() {
        let expected = "github.com/owner/repo";
        for remote in [
            "git@github.com:owner/repo.git",
            "https://github.com/owner/repo.git",
            "ssh://git@GitHub.com/owner/repo.git/",
        ] {
            assert_eq!(normalize_git_remote(remote).unwrap(), expected);
        }
    }

    #[test]
    fn directory_without_git_has_no_project() {
        let directory = tempfile::TempDir::new().unwrap();
        assert!(
            ProjectResolver::resolve(directory.path())
                .unwrap()
                .is_none()
        );
    }
}
