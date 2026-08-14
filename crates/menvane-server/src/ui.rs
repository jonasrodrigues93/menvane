use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use menvane_domain::{
    HandoffItem, KnowledgeType, Memory, NormalizedEvent, Project, ProviderHealth,
};
use menvane_engine::{Menvane, ScopeSelection};
use serde::Deserialize;
use uuid::Uuid;

pub fn router() -> Router<Arc<Menvane>> {
    Router::new()
        .route("/", get(dashboard))
        .route("/projects", get(projects))
        .route("/projects/{id}", get(project_detail))
        .route("/memories", get(memories))
        .route("/memories/{id}", get(memory_detail))
        .route("/sessions", get(sessions))
        .route("/sessions/{id}", get(session_detail))
        .route("/handoffs/{project_id}", get(handoff_detail))
        .route("/imports", get(imports))
        .route("/integrations", get(integrations))
        .route("/providers", get(providers))
        .route("/settings", get(settings))
        .route("/assets/menvane.css", get(styles))
}

async fn dashboard(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = async {
        let projects = menvane.all_projects()?;
        let memories = knowledge_memories(menvane.all_memories()?);
        let jobs = menvane.jobs()?;
        let provider = menvane.provider_health().await.ok();
        let ready = provider.is_some_and(|(_, _, health)| health == ProviderHealth::Ready);
        Ok(format!(
            "{}<section class='panel'><p>{} projects, {} context/playbook memories, {} pending jobs.</p><p>Provider: {}</p></section>",
            page_head("Overview", "Operational continuity and durable knowledge."),
            projects.len(),
            memories.len(),
            jobs.iter().filter(|job| job.status == "pending").count(),
            if ready { "ready" } else { "attention required" }
        ))
    }
    .await;
    page_result(&menvane, "overview", "Overview", content)
}

async fn projects(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.all_projects().map(|projects| {
        let memories = menvane.all_memories().unwrap_or_default();
        let rows = projects
            .iter()
            .map(|project| project_row(project, &memories))
            .collect::<String>();
        format!(
            "{}<section class='panel'><table><tr><th>Project</th><th>Technologies</th><th>Memory</th></tr>{}</table></section>",
            page_head("Projects", "Stable project identities."),
            if rows.is_empty() { empty_state("No projects yet.") } else { rows }
        )
    });
    page_result(&menvane, "projects", "Projects", content)
}

async fn project_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<String>) -> Response {
    let content = menvane.all_projects().and_then(|projects| {
        let project = projects
            .into_iter()
            .find(|project| project.id == id)
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let memories = knowledge_memories(menvane.all_memories()?)
            .into_iter()
            .filter(|memory| memory.metadata.project_id.as_deref() == Some(project.id.as_str()))
            .collect::<Vec<_>>();
        let handoff = menvane.current_project_handoff(Some(&project.id))?;
        Ok(format!(
            "{}<section class='panel'><dl><dt>Identity</dt><dd>{}</dd><dt>Known paths</dt><dd>{}</dd><dt>Technologies</dt><dd>{}</dd></dl></section>{}<section class='panel'><h2>Memories</h2>{}</section>",
            page_head(&project.name, "Project identity and current work fronts."),
            escape(&project.identity),
            escape(&project.known_paths.join(" · ")),
            escape(&technologies(&project)),
            handoff_sections(handoff.as_ref()),
            memory_list(&memories, &project_names(std::slice::from_ref(&project)))
        ))
    });
    page_result(&menvane, "projects", "Project", content)
}

#[derive(Default, Deserialize)]
struct MemoryFilters {
    q: Option<String>,
    scope: Option<String>,
    r#type: Option<String>,
    status: Option<String>,
}

