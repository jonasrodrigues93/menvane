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
        .route("/assets/menvane.js", get(script))
}

async fn dashboard(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = async {
        let projects = menvane.all_projects()?;
        let memories = knowledge_memories(menvane.all_memories()?);
        let sessions = menvane.sessions(100)?;
        let jobs = menvane.jobs()?;
        let provider = menvane.provider_health().await.ok();
        let ready = provider.is_some_and(|(_, _, health)| health == ProviderHealth::Ready);
        let context_count = memories
            .iter()
            .filter(|memory| memory.metadata.knowledge_type == KnowledgeType::Context)
            .count();
        let playbook_count = memories.len() - context_count;
        let pending = jobs.iter().filter(|job| job.status == "pending").count();
        let names = project_names(&projects);
        let recent_memories = memories.iter().take(5).cloned().collect::<Vec<_>>();
        let recent_sessions = sessions
            .iter()
            .take(4)
            .map(session_row)
            .collect::<String>();
        Ok(format!(
            "{}<section class='metrics'>{}{}{}{}{}{}</section><div class='dashboard-grid'><section class='panel'><header class='panel-head'><div><h2>Durable knowledge</h2><p>Recent context and playbooks</p></div><a class='panel-link' href='/memories'>All knowledge →</a></header><div class='memory-list'>{}</div></section><aside class='right-stack'><section class='panel'><header class='panel-head'><div><h2>Recent sessions</h2><p>Chronological evidence</p></div><a class='panel-link' href='/sessions'>All sessions →</a></header><div class='session-list'>{}</div></section><section class='panel'><header class='panel-head'><div><h2>System</h2><p>Local runtime</p></div></header><div class='system-list'><div class='system-row'><span>Provider</span><strong class='{}'>{}</strong></div><div class='system-row'><span>Queue</span><strong>{} pending</strong></div><div class='system-row'><span>Storage</span><strong>Markdown + SQLite</strong></div></div></section></aside></div><div class='section-title'><div><h2>Projects</h2><p>Stable identities and live work fronts</p></div><a href='/projects'>All projects →</a></div><section class='panel'><table class='project-table'><thead><tr><th>Project</th><th>Technologies</th><th>Knowledge</th></tr></thead><tbody>{}</tbody></table></section>",
            page_head("Overview", "Operational continuity, not a backlog."),
            metric("01", "Context", context_count, "durable records"),
            metric("02", "Playbooks", playbook_count, "reusable procedures"),
            metric("03", "Sessions", sessions.len(), "captured journeys"),
            metric("04", "Projects", projects.len(), "known identities"),
            metric("05", "Queue", pending, "pending jobs"),
            metric("06", "Provider", usize::from(ready), if ready { "ready" } else { "attention" }),
            memory_list(&recent_memories, &names),
            if recent_sessions.is_empty() { empty_state("No sessions captured yet.") } else { recent_sessions },
            if ready { "ready" } else { "attention" },
            if ready { "Ready" } else { "Attention" },
            pending,
            project_rows(&projects, &memories),
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
            "{}<section class='panel'><table class='project-table'><thead><tr><th>Project</th><th>Technologies</th><th>Knowledge</th></tr></thead><tbody>{}</tbody></table></section>",
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
            "{}<section class='panel metadata-panel'><dl class='metadata'><dt>Identity</dt><dd>{}</dd><dt>Known paths</dt><dd>{}</dd><dt>Technologies</dt><dd>{}</dd></dl></section>{}<div class='section-title'><div><h2>Knowledge</h2><p>Context and playbooks scoped to this project</p></div></div><section class='panel'><div class='memory-list'>{}</div></section>",
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
            "{}<form class='filters'><label class='search-field'><span>⌕</span><input name='q' placeholder='Search title or content' value='{}'></label><select name='type'><option value=''>All types</option><option value='context'>Context</option><option value='playbook'>Playbook</option></select><select name='scope'><option value=''>All scopes</option><option value='project'>Project</option><option value='global'>Global</option></select><button>Apply</button></form><section class='panel'><header class='panel-head'><div><h2>Context and playbooks</h2><p>{} records match this view</p></div></header><div class='memory-list'>{}</div></section>",
            page_head("Memories", "Durable context and playbooks only."),
            escape_attribute(filters.q.as_deref().unwrap_or_default()),
            filtered.len(),
            memory_list(&filtered, &names)
        ))
    })();
    page_result(&menvane, "memories", "Memories", content)
}

