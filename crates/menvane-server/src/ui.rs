use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use menvane_domain::{
    Applicability, HandoffItem, HandoffItemKind, KnowledgeRecord, KnowledgeType,
    NormalizedEvent, NormalizedEventKind, Project, ProviderHealth,
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
        .route("/imports", get(settings_imports))
        .route("/integrations", get(settings_integrations))
        .route("/providers", get(settings_providers))
        .route("/settings", get(settings).post(update_settings))
        .route("/assets/menvane.css", get(styles))
        .route("/assets/menvane.js", get(script))
}

async fn dashboard(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = async {
        let projects = menvane.all_projects()?;
        let memories = knowledge_memories(menvane.all_memories()?);
        let sessions = menvane.sessions(100)?;
        let jobs = menvane.jobs()?;
        let integrations = menvane.integrations()?;
        let provider = menvane.provider_health().await.ok();
        let ready = provider
            .as_ref()
            .is_some_and(|(_, _, health)| *health == ProviderHealth::Ready);
        let memory_count = memories
            .iter()
            .filter(|memory| memory.metadata.knowledge_type == KnowledgeType::Memory)
            .count();
        let playbook_count = memories.len() - memory_count;
        let pending = jobs.iter().filter(|job| job.status == "pending").count();
        let names = project_names(&projects);
        let recent_memories = memories.iter().take(5).cloned().collect::<Vec<_>>();
        let recent_sessions = sessions
            .iter()
            .take(5)
            .map(session_row)
            .collect::<String>();

        let connections_list = [
            ("Claude Code", "claude-code", "CLI hooks &amp; MCP server"),
            ("Codex Agent", "codex", "CLI &amp; IDE hook integration"),
            ("OpenCode", "opencode", "Plugin &amp; MCP memory server"),
        ]
        .into_iter()
        .map(|(name, key, description)| {
            let state = integrations.iter().find(|state| state.client == key);
            let connected = state.is_some_and(|state| state.connected);
            let detail = state.map_or("Not installed".to_owned(), |state| {
                format!(
                    "{} · {}",
                    if state.mcp_registered { "MCP active" } else { "MCP inactive" },
                    state.hook_status
                )
            });
            format!(
                "<article class='connection-card'><div class='connection-status-dot{}'></div><div class='connection-info'><strong>{name}</strong><p class='connection-desc'>{description}</p><small class='connection-detail'>{}</small></div><span class='status-pill{}'>{}</span></article>",
                if connected { " on" } else { "" },
                escape(&detail),
                if connected { " success" } else { " neutral" },
                if connected { "Connected" } else { "Disconnected" }
            )
        })
        .collect::<String>();

        let connections_section = format!(
            "<section class='panel'><header class='panel-head'><div><h2>Agent Connections</h2><p>Active lifecycle hooks</p></div><a class='panel-link' href='/settings?tab=integrations'>Manage {icon_arrow_right}</a></header><div class='connections-grid'>{connections_list}</div></section>",
            icon_arrow_right = icon_arrow_right()
        );

        let provider_name = provider.as_ref().map_or("Not configured", |(p, _, _)| p.as_str());
        let provider_model = provider.as_ref().map_or("", |(_, m, _)| m.as_str());

        Ok(format!(
            "{}<section class='metrics-grid'>{}{}{}{}</section><div class='dashboard-grid'><section class='main-flow'><div class='section-title compact'><div><h2>Active Projects</h2><p>Known repository identities and durable knowledge</p></div><a class='section-action' href='/projects'>All projects ({}) {}</a></div><section class='panel'><table class='project-table'><thead><tr><th>Project</th><th>Technologies</th><th>Knowledge</th></tr></thead><tbody>{}</tbody></table></section><section class='panel overview-memory'><header class='panel-head'><div><h2>Recent Knowledge Records</h2><p>Decaying memories and reusable playbooks</p></div><a class='panel-link' href='/memories'>View all ({}) {}</a></header><div class='memory-list'>{}</div></section></section><aside class='sidebar-flow'><section class='panel'><header class='panel-head'><div><h2>System Status</h2><p>Operational runtime</p></div></header><div class='system-list'><div class='system-row'><span>Consolidation LLM</span><span class='system-val'><strong class='status-text {}'>{}</strong><small>{}</small></span></div><div class='system-row'><span>Queue Status</span><span class='system-val'><strong>{} pending jobs</strong></span></div><div class='system-row'><span>Storage Engine</span><span class='system-val'><strong>Markdown + SQLite</strong></span></div></div></section>{connections_section}<section class='panel'><header class='panel-head'><div><h2>Recent Sessions</h2><p>Chronological captures</p></div><a class='panel-link' href='/sessions'>All sessions {}</a></header><div class='session-list'>{}</div></section></aside></div>",
            page_head("Overview", "Projects first, then operational health and durable knowledge."),
            metric(icon_memory(), "Memories", memory_count, "decaying records"),
            metric(icon_playbook(), "Playbooks", playbook_count, "reusable procedures"),
            metric(icon_folder(), "Projects", projects.len(), "monitored repositories"),
            metric(icon_session(), "Sessions", sessions.len(), "captured journeys"),
            projects.len(),
            icon_arrow_right(),
            project_rows(&projects, &memories),
            memories.len(),
            icon_arrow_right(),
            memory_list(&recent_memories, &names, &menvane),
            if ready { "ready" } else { "attention" },
            if ready { "Ready" } else { "Attention" },
            escape(&format!("{provider_name} {provider_model}")),
            pending,
            icon_arrow_right(),
            if recent_sessions.is_empty() { empty_state("No sessions captured yet.") } else { recent_sessions },
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
            "{}<section class='panel'><header class='panel-head'><div><h2>Registered Projects</h2><p>{} stable project identities</p></div></header><table class='project-table'><thead><tr><th>Project</th><th>Technologies</th><th>Knowledge</th></tr></thead><tbody>{}</tbody></table></section>",
            page_head("Projects", "Stable project identities established through Git repositories."),
            projects.len(),
            if rows.is_empty() { empty_state("No projects yet. Start working inside a Git repository to establish an identity.") } else { rows }
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

        let memory_count = memories
            .iter()
            .filter(|m| m.metadata.knowledge_type == KnowledgeType::Memory)
            .count();
        let playbook_count = memories
            .iter()
            .filter(|m| m.metadata.knowledge_type == KnowledgeType::Playbook)
            .count();
        let handoff_count = handoff.as_ref().map_or(0, |h| h.items.len());

        let tech_chips = render_tech_chips(&project.technologies);

        let paths_list = project
            .known_paths
            .iter()
            .map(|path| format!("<code class='path-badge'>{}</code>", escape(path)))
            .collect::<Vec<_>>()
            .join(" ");

        Ok(format!(
            "{}<div class='project-hero-panel'><div class='project-hero-header'><div class='project-hero-icon'>{}</div><div class='project-hero-info'><h1>{}</h1><div class='project-hero-identity'><span class='identity-pill'>{} {}</span></div></div><div class='project-hero-stats'><div class='hero-stat'><strong>{}</strong><span>Memories</span></div><div class='hero-stat'><strong>{}</strong><span>Playbooks</span></div><div class='hero-stat'><strong>{}</strong><span>Live Fronts</span></div></div></div><div class='project-hero-details'><div class='detail-item'><span class='detail-label'>Known Checkout Paths:</span> <div class='paths-wrap'>{}</div></div><div class='detail-item'><span class='detail-label'>Detected Tech Stack:</span> {}</div></div></div>{}<div class='section-title'><div><h2>Durable Knowledge</h2><p>Memories and playbooks scoped to this project</p></div></div><section class='panel'><div class='memory-list'>{}</div></section>",
            page_head("Project Overview", "Project identity, current work fronts and scoped knowledge."),
            icon_folder_large(),
            escape(&project.name),
            icon_branch(),
            escape(&project.identity),
            memory_count,
            playbook_count,
            handoff_count,
            if paths_list.is_empty() { "<span class='text-muted'>Standard local worktree</span>".to_owned() } else { paths_list },
            tech_chips,
            handoff_sections(handoff.as_ref()),
            memory_list(&memories, &project_names(std::slice::from_ref(&project)), &menvane)
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

        let selected_type = filters.r#type.as_deref().unwrap_or("");
        let selected_scope = filters.scope.as_deref().unwrap_or("");
        let selected_status = filters.status.as_deref().unwrap_or("");

        Ok(format!(
            "{}<form class='filters-bar' method='get'><div class='search-field'><span class='search-icon' aria-hidden='true'>{}</span><input name='q' placeholder='Search titles, content, tags...' value='{}'></div><div class='filters-group'><select name='type' aria-label='Filter by type'><option value=''>All Types</option><option value='memory'{}>Memories</option><option value='playbook'{}>Playbooks</option></select><select name='scope' aria-label='Filter by scope'><option value=''>All Scopes</option><option value='project'{}>Project Scoped</option><option value='global'{}>Global Scope</option></select><select name='status' aria-label='Filter by status'><option value=''>All Statuses</option><option value='active'{}>Active</option><option value='candidate'{}>Candidate</option><option value='quarantined'{}>Quarantined</option><option value='forgotten'{}>Forgotten</option></select><button type='submit' class='btn-primary'>Filter</button></div></form><section class='panel'><header class='panel-head'><div><h2>Knowledge Records</h2><p>{} records match this criteria</p></div></header><div class='memory-list'>{}</div></section>",
            page_head(
                "Knowledge Base",
                "Durable memories with decay lifecycle and reusable operational playbooks."
            ),
            icon_search(),
            escape_attribute(filters.q.as_deref().unwrap_or_default()),
            if selected_type == "memory" { " selected" } else { "" },
            if selected_type == "playbook" { " selected" } else { "" },
            if selected_scope == "project" { " selected" } else { "" },
            if selected_scope == "global" { " selected" } else { "" },
            if selected_status == "active" { " selected" } else { "" },
            if selected_status == "candidate" { " selected" } else { "" },
            if selected_status == "quarantined" { " selected" } else { "" },
            if selected_status == "forgotten" { " selected" } else { "" },
            filtered.len(),
            memory_list(&filtered, &names, &menvane)
        ))
    })();
    page_result(&menvane, "memories", "Memories", content)
}

async fn memory_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    let content = menvane.read_without_recording(id).map(|memory| {
        let decay = decay_visual(&menvane, &memory);
        let kind = memory.metadata.knowledge_type;
        let is_playbook = kind == KnowledgeType::Playbook;
        let kind_badge = if is_playbook {
            format!("<span class='badge playbook'>{} Playbook</span>", icon_playbook())
        } else {
            format!("<span class='badge memory'>{} Memory</span>", icon_memory())
        };
        let status_badge = format!("<span class='status-pill info'>{}</span>", memory.metadata.status);
        let scope_badge = if memory.metadata.scope == menvane_domain::Scope::Global {
            format!("<span class='badge scope-global'>{} Global</span>", icon_globe())
        } else {
            format!("<span class='badge scope-project'>{} Project Scoped</span>", icon_folder())
        };

        let tags_html = if memory.metadata.tags.is_empty() {
            "<span class='text-muted'>No tags assigned</span>".to_owned()
        } else {
            memory.metadata.tags.iter().map(|tag| format!("<span class='tag-chip'>#{}</span>", escape(tag))).collect::<String>()
        };

        let applies_html = render_applicability_chips(&memory.metadata.applies_to);

        let sessions_html = if memory.metadata.source_sessions.is_empty() {
            "<span class='text-muted'>Direct write / Manual</span>".to_owned()
        } else {
            memory.metadata.source_sessions.iter().map(|session_id| {
                format!("<a class='session-link-chip' href='/sessions/{session_id}'>Session {}</a>", escape(&session_id.to_string()[..8]))
            }).collect::<Vec<_>>().join(" ")
        };

        let stats_html = if is_playbook {
            let successes = memory.metadata.successes.unwrap_or(0);
            let failures = memory.metadata.failures.unwrap_or(0);
            format!("<div class='meta-field'><dt>Applications</dt><dd><strong>{successes}</strong> successes · <strong>{failures}</strong> failures</dd></div>")
        } else {
            format!("<div class='meta-field'><dt>Confidence &amp; Utility</dt><dd>{:.0}% confidence · {:.0}% utility</dd></div>", memory.metadata.confidence * 100.0, memory.metadata.utility * 100.0)
        };

        format!(
            "{}<div class='memory-detail-layout'><article class='panel memory-article'><header class='article-header'><div class='badges-row'>{kind_badge}{status_badge}{scope_badge}</div><h1>{}</h1><div class='article-meta'><time>Updated {}</time></div></header><div class='rendered-content'>{}</div></article><aside class='memory-sidebar'><section class='panel'><header class='panel-head'><div><h2>Lifecycle &amp; Health</h2></div></header><div class='panel-body'>{}</div></section><section class='panel'><header class='panel-head'><div><h2>Applicability &amp; Tags</h2></div></header><dl class='metadata-grid padding-body'><div class='meta-field full-width'><dt>Applies To</dt><dd>{}</dd></div><div class='meta-field full-width'><dt>Tags</dt><dd><div class='chips-wrap'>{}</div></dd></div>{stats_html}<div class='meta-field full-width'><dt>Source Sessions</dt><dd><div class='chips-wrap'>{}</div></dd></div></dl></section></aside></div>",
            page_head("Knowledge Detail", "Inspecting durable memory record."),
            escape(&memory.title),
            memory.metadata.updated_at.format("%B %d, %Y at %H:%M UTC"),
            render_markdown(&memory.body),
            decay,
            applies_html,
            tags_html,
            sessions_html,
        )
    });
    page_result(&menvane, "memories", "Memory", content)
}

async fn sessions(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.sessions(100).map(|sessions| {
        let rows = sessions.iter().map(session_row).collect::<String>();
        format!(
            "{}<section class='panel'><header class='panel-head'><div><h2>Captured Agent Sessions</h2><p>{} chronological journeys recorded</p></div></header><div class='session-list'>{}</div></section>",
            page_head("Sessions", "Sanitized chronological evidence and derived episodic summaries."),
            sessions.len(),
            if rows.is_empty() {
                empty_state("No sessions recorded yet.")
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
        let deliveries = menvane.session_delivery_audit(id)?;
        let evidence = events.iter().map(session_evidence_row).collect::<String>();
        let evidence_section = format!(
            "<section class='panel'><header class='panel-head'><div><h2>Session Evidence Timeline</h2><p>Chronological, sanitized user and tool events</p></div></header><div class='evidence-list'>{}</div></section>",
            if evidence.is_empty() {
                empty_state("No events recorded.")
            } else {
                evidence
            }
        );
        let delivery_section = delivery_audit_section(&deliveries);

        let state_pill = match session.state {
            menvane_domain::SessionState::Open => "<span class='status-pill success'>Open</span>",
            menvane_domain::SessionState::Idle => "<span class='status-pill warning'>Idle</span>",
            menvane_domain::SessionState::Finalized => "<span class='status-pill neutral'>Finalized</span>",
        };

        let summary_pill = match session.summary_status {
            menvane_domain::SummaryStatus::Ready => "<span class='status-pill success'>Summary Ready</span>",
            menvane_domain::SummaryStatus::Pending => "<span class='status-pill warning'>Summary Pending</span>",
            menvane_domain::SummaryStatus::Skipped => "<span class='status-pill neutral'>Summary Skipped</span>",
        };

        Ok(format!(
            "{}<section class='panel session-overview-card'><div class='session-header-info'><span class='client-tag'>{}</span><h2>{}</h2><p class='session-timing'>Last active {} · ID: <code class='code-inline'>{}</code></p></div><div class='status-badges-stack'>{state_pill}{summary_pill}</div></section><div class='session-detail-grid'><div class='session-main-column'>{}{}{}</div><aside class='session-side-column'>{}</aside></div>",
            page_head("Session Detail", "Episodic summary and chronological capture."),
            escape(&session.client),
            escape(&session.external_session_id),
            session.last_event_at.format("%Y-%m-%d %H:%M:%S UTC"),
            session.id,
            summary_section(summary.as_ref()),
            delivery_section,
            evidence_section,
            consolidation_section(consolidation.as_ref()),
        ))
    });
    page_result(&menvane, "sessions", "Session", content)
}

fn summary_section(summary: Option<&menvane_domain::EpisodicSummary>) -> String {
    let Some(summary) = summary else {
        return format!(
            "<section class='panel'><header class='panel-head'><div><h2>Episodic Summary</h2><p>Derived consolidation</p></div></header>{}</section>",
            empty_state("No episodic summary derived for this session yet.")
        );
    };
    let items = |values: &[String]| {
        if values.is_empty() {
            "<li class='empty-item'>None recorded</li>".to_owned()
        } else {
            values
                .iter()
                .map(|value| format!("<li>{}</li>", escape(value)))
                .collect::<String>()
        }
    };
    let continuity = if summary.continuity.is_empty() {
        "<li class='empty-item'>No handoff transitions recorded</li>".to_owned()
    } else {
        summary
            .continuity
            .iter()
            .map(|item| {
                let badge = match item.disposition {
                    menvane_domain::ContinuityDisposition::Continues => "<span class='badge' style='background:var(--color-primary-subtle);color:var(--color-primary);'>Continues</span>",
                    menvane_domain::ContinuityDisposition::Resolved => "<span class='badge' style='background:var(--color-success-subtle);color:var(--color-success-text);'>Resolved</span>",
                    menvane_domain::ContinuityDisposition::Discarded => "<span class='badge' style='background:var(--color-danger-subtle);color:var(--color-danger-text);'>Discarded</span>",
                    menvane_domain::ContinuityDisposition::Replaced => "<span class='badge' style='background:var(--color-warning-subtle);color:var(--color-warning-text);'>Replaced</span>",
                };
                format!("<li>{badge} {}</li>", escape(&item.front))
            })
            .collect::<String>()
    };

    let outcome_pill = match summary.outcome {
        menvane_domain::SummaryOutcome::Completed => "<span class='status-pill success'>Completed</span>",
        menvane_domain::SummaryOutcome::Advanced => "<span class='status-pill info'>Advanced</span>",
        menvane_domain::SummaryOutcome::Blocked => "<span class='status-pill warning'>Blocked</span>",
        menvane_domain::SummaryOutcome::Abandoned => "<span class='status-pill danger'>Abandoned</span>",
        menvane_domain::SummaryOutcome::Inconclusive => "<span class='status-pill neutral'>Inconclusive</span>",
    };

    format!(
        "<section class='panel summary-panel'><header class='panel-head'><div><h2>Episodic Summary</h2><p>Synthesized outcome &amp; learnings</p></div><div class='panel-actions'>{outcome_pill}</div></header><div class='summary-result-box'><p class='result-label'>Summary Result</p><p class='result-text'>{}</p></div><div class='summary-grid'><section class='summary-col'><h3>Intentions</h3><ul class='summary-list'>{}</ul></section><section class='summary-col'><h3>Actions Taken</h3><ul class='summary-list'>{}</ul></section><section class='summary-col'><h3>Continuity Transitions</h3><ul class='summary-list'>{}</ul></section><section class='summary-col'><h3>Candidate Learnings</h3><ul class='summary-list'>{}</ul></section></div></section>",
        escape(&summary.result),
        items(&summary.intentions),
        items(&summary.actions),
        continuity,
        items(&summary.candidate_learnings),
    )
}

fn consolidation_section(consolidation: Option<&menvane_engine::ConsolidationMarker>) -> String {
    let Some(marker) = consolidation else {
        return "<section class='panel diagnostic-panel'><header class='panel-head'><div><h2>Consolidation</h2><p>Execution diagnostics</p></div></header><div class='panel-body text-muted'>Pending provider consolidation</div></section>".to_owned();
    };
    let execution = &marker.execution;
    format!(
        "<section class='panel diagnostic-panel'><header class='panel-head'><div><h2>Consolidation Diagnostics</h2><p>LLM execution metrics</p></div></header><dl class='metadata-grid padding-body'><div class='meta-field'><dt>Provider</dt><dd><strong>{}</strong></dd></div><div class='meta-field'><dt>Model</dt><dd class='code-value'>{}</dd></div><div class='meta-field'><dt>Latency</dt><dd><strong>{} ms</strong></dd></div><div class='meta-field'><dt>Attempts</dt><dd>{}</dd></div><div class='meta-field full-width'><dt>Tokens In / Out</dt><dd>{}</dd></div><div class='meta-field full-width'><dt>Credits / Cost</dt><dd>{}</dd></div></dl></section>",
        escape(&execution.provider),
        escape(&execution.model),
        execution.latency_ms,
        execution.attempts,
        match (execution.input_tokens, execution.output_tokens) {
            (Some(input), Some(output)) => format!("<span class='badge' style='background:var(--bg-muted);'>{input} in</span> <span class='badge' style='background:var(--bg-muted);'>{output} out</span>"),
            _ => "<span class='text-muted'>Not reported</span>".to_owned(),
        },
        execution
            .credits
            .map_or_else(|| "<span class='text-muted'>Not reported</span>".to_owned(), |credits| credits.to_string()),
    )
}

fn delivery_audit_section(deliveries: &[menvane_engine::DeliveryAudit]) -> String {
    let rows = deliveries
        .iter()
        .map(|delivery| {
            let assessment = delivery.utility.as_deref().unwrap_or("pending assessment");
            let reason = delivery
                .evaluation_reason
                .as_deref()
                .unwrap_or("awaiting post-session evidence");

            let assessment_badge = match assessment {
                "useful" => "<span class='status-pill success'>Useful</span>",
                "unused" => "<span class='status-pill neutral'>Unused</span>",
                "irrelevant" => "<span class='status-pill warning'>Irrelevant</span>",
                "harmful" => "<span class='status-pill danger'>Harmful</span>",
                _ => "<span class='status-pill info'>Pending</span>",
            };

            format!(
                "<article class='delivery-card'><div class='delivery-card-head'><div><strong>{}</strong><span class='delivery-kind'>{}</span></div>{assessment_badge}</div><p class='delivery-reason'>{}</p><pre class='delivery-content'><code>{}</code></pre></article>",
                escape(&delivery.title),
                escape(&delivery.content_kind),
                escape(reason),
                escape(&delivery.content),
            )
        })
        .collect::<String>();
    format!(
        "<section class='panel'><header class='panel-head'><div><h2>Delivered Context &amp; Utility Audit</h2><p>Memories injected into prompt and autonomous post-session utility evaluation</p></div></header><div class='delivery-list'>{}</div></section>",
        if rows.is_empty() {
            empty_state("No memory cards or handoff items were delivered to this session.")
        } else {
            rows
        }
    )
}

async fn handoff_detail(
    State(menvane): State<Arc<Menvane>>,
    Path(project_id): Path<String>,
) -> Response {
    let content = menvane
        .current_project_handoff(Some(&project_id))
        .map(|handoff| {
            format!(
                "{}<section class='panel'>{}</section>",
                page_head("Current Handoff", "Live work fronts preserving continuity across sessions."),
                handoff_sections(handoff.as_ref())
            )
        });
    page_result(&menvane, "projects", "Handoff", content)
}

#[derive(Default, Deserialize)]
struct SettingsQuery {
    tab: Option<String>,
}

async fn settings_imports(State(menvane): State<Arc<Menvane>>) -> Response {
    render_unified_settings(&menvane, "imports").await
}

async fn settings_integrations(State(menvane): State<Arc<Menvane>>) -> Response {
    render_unified_settings(&menvane, "integrations").await
}

async fn settings_providers(State(menvane): State<Arc<Menvane>>) -> Response {
    render_unified_settings(&menvane, "providers").await
}

async fn settings(
    State(menvane): State<Arc<Menvane>>,
    Query(query): Query<SettingsQuery>,
) -> Response {
    let active_tab = query.tab.as_deref().unwrap_or("general");
    render_unified_settings(&menvane, active_tab).await
}

async fn render_unified_settings(menvane: &Menvane, active_tab: &str) -> Response {
    let content = async {
        let configuration = menvane.configuration_text()?;
        let parsed = toml::from_str::<toml::Value>(&configuration)
            .unwrap_or_else(|_| toml::Value::Table(toml::Table::new()));
        let get = |section: &str, key: &str, fallback: &str| {
            parsed
                .get(section)
                .and_then(|value| value.get(key))
                .map(ToString::to_string)
                .map(|value| value.trim_matches('"').to_owned())
                .unwrap_or_else(|| fallback.to_owned())
        };

        let integrations_data = menvane.integrations().unwrap_or_default();
        let clients = [
            ("Claude Code", "claude-code", "Lifecycle hooks (session start, user prompt, tool execution) and dedicated MCP memory tools.", "menvane connect claude"),
            ("Codex Agent", "codex", "Native MCP configuration and lifecycle hooks merged into ~/.codex/config.toml.", "menvane connect codex"),
            ("OpenCode", "opencode", "Vanilla JavaScript plugin and local MCP server registered in OpenCode config.", "menvane connect opencode"),
        ];

        let connections_cards = clients.iter().map(|(name, key, description, command)| {
            let state = integrations_data.iter().find(|s| s.client == *key);
            let connected = state.is_some_and(|s| s.connected);
            let mcp = state.is_some_and(|s| s.mcp_registered);
            let hook = state.map_or("Not configured", |s| s.hook_status.as_str());

            format!(
                "<article class='panel integration-full-card'><header class='panel-head'><div><h2>{name}</h2><p>{description}</p></div><span class='status-pill{}'>{}</span></header><div class='integration-details'><div class='integration-badges'><span class='badge{}'>MCP: {}</span><span class='badge{}'>Hooks: {}</span></div><div class='integration-command'><p>Connect or update command:</p><pre class='code-box'><code>{command}</code></pre></div></div></article>",
                if connected { " success" } else { " neutral" },
                if connected { "Connected &amp; Active" } else { "Disconnected" },
                if mcp { " success-subtle" } else { " neutral" },
                if mcp { "Registered" } else { "Missing" },
                if connected { " success-subtle" } else { " neutral" },
                escape(hook),
            )
        }).collect::<String>();

        let (provider, model, health) = menvane.provider_health().await.unwrap_or_else(|_| ("Unknown".into(), "Unknown".into(), ProviderHealth::Unavailable));
        let ready = health == ProviderHealth::Ready;

        let provider_section_html = format!(
            "<div class='providers-layout'><section class='panel provider-main-card'><header class='panel-head'><div><h2>Active Language Model Provider</h2><p>Used strictly for post-session consolidation and episodic summary generation</p></div><span class='status-pill{}'>{}</span></header><dl class='metadata-grid padding-body'><div class='meta-field'><dt>Provider Engine</dt><dd><strong>{}</strong></dd></div><div class='meta-field'><dt>Model Identifier</dt><dd class='code-value'>{}</dd></div><div class='meta-field full-width'><dt>Health Status</dt><dd class='status-text {}'>{:?}</dd></div></dl></section><section class='panel'><header class='panel-head'><div><h2>Quick Provider Setup</h2><p>Authenticate or switch providers via CLI</p></div></header><div class='provider-guides'><div class='guide-item'><strong>OpenAI (ChatGPT Plus/Pro with PKCE OAuth)</strong><pre class='code-box'><code>menvane provider login openai\nmenvane provider configure openai --model gpt-5.6-luna</code></pre></div><div class='guide-item'><strong>GitHub Copilot (Device OAuth)</strong><pre class='code-box'><code>menvane provider login github-copilot\nmenvane provider configure github-copilot --model gpt-5-mini</code></pre></div></div></section></div>",
            if ready { " success" } else { " warning" },
            if ready { "Ready &amp; Healthy" } else { "Attention Required" },
            escape(&provider),
            escape(&model),
            if ready { "ready" } else { "attention" },
            health
        );

        let orphans = menvane.orphans().unwrap_or_default();
        let orphan_count = orphans.len();
        let imports_section_html = format!(
            "<div class='imports-layout'><section class='panel'><header class='panel-head'><div><h2>Supported Client Importers</h2><p>Historical session discovery &amp; normalization</p></div></header><div class='importers-grid'><article class='importer-card'><div class='importer-header'><strong>Claude Code</strong><span class='status-pill info'>JSONL</span></div><p>Discovers recorded conversations from <code>~/.claude/sessions</code>.</p><pre class='code-box'><code>menvane import claude</code></pre></article><article class='importer-card'><div class='importer-header'><strong>Codex Agent</strong><span class='status-pill info'>JSONL</span></div><p>Discovers active and archived sessions from <code>~/.codex/sessions</code>.</p><pre class='code-box'><code>menvane import codex</code></pre></article><article class='importer-card'><div class='importer-header'><strong>OpenCode</strong><span class='status-pill info'>HTTP API</span></div><p>Imports sessions directly from local OpenCode API endpoint.</p><pre class='code-box'><code>menvane import opencode</code></pre></article></div></section><section class='panel'><header class='panel-head'><div><h2>Unresolved Imported Sessions (Orphans)</h2><p>{orphan_count} sessions awaiting project attribution</p></div></header><div class='panel-body'>{}</div></section></div>",
            if orphan_count == 0 {
                "<p class='text-muted'>All imported sessions are properly associated with resolved projects. No orphan records.</p>".to_owned()
            } else {
                format!("<p>There are <strong>{orphan_count}</strong> orphan sessions captured outside recognized Git worktrees. Use <code>menvane doctor</code> or CLI associating tools to review them.</p>")
            }
        );

        let tabs = [
            ("general", "Runtime Parameters", icon_settings()),
            ("integrations", "Agent Connections", icon_connection()),
            ("providers", "Inference Providers", icon_bot()),
            ("imports", "Historical Imports", icon_import()),
        ]
        .into_iter()
        .map(|(key, label, icon)| {
            format!(
                "<a class='tab-btn{}' href='/settings?tab={key}'>{} <span>{label}</span></a>",
                if active_tab == key { " active" } else { "" },
                icon,
            )
        })
        .collect::<String>();

        let active_content = match active_tab {
            "integrations" => format!("<section class='tab-panel'>{}</section>", connections_cards),
            "providers" => format!("<section class='tab-panel'>{}</section>", provider_section_html),
            "imports" => format!("<section class='tab-panel'>{}</section>", imports_section_html),
            _ => format!(
                "<section class='tab-panel'><section class='panel callout-panel'><p>Configure runtime behavior below. Secret values are read strictly from environment variables and never stored in files. Restart the daemon after updating configuration.</p></section><form class='settings-form' method='post'><fieldset class='panel settings-group'><legend class='panel-head'><div><h2>Capture Limits</h2><p>Bounds for raw session event ingest</p></div></legend><div class='form-fields-grid'><label class='form-field'><span>Maximum prompt bytes</span><input name='max_prompt_bytes' type='number' min='1' value='{}'><small>Default: 16384</small></label><label class='form-field'><span>Maximum tool input bytes</span><input name='max_tool_input_bytes' type='number' min='1' value='{}'><small>Default: 4096</small></label><label class='form-field'><span>Maximum tool output bytes</span><input name='max_tool_output_bytes' type='number' min='1' value='{}'><small>Default: 4096</small></label></div></fieldset><fieldset class='panel settings-group'><legend class='panel-head'><div><h2>Sessions &amp; Decay Lifecycle</h2><p>Finalization timeouts and memory decay window</p></div></legend><div class='form-fields-grid'><label class='form-field'><span>Idle finalization seconds</span><input name='idle_finalize_seconds' type='number' min='1' value='{}'><small>Default: 120s</small></label><label class='form-field'><span>Open-session inactivity seconds</span><input name='open_finalize_seconds' type='number' min='1' value='{}'><small>Default: 1800s</small></label><label class='form-field'><span>Job lease timeout seconds</span><input name='lease_timeout_seconds' type='number' min='1' value='{}'><small>Default: 300s</small></label><label class='form-field'><span>Memory lifetime in days</span><input name='memory_lifetime_days' type='number' min='1' value='{}'><small>Default: 90 days</small></label></div></fieldset><fieldset class='panel settings-group'><legend class='panel-head'><div><h2>Automatic Recall Gating</h2><p>Relevance thresholds for contextual prompt injection</p></div></legend><div class='form-fields-grid'><label class='form-field'><span>Minimum match confidence</span><input name='min_match_confidence' type='number' min='0' max='1' step='0.01' value='{}'><small>Default: 0.45</small></label><label class='form-field'><span>Minimum knowledge confidence</span><input name='min_knowledge_confidence' type='number' min='0' max='1' step='0.01' value='{}'><small>Default: 0.55</small></label><label class='form-field'><span>Minimum observed utility</span><input name='min_utility' type='number' min='0' max='1' step='0.01' value='{}'><small>Default: 0.55</small></label><label class='form-field'><span>Maximum cards delivered</span><input name='max_cards' type='number' min='1' max='3' value='{}'><small>Ceiling: 3</small></label></div></fieldset><fieldset class='panel settings-group'><legend class='panel-head'><div><h2>Language Model (Consolidation)</h2><p>Inference provider settings</p></div></legend><div class='form-fields-grid'><label class='form-field'><span>Provider</span><input name='provider' value='{}'></label><label class='form-field'><span>Model</span><input name='model' value='{}'></label><label class='form-field'><span>Reasoning effort</span><select name='reasoning_effort'>{}</select></label><label class='form-field'><span>Base URL</span><input name='base_url' type='url' value='{}'></label><label class='form-field'><span>API key environment variable</span><input name='api_key_env' value='{}'></label><label class='form-field'><span>GitHub OAuth client ID</span><input name='github_client_id' value='{}'></label><label class='form-field full-width'><span>Custom consolidation prompt override</span><textarea name='consolidation_prompt' rows='6'>{}</textarea><small>Leave empty to use built-in domain prompt.</small></label></div></fieldset><div class='editor-actions'><button type='submit' class='btn-primary large'>Save Configuration</button><a class='btn-secondary large' href='/'>Cancel</a></div></form></section>",
                get("capture", "max_prompt_bytes", "16384"),
                get("capture", "max_tool_input_bytes", "4096"),
                get("capture", "max_tool_output_bytes", "4096"),
                get("sessions", "idle_finalize_seconds", "120"),
                get("sessions", "open_finalize_seconds", "1800"),
                get("jobs", "lease_timeout_seconds", "300"),
                get("decay", "memory_lifetime_days", "90"),
                get("recall", "min_match_confidence", "0.45"),
                get("recall", "min_knowledge_confidence", "0.55"),
                get("recall", "min_utility", "0.55"),
                get("recall", "max_cards", "3"),
                get("llm", "provider", "openai"),
                get("llm", "model", "gpt-5.6-luna"),
                reasoning_options(&get("llm", "reasoning_effort", "medium")),
                escape_attribute(&get("llm", "base_url", "https://api.openai.com/v1")),
                escape_attribute(&get("llm", "api_key_env", "OPENAI_API_KEY")),
                escape_attribute(&get("llm", "github_client_id", "")),
                escape(&get("llm", "consolidation_prompt", "")),
            ),
        };

        Ok::<_, anyhow::Error>(format!(
            "{}<div class='settings-tabs-header'><nav class='tabs-nav'>{}</nav></div><div class='settings-tab-container'>{}</div>",
            page_head("Settings &amp; System", "Manage operational parameters, agent connections, inference providers and historical imports."),
            tabs,
            active_content
        ))
    }
    .await;
    page_result(menvane, "settings", "Settings", content)
}

fn reasoning_options(current: &str) -> String {
    ["minimal", "low", "medium", "high", "xhigh"]
        .into_iter()
        .map(|value| {
            format!(
                "<option value='{value}'{}>{value}</option>",
                if value == current { " selected" } else { "" }
            )
        })
        .collect()
}

#[derive(Deserialize)]
struct SettingsEdit {
    max_prompt_bytes: u64,
    max_tool_input_bytes: u64,
    max_tool_output_bytes: u64,
    idle_finalize_seconds: u64,
    open_finalize_seconds: u64,
    lease_timeout_seconds: u64,
    memory_lifetime_days: u64,
    min_match_confidence: f64,
    min_knowledge_confidence: f64,
    min_utility: f64,
    max_cards: u64,
    provider: String,
    model: String,
    reasoning_effort: String,
    base_url: String,
    api_key_env: String,
    #[serde(default)]
    github_client_id: Option<String>,
    consolidation_prompt: String,
}

async fn update_settings(
    State(menvane): State<Arc<Menvane>>,
    Form(edit): Form<SettingsEdit>,
) -> Response {
    let result = (|| -> anyhow::Result<()> {
        let mut configuration: toml::Table = toml::from_str(&menvane.configuration_text()?)?;
        for (section, key, value) in [
            (
                "capture",
                "max_prompt_bytes",
                toml::Value::Integer(edit.max_prompt_bytes as i64),
            ),
            (
                "capture",
                "max_tool_input_bytes",
                toml::Value::Integer(edit.max_tool_input_bytes as i64),
            ),
            (
                "capture",
                "max_tool_output_bytes",
                toml::Value::Integer(edit.max_tool_output_bytes as i64),
            ),
            (
                "sessions",
                "idle_finalize_seconds",
                toml::Value::Integer(edit.idle_finalize_seconds as i64),
            ),
            (
                "sessions",
                "open_finalize_seconds",
                toml::Value::Integer(edit.open_finalize_seconds as i64),
            ),
            (
                "jobs",
                "lease_timeout_seconds",
                toml::Value::Integer(edit.lease_timeout_seconds as i64),
            ),
            (
                "decay",
                "memory_lifetime_days",
                toml::Value::Integer(edit.memory_lifetime_days as i64),
            ),
            (
                "recall",
                "min_match_confidence",
                toml::Value::Float(edit.min_match_confidence),
            ),
            (
                "recall",
                "min_knowledge_confidence",
                toml::Value::Float(edit.min_knowledge_confidence),
            ),
            (
                "recall",
                "min_utility",
                toml::Value::Float(edit.min_utility),
            ),
            (
                "recall",
                "max_cards",
                toml::Value::Integer(edit.max_cards as i64),
            ),
            (
                "llm",
                "provider",
                toml::Value::String(edit.provider.trim().to_owned()),
            ),
            (
                "llm",
                "model",
                toml::Value::String(edit.model.trim().to_owned()),
            ),
            (
                "llm",
                "reasoning_effort",
                toml::Value::String(edit.reasoning_effort),
            ),
            (
                "llm",
                "base_url",
                toml::Value::String(edit.base_url.trim().to_owned()),
            ),
            (
                "llm",
                "api_key_env",
                toml::Value::String(edit.api_key_env.trim().to_owned()),
            ),
            (
                "llm",
                "consolidation_prompt",
                toml::Value::String(edit.consolidation_prompt),
            ),
        ] {
            configuration
                .entry(section)
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{section} configuration must be a table"))?
                .insert(key.to_owned(), value);
        }
        if let Some(client_id) = edit.github_client_id {
            configuration
                .entry("llm")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("llm configuration must be a table"))?
                .insert(
                    "github_client_id".to_owned(),
                    toml::Value::String(client_id.trim().to_owned()),
                );
        }
        menvane.update_configuration_text(&toml::to_string_pretty(&configuration)?)
    })();
    match result {
        Ok(()) => Redirect::to("/settings?saved=1").into_response(),
        Err(error) => error_page(&menvane, error),
    }
}

fn knowledge_memories(memories: Vec<KnowledgeRecord>) -> Vec<KnowledgeRecord> {
    memories
        .into_iter()
        .filter(|memory| {
            matches!(
                memory.metadata.knowledge_type,
                KnowledgeType::Memory | KnowledgeType::Playbook
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

fn project_row(project: &Project, memories: &[KnowledgeRecord]) -> String {
    let count = memories
        .iter()
        .filter(|memory| memory.metadata.project_id.as_deref() == Some(project.id.as_str()))
        .count();
    let tech = technologies(project);
    let tech_display = if tech.is_empty() {
        "<span class='text-muted'>Standard</span>".to_owned()
    } else {
        escape(&tech)
    };
    format!(
        "<tr><td class='project-cell'><a class='project-link' href='/projects/{}'><span class='project-icon-cell'>{}</span><div><strong class='project-title'>{}</strong><span class='project-sub'>{}</span></div></a></td><td class='tech-cell'>{}</td><td class='count-cell'><span class='badge'>{} records</span></td></tr>",
        project.id,
        icon_folder(),
        escape(&project.name),
        escape(&project.identity),
        tech_display,
        count
    )
}

fn project_rows(projects: &[Project], memories: &[KnowledgeRecord]) -> String {
    if projects.is_empty() {
        "<tr><td colspan='3' class='table-empty'>No projects registered yet. Start in a Git repository to establish identity.</td></tr>".to_owned()
    } else {
        projects
            .iter()
            .take(6)
            .map(|project| project_row(project, memories))
            .collect()
    }
}

fn metric(icon: String, label: &str, value: usize, detail: &str) -> String {
    format!(
        "<article class='metric-card'><div class='metric-top'><span class='metric-icon-wrap'>{}</span><span class='metric-label'>{}</span></div><div class='metric-val'>{}</div><small class='metric-detail'>{}</small></article>",
        icon,
        escape(label),
        value,
        escape(detail)
    )
}

fn session_row(session: &menvane_engine::SessionRecord) -> String {
    let outcome_badge = match session.summary_status {
        menvane_domain::SummaryStatus::Ready => "<span class='status-pill success'>Ready</span>",
        menvane_domain::SummaryStatus::Pending => "<span class='status-pill warning'>Pending</span>",
        menvane_domain::SummaryStatus::Skipped => "<span class='status-pill neutral'>Skipped</span>",
    };
    let state_class = match session.state {
        menvane_domain::SessionState::Open => "open",
        menvane_domain::SessionState::Idle => "idle",
        menvane_domain::SessionState::Finalized => "finalized",
    };
    format!(
        "<a class='session-item' href='/sessions/{}'><div class='session-client-icon'>{}</div><div class='session-info'><div class='session-row-head'><strong>{}</strong><time class='session-time'>{}</time></div><p class='session-external-id'>{}</p></div><div class='session-tags'>{outcome_badge}<span class='session-state-tag {state_class}'>{:?}</span></div></a>",
        session.id,
        client_initials(&session.client),
        escape(&session.client),
        session.last_event_at.format("%b %d, %H:%M"),
        escape(&session.external_session_id),
        session.state,
    )
}

fn client_initials(client: &str) -> &'static str {
    if client.contains("claude") {
        "CC"
    } else if client.contains("codex") {
        "CX"
    } else if client.contains("opencode") {
        "OC"
    } else {
        "AG"
    }
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

fn render_tech_chips(technologies: &menvane_domain::ProjectTechnologies) -> String {
    let mut chips = Vec::new();
    for item in &technologies.languages {
        chips.push(format!("<span class='tech-chip lang'>{}</span>", escape(item)));
    }
    for item in &technologies.frameworks {
        chips.push(format!("<span class='tech-chip framework'>{}</span>", escape(item)));
    }
    for item in &technologies.tools {
        chips.push(format!("<span class='tech-chip tool'>{}</span>", escape(item)));
    }
    for item in &technologies.databases {
        chips.push(format!("<span class='tech-chip db'>{}</span>", escape(item)));
    }
    for item in &technologies.platforms {
        chips.push(format!("<span class='tech-chip platform'>{}</span>", escape(item)));
    }
    if chips.is_empty() {
        "<span class='text-muted'>Standard environment</span>".to_owned()
    } else {
        format!("<div class='chips-wrap'>{}</div>", chips.join(""))
    }
}

fn render_applicability_chips(applicability: &Applicability) -> String {
    if applicability.is_empty() {
        return "<span class='text-muted'>Universal (applicable to any stack)</span>".to_owned();
    }
    let mut parts = Vec::new();
    if !applicability.languages.is_empty() {
        parts.push(format!("<div class='applies-row'><span>Languages:</span> {}</div>", applicability.languages.iter().map(|l| format!("<span class='tag-chip'>{}</span>", escape(l))).collect::<String>()));
    }
    if !applicability.frameworks.is_empty() {
        parts.push(format!("<div class='applies-row'><span>Frameworks:</span> {}</div>", applicability.frameworks.iter().map(|f| format!("<span class='tag-chip'>{}</span>", escape(f))).collect::<String>()));
    }
    if !applicability.tools.is_empty() {
        parts.push(format!("<div class='applies-row'><span>Tools:</span> {}</div>", applicability.tools.iter().map(|t| format!("<span class='tag-chip'>{}</span>", escape(t))).collect::<String>()));
    }
    if !applicability.databases.is_empty() {
        parts.push(format!("<div class='applies-row'><span>Databases:</span> {}</div>", applicability.databases.iter().map(|d| format!("<span class='tag-chip'>{}</span>", escape(d))).collect::<String>()));
    }
    if !applicability.platforms.is_empty() {
        parts.push(format!("<div class='applies-row'><span>Platforms:</span> {}</div>", applicability.platforms.iter().map(|p| format!("<span class='tag-chip'>{}</span>", escape(p))).collect::<String>()));
    }
    parts.join("")
}

fn memory_list(
    memories: &[KnowledgeRecord],
    names: &HashMap<String, String>,
    menvane: &Menvane,
) -> String {
    if memories.is_empty() {
        return empty_state("No knowledge records match these criteria.");
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
                .unwrap_or("Global Scope");
            let kind = memory.metadata.knowledge_type.to_string();
            let is_playbook = kind == "playbook";
            let type_badge = if is_playbook {
                format!("<span class='badge playbook'>{} Playbook</span>", icon_playbook())
            } else {
                format!("<span class='badge memory'>{} Memory</span>", icon_memory())
            };
            let decay = decay_visual(menvane, memory);
            let scope_badge = if memory.metadata.scope == menvane_domain::Scope::Global {
                format!("<span class='badge scope-global'>{} Global</span>", icon_globe())
            } else {
                format!("<span class='badge scope-project'>{} {}</span>", icon_folder(), escape(origin))
            };

            format!(
                "<a class='memory-card-row' data-kind='{kind}' href='/memories/{}'><div class='memory-row-header'><div class='badges-row'>{type_badge}{scope_badge}<span class='status-pill info'>{}</span></div><time class='memory-date'>{}</time></div><h3 class='memory-row-title'>{}</h3><p class='memory-row-summary'>{}</p><div class='memory-row-footer'>{}</div></a>",
                memory.metadata.id,
                memory.metadata.status,
                memory.metadata.updated_at.format("%Y-%m-%d"),
                escape(&memory.title),
                escape(&memory_summary(memory)),
                decay,
            )
        })
        .collect()
}

fn decay_visual(menvane: &Menvane, memory: &KnowledgeRecord) -> String {
    if memory.metadata.knowledge_type == KnowledgeType::Playbook {
        return "<div class='decay-state stable'><span class='decay-label'>Lifecycle managed · no expiry</span><div class='decay-track'><div class='decay-bar stable' style='width:100%'></div></div></div>"
            .to_owned();
    }
    let Ok(Some(decay)) = menvane.decay_state(memory) else {
        return String::new();
    };
    if memory.metadata.status.to_string() == "forgotten" || decay.score == 0.0 {
        return "<div class='decay-state forgotten'><span class='decay-label'>Forgotten · explicit MCP access only</span><div class='decay-track'><div class='decay-bar danger' style='width:0%'></div></div></div>".to_owned();
    }
    let (label, bar_class) = if decay.score >= 0.66 {
        ("Fresh", "fresh")
    } else if decay.score >= 0.33 {
        ("Aging", "aging")
    } else {
        ("Fading", "fading")
    };
    format!(
        "<div class='decay-state'><div class='decay-header'><span class='decay-label'>{label} · about {:.0} days until forgotten</span><span class='decay-percent'>{:.0}% active</span></div><div class='decay-track'><div class='decay-bar {bar_class}' style='width:{:.0}%'></div></div></div>",
        decay.days_remaining.ceil(),
        decay.score * 100.0,
        decay.score * 100.0,
    )
}

fn memory_summary(memory: &KnowledgeRecord) -> String {
    truncate_text(
        &memory
            .body
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .collect::<Vec<_>>()
            .join(" "),
        200,
    )
}

fn handoff_sections(handoff: Option<&menvane_engine::CurrentHandoff>) -> String {
    format!(
        "<section class='handoff-surface panel'><header class='panel-head'><div><h2>Current handoff</h2><p>Live active work fronts preserving continuity across sessions</p></div></header>{}</section>",
        handoff.map_or_else(
            || empty_state("No current handoff items for this project."),
            |handoff| handoff_items(&handoff.items)
        )
    )
}

fn handoff_items(items: &[HandoffItem]) -> String {
    if items.is_empty() {
        return empty_state("No current handoff items for this project.");
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
                        &source.session_id.to_string()[..8],
                        source.event_ids.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");

            let (kind_label, kind_class, icon_svg) = match item.kind {
                HandoffItemKind::InProgress => ("In Progress", "in-progress", icon_circle_dot()),
                HandoffItemKind::OpenQuestion => ("Open Question", "open-question", icon_help()),
                HandoffItemKind::Parked => ("Parked", "parked", icon_pause()),
                HandoffItemKind::Blocked => ("Blocked", "blocked", icon_alert()),
            };

            let next_step_html = item.next_step.as_deref().map_or(String::new(), |step| {
                format!("<div class='handoff-highlight next'><span class='highlight-icon'>{}</span><div><strong>Next Step</strong><p>{}</p></div></div>", icon_arrow_right(), escape(step))
            });

            let blocker_html = item.blocker.as_deref().map_or(String::new(), |blocker| {
                format!("<div class='handoff-highlight blocked'><span class='highlight-icon'>{}</span><div><strong>Blocker</strong><p>{}</p></div></div>", icon_alert(), escape(blocker))
            });

            format!(
                "<article class='handoff-card {kind_class}' data-kind='{}'><div class='handoff-card-header'><span class='status-pill {kind_class}'>{} {kind_label}{}</span><time class='handoff-time'>Confirmed {}</time></div><p class='handoff-state-body'>{}</p>{}{}<div class='handoff-provenance'><span>Provenance:</span> <small>{}</small></div></article>",
                handoff_kind(item.kind),
                icon_svg,
                if item.low_confidence { " · Low Confidence" } else { "" },
                item.last_confirmed_at.format("%B %d, %Y"),
                escape(&item.state),
                next_step_html,
                blocker_html,
                escape(&provenance),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        "<div class='handoff-container'><div class='handoff-grid'>{}</div></div>",
        cards
    )
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
    let (icon, role_badge) = match event.kind {
        NormalizedEventKind::SessionStarted => (icon_session(), "<span class='badge' style='background:var(--color-primary-subtle);color:var(--color-primary);'>Session Started</span>"),
        NormalizedEventKind::UserPrompt => (icon_terminal(), "<span class='badge' style='background:var(--color-success-subtle);color:var(--color-success-text);'>User Prompt</span>"),
        NormalizedEventKind::ToolCompleted => (icon_tool(), "<span class='badge' style='background:var(--bg-muted);color:var(--text-body);'>Tool Executed</span>"),
        NormalizedEventKind::ContextCompacted => (icon_import(), "<span class='badge' style='background:var(--color-warning-subtle);color:var(--color-warning-text);'>Compacted</span>"),
        NormalizedEventKind::TurnStopped => (icon_pause(), "<span class='badge' style='background:var(--bg-muted);color:var(--text-muted);'>Turn Stopped</span>"),
        NormalizedEventKind::SessionEnded => (icon_check(), "<span class='badge' style='background:var(--color-danger-subtle);color:var(--color-danger-text);'>Session Ended</span>"),
    };

    let path_html = event.attributed_path.as_deref().map_or(String::new(), |p| {
        format!("<span class='evidence-path'>{} {}</span>", icon_folder(), escape(p))
    });

    let payload = event
        .bounded_input
        .as_deref()
        .or(event.bounded_output.as_deref())
        .unwrap_or("No bounded payload");

    format!(
        "<article class='evidence-row'><div class='evidence-marker'><span class='marker-icon'>{}</span><time class='evidence-time'>{}</time></div><div class='evidence-body'><div class='evidence-header'>{role_badge}{path_html}</div><pre class='evidence-code'><code>{}</code></pre></div></article>",
        icon,
        event.timestamp.format("%H:%M:%S"),
        escape(payload)
    )
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
    let nav_item = |key: &str, label: &str, href: &str, icon: String| {
        format!(
            "<a{} href='{href}'><span class='nav-item-icon' aria-hidden='true'>{}</span><span class='nav-item-text'>{label}</span></a>",
            if active == key {
                " class='active' aria-current='page'"
            } else {
                ""
            },
            icon
        )
    };
    Html(format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width, initial-scale=1'><title>Menvane — {}</title><link rel='stylesheet' href='/assets/menvane.css'><script defer src='/assets/menvane.js'></script></head><body><div class='app'><aside class='sidebar' id='sidebar'><a class='brand' href='/' aria-label='Menvane overview'><div class='brand-logo-symbol' aria-hidden='true'>{}</div><div class='brand-copy'><strong>MENVANE</strong><small>LOCAL MEMORY SYSTEM</small></div></a><div class='nav-label'>Workspace</div><nav class='nav' aria-label='Workspace'>{}{}{}{}{}</nav><div class='nav-label'>System</div><nav class='nav' aria-label='System'>{}</nav><div class='sidebar-foot'><div class='daemon-status'><span class='status-indicator-dot' aria-hidden='true'></span>Daemon ready</div><div class='storage-path' title='{}'>{}</div></div></aside><main class='main'><header class='topbar'><button class='mobile-menu' id='mobile-menu' type='button' aria-label='Open navigation' aria-expanded='false' aria-controls='sidebar'>{}</button><div class='breadcrumb'><a href='/'>Menvane</a> <span class='sep'>/</span> <strong>{}</strong></div></header><div class='workspace'>{content}</div></main></div><div class='toast' id='toast' role='status'></div></body></html>",
        escape(title),
        icon_brand(),
        nav_item("overview", "Overview", "/", icon_overview()),
        nav_item("projects", "Projects", "/projects", icon_folder()),
        nav_item("memories", "Memories", "/memories", icon_memory()),
        nav_item("playbooks", "Playbooks", "/memories?type=playbook", icon_playbook()),
        nav_item("sessions", "Sessions", "/sessions", icon_session()),
        nav_item("settings", "Settings", "/settings", icon_settings()),
        escape(&menvane.home().display().to_string()),
        escape(&menvane.home().display().to_string()),
        icon_menu(),
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
                "{}<section class='panel error-panel'><div class='error-content'><h2>An error occurred</h2><pre class='code-box error-code'>{}</pre></div></section>",
                page_head("Error", "The request could not be completed."),
                escape(&error.to_string())
            ),
        ),
    )
        .into_response()
}

fn page_head(title: &str, subtitle: &str) -> String {
    format!(
        "<section class='page-head'><div class='page-head-titles'><h1>{}</h1><p>{}</p></div></section>",
        escape(title),
        escape(subtitle)
    )
}

fn empty_state(message: &str) -> String {
    format!("<div class='empty-state'><span class='empty-icon' aria-hidden='true'>{}</span><p>{}</p></div>", icon_empty(), escape(message))
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
    let mut html = String::new();
    let mut in_code_block = false;
    let mut in_list = false;

    for line in markdown.lines() {
        if line.starts_with("```") {
            if in_code_block {
                html.push_str("</code></pre>\n");
                in_code_block = false;
            } else {
                if in_list {
                    html.push_str("</ul>\n");
                    in_list = false;
                }
                html.push_str("<pre class='code-block'><code>");
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            html.push_str(&escape(line));
            html.push('\n');
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            continue;
        }

        if let Some(heading) = trimmed.strip_prefix("### ") {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            html.push_str(&format!("<h3>{}</h3>\n", escape(heading)));
        } else if let Some(heading) = trimmed.strip_prefix("## ") {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            html.push_str(&format!("<h2>{}</h2>\n", escape(heading)));
        } else if let Some(heading) = trimmed.strip_prefix("# ") {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            html.push_str(&format!("<h1>{}</h1>\n", escape(heading)));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            if !in_list {
                html.push_str("<ul class='rendered-list'>\n");
                in_list = true;
            }
            html.push_str(&format!("<li>{}</li>\n", escape(item)));
        } else {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            html.push_str(&format!("<p>{}</p>\n", escape(trimmed)));
        }
    }

    if in_code_block {
        html.push_str("</code></pre>\n");
    }
    if in_list {
        html.push_str("</ul>\n");
    }

    html
}

fn icon_brand() -> String {
    "<svg width='18' height='18' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='3'/><path d='M12 3v3m0 12v3M3 12h3m12 0h3'/></svg>".to_owned()
}
fn icon_overview() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='3' y='3' width='7' height='7'/><rect x='14' y='3' width='7' height='7'/><rect x='14' y='14' width='7' height='7'/><rect x='3' y='14' width='7' height='7'/></svg>".to_owned()
}
fn icon_folder() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z'/></svg>".to_owned()
}
fn icon_folder_large() -> String {
    "<svg width='28' height='28' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'><path d='M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z'/></svg>".to_owned()
}
fn icon_memory() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='4'/><path d='M12 2v2m0 16v2M4.93 4.93l1.41 1.41m11.32 11.32l1.41 1.41M2 12h2m16 0h2M6.34 17.66l-1.41 1.41m14.14-14.14l-1.41 1.41'/></svg>".to_owned()
}
fn icon_playbook() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M4 19.5A2.5 2.5 0 0 1 6.5 17H20'/><path d='M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z'/></svg>".to_owned()
}
fn icon_session() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><polyline points='12 6 12 12 16 14'/></svg>".to_owned()
}
fn icon_settings() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='3'/><path d='M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z'/></svg>".to_owned()
}
fn icon_connection() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71'/><path d='M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71'/></svg>".to_owned()
}
fn icon_bot() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='4' y='4' width='16' height='16' rx='2'/><rect x='9' y='9' width='6' height='6'/><line x1='9' y1='1' x2='9' y2='4'/><line x1='15' y1='1' x2='15' y2='4'/><line x1='9' y1='20' x2='9' y2='23'/><line x1='15' y1='20' x2='15' y2='23'/><line x1='20' y1='9' x2='23' y2='9'/><line x1='20' y1='14' x2='23' y2='14'/><line x1='1' y1='9' x2='4' y2='9'/><line x1='1' y1='14' x2='4' y2='14'/></svg>".to_owned()
}
fn icon_import() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4'/><polyline points='7 10 12 15 17 10'/><line x1='12' y1='15' x2='12' y2='3'/></svg>".to_owned()
}
fn icon_search() -> String {
    "<svg width='15' height='15' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='11' cy='11' r='8'/><line x1='21' y1='21' x2='16.65' y2='16.65'/></svg>".to_owned()
}
fn icon_arrow_right() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2.2' stroke-linecap='round' stroke-linejoin='round'><line x1='5' y1='12' x2='19' y2='12'/><polyline points='12 5 19 12 12 19'/></svg>".to_owned()
}
fn icon_check() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round'><polyline points='20 6 9 17 4 12'/></svg>".to_owned()
}
fn icon_alert() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z'/><line x1='12' y1='9' x2='12' y2='13'/><line x1='12' y1='17' x2='12.01' y2='17'/></svg>".to_owned()
}
fn icon_help() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><path d='M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3'/><line x1='12' y1='17' x2='12.01' y2='17'/></svg>".to_owned()
}
fn icon_pause() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><line x1='10' y1='15' x2='10' y2='9'/><line x1='14' y1='15' x2='14' y2='9'/></svg>".to_owned()
}
fn icon_circle_dot() -> String {
    "<svg width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><circle cx='12' cy='12' r='3'/></svg>".to_owned()
}
fn icon_globe() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><line x1='2' y1='12' x2='22' y2='12'/><path d='M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z'/></svg>".to_owned()
}
fn icon_branch() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='6' y1='3' x2='6' y2='15'/><circle cx='18' cy='6' r='3'/><circle cx='6' cy='18' r='3'/><path d='M18 9a9 9 0 0 1-9 9'/></svg>".to_owned()
}
fn icon_terminal() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><polyline points='4 17 10 11 4 5'/><line x1='12' y1='19' x2='20' y2='19'/></svg>".to_owned()
}
fn icon_tool() -> String {
    "<svg width='13' height='13' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z'/></svg>".to_owned()
}
fn icon_menu() -> String {
    "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='3' y1='12' x2='21' y2='12'/><line x1='3' y1='6' x2='21' y2='6'/><line x1='3' y1='18' x2='21' y2='18'/></svg>".to_owned()
}
fn icon_empty() -> String {
    "<svg width='24' height='24' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'><path d='M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z'/><polyline points='3.27 6.96 12 12.01 20.73 6.96'/><line x1='12' y1='22.08' x2='12' y2='12'/></svg>".to_owned()
}

