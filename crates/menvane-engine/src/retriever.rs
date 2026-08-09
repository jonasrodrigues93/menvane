use std::collections::{HashMap, HashSet};

use anyhow::Result;
use menvane_domain::{Applicability, Project, ProjectTechnologies};
use menvane_store::{IndexStore, RecallContext, SearchResult, SearchScope};
use serde::Serialize;

use crate::DecayEngine;
use crate::sanitizer::MAX_RECALL_PROMPT_BYTES;

pub const RETRIEVAL_RRF_K: f64 = 60.0;
pub const CURRENT_PROMPT_WEIGHT: f64 = 1.00;
pub const ACTIVE_EPISODE_GOAL_WEIGHT: f64 = 0.85;
pub const ACTIVE_CORRECTION_WEIGHT: f64 = 1.00;
pub const ACTIVE_CONSTRAINT_WEIGHT: f64 = 0.80;
pub const CONVERSATION_ROOT_GOAL_WEIGHT: f64 = 0.35;
pub const PROJECT_SCOPE_MULTIPLIER: f64 = 1.15;
pub const GLOBAL_SCOPE_MULTIPLIER: f64 = 1.00;
pub const UNIVERSAL_APPLICABILITY_MULTIPLIER: f64 = 1.00;
pub const MATCHED_APPLICABILITY_MULTIPLIER: f64 = 1.05;
pub const CONFIDENCE_FLOOR: f64 = 0.50;
pub const CONFIDENCE_RANGE: f64 = 0.50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalMode {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, Copy)]
pub enum RetrievalScope {
    Auto,
    Project,
    Global,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallQueryDiagnostic {
    pub source: String,
    pub query: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallSourceDiagnostic {
    pub source: String,
    pub rank: Option<usize>,
    pub contribution: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallResultDiagnostic {
    pub memory_id: String,
    pub sources: Vec<RecallSourceDiagnostic>,
    pub fused_rrf: f64,
    pub lifecycle_multiplier: f64,
    pub type_multiplier: f64,
    pub confidence_multiplier: f64,
    pub freshness_multiplier: f64,
    pub applicability_multiplier: f64,
    pub scope_multiplier: f64,
    pub final_score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallDiagnostics {
    pub rrf_k: f64,
    pub queries: Vec<RecallQueryDiagnostic>,
    pub results: Vec<RecallResultDiagnostic>,
}

#[derive(Debug, Clone)]
struct RecallQuery {
    source: String,
    query: String,
    weight: f64,
    enabled: bool,
}

pub struct Retriever<'a> {
    index: &'a IndexStore,
}

impl<'a> Retriever<'a> {
    pub fn new(index: &'a IndexStore) -> Self {
        Self { index }
    }

    pub fn retrieve(
        &self,
        query: &str,
        project: Option<&Project>,
        scope: RetrievalScope,
        mode: RetrievalMode,
        include_sessions: bool,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let search_scope = search_scope(project, scope);
        let candidate_limit = limit.saturating_mul(8).max(limit);
        let mut results =
            self.index
                .search(query, search_scope, candidate_limit, include_sessions, true)?;
        if results.is_empty() && mode == RetrievalMode::Automatic {
            results = self.index.search(
                query,
                search_scope,
                candidate_limit,
                include_sessions,
                false,
            )?;
        }
        results.retain(|memory| {
            memory.scope != "global"
                || eligible_global(
                    &memory.applicability,
                    project.map(|project| &project.technologies),
                    mode,
                    query,
                )
        });
        for result in &mut results {
            let rrf = 1.0 / (RETRIEVAL_RRF_K + result.fts_rank as f64);
            result.score = rrf
                * type_multiplier(&result.memory_type)
                * status_multiplier(&result.status)
                * DecayEngine::freshness(&result.memory_type, result.age_days);
        }
        results.sort_by(|left, right| right.score.total_cmp(&left.score));
        let mut seen = HashSet::new();
        results.retain(|memory| {
            seen.insert(format!(
                "{}:{}",
                memory.memory_type,
                memory.title.to_ascii_lowercase()
            ))
        });
        results.truncate(limit);
        Ok(results)
    }

    pub fn retrieve_intent(
        &self,
        current_prompt: &str,
        context: Option<&RecallContext>,
        project: Option<&Project>,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, RecallDiagnostics)> {
        let queries = intent_queries(current_prompt, context);
        let search_scope = search_scope(project, RetrievalScope::Auto);
        let candidate_limit = limit.saturating_mul(8).max(limit);
        let mut fused = HashMap::<uuid::Uuid, FusedResult>::new();
        for query in &queries {
            if !query.enabled {
                continue;
            }
            let mut results =
                self.index
                    .search(&query.query, search_scope, candidate_limit, false, true)?;
            if results.is_empty() {
                results =
                    self.index
                        .search(&query.query, search_scope, candidate_limit, false, false)?;
            }
            results.retain(|memory| {
                memory.scope != "global"
                    || eligible_global(
                        &memory.applicability,
                        project.map(|project| &project.technologies),
                        RetrievalMode::Automatic,
                        &query.query,
                    )
            });
            for result in results {
                let rank = result.fts_rank;
                let contribution = query.weight / (RETRIEVAL_RRF_K + rank as f64);
                let entry = fused.entry(result.id).or_insert_with(|| FusedResult {
                    result,
                    sources: Vec::new(),
                });
                entry
                    .sources
                    .push((query.clone(), Some(rank), contribution));
            }
        }

        let query_diagnostics = queries
            .iter()
            .map(|query| RecallQueryDiagnostic {
                source: query.source.clone(),
                query: query.query.clone(),
                weight: query.weight,
            })
            .collect::<Vec<_>>();
        let mut ranked = fused
            .into_values()
            .map(|mut candidate| {
                let fused_rrf = candidate
                    .sources
                    .iter()
                    .map(|(_, _, contribution)| contribution)
                    .sum::<f64>();
                let result = &mut candidate.result;
                let lifecycle = status_multiplier(&result.status);
                let memory_type = type_multiplier(&result.memory_type);
                let confidence = confidence_multiplier(result.confidence);
                let freshness = DecayEngine::freshness(&result.memory_type, result.age_days);
                let applicability = applicability_multiplier(result, project);
                let scope = scope_multiplier(result, project);
                result.score = fused_rrf
                    * lifecycle
                    * memory_type
                    * confidence
                    * freshness
                    * applicability
                    * scope;
                let sources = queries
                    .iter()
                    .map(|query| {
                        candidate
                            .sources
                            .iter()
                            .find(|(source, _, _)| source.source == query.source)
                            .map_or(
                                RecallSourceDiagnostic {
                                    source: query.source.clone(),
                                    rank: None,
                                    contribution: 0.0,
                                },
                                |(_, rank, contribution)| RecallSourceDiagnostic {
                                    source: query.source.clone(),
                                    rank: *rank,
                                    contribution: *contribution,
                                },
                            )
                    })
                    .collect::<Vec<_>>();
                let diagnostics = RecallResultDiagnostic {
                    memory_id: result.id.to_string(),
                    sources,
                    fused_rrf,
                    lifecycle_multiplier: lifecycle,
                    type_multiplier: memory_type,
                    confidence_multiplier: confidence,
                    freshness_multiplier: freshness,
                    applicability_multiplier: applicability,
                    scope_multiplier: scope,
                    final_score: result.score,
                };
                (candidate.result, diagnostics)
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.0.score.total_cmp(&left.0.score));

        let mut seen = HashSet::new();
        let mut diagnostics = Vec::new();
        let mut results = Vec::new();
        for (result, diagnostic) in ranked {
            if !seen.insert(format!(
                "{}:{}",
                result.memory_type,
                result.title.to_ascii_lowercase()
            )) {
                continue;
            }
            diagnostics.push(diagnostic);
            results.push(result);
            if results.len() == limit {
                break;
            }
        }
        Ok((
            results,
            RecallDiagnostics {
                rrf_k: RETRIEVAL_RRF_K,
                queries: query_diagnostics,
                results: diagnostics,
            },
        ))
    }

    pub fn briefing(&self, project: Option<&Project>, limit: usize) -> Result<Vec<SearchResult>> {
        let scope = project
            .map(|project| SearchScope::Auto(project.id.as_str()))
            .unwrap_or(SearchScope::Global);
        let mut results = self.index.list(scope, limit.saturating_mul(8), false)?;
        results.retain(|memory| {
            memory.scope != "global"
                || eligible_global(
                    &memory.applicability,
                    project.map(|project| &project.technologies),
                    RetrievalMode::Automatic,
                    "",
                )
        });
        results.retain(|memory| match memory.memory_type.as_str() {
            "decision" | "gotcha" => true,
            "fact" => memory.scope == "global" && memory.confidence >= 0.8,
            _ => false,
        });
        for result in &mut results {
            result.score = result.confidence
                * type_multiplier(&result.memory_type)
                * status_multiplier(&result.status);
        }
        results.sort_by(|left, right| right.score.total_cmp(&left.score));
        results.truncate(limit);
        Ok(results)
    }
}

struct FusedResult {
    result: SearchResult,
    sources: Vec<(RecallQuery, Option<usize>, f64)>,
}

fn intent_queries(current_prompt: &str, context: Option<&RecallContext>) -> Vec<RecallQuery> {
    let mut queries = Vec::new();
    add_query(
        &mut queries,
        "current-prompt",
        current_prompt,
        CURRENT_PROMPT_WEIGHT,
    );
    if let Some(context) = context
        && let Some(episode) = &context.active_episode
    {
        add_query(
            &mut queries,
            "active-episode-goal",
            &episode.goal,
            ACTIVE_EPISODE_GOAL_WEIGHT,
        );
        for (index, correction) in context.active_corrections.iter().enumerate() {
            add_query(
                &mut queries,
                &format!("active-correction-{}", index + 1),
                correction,
                ACTIVE_CORRECTION_WEIGHT,
            );
        }
        for (index, constraint) in context.active_constraints.iter().enumerate() {
            add_query(
                &mut queries,
                &format!("active-constraint-{}", index + 1),
                constraint,
                ACTIVE_CONSTRAINT_WEIGHT,
            );
        }
        let root_goal = context
            .conversation_root_goal
            .as_deref()
            .unwrap_or_default();
        if !root_goal.trim().is_empty() {
            let root_goal = bounded_query(root_goal);
            queries.push(RecallQuery {
                source: "conversation-root-goal".to_owned(),
                query: root_goal.clone(),
                weight: CONVERSATION_ROOT_GOAL_WEIGHT,
                enabled: !root_goal.eq_ignore_ascii_case(&bounded_query(&episode.goal)),
            });
        }
    }
    queries
}

fn add_query(queries: &mut Vec<RecallQuery>, source: &str, query: &str, weight: f64) {
    if !query.trim().is_empty() {
        queries.push(RecallQuery {
            source: source.to_owned(),
            query: bounded_query(query),
            weight,
            enabled: true,
        });
    }
}

fn bounded_query(query: &str) -> String {
    let query = query.trim();
    if query.len() <= MAX_RECALL_PROMPT_BYTES {
        return query.to_owned();
    }
    let mut boundary = MAX_RECALL_PROMPT_BYTES;
    while !query.is_char_boundary(boundary) {
        boundary -= 1;
    }
    query[..boundary].to_owned()
}

fn search_scope(project: Option<&Project>, scope: RetrievalScope) -> SearchScope<'_> {
    match scope {
        RetrievalScope::Auto => project
            .map(|project| SearchScope::Auto(project.id.as_str()))
            .unwrap_or(SearchScope::Global),
        RetrievalScope::Project => project
            .map(|project| SearchScope::Project(project.id.as_str()))
            .unwrap_or(SearchScope::Global),
        RetrievalScope::Global => SearchScope::Global,
    }
}

fn eligible_global(
    applicability: &Applicability,
    technologies: Option<&ProjectTechnologies>,
    mode: RetrievalMode,
    query: &str,
) -> bool {
    if applicability.is_empty() {
        return true;
    }
    if mode == RetrievalMode::Explicit && explicitly_requests(query, applicability) {
        return true;
    }
    let Some(technologies) = technologies else {
        return false;
    };
    overlaps_or_unrestricted(&applicability.languages, &technologies.languages)
        && overlaps_or_unrestricted(&applicability.frameworks, &technologies.frameworks)
        && overlaps_or_unrestricted(&applicability.tools, &technologies.tools)
        && overlaps_or_unrestricted(&applicability.databases, &technologies.databases)
        && overlaps_or_unrestricted(&applicability.platforms, &technologies.platforms)
}

fn explicitly_requests(query: &str, applicability: &Applicability) -> bool {
    let query_tokens = query
        .split(|character: char| !character.is_alphanumeric() && character != '.')
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<HashSet<_>>();
    [
        &applicability.languages,
        &applicability.frameworks,
        &applicability.tools,
        &applicability.databases,
        &applicability.platforms,
    ]
    .into_iter()
    .flatten()
    .any(|value| query_tokens.contains(&value.to_ascii_lowercase()))
}

fn overlaps_or_unrestricted(required: &[String], actual: &[String]) -> bool {
    required.is_empty()
        || required
            .iter()
            .any(|required| actual.iter().any(|actual| actual == required))
}

fn type_multiplier(memory_type: &str) -> f64 {
    match memory_type {
        "procedure" | "decision" => 1.15,
        "gotcha" => 1.10,
        "session" => 0.75,
        _ => 1.00,
    }
}

fn status_multiplier(status: &str) -> f64 {
    match status {
        "candidate" => 0.85,
        "needs-validation" => 0.70,
        "superseded" => 0.25,
        "historical" => 0.20,
        _ => 1.00,
    }
}

fn confidence_multiplier(confidence: f64) -> f64 {
    CONFIDENCE_FLOOR + CONFIDENCE_RANGE * confidence.clamp(0.0, 1.0)
}

fn applicability_multiplier(result: &SearchResult, project: Option<&Project>) -> f64 {
    if result.scope == "global" && !result.applicability.is_empty() && project.is_some() {
        MATCHED_APPLICABILITY_MULTIPLIER
    } else {
        UNIVERSAL_APPLICABILITY_MULTIPLIER
    }
}

fn scope_multiplier(result: &SearchResult, project: Option<&Project>) -> f64 {
    if result.scope == "project" && project.is_some() {
        PROJECT_SCOPE_MULTIPLIER
    } else {
        GLOBAL_SCOPE_MULTIPLIER
    }
}
