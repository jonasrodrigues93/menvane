use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Arc;

use menvane_engine::{MemoryCompiler, Menvane, ScopeSelection, WriteMemory};
use menvane_store::conversation_key;
use serde::Serialize;
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

mod common;
#[path = "common/phase0.rs"]
mod phase0;
use phase0::{Corpus, Fixture, FixtureProvider, compilation_input, load_corpus};

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    phase: u32,
    corpus: String,
    fixture_count: usize,
    metrics: HashMap<String, Metric>,
    fixtures: Vec<FixtureReport>,
}

#[derive(Debug, Serialize)]
struct Metric {
    value: Option<f64>,
    numerator: Option<usize>,
    denominator: Option<usize>,
    unit: Option<&'static str>,
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct FixtureReport {
    id: String,
    categories: Vec<String>,
    metrics: HashMap<String, Metric>,
}

#[tokio::test]
async fn phase0_corpus_compiles_replays_recall_and_matches_baseline() {
    let corpus = load_corpus();
    assert_eq!(corpus.schema_version, 1);
    assert_category_coverage(&corpus);
    assert_expected_sections(&corpus);

    let provider = Arc::new(FixtureProvider::new(&corpus));
    let compiler = MemoryCompiler::new(provider.clone());
    let mut compiled = HashMap::new();
    for fixture in &corpus.fixtures {
        let result = compiler.compile(compilation_input(fixture)).await.unwrap();
        assert_eq!(result.provider, "fixture");
        assert_eq!(result.model, "phase-0-deterministic");
        assert_compiled_operations(fixture, &result.operations);
        assert_compiled_memories(fixture, &result.memories);
        compiled.insert(fixture.id.clone(), result.memories);
    }

    let replay = evaluate_recall(&corpus, &compiled);
    let classification = evaluate_classification(&corpus);
    let calls = provider.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.fixture_id.as_str())
            .collect::<HashSet<_>>(),
        corpus
            .fixtures
            .iter()
            .map(|fixture| fixture.id.as_str())
            .collect::<HashSet<_>>()
    );
    let report = build_report(&corpus, &calls, &compiled, &replay, &classification);
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/phase0/baseline.json")).unwrap();
    assert_eq!(serde_json::to_value(report).unwrap(), expected);
}

fn assert_compiled_operations(fixture: &Fixture, actual: &[menvane_engine::CompiledOperation]) {
    let expected = fixture.expected.memory_operations.as_array().unwrap();
    assert_eq!(actual.len(), expected.len(), "{}", fixture.id);
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            actual.operation,
            expected["operation"].as_str().unwrap(),
            "{}",
            fixture.id
        );
        assert_eq!(
            serde_json::to_value(&actual.evidence_event_ids).unwrap(),
            expected["evidence_event_ids"]
        );
    }
}

fn assert_category_coverage(corpus: &Corpus) {
    let categories = corpus
        .fixtures
        .iter()
        .flat_map(|fixture| fixture.categories.iter().cloned())
        .collect::<HashSet<_>>();
    for category in [
        "single-task",
        "multi-task",
        "correction",
        "constraint",
        "follow-up",
        "topic-change",
        "failed-attempt",
        "successful-validation",
        "conflicting-facts",
        "repeated-procedure",
        "forgotten-knowledge",
        "continuation-across-generations",
    ] {
        assert!(categories.contains(category), "missing category {category}");
    }
}

fn assert_expected_sections(corpus: &Corpus) {
    for fixture in &corpus.fixtures {
        assert!(!fixture.id.is_empty());
        assert!(!fixture.session.events.is_empty());
        assert!(fixture.expected.episodes.is_array());
        assert!(fixture.expected.intents.is_array());
        assert!(fixture.expected.handoffs.is_array());
        assert!(fixture.expected.evidence.is_array());
        assert!(fixture.expected.memory_operations.is_array());
        for memory in &fixture.expected.durable_memories {
            assert!(!memory.title.is_empty());
            assert!(!memory.body.is_empty());
        }
    }
}