async fn memories(
    State(menvane): State<Arc<Menvane>>,
    Query(filters): Query<MemoryFilters>,
) -> Response {
    let content = (|| -> anyhow::Result<String> {
        let all = knowledge_memories(menvane.all_memories()?);
        let names = project_names(&menvane.all_projects()?);
        let query_results = filters
            .q
            .as_deref()
            .filter(|query| !query.trim().is_empty())
            .map(|query| {
                menvane.search_without_recording(
                    &std::env::current_dir().unwrap_or_default(),
                    query,
                    ScopeSelection::Auto,
                    all.len().max(20),
                )
            })
            .transpose()?
            .unwrap_or_default();
        let ids = query_results
            .iter()
            .map(|result| result.id)
            .collect::<std::collections::HashSet<_>>();
        let filtered = all
            .iter()
            .filter(|memory| {
                filters.q.as_deref().is_none_or(|query| {
                    query.trim().is_empty() || ids.contains(&memory.metadata.id)
                })
            })
            .filter(|memory| {
                filters.scope.as_deref().is_none_or(|scope| {
                    scope.is_empty() || memory.metadata.scope.to_string() == scope
                })
            })
            .filter(|memory| {
                filters.r#type.as_deref().is_none_or(|kind| {
                    kind.is_empty() || memory.metadata.knowledge_type.to_string() == kind
                })
            })
            .filter(|memory| {
                filters.status.as_deref().is_none_or(|status| {
                    status.is_empty() || memory.metadata.status.to_string() == status
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(format!(
            "{}<form class='filters'><input name='q' placeholder='Search' value='{}'><select name='type'><option value=''>All types</option><option value='context'>Context</option><option value='playbook'>Playbook</option></select><select name='scope'><option value=''>All scopes</option><option value='project'>Project</option><option value='global'>Global</option></select><button>Apply</button></form><section class='panel'><h2>Context and playbooks</h2>{}</section>",
            page_head("Memories", "Durable context and playbooks only."),
            escape_attribute(filters.q.as_deref().unwrap_or_default()),
            memory_list(&filtered, &names)
        ))
    })();
    page_result(&menvane, "memories", "Memories", content)
}

async fn memory_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    let content = menvane.read_without_recording(id).map(|memory| {
        format!(
            "{}<section class='panel'><article class='rendered'>{}</article><dl><dt>Type</dt><dd>{}</dd><dt>Scope</dt><dd>{}</dd><dt>Status</dt><dd>{}</dd><dt>Tags</dt><dd>{}</dd><dt>Applies to</dt><dd>{}</dd><dt>Sources</dt><dd>{}</dd></dl></section>",
            page_head(&memory.title, "Durable memory detail."),
            render_markdown(&memory.body),
            memory.metadata.knowledge_type,
            memory.metadata.scope,
            memory.metadata.status,
            escape(&memory.metadata.tags.join(", ")),
            escape(&serde_json::to_string(&memory.metadata.applies_to).unwrap_or_default()),
            memory.metadata.source_sessions.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        )
    });
    page_result(&menvane, "memories", "Memory", content)
}

async fn sessions(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.sessions(100).map(|sessions| {
        let rows = sessions
            .iter()
            .map(|session| {
                format!(
                    "<a class='memory-row' href='/sessions/{}'><strong>{} · {}</strong><span>{:?} · summary {:?} · {}</span></a>",
                    session.id,
                    escape(&session.client),
                    escape(&session.external_session_id),
                    session.state,
                    session.summary_status,
                    session.last_event_at.format("%Y-%m-%d %H:%M:%S"),
                )
            })
            .collect::<String>();
        format!(
            "{}<section class='panel'><h2>Sessions</h2>{}</section>",
            page_head("Sessions", "Captured sessions and episodic summaries."),
            if rows.is_empty() {
                empty_state("No sessions recorded.")
            } else {
                rows
            }
        )
    });
    page_result(&menvane, "sessions", "Sessions", content)
}

async fn session_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    let content = menvane.session(id).and_then(|session| {
        let session = session.ok_or_else(|| anyhow::anyhow!("session not found"))?;
        let summary = menvane.session_summary(id)?;
        let consolidation = menvane.session_consolidation(id)?;
        let events = menvane.session_events(id)?;
        let evidence = events.iter().map(session_evidence_row).collect::<String>();
        let evidence_section = format!(
            "<section class='panel'><h2>Session evidence</h2>{}</section>",
            if evidence.is_empty() {
                empty_state("No events recorded.")
            } else {
                evidence
            }
        );
        Ok(format!(
            "{}<section class='panel'><dl><dt>Client</dt><dd>{}</dd><dt>External session</dt><dd>{}</dd><dt>State</dt><dd>{:?}</dd><dt>Summary</dt><dd>{:?}</dd><dt>Last event</dt><dd>{}</dd></dl></section>{}{}{}",
            page_head("Session", "Episodic summary and chronological evidence."),
            escape(&session.client),
            escape(&session.external_session_id),
            session.state,
            session.summary_status,
            session.last_event_at.format("%Y-%m-%d %H:%M:%S"),
            summary_section(summary.as_ref()),
            consolidation_section(consolidation.as_ref()),
            evidence_section
        ))
    });
    page_result(&menvane, "sessions", "Session", content)
}