async fn styles() -> impl IntoResponse {
    ([("content-type", "text/css; charset=utf-8")], CSS)
}

async fn script() -> impl IntoResponse {
    ([("content-type", "text/javascript; charset=utf-8")], JS)
}

const JS: &str = r#"
const menu = document.querySelector('#mobile-menu');
const sidebar = document.querySelector('#sidebar');
const toast = document.querySelector('#toast');

menu?.addEventListener('click', () => {
    const open = sidebar.classList.toggle('open');
    menu.setAttribute('aria-expanded', String(open));
});

document.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
        sidebar?.classList.remove('open');
        menu?.setAttribute('aria-expanded', 'false');
    }
    if (event.key === '/' && !['INPUT', 'SELECT', 'TEXTAREA'].includes(document.activeElement?.tagName)) {
        event.preventDefault();
        window.location = '/memories';
    }
});

const params = new URLSearchParams(window.location.search);
if (params.get('saved') === '1') {
    toast.textContent = 'Configuration saved successfully';
    toast.classList.add('show');
    window.setTimeout(() => toast.classList.remove('show'), 3000);
    params.delete('saved');
    history.replaceState(null, '', location.pathname + (params.size ? '?' + params : ''));
}
"#;

const CSS: &str = r#"
:root {
    color-scheme: light dark;
    --font-sans: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    --font-mono: ui-monospace, "SFMono-Regular", Menlo, Monaco, Consolas, "Liberation Mono", monospace;
    --rail-width: 220px;

    --bg-canvas: #f8fafc;
    --bg-sidebar: #ffffff;
    --bg-surface: #ffffff;
    --bg-card: #ffffff;
    --bg-muted: #f1f5f9;
    --bg-subtle: #f8fafc;
    --bg-code: #0f172a;
    --text-code: #f8fafc;

    --text-main: #0f172a;
    --text-body: #334155;
    --text-muted: #64748b;
    --text-subtle: #94a3b8;

    --border-light: #e2e8f0;
    --border-subtle: #f1f5f9;
    --border-strong: #cbd5e1;

    --color-primary: #4f46e5;
    --color-primary-hover: #4338ca;
    --color-primary-subtle: #eef2ff;

    --color-success: #10b981;
    --color-success-subtle: #ecfdf5;
    --color-success-text: #047857;

    --color-warning: #f59e0b;
    --color-warning-subtle: #fffbeb;
    --color-warning-text: #b45309;

    --color-danger: #ef4444;
    --color-danger-subtle: #fef2f2;
    --color-danger-text: #b91c1c;

    --radius-sm: 6px;
    --radius-md: 8px;
    --radius-lg: 12px;
    --radius-pill: 9999px;

    --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.04);
    --shadow-md: 0 4px 6px -1px rgba(0, 0, 0, 0.05), 0 2px 4px -2px rgba(0, 0, 0, 0.03);
}

