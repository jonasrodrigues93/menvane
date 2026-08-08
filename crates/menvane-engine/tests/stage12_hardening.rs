use std::fs;
use std::time::{Duration, Instant};

use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, WriteMemory};
use tempfile::TempDir;

#[test]
fn backup_restore_validates_and_replaces_state() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let home = temporary.path().join("home");
    let backup = temporary.path().join("backup");
    let menvane = Menvane::new(&home).unwrap();
    let retained = write(
        &menvane,
        &project,
        "Retained backup memory",
        "retained-token",
    );
    menvane.backup(&backup).unwrap();
    write(&menvane, &project, "Later memory", "later-token");
    menvane.restore(&backup).unwrap();
    assert_eq!(
        menvane.read(retained).unwrap().title,
        "Retained backup memory"
    );
    assert!(
        menvane
            .all_memories()
            .unwrap()
            .iter()
            .all(|memory| memory.title != "Later memory")
    );

    let invalid = temporary.path().join("invalid-backup");
    copy_tree(&backup, &invalid);
    fs::write(invalid.join("config.toml"), "corrupt").unwrap();
    assert!(menvane.restore(&invalid).is_err());
    assert_eq!(
        menvane.read(retained).unwrap().title,
        "Retained backup memory"
    );
}

#[test]
fn local_prompt_retrieval_stays_within_hot_path_budget() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    for index in 0..100 {
        write(
            &menvane,
            &project,
            &format!("Performance memory {index}"),
            &format!("retrieval-performance-token-{index}"),
        );
    }
    let started = Instant::now();
    let context = menvane
        .prompt_context(
            &project,
            "retrieval performance token 42",
            "performance-session",
        )
        .unwrap();
    assert!(!context.is_empty());
    assert!(started.elapsed() < Duration::from_millis(300));
}

fn write(menvane: &Menvane, project: &std::path::Path, title: &str, body: &str) -> uuid::Uuid {
    menvane
        .write(
            project,
            WriteMemory {
                title: title.to_owned(),
                body: body.to_owned(),
                memory_type: MemoryType::Fact,
                scope: Scope::Project,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap()
        .metadata
        .id
}

fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let path = entry.unwrap().path();
        let target = destination.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(path, target).unwrap();
        }
    }
}