fn summary_section(summary: Option<&menvane_domain::EpisodicSummary>) -> String {
    let Some(summary) = summary else {
        return format!(
            "<section class='panel'><h2>Episodic summary</h2>{}</section>",
            empty_state("No episodic summary.")
        );
    };
    let items = |values: &[String]| {
        values
            .iter()
            .map(|value| format!("<li>{}</li>", escape(value)))
            .collect::<String>()
    };
    let continuity = summary
        .continuity
        .iter()
        .map(|item| format!("<li>{:?}: {}</li>", item.disposition, escape(&item.front)))
        .collect::<String>();
    format!(
        "<section class='panel'><h2>Episodic summary</h2><dl><dt>Outcome</dt><dd>{:?}</dd><dt>Result</dt><dd>{}</dd></dl><h3>Intentions</h3><ul>{}</ul><h3>Actions</h3><ul>{}</ul><h3>Continuity</h3><ul>{}</ul><h3>Candidate learnings</h3><ul>{}</ul></section>",
        summary.outcome,
        escape(&summary.result),
        items(&summary.intentions),
        items(&summary.actions),
        continuity,
        items(&summary.candidate_learnings),
    )
}

fn consolidation_section(consolidation: Option<&menvane_engine::ConsolidationMarker>) -> String {
    let Some(marker) = consolidation else {
        return String::new();
    };
    let execution = &marker.execution;
    format!(
        "<section class='panel'><h2>Consolidation</h2><dl><dt>Provider</dt><dd>{}</dd><dt>Model</dt><dd>{}</dd><dt>Latency</dt><dd>{} ms</dd><dt>Attempts</dt><dd>{}</dd><dt>Tokens</dt><dd>{}</dd><dt>Credits</dt><dd>{}</dd></dl></section>",
        escape(&execution.provider),
        escape(&execution.model),
        execution.latency_ms,
        execution.attempts,
        match (execution.input_tokens, execution.output_tokens) {
            (Some(input), Some(output)) => format!("{input} in / {output} out"),
            _ => "not reported".to_owned(),
        },
        execution
            .credits
            .map_or_else(|| "not reported".to_owned(), |credits| credits.to_string()),
    )
}

async fn handoff_detail(
    State(menvane): State<Arc<Menvane>>,
    Path(project_id): Path<String>,
) -> Response {
    let content = menvane
        .current_project_handoff(Some(&project_id))
        .map(|handoff| {
            handoff.map_or_else(
                || {
                    format!(
                        "{}<section class='panel'>{}</section>",
                        page_head("Handoff", "Current live work fronts."),
                        empty_state("No current handoff items.")
                    )
                },
                |handoff| {
                    format!(
                        "{}{}",
                        page_head("Handoff", "Current live work fronts."),
                        handoff_items(&handoff.items)
                    )
                },
            )
        });
    page_result(&menvane, "projects", "Handoff", content)
}

async fn imports(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.orphans().map(|orphans| {
        format!(
            "{}<section class='panel'><p>{} unresolved imported sessions.</p></section>",
            page_head(
                "Imports",
                "External evidence remains operational session data."
            ),
            orphans.len()
        )
    });
    page_result(&menvane, "imports", "Imports", content)
}

async fn integrations(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.integrations().map(|states| {
        let rows = states
            .iter()
            .map(|state| {
                format!(
                    "<p>{}: {}</p>",
                    escape(&state.client),
                    if state.connected {
                        "connected"
                    } else {
                        "disconnected"
                    }
                )
            })
            .collect::<String>();
        format!(
            "{}<section class='panel'>{}</section>",
            page_head("Connections", "Agent integrations."),
            rows
        )
    });
    page_result(&menvane, "integrations", "Connections", content)
}

async fn providers(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = async {
        let (provider, model, health) = menvane.provider_health().await?;
        Ok::<_, anyhow::Error>(format!(
            "{}<section class='panel'><p>{} / {} / {:?}</p></section>",
            page_head("Providers", "Inference health."),
            escape(&provider),
            escape(&model),
            health
        ))
    }
    .await;
    page_result(&menvane, "providers", "Providers", content)
}

async fn settings(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.configuration_text().map(|configuration| {
        format!(
            "{}<section class='panel'><pre>{}</pre></section>",
            page_head("Settings", "Current non-secret runtime configuration."),
            escape(&configuration)
        )
    });
    page_result(&menvane, "settings", "Settings", content)
}