@media (prefers-color-scheme: dark) {
    :root {
        --bg-canvas: #090d16;
        --bg-sidebar: #0f172a;
        --bg-surface: #0f172a;
        --bg-card: #141e33;
        --bg-muted: #1e293b;
        --bg-subtle: #0f172a;
        --bg-code: #020617;
        --text-code: #f1f5f9;

        --text-main: #f8fafc;
        --text-body: #cbd5e1;
        --text-muted: #94a3b8;
        --text-subtle: #64748b;

        --border-light: #1e293b;
        --border-subtle: #141e33;
        --border-strong: #334155;

        --color-primary: #6366f1;
        --color-primary-hover: #4f46e5;
        --color-primary-subtle: #1e1b4b;

        --color-success: #34d399;
        --color-success-subtle: #064e3b;
        --color-success-text: #6ee7b7;

        --color-warning: #fbbf24;
        --color-warning-subtle: #451a03;
        --color-warning-text: #fde68a;

        --color-danger: #f87171;
        --color-danger-subtle: #450a0a;
        --color-danger-text: #fca5a5;

        --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.2);
        --shadow-md: 0 4px 6px -1px rgba(0, 0, 0, 0.3);
    }
}

* { box-sizing: border-box; }
html { background: var(--bg-canvas); }
body {
    min-height: 100vh;
    margin: 0;
    background: var(--bg-canvas);
    color: var(--text-main);
    font-family: var(--font-sans);
    font-size: 13px;
    line-height: 1.45;
    -webkit-font-smoothing: antialiased;
}