async fn memory_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    let content = menvane.read_without_recording(id).map(|memory| {
        format!(
            "{}<section class='panel detail-grid'><article class='rendered'>{}</article><aside class='detail-side'><div class='stamp'>{} / {}</div><dl class='metadata'><dt>Type</dt><dd>{}</dd><dt>Scope</dt><dd>{}</dd><dt>Status</dt><dd>{}</dd><dt>Tags</dt><dd>{}</dd><dt>Applies to</dt><dd>{}</dd><dt>Sources</dt><dd>{}</dd></dl></aside></section>",
            page_head(&memory.title, "Durable memory detail."),
            render_markdown(&memory.body),
            memory.metadata.knowledge_type,
            memory.metadata.status,
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
        let rows = sessions.iter().map(session_row).collect::<String>();
        format!(
            "{}<section class='panel'><header class='panel-head'><div><h2>Captured sessions</h2><p>{} chronological journeys</p></div></header><div class='session-list'>{}</div></section>",
            page_head("Sessions", "Captured sessions and episodic summaries."),
            sessions.len(),
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
            "<section class='panel'><header class='panel-head'><div><h2>Session evidence</h2><p>Chronological, sanitized capture</p></div></header><div class='evidence-list'>{}</div></section>",
            if evidence.is_empty() {
                empty_state("No events recorded.")
            } else {
                evidence
            }
        );
        Ok(format!(
            "{}<section class='panel session-overview'><div><span class='eyebrow'>Session identity</span><h2>{} / {}</h2><p>Last event {}</p></div><div class='status-stack'><span class='status-badge'>{:?}</span><span class='status-badge subtle'>Summary {:?}</span></div></section><div class='session-detail-grid'><div>{}{}</div><aside>{}</aside></div>",
            page_head("Session", "Episodic summary and chronological evidence."),
            escape(&session.client),
            escape(&session.external_session_id),
            session.last_event_at.format("%Y-%m-%d %H:%M:%S"),
            session.state,
            session.summary_status,
            summary_section(summary.as_ref()),
            evidence_section,
            consolidation_section(consolidation.as_ref()),
        ))
    });
    page_result(&menvane, "sessions", "Session", content)
}

