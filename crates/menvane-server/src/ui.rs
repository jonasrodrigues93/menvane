use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use menvane_domain::{
    Memory, MemoryType, NormalizedEvent, Project, ProjectHandoff, ProviderHealth,
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
        .route("/memories/{id}/edit", post(edit_memory))
        .route("/procedures", get(procedures))
        .route("/sessions", get(sessions))
        .route("/sessions/{id}", get(session_detail))
        .route("/handoffs/{project_id}", get(handoff_detail))
        .route("/search", get(search))
        .route("/imports", get(imports))
        .route("/imports/associate", post(associate_orphan))
        .route("/integrations", get(integrations))
        .route("/providers", get(providers))
        .route("/settings", get(settings).post(update_settings))
        .route("/assets/menvane.css", get(styles))
        .route("/assets/menvane.js", get(script))
}

async fn dashboard(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = dashboard_content(&menvane).await;
    page_result(&menvane, "overview", "Overview", content)
}

async fn dashboard_content(menvane: &Menvane) -> anyhow::Result<String> {
    let projects = menvane.all_projects()?;
    let memories = menvane.all_memories()?;
    let jobs = menvane.jobs()?;
    let integrations = menvane.integrations()?;
    let provider = menvane.provider_health().await.ok();
    let names = project_names(&projects);
    let durable = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type != MemoryType::Session)
        .count();
    let global = memories
        .iter()
        .filter(|memory| memory.metadata.scope.to_string() == "global")
        .count();
    let procedures = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Procedure)
        .count();
    let session_count = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Session)
        .count();
    let pending = jobs.iter().filter(|job| job.status == "pending").count();
    let mut recent = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type != MemoryType::Session)
        .collect::<Vec<_>>();
    recent.sort_by_key(|memory| std::cmp::Reverse(memory.metadata.created_at));
    let mut recent_sessions = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Session)
        .collect::<Vec<_>>();
    recent_sessions.sort_by_key(|memory| std::cmp::Reverse(memory.metadata.created_at));
    let session_rows = recent_sessions
        .iter()
        .take(3)
        .map(|memory| session_row(memory, &names))
        .collect::<String>();
    let (provider_name, provider_model, provider_ready) = provider
        .map(|(name, model, health)| (name, model, matches!(health, ProviderHealth::Ready)))
        .unwrap_or_else(|| ("unconfigured".to_owned(), String::new(), false));
    let connected = integrations.iter().filter(|state| state.connected).count();
    let project_rows = projects
        .iter()
        .map(|project| project_row(project, &memories))
        .collect::<String>();
    Ok(format!(
        "<section class='page-head'><div><h1>Overview</h1><p>Memory inventory, capture activity and system health across all projects.</p></div></section><section class='metrics' aria-label='Memory statistics'>{}{}{}{}{}{}</section><div class='dashboard-grid'><section class='panel'><header class='panel-head'><h2>Recent durable memory</h2><p>Across all projects and global scope</p><div class='tabs' role='tablist' aria-label='Memory filters'><button class='tab active' type='button' data-filter='all'>All</button><button class='tab' type='button' data-filter='fact'>Facts</button><button class='tab' type='button' data-filter='procedure'>Procedures</button><button class='tab' type='button' data-filter='decision'>Decisions</button><button class='tab' type='button' data-filter='gotcha'>Gotchas</button></div></header><div class='memory-list'>{}</div></section><aside class='right-stack'><section class='panel'><header class='panel-head'><h2>Recent sessions</h2><a class='panel-link' href='/sessions'>All sessions →</a></header><div class='session-list'>{session_rows}</div></section><section class='panel'><header class='panel-head'><h2>System</h2><a class='panel-link' href='/providers'>Providers →</a></header><div class='system-list'><div class='system-row'><span>{} provider</span><div class='system-value'><strong{}>{}</strong><small>{}</small></div></div><div class='system-row'><span>Markdown / SQLite FTS5</span><div class='system-value'><strong>Ready</strong></div></div><div class='system-row'><span>Integrations</span><div class='system-value'><strong>{connected} connected</strong></div></div><div class='system-row'><span>Jobs</span><div class='system-value'><strong{}>{pending} queued</strong></div></div></div></section></aside></div><div class='section-title'><h2>Projects</h2><p>Recently active identities</p><a href='/projects'>All projects →</a></div><section class='panel'><table class='project-table'><thead><tr><th>Project</th><th>Technologies</th><th>Memory</th></tr></thead><tbody>{project_rows}</tbody></table></section>{}",
        metric(1, "Durable memory", durable, "RECORDS", false),
        metric(2, "Global memory", global, "SHARED CONTEXT", false),
        metric(3, "Procedures", procedures, "LEARNED WORK", false),
        metric(4, "Sessions", session_count, "CAPTURED EPISODES", false),
        metric(5, "Projects", projects.len(), "KNOWN IDENTITIES", false),
        metric(6, "Queue", pending, "PENDING JOBS", true),
        recent
            .iter()
            .take(4)
            .map(|memory| memory_row(memory, &names))
            .collect::<String>(),
        escape(&provider_name),
        if provider_ready {
            ""
        } else {
            " class='pending'"
        },
        if provider_ready { "Ready" } else { "Attention" },
        escape(&provider_model),
        if pending > 0 { " class='pending'" } else { "" },
        connection_strip(&integrations)
    ))
}

async fn projects(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.all_projects().and_then(|projects| {
        let memories = menvane.all_memories()?;
        let rows = projects
            .iter()
            .map(|project| project_row(project, &memories))
            .collect::<String>();
        Ok(format!(
            "{}<section class='panel'><table class='project-table'><thead><tr><th>Project</th><th>Technologies</th><th>Memory</th></tr></thead><tbody>{rows}</tbody></table></section>",
            page_head("Projects", "Stable identities, not directory names.")
        ))
    });
    page_result(&menvane, "projects", "Projects", content)
}

async fn project_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<String>) -> Response {
    let content = menvane.all_projects().and_then(|projects| {
        let project = projects
            .into_iter()
            .find(|project| project.id == id)
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let memories = menvane
            .all_memories()?
            .into_iter()
            .filter(|memory| memory.metadata.project_id.as_deref() == Some(project.id.as_str()))
            .collect::<Vec<_>>();
        let handoff = menvane.current_project_handoff(Some(&project.id))?;
        let mut names = HashMap::new();
        names.insert(project.id.clone(), project.name.clone());
        Ok(format!(
            "{}<section class='panel'><dl class='metadata'><dt>Identity</dt><dd>{}</dd><dt>Known paths</dt><dd>{}</dd><dt>Languages</dt><dd>{}</dd><dt>Frameworks</dt><dd>{}</dd><dt>Tools</dt><dd>{}</dd><dt>Databases</dt><dd>{}</dd><dt>Platforms</dt><dd>{}</dd></dl></section>{}<section class='panel memory-panel'><div class='memory-list'>{}</div></section>",
            page_head(
                &project.name,
                &format!("{} durable memories", memories.len())
            ),
            escape(&project.identity),
            escape(&project.known_paths.join(" · ")),
            escape(&project.technologies.languages.join(", ")),
            escape(&project.technologies.frameworks.join(", ")),
            escape(&project.technologies.tools.join(", ")),
            escape(&project.technologies.databases.join(", ")),
            escape(&project.technologies.platforms.join(", ")),
            handoff_sections(handoff.as_ref(), &project.id),
            memories
                .iter()
                .map(|memory| memory_row(memory, &names))
                .collect::<String>()
        ))
    });
    page_result(&menvane, "projects", "Project", content)
}

#[derive(Default, Deserialize)]
struct MemoryFilters {
    scope: Option<String>,
    r#type: Option<String>,
    status: Option<String>,
    technology: Option<String>,
}

