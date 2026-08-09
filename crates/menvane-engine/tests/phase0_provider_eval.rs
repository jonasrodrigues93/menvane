use std::env;
use std::fs;
use std::time::Instant;

use menvane_engine::MemoryCompiler;
use serde::Serialize;

#[path = "common/phase0.rs"]
#[allow(dead_code)]
mod phase0;
use phase0::{compilation_input, load_corpus};

#[derive(Serialize)]
struct ProviderReport {
    schema_version: u32,
    phase: u32,
    provider: String,
    model: String,
    fixtures: Vec<FixtureResult>,
}

#[derive(Serialize)]
struct FixtureResult {
    id: String,
    latency_ms: u128,
    memories: usize,
    error: Option<String>,
}

#[tokio::test]
#[ignore = "requires an explicitly configured provider and is not part of normal CI"]
async fn provider_evaluation_runner() {
    let home = env::var("MENVANE_PHASE0_HOME").expect("MENVANE_PHASE0_HOME is required");
    let output = env::var("MENVANE_PHASE0_REPORT")
        .unwrap_or_else(|_| "phase0-provider-report.json".to_owned());
    let menvane = menvane_engine::Menvane::new(home).unwrap();
    let provider = menvane.configured_provider().unwrap();
    let corpus = load_corpus();
    let mut fixtures = Vec::new();
    for fixture in &corpus.fixtures {
        let started = Instant::now();
        let result = MemoryCompiler::new(provider.clone())
            .compile(compilation_input(fixture))
            .await;
        fixtures.push(FixtureResult {
            id: fixture.id.clone(),
            latency_ms: started.elapsed().as_millis(),
            memories: result.as_ref().map_or(0, |value| value.memories.len()),
            error: result.err().map(|error| error.to_string()),
        });
    }
    let report = ProviderReport {
        schema_version: 1,
        phase: 0,
        provider: provider.name().to_owned(),
        model: provider.model().to_owned(),
        fixtures,
    };
    fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