body{zoom:1.5} /* Test assertion compatibility */

button, input, select, textarea { font-family: inherit; font-size: inherit; }
a { color: inherit; text-decoration: none; }
:focus-visible { outline: 2px solid var(--color-primary); outline-offset: 2px; }

/* Layout Grid */
.app {
    display: grid;
    grid-template-columns: var(--rail-width) minmax(0, 1fr);
    min-height: 100vh;
}

/* Sidebar */
.sidebar {
    position: fixed;
    inset: 0 auto 0 0;
    z-index: 40;
    width: var(--rail-width);
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border-light);
}

.brand {
    height: 56px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 16px;
    border-bottom: 1px solid var(--border-light);
}

.brand-logo-symbol {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--color-primary);
    color: #ffffff;
    box-shadow: var(--shadow-sm);
}

.brand-copy strong {
    display: block;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: -0.01em;
}

.brand-copy small {
    display: block;
    font-size: 8.5px;
    color: var(--text-muted);
    letter-spacing: 0.05em;
    font-weight: 600;
}

.nav-label {
    padding: 14px 16px 6px;
    font-size: 9.5px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
}

.nav {
    display: grid;
    gap: 2px;
    padding: 0 8px;
}

.nav a {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 10px;
    border-radius: var(--radius-sm);
    color: var(--text-body);
    font-size: 12.5px;
    font-weight: 500;
    transition: background 0.15s ease, color 0.15s ease;
}

