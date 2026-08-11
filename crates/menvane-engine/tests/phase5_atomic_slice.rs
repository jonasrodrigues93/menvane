use std::fs;

use chrono::{Duration, TimeZone, Utc};
use menvane_domain::{
    EpisodeState, IntentClassificationSource, NormalizedEvent, NormalizedEventKind,
    NormalizedEventOrigin, NormalizedEventRole, PromptIntent, PromptIntentKind, TaskEpisode,
};
use menvane_engine::{CaptureOutcome, EvidenceBuilder, Menvane};
use menvane_store::EpisodeEvent;
use tempfile::TempDir;
use uuid::Uuid;

mod common;

#[test]
fn packet_budget_priority_and_event_references_are_bounded() {
    let episode = episode();
    let events = vec![
        episode_event(
            "goal",
            NormalizedEventKind::UserPrompt,
            Some("Implement export"),
            None,
        ),
        episode_event(
            "correction",
            NormalizedEventKind::UserPrompt,
            Some("Correction: use the JSON export format"),
            None,
        ),
        episode_event(
            "late-prompt",
            NormalizedEventKind::UserPrompt,
            Some("The final export must preserve the observed outcome"),
            None,
        ),
        tool(
            "failure",
            "cargo check",
            false,
            Some("src/export.rs"),
            Some("compile failed"),
        ),
        tool(
            "resolution",
            "cargo check",
            true,
            Some("src/export.rs"),
            Some("tests passed"),
        ),
        tool("noise-a", "edit", true, Some("src/export.rs"), None),
        tool("noise-b", "edit", true, Some("src/export.rs"), None),
        episode_event(
            "compact",
            NormalizedEventKind::ContextCompacted,
            Some("The context summary says tests passed"),
            None,
        ),
        episode_event(
            "question",
            NormalizedEventKind::UserPrompt,
            Some("Which export field remains unresolved?"),
            None,
        ),
    ];
    let intents = vec![prompt_intent("correction", PromptIntentKind::Correction)];
    let packet = EvidenceBuilder::new(2_400)
        .build(&episode, &events, &intents)
        .unwrap();

    assert!(packet.goal.content.contains("Implement export"));
    assert!(
        packet
            .prompts
            .iter()
            .any(|item| item.event_id == "correction")
    );
    assert!(
        packet
            .prompts
            .iter()
            .any(|item| item.event_id == "late-prompt")
    );
    assert_eq!(packet.actions.len(), 3);
    assert!(
        packet
            .actions
            .iter()
            .any(|item| item.content.contains("2 repetitions"))
    );
    assert!(
        packet
            .errors
            .iter()
            .any(|item| item.content.contains("[event:resolution]"))
    );
    assert!(
        packet
            .validations
            .iter()
            .all(|item| item.event_id != "compact")
    );
    assert!(packet.compaction_context.is_empty());
    assert!(
        packet
            .unresolved_questions
            .iter()
            .any(|item| item.event_id == "question")
    );
    assert!(
        packet
            .validations
            .iter()
            .any(|item| item.content.contains("tests passed"))
    );
    assert!(packet.files.contains(&"src/export.rs".to_owned()));
    assert!(serde_json::to_vec(&packet).unwrap().len() <= 2_400);
    for item in packet_items(&packet) {
        assert!(
            events
                .iter()
                .any(|event| event.event.event_id == item.event_id)
        );
    }
}

