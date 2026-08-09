use std::fs;

use chrono::{Duration, Utc};
use menvane_domain::{NormalizedEvent, NormalizedEventKind};
use menvane_engine::{CaptureOutcome, Menvane};
use rusqlite::Connection;
use tempfile::TempDir;

mod common;

#[test]
fn meaningful_progress_creates_and_updates_one_current_handoff() {
    let (temporary, project, menvane) = setup();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn export() {}\n").unwrap();
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
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest_tool(&menvane, &project, "test", true, "src/lib.rs");
    assert!(menvane.process_next_checkpoint_job_blocking());
    let first = menvane
        .handoffs(&project)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    ingest_tool(&menvane, &project, "cargo test", true, "src/lib.rs");
    assert!(menvane.process_next_checkpoint_job_blocking());
    let second = menvane
        .handoffs(&project)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.status, menvane_domain::HandoffStatus::Active);
    assert!(second.validation.len() >= 2);
    assert_eq!(handoff_count(&temporary), 1);
}

#[test]
fn event_links_are_idempotent_cross_generation_and_isolated() {
    let (_temporary, project, menvane) = setup();
    ingest(
        &menvane,
        &project,
        "start-1",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "prompt-1",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest_tool(&menvane, &project, "cargo test", true, "src/export-1.rs");
    assert!(menvane.process_next_checkpoint_job_blocking());
    ingest(
        &menvane,
        &project,
        "end-1",
        NormalizedEventKind::SessionEnded,
        None,
    );
    ingest(
        &menvane,
        &project,
        "start-2",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project,
        "prompt-2",
        NormalizedEventKind::UserPrompt,
        Some("Continue the export command and add validation."),
    );
    ingest_tool(&menvane, &project, "cargo test", true, "src/export-2.rs");
    assert!(menvane.process_next_checkpoint_job_blocking());
    assert_eq!(menvane.handoffs(&project).unwrap().len(), 1);
    let handoff = menvane.handoffs(&project).unwrap().remove(0);
    assert!(handoff.source_event_ids.contains(&"prompt-1".to_owned()));
    assert!(handoff.source_event_ids.contains(&"prompt-2".to_owned()));

    ingest(
        &menvane,
        &project,
        "prompt-3",
        NormalizedEventKind::UserPrompt,
        Some("Now review the dashboard colors."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let handoffs = menvane.handoffs(&project).unwrap();
    assert_eq!(handoffs.len(), 2);
    assert!(
        handoffs
            .iter()
            .any(|value| value.source_event_ids.contains(&"prompt-3".to_owned()))
    );
    let before = menvane.jobs().unwrap().len();
    let _ = menvane.ingest_event(event(
        &project,
        "prompt-3",
        NormalizedEventKind::UserPrompt,
        Some("Now review the dashboard colors."),
    ));
    assert_eq!(menvane.jobs().unwrap().len(), before);
}

#[test]
fn checkpoint_generation_is_provider_free_and_flushes_debounced_work() {
    let (temporary, project, menvane) = setup();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn edit() {}\n").unwrap();
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
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement a bounded handoff."),
    );
    ingest_tool(&menvane, &project, "edit", true, "src/lib.rs");
    menvane.flush_dirty_checkpoints_blocking();
    let handoff = menvane.handoffs(&project).unwrap().remove(0);
    assert_eq!(handoff.changed_files, vec!["src/lib.rs"]);
    assert!(handoff.worktree_state_hash.is_some());
    assert_eq!(handoff.git_head, None);
    assert_eq!(handoff_count(&temporary), 1);
    assert!(!menvane.process_next_checkpoint_job_blocking());
}

#[test]
fn repository_facts_replace_prior_handoff_text_and_validation_is_deterministic() {
    let (_temporary, project, menvane) = setup();
    fs::write(project.join("src.rs"), "fn main() {}\n").unwrap();
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
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement deterministic validation."),
    );
    ingest_tool(&menvane, &project, "cargo check", false, "cargo check");
    assert!(menvane.process_next_checkpoint_job_blocking());
    let first = menvane.handoffs(&project).unwrap().remove(0);
    assert_eq!(first.changed_files, vec!["src.rs"]);
    assert_eq!(first.validation[0].summary, "cargo check failed");
    assert_eq!(first.validation[0].command.as_deref(), Some("cargo check"));
    assert!(
        first
            .current_state
            .contains("repository changed files: present")
    );
    ingest_tool(&menvane, &project, "cargo check", true, "cargo check");
    assert!(menvane.process_next_checkpoint_job_blocking());
    let second = menvane.handoffs(&project).unwrap().remove(0);
    assert_eq!(first.id, second.id);
    assert_eq!(second.changed_files, first.changed_files);
    assert_eq!(second.worktree_state_hash, first.worktree_state_hash);
    assert_eq!(second.validation[0].summary, first.validation[0].summary);
}

#[test]
fn lifecycle_status_is_ready_and_consumed_handoffs_reactivate_on_new_evidence() {
    let (_temporary, project, menvane) = setup();
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
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement lifecycle checkpoints."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let active = menvane.handoffs(&project).unwrap().remove(0);
    assert_eq!(active.status, menvane_domain::HandoffStatus::Active);

    ingest(
        &menvane,
        &project,
        "compact",
        NormalizedEventKind::ContextCompacted,
        Some("bounded context"),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let ready = menvane.handoffs(&project).unwrap().remove(0);
    assert_eq!(ready.status, menvane_domain::HandoffStatus::Ready);
    menvane.consume_handoff(ready.id).unwrap();
    let before = menvane.jobs().unwrap().len();
    assert_eq!(
        menvane
            .ingest_event(event(
                &project,
                "compact",
                NormalizedEventKind::ContextCompacted,
                Some("bounded context"),
            ))
            .unwrap(),
        CaptureOutcome::Duplicate
    );
    assert_eq!(menvane.jobs().unwrap().len(), before);
    assert_eq!(
        menvane.handoffs(&project).unwrap()[0].status,
        menvane_domain::HandoffStatus::Consumed
    );

    ingest(
        &menvane,
        &project,
        "stop",
        NormalizedEventKind::TurnStopped,
        None,
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    assert_eq!(menvane.handoffs(&project).unwrap()[0].id, ready.id);
    assert_eq!(
        menvane.handoffs(&project).unwrap()[0].status,
        menvane_domain::HandoffStatus::Ready
    );

    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    assert_eq!(
        menvane.handoffs(&project).unwrap()[0].status,
        menvane_domain::HandoffStatus::Ready
    );
}

#[test]
fn generated_evidence_respects_utf8_byte_bounds() {
    let (_temporary, project, menvane) = setup();
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let prompt = "é".repeat(1_200);
    ingest(
        &menvane,
        &project,
        "unicode-prompt",
        NormalizedEventKind::UserPrompt,
        Some(&prompt),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let handoff = menvane.handoffs(&project).unwrap().remove(0);
    assert!(handoff.goal.len() <= 2_048);
    assert!(handoff.goal.is_char_boundary(handoff.goal.len()));
}

#[test]
fn nonvalidation_tool_debounce_uses_handoff_configuration() {
    let (temporary, project, menvane) = setup();
    menvane
        .update_configuration_text("[handoff]\nnonvalidation_tool_debounce_seconds = 0\n")
        .unwrap();
    drop(menvane);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
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
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement configurable debounce."),
    );
    ingest_tool(&menvane, &project, "edit", true, "src/lib.rs");
    assert!(menvane.process_next_job_blocking());
}

fn setup() -> (TempDir, std::path::PathBuf, Menvane) {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    (temporary, project, menvane)
}

fn ingest(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    input: Option<&str>,
) {
    assert_eq!(
        menvane
            .ingest_event(event(project, id, kind, input))
            .unwrap(),
        CaptureOutcome::Stored
    );
}

fn ingest_tool(
    menvane: &Menvane,
    project: &std::path::Path,
    family: &str,
    success: bool,
    path: &str,
) {
    let mut event = event(
        project,
        &format!(
            "tool-{}-{}-{}",
            family.replace(' ', "-"),
            success,
            path.replace('/', "-")
        ),
        NormalizedEventKind::ToolCompleted,
        Some(family),
    );
    event.success = Some(success);
    event.attributed_path = Some(path.to_owned());
    assert_eq!(menvane.ingest_event(event).unwrap(), CaptureOutcome::Stored);
}

fn event(
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    input: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
        kind,
        client: "test-client".to_owned(),
        external_session_id: "external-session".to_owned(),
        timestamp: Utc::now() + Duration::milliseconds(id.len() as i64),
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: (kind == NormalizedEventKind::ToolCompleted)
            .then(|| input.unwrap_or("tool").to_owned()),
        bounded_input: input.map(str::to_owned),
        bounded_output: None,
        attributed_path: (kind == NormalizedEventKind::ToolCompleted)
            .then(|| "src/lib.rs".to_owned()),
        success: None,
        model: None,
    }
}

fn handoff_count(temporary: &TempDir) -> u64 {
    Connection::open(temporary.path().join("home/state.sqlite"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM handoffs", [], |row| row.get(0))
        .unwrap()
}

trait BlockingCheckpoints {
    fn process_next_job_blocking(&self) -> bool;
    fn process_next_checkpoint_job_blocking(&self) -> bool;
    fn flush_dirty_checkpoints_blocking(&self);
}

impl BlockingCheckpoints for Menvane {
    fn process_next_job_blocking(&self) -> bool {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.process_next_job())
            .unwrap()
    }

    fn process_next_checkpoint_job_blocking(&self) -> bool {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.process_next_checkpoint_job())
            .unwrap()
    }

    fn flush_dirty_checkpoints_blocking(&self) {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.flush_dirty_checkpoints())
            .unwrap();
    }
}