async fn memories(
    State(menvane): State<Arc<Menvane>>,
    Query(filters): Query<MemoryFilters>,
) -> Response {
    let content = menvane.all_memories().and_then(|memories| {
        let names = project_names(&menvane.all_projects()?);
        let form = filter_form(&filters);
        let filtered = memories
            .iter()
            .filter(|memory| memory_matches(memory, &filters))
            .map(|memory| memory_row(memory, &names))
            .collect::<String>();
        Ok(format!(
            "{}{form}<section class='panel memory-panel'><div class='memory-list'>{filtered}</div></section>",
            page_head("Memories", "Filter the durable source, not a shadow database.")
        ))
    });
    page_result(&menvane, "memories", "Memories", content)
}

async fn memory_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    let content = menvane.read(id).and_then(|memory| {
        let metadata = serde_yaml::to_string(&memory.metadata)?;
        Ok(format!(
            "{}<section class='panel'><div class='detail-grid'><article class='rendered'>{}</article><aside class='detail-side'><p class='stamp'>{} · {} · {:.0}%</p><dl class='metadata'><dt>Sources</dt><dd>{}</dd><dt>Applies to</dt><dd>{}</dd><dt>Success / failure</dt><dd>{} / {}</dd><dt>Supersedes</dt><dd>{}</dd></dl></aside></div></section><details class='raw'><summary>Raw Markdown and metadata</summary><pre>---\n{}---\n# {}\n\n{}</pre></details><form class='editor panel' method='post' action='/memories/{}/edit'><label>Title<input name='title' value='{}'></label><label>Markdown body<textarea name='body' rows='18'>{}</textarea></label><button>Commit manual edit</button></form>",
            page_head(&memory.title, "Durable record detail"),
            render_markdown(&memory.body),
            memory.metadata.scope,
            memory.metadata.status,
            memory.metadata.confidence * 100.0,
            escape(
                &memory
                    .metadata
                    .source_sessions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            escape(&serde_json::to_string(&memory.metadata.applies_to)?),
            memory.metadata.successes.unwrap_or(0),
            memory.metadata.failures.unwrap_or(0),
            escape(
                &memory
                    .metadata
                    .supersedes
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            escape(&metadata),
            escape(&memory.title),
            escape(&memory.body),
            id,
            escape_attribute(&memory.title),
            escape(&memory.body)
        ))
    });
    page_result(&menvane, "memories", "Memory", content)
}

#[derive(Deserialize)]
struct EditMemory {
    title: String,
    body: String,
}

async fn edit_memory(
    State(menvane): State<Arc<Menvane>>,
    Path(id): Path<Uuid>,
    Form(edit): Form<EditMemory>,
) -> Response {
    match menvane.edit_memory(id, &edit.title, &edit.body) {
        Ok(_) => Redirect::to(&format!("/memories/{id}")).into_response(),
        Err(error) => error_page(&menvane, error),
    }
}

async fn procedures(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.all_memories().and_then(|memories| {
        let names = project_names(&menvane.all_projects()?);
        let rows = memories
            .iter()
            .filter(|memory| memory.metadata.memory_type == MemoryType::Procedure)
            .map(|memory| memory_row(memory, &names))
            .collect::<String>();
        Ok(format!(
            "{}<section class='panel memory-panel'><div class='memory-list'>{rows}</div></section>",
            page_head(
                "Procedures",
                "Candidates become dependable through evidence."
            )
        ))
    });
    page_result(&menvane, "procedures", "Procedures", content)
}

async fn sessions(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.all_memories().and_then(|memories| {
        let names = project_names(&menvane.all_projects()?);
        let mut sessions = memories
            .iter()
            .filter(|memory| memory.metadata.memory_type == MemoryType::Session)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|memory| std::cmp::Reverse(memory.metadata.created_at));
        let rows = sessions
            .iter()
            .map(|memory| session_row(memory, &names))
            .collect::<String>();
        Ok(format!(
            "{}<section class='panel'><div class='session-list'>{rows}</div></section>",
            page_head(
                "Sessions",
                "Chronological, sanitized capture reconstructed from operational evidence."
            )
        ))
    });
    page_result(&menvane, "sessions", "Sessions", content)
}

async fn session_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    let content = menvane.read(id).and_then(|memory| {
        let events = menvane.session_events(id)?;
        let handoff = menvane.session_project_handoff(id)?;
        let evidence = events.iter().map(session_evidence_row).collect::<String>();
        let handoff_rows = handoff
            .as_ref()
            .map(project_handoff_row)
            .unwrap_or_else(|| "<div class='empty-state'>No handoff covers this session.</div>".to_owned());
        Ok(format!(
            "{}<section class='session-overview panel'><div><span class='eyebrow'>Captured session</span><h2>{}</h2><p>{} · {} · generation {}</p></div><a class='panel-link' href='/memories/{}'>Open finalized record →</a></section><div class='session-detail-grid'><section class='panel'><header class='panel-head'><h2>Session evidence</h2><p>Bounded normalized events</p></header><div class='evidence-list'>{}</div></section><section class='panel'><header class='panel-head'><h2>Generated handoffs</h2><p>Artifacts and source evidence</p></header><div class='handoff-list'>{}</div></section></div>",
            page_head(&memory.title, "Operational evidence for one captured session."),
            escape(&memory.title),
            escape(memory.metadata.client.as_deref().unwrap_or("unknown")),
            escape(memory.metadata.external_session_id.as_deref().unwrap_or("unknown")),
            memory.metadata.generation.unwrap_or(0),
            id,
            evidence,
            handoff_rows
        ))
    });
    page_result(&menvane, "sessions", "Session", content)
}

async fn handoff_detail(
    State(menvane): State<Arc<Menvane>>,
    Path(project_id): Path<String>,
) -> Response {
    let content = menvane
        .current_project_handoff(Some(&project_id))
        .and_then(|handoff| {
            handoff
                .ok_or_else(|| anyhow::anyhow!("no handoff for project {project_id}"))
                .map(|handoff| {
                    format!(
                        "{}<section class='panel handoff-detail'><div class='handoff-detail-head'><div><span class='eyebrow'>Project handoff</span><h2>Current summary</h2><p>Updated {}</p></div></div><div class='handoff-detail-grid'><div><article class='rendered'><p>{}</p></article><h3>Source sessions</h3>{}</div></div></section>",
                        page_head("Handoff", "The single current project summary."),
                        handoff.updated_at.format("%Y-%m-%d %H:%M"),
                        escape(&handoff.summary),
                        handoff
                            .source_session_ids
                            .iter()
                            .map(|id| format!("<div class='version-row'><strong>session</strong><span>{id}</span></div>"))
                            .collect::<String>()
                    )
                })
        });
    page_result(&menvane, "projects", "Handoff", content)
}

#[derive(Default, Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

async fn search(State(menvane): State<Arc<Menvane>>, Query(query): Query<SearchQuery>) -> Response {
    let cwd = std::env::current_dir().unwrap_or_default();
    let results = query
        .q
        .as_deref()
        .filter(|query| !query.is_empty())
        .map(|query| menvane.search(&cwd, query, ScopeSelection::Auto, 20))
        .transpose();
    let content = results.map(|results| {
        let rows = results
            .unwrap_or_default()
            .iter()
            .map(|memory| {
                format!("<a class='memory-row' href='/memories/{}' data-kind='{}'><span class='type'>{}</span><span class='memory-copy'><h3>{}</h3><p>{}</p><span class='memory-meta'><span class='status'>{}</span><span>FTS rank {}</span><span>freshness {:.3}</span><span>score {:.5}</span></span></span><span class='memory-tail'><span class='scope-tag'>{}</span></span></a>",
                    memory.id,
                    escape(&memory.memory_type),
                    type_letter(&memory.memory_type),
                    escape(&memory.title),
                    escape(&memory.excerpt),
                    escape(&memory.status),
                    memory.fts_rank,
                    menvane_engine::DecayEngine::freshness(&memory.memory_type, memory.age_days),
                    memory.score,
                    escape(&memory.scope))
            })
            .collect::<String>();
        format!(
            "{}<form class='search-bar' action='/search'><span>⌕</span><input name='q' value='{}' placeholder='Search historical context'><button>Search</button></form><section class='panel memory-panel'><div class='memory-list'>{rows}</div></section>",
            page_head("Recall", "The same retrieval engine used by agents. RRF K=60."),
            escape_attribute(query.q.as_deref().unwrap_or_default())
        )
    });
    page_result(&menvane, "search", "Recall", content)
}