fn assert_compiled_memories(fixture: &Fixture, actual: &[menvane_engine::CompiledMemory]) {
    assert_eq!(
        actual.len(),
        fixture.expected.durable_memories.len(),
        "{}",
        fixture.id
    );
    for (actual, expected) in actual.iter().zip(&fixture.expected.durable_memories) {
        assert_eq!(actual.memory_type.to_string(), expected.memory_type);
        assert_eq!(actual.title, expected.title);
        assert_eq!(actual.scope.to_string(), expected.scope);
        assert_eq!(actual.confidence, expected.confidence);
        assert_eq!(actual.body, expected.body);
        assert_eq!(actual.evidence, expected.evidence);
    }
}

fn evaluate_recall(
    corpus: &Corpus,
    compiled: &HashMap<String, Vec<menvane_engine::CompiledMemory>>,
) -> ReplayMetrics {
    let mut scores = HashMap::new();
    let mut procedure_successes = 0;
    let mut procedure_attempts = 0;
    let mut forgotten_recreations = 0;
    let mut forgotten_fixtures = 0;
    for fixture in &corpus.fixtures {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        common::init_git(&project);
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        for memory in compiled.get(&fixture.id).unwrap() {
            let stored = menvane
                .write(
                    &project,
                    WriteMemory {
                        title: memory.title.clone(),
                        body: memory.body.clone(),
                        memory_type: memory.memory_type,
                        scope: memory.scope,
                        confidence: memory.confidence,
                        tags: Vec::new(),
                        applies_to: memory.applies_to.clone(),
                    },
                )
                .unwrap();
            if fixture
                .categories
                .iter()
                .any(|category| category == "repeated-procedure")
                && memory.memory_type == menvane_domain::MemoryType::Procedure
            {
                procedure_attempts += 1;
                let applied = menvane
                    .record_procedure_application(stored.metadata.id, Uuid::from_u128(1), true)
                    .unwrap();
                if applied.metadata.status == menvane_domain::MemoryStatus::Active
                    && applied.metadata.successes == Some(2)
                {
                    procedure_successes += 1;
                }
            }
        }
        if fixture
            .categories
            .iter()
            .any(|category| category == "forgotten-knowledge")
        {
            forgotten_fixtures += 1;
            if !compiled.get(&fixture.id).unwrap().is_empty() {
                forgotten_recreations += 1;
            }
        }
        for expectation in &fixture.expected.recall {
            let actual = menvane
                .search(&project, &expectation.query, ScopeSelection::Auto, 6)
                .unwrap()
                .into_iter()
                .map(|result| result.title)
                .collect::<Vec<_>>();
            assert_eq!(actual, expectation.expected_titles, "{}", fixture.id);
            scores.insert(
                format!("{}:{}", fixture.id, expectation.query),
                RecallScore {
                    relevant: expectation.expected_titles.len(),
                    retrieved: actual,
                    expected: expectation.expected_titles.clone(),
                },
            );
        }
    }
    let (cross_project_leaks, cross_project_cases) = evaluate_project_isolation();
    ReplayMetrics {
        recall: scores,
        cross_project_leaks,
        cross_project_cases,
        procedure_successes,
        procedure_attempts,
        forgotten_recreations,
        forgotten_fixtures,
    }
}

#[derive(Debug)]
struct RecallScore {
    relevant: usize,
    retrieved: Vec<String>,
    expected: Vec<String>,
}

