use std::fs;
use std::process::Command;

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
    let topic_change_context = menvane
        .prompt_context_for_client(
            &project,
            "test-client",
            "external-session",
            "Now review the dashboard colors.",
        )
        .unwrap()
        .0;
    assert!(!topic_change_context.contains("[TASK HANDOFF]"));
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

#[test]
fn session_start_injects_one_unambiguous_handoff_and_first_prompt_dedupes_it() {
    let (temporary, project, menvane) = setup();
    ingest_as(
        &menvane,
        &project,
        "source-client",
        "source-session",
        "source-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest_as(
        &menvane,
        &project,
        "source-client",
        "source-session",
        "source-prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export parser."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    ingest_as(
        &menvane,
        &project,
        "source-client",
        "source-session",
        "source-end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(menvane.process_next_checkpoint_job_blocking());

    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let briefing = menvane
        .session_briefing_for_client(&project, "resume-client", "resume-session")
        .unwrap();
    assert!(briefing.contains("[TASK HANDOFF]"));
    assert!(briefing.contains("Implement the export parser."));
    assert!(briefing.contains("Fingerprint confidence: medium"));
    let handoff_id = menvane.handoffs(&project).unwrap()[0].id;
    assert_eq!(
        menvane.handoffs(&project).unwrap()[0].status,
        menvane_domain::HandoffStatus::Consumed
    );
    let connection = Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let full_deliveries: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM handoff_deliveries WHERE delivery_kind='full'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(full_deliveries, 1);

    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-prompt",
        NormalizedEventKind::UserPrompt,
        Some("Continue the export parser."),
    );
    let prompt = menvane
        .prompt_context_for_client(
            &project,
            "resume-client",
            "resume-session",
            "Continue the export parser.",
        )
        .unwrap()
        .0;
    assert!(!prompt.contains(&handoff_id.to_string()));
}

#[test]
fn repeated_briefing_does_not_consume_a_handoff_created_after_delivery() {
    let (_temporary, project, menvane) = setup();
    ingest(
        &menvane,
        &project,
        "start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let first = menvane
        .session_briefing_for_client(&project, "test-client", "external-session")
        .unwrap();
    assert!(!first.is_empty());
    ingest(
        &menvane,
        &project,
        "prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement delayed handoff delivery."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let handoff = menvane.handoffs(&project).unwrap().remove(0);

    let repeated = menvane
        .session_briefing_for_client(&project, "test-client", "external-session")
        .unwrap();

    assert!(repeated.is_empty());
    assert_eq!(
        menvane.handoffs(&project).unwrap().remove(0).status,
        handoff.status
    );
}

#[test]
fn ambiguous_session_start_returns_cards_without_guessing_current_state() {
    let (temporary, project, menvane) = setup();
    for (index, goal) in ["Implement export parsing.", "Implement dashboard colors."]
        .into_iter()
        .enumerate()
    {
        let session = format!("source-{index}");
        ingest_as(
            &menvane,
            &project,
            "source-client",
            &session,
            &format!("start-{index}"),
            NormalizedEventKind::SessionStarted,
            None,
        );
        ingest_as(
            &menvane,
            &project,
            "source-client",
            &session,
            &format!("prompt-{index}"),
            NormalizedEventKind::UserPrompt,
            Some(goal),
        );
        assert!(menvane.process_next_checkpoint_job_blocking());
        ingest_as(
            &menvane,
            &project,
            "source-client",
            &session,
            &format!("end-{index}"),
            NormalizedEventKind::SessionEnded,
            None,
        );
        assert!(menvane.process_next_checkpoint_job_blocking());
    }
    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let context = menvane
        .session_briefing_for_client(&project, "resume-client", "resume-session")
        .unwrap();
    assert!(!context.contains("[TASK HANDOFF]"));
    assert!(context.contains("[HISTORICAL HANDOFF CARD]"));
    assert_eq!(context.matches("[HISTORICAL HANDOFF CARD]").count(), 2);
    let connection = Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let card_deliveries: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM handoff_deliveries WHERE delivery_kind='card'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(card_deliveries, 2);
    assert!(
        menvane
            .handoffs(&project)
            .unwrap()
            .iter()
            .all(|handoff| handoff.status == menvane_domain::HandoffStatus::Ready)
    );
}

#[test]
fn first_prompt_can_resume_from_another_client_when_intent_matches() {
    let (_temporary, project, menvane) = setup();
    ingest_as(
        &menvane,
        &project,
        "client-a",
        "old-session",
        "old-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest_as(
        &menvane,
        &project,
        "client-a",
        "old-session",
        "old-prompt",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export parser."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    ingest_as(
        &menvane,
        &project,
        "client-a",
        "old-session",
        "old-end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    ingest_as(
        &menvane,
        &project,
        "client-b",
        "new-session",
        "new-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest_as(
        &menvane,
        &project,
        "client-b",
        "new-session",
        "new-prompt",
        NormalizedEventKind::UserPrompt,
        Some("Continue implementing the export parser."),
    );
    let context = menvane
        .prompt_context_for_client(
            &project,
            "client-b",
            "new-session",
            "Continue implementing the export parser.",
        )
        .unwrap()
        .0;
    assert!(context.contains("[TASK HANDOFF]"));
    assert!(context.contains("Implement the export parser."));
}

#[test]
fn fingerprint_mismatch_marks_stale_and_only_prompt_cards_are_historical() {
    let (_temporary, project, menvane) = setup();
    fs::write(project.join("tracked.rs"), "fn old() {}\n").unwrap();
    git(&project, ["add", "."]);
    git(&project, ["commit", "-m", "initial"]);
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
        Some("Implement tracked parser."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    fs::write(project.join("tracked.rs"), "fn new() {}\n").unwrap();
    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let _ = menvane
        .session_briefing_for_client(&project, "resume-client", "resume-session")
        .unwrap();
    assert_eq!(
        menvane.handoffs(&project).unwrap()[0].status,
        menvane_domain::HandoffStatus::Stale
    );
    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-prompt",
        NormalizedEventKind::UserPrompt,
        Some("Continue implementing tracked parser."),
    );
    let context = menvane
        .prompt_context_for_client(
            &project,
            "resume-client",
            "resume-session",
            "Continue implementing tracked parser.",
        )
        .unwrap()
        .0;
    assert!(!context.contains("[TASK HANDOFF]"));
    assert!(context.contains("[HISTORICAL HANDOFF CARD]"));
    assert!(context.contains("never current repository truth"));
}

#[test]
fn unborn_repository_hash_mismatch_marks_stale_without_a_head() {
    let (_temporary, project, menvane) = setup();
    fs::write(project.join("unborn.rs"), "fn old() {}\n").unwrap();
    git(&project, ["add", "unborn.rs"]);
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
        Some("Implement the unborn parser."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let generated = menvane.handoffs(&project).unwrap().remove(0);
    assert_eq!(generated.git_head, None);
    assert!(generated.worktree_state_hash.is_some());
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    fs::write(project.join("unborn.rs"), "fn new() {}\n").unwrap();
    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let _ = menvane
        .session_briefing_for_client(&project, "resume-client", "resume-session")
        .unwrap();
    assert_eq!(
        menvane.handoffs(&project).unwrap()[0].status,
        menvane_domain::HandoffStatus::Stale
    );
}

#[test]
fn completed_handoff_is_excluded_and_delivery_is_bounded_and_sanitized() {
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
        Some("Implement bounded delivery password=do-not-leak."),
    );
    assert!(menvane.process_next_checkpoint_job_blocking());
    let id = menvane.handoffs(&project).unwrap()[0].id;
    menvane.complete_handoff(id).unwrap();
    ingest_as(
        &menvane,
        &project,
        "resume-client",
        "resume-session",
        "resume-start",
        NormalizedEventKind::SessionStarted,
        None,
    );
    let context = menvane
        .session_briefing_for_client(&project, "resume-client", "resume-session")
        .unwrap();
    assert!(!context.contains(&id.to_string()));
    assert!(context.chars().count() <= 2_500);
    assert!(!context.contains("do-not-leak"));
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
    ingest_as(
        menvane,
        project,
        "test-client",
        "external-session",
        id,
        kind,
        input,
    );
}

fn ingest_as(
    menvane: &Menvane,
    project: &std::path::Path,
    client: &str,
    external_session_id: &str,
    id: &str,
    kind: NormalizedEventKind,
    input: Option<&str>,
) {
    assert_eq!(
        menvane
            .ingest_event(event_as(
                project,
                client,
                external_session_id,
                id,
                kind,
                input,
            ))
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
    event_as(project, "test-client", "external-session", id, kind, input)
}

fn event_as(
    project: &std::path::Path,
    client: &str,
    external_session_id: &str,
    id: &str,
    kind: NormalizedEventKind,
    input: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
        event_id: id.to_owned(),
        kind,
        client: client.to_owned(),
        external_session_id: external_session_id.to_owned(),
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

fn git<const N: usize>(project: &std::path::Path, args: [&str; N]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(project)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
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