async fn imports(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.orphans().and_then(|orphans| {
        let projects = menvane.all_projects()?;
        let rows = orphans
            .iter()
            .map(|orphan| {
                let options = projects
                    .iter()
                    .map(|project| {
                        format!(
                            "<option value='{}'>{}</option>",
                            project.id,
                            escape(&project.name)
                        )
                    })
                    .collect::<String>();
                format!("<form class='orphan-row' method='post' action='/imports/associate'><span class='type'>{}</span><span class='memory-copy'><h3>{}</h3><p>{}</p></span><input type='hidden' name='client' value='{}'><input type='hidden' name='external_session_id' value='{}'><select name='project_id'>{options}</select><button>Associate</button></form>",
                    escape(&orphan.client.chars().next().unwrap_or('?').to_uppercase().collect::<String>()),
                    escape(&orphan.client),
                    escape(&orphan.external_session_id),
                    escape_attribute(&orphan.client),
                    escape_attribute(&orphan.external_session_id))
            })
            .collect::<String>();
        Ok(format!(
            "{}<section class='panel callout'><pre>menvane import claude --dry-run\nmenvane import codex --dry-run\nmenvane import opencode --dry-run</pre><p>Unresolved identities remain orphaned until explicitly associated.</p></section><section class='panel memory-panel'>{rows}</section>",
            page_head("Imports", "Preview external evidence before consolidation.")
        ))
    });
    page_result(&menvane, "imports", "Imports", content)
}

#[derive(Deserialize)]
struct AssociateOrphan {
    client: String,
    external_session_id: String,
    project_id: String,
}

async fn associate_orphan(
    State(menvane): State<Arc<Menvane>>,
    Form(form): Form<AssociateOrphan>,
) -> Response {
    match menvane.associate_orphan(&form.client, &form.external_session_id, &form.project_id) {
        Ok(_) => Redirect::to("/imports").into_response(),
        Err(error) => error_page(&menvane, error),
    }
}

async fn integrations(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.integrations().map(|states| {
        format!(
            "{}{}<section class='panel callout'><pre>menvane connect all</pre></section>",
            page_head("Connections", "Three agents, one local memory plane."),
            connection_strip(&states)
        )
    });
    page_result(&menvane, "integrations", "Connections", content)
}

async fn providers(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.provider_health().await.map(|(provider, model, health)| {
        let ready = matches!(health, ProviderHealth::Ready);
        format!(
            "{}<section class='panel'><div class='system-list'><div class='system-row'><span>Active provider</span><div class='system-value'><strong>{}</strong></div></div><div class='system-row'><span>Model</span><div class='system-value'><strong>{}</strong></div></div><div class='system-row'><span>Health</span><div class='system-value'><strong{}>{:?}</strong></div></div><div class='system-row'><span>Credentials</span><div class='system-value'><strong>Hidden</strong><small>Environment or existing local authentication; never displayed</small></div></div></div></section>",
            page_head("Providers", "Inference is isolated from retrieval."),
            escape(&provider),
            escape(&model),
            if ready { "" } else { " class='pending'" },
            health
        )
    });
    page_result(&menvane, "providers", "Providers", content)
}

async fn settings(State(menvane): State<Arc<Menvane>>) -> Response {
    let content = menvane.configuration_text().map(|configuration| {
        format!(
            "{}<section class='panel callout'><p>Secret values are environment-only. Restart the daemon after changes.</p></section><form class='editor panel' method='post'><label>Configuration<textarea name='configuration' rows='28'>{}</textarea></label><button>Validate and save</button></form>",
            page_head("Settings", "Observable runtime configuration."),
            escape(&configuration)
        )
    });
    page_result(&menvane, "settings", "Settings", content)
}

#[derive(Deserialize)]
struct SettingsEdit {
    configuration: String,
}

async fn update_settings(
    State(menvane): State<Arc<Menvane>>,
    Form(edit): Form<SettingsEdit>,
) -> Response {
    match menvane.update_configuration_text(&edit.configuration) {
        Ok(_) => Redirect::to("/settings").into_response(),
        Err(error) => error_page(&menvane, error),
    }
}

async fn styles() -> impl IntoResponse {
    ([("content-type", "text/css; charset=utf-8")], CSS)
}

async fn script() -> impl IntoResponse {
    ([("content-type", "text/javascript; charset=utf-8")], JS)
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
    let projects = menvane.all_projects().unwrap_or_default();
    let memories = menvane.all_memories().unwrap_or_default();
    let durable = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type != MemoryType::Session)
        .count();
    let procedures = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Procedure)
        .count();
    let sessions = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Session)
        .count();
    let nav_item = |key: &str, number: &str, label: &str, href: &str, count: Option<usize>| {
        format!(
            "<a{} href='{}'><span class='nav-icon'>{number}</span>{label}{}</a>",
            if active == key { " class='active'" } else { "" },
            href,
            count
                .map(|count| format!("<span class='nav-count'>{count:02}</span>"))
                .unwrap_or_default()
        )
    };
    Html(format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width, initial-scale=1'><title>Menvane — {}</title><link rel='stylesheet' href='/assets/menvane.css'><script defer src='/assets/menvane.js'></script></head><body><div class='app'><aside class='sidebar' id='sidebar'><a class='brand' href='/' aria-label='Menvane overview'><span class='brand-mark' aria-hidden='true'></span><span class='brand-copy'><strong>MENVANE</strong><small>LOCAL MEMORY</small></span></a><div class='nav-label'>Workspace</div><nav class='nav' aria-label='Workspace'>{}{}{}{}{}{}</nav><div class='nav-label'>System</div><nav class='nav' aria-label='System'>{}{}{}{}</nav><div class='sidebar-foot'><div class='daemon'><i></i>Daemon ready · :{}</div><div class='storage'>{} · Markdown / SQLite FTS5</div></div></aside><main class='main'><header class='topbar'><button class='mobile-menu' id='mobile-menu' type='button' aria-label='Open navigation'>≡</button><div class='breadcrumb'>Menvane / <strong>{}</strong></div><button class='command-trigger' id='command-trigger' type='button'><span>⌕</span>Search memory or navigate<kbd>Ctrl K</kbd></button><div class='local-label'>Local only</div></header><div class='workspace'>{content}</div></main></div><div class='palette-backdrop' id='palette-backdrop' role='dialog' aria-modal='true' aria-label='Command palette'><div class='palette'><label class='palette-search'><span>⌕</span><input id='palette-input' type='search' placeholder='Search memories, projects or commands'></label><div class='palette-list'><div class='palette-label'>Quick actions</div><a class='palette-item' href='/search'><span>01</span><span>Recall memory</span><kbd>Enter</kbd></a><a class='palette-item' href='/projects'><span>02</span><span>Browse projects</span><kbd>P</kbd></a><a class='palette-item' href='/memories'><span>03</span><span>Browse durable memories</span><kbd>M</kbd></a><a class='palette-item' href='/sessions'><span>04</span><span>Open recent sessions</span><kbd>S</kbd></a></div></div></div><div class='toast' id='toast' role='status'></div></body></html>",
        escape(title),
        nav_item("overview", "01", "Overview", "/", None),
        nav_item("projects", "02", "Projects", "/projects", Some(projects.len())),
        nav_item("memories", "03", "Memories", "/memories", Some(durable)),
        nav_item("procedures", "04", "Procedures", "/procedures", Some(procedures)),
        nav_item("sessions", "05", "Sessions", "/sessions", Some(sessions)),
        nav_item("search", "06", "Recall", "/search", None),
        nav_item("imports", "07", "Imports", "/imports", None),
        nav_item("integrations", "08", "Connections", "/integrations", None),
        nav_item("providers", "09", "Providers", "/providers", None),
        nav_item("settings", "10", "Settings", "/settings", None),
        crate::DEFAULT_PORT,
        escape(&menvane.home().display().to_string()),
        escape(title)
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
                "{}<section class='panel callout'><pre>{}</pre></section>",
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

fn metric(index: usize, label: &str, value: usize, note: &str, queue: bool) -> String {
    format!(
        "<article class='metric{}'><span class='metric-label'><b>{index:02}</b>{}</span><strong>{value:02}</strong><small>{}</small></article>",
        if queue { " queue" } else { "" },
        escape(label),
        escape(note)
    )
}

fn project_names(projects: &[Project]) -> HashMap<String, String> {
    projects
        .iter()
        .map(|project| (project.id.clone(), project.name.clone()))
        .collect()
}

fn type_letter(memory_type: &str) -> &'static str {
    match memory_type {
        "fact" => "F",
        "decision" => "D",
        "procedure" => "P",
        "gotcha" => "!",
        _ => "S",
    }
}