fn summary_section(summary: Option<&menvane_domain::EpisodicSummary>) -> String {
    let Some(summary) = summary else {
        return format!(
            "<section class='panel'><header class='panel-head'><div><h2>Episodic summary</h2><p>Derived from this session</p></div></header>{}</section>",
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
        "<section class='panel summary-panel'><header class='panel-head'><div><h2>Episodic summary</h2><p>Outcome <strong>{:?}</strong></p></div></header><div class='summary-result'>{}</div><div class='summary-grid'><section><h3>Intentions</h3><ul>{}</ul></section><section><h3>Actions</h3><ul>{}</ul></section><section><h3>Continuity</h3><ul>{}</ul></section><section><h3>Candidate learnings</h3><ul>{}</ul></section></div></section>",
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
        "<section class='panel diagnostic-panel'><header class='panel-head'><div><h2>Consolidation</h2><p>Execution diagnostics</p></div></header><dl class='metadata'><dt>Provider</dt><dd>{}</dd><dt>Model</dt><dd>{}</dd><dt>Latency</dt><dd>{} ms</dd><dt>Attempts</dt><dd>{}</dd><dt>Tokens</dt><dd>{}</dd><dt>Credits</dt><dd>{}</dd></dl></section>",
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
        "<tr><td class='project-name'><strong><a href='/projects/{}'>{}</a></strong><small>{}</small></td><td class='tech'>{}</td><td class='number'>{count:02}</td></tr>",
        project.id,
        escape(&project.name),
        escape(&project.identity),
        escape(&technologies(project))
    )
}

fn project_rows(projects: &[Project], memories: &[Memory]) -> String {
    if projects.is_empty() {
        "<tr><td colspan='3' class='table-empty'>No projects yet. Start in a Git repository to establish identity.</td></tr>".to_owned()
    } else {
        projects
            .iter()
            .take(6)
            .map(|project| project_row(project, memories))
            .collect()
    }
}

fn metric(index: &str, label: &str, value: usize, detail: &str) -> String {
    format!(
        "<article class='metric'><span class='metric-label'><b>{index}</b>{}</span><strong>{value:02}</strong><small>{}</small></article>",
        escape(label),
        escape(detail)
    )
}

fn session_row(session: &menvane_engine::SessionRecord) -> String {
    format!(
        "<a class='session-row' href='/sessions/{}'><time>{}</time><div><strong>{} / {}</strong><p>Summary {:?}</p></div><span class='session-state'>{:?}</span></a>",
        session.id,
        session.last_event_at.format("%m-%d %H:%M"),
        escape(&session.client),
        escape(&session.external_session_id),
        session.summary_status,
        session.state,
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
    memories
        .iter()
        .map(|memory| {
            let origin = memory
                .metadata
                .project_id
                .as_ref()
                .and_then(|id| names.get(id))
                .map(String::as_str)
                .unwrap_or("Global");
            let kind = memory.metadata.knowledge_type.to_string();
            let abbreviation = if kind == "playbook" { "PB" } else { "CX" };
            format!(
                "<a class='memory-row' data-kind='{kind}' href='/memories/{}'><span class='type'>{abbreviation}</span><div class='memory-copy'><h3>{}</h3><p>{}</p><div class='memory-meta'><span>{}</span><span class='status'>{}</span><span>{}</span></div></div><div class='memory-tail'><span class='scope-tag'>{}</span><span>{}</span></div></a>",
                memory.metadata.id,
                escape(&memory.title),
                escape(&memory_summary(memory)),
                memory.metadata.knowledge_type,
                memory.metadata.status,
                escape(origin),
                memory.metadata.scope,
                memory.metadata.updated_at.format("%Y-%m-%d"),
            )
        })
        .collect()
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
        "<section class='handoff-surface'><div class='section-title'><div><h2>Current handoff</h2><p>Only live work fronts</p></div></div>{}</section>",
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
    let cards = items
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
                "<article class='handoff-card' data-kind='{}'><div class='handoff-card-top'><span class='handoff-status'>{:?}{}</span><time>{}</time></div><h3>{}</h3><dl class='handoff-facts'><dt>Next</dt><dd>{}</dd><dt>Blocker</dt><dd>{}</dd><dt>Sources</dt><dd>{}</dd></dl></article>",
                handoff_kind(item.kind),
                item.kind,
                if item.low_confidence { " · low confidence" } else { "" },
                item.last_confirmed_at.format("%Y-%m-%d"),
                escape(&item.state),
                escape(
                    item
                        .next_step
                        .as_deref()
                        .unwrap_or("Not recorded")
                ),
                escape(
                    item
                        .blocker
                        .as_deref()
                        .unwrap_or("None")
                ),
                escape(&provenance),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!("<div class='handoff-grid'>{cards}</div>")
}

fn handoff_kind(kind: menvane_domain::HandoffItemKind) -> &'static str {
    match kind {
        menvane_domain::HandoffItemKind::InProgress => "in-progress",
        menvane_domain::HandoffItemKind::OpenQuestion => "open-question",
        menvane_domain::HandoffItemKind::Parked => "parked",
        menvane_domain::HandoffItemKind::Blocked => "blocked",
    }
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
    let project_count = menvane.all_projects().map_or(0, |items| items.len());
    let memory_count = menvane.all_memories().map_or(0, |items| items.len());
    let session_count = menvane.sessions(100).map_or(0, |items| items.len());
    let nav_item = |key: &str, number: &str, label: &str, href: &str, count: Option<usize>| {
        format!(
            "<a{} href='{href}'><span class='nav-icon'>{number}</span><span>{label}</span>{}</a>",
            if active == key {
                " class='active' aria-current='page'"
            } else {
                ""
            },
            count
                .map(|value| format!("<span class='nav-count'>{value:02}</span>"))
                .unwrap_or_default()
        )
    };
    Html(format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width, initial-scale=1'><title>Menvane — {}</title><link rel='stylesheet' href='/assets/menvane.css'><script defer src='/assets/menvane.js'></script></head><body><div class='app'><aside class='sidebar' id='sidebar'><a class='brand' href='/'><span class='brand-mark'></span><span class='brand-copy'><strong>MENVANE</strong><small>LOCAL MEMORY</small></span></a><div class='nav-label'>Workspace</div><nav class='nav'>{}{}{}{}</nav><div class='nav-label'>System</div><nav class='nav'>{}{}{}{}</nav><div class='sidebar-foot'><div class='daemon'><i></i>Local runtime</div><div class='storage'>{}</div></div></aside><main class='main'><header class='topbar'><button class='mobile-menu' id='mobile-menu' type='button' aria-label='Toggle navigation'>≡</button><div class='breadcrumb'>Menvane / <strong>{}</strong></div><a class='command-trigger' href='/memories'><span>⌕</span>Search durable knowledge<kbd>/</kbd></a><div class='local-label'>Local only</div></header><div class='workspace'>{content}</div></main></div></body></html>",
        escape(title),
        nav_item("overview", "01", "Overview", "/", None),
        nav_item("projects", "02", "Projects", "/projects", Some(project_count)),
        nav_item("memories", "03", "Knowledge", "/memories", Some(memory_count)),
        nav_item("sessions", "04", "Sessions", "/sessions", Some(session_count)),
        nav_item("imports", "05", "Imports", "/imports", None),
        nav_item("integrations", "06", "Connections", "/integrations", None),
        nav_item("providers", "07", "Providers", "/providers", None),
        nav_item("settings", "08", "Settings", "/settings", None),
        escape(&menvane.home().display().to_string()),
        escape(title),
    )).into_response()
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
        "<section class='page-head'><div><h1>{}</h1><p>{}</p></div></section>",
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
    ([("content-type", "text/css; charset=utf-8")], CSS)
}