fn knowledge_memories(memories: Vec<Memory>) -> Vec<Memory> {
    memories
        .into_iter()
        .filter(|memory| {
            matches!(
                memory.metadata.knowledge_type,
                KnowledgeType::Context | KnowledgeType::Playbook
            )
        })
        .collect()
}

fn project_names(projects: &[Project]) -> HashMap<String, String> {
    projects
        .iter()
        .map(|project| (project.id.clone(), project.name.clone()))
        .collect()
}

fn project_row(project: &Project, memories: &[Memory]) -> String {
    let count = memories
        .iter()
        .filter(|memory| memory.metadata.project_id.as_deref() == Some(project.id.as_str()))
        .count();
    format!(
        "<tr><td><a href='/projects/{}'>{}</a><small>{}</small></td><td>{}</td><td>{count}</td></tr>",
        project.id,
        escape(&project.name),
        escape(&project.identity),
        escape(&technologies(project))
    )
}

fn technologies(project: &Project) -> String {
    [
        project.technologies.languages.as_slice(),
        project.technologies.frameworks.as_slice(),
        project.technologies.tools.as_slice(),
    ]
    .concat()
    .join(" · ")
}

fn memory_list(memories: &[Memory], names: &HashMap<String, String>) -> String {
    if memories.is_empty() {
        return empty_state("No memories match these filters.");
    }
    memories.iter().map(|memory| {
        let origin = memory.metadata.project_id.as_ref().and_then(|id| names.get(id)).map(String::as_str).unwrap_or("Global");
        format!("<a class='memory-row' href='/memories/{}'><strong>{}</strong><span>{} · {} · {}</span><p>{}</p></a>", memory.metadata.id, escape(&memory.title), memory.metadata.knowledge_type, memory.metadata.status, escape(origin), escape(&memory_summary(memory)))
    }).collect()
}

fn memory_summary(memory: &Memory) -> String {
    truncate_text(
        &memory
            .body
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .collect::<Vec<_>>()
            .join(" "),
        180,
    )
}

fn handoff_sections(handoff: Option<&menvane_engine::CurrentHandoff>) -> String {
    format!(
        "<section class='handoff-surface'><h2>Current handoff</h2>{}</section>",
        handoff.map_or_else(
            || empty_state("No current handoff items."),
            |handoff| handoff_items(&handoff.items)
        )
    )
}

fn handoff_items(items: &[HandoffItem]) -> String {
    if items.is_empty() {
        return empty_state("No current handoff items.");
    }
    items
        .iter()
        .map(|item| {
            let provenance = item
                .sources
                .iter()
                .map(|source| {
                    format!(
                        "session {} ({} events)",
                        source.session_id,
                        source.event_ids.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "<article class='handoff-item'><strong>{:?}{}</strong><p>{}</p><small>{}</small><small>{}</small><small>Sources: {} · confirmed {}</small></article>",
                item.kind,
                if item.low_confidence { " · low confidence" } else { "" },
                escape(&item.state),
                escape(
                    &item
                        .next_step
                        .as_deref()
                        .map(|step| format!("Next: {step}"))
                        .unwrap_or_else(|| "No next step recorded.".to_owned())
                ),
                escape(
                    &item
                        .blocker
                        .as_deref()
                        .map(|blocker| format!("Blocked by: {blocker}"))
                        .unwrap_or_default()
                ),
                escape(&provenance),
                item.last_confirmed_at.format("%Y-%m-%d"),
            )
        })
        .collect()
}

fn session_evidence_row(event: &NormalizedEvent) -> String {
    format!(
        "<article class='evidence-row'><strong>{}</strong><span>{}</span><p>{}</p></article>",
        event_kind(event),
        event.timestamp.format("%Y-%m-%d %H:%M:%S"),
        escape(
            event
                .bounded_input
                .as_deref()
                .or(event.bounded_output.as_deref())
                .unwrap_or("No bounded payload")
        )
    )
}

fn event_kind(event: &NormalizedEvent) -> &'static str {
    match event.kind {
        menvane_domain::NormalizedEventKind::SessionStarted => "Session started",
        menvane_domain::NormalizedEventKind::UserPrompt => "User prompt",
        menvane_domain::NormalizedEventKind::ToolCompleted => "Tool completed",
        menvane_domain::NormalizedEventKind::ContextCompacted => "Context compacted",
        menvane_domain::NormalizedEventKind::TurnStopped => "Turn stopped",
        menvane_domain::NormalizedEventKind::SessionEnded => "Session ended",
    }
}

fn page_result(
    menvane: &Menvane,
    active: &str,
    title: &str,
    content: anyhow::Result<String>,
) -> Response {
    match content {
        Ok(content) => page(menvane, active, title, content),
        Err(error) => error_page(menvane, error),
    }
}

fn page(menvane: &Menvane, active: &str, title: &str, content: String) -> Response {
    let nav = [
        ("overview", "Overview", "/"),
        ("projects", "Projects", "/projects"),
        ("memories", "Memories", "/memories"),
        ("sessions", "Sessions", "/sessions"),
        ("imports", "Imports", "/imports"),
        ("integrations", "Connections", "/integrations"),
        ("providers", "Providers", "/providers"),
        ("settings", "Settings", "/settings"),
    ]
    .into_iter()
    .map(|(key, label, href)| {
        format!(
            "<a{} href='{href}'>{label}</a>",
            if active == key { " class='active'" } else { "" }
        )
    })
    .collect::<String>();
    Html(format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width, initial-scale=1'><title>Menvane - {}</title><link rel='stylesheet' href='/assets/menvane.css'></head><body><aside><a href='/'><strong>MENVANE</strong></a><nav>{nav}</nav><small>{}</small></aside><main><header>Menvane / {}</header>{content}</main></body></html>", escape(title), escape(&menvane.home().display().to_string()), escape(title))).into_response()
}

fn error_page(menvane: &Menvane, error: impl std::fmt::Display) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        page(
            menvane,
            "",
            "Error",
            format!(
                "{}<section class='panel'><pre>{}</pre></section>",
                page_head("Error", "The request could not be completed."),
                escape(&error.to_string())
            ),
        ),
    )
        .into_response()
}