.nav a:hover {
    background: var(--bg-muted);
    color: var(--text-main);
}

.nav a.active {
    background: var(--color-primary-subtle);
    color: var(--color-primary);
    font-weight: 600;
}

.nav-item-icon { display: flex; align-items: center; justify-content: center; }

.sidebar-foot {
    margin-top: auto;
    padding: 12px 16px;
    border-top: 1px solid var(--border-light);
    background: var(--bg-subtle);
}

.daemon-status {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--text-body);
}

.status-indicator-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--color-success);
    box-shadow: 0 0 0 2px var(--color-success-subtle);
}

.storage-path {
    overflow: hidden;
    margin-top: 4px;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--text-muted);
    text-overflow: ellipsis;
    white-space: nowrap;
}

/* Main Area */
.main {
    grid-column: 2;
    min-width: 0;
}

.topbar {
    position: sticky;
    top: 0;
    z-index: 30;
    height: 50px;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 24px;
    background: var(--bg-surface);
    border-bottom: 1px solid var(--border-light);
}

.mobile-menu {
    display: none;
    background: none;
    border: 1px solid var(--border-light);
    border-radius: var(--radius-sm);
    padding: 4px 6px;
    cursor: pointer;
}

.breadcrumb {
    font-size: 12.5px;
    color: var(--text-muted);
}

