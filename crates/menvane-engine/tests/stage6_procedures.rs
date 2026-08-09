use std::fs;

use menvane_domain::{Applicability, MemoryStatus, MemoryType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use tempfile::TempDir;
use uuid::Uuid;

mod common;

#[test]
fn procedure_reinforcement_activation_failure_and_promotion() {
    let temporary = TempDir::new().unwrap();
    let project_a = temporary.path().join("project-a");
    let project_b = temporary.path().join("project-b");
    let project_c = temporary.path().join("project-c");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    fs::create_dir_all(&project_c).unwrap();
    common::init_git(&project_a);
    common::init_git(&project_b);
    common::init_git(&project_c);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let first = write_procedure(&menvane, &project_a);
    assert_eq!(first.metadata.status, MemoryStatus::Candidate);
    assert_eq!(first.metadata.successes, Some(1));
    let reuse_session = Uuid::now_v7();
    let active = menvane
        .record_procedure_application(first.metadata.id, reuse_session, true)
        .unwrap();
    assert_eq!(active.metadata.status, MemoryStatus::Active);
    assert_eq!(active.metadata.successes, Some(2));
    let duplicate = menvane
        .record_procedure_application(first.metadata.id, reuse_session, true)
        .unwrap();
    assert_eq!(duplicate.metadata.successes, Some(2));
    let failed = menvane
        .record_procedure_application(first.metadata.id, Uuid::now_v7(), false)
        .unwrap();
    assert_eq!(failed.metadata.failures, Some(1));
    assert_eq!(failed.metadata.status, MemoryStatus::Active);

    write_procedure(&menvane, &project_b);
    let promoted = menvane.promote_global_memories().unwrap();
    assert_eq!(promoted.len(), 1);
    let global = menvane.read(promoted[0]).unwrap();
    assert_eq!(global.metadata.scope, Scope::Global);
    assert_eq!(global.metadata.source_project_ids.len(), 2);
    assert!(global.metadata.successes.unwrap() >= 3);
    let visible = menvane
        .search(
            &project_c,
            "verify sqlite migration",
            ScopeSelection::Auto,
            10,
        )
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].scope, "global");
}

fn write_procedure(menvane: &Menvane, project: &std::path::Path) -> menvane_domain::Memory {
    menvane
        .write(
            project,
            WriteMemory {
                title: "Verify SQLite migration".to_owned(),
                body: "1. Run migration\n2. Verify SQLite schema".to_owned(),
                memory_type: MemoryType::Procedure,
                scope: Scope::Project,
                confidence: 0.9,
                tags: Vec::new(),
                applies_to: Applicability {
                    databases: vec!["sqlite".to_owned()],
                    ..Applicability::default()
                },
            },
        )
        .unwrap()
}
