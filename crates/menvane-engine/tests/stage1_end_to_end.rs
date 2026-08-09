use std::fs;
use std::path::Path;
use std::process::Command;

use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, ProjectResolver, ScopeSelection, WriteMemory};
use tempfile::TempDir;

mod common;

#[test]
fn project_isolation_global_visibility_and_database_rebuild() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let project_a = temporary.path().join("project-a");
    let project_b = temporary.path().join("project-b");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    common::init_git(&project_a);
    common::init_git(&project_b);
    let menvane = Menvane::new(&home).unwrap();

    menvane
        .write(
            &project_a,
            write_request(
                "Project alpha marker",
                "project-alpha-token",
                Scope::Project,
            ),
        )
        .unwrap();
    menvane
        .write(
            &project_b,
            write_request("Project beta marker", "project-beta-token", Scope::Project),
        )
        .unwrap();
    menvane
        .write(
            &project_a,
            write_request("Universal marker", "universal-token", Scope::Global),
        )
        .unwrap();

    assert_eq!(
        menvane
            .search(&project_a, "project-alpha-token", ScopeSelection::Auto, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        menvane
            .search(&project_b, "project-alpha-token", ScopeSelection::Auto, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        menvane
            .search(&project_b, "universal-token", ScopeSelection::Auto, 10)
            .unwrap()
            .len(),
        1
    );

    drop(menvane);
    fs::remove_file(home.join("index.sqlite")).unwrap();
    let rebuilt = Menvane::new(&home).unwrap();
    rebuilt.reindex().unwrap();
    assert_eq!(
        rebuilt
            .search(&project_a, "project-alpha-token", ScopeSelection::Auto, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rebuilt
            .search(&project_b, "project-beta-token", ScopeSelection::Auto, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rebuilt
            .search(&project_b, "universal-token", ScopeSelection::Auto, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn git_worktrees_share_project_identity() {
    let temporary = TempDir::new().unwrap();
    let repository = temporary.path().join("repository");
    let worktree = temporary.path().join("worktree");
    fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    fs::write(repository.join("file.txt"), "initial").unwrap();
    git(&repository, &["add", "file.txt"]);
    git(
        &repository,
        &[
            "-c",
            "user.name=Menvane Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
    );
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "test-worktree",
            worktree.to_str().unwrap(),
        ],
    );

    let main = ProjectResolver::resolve(&repository).unwrap().unwrap();
    let linked = ProjectResolver::resolve(&worktree).unwrap().unwrap();
    assert_eq!(main.id, linked.id);
    assert_eq!(main.identity, linked.identity);
}

#[test]
fn directory_without_git_uses_only_global_memory() {
    let temporary = TempDir::new().unwrap();
    let directory = temporary.path().join("notes");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(".menvane.toml"), "project = 'notes'").unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();

    let memory = menvane
        .write(
            &directory,
            write_request("Unrelated note", "global-note-token", Scope::Project),
        )
        .unwrap();

    assert_eq!(memory.metadata.scope, Scope::Global);
    assert!(memory.metadata.project_id.is_none());
    assert!(menvane.all_projects().unwrap().is_empty());
    assert_eq!(
        menvane
            .search(&directory, "global-note-token", ScopeSelection::Auto, 10)
            .unwrap()
            .len(),
        1
    );
}

fn write_request(title: &str, body: &str, scope: Scope) -> WriteMemory {
    WriteMemory {
        title: title.to_owned(),
        body: body.to_owned(),
        memory_type: MemoryType::Fact,
        scope,
        confidence: 1.0,
        tags: Vec::new(),
        applies_to: Applicability::default(),
    }
}

fn git(cwd: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success());
}