async fn script() -> impl IntoResponse {
    ([("content-type", "text/javascript; charset=utf-8")], JS)
}

const JS: &str = r"const menu=document.querySelector('#mobile-menu');const sidebar=document.querySelector('#sidebar');menu?.addEventListener('click',()=>sidebar.classList.toggle('open'));document.addEventListener('keydown',event=>{if(event.key==='Escape')sidebar?.classList.remove('open');if(event.key==='/'&&document.activeElement?.tagName!=='INPUT'){event.preventDefault();window.location='/memories'}});";

const CSS: &str = r#"
:root{color-scheme:light;--canvas:#efeee8;--surface:#faf9f5;--raised:#fff;--muted-surface:#e7e6df;--ink:#1d1e1b;--text:#3e403a;--muted:#777970;--quiet:#a3a59b;--line:#d0d1c9;--strong:#a9aba1;--accent:#315cf4;--accent-soft:#e7ebff;--signal:#b9e936;--signal-soft:#eff8d4;--warn:#d88614;--warn-soft:#fff0d9;--danger:#d8523f;--rail:224px;--mono:"IBM Plex Mono","SFMono-Regular",Consolas,monospace;--sans:"Aptos","Segoe UI",sans-serif}
*{box-sizing:border-box}html{background:var(--canvas)}body{min-height:100vh;margin:0;background:var(--canvas);color:var(--ink);font:14px var(--sans)}button,input,select{font:inherit}a{color:inherit}:focus-visible{outline:3px solid rgba(49,92,244,.35);outline-offset:2px}.app{display:grid;grid-template-columns:var(--rail) minmax(0,1fr);min-height:100vh}.sidebar{position:fixed;inset:0 auto 0 0;z-index:30;width:var(--rail);height:100vh;display:flex;flex-direction:column;border-right:1px solid var(--strong);background:#e5e4dd}.brand{height:68px;display:flex;align-items:center;gap:11px;padding:0 17px;border-bottom:1px solid var(--strong);text-decoration:none}.brand-mark{position:relative;width:30px;height:30px;flex:none;border:1px solid var(--ink);background:var(--signal)}.brand-mark:before,.brand-mark:after{content:"";position:absolute;background:var(--ink)}.brand-mark:before{width:14px;height:1px;left:7px;top:14px}.brand-mark:after{width:1px;height:14px;left:14px;top:7px}.brand-copy strong,.brand-copy small{display:block}.brand-copy strong{font:800 13px var(--mono);letter-spacing:.1em}.brand-copy small{margin-top:4px;color:var(--muted);font:8px var(--mono);letter-spacing:.08em}.nav-label{padding:20px 17px 7px;color:var(--quiet);font:8px var(--mono);letter-spacing:.12em;text-transform:uppercase}.nav{display:grid;gap:2px;padding:0 9px}.nav a{min-height:38px;display:grid;grid-template-columns:22px 1fr auto;align-items:center;gap:8px;padding:0 9px;border:1px solid transparent;color:var(--text);text-decoration:none;font-size:12px}.nav a:hover{border-color:var(--strong);background:rgba(255,255,255,.45)}.nav a.active{border-color:var(--ink);background:var(--raised);box-shadow:3px 3px 0 var(--ink);color:var(--ink)}.nav-icon,.nav-count{color:var(--quiet);font:8px var(--mono)}.nav a.active .nav-icon{color:var(--accent)}.sidebar-foot{margin-top:auto;padding:14px 17px 17px;border-top:1px solid var(--strong)}.daemon{display:flex;align-items:center;gap:8px;color:var(--text);font:8px var(--mono);text-transform:uppercase}.daemon i,.local-label:before{width:7px;height:7px;background:var(--signal);border:1px solid #769b0a;content:""}.storage{overflow:hidden;margin-top:9px;color:var(--muted);font:8px/1.5 var(--mono);text-overflow:ellipsis;white-space:nowrap}.main{grid-column:2;min-width:0}.topbar{position:sticky;top:0;z-index:25;height:52px;display:flex;align-items:center;gap:16px;padding:0 24px;border-bottom:1px solid var(--strong);background:rgba(239,238,232,.94);backdrop-filter:blur(14px)}.mobile-menu{display:none}.breadcrumb{color:var(--muted);font:8px var(--mono);letter-spacing:.04em;text-transform:uppercase}.breadcrumb strong{color:var(--ink)}.command-trigger{width:min(420px,45vw);height:32px;display:flex;align-items:center;gap:9px;margin-left:auto;padding:0 10px;border:1px solid var(--strong);background:var(--surface);color:var(--muted);text-decoration:none;font:8px var(--mono)}.command-trigger:hover{border-color:var(--ink);background:var(--raised)}.command-trigger kbd{margin-left:auto;padding:2px 4px;border:1px solid var(--line);background:var(--canvas);font:7px var(--mono)}.local-label{display:flex;align-items:center;gap:7px;color:var(--muted);font:8px var(--mono);white-space:nowrap}.workspace{max-width:1480px;margin:0 auto;padding:28px 30px 50px}.page-head{display:flex;justify-content:space-between;gap:24px;margin-bottom:24px}.page-head h1{margin:0;font-size:30px;line-height:1;letter-spacing:-.035em}.page-head p{margin:8px 0 0;color:var(--muted);font-size:12px}.metrics{display:grid;grid-template-columns:repeat(6,minmax(0,1fr));margin-bottom:18px;border:1px solid var(--strong);background:var(--surface)}.metric{min-width:0;padding:14px 15px;border-right:1px solid var(--line)}.metric:last-child{border:0}.metric-label{display:flex;gap:7px;color:var(--muted);font:8px var(--mono);text-transform:uppercase}.metric-label b{color:var(--quiet);font-weight:400}.metric strong{display:block;margin-top:10px;font:600 24px/1 var(--mono);letter-spacing:-.06em}.metric small{display:block;overflow:hidden;margin-top:7px;color:var(--quiet);font:8px var(--mono);text-overflow:ellipsis;white-space:nowrap}.dashboard-grid{display:grid;grid-template-columns:minmax(0,1.45fr) minmax(300px,.55fr);gap:18px;align-items:start}.panel{border:1px solid var(--strong);background:var(--surface)}.panel-head{min-height:48px;display:flex;align-items:center;gap:10px;padding:9px 14px;border-bottom:1px solid var(--line)}.panel-head h2{margin:0;font-size:13px}.panel-head p{margin:3px 0 0;color:var(--muted);font:8px var(--mono)}.panel-link{margin-left:auto;color:var(--accent);font:8px var(--mono);text-decoration:none}.right-stack{display:grid;gap:18px}.memory-list{display:grid}.memory-row{display:grid;grid-template-columns:36px minmax(0,1fr) auto;gap:12px;min-height:86px;align-items:start;padding:13px 14px;border-bottom:1px solid var(--line);text-decoration:none}.memory-row:last-child{border:0}.memory-row:hover{background:var(--accent-soft)}.type{width:30px;height:30px;display:grid;place-items:center;border:1px solid var(--strong);background:var(--raised);font:9px var(--mono)}.memory-row[data-kind=playbook] .type{border-color:#88a91e;background:var(--signal-soft)}.memory-copy h3{margin:0 0 5px;font-size:12px}.memory-copy p{overflow:hidden;margin:0;color:var(--muted);font:8px/1.5 var(--mono);text-overflow:ellipsis;white-space:nowrap}.memory-meta{display:flex;flex-wrap:wrap;gap:9px;margin-top:8px;color:var(--quiet);font:7px var(--mono);text-transform:uppercase}.memory-tail{display:grid;justify-items:end;gap:8px;color:var(--quiet);font:7px var(--mono);text-transform:uppercase}.scope-tag,.status-badge{padding:3px 5px;border:1px solid var(--line);background:var(--raised);font:8px var(--mono);text-transform:uppercase}.session-list{padding:3px 14px 8px}.session-row{display:grid;grid-template-columns:50px 1fr auto;gap:10px;padding:11px 0;border-bottom:1px solid var(--line);text-decoration:none}.session-row:last-child{border:0}.session-row:hover strong{color:var(--accent)}.session-row time,.session-state{color:var(--quiet);font:7px var(--mono);text-transform:uppercase}.session-row strong{display:block;font-size:10px}.session-row p{margin:4px 0 0;color:var(--muted);font:7px var(--mono)}.system-list{padding:5px 14px 10px}.system-row{display:grid;grid-template-columns:1fr auto;gap:12px;align-items:center;min-height:42px;border-bottom:1px solid var(--line)}.system-row:last-child{border:0}.system-row span{font-size:10px}.system-row strong{font:8px var(--mono);text-transform:uppercase}.system-row strong.ready{color:#66810d}.system-row strong.attention{color:var(--warn)}.section-title{display:flex;align-items:baseline;justify-content:space-between;gap:10px;margin:24px 0 10px}.section-title h2{margin:0;font-size:16px}.section-title p{margin:4px 0 0;color:var(--muted);font:8px var(--mono)}.section-title a{color:var(--accent);font:8px var(--mono);text-decoration:none}.project-table{width:100%;border-collapse:collapse}.project-table th{height:34px;padding:0 13px;border-bottom:1px solid var(--line);color:var(--quiet);font:7px var(--mono);text-align:left;text-transform:uppercase}.project-table td{height:52px;padding:0 13px;border-bottom:1px solid var(--line);font-size:10px}.project-table tr:last-child td{border:0}.project-table tbody tr:hover{background:var(--accent-soft)}.project-name strong{display:block}.project-name a{text-decoration:none}.project-name small{display:block;max-width:250px;overflow:hidden;margin-top:4px;color:var(--quiet);font:7px var(--mono);text-overflow:ellipsis;white-space:nowrap}.tech{color:var(--muted);font:7px var(--mono)}.number{font:9px var(--mono);text-align:right}.table-empty,.empty-state{padding:18px!important;color:var(--muted);font:8px var(--mono)}.filters{display:flex;flex-wrap:wrap;gap:8px;margin-bottom:18px}.filters select,.filters input{height:32px;padding:0 9px;border:1px solid var(--strong);background:var(--surface);color:var(--text);font:8px var(--mono)}.search-field{display:flex;align-items:center;border:1px solid var(--strong);background:var(--surface)}.search-field span{padding-left:9px;color:var(--accent)}.search-field input{width:260px;border:0}.filters button{height:32px;padding:0 12px;border:1px solid var(--ink);background:var(--signal);box-shadow:2px 2px 0 var(--ink);cursor:pointer;font:8px var(--mono);text-transform:uppercase}.metadata-panel{margin-bottom:18px}.metadata{display:grid;grid-template-columns:auto 1fr;gap:8px 14px;margin:0;padding:16px}.metadata dt{color:var(--quiet);font:7px var(--mono);text-transform:uppercase}.metadata dd{margin:0;overflow-wrap:anywhere;color:var(--text);font:8px/1.5 var(--mono)}.detail-grid{display:grid;grid-template-columns:minmax(0,1.45fr) minmax(260px,.55fr)}.rendered{padding:18px;font-size:12px;line-height:1.65}.rendered h2{margin:16px 0 7px;font-size:15px}.rendered p{margin:0 0 8px}.detail-side{padding:18px;border-left:1px solid var(--line)}.stamp{margin-bottom:12px;color:var(--muted);font:8px var(--mono);text-transform:uppercase}.handoff-grid{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:10px}.handoff-card{min-width:0;padding:12px;border:1px solid var(--strong);background:var(--surface);box-shadow:3px 3px 0 var(--strong)}.handoff-card[data-kind=blocked]{background:var(--warn-soft)}.handoff-card-top{display:flex;justify-content:space-between;gap:8px;color:var(--quiet);font:7px var(--mono)}.handoff-status{color:var(--text);text-transform:uppercase}.handoff-card h3{margin:10px 0;font-size:11px;line-height:1.35}.handoff-facts{display:grid;grid-template-columns:55px 1fr;gap:6px 8px;margin:0}.handoff-facts dt{color:var(--quiet);font:7px var(--mono);text-transform:uppercase}.handoff-facts dd{overflow:hidden;margin:0;color:var(--text);font:7px/1.4 var(--mono);text-overflow:ellipsis;white-space:nowrap}.session-overview{display:flex;align-items:center;justify-content:space-between;gap:18px;margin-bottom:18px;padding:16px}.session-overview h2{margin:5px 0 0;font-size:16px}.session-overview p{margin:6px 0 0;color:var(--muted);font:8px var(--mono)}.eyebrow{color:var(--quiet);font:7px var(--mono);letter-spacing:.08em;text-transform:uppercase}.status-stack{display:flex;gap:6px}.status-badge.subtle{color:var(--muted)}.session-detail-grid{display:grid;grid-template-columns:minmax(0,1.25fr) minmax(300px,.75fr);gap:18px;align-items:start}.session-detail-grid>div{display:grid;gap:18px}.summary-result{padding:16px;border-bottom:1px solid var(--line);font-size:12px;line-height:1.55}.summary-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr))}.summary-grid section{padding:14px;border-right:1px solid var(--line);border-bottom:1px solid var(--line)}.summary-grid section:nth-child(2n){border-right:0}.summary-grid section:nth-last-child(-n+2){border-bottom:0}.summary-grid h3{margin:0 0 8px;color:var(--quiet);font:7px var(--mono);text-transform:uppercase}.summary-grid ul{margin:0;padding-left:16px;color:var(--text);font:9px/1.55 var(--mono)}.diagnostic-panel .metadata{padding:14px}.evidence-list{display:grid}.evidence-row{display:grid;grid-template-columns:120px minmax(0,1fr);gap:12px;padding:12px 14px;border-bottom:1px solid var(--line)}.evidence-row:last-child{border:0}.evidence-row strong{font-size:10px}.evidence-row span{display:block;margin-top:4px;color:var(--quiet);font:7px var(--mono)}.evidence-row p{grid-column:2;margin:0;color:var(--text);font:8px/1.5 var(--mono);overflow-wrap:anywhere}.callout{padding:16px}.callout pre{overflow:auto;font:8px/1.5 var(--mono)}
@media(max-width:1180px){.app{display:block}.sidebar{width:min(278px,86vw);transform:translateX(-105%);transition:transform .17s ease;box-shadow:18px 0 50px rgba(29,30,27,.22)}.sidebar.open{transform:translateX(0)}.main{grid-column:auto}.topbar{padding:0 13px}.mobile-menu{width:30px;height:30px;display:grid;place-items:center;border:1px solid var(--ink);background:var(--signal)}.breadcrumb{display:none}.command-trigger{width:auto;flex:1;margin:0}.local-label{font-size:0}.workspace{padding:22px 14px 38px}.dashboard-grid,.detail-grid,.session-detail-grid{grid-template-columns:1fr}.detail-side{border-left:0;border-top:1px solid var(--line)}.handoff-grid{grid-template-columns:repeat(2,minmax(0,1fr))}}
@media(max-width:760px){.metrics{grid-template-columns:repeat(2,1fr)}.metric{border-bottom:1px solid var(--line)}.metric:nth-child(2n){border-right:0}.page-head h1{font-size:25px}.memory-row{grid-template-columns:34px 1fr}.memory-tail{display:none}.memory-copy p{white-space:normal}.project-table th:nth-child(2),.project-table td:nth-child(2),.project-table th:nth-child(3),.project-table td:nth-child(3){display:none}.handoff-grid,.summary-grid{grid-template-columns:1fr}.summary-grid section{border-right:0}.session-overview{align-items:flex-start;flex-direction:column}.evidence-row{grid-template-columns:1fr}.evidence-row p{grid-column:1}.filters{display:grid}.filters>*{width:100%}.search-field input{width:100%}}
@media(prefers-reduced-motion:reduce){*,*:before,*:after{animation-duration:.01ms!important;transition-duration:.01ms!important}}
@media(prefers-color-scheme:dark){:root{color-scheme:dark;--canvas:#171a19;--surface:#202523;--raised:#282e2b;--muted-surface:#303834;--ink:#f2f4ed;--text:#d4d9d0;--muted:#a1aaa0;--quiet:#7f8b80;--line:#3d4740;--strong:#657166;--accent:#8ea8ff;--accent-soft:#29334f;--signal:#b9e936;--signal-soft:#34421b;--warn:#f0ad4e;--warn-soft:#49351d}.sidebar{background:#1d221f}.topbar{background:rgba(23,26,25,.94)}}
"#;