fn page_head(title: &str, subtitle: &str) -> String {
    format!(
        "<section class='page-head'><h1>{}</h1><p>{}</p></section>",
        escape(title),
        escape(subtitle)
    )
}
fn empty_state(message: &str) -> String {
    format!("<div class='empty-state'>{}</div>", escape(message))
}
fn truncate_text(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        format!(
            "{}...",
            value.chars().take(limit).collect::<String>().trim_end()
        )
    }
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn escape_attribute(value: &str) -> String {
    escape(value)
}
fn render_markdown(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            if let Some(value) = line.strip_prefix("## ") {
                format!("<h2>{}</h2>", escape(value))
            } else {
                format!("<p>{}</p>", escape(line))
            }
        })
        .collect()
}

async fn styles() -> impl IntoResponse {
    (
        [("content-type", "text/css; charset=utf-8")],
        "body{font-family:system-ui,sans-serif;margin:0;color:#20252b;background:#f4f1ea}aside{background:#18242b;color:#fff;min-height:100vh;padding:2rem;position:fixed;width:12rem}aside a{color:inherit;text-decoration:none}nav{display:grid;gap:.7rem;margin:2rem 0}nav a{color:#b9c7c8}.active{color:#fff;font-weight:700}main{margin-left:16rem;padding:2rem;max-width:70rem}.page-head{margin-bottom:1.5rem}.panel{background:#fff;border:1px solid #d9d4ca;border-radius:.5rem;padding:1.25rem;margin:1rem 0}.page-head h1{margin-bottom:.25rem}.page-head p,p{color:#59636a}.filters{display:flex;gap:.5rem;margin:1rem 0}.filters input,.filters select,.filters button{padding:.6rem}table{border-collapse:collapse;width:100%}td,th{padding:.8rem;text-align:left;border-bottom:1px solid #ddd}td small{display:block;color:#68737a}.memory-row,.handoff-item,.evidence-row{display:block;padding:1rem 0;border-bottom:1px solid #ddd;color:inherit;text-decoration:none}.memory-row strong{font-size:1.05rem}.memory-row span,.memory-row p{display:block}.empty-state{padding:1.5rem;color:#68737a}dl{display:grid;grid-template-columns:10rem 1fr;gap:.6rem}dt{font-weight:700}.rendered{line-height:1.6}@media(max-width:700px){aside{position:static;width:auto;min-height:0}nav{display:flex;flex-wrap:wrap;margin:1rem 0}main{margin:0;padding:1rem}.filters{flex-wrap:wrap}dl{grid-template-columns:1fr}}",
    )
}
