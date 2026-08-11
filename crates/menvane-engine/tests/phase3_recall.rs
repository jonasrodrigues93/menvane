use std::fs;

use chrono::{Duration, Utc};
use menvane_domain::{Applicability, MemoryType, NormalizedEvent, NormalizedEventKind, Scope};
use menvane_engine::{Menvane, WriteMemory};
use tempfile::TempDir;

mod common;

#[test]
fn current_prompt_is_diagnosed_without_existing_session_state() {
    let (temporary, project, menvane) = setup();
    write(
        &menvane,
        &project,
        "Standalone recall guidance",
        "standalone recall guidance",
    );

    let recall = menvane
        .prompt_recall(
            &project,
            "test-client",
            "missing-session",
            "standalone recall guidance",
            10,
        )
        .unwrap();

    assert_eq!(recall.diagnostics.queries.len(), 1);
    assert_eq!(recall.diagnostics.queries[0].source, "current-prompt");
    assert_eq!(recall.results.len(), 1);
    drop(temporary);
}

#[test]
fn current_prompt_dominates_conflicting_root_intent() {
    let (temporary, project, menvane) = setup();
    let root_memory = write(
        &menvane,
        &project,
        "Root migration guidance",
        "migration root",
    );
    let current_memory = write(
        &menvane,
        &project,
        "Dashboard palette guidance",
        "dashboard palette current",
    );
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Implement the migration root."),
    );
    let recall = menvane
        .prompt_recall(
            &project,
            "test-client",
            "external-session",
            "Document the dashboard palette current.",
            10,
        )
        .unwrap();
    assert_eq!(recall.results[0].id, current_memory.metadata.id);
    assert_ne!(recall.results[0].id, root_memory.metadata.id);
    drop(temporary);
}

#[test]
fn active_constraints_contribute_to_recall() {
    let (temporary, project, menvane) = setup();
    let constraint_memory = write(
        &menvane,
        &project,
        "Encrypted backup constraint",
        "encrypted backup constraint must preserve encrypted backups",
    );
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Implement storage migration."),
    );
    ingest(
        &menvane,
        &project,
        "constraint",
        NormalizedEventKind::UserPrompt,
        Some("Must preserve encrypted backups."),
    );
    let recall = menvane
        .prompt_recall(
            &project,
            "test-client",
            "external-session",
            "Continue storage migration.",
            10,
        )
        .unwrap();
    assert!(
        recall
            .results
            .iter()
            .any(|result| result.id == constraint_memory.metadata.id)
    );
    assert!(
        recall
            .diagnostics
            .queries
            .iter()
            .any(|query| query.source == "active-constraint-1")
    );
    drop(temporary);
}

#[test]
fn dormant_episode_does_not_contribute_to_recall() {
    let (temporary, project, menvane) = setup();
    let previous_memory = write(
        &menvane,
        &project,
        "Previous export guidance",
        "previous export command",
    );
    let current_memory = write(
        &menvane,
        &project,
        "Current dashboard guidance",
        "current dashboard palette",
    );
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Set the project foundation."),
    );
    ingest(
        &menvane,
        &project,
        "previous-goal",
        NormalizedEventKind::UserPrompt,
        Some("Separately implement the export command."),
    );
    ingest(
        &menvane,
        &project,
        "new-goal",
        NormalizedEventKind::UserPrompt,
        Some("Now review the dashboard colors."),
    );
    let recall = menvane
        .prompt_recall(
            &project,
            "test-client",
            "external-session",
            "Review the current dashboard palette.",
            10,
        )
        .unwrap();
    assert!(
        recall
            .results
            .iter()
            .any(|result| result.id == current_memory.metadata.id)
    );
    assert!(
        !recall
            .results
            .iter()
            .any(|result| result.id == previous_memory.metadata.id)
    );
    assert!(
        !recall
            .diagnostics
            .queries
            .iter()
            .any(|query| query.query.contains("export command"))
    );
    drop(temporary);
}