struct ReplayMetrics {
    recall: HashMap<String, RecallScore>,
    cross_project_leaks: usize,
    cross_project_cases: usize,
    procedure_successes: usize,
    procedure_attempts: usize,
    forgotten_recreations: usize,
    forgotten_fixtures: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct ClassificationMetrics {
    boundary_matches: usize,
    predicted_boundaries: usize,
    expected_boundaries: usize,
    intent_matches: usize,
    expected_intents: usize,
}

#[derive(Debug, Default)]
struct ClassificationEvaluation {
    total: ClassificationMetrics,
    fixtures: HashMap<String, ClassificationMetrics>,
}

fn evaluate_classification(corpus: &Corpus) -> ClassificationEvaluation {
    let mut groups: HashMap<String, Vec<&Fixture>> = HashMap::new();
    for fixture in &corpus.fixtures {
        groups
            .entry(format!(
                "{}\0{}",
                fixture.session.client, fixture.session.external_session_id
            ))
            .or_default()
            .push(fixture);
    }
    let mut evaluation = ClassificationEvaluation::default();
    for fixtures in groups.into_values() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        common::init_git(&project);
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let mut events = fixtures
            .iter()
            .flat_map(|fixture| fixture.session.events.iter().cloned())
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.timestamp);
        for mut event in events {
            event.cwd = project.to_string_lossy().into_owned();
            menvane.ingest_event(event).unwrap();
        }
        let project_id = menvane
            .ensure_project(&project)
            .unwrap()
            .map(|project| project.id);
        let key = conversation_key(
            &fixtures[0].session.client,
            &fixtures[0].session.external_session_id,
        );
        let episodes = menvane.episodes(&key, project_id.as_deref()).unwrap();
        let intents = menvane.prompt_intents(&key, project_id.as_deref()).unwrap();
        for fixture in fixtures {
            let expected_roots = fixture
                .expected
                .episodes
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|episode| {
                    episode
                        .get("event_ids")
                        .and_then(Value::as_array)
                        .and_then(|events| events.first())
                        .and_then(Value::as_str)
                })
                .collect::<HashSet<_>>();
            let predicted_roots = episodes
                .iter()
                .map(|episode| episode.root_event_id.as_str())
                .collect::<HashSet<_>>();
            let expected_intents = fixture.expected.intents.as_array().unwrap();
            let actual_intents = intents
                .iter()
                .map(|intent| (intent.event_id.clone(), intent.kind))
                .collect::<HashMap<_, _>>();
            let intent_matches = expected_intents
                .iter()
                .filter(|expected| {
                    let event_id = expected.get("event_id").and_then(Value::as_str);
                    let kind = expected.get("kind").and_then(Value::as_str);
                    event_id.zip(kind).is_some_and(|(event_id, kind)| {
                        actual_intents
                            .get(event_id)
                            .is_some_and(|actual| format_intent_kind(*actual) == kind)
                    })
                })
                .count();
            let metrics = ClassificationMetrics {
                boundary_matches: expected_roots.intersection(&predicted_roots).count(),
                predicted_boundaries: predicted_roots.len(),
                expected_boundaries: expected_roots.len(),
                intent_matches,
                expected_intents: expected_intents.len(),
            };
            evaluation.total.boundary_matches += metrics.boundary_matches;
            evaluation.total.predicted_boundaries += metrics.predicted_boundaries;
            evaluation.total.expected_boundaries += metrics.expected_boundaries;
            evaluation.total.intent_matches += metrics.intent_matches;
            evaluation.total.expected_intents += metrics.expected_intents;
            evaluation.fixtures.insert(fixture.id.clone(), metrics);
        }
    }
    evaluation
}

fn format_intent_kind(kind: menvane_domain::PromptIntentKind) -> &'static str {
    match kind {
        menvane_domain::PromptIntentKind::RootGoal => "root-goal",
        menvane_domain::PromptIntentKind::NewGoal => "new-goal",
        menvane_domain::PromptIntentKind::Refinement => "refinement",
        menvane_domain::PromptIntentKind::Constraint => "constraint",
        menvane_domain::PromptIntentKind::Correction => "correction",
        menvane_domain::PromptIntentKind::FollowUp => "follow-up",
        menvane_domain::PromptIntentKind::Operational => "operational",
    }
}

fn evaluate_project_isolation() -> (usize, usize) {
    let temporary = TempDir::new().unwrap();
    let project_a = temporary.path().join("project-a");
    let project_b = temporary.path().join("project-b");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    common::init_git(&project_a);
    common::init_git(&project_b);
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();
    menvane
        .write(
            &project_a,
            WriteMemory {
                title: "Project A only memory".to_owned(),
                body: "project-a-isolation-marker".to_owned(),
                memory_type: menvane_domain::MemoryType::Fact,
                scope: menvane_domain::Scope::Project,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: menvane_domain::Applicability::default(),
            },
        )
        .unwrap();
    let leaked = menvane
        .recall(&project_b, "project-a-isolation-marker", 6)
        .unwrap()
        .len();
    (usize::from(leaked > 0), 1)
}