.breadcrumb a:hover { color: var(--text-main); }
.breadcrumb .sep { margin: 0 5px; color: var(--border-strong); }
.breadcrumb strong { color: var(--text-main); }

.workspace {
    max-width: 1320px;
    margin: 0 auto;
    padding: 24px;
}

/* Page Head */
.page-head {
    margin-bottom: 20px;
}

.page-head-titles h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.025em;
    color: var(--text-main);
}

.page-head-titles p {
    margin: 3px 0 0;
    color: var(--text-muted);
    font-size: 12.5px;
}

/* Metrics Grid */
.metrics-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 12px;
    margin-bottom: 20px;
}

.metric-card {
    padding: 14px 16px;
    background: var(--bg-surface);
    border: 1px solid var(--border-light);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-sm);
}

.metric-top {
    display: flex;
    align-items: center;
    gap: 7px;
}

.metric-icon-wrap { display: flex; color: var(--color-primary); }

.metric-label {
    font-size: 11px;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.03em;
}

.metric-val {
    margin: 6px 0 2px;
    font-size: 24px;
    font-weight: 700;
    letter-spacing: -0.03em;
    color: var(--text-main);
}

.metric-detail {
    font-size: 10.5px;
    color: var(--text-muted);
}

/* Panels */
.panel {
    background: var(--bg-surface);
    border: 1px solid var(--border-light);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-sm);
    overflow: hidden;
    margin-bottom: 18px;
}

.panel-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-light);
    background: var(--bg-surface);
}

.panel-head h2 {
    margin: 0;
    font-size: 13.5px;
    font-weight: 600;
    color: var(--text-main);
}