#[test]
fn project_variant_beats_equivalent_global_variant() {
    let (temporary, project, menvane) = setup();
    let global = menvane
        .write(
            &project,
            WriteMemory {
                title: "Shared deployment guardrail".to_owned(),
                body: "deployment guardrail shared guidance".to_owned(),
                memory_type: MemoryType::Fact,
                scope: Scope::Global,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )
        .unwrap();
    let project_variant = write(
        &menvane,
        &project,
        "Shared deployment guardrail",
        "deployment guardrail project guidance",
    );
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Use the shared deployment guardrail."),
    );
    let recall = menvane
        .prompt_recall(
            &project,
            "test-client",
            "external-session",
            "Use the shared deployment guardrail.",
            10,
        )
        .unwrap();
    assert_eq!(
        recall
            .results
            .iter()
            .find(|result| result.title == "Shared deployment guardrail")
            .unwrap()
            .id,
        project_variant.metadata.id
    );
    assert!(
        !recall
            .results
            .iter()
            .any(|result| result.id == global.metadata.id)
    );
    drop(temporary);
}

#[test]
fn diagnostics_recompute_final_scores() {
    let (temporary, project, menvane) = setup();
    let memory = write(
        &menvane,
        &project,
        "Diagnostic storage guidance",
        "diagnostic storage guidance",
    );
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Implement diagnostic storage guidance."),
    );
    let recall = menvane
        .prompt_recall(
            &project,
            "test-client",
            "external-session",
            "Implement diagnostic storage guidance.",
            10,
        )
        .unwrap();
    let result = recall
        .results
        .iter()
        .find(|result| result.id == memory.metadata.id)
        .unwrap();
    let diagnostic = recall
        .diagnostics
        .results
        .iter()
        .find(|diagnostic| diagnostic.memory_id == memory.metadata.id.to_string())
        .unwrap();
    let fused = diagnostic
        .sources
        .iter()
        .map(|source| source.contribution)
        .sum::<f64>();
    let recomputed = fused
        * diagnostic.lifecycle_multiplier
        * diagnostic.type_multiplier
        * diagnostic.confidence_multiplier
        * diagnostic.freshness_multiplier
        * diagnostic.applicability_multiplier
        * diagnostic.scope_multiplier;
    assert!((fused - diagnostic.fused_rrf).abs() < f64::EPSILON);
    assert!((recomputed - result.score).abs() < f64::EPSILON);
    assert!((recomputed - diagnostic.final_score).abs() < f64::EPSILON);
    drop(temporary);
}

#[test]
fn oversized_prompt_is_sanitized_before_retrieval() {
    let (temporary, project, menvane) = setup();
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Implement bounded retrieval."),
    );
    let prompt = "raw-secret retrieval ".repeat(2_000);
    let recall = menvane
        .prompt_recall(&project, "test-client", "external-session", &prompt, 10)
        .unwrap();
    assert!(
        recall
            .diagnostics
            .queries
            .iter()
            .all(|query| query.query.len() <= menvane_engine::MAX_RECALL_PROMPT_BYTES)
    );
    assert!(recall.diagnostics.queries.iter().all(|query| {
        !query
            .query
            .contains("raw-secret retrieval ".repeat(2_000).as_str())
    }));
    drop(temporary);
}

fn setup() -> (TempDir, std::path::PathBuf, Menvane) {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    (temporary, project, menvane)
}

fn write(
    menvane: &Menvane,
    project: &std::path::Path,
    title: &str,
    body: &str,
) -> menvane_domain::Memory {
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
}

fn ingest(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    prompt: Option<&str>,
) {
    menvane
        .ingest_event(NormalizedEvent {
            event_id: id.to_owned(),
            kind,
            origin: Default::default(),
            role: Default::default(),
            client: "test-client".to_owned(),
            external_session_id: "external-session".to_owned(),
            timestamp: Utc::now() + Duration::milliseconds(id.len() as i64),
            cwd: project.to_string_lossy().into_owned(),
            project_id: None,
            tool_family: None,
            bounded_input: prompt.map(str::to_owned),
            bounded_output: None,
            attributed_path: None,
            success: None,
            model: None,
        })
        .unwrap();
}