fn memory_row(memory: &Memory, names: &HashMap<String, String>) -> String {
    let metadata = &memory.metadata;
    let kind = metadata.memory_type.to_string();
    let excerpt = memory
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let origin = metadata
        .project_id
        .as_ref()
        .and_then(|id| names.get(id))
        .cloned()
        .unwrap_or_else(|| "Global".to_owned());
    let evidence = match (metadata.successes, metadata.failures) {
        (Some(successes), _) if metadata.memory_type == MemoryType::Procedure => {
            format!("{successes} successes")
        }
        _ => metadata.tags.first().cloned().unwrap_or_default(),
    };
    format!(
        "<a class='memory-row' href='/memories/{}' data-kind='{kind}'><span class='type'>{}</span><span class='memory-copy'><h3>{}</h3><p>{}</p><span class='memory-meta'><span class='status {}'>{}</span><span>{}</span><span>{}</span></span></span><span class='memory-tail'><span class='scope-tag'>{}</span><time>{}</time></span></a>",
        metadata.id,
        type_letter(&kind),
        escape(&memory.title),
        escape(excerpt),
        metadata.status,
        title_case(&metadata.status.to_string()),
        escape(&origin),
        escape(&evidence),
        title_case(&metadata.scope.to_string()),
        metadata.created_at.format("%Y-%m-%d")
    )
}

fn session_row(memory: &Memory, names: &HashMap<String, String>) -> String {
    let metadata = &memory.metadata;
    let origin = metadata
        .project_id
        .as_ref()
        .and_then(|id| names.get(id))
        .cloned()
        .unwrap_or_else(|| "unresolved".to_owned());
    let client = metadata.client.as_deref().unwrap_or("unknown");
    let state = if metadata.imported.unwrap_or(false) {
        "Imported"
    } else {
        "Captured"
    };
    format!(
        "<article class='session-row'><time>{}</time><div><strong><a href='/sessions/{}'>{}</a></strong><p>{} · {}</p></div><span class='session-state'>{}</span></article>",
        metadata.created_at.format("%d %b"),
        metadata.id,
        escape(&memory.title),
        escape(&origin),
        escape(client),
        state
    )
}

