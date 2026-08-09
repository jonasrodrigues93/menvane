use std::collections::HashSet;

use anyhow::Result;
use menvane_domain::{Applicability, Project, ProjectTechnologies};
use menvane_store::{IndexStore, SearchResult, SearchScope};

use crate::DecayEngine;

const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalMode {
    Automatic,
    Explicit,
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
        let search_scope = match scope {
            RetrievalScope::Auto => project
                .map(|project| SearchScope::Auto(project.id.as_str()))
                .unwrap_or(SearchScope::Global),
            RetrievalScope::Project => project
                .map(|project| SearchScope::Project(project.id.as_str()))
                .unwrap_or(SearchScope::Global),
            RetrievalScope::Global => SearchScope::Global,
        };
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
            let rrf = 1.0 / (RRF_K + result.fts_rank as f64);
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

#[derive(Debug, Clone, Copy)]
pub enum RetrievalScope {
    Auto,
    Project,
    Global,
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
