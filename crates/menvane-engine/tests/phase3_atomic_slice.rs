use std::fs;

use chrono::{Duration, Utc};
use menvane_domain::{Applicability, MemoryType, NormalizedEvent, NormalizedEventKind, Scope};
use menvane_engine::{Menvane, WriteMemory};
use rusqlite::Connection;
use tempfile::TempDir;

mod common;

#[test]
fn budget_omission_is_not_claimed_and_retrieval_is_separate() {
    let (temporary, project, menvane) = setup();
    for index in 0..20 {
        write(
            &menvane,
            &project,
            &format!("Budget memory {index}"),
            &format!("budget marker {index} {}", "bounded content ".repeat(60)),
            MemoryType::Fact,
        );
    }
    let recall = menvane
        .prompt_recall(&project, "client-a", "conversation-a", "budget marker", 20)
        .unwrap();
    let ids = recall
        .results
        .iter()
        .map(|result| result.id)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 20);
    let (context, _) = menvane
        .prompt_context_for_client(&project, "client-a", "conversation-a", "budget marker")
        .unwrap();
    assert!(context.chars().count() <= 6_000);
    let claims = Connection::open(temporary.path().join("home/state.sqlite")).unwrap();
    let retrieved: u64 = claims
        .query_row(
            "SELECT COUNT(*) FROM access_events WHERE signal='retrieved'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let injected: u64 = claims
        .query_row(
            "SELECT COUNT(*) FROM access_events WHERE signal='injected'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(retrieved >= 40);
    assert!(injected < retrieved);
    for id in ids {
        if context.contains(&id.to_string()) {
            continue;
        }
        let claimed: u64 = claims
            .query_row(
                "SELECT COUNT(*) FROM session_injections WHERE memory_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(claimed, 0, "omitted memory {id} was claimed");
    }
}

#[test]
fn rendered_entries_have_metadata_and_no_full_body() {
    let (_temporary, project, menvane) = setup();
    let memory = write(
        &menvane,
        &project,
        "Metadata memory",
        &format!(
            "metadata marker {}{}",
            "visible content ".repeat(60),
            "private body ".repeat(100)
        ),
        MemoryType::Decision,
    );
    let context = menvane
        .prompt_context(&project, "metadata marker", "metadata-session")
        .unwrap();
    for field in [
        "ID:",
        "Type:",
        "Scope:",
        "Status:",
        "Confidence:",
        "Age:",
        "Provenance:",
        "Relevance:",
    ] {
        assert!(context.contains(field), "missing {field}");
    }
    assert!(context.contains(&memory.metadata.id.to_string()));
    assert!(context.contains("Provenance: source sessions 0; supersession count 0"));
    assert!(!context.contains("bounded indexed excerpt"));
    assert!(!context.contains("private body private body private body private body"));
}

#[test]
fn active_constraints_are_required_without_mcp() {
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
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Implement the storage change."),
    );
    ingest(
        &menvane,
        &project,
        "constraint",
        NormalizedEventKind::UserPrompt,
        Some("The migration must preserve encrypted backups."),
    );
    let context = menvane
        .prompt_context_for_client(
            &project,
            "test-client",
            "external-session",
            "Continue the storage change.",
        )
        .unwrap()
        .0;
    assert!(context.contains("The migration must preserve encrypted backups."));
    assert!(context.contains("REQUIRED ACTIVE CONSTRAINT OR CORRECTION"));
}

#[test]
fn retrieval_cards_contain_ids_usable_by_read() {
    let (_temporary, project, menvane) = setup();
    for index in 0..8 {
        write(
            &menvane,
            &project,
            &format!("Card memory {index}"),
            &format!("card marker {index}"),
            MemoryType::Fact,
        );
    }
    let recall = menvane
        .prompt_recall(&project, "card-client", "card-session", "card marker", 20)
        .unwrap();
    let context = menvane
        .prompt_context_for_client(&project, "card-client", "card-session", "card marker")
        .unwrap()
        .0;
    let card = recall
        .results
        .iter()
        .skip(6)
        .find(|result| context.contains(&result.id.to_string()))
        .expect("expected a retrieval card");
    let memory = menvane.read(card.id).unwrap();
    assert_eq!(memory.metadata.id, card.id);
}

#[test]
fn injection_dedupe_is_identity_aware() {
    let (_temporary, project, menvane) = setup();
    write(
        &menvane,
        &project,
        "Identity memory",
        "identity marker",
        MemoryType::Fact,
    );
    let first = menvane
        .prompt_context_for_client(&project, "client-a", "same-session", "identity marker")
        .unwrap()
        .0;
    let repeated = menvane
        .prompt_context_for_client(&project, "client-a", "same-session", "identity marker")
        .unwrap()
        .0;
    let other_client = menvane
        .prompt_context_for_client(&project, "client-b", "same-session", "identity marker")
        .unwrap()
        .0;
    assert!(!first.is_empty());
    assert!(repeated.is_empty());
    assert!(!other_client.is_empty());
}

#[test]
fn briefing_delivery_is_once_without_suppressing_prompt_recall() {
    let (_temporary, project, menvane) = setup();
    let first = menvane
        .session_briefing_for_client(&project, "client", "shared-session")
        .unwrap();
    let repeated = menvane
        .session_briefing_for_client(&project, "client", "shared-session")
        .unwrap();
    write(
        &menvane,
        &project,
        "Prompt-only memory",
        "prompt-only marker",
        MemoryType::Fact,
    );
    let prompt = menvane
        .prompt_context_for_client(&project, "client", "shared-session", "prompt-only marker")
        .unwrap()
        .0;
    assert!(!first.is_empty());
    assert!(repeated.is_empty());
    assert!(prompt.contains("Prompt-only memory"));
}

#[test]
fn required_budget_overflow_keeps_gotcha_in_secondary_delivery() {
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
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Implement the fallback change."),
    );
    let constraint = format!("Constraint: {}", "preserve this rule ".repeat(95));
    ingest(
        &menvane,
        &project,
        "constraint",
        NormalizedEventKind::UserPrompt,
        Some(&constraint),
    );
    write(
        &menvane,
        &project,
        &format!("Fallback gotcha {}", "long title ".repeat(15)),
        &format!("fallback gotcha marker {}", "supporting detail ".repeat(50)),
        MemoryType::Gotcha,
    );
    let context = menvane
        .prompt_context_for_client(
            &project,
            "test-client",
            "external-session",
            "fallback gotcha marker",
        )
        .unwrap()
        .0;
    assert!(context.contains("Fallback gotcha"));
    assert!(!context.contains("[REQUIRED CONTEXT]"));
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
    memory_type: MemoryType,
) -> menvane_domain::Memory {
    menvane
        .write(
            project,
            WriteMemory {
                title: title.to_owned(),
                body: body.to_owned(),
                memory_type,
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
