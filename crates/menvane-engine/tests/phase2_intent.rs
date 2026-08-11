use std::fs;

use chrono::{Duration, Utc};
use menvane_domain::{NormalizedEvent, NormalizedEventKind, PromptIntentKind};
use menvane_engine::{CLASSIFIER_VERSION, CaptureOutcome, Menvane};
use menvane_store::conversation_key;
use rusqlite::Connection;
use tempfile::TempDir;

mod common;

#[test]
fn deterministic_classifier_covers_root_constraint_correction_refinement_follow_up_and_topic_change()
 {
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
        "root",
        NormalizedEventKind::UserPrompt,
        Some("Add a bounded cache for project metadata."),
    );
    ingest(
        &menvane,
        &project,
        "constraint",
        NormalizedEventKind::UserPrompt,
        Some("Also keep the cache disabled by default."),
    );
    ingest(
        &menvane,
        &project,
        "follow-up",
        NormalizedEventKind::UserPrompt,
        Some("What about rollback?"),
    );
    ingest(
        &menvane,
        &project,
        "correction",
        NormalizedEventKind::UserPrompt,
        Some("Correction: the cache uses a bounded size."),
    );
    ingest(
        &menvane,
        &project,
        "new-goal",
        NormalizedEventKind::UserPrompt,
        Some("Now review the dashboard colors."),
    );

    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let intents = menvane
        .prompt_intents(
            &conversation_key("test-client", "external-session"),
            Some(&project_id),
        )
        .unwrap();
    assert_eq!(
        intents.iter().map(|intent| intent.kind).collect::<Vec<_>>(),
        vec![
            PromptIntentKind::RootGoal,
            PromptIntentKind::Constraint,
            PromptIntentKind::FollowUp,
            PromptIntentKind::Correction,
            PromptIntentKind::NewGoal,
        ]
    );

    let episodes = menvane
        .episodes(
            &conversation_key("test-client", "external-session"),
            Some(&project_id),
        )
        .unwrap();
    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[0].state, menvane_domain::EpisodeState::Dormant);
    assert_eq!(episodes[1].state, menvane_domain::EpisodeState::Active);
    assert_eq!(episodes[0].goal, "The cache uses a bounded size.");
    assert_eq!(episodes[1].goal, "Now review the dashboard colors.");
}

#[test]
fn continuation_crosses_generations_without_turn_stop_or_elapsed_time_boundary() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();

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
    ingest(
        &menvane,
        &project,
        "stop-1",
        NormalizedEventKind::TurnStopped,
        None,
    );
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

    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let intents = menvane
        .prompt_intents(
            &conversation_key("test-client", "external-session"),
            Some(&project_id),
        )
        .unwrap();
    assert_eq!(intents.len(), 2);
    assert_eq!(intents[1].kind, PromptIntentKind::Refinement);
    assert_eq!(intents[1].episode_id, intents[0].episode_id);
    assert_eq!(
        menvane
            .episodes(
                &conversation_key("test-client", "external-session"),
                Some(&project_id)
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn project_change_creates_an_isolated_root_episode() {
    let temporary = TempDir::new().unwrap();
    let project_a = temporary.path().join("project-a");
    let project_b = temporary.path().join("project-b");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    common::init_git(&project_a);
    common::init_git(&project_b);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();

    ingest(
        &menvane,
        &project_a,
        "start-1",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project_a,
        "prompt-1",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest(
        &menvane,
        &project_a,
        "end-1",
        NormalizedEventKind::SessionEnded,
        None,
    );
    ingest(
        &menvane,
        &project_b,
        "start-2",
        NormalizedEventKind::SessionStarted,
        None,
    );
    ingest(
        &menvane,
        &project_b,
        "prompt-2",
        NormalizedEventKind::UserPrompt,
        Some("Continue the export command."),
    );

    let project_a_id = menvane.ensure_project(&project_a).unwrap().unwrap().id;
    let project_b_id = menvane.ensure_project(&project_b).unwrap().unwrap().id;
    let first = menvane
        .episodes(
            &conversation_key("test-client", "external-session"),
            Some(&project_a_id),
        )
        .unwrap();
    let second = menvane
        .episodes(
            &conversation_key("test-client", "external-session"),
            Some(&project_b_id),
        )
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id);
    assert_eq!(
        menvane
            .prompt_intents(
                &conversation_key("test-client", "external-session"),
                Some(&project_b_id)
            )
            .unwrap()[0]
            .kind,
        PromptIntentKind::RootGoal
    );
}

#[test]
fn duplicate_prompt_repairs_missing_state_using_the_event_session_and_is_idempotent() {
    let temporary = TempDir::new().unwrap();
    let project = temporary.path().join("project");
    fs::create_dir_all(&project).unwrap();
    common::init_git(&project);
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    let prompt = event(
        &project,
        "prompt-1",
        NormalizedEventKind::UserPrompt,
        Some("Implement the export command."),
    );
    ingest(
        &menvane,
        &project,
        "start-1",
        NormalizedEventKind::SessionStarted,
        None,
    );
    menvane.ingest_event(prompt.clone()).unwrap();
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

    let state = Connection::open(home.join("state.sqlite")).unwrap();
    state
        .execute("DELETE FROM prompt_intents WHERE event_id='prompt-1'", [])
        .unwrap();
    state
        .execute(
            "DELETE FROM task_episodes WHERE root_event_id='prompt-1'",
            [],
        )
        .unwrap();
    drop(state);

    assert_eq!(
        menvane.ingest_event(prompt).unwrap(),
        CaptureOutcome::Duplicate
    );
    assert_eq!(
        menvane
            .ingest_event(event(
                &project,
                "prompt-1",
                NormalizedEventKind::UserPrompt,
                Some("Implement the export command.")
            ))
            .unwrap(),
        CaptureOutcome::Duplicate
    );
    let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
    let episodes = menvane
        .episodes(
            &conversation_key("test-client", "external-session"),
            Some(&project_id),
        )
        .unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].root_event_id, "prompt-1");
    assert_eq!(
        menvane
            .prompt_intents(
                &conversation_key("test-client", "external-session"),
                Some(&project_id)
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn classifier_diagnostics_are_provider_free_and_expose_plan_weights() {
    let temporary = TempDir::new().unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    let diagnostics = menvane.classifier_diagnostics();
    assert_eq!(diagnostics.version, CLASSIFIER_VERSION);
    assert_eq!(diagnostics.weights.current_prompt, 1.00);
    assert_eq!(diagnostics.weights.current_correction, 1.00);
    assert_eq!(diagnostics.weights.active_episode_goal, 0.85);
    assert_eq!(diagnostics.weights.active_constraints, 0.80);
    assert_eq!(diagnostics.weights.refinements, 0.70);
    assert_eq!(diagnostics.weights.conversation_root_goal, 0.35);
    assert_eq!(diagnostics.weights.previous_episodes, 0.10);
    assert_eq!(diagnostics.weights.operational_prompts, 0.05);
}

fn ingest(
    menvane: &Menvane,
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    prompt: Option<&str>,
) {
    menvane
        .ingest_event(event(project, id, kind, prompt))
        .unwrap();
}

fn event(
    project: &std::path::Path,
    id: &str,
    kind: NormalizedEventKind,
    prompt: Option<&str>,
) -> NormalizedEvent {
    NormalizedEvent {
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
    }
}