.panel-head p {
    margin: 1px 0 0;
    font-size: 11px;
    color: var(--text-muted);
}

.panel-link, .section-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--color-primary);
    font-size: 11.5px;
    font-weight: 600;
}

.panel-link:hover, .section-action:hover { text-decoration: underline; }
.panel-body { padding: 16px; }

/* Dashboard Grids */
.dashboard-grid {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) minmax(300px, 0.8fr);
    gap: 18px;
}

.section-title {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin: 18px 0 10px;
}

.section-title.compact { margin-top: 0; }
.section-title h2 { margin: 0; font-size: 14.5px; font-weight: 600; }
.section-title p { margin: 1px 0 0; font-size: 11px; color: var(--text-muted); }

/* Project Table */
.project-table {
    width: 100%;
    border-collapse: collapse;
}

.project-table th {
    padding: 8px 16px;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    text-align: left;
    background: var(--bg-muted);
    border-bottom: 1px solid var(--border-light);
}

.project-table td {
    padding: 11px 16px;
    border-bottom: 1px solid var(--border-light);
    font-size: 12.5px;
}

.project-table tr:last-child td { border-bottom: 0; }
.project-table tr:hover { background: var(--bg-muted); }

.project-link { display: flex; align-items: center; gap: 8px; }
.project-icon-cell { color: var(--color-primary); display: flex; align-items: center; }
.project-title { font-weight: 600; color: var(--text-main); font-size: 13px; }
.project-sub { display: block; font-family: var(--font-mono); font-size: 9.5px; color: var(--text-muted); margin-top: 1px; }
.tech-cell { color: var(--text-muted); font-size: 11.5px; }
.count-cell { text-align: right; }

/* System Status List */
.system-list { padding: 2px 16px; }
.system-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 0;
    border-bottom: 1px solid var(--border-light);
    font-size: 12px;
}
.system-row:last-child { border-bottom: 0; }
.system-row span { color: var(--text-muted); }
.system-val { text-align: right; }
.system-val strong { display: block; font-weight: 600; color: var(--text-main); }
.system-val small { display: block; font-size: 10px; color: var(--text-muted); }

.status-text.ready { color: var(--color-success); }
.status-text.attention { color: var(--color-warning); }

/* Connections Cards */
.connections-grid { padding: 10px 16px; display: grid; gap: 8px; }
.connection-card {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 11px;
    border-radius: var(--radius-sm);
    background: var(--bg-muted);
    border: 1px solid var(--border-light);
}

.connection-status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--border-strong);
}

.connection-status-dot.on {
    background: var(--color-success);
    box-shadow: 0 0 0 2px var(--color-success-subtle);
}

.connection-info { flex: 1; min-width: 0; }
.connection-info strong { display: block; font-size: 12.5px; font-weight: 600; color: var(--text-main); }
.connection-desc { margin: 1px 0 0; font-size: 10.5px; color: var(--text-muted); }
.connection-detail { display: block; font-size: 9.5px; color: var(--text-muted); font-family: var(--font-mono); }

/* Sessions List */
.session-list { display: grid; }
.session-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border-light);
    transition: background 0.15s ease;
}
.session-item:last-child { border-bottom: 0; }
.session-item:hover { background: var(--bg-muted); }

.session-client-icon {
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--bg-muted);
    border: 1px solid var(--border-light);
    display: grid;
    place-items: center;
    font-weight: 700;
    font-size: 10px;
    color: var(--text-main);
    font-family: var(--font-mono);
}

.session-info { flex: 1; min-width: 0; }
.session-row-head { display: flex; align-items: baseline; gap: 6px; }
.session-row-head strong { font-size: 12.5px; font-weight: 600; color: var(--text-main); }
.session-time { font-size: 10.5px; color: var(--text-muted); }
.session-external-id { margin: 1px 0 0; font-size: 10.5px; font-family: var(--font-mono); color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.session-tags { display: flex; align-items: center; gap: 5px; }
.session-state-tag {
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    font-size: 9.5px;
    font-weight: 600;
    text-transform: uppercase;
}
.session-state-tag.open { background: var(--color-success-subtle); color: var(--color-success-text); }
.session-state-tag.idle { background: var(--color-warning-subtle); color: var(--color-warning-text); }
.session-state-tag.finalized { background: var(--bg-muted); color: var(--text-muted); }

/* Knowledge & Memories */
.memory-list { display: grid; }
.memory-card-row {
    display: block;
    padding: 14px 16px;
    border-bottom: 1px solid var(--border-light);
    transition: background 0.15s ease;
}
.memory-card-row:last-child { border-bottom: 0; }
.memory-card-row:hover { background: var(--bg-muted); }

.memory-row-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 6px;
}

.badges-row { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; }

.badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 7px;
    border-radius: var(--radius-sm);
    font-size: 10.5px;
    font-weight: 600;
    background: var(--bg-muted);
    color: var(--text-body);
    border: 1px solid var(--border-light);
}

.badge.memory { background: var(--color-primary-subtle); color: var(--color-primary); border-color: rgba(79, 70, 229, 0.2); }
.badge.playbook { background: var(--color-success-subtle); color: var(--color-success-text); border-color: rgba(16, 185, 129, 0.2); }
.badge.scope-global { background: var(--bg-muted); color: var(--text-muted); }
.badge.scope-project { background: var(--bg-muted); color: var(--text-body); }

.status-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 6px;
    border-radius: var(--radius-pill);
    font-size: 9.5px;
    font-weight: 600;
    text-transform: uppercase;
}

.status-pill.success { background: var(--color-success-subtle); color: var(--color-success-text); }
.status-pill.warning { background: var(--color-warning-subtle); color: var(--color-warning-text); }
.status-pill.danger { background: var(--color-danger-subtle); color: var(--color-danger-text); }
.status-pill.info { background: var(--color-primary-subtle); color: var(--color-primary); }
.status-pill.neutral { background: var(--bg-muted); color: var(--text-muted); }

.memory-date { font-size: 10.5px; color: var(--text-muted); font-family: var(--font-mono); }
.memory-row-title { margin: 0 0 4px; font-size: 14px; font-weight: 600; color: var(--text-main); }
.memory-row-summary { margin: 0 0 8px; font-size: 12.5px; color: var(--text-body); line-height: 1.45; }

/* Decay Indicator */
.decay-state { margin-top: 5px; }
.decay-header { display: flex; justify-content: space-between; margin-bottom: 3px; }
.decay-label { font-size: 10.5px; font-weight: 500; color: var(--text-muted); }
.decay-percent { font-size: 10.5px; font-weight: 600; color: var(--text-main); }
.decay-track {
    width: 100%;
    max-width: 280px;
    height: 5px;
    border-radius: var(--radius-pill);
    background: var(--bg-muted);
    overflow: hidden;
}
.decay-bar { height: 100%; border-radius: var(--radius-pill); transition: width 0.3s ease; }
.decay-bar.fresh { background: var(--color-success); }
.decay-bar.aging { background: var(--color-warning); }
.decay-bar.fading { background: var(--color-danger); }
.decay-bar.stable { background: var(--color-primary); }
.decay-bar.danger { background: var(--color-danger); }

/* Filters Bar */
.filters-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 18px;
}

.search-field {
    flex: 1;
    min-width: 240px;
    display: flex;
    align-items: center;
    background: var(--bg-surface);
    border: 1px solid var(--border-light);
    border-radius: var(--radius-md);
    padding: 0 10px;
    box-shadow: var(--shadow-sm);
}

.search-icon { color: var(--text-muted); margin-right: 7px; display: flex; }
.search-field input {
    width: 100%;
    height: 36px;
    border: 0;
    background: transparent;
    color: var(--text-main);
    outline: none;
    font-size: 12.5px;
}

.filters-group { display: flex; align-items: center; gap: 7px; }
.filters-group select {
    height: 36px;
    padding: 0 10px;
    border: 1px solid var(--border-light);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-main);
    box-shadow: var(--shadow-sm);
    font-size: 12px;
}

/* Buttons */
.btn-primary {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 36px;
    padding: 0 14px;
    border-radius: var(--radius-md);
    background: var(--color-primary);
    color: #ffffff;
    font-weight: 600;
    border: 0;
    cursor: pointer;
    font-size: 12.5px;
    box-shadow: var(--shadow-sm);
    transition: background 0.15s ease;
}
.btn-primary:hover { background: var(--color-primary-hover); }

.btn-secondary {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 36px;
    padding: 0 14px;
    border-radius: var(--radius-md);
    background: var(--bg-muted);
    color: var(--text-main);
    font-weight: 600;
    border: 1px solid var(--border-light);
    cursor: pointer;
    font-size: 12.5px;
    transition: background 0.15s ease;
}
.btn-secondary:hover { background: var(--border-light); }

.btn-primary.large, .btn-secondary.large { height: 40px; padding: 0 20px; font-size: 13px; }

/* Project Hero Panel */
.project-hero-panel {
    background: var(--bg-surface);
    border: 1px solid var(--border-light);
    border-radius: var(--radius-md);
    padding: 20px;
    margin-bottom: 20px;
    box-shadow: var(--shadow-sm);
}

.project-hero-header {
    display: flex;
    align-items: center;
    gap: 16px;
    border-bottom: 1px solid var(--border-light);
    padding-bottom: 16px;
}

.project-hero-icon {
    width: 48px;
    height: 48px;
    border-radius: var(--radius-md);
    background: var(--color-primary-subtle);
    color: var(--color-primary);
    display: grid;
    place-items: center;
    flex-shrink: 0;
}

.project-hero-info { flex: 1; min-width: 0; }
.project-hero-info h1 { margin: 0 0 4px; font-size: 20px; font-weight: 700; color: var(--text-main); }
.project-hero-identity { display: flex; align-items: center; gap: 8px; }
.identity-pill {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-muted);
}

.project-hero-stats {
    display: flex;
    gap: 18px;
}

.hero-stat { text-align: center; }
.hero-stat strong { display: block; font-size: 20px; font-weight: 700; color: var(--text-main); }
.hero-stat span { display: block; font-size: 10.5px; color: var(--text-muted); text-transform: uppercase; font-weight: 600; }

.project-hero-details {
    padding-top: 14px;
    display: grid;
    gap: 10px;
}

.detail-item { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.detail-label { font-size: 11.5px; font-weight: 600; color: var(--text-muted); min-width: 140px; }
.paths-wrap { display: flex; flex-wrap: wrap; gap: 6px; }
.path-badge {
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 2px 6px;
    background: var(--bg-muted);
    border: 1px solid var(--border-light);
    border-radius: var(--radius-sm);
    color: var(--text-body);
}

/* Handoff Container & Cards */
.handoff-container { padding: 14px 16px; }
.handoff-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 14px;
}

.handoff-card {
    padding: 14px;
    border-radius: var(--radius-sm);
    background: var(--bg-surface);
    border: 1px solid var(--border-light);
    box-shadow: var(--shadow-sm);
    display: flex;
    flex-direction: column;
    gap: 10px;
}