#[test]
fn instruction_and_metadata_events_are_excluded_from_evidence() {
    let episode = episode();
    let mut system = episode_event(
        "system",
        NormalizedEventKind::UserPrompt,
        Some("<available-skills>agent instructions</available-skills>"),
        None,
    );
    system.event.origin = NormalizedEventOrigin::System;
    system.event.role = NormalizedEventRole::SystemPrompt;
    let mut metadata = tool("metadata", "tool metadata", true, Some("AGENTS.md"), None);
    metadata.event.origin = NormalizedEventOrigin::Tool;
    metadata.event.role = NormalizedEventRole::ToolMetadata;
    let mut compacted = episode_event(
        "compacted",
        NormalizedEventKind::ContextCompacted,
        Some("<recommended_plugins>plugin metadata</recommended_plugins>"),
        None,
    );
    compacted.event.origin = NormalizedEventOrigin::Compaction;
    compacted.event.role = NormalizedEventRole::CompactionSummary;

    let packet = EvidenceBuilder::new(4_096)
        .build(
            &episode,
            &[
                episode_event(
                    "goal",
                    NormalizedEventKind::UserPrompt,
                    Some("Implement the export"),
                    None,
                ),
                system,
                metadata,
                compacted,
            ],
            &[],
        )
        .unwrap();
    let serialized = serde_json::to_string(&packet).unwrap();
    assert!(!serialized.contains("available-skills"));
    assert!(!serialized.contains("recommended_plugins"));
    assert!(!serialized.contains("AGENTS.md"));
    assert!(packet.goal.content.contains("Implement the export"));
    assert!(packet.actions.is_empty());
    assert!(packet.compaction_context.is_empty());
}

#[test]
fn packet_budget_and_markdown_bounds_preserve_utf8() {
    let episode = episode();
    let events = vec![episode_event(
        "goal",
        NormalizedEventKind::UserPrompt,
        Some(&"é".repeat(2_000)),
        None,
    )];
    let packet = EvidenceBuilder::new(512)
        .build(&episode, &events, &[])
        .unwrap();

    assert!(serde_json::to_vec(&packet).unwrap().len() <= 512);
    let markdown = menvane_engine::render_episode_markdown(&packet, 257);
    assert!(markdown.len() <= 257);
    assert!(std::str::from_utf8(markdown.as_bytes()).is_ok());
}

#[test]
fn linked_episode_events_isolate_multi_episode_markdown_and_actual_outcomes() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
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
        "goal-a",
        NormalizedEventKind::UserPrompt,
        Some("Implement export parsing"),
    );
    ingest_tool(&menvane, &project, "edit-a", "edit", true, "src/export.rs");
    ingest(
        &menvane,
        &project,
        "goal-b",
        NormalizedEventKind::UserPrompt,
        Some("Now document the dashboard navigation"),
    );
    ingest_tool(
        &menvane,
        &project,
        "check-b",
        "cargo check",
        true,
        "docs/dashboard.md",
    );
    ingest(
        &menvane,
        &project,
        "end",
        NormalizedEventKind::SessionEnded,
        None,
    );
    assert!(process_one(&menvane));

    let session = menvane
        .all_memories()
        .unwrap()
        .into_iter()
        .find(|memory| memory.metadata.memory_type == menvane_domain::MemoryType::Session)
        .unwrap();
    assert_eq!(session.body.matches("## Task episode ").count(), 2);
    assert!(session.body.contains("Implement export parsing"));
    assert!(
        session
            .body
            .contains("Now document the dashboard navigation")
    );
    assert!(session.body.contains("[event:edit-a]"));
    assert!(session.body.contains("[event:check-b]"));
    assert!(session.body.contains("cargo check succeeded"));
    assert!(
        !session
            .body
            .contains("Session evidence was captured and finalized.")
    );
    assert!(session.body.len() <= 32_768);
    assert!(
        menvane
            .configuration_text()
            .unwrap()
            .contains("aggregate_evidence_budget_bytes")
    );
    let compilation_jobs = menvane
        .jobs()
        .unwrap()
        .into_iter()
        .filter(|job| job.job_type == "compile_session")
        .collect::<Vec<_>>();
    assert_eq!(compilation_jobs.len(), 2);
    assert!(
        compilation_jobs
            .iter()
            .all(|job| job.dedupe_key.contains(':'))
    );
    let (_, indexed_memories) = menvane.reindex().unwrap();
    assert!(indexed_memories >= 1);
}

fn packet_items(
    packet: &menvane_domain::EpisodeEvidencePacket,
) -> Vec<&menvane_domain::EvidenceItem> {
    packet
        .prompts
        .iter()
        .chain(packet.actions.iter())
        .chain(packet.decisions.iter())
        .chain(packet.discoveries.iter())
        .chain(packet.errors.iter())
        .chain(packet.validations.iter())
        .chain(packet.compaction_context.iter())
        .chain(packet.unresolved_questions.iter())
        .chain(std::iter::once(&packet.goal))
        .collect()
}