fn session_evidence_row(event: &NormalizedEvent) -> String {
    let detail = event
        .bounded_input
        .as_deref()
        .or(event.bounded_output.as_deref())
        .unwrap_or("No bounded payload");
    format!(
        "<article class='evidence-row'><div><strong>{}</strong><span>{}</span></div><p>{}</p><small>{}</small></article>",
        event_kind(event),
        escape(event.tool_family.as_deref().unwrap_or("session")),
        escape(detail),
        escape(
            event
                .attributed_path
                .as_deref()
                .unwrap_or("no attributed file")
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

fn project_handoff_row(handoff: &ProjectHandoff) -> String {
    format!(
        "<a class='session-handoff-row' href='/handoffs/{}'><div><strong>Project handoff</strong><p>{}</p></div></a>",
        escape(handoff.project_id.as_deref().unwrap_or("global")),
        escape(&handoff.summary)
    )
}

fn handoff_sections(handoff: Option<&ProjectHandoff>, _project_id: &str) -> String {
    let Some(handoff) = handoff else {
        return "<section class='handoff-surface'><div class='section-title'><h2>Handoff</h2><p>Continuation summary</p></div><div class='panel empty-state'>No handoff summary has been generated for this project.</div></section>".to_owned();
    };
    format!(
        "<section class='handoff-surface'><div class='section-title'><h2>Handoff</h2><p>Current continuation summary</p><a href='/handoffs/{}'>Inspect →</a></div><article class='handoff-card current'><h3>Current summary</h3><p>Updated {}</p><div class='rendered'><p>{}</p></div><p class='handoff-meta'>{} source sessions; fingerprint {}</p></article></section>",
        escape(handoff.project_id.as_deref().unwrap_or("global")),
        handoff.updated_at.format("%Y-%m-%d %H:%M"),
        escape(&handoff.summary),
        handoff.source_session_ids.len(),
        if handoff.fingerprint.is_some() {
            "recorded"
        } else {
            "none"
        }
    )
}

fn project_row(project: &Project, memories: &[Memory]) -> String {
    let count = memories
        .iter()
        .filter(|memory| memory.metadata.project_id.as_deref() == Some(project.id.as_str()))
        .count();
    let technologies = [
        project.technologies.languages.as_slice(),
        project.technologies.frameworks.as_slice(),
    ]
    .concat()
    .join(" · ");
    format!(
        "<tr><td class='project-name'><strong><a href='/projects/{}'>{}</a></strong><small>{}</small></td><td class='tech'>{}</td><td class='number'>{count}</td></tr>",
        project.id,
        escape(&project.name),
        escape(&project.identity),
        escape(&technologies)
    )
}

fn connection_strip(states: &[menvane_engine::IntegrationRecord]) -> String {
    let clients = [
        ("CC", "Claude Code", "claude-code"),
        ("CX", "Codex", "codex"),
        ("OC", "OpenCode", "opencode"),
    ];
    format!(
        "<section class='connections' aria-label='Connections and provider'>{}</section>",
        clients
            .iter()
            .map(|(icon, name, key)| {
                let state = states.iter().find(|state| state.client == *key);
                let connected = state.is_some_and(|state| state.connected);
                let detail = state
                    .map(|state| {
                        format!(
                            "{} · {}",
                            if state.mcp_registered {
                                "MCP registered"
                            } else {
                                "MCP missing"
                            },
                            state.hook_status
                        )
                    })
                    .unwrap_or_else(|| "not installed".to_owned());
                format!(
                    "<article class='connection'><span class='connection-icon'>{icon}</span><div><strong>{name}</strong><small>{}</small></div><span class='connection-state{}'>{}</span></article>",
                    escape(&detail),
                    if connected { "" } else { " class='off'" },
                    if connected { "Connected" } else { "Disconnected" }
                )
            })
            .collect::<String>()
    )
}

fn memory_matches(memory: &Memory, filters: &MemoryFilters) -> bool {
    filters
        .scope
        .as_deref()
        .is_none_or(|value| value.is_empty() || memory.metadata.scope.to_string() == value)
        && filters.r#type.as_deref().is_none_or(|value| {
            value.is_empty() || memory.metadata.memory_type.to_string() == value
        })
        && filters
            .status
            .as_deref()
            .is_none_or(|value| value.is_empty() || memory.metadata.status.to_string() == value)
        && filters.technology.as_deref().is_none_or(|value| {
            value.is_empty()
                || serde_json::to_string(&memory.metadata.applies_to)
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains(&value.to_ascii_lowercase())
        })
}

fn filter_form(filters: &MemoryFilters) -> String {
    fn select(name: &str, placeholder: &str, options: &[&str], current: Option<&str>) -> String {
        format!(
            "<select name='{name}'><option value=''>{placeholder}</option>{}</select>",
            options
                .iter()
                .map(|option| {
                    format!(
                        "<option value='{option}'{}>{option}</option>",
                        if current == Some(option) {
                            " selected"
                        } else {
                            ""
                        }
                    )
                })
                .collect::<String>()
        )
    }
    format!(
        "<form class='filters' action='/memories'>{}<button>Apply</button></form>",
        [
            select(
                "scope",
                "All scopes",
                &["project", "global"],
                filters.scope.as_deref()
            ),
            select(
                "type",
                "All types",
                &["fact", "decision", "procedure", "gotcha", "session"],
                filters.r#type.as_deref()
            ),
            select(
                "status",
                "All states",
                &[
                    "active",
                    "candidate",
                    "needs-validation",
                    "superseded",
                    "historical",
                ],
                filters.status.as_deref()
            ),
            format!(
                "<input name='technology' placeholder='technology' value='{}'>",
                escape_attribute(filters.technology.as_deref().unwrap_or_default())
            )
        ]
        .concat()
    )
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn render_markdown(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            if let Some(value) = line.strip_prefix("## ") {
                format!("<h2>{}</h2>", escape(value))
            } else if let Some(value) = line.strip_prefix("### ") {
                format!("<h3>{}</h3>", escape(value))
            } else if let Some(value) = line.strip_prefix("- ") {
                format!("<li>{}</li>", escape(value))
            } else if line.trim().is_empty() {
                String::new()
            } else {
                format!("<p>{}</p>", escape(line))
            }
        })
        .collect()
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

const JS: &str = r"const palette=document.querySelector('#palette-backdrop');const paletteInput=document.querySelector('#palette-input');const commandTrigger=document.querySelector('#command-trigger');const sidebar=document.querySelector('#sidebar');const toast=document.querySelector('#toast');function openPalette(){palette.classList.add('open');window.setTimeout(()=>paletteInput.focus(),20)}function closePalette(){palette.classList.remove('open');paletteInput.value=''}function showToast(message){toast.textContent=message;toast.classList.add('show');window.clearTimeout(showToast.timer);showToast.timer=window.setTimeout(()=>toast.classList.remove('show'),2200)}commandTrigger.addEventListener('click',openPalette);palette.addEventListener('click',event=>{if(event.target===palette)closePalette()});paletteInput.addEventListener('keydown',event=>{if(event.key==='Enter'&&paletteInput.value.trim()){window.location='/search?q='+encodeURIComponent(paletteInput.value.trim())}});document.addEventListener('keydown',event=>{if((event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='k'){event.preventDefault();palette.classList.contains('open')?closePalette():openPalette()}if(event.key==='Escape'){closePalette();sidebar.classList.remove('open')}});document.querySelector('#mobile-menu').addEventListener('click',()=>sidebar.classList.toggle('open'));document.querySelectorAll('.tab').forEach(tab=>{tab.addEventListener('click',()=>{document.querySelectorAll('.tab').forEach(item=>item.classList.remove('active'));tab.classList.add('active');document.querySelectorAll('.memory-row').forEach(row=>{row.hidden=tab.dataset.filter!=='all'&&row.dataset.kind!==tab.dataset.filter})})});";

const CSS: &str = r#"
:root {
  color-scheme: light;
  --canvas: #efeee8;
  --surface: #faf9f5;
  --surface-raised: #ffffff;
  --surface-muted: #e7e6df;
  --ink: #1d1e1b;
  --text: #3e403a;
  --muted: #777970;
  --quiet: #a3a59b;
  --line: #d0d1c9;
  --line-strong: #a9aba1;
  --accent: #315cf4;
  --accent-soft: #e7ebff;
  --signal: #b9e936;
  --signal-soft: #eff8d4;
  --warn: #d88614;
  --warn-soft: #fff0d9;
  --danger: #d8523f;
  --danger-soft: #fde6e2;
  --rail: 336px;
  --mono: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  --sans: Inter, "Aptos", "Segoe UI", Arial, sans-serif;
}

* { box-sizing: border-box; }

html { background: var(--canvas); }

body {
  min-width: 480px;
  min-height: 100vh;
  margin: 0;
  background: var(--canvas);
  color: var(--ink);
  font-family: var(--sans);
  font-size: 20px;
}

button, input, select, textarea { font: inherit; }
button, a { -webkit-tap-highlight-color: transparent; }
button { color: inherit; }
a { color: inherit; }

:focus-visible {
  outline: 5px solid rgba(49, 92, 244, 0.35);
  outline-offset: 3px;
}

.app {
  display: grid;
  grid-template-columns: var(--rail) minmax(0, 1fr);
  min-height: 100vh;
}

.sidebar {
  position: fixed;
  inset: 0 auto 0 0;
  z-index: 30;
  width: var(--rail);
  height: 100vh;
  display: flex;
  flex-direction: column;
  border-right: 2px solid var(--line-strong);
  background: #e5e4dd;
}

.brand {
  height: 102px;
  display: flex;
  align-items: center;
  gap: 17px;
  padding: 0 26px;
  border-bottom: 2px solid var(--line-strong);
  text-decoration: none;
}

.brand-mark {
  position: relative;
  width: 45px;
  height: 45px;
  flex: 0 0 auto;
  border: 2px solid var(--ink);
  background: var(--signal);
}

.brand-mark::before,
.brand-mark::after {
  content: "";
  position: absolute;
  background: var(--ink);
}

.brand-mark::before { width: 21px; height: 2px; left: 11px; top: 21px; }
.brand-mark::after { width: 2px; height: 21px; left: 21px; top: 11px; }
.brand-copy strong,
.brand-copy small { display: block; }
.brand-copy strong { font: 800 20px var(--mono); letter-spacing: 0.1em; }
.brand-copy small { margin-top: 6px; color: var(--muted); font: 11px var(--mono); letter-spacing: 0.08em; }

.nav-label {
  padding: 30px 26px 11px;
  color: var(--quiet);
  font: 12px var(--mono);
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.nav { display: grid; gap: 3px; padding: 0 14px; }

.nav a {
  min-height: 56px;
  display: grid;
  grid-template-columns: 33px 1fr auto;
  align-items: center;
  gap: 12px;
  padding: 0 14px;
  border: 2px solid transparent;
  color: var(--text);
  text-decoration: none;
  font-size: 17px;
}

.nav a:hover { border-color: var(--line-strong); background: rgba(255, 255, 255, 0.45); }
.nav a.active { border-color: var(--ink); background: var(--surface-raised); color: var(--ink); box-shadow: 5px 5px 0 var(--ink); }
.nav-icon { color: var(--muted); font: 12px var(--mono); }
.nav a.active .nav-icon { color: var(--accent); }
.nav-count { color: var(--quiet); font: 12px var(--mono); }

.sidebar-foot {
  margin-top: auto;
  padding: 21px 26px 26px;
  border-top: 2px solid var(--line-strong);
}

.daemon {
  display: flex;
  align-items: center;
  gap: 12px;
  color: var(--text);
  font: 12px var(--mono);
  text-transform: uppercase;
}

.daemon i { width: 11px; height: 11px; background: var(--signal); border: 2px solid #769b0a; }
.storage { overflow: hidden; margin-top: 14px; color: var(--muted); font: 11px/1.5 var(--mono); text-overflow: ellipsis; white-space: nowrap; }

.main { grid-column: 2; min-width: 0; }

.topbar {
  position: sticky;
  top: 0;
  z-index: 25;
  height: 78px;
  display: flex;
  align-items: center;
  gap: 24px;
  padding: 0 36px;
  border-bottom: 2px solid var(--line-strong);
  background: rgba(239, 238, 232, 0.94);
  backdrop-filter: blur(21px);
}

.mobile-menu { display: none; }
.breadcrumb { color: var(--muted); font: 12px var(--mono); letter-spacing: 0.04em; text-transform: uppercase; }
.breadcrumb strong { color: var(--ink); }

.command-trigger {
  width: min(630px, 45vw);
  height: 48px;
  display: flex;
  align-items: center;
  gap: 14px;
  margin-left: auto;
  padding: 0 15px;
  border: 2px solid var(--line-strong);
  background: var(--surface);
  color: var(--muted);
  cursor: pointer;
  text-align: left;
  font: 12px var(--mono);
}

.command-trigger:hover { border-color: var(--ink); background: var(--surface-raised); }
.command-trigger kbd { margin-left: auto; padding: 3px 6px; border: 2px solid var(--line); background: var(--canvas); font: 11px var(--mono); }
.local-label { display: flex; align-items: center; gap: 11px; color: var(--muted); font: 12px var(--mono); white-space: nowrap; }
.local-label::before { content: ""; width: 9px; height: 9px; background: var(--signal); border: 2px solid #769b0a; }

.workspace { max-width: 2220px; margin: 0 auto; padding: 42px 45px 75px; }

.page-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 36px;
  margin-bottom: 36px;
}

.page-head h1 { margin: 0; font-size: 45px; line-height: 1; letter-spacing: -0.035em; overflow-wrap: anywhere; }
.page-head p { margin: 12px 0 0; color: var(--muted); font-size: 17px; }
.metrics {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  margin-bottom: 27px;
  border: 2px solid var(--line-strong);
  background: var(--surface);
}

.metric { min-width: 0; padding: 21px 23px; border-right: 2px solid var(--line); }
.metric:last-child { border-right: 0; }
.metric-label { display: flex; align-items: center; gap: 11px; color: var(--muted); font: 11px var(--mono); letter-spacing: 0.03em; text-transform: uppercase; }
.metric-label b { color: var(--quiet); font-weight: 400; }
.metric strong { display: block; margin-top: 15px; font: 600 36px/1 var(--mono); letter-spacing: -0.06em; }
.metric small { display: block; overflow: hidden; margin-top: 11px; color: var(--quiet); font: 11px var(--mono); text-overflow: ellipsis; white-space: nowrap; }
.metric.queue strong { color: var(--warn); }

.dashboard-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.45fr) minmax(450px, 0.55fr);
  gap: 27px;
  align-items: start;
}

.panel { border: 2px solid var(--line-strong); background: var(--surface); }
.panel-head { min-height: 72px; display: flex; align-items: center; gap: 15px; padding: 0 21px; border-bottom: 2px solid var(--line); flex-wrap: wrap; }
.panel-head h2 { margin: 0; font-size: 18px; font-weight: 650; }
.panel-head p { margin: 0; color: var(--muted); font: 11px var(--mono); }
.panel-link { margin-left: auto; color: var(--accent); font: 12px var(--mono); text-decoration: none; }
.panel-link:hover { text-decoration: underline; }

.tabs { display: flex; gap: 5px; margin-left: auto; }
.tab { min-height: 39px; padding: 0 12px; border: 2px solid transparent; background: transparent; color: var(--muted); cursor: pointer; font: 11px var(--mono); text-transform: uppercase; }
.tab:hover { border-color: var(--line); }
.tab.active { border-color: var(--ink); background: var(--accent-soft); color: var(--accent); }

.memory-list { display: grid; }
.memory-row {
  display: grid;
  grid-template-columns: 54px minmax(0, 1fr) auto;
  gap: 18px;
  min-height: 129px;
  align-items: start;
  padding: 20px 21px;
  border-bottom: 2px solid var(--line);
  text-decoration: none;
  transition: background 120ms ease;
}

.memory-row:last-child { border-bottom: 0; }
.memory-row:hover { background: var(--accent-soft); }
.memory-row[hidden] { display: none; }
.type { width: 45px; height: 45px; display: grid; place-items: center; border: 2px solid var(--line-strong); background: var(--surface-raised); color: var(--text); font: 14px var(--mono); }
.memory-row[data-kind="procedure"] .type { border-color: #88a91e; background: var(--signal-soft); }
.memory-row[data-kind="decision"] .type { border-color: #8498e9; background: var(--accent-soft); color: var(--accent); }
.memory-row[data-kind="gotcha"] .type { border-color: #dd9a8e; background: var(--danger-soft); color: var(--danger); }
.memory-copy h3 { margin: 0 0 8px; font-size: 17px; line-height: 1.3; }
.memory-copy p { overflow: hidden; margin: 0; color: var(--muted); font: 12px/1.5 var(--mono); text-overflow: ellipsis; white-space: nowrap; }
.memory-meta { display: flex; flex-wrap: wrap; gap: 14px; margin-top: 12px; color: var(--quiet); font: 11px var(--mono); text-transform: uppercase; }
.status { color: var(--text); }
.status.candidate { color: var(--warn); }
.memory-tail { display: grid; justify-items: end; gap: 12px; color: var(--quiet); font: 11px var(--mono); text-transform: uppercase; }
.scope-tag { padding: 5px 8px; border: 2px solid var(--line); background: var(--surface-raised); color: var(--text); }

.right-stack { display: grid; gap: 27px; }
.system-list { padding: 8px 21px 15px; }
.system-row { display: grid; grid-template-columns: 1fr auto; gap: 18px; align-items: center; min-height: 62px; border-bottom: 2px solid var(--line); }
.system-row:last-child { border-bottom: 0; }
.system-row span { color: var(--text); font-size: 15px; }
.system-value { text-align: right; }
.system-value strong { display: flex; align-items: center; justify-content: flex-end; gap: 9px; font: 12px var(--mono); text-transform: uppercase; }
.system-value strong::before { content: ""; width: 8px; height: 8px; background: var(--signal); border: 2px solid #769b0a; }
.system-value strong.pending::before { background: #ffc35b; border-color: var(--warn); }
.system-value small { display: block; margin-top: 6px; color: var(--quiet); font: 11px var(--mono); }

.section-title { display: flex; align-items: baseline; gap: 15px; margin: 36px 0 15px; }
.section-title h2 { margin: 0; font-size: 23px; }
.section-title p { margin: 0; color: var(--muted); font: 11px var(--mono); }
.section-title a { margin-left: auto; color: var(--accent); font: 12px var(--mono); text-decoration: none; }

.project-table { width: 100%; border-collapse: collapse; }
.project-table th { height: 50px; padding: 0 20px; border-bottom: 2px solid var(--line); color: var(--quiet); font: 11px var(--mono); text-align: left; text-transform: uppercase; }
.project-table td { height: 78px; padding: 0 20px; border-bottom: 2px solid var(--line); font-size: 14px; }
.project-table tr:last-child td { border-bottom: 0; }
.project-table tbody tr:hover { background: var(--accent-soft); }
.project-name strong { display: block; font-size: 15px; }
.project-name a { text-decoration: none; }
.project-name a:hover { color: var(--accent); }
.project-name small { display: block; max-width: 375px; overflow: hidden; margin-top: 6px; color: var(--quiet); font: 11px var(--mono); text-overflow: ellipsis; white-space: nowrap; }
.tech { color: var(--muted); font: 11px var(--mono); }
.number { font: 14px var(--mono); text-align: right; }

.session-list { padding: 5px 21px 12px; }
.session-row { display: grid; grid-template-columns: 75px 1fr auto; gap: 15px; padding: 17px 0; border-bottom: 2px solid var(--line); }
.session-row:last-child { border-bottom: 0; }
.session-row time { color: var(--quiet); font: 11px var(--mono); }
.session-row strong { display: block; font-size: 14px; }
.session-row strong a { text-decoration: none; }
.session-row strong a:hover { color: var(--accent); }
.session-row p { overflow: hidden; margin: 6px 0 0; color: var(--muted); font: 11px var(--mono); text-overflow: ellipsis; white-space: nowrap; }
.session-state { align-self: start; color: var(--muted); font: 11px var(--mono); text-transform: uppercase; }
.session-state.open { color: #66810d; }

.connections {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  margin-top: 27px;
  border: 2px solid var(--line-strong);
  background: var(--surface);
}

.connection { display: grid; grid-template-columns: 45px 1fr auto; align-items: center; gap: 15px; min-height: 92px; padding: 0 20px; border-right: 2px solid var(--line); }
.connection:last-child { border-right: 0; }
.connection-icon { width: 42px; height: 42px; display: grid; place-items: center; border: 2px solid var(--line-strong); background: var(--surface-raised); font: 12px var(--mono); }
.connection strong { display: block; font-size: 14px; }
.connection small { display: block; margin-top: 6px; color: var(--quiet); font: 11px var(--mono); }
.connection-state { display: flex; align-items: center; gap: 8px; color: var(--muted); font: 11px var(--mono); text-transform: uppercase; }
.connection-state::before { content: ""; width: 8px; height: 8px; background: var(--signal); border: 2px solid #769b0a; }
.connection-state.off::before { background: var(--danger); border-color: #a33627; }

.palette-backdrop {
  position: fixed;
  inset: 0;
  z-index: 100;
  display: none;
  place-items: start center;
  padding-top: min(15vh, 195px);
  background: rgba(29, 30, 27, 0.48);
  backdrop-filter: blur(6px);
}

.palette-backdrop.open { display: grid; }
.palette { width: min(885px, calc(100vw - 42px)); border: 2px solid var(--ink); background: var(--surface-raised); box-shadow: 9px 9px 0 var(--ink); animation: palette-in 140ms ease-out; }
.palette-search { display: flex; align-items: center; gap: 15px; padding: 21px; border-bottom: 2px solid var(--line-strong); }
.palette-search span { color: var(--accent); font: 23px var(--mono); }
.palette-search input { width: 100%; border: 0; outline: 0; background: transparent; color: var(--ink); font: 15px var(--mono); }
.palette-label { padding: 17px 18px 8px; color: var(--quiet); font: 11px var(--mono); text-transform: uppercase; }
.palette-list { padding: 8px; }
.palette-item { display: grid; grid-template-columns: 36px 1fr auto; align-items: center; gap: 14px; padding: 15px; font-size: 15px; text-decoration: none; }
.palette-item:first-of-type { background: var(--accent-soft); }
.palette-item:hover { background: var(--accent-soft); }
.palette-item kbd { color: var(--muted); font: 11px var(--mono); }

.toast {
  position: fixed;
  right: 30px;
  bottom: 30px;
  z-index: 120;
  max-width: min(630px, calc(100vw - 60px));
  padding: 17px 20px;
  border: 2px solid var(--ink);
  background: var(--signal);
  box-shadow: 6px 6px 0 var(--ink);
  font: 12px var(--mono);
  opacity: 0;
  transform: translateY(11px);
  pointer-events: none;
  transition: opacity 140ms ease, transform 140ms ease;
}

.toast.show { opacity: 1; transform: translateY(0); }

.memory-panel { margin-top: 0; }
.detail-grid { display: grid; grid-template-columns: minmax(0, 1.45fr) minmax(390px, 0.55fr); }
.rendered { padding: 27px; font-size: 17px; line-height: 1.6; }
.rendered h2 { font-size: 21px; margin: 21px 0 9px; }
.rendered h3 { font-size: 18px; margin: 18px 0 8px; }
.rendered p { margin: 0 0 12px; }
.rendered li { margin: 0 0 5px 24px; }
.detail-side { padding: 27px; border-left: 2px solid var(--line); }
.stamp { margin: 0 0 18px; color: var(--muted); font: 12px var(--mono); text-transform: uppercase; }
.metadata { display: grid; grid-template-columns: auto 1fr; gap: 11px 21px; margin: 0; padding: 24px; }
.metadata dt { color: var(--quiet); font: 11px var(--mono); text-transform: uppercase; }
.metadata dd { margin: 0; overflow-wrap: anywhere; font: 12px/1.5 var(--mono); color: var(--text); }
.raw { margin-top: 27px; border: 2px solid var(--line-strong); background: var(--surface); }
.raw summary { padding: 18px 21px; cursor: pointer; color: var(--muted); font: 12px var(--mono); text-transform: uppercase; }
.raw pre { margin: 0; padding: 21px; overflow-x: auto; border-top: 2px solid var(--line); font: 12px/1.6 var(--mono); }
.callout { margin-bottom: 27px; padding: 21px; }
.callout p { margin: 0; color: var(--muted); font: 12px/1.6 var(--mono); }
.callout pre { margin: 0 0 12px; font: 12px/1.6 var(--mono); }
.filters { display: flex; flex-wrap: wrap; gap: 12px; margin-bottom: 27px; }
.filters select, .filters input { height: 48px; padding: 0 14px; border: 2px solid var(--line-strong); background: var(--surface); color: var(--text); font: 12px var(--mono); }
.filters button, .editor button, .search-bar button, .orphan-row button { height: 48px; padding: 0 18px; border: 2px solid var(--ink); background: var(--signal); cursor: pointer; font: 12px var(--mono); text-transform: uppercase; box-shadow: 3px 3px 0 var(--ink); }
.filters button:hover, .editor button:hover, .search-bar button:hover, .orphan-row button:hover { background: var(--signal-soft); }
.search-bar { display: flex; align-items: center; gap: 15px; margin-bottom: 27px; padding: 0 18px; height: 66px; border: 2px solid var(--line-strong); background: var(--surface); }
.search-bar span { color: var(--accent); font: 21px var(--mono); }
.search-bar input { flex: 1; border: 0; outline: 0; background: transparent; font: 15px var(--mono); }
.editor { margin-top: 27px; padding: 21px; display: grid; gap: 18px; justify-items: start; }
.editor label { display: grid; gap: 9px; width: 100%; color: var(--quiet); font: 11px var(--mono); text-transform: uppercase; }
.editor input, .editor textarea { width: 100%; padding: 14px; border: 2px solid var(--line-strong); background: var(--surface-raised); color: var(--ink); font: 14px/1.5 var(--mono); }
.editor textarea { resize: vertical; }
.orphan-row { display: grid; grid-template-columns: 54px minmax(0, 1fr) minmax(210px, auto) auto; gap: 18px; align-items: center; padding: 20px 21px; border-bottom: 2px solid var(--line); }
.orphan-row:last-child { border-bottom: 0; }
.orphan-row select { height: 45px; padding: 0 12px; border: 2px solid var(--line-strong); background: var(--surface-raised); font: 12px var(--mono); }

.handoff-surface { margin-top: 36px; }
.handoff-bucket { margin-bottom: 27px; }
.handoff-bucket > header { display: flex; align-items: baseline; gap: 14px; margin-bottom: 12px; }
.handoff-bucket > header h3 { margin: 0; font-size: 18px; }
.handoff-bucket > header span { color: var(--quiet); font: 12px var(--mono); }
.handoff-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 15px; }
.handoff-card { min-width: 0; padding: 18px; border: 2px solid var(--line-strong); background: var(--surface); box-shadow: 5px 5px 0 var(--line-strong); }
.handoff-card[data-kind="blocked"] { background: var(--warn-soft); }
.handoff-card[data-kind="completed"] { background: var(--surface-muted); }
.handoff-card-top { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.handoff-card-top > a { color: var(--accent); font: 12px var(--mono); text-decoration: none; }
.handoff-card-top > a:hover { text-decoration: underline; }
.handoff-status { display: inline-block; color: var(--text); font: 11px var(--mono); letter-spacing: .04em; text-transform: uppercase; }
.handoff-status.active, .handoff-status.ready { color: #66810d; }
.handoff-status.consumed { color: var(--accent); }
.handoff-status.stale, .handoff-status.superseded { color: var(--warn); }
.handoff-status.completed { color: var(--muted); }
.handoff-card h3 { margin: 15px 0; overflow-wrap: anywhere; font-size: 15px; line-height: 1.35; }
.handoff-facts { display: grid; grid-template-columns: 108px 1fr; gap: 8px 12px; margin: 0; }
.handoff-facts dt { color: var(--quiet); font: 11px var(--mono); text-transform: uppercase; }
.handoff-facts dd { overflow: hidden; margin: 0; color: var(--text); font: 11px/1.4 var(--mono); text-overflow: ellipsis; white-space: nowrap; }
.handoff-actions { display: flex; flex-wrap: wrap; gap: 9px; margin-top: 18px; }
.handoff-actions form { margin: 0; }
.quiet-action { min-height: 38px; padding: 0 11px; border: 2px solid var(--line-strong); background: var(--surface-raised); cursor: pointer; color: var(--text); font: 11px var(--mono); text-transform: uppercase; }
.quiet-action:hover { border-color: var(--ink); background: var(--signal-soft); }
.danger-action:hover { background: var(--danger-soft); color: var(--danger); }
.empty-state { padding: 24px; color: var(--muted); font: 12px var(--mono); }
.session-overview { display: flex; align-items: center; justify-content: space-between; gap: 27px; margin-bottom: 27px; padding: 24px; }
.session-overview h2, .handoff-detail h2 { margin: 8px 0 0; font-size: 24px; overflow-wrap: anywhere; }
.session-overview p, .handoff-detail-head p { margin: 9px 0 0; color: var(--muted); font: 12px var(--mono); }
.eyebrow { color: var(--quiet); font: 11px var(--mono); letter-spacing: .08em; text-transform: uppercase; }
.session-detail-grid { display: grid; grid-template-columns: minmax(0, 1.2fr) minmax(480px, .8fr); gap: 27px; align-items: start; }
.evidence-list, .handoff-list { display: grid; }
.evidence-row { display: grid; grid-template-columns: 225px minmax(0, 1fr) 225px; gap: 18px; align-items: start; padding: 18px 21px; border-bottom: 2px solid var(--line); }
.evidence-row:last-child, .session-handoff-row:last-child { border-bottom: 0; }
.evidence-row strong, .evidence-row span { display: block; }
.evidence-row strong { font-size: 14px; }
.evidence-row span, .evidence-row small { margin-top: 6px; color: var(--quiet); font: 11px var(--mono); }
.evidence-row p { overflow: hidden; margin: 0; color: var(--text); font: 12px/1.5 var(--mono); overflow-wrap: anywhere; }
.evidence-row small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.session-handoff-row { display: grid; grid-template-columns: 105px minmax(0, 1fr) 165px; gap: 15px; align-items: start; padding: 18px 21px; border-bottom: 2px solid var(--line); text-decoration: none; }
.session-handoff-row:hover { background: var(--accent-soft); }
.session-handoff-row strong { display: block; font-size: 14px; overflow-wrap: anywhere; }
.session-handoff-row p { margin: 6px 0 0; color: var(--muted); font: 11px var(--mono); overflow-wrap: anywhere; }
.session-handoff-row > .session-state { overflow-wrap: anywhere; }
.handoff-detail { padding: 24px; }
.handoff-detail-head { display: flex; justify-content: space-between; gap: 21px; padding-bottom: 23px; border-bottom: 2px solid var(--line); }
.handoff-detail-grid { display: grid; grid-template-columns: minmax(0, 1fr) 420px; gap: 27px; padding-top: 24px; }
.handoff-field-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 15px; }
.handoff-field-grid article { padding: 17px; border: 2px solid var(--line); background: var(--surface-raised); }
.handoff-field-grid h3, .handoff-detail-grid aside h3 { margin: 0 0 12px; color: var(--quiet); font: 11px var(--mono); letter-spacing: .05em; text-transform: uppercase; }
.handoff-field-grid p { margin: 0; color: var(--text); font: 12px/1.5 var(--mono); overflow-wrap: anywhere; }
.handoff-field-grid ul { margin: 0; padding-left: 23px; color: var(--text); font: 12px/1.5 var(--mono); }
.handoff-detail-grid aside { border-left: 2px solid var(--line); padding-left: 24px; }
.version-row { display: grid; grid-template-columns: 53px minmax(0, 1fr); gap: 8px; padding: 12px 0; border-bottom: 2px solid var(--line); font: 11px var(--mono); }
.version-row time { grid-column: 2; color: var(--quiet); }

@keyframes palette-in {
  from { opacity: 0; transform: translateY(-7px); }
  to { opacity: 1; transform: translateY(0); }
}

@media (max-width: 1770px) {
  :root { --rail: 308px; }
  .workspace { padding: 38px 35px 68px; }
  .metrics { grid-template-columns: repeat(3, 1fr); }
  .metric:nth-child(3) { border-right: 0; }
  .metric:nth-child(-n + 3) { border-bottom: 2px solid var(--line); }
  .dashboard-grid { grid-template-columns: 1fr; }
  .right-stack { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}

@media (max-width: 1170px) {
  .app { display: block; }
  .sidebar { width: min(417px, 86vw); transform: translateX(-105%); transition: transform 170ms ease; box-shadow: 27px 0 75px rgba(29, 30, 27, 0.22); }
  .sidebar.open { transform: translateX(0); }
  .main { grid-column: auto; }
  .topbar { padding: 0 20px; gap: 15px; }
  .mobile-menu { width: 45px; height: 45px; display: grid; place-items: center; border: 2px solid var(--ink); background: var(--signal); cursor: pointer; font: 20px var(--mono); }
  .breadcrumb { display: none; }
  .command-trigger { width: auto; flex: 1; margin: 0; }
  .local-label { font-size: 0; }
  .workspace { padding: 33px 21px 57px; }
  .right-stack { grid-template-columns: 1fr; }
  .detail-grid { grid-template-columns: 1fr; }
  .detail-side { border-left: 0; border-top: 2px solid var(--line); }
  .handoff-grid, .session-detail-grid { grid-template-columns: 1fr; }
  .handoff-detail-grid { grid-template-columns: 1fr; }
  .handoff-detail-grid aside { border-top: 2px solid var(--line); border-left: 0; padding: 24px 0 0; }
}

@media (max-width: 840px) {
  .page-head h1 { font-size: 38px; }
  .metrics { grid-template-columns: repeat(2, 1fr); }
  .metric:nth-child(2n) { border-right: 0; }
  .metric:nth-child(3) { border-right: 2px solid var(--line); }
  .metric:nth-child(-n + 4) { border-bottom: 2px solid var(--line); }
  .panel-head p { display: none; }
  .tabs .tab:not(.active) { display: none; }
  .memory-row { grid-template-columns: 51px minmax(0, 1fr); gap: 15px; }
  .memory-tail { display: none; }
  .memory-copy p { white-space: normal; }
  .project-table th:nth-child(2),
  .project-table td:nth-child(2),
  .project-table th:nth-child(3),
  .project-table td:nth-child(3) { display: none; }
  .connections { grid-template-columns: 1fr; }
  .connection { border-right: 0; border-bottom: 2px solid var(--line); }
  .connection:nth-child(2) { border-right: 0; }
  .connection:nth-child(-n + 2) { border-bottom: 2px solid var(--line); }
  .connection:last-child { border-bottom: 0; }
  .orphan-row { grid-template-columns: 51px minmax(0, 1fr); }
  .handoff-field-grid { grid-template-columns: 1fr; }
  .session-overview, .handoff-detail-head { align-items: flex-start; flex-direction: column; }
  .evidence-row { grid-template-columns: 1fr; gap: 9px; }
  .session-handoff-row { grid-template-columns: 1fr auto; }
  .session-handoff-row > div { grid-column: 1 / -1; grid-row: 1; }
}

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation-duration: 0.01ms !important; transition-duration: 0.01ms !important; }
}
"#;