fn build_report(
    corpus: &Corpus,
    calls: &[phase0::ProviderCall],
    compiled: &HashMap<String, Vec<menvane_engine::CompiledMemory>>,
    replay: &ReplayMetrics,
    classification: &ClassificationEvaluation,
) -> Report {
    let expected_count = corpus
        .fixtures
        .iter()
        .map(|fixture| fixture.expected.durable_memories.len())
        .sum::<usize>();
    let actual_count = compiled.values().map(Vec::len).sum::<usize>();
    let matched_count = corpus
        .fixtures
        .iter()
        .flat_map(|fixture| fixture.expected.durable_memories.iter())
        .count();
    let evidence_total = compiled
        .values()
        .flatten()
        .filter(|memory| !memory.evidence.is_empty())
        .count();
    let duplicate_count = compiled
        .values()
        .flatten()
        .map(|memory| {
            format!(
                "{}:{}",
                memory.memory_type,
                memory.title.to_ascii_lowercase()
            )
        })
        .collect::<Vec<_>>();
    let unique_count = duplicate_count.iter().collect::<HashSet<_>>().len();
    let recall_relevant = replay
        .recall
        .values()
        .map(|score| score.relevant)
        .sum::<usize>();
    let recall_retrieved = replay
        .recall
        .values()
        .map(|score| score.retrieved.len())
        .sum::<usize>();
    let recall_hits = replay
        .recall
        .values()
        .map(|score| {
            score
                .retrieved
                .iter()
                .filter(|title| score.expected.contains(title))
                .count()
        })
        .sum::<usize>();
    let mut metrics = HashMap::new();
    for name in [
        "handoff_selection_precision",
        "stale_handoff_rejection_rate",
        "resume_success_without_repository_rediscovery",
    ] {
        metrics.insert(
            name.to_owned(),
            not_meaningful("episode and handoff engines are planned for later phases"),
        );
    }
    metrics.insert(
        "episode_boundary_precision".to_owned(),
        ratio(
            classification.total.boundary_matches,
            classification.total.predicted_boundaries,
        ),
    );
    metrics.insert(
        "episode_boundary_recall".to_owned(),
        ratio(
            classification.total.boundary_matches,
            classification.total.expected_boundaries,
        ),
    );
    metrics.insert(
        "prompt_intent_classification_accuracy".to_owned(),
        ratio(
            classification.total.intent_matches,
            classification.total.expected_intents,
        ),
    );
    metrics.insert(
        "memory_extraction_precision".to_owned(),
        ratio(matched_count, actual_count),
    );
    metrics.insert(
        "memory_extraction_recall".to_owned(),
        ratio(matched_count, expected_count),
    );
    metrics.insert("unsupported_memory_rate".to_owned(), ratio(0, actual_count));
    metrics.insert(
        "important_knowledge_omission_rate".to_owned(),
        ratio(0, expected_count),
    );
    metrics.insert(
        "duplicate_memory_rate".to_owned(),
        ratio(actual_count.saturating_sub(unique_count), actual_count),
    );
    metrics.insert(
        "contradiction_resolution_accuracy".to_owned(),
        not_meaningful("explicit compilation operations are planned for Phase 6"),
    );
    metrics.insert(
        "provenance_coverage".to_owned(),
        ratio(evidence_total, actual_count),
    );
    metrics.insert(
        "forgotten_memory_recreation_rate".to_owned(),
        ratio(replay.forgotten_recreations, replay.forgotten_fixtures),
    );
    metrics.insert(
        "recall_precision_at_6".to_owned(),
        ratio(recall_hits, recall_retrieved),
    );
    metrics.insert(
        "recall_recall_at_6".to_owned(),
        ratio(recall_hits, recall_relevant),
    );
    metrics.insert(
        "cross_project_leakage_rate".to_owned(),
        ratio(replay.cross_project_leaks, replay.cross_project_cases),
    );
    metrics.insert(
        "procedure_reuse_success_rate".to_owned(),
        ratio(replay.procedure_successes, replay.procedure_attempts),
    );
    metrics.insert(
        "compilation_input_bytes".to_owned(),
        Metric {
            value: Some(calls.iter().map(|call| call.input_bytes).sum::<usize>() as f64),
            numerator: Some(calls.iter().map(|call| call.input_bytes).sum()),
            denominator: Some(calls.len()),
            unit: Some("bytes"),
            reason: None,
        },
    );
    metrics.insert(
        "compilation_latency_ms".to_owned(),
        not_meaningful("deterministic baseline excludes machine-dependent latency"),
    );
    metrics.insert(
        "provider_calls".to_owned(),
        Metric {
            value: Some(calls.len() as f64),
            numerator: Some(calls.len()),
            denominator: Some(corpus.fixtures.len()),
            unit: Some("calls"),
            reason: None,
        },
    );
    let fixtures = corpus
        .fixtures
        .iter()
        .map(|fixture| {
            let expected = fixture.expected.durable_memories.len();
            let actual = compiled.get(&fixture.id).unwrap().len();
            let evidence = compiled
                .get(&fixture.id)
                .unwrap()
                .iter()
                .filter(|memory| !memory.evidence.is_empty())
                .count();
            let fixture_calls = calls
                .iter()
                .filter(|call| call.fixture_id == fixture.id)
                .collect::<Vec<_>>();
            let scores = replay
                .recall
                .iter()
                .filter(|(key, _)| key.starts_with(&format!("{}:", fixture.id)))
                .map(|(_, score)| score)
                .collect::<Vec<_>>();
            let relevant = scores.iter().map(|score| score.relevant).sum::<usize>();
            let retrieved = scores
                .iter()
                .map(|score| score.retrieved.len())
                .sum::<usize>();
            let hits = scores
                .iter()
                .map(|score| {
                    score
                        .retrieved
                        .iter()
                        .filter(|title| score.expected.contains(title))
                        .count()
                })
                .sum::<usize>();
            let mut fixture_metrics = HashMap::new();
            fixture_metrics.insert(
                "memory_extraction_precision".to_owned(),
                ratio(expected, actual),
            );
            let classification = classification.fixtures.get(&fixture.id).unwrap();
            fixture_metrics.insert(
                "episode_boundary_precision".to_owned(),
                ratio(
                    classification.boundary_matches,
                    classification.predicted_boundaries,
                ),
            );
            fixture_metrics.insert(
                "episode_boundary_recall".to_owned(),
                ratio(
                    classification.boundary_matches,
                    classification.expected_boundaries,
                ),
            );
            fixture_metrics.insert(
                "prompt_intent_classification_accuracy".to_owned(),
                ratio(
                    classification.intent_matches,
                    classification.expected_intents,
                ),
            );
            fixture_metrics.insert(
                "memory_extraction_recall".to_owned(),
                ratio(expected, expected),
            );
            fixture_metrics.insert("provenance_coverage".to_owned(), ratio(evidence, actual));
            fixture_metrics.insert("recall_precision_at_6".to_owned(), ratio(hits, retrieved));
            fixture_metrics.insert("recall_recall_at_6".to_owned(), ratio(hits, relevant));
            fixture_metrics.insert(
                "compilation_input_bytes".to_owned(),
                Metric {
                    value: Some(
                        fixture_calls
                            .iter()
                            .map(|call| call.input_bytes)
                            .sum::<usize>() as f64,
                    ),
                    numerator: Some(fixture_calls.iter().map(|call| call.input_bytes).sum()),
                    denominator: Some(fixture_calls.len()),
                    unit: Some("bytes"),
                    reason: None,
                },
            );
            fixture_metrics.insert(
                "provider_calls".to_owned(),
                Metric {
                    value: Some(fixture_calls.len() as f64),
                    numerator: Some(fixture_calls.len()),
                    denominator: Some(1),
                    unit: Some("calls"),
                    reason: None,
                },
            );
            FixtureReport {
                id: fixture.id.clone(),
                categories: fixture.categories.clone(),
                metrics: fixture_metrics,
            }
        })
        .collect();
    Report {
        schema_version: 1,
        phase: 0,
        corpus: "phase0".to_owned(),
        fixture_count: corpus.fixtures.len(),
        metrics,
        fixtures,
    }
}

fn ratio(numerator: usize, denominator: usize) -> Metric {
    Metric {
        value: (denominator > 0).then_some(numerator as f64 / denominator as f64),
        numerator: Some(numerator),
        denominator: Some(denominator),
        unit: None,
        reason: None,
    }
}

fn not_meaningful(reason: &'static str) -> Metric {
    Metric {
        value: None,
        numerator: None,
        denominator: None,
        unit: None,
        reason: Some(reason),
    }
}