fn episode() -> TaskEpisode {
    TaskEpisode {
        id: Uuid::from_u128(7),
        project_id: Some("project".to_owned()),
        conversation_key: "conversation".to_owned(),
        root_event_id: "goal".to_owned(),
        goal: "Implement export".to_owned(),
        ordinal: 1,
        state: EpisodeState::Active,
        created_at: timestamp(0),
        updated_at: timestamp(0),
    }
}

fn prompt_intent(event_id: &str, kind: PromptIntentKind) -> PromptIntent {
    PromptIntent {
        event_id: event_id.to_owned(),
        episode_id: Uuid::from_u128(7),
        kind,
        confidence: 1.0,
        weight: 1.0,
        classifier_version: "test".to_owned(),
        source: IntentClassificationSource::Deterministic,
        classified_at: timestamp(1),
    }
}

fn episode_event(
    id: &str,
    kind: NormalizedEventKind,
    input: Option<&str>,
    output: Option<&str>,
) -> EpisodeEvent {
    EpisodeEvent {
        event: NormalizedEvent {
            event_id: id.to_owned(),
            kind,
            origin: Default::default(),
            role: Default::default(),
            client: "client".to_owned(),
            external_session_id: "session".to_owned(),
            timestamp: timestamp(id.len() as i64),
            cwd: "/tmp/project".to_owned(),
            project_id: Some("project".to_owned()),
            tool_family: None,
            bounded_input: input.map(str::to_owned),
            bounded_output: output.map(str::to_owned),
            attributed_path: None,
            success: None,
            model: None,
        harness_injected: false,
        },
        session_id: Uuid::from_u128(9),
        generation: 1,
        client: "client".to_owned(),
        external_session_id: "session".to_owned(),
        project_id: Some("project".to_owned()),
    }
}

fn tool(
    id: &str,
    family: &str,
    success: bool,
    path: Option<&str>,
    output: Option<&str>,
) -> EpisodeEvent {
    let mut event = episode_event(id, NormalizedEventKind::ToolCompleted, Some(family), output);
    event.event.tool_family = Some(family.to_owned());
    event.event.success = Some(success);
    event.event.attributed_path = path.map(str::to_owned);
    event
}

fn ingest(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    input: Option<&str>,
) {
    let event = NormalizedEvent {
        event_id: id.to_owned(),
        kind,
        origin: Default::default(),
        role: Default::default(),
        client: "client".to_owned(),
        external_session_id: "session".to_owned(),
        timestamp: Utc::now() + Duration::milliseconds(id.len() as i64),
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: None,
        bounded_input: input.map(str::to_owned),
        bounded_output: None,
        attributed_path: None,
        success: None,
        model: None,
        harness_injected: false,
    };
    assert_eq!(menvane.ingest_event(event).unwrap(), CaptureOutcome::Stored);
}

fn ingest_tool(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    family: &str,
    success: bool,
    path: &str,
) {
    let mut event = NormalizedEvent {
        event_id: id.to_owned(),
        kind: NormalizedEventKind::ToolCompleted,
        origin: Default::default(),
        role: Default::default(),
        client: "client".to_owned(),
        external_session_id: "session".to_owned(),
        timestamp: Utc::now() + Duration::milliseconds(id.len() as i64),
        cwd: project.to_string_lossy().into_owned(),
        project_id: None,
        tool_family: Some(family.to_owned()),
        bounded_input: Some(family.to_owned()),
        bounded_output: success.then(|| "tests passed".to_owned()),
        attributed_path: Some(path.to_owned()),
        success: Some(success),
        model: None,
        harness_injected: false,
    };
    event.timestamp += Duration::milliseconds(1);
    assert_eq!(menvane.ingest_event(event).unwrap(), CaptureOutcome::Stored);
}

fn process_one(menvane: &Menvane) -> bool {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(menvane.process_next_job())
        .unwrap()
}

fn timestamp(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).single().unwrap() + Duration::seconds(seconds)
}
