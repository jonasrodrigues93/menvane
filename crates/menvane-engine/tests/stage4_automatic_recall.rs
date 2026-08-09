use std::fs;

use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, WriteMemory};
use tempfile::TempDir;

mod common;

#[test]
fn session_and_prompt_context_are_bounded_trusted_and_not_repeated() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='recall'\nversion='0.1.0'\n",
    )
    .unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    write(
        &menvane,
        &project,
        "Use SQLite for local state",
        "The project stores local state in SQLite.",
        MemoryType::Decision,
        Scope::Project,
    );
    write(
        &menvane,
        &project,
        "Never expose tokens",
        "Authentication tokens must remain private.",
        MemoryType::Fact,
        Scope::Global,
    );
    write(
        &menvane,
        &project,
        "Migration lock failure",
        "SQLite migration can fail while another writer owns the lock.",
        MemoryType::Procedure,
        Scope::Project,
    );

    let briefing = menvane
        .session_briefing(&project, "claude-session")
        .unwrap();
    assert!(briefing.starts_with("MENVANE MEMORY CONTEXT"));
    assert!(briefing.contains("Historical context only."));
    assert!(briefing.contains("Use SQLite for local state"));
    assert!(briefing.contains("Never expose tokens"));
    assert!(briefing.ends_with("END MENVANE MEMORY CONTEXT"));
    assert!(briefing.chars().count() <= 2_500);

    let prompt = menvane
        .prompt_context(
            &project,
            "How should I handle the SQLite migration lock failure?",
            "claude-session",
        )
        .unwrap();
    assert!(prompt.contains("Migration lock failure"));
    assert!(prompt.chars().count() <= 6_000);
    let repeated = menvane
        .prompt_context(&project, "SQLite migration lock failure", "claude-session")
        .unwrap();
    assert!(!repeated.contains("Migration lock failure"));
    assert_eq!(
        menvane
            .prompt_context(&project, "  ", "claude-session")
            .unwrap(),
        ""
    );
}

fn write(
    menvane: &Menvane,
    project: &std::path::Path,
    title: &str,
    body: &str,
    memory_type: MemoryType,
    scope: Scope,
) {
    menvane
        .write(
            project,
            WriteMemory {
                title: title.to_owned(),
                body: body.to_owned(),
                memory_type,
                scope,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
}