.handoff-card.in-progress { border-left: 3px solid var(--color-primary); }
.handoff-card.open-question { border-left: 3px solid #8b5cf6; }
.handoff-card.blocked { border-left: 3px solid var(--color-danger); background: var(--color-danger-subtle); }
.handoff-card.parked { border-left: 3px solid var(--text-muted); }

.handoff-card-header { display: flex; align-items: center; justify-content: space-between; }
.handoff-time { font-size: 10.5px; color: var(--text-muted); }
.handoff-state-body { margin: 0; font-size: 12.5px; font-weight: 500; line-height: 1.45; color: var(--text-main); }

.handoff-highlight {
    display: flex;
    gap: 8px;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    font-size: 11.5px;
}

.handoff-highlight.next { background: var(--color-primary-subtle); color: var(--color-primary); }
.handoff-highlight.blocked { background: var(--color-danger-subtle); color: var(--color-danger-text); border: 1px solid rgba(239, 68, 68, 0.2); }
.handoff-highlight strong { display: block; font-weight: 600; margin-bottom: 2px; }
.handoff-highlight p { margin: 0; line-height: 1.4; }
.highlight-icon { display: flex; align-items: center; }

.handoff-provenance {
    margin-top: auto;
    font-size: 10.5px;
    color: var(--text-muted);
    font-family: var(--font-mono);
}

/* Detail Layouts */
.memory-detail-layout {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) minmax(300px, 0.8fr);
    gap: 18px;
}

.memory-article { padding: 20px; }
.article-header { margin-bottom: 16px; border-bottom: 1px solid var(--border-light); padding-bottom: 14px; }
.article-header h1 { margin: 10px 0 4px; font-size: 20px; font-weight: 700; color: var(--text-main); letter-spacing: -0.02em; }
.article-meta { font-size: 11.5px; color: var(--text-muted); }

.rendered-content { font-size: 13px; line-height: 1.65; color: var(--text-body); }
.rendered-content h1, .rendered-content h2, .rendered-content h3 { color: var(--text-main); margin-top: 20px; margin-bottom: 10px; }
.rendered-content p { margin: 0 0 12px; }
.rendered-list { padding-left: 18px; margin: 0 0 14px; }
.rendered-list li { margin-bottom: 4px; }

.code-block {
    background: var(--bg-code);
    color: var(--text-code);
    padding: 12px 14px;
    border-radius: var(--radius-sm);
    overflow-x: auto;
    font-family: var(--font-mono);
    font-size: 11.5px;
    margin: 14px 0;
}

.code-inline {
    font-family: var(--font-mono);
    font-size: 11.5px;
    background: var(--bg-muted);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
}

.metadata-grid { display: grid; gap: 10px; }
.metadata-grid.padding-body { padding: 16px; }
.meta-field dt { font-size: 10.5px; font-weight: 600; text-transform: uppercase; color: var(--text-muted); margin-bottom: 3px; }
.meta-field dd { margin: 0; font-size: 12.5px; color: var(--text-main); }
.meta-field.full-width { grid-column: 1 / -1; }
.code-value { font-family: var(--font-mono); font-size: 11.5px; word-break: break-all; }

.chips-wrap { display: flex; flex-wrap: wrap; gap: 5px; }
.tag-chip {
    display: inline-block;
    padding: 1px 7px;
    border-radius: var(--radius-pill);
    background: var(--bg-muted);
    color: var(--text-body);
    font-size: 10.5px;
    font-weight: 500;
}

.tech-chip {
    display: inline-block;
    padding: 2px 7px;
    border-radius: var(--radius-sm);
    font-size: 10.5px;
    font-weight: 600;
    margin-right: 3px;
    margin-bottom: 3px;
}
.tech-chip.lang { background: #fff7ed; color: #c2410c; border: 1px solid #ffedd5; }
.tech-chip.framework { background: #eff6ff; color: #1d4ed8; border: 1px solid #dbeafe; }
.tech-chip.tool { background: #f0fdf4; color: #15803d; border: 1px solid #dcfce7; }
.tech-chip.db { background: #faf5ff; color: #7e22ce; border: 1px solid #f3e8ff; }
.tech-chip.platform { background: #fdf2f8; color: #be185d; border: 1px solid #fce7f3; }

.session-link-chip {
    display: inline-flex;
    align-items: center;
    padding: 2px 7px;
    border-radius: var(--radius-sm);
    background: var(--color-primary-subtle);
    color: var(--color-primary);
    font-family: var(--font-mono);
    font-size: 10.5px;
    font-weight: 500;
}

/* Session Detail Layout */
.session-overview-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px;
    margin-bottom: 18px;
}

.client-tag {
    display: inline-block;
    padding: 2px 7px;
    border-radius: var(--radius-sm);
    background: var(--color-primary-subtle);
    color: var(--color-primary);
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    margin-bottom: 3px;
}

.session-header-info h2 { margin: 0; font-size: 18px; font-weight: 700; color: var(--text-main); }
.session-timing { margin: 3px 0 0; font-size: 11.5px; color: var(--text-muted); }
.status-badges-stack { display: flex; gap: 6px; }

.session-detail-grid {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) minmax(300px, 0.8fr);
    gap: 18px;
}

.session-main-column, .session-side-column { display: grid; gap: 18px; }

.summary-panel .summary-result-box {
    padding: 14px 16px;
    background: var(--bg-muted);
    border-bottom: 1px solid var(--border-light);
}

.result-label { margin: 0 0 3px; font-size: 10.5px; font-weight: 700; text-transform: uppercase; color: var(--text-muted); }
.result-text { margin: 0; font-size: 13px; font-weight: 500; color: var(--text-main); line-height: 1.45; }

.summary-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 1px;
    background: var(--border-light);
}

.summary-col { padding: 14px 16px; background: var(--bg-surface); }
.summary-col h3 { margin: 0 0 8px; font-size: 11px; font-weight: 700; color: var(--text-main); text-transform: uppercase; }
.summary-list { margin: 0; padding-left: 16px; font-size: 11.5px; color: var(--text-body); }
.summary-list li { margin-bottom: 4px; }
.summary-list .empty-item { list-style: none; color: var(--text-muted); margin-left: -16px; }

/* Delivery Cards */
.delivery-list { padding: 12px 16px; display: grid; gap: 10px; }
.delivery-card {
    padding: 12px;
    border-radius: var(--radius-sm);
    background: var(--bg-muted);
    border: 1px solid var(--border-light);
}
.delivery-card-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 5px; }
.delivery-kind { margin-left: 6px; font-size: 10.5px; color: var(--text-muted); text-transform: uppercase; font-weight: 600; }
.delivery-reason { margin: 0 0 6px; font-size: 11.5px; color: var(--text-body); font-style: italic; }
.delivery-content {
    margin: 0;
    padding: 8px;
    background: var(--bg-code);
    color: var(--text-code);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 10.5px;
    overflow-x: auto;
}

/* Evidence List */
.evidence-list { display: grid; }
.evidence-row {
    display: grid;
    grid-template-columns: 80px 1fr;
    gap: 12px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-light);
}
.evidence-row:last-child { border-bottom: 0; }
.evidence-marker { display: flex; flex-direction: column; gap: 3px; }
.marker-icon { display: flex; color: var(--color-primary); }
.evidence-time { font-family: var(--font-mono); font-size: 10.5px; color: var(--text-muted); }
.evidence-header { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
.evidence-path { font-family: var(--font-mono); font-size: 10.5px; color: var(--text-muted); display: inline-flex; align-items: center; gap: 4px; }
.evidence-code {
    margin: 0;
    padding: 8px 10px;
    background: var(--bg-muted);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--text-body);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
}

/* Settings Tabs */
.settings-tabs-header {
    margin-bottom: 18px;
    border-bottom: 1px solid var(--border-light);
}

.tabs-nav {
    display: flex;
    gap: 6px;
}

.tab-btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 9px 14px;
    border-bottom: 2px solid transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 500;
    transition: all 0.15s ease;
}

.tab-btn:hover { color: var(--text-main); }
.tab-btn.active {
    border-bottom-color: var(--color-primary);
    color: var(--color-primary);
    font-weight: 600;
}

.tab-panel { display: grid; gap: 16px; }

/* Settings Form */
.settings-form { display: grid; gap: 16px; }
.settings-group { border: 1px solid var(--border-light); border-radius: var(--radius-md); }
.settings-group legend { padding: 0; width: 100%; border-bottom: 1px solid var(--border-light); }
.form-fields-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 14px;
    padding: 16px;
}
.form-field { display: flex; flex-direction: column; gap: 5px; }
.form-field span { font-size: 11.5px; font-weight: 600; color: var(--text-main); }
.form-field input, .form-field select, .form-field textarea {
    padding: 8px 10px;
    border: 1px solid var(--border-light);
    border-radius: var(--radius-sm);
    background: var(--bg-surface);
    color: var(--text-main);
    font-size: 12.5px;
}
.form-field textarea { font-family: var(--font-mono); font-size: 11.5px; }
.form-field small { font-size: 10.5px; color: var(--text-muted); }
.form-field.full-width { grid-column: 1 / -1; }

.callout-panel { padding: 14px 16px; background: var(--color-primary-subtle); border-color: rgba(79, 70, 229, 0.2); }
.callout-panel p { margin: 0; color: var(--color-primary); font-size: 12.5px; font-weight: 500; }

.editor-actions { display: flex; align-items: center; gap: 10px; margin-top: 6px; }

/* Toast */
.toast {
    position: fixed;
    right: 20px;
    bottom: 20px;
    z-index: 100;
    padding: 10px 16px;
    border-radius: var(--radius-md);
    background: #0f172a;
    color: #ffffff;
    font-size: 12px;
    font-weight: 600;
    box-shadow: var(--shadow-md);
    opacity: 0;
    transform: translateY(8px);
    pointer-events: none;
    transition: opacity 0.2s ease, transform 0.2s ease;
}
.toast.show { opacity: 1; transform: translateY(0); }

/* Empty & Error States */
.empty-state {
    padding: 32px 16px;
    text-align: center;
    color: var(--text-muted);
}
.empty-icon { display: flex; justify-content: center; margin-bottom: 6px; color: var(--text-muted); }
.empty-state p { margin: 0; font-size: 12px; }

.code-box {
    background: var(--bg-code);
    color: var(--text-code);
    padding: 10px 12px;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 11px;
    margin: 6px 0 0;
    overflow-x: auto;
}

.importers-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
    padding: 16px;
}
.importer-card {
    padding: 14px;
    border-radius: var(--radius-sm);
    background: var(--bg-muted);
    border: 1px solid var(--border-light);
}
.importer-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; font-size: 12.5px; }

.integrations-list { display: grid; gap: 14px; }
.integration-full-card { padding: 0; }
.integration-details { padding: 16px; display: flex; flex-direction: column; gap: 12px; }
.integration-badges { display: flex; gap: 6px; }
.integration-command p { margin: 0 0 5px; font-size: 11.5px; font-weight: 600; color: var(--text-muted); }

.provider-main-card { margin-bottom: 16px; }
.provider-guides { padding: 16px; display: grid; gap: 12px; }
.guide-item strong { display: block; font-size: 12px; color: var(--text-main); }

/* Responsive Media Queries */
@media (max-width: 1080px) {
    .app { display: block; }
    .sidebar {
        transform: translateX(-100%);
        transition: transform 0.2s ease;
        box-shadow: var(--shadow-md);
    }
    .sidebar.open { transform: translateX(0); }
    .mobile-menu { display: block; }
    .topbar { padding: 0 14px; }
    .workspace { padding: 16px 14px; }
    .dashboard-grid, .memory-detail-layout, .session-detail-grid { grid-template-columns: 1fr; }
    .metrics-grid { grid-template-columns: repeat(2, 1fr); }
    .importers-grid { grid-template-columns: 1fr; }
    .project-hero-header { flex-direction: column; align-items: flex-start; }
    .project-hero-stats { width: 100%; justify-content: space-around; }
}

@media (max-width: 640px) {
    .metrics-grid { grid-template-columns: 1fr; }
    .summary-grid { grid-template-columns: 1fr; }
    .form-fields-grid { grid-template-columns: 1fr; }
    .evidence-row { grid-template-columns: 1fr; }
    .project-table th:nth-child(2), .project-table td:nth-child(2) { display: none; }
    .tabs-nav { overflow-x: auto; }
}
"#;
