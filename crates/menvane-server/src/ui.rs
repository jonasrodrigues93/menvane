use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use menvane_domain::{Memory, MemoryType};
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
        .route("/search", get(search))
        .route("/imports", get(imports))
        .route("/integrations", get(integrations))
        .route("/providers", get(providers))
        .route("/settings", get(settings))
        .route("/assets/menvane.css", get(styles))
        .route("/assets/menvane.js", get(script))
}

async fn dashboard(State(menvane): State<Arc<Menvane>>) -> Response {
    page_result("Dashboard", dashboard_content(&menvane))
}

fn dashboard_content(menvane: &Menvane) -> anyhow::Result<String> {
    let projects = menvane.all_projects()?;
    let memories = menvane.all_memories()?;
    let jobs = menvane.jobs()?;
    let active = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Procedure)
        .filter(|memory| memory.metadata.status.to_string() == "active")
        .count();
    let candidates = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Procedure)
        .filter(|memory| memory.metadata.status.to_string() == "candidate")
        .count();
    let sessions = memories
        .iter()
        .filter(|memory| memory.metadata.memory_type == MemoryType::Session)
        .count();
    let global = memories
        .iter()
        .filter(|memory| memory.metadata.scope.to_string() == "global")
        .count();
    let pending = jobs.iter().filter(|job| job.status == "pending").count();
    Ok(format!(
        "<section class='hero'><p class='kicker'>Local memory instrument</p><h1>What survives<br>the session?</h1><p class='lede'>A human-readable ledger of decisions, failures and repeatable work.</p></section><section class='metrics'>{}{}{}{}{}{} </section><section class='rule'><span>Current archive</span><span>{} durable records</span></section>{}",
        metric("Projects", projects.len(), "known identities"),
        metric("Global", global, "shared memories"),
        metric("Procedures", active, "active"),
        metric("Candidates", candidates, "need reuse"),
        metric("Sessions", sessions, "episodic evidence"),
        metric("Queue", pending, "pending jobs"),
        memories.len(),
        memory_rows(memories.iter().take(8))
    ))
}

async fn projects(State(menvane): State<Arc<Menvane>>) -> Response {
    page_result(
        "Projects",
        menvane.all_projects().map(|projects| {
            let rows = projects
                .iter()
                .map(|project| format!("<a class='ledger-row' href='/projects/{}'><span>{}</span><strong>{}</strong><small>{}</small></a>", project.id, escape(&project.name), escape(&project.identity), project.technologies.languages.join(", ")))
                .collect::<String>();
            format!("{}<section class='ledger'>{rows}</section>", heading("Projects", "Stable identities, not directory names."))
        }),
    )
}

async fn project_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<String>) -> Response {
    page_result(
        "Project",
        menvane.all_projects().and_then(|projects| {
            let project = projects.into_iter().find(|project| project.id == id).ok_or_else(|| anyhow::anyhow!("project not found"))?;
            let memories = menvane.all_memories()?.into_iter().filter(|memory| memory.metadata.project_id.as_deref() == Some(&project.id)).collect::<Vec<_>>();
            Ok(format!("{}<dl class='metadata'><dt>Identity</dt><dd>{}</dd><dt>Known paths</dt><dd>{}</dd><dt>Languages</dt><dd>{}</dd><dt>Frameworks</dt><dd>{}</dd><dt>Tools</dt><dd>{}</dd><dt>Databases</dt><dd>{}</dd><dt>Platforms</dt><dd>{}</dd></dl>{}", heading(&project.name, &format!("{} memories", memories.len())), escape(&project.identity), escape(&project.known_paths.join(" · ")), escape(&project.technologies.languages.join(", ")), escape(&project.technologies.frameworks.join(", ")), escape(&project.technologies.tools.join(", ")), escape(&project.technologies.databases.join(", ")), escape(&project.technologies.platforms.join(", ")), memory_rows(memories.iter())))
        }),
    )
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
    page_result(
        "Memories",
        menvane.all_memories().map(|memories| {
            let filtered = memories.iter().filter(|memory| {
                filters.scope.as_deref().is_none_or(|value| {
                    value.is_empty() || memory.metadata.scope.to_string() == value
                }) && filters.r#type.as_deref().is_none_or(|value| {
                    value.is_empty() || memory.metadata.memory_type.to_string() == value
                }) && filters.status.as_deref().is_none_or(|value| {
                    value.is_empty() || memory.metadata.status.to_string() == value
                }) && filters.technology.as_deref().is_none_or(|value| {
                    value.is_empty()
                        || serde_json::to_string(&memory.metadata.applies_to)
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                            .contains(&value.to_ascii_lowercase())
                })
            });
            format!(
                "{}{}{}",
                heading(
                    "Memories",
                    "Filter the durable source, not a shadow database."
                ),
                filter_form(),
                memory_rows(filtered)
            )
        }),
    )
}

async fn memory_detail(State(menvane): State<Arc<Menvane>>, Path(id): Path<Uuid>) -> Response {
    page_result("Memory", menvane.read(id).and_then(|memory| {
        let metadata = serde_yaml::to_string(&memory.metadata)?;
        Ok(format!("{}<div class='detail-grid'><article class='rendered'>{}</article><aside><p class='stamp'>{} · {} · {:.0}%</p><dl class='metadata'><dt>Sources</dt><dd>{}</dd><dt>Applies to</dt><dd>{}</dd><dt>Success / failure</dt><dd>{} / {}</dd><dt>Supersedes</dt><dd>{}</dd></dl></aside></div><details><summary>Raw Markdown and metadata</summary><pre>---\n{}---\n# {}\n\n{}</pre></details><form class='editor' method='post' action='/memories/{}/edit'><label>Title<input name='title' value='{}'></label><label>Markdown body<textarea name='body' rows='18'>{}</textarea></label><button>Commit manual edit</button></form>", heading(&memory.title, "Durable record detail"), render_markdown(&memory.body), memory.metadata.scope, memory.metadata.status, memory.metadata.confidence * 100.0, escape(&memory.metadata.source_sessions.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")), escape(&serde_json::to_string(&memory.metadata.applies_to)?), memory.metadata.successes.unwrap_or(0), memory.metadata.failures.unwrap_or(0), escape(&memory.metadata.supersedes.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")), escape(&metadata), escape(&memory.title), escape(&memory.body), id, escape_attribute(&memory.title), escape(&memory.body)))
    }))
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
        Err(error) => error_page(error),
    }
}

async fn procedures(State(menvane): State<Arc<Menvane>>) -> Response {
    page_result(
        "Procedures",
        menvane.all_memories().map(|memories| {
            let procedures = memories
                .iter()
                .filter(|memory| memory.metadata.memory_type == MemoryType::Procedure);
            format!(
                "{}{}",
                heading(
                    "Procedures",
                    "Candidates become dependable through evidence."
                ),
                memory_rows(procedures)
            )
        }),
    )
}

async fn sessions(State(menvane): State<Arc<Menvane>>) -> Response {
    page_result(
        "Sessions",
        menvane.all_memories().map(|memories| {
            let sessions = memories
                .iter()
                .filter(|memory| memory.metadata.memory_type == MemoryType::Session);
            format!(
                "{}{}",
                heading(
                    "Sessions",
                    "Live capture and imported evidence, kept episodic."
                ),
                memory_rows(sessions)
            )
        }),
    )
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
    page_result("Search", results.map(|results| {
        let rows = results.unwrap_or_default().iter().map(|memory| format!("<a class='search-hit' href='/memories/{}'><strong>{}</strong><span>{} · {} · {:.5}</span><p>{}</p><small>FTS rank {} · RRF K=60 · freshness {:.3} · final {:.5}</small></a>", memory.id, escape(&memory.title), memory.memory_type, memory.status, memory.score, escape(&memory.excerpt), memory.fts_rank, menvane_engine::DecayEngine::freshness(&memory.memory_type, memory.age_days), memory.score)).collect::<String>();
        format!("{}<form class='search' action='/search'><input name='q' value='{}' placeholder='Search historical context'><button>Search</button></form><section>{rows}</section>", heading("Search", "The same retrieval engine used by agents."), escape_attribute(query.q.as_deref().unwrap_or_default()))
    }))
}

async fn imports() -> Response {
    page(
        "Imports",
        format!(
            "{}<section class='callout'><p>Scan and import from the CLI while the daemon records status here.</p><pre>menvane import claude --dry-run\nmenvane import codex --dry-run\nmenvane import opencode --dry-run</pre><p>Unresolved project identities remain orphaned and are never guessed.</p></section>",
            heading("Imports", "Preview external evidence before consolidation.")
        ),
    )
}

async fn integrations() -> Response {
    page(
        "Integrations",
        format!(
            "{}<section class='metrics'>{}{}{}</section><pre>menvane connect claude\nmenvane connect codex\nmenvane connect opencode</pre>",
            heading("Integrations", "Three agents, one local memory plane."),
            metric("Claude", 1, "hook + MCP"),
            metric("Codex", 1, "hook + MCP"),
            metric("OpenCode", 1, "plugin + MCP")
        ),
    )
}

async fn providers(State(menvane): State<Arc<Menvane>>) -> Response {
    match menvane.provider_health().await {
        Ok((provider, model, health)) => page(
            "Providers",
            format!(
                "{}<dl class='metadata'><dt>Active provider</dt><dd>{}</dd><dt>Model</dt><dd>{}</dd><dt>Health</dt><dd>{:?}</dd><dt>Credentials</dt><dd>Environment or existing local authentication; never displayed</dd></dl>",
                heading("Providers", "Inference is isolated from retrieval."),
                escape(&provider),
                escape(&model),
                health
            ),
        ),
        Err(error) => error_page(error),
    }
}

async fn settings(State(menvane): State<Arc<Menvane>>) -> Response {
    page_result("Settings", menvane.configuration_text().map(|configuration| format!("{}<p class='callout'>Non-secret configuration lives at <code>MENVANE_HOME/config.toml</code>. Secret values are environment-only.</p><pre>{}</pre>", heading("Settings", "Observable runtime configuration."), escape(&configuration))))
}

async fn styles() -> impl IntoResponse {
    ([("content-type", "text/css; charset=utf-8")], CSS)
}

async fn script() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        "document.documentElement.classList.add('ready');",
    )
}

fn page_result(title: &str, content: anyhow::Result<String>) -> Response {
    match content {
        Ok(content) => page(title, content),
        Err(error) => error_page(error),
    }
}

fn page(title: &str, content: String) -> Response {
    Html(format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>{} · Menvane</title><link rel='stylesheet' href='/assets/menvane.css'><script defer src='/assets/menvane.js'></script></head><body><header><a class='brand' href='/'><i></i>MENVANE</a><nav><a href='/projects'>Projects</a><a href='/memories'>Memories</a><a href='/procedures'>Procedures</a><a href='/sessions'>Sessions</a><a href='/search'>Search</a><a href='/imports'>Imports</a><a href='/integrations'>Connections</a><a href='/providers'>Providers</a><a href='/settings'>Settings</a></nav></header><main>{content}</main><footer><span>Markdown is source</span><span>SQLite is index</span><span>Local by design</span></footer></body></html>", escape(title))).into_response()
}

fn error_page(error: impl std::fmt::Display) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Html(format!(
            "<h1>Menvane error</h1><pre>{}</pre>",
            escape(&error.to_string())
        )),
    )
        .into_response()
}

fn heading(title: &str, subtitle: &str) -> String {
    format!(
        "<section class='page-heading'><p class='kicker'>Memory ledger</p><h1>{}</h1><p>{}</p></section>",
        escape(title),
        escape(subtitle)
    )
}

fn metric(label: &str, value: usize, note: &str) -> String {
    format!(
        "<article class='metric'><span>{}</span><strong>{value:02}</strong><small>{}</small></article>",
        escape(label),
        escape(note)
    )
}

fn memory_rows<'a>(memories: impl Iterator<Item = &'a Memory>) -> String {
    format!("<section class='ledger'>{}</section>", memories.map(|memory| format!("<a class='ledger-row' href='/memories/{}'><span>{}</span><strong>{}</strong><small>{} · {:.0}%</small></a>", memory.metadata.id, memory.metadata.memory_type, escape(&memory.title), memory.metadata.status, memory.metadata.confidence * 100.0)).collect::<String>())
}

fn filter_form() -> String {
    "<form class='filters'><select name='scope'><option value=''>All scopes</option><option>project</option><option>global</option></select><select name='type'><option value=''>All types</option><option>fact</option><option>decision</option><option>procedure</option><option>gotcha</option><option>session</option></select><select name='status'><option value=''>All states</option><option>active</option><option>candidate</option><option>needs-validation</option><option>superseded</option><option>historical</option></select><input name='technology' placeholder='technology'><button>Apply</button></form>".to_owned()
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

const CSS: &str = r#":root{--ink:#171612;--paper:#ebe6d8;--red:#d9432f;--muted:#817b6c;--line:#b8b09d}*{box-sizing:border-box}html{background:var(--ink);color:var(--ink);font-family:Georgia,'Times New Roman',serif}body{margin:0;background:var(--paper);min-height:100vh;background-image:linear-gradient(90deg,transparent 49.8%,rgba(23,22,18,.035) 50%,transparent 50.2%)}header{min-height:92px;border-bottom:1px solid var(--ink);display:flex;align-items:stretch}.brand{width:24%;background:var(--ink);color:var(--paper);display:flex;align-items:center;padding:0 3vw;text-decoration:none;letter-spacing:.22em;font:700 14px ui-monospace,monospace}.brand i{width:10px;height:10px;background:var(--red);border-radius:50%;margin-right:14px}nav{display:flex;align-items:center;gap:1.4vw;padding:0 2vw;overflow-x:auto}nav a{color:var(--ink);text-decoration:none;text-transform:uppercase;font:600 10px ui-monospace,monospace;letter-spacing:.09em}nav a:hover{color:var(--red)}main{max-width:1400px;margin:auto;padding:5vw}.hero{min-height:54vh;display:grid;grid-template-columns:1fr 1fr;align-content:center}.hero h1,.page-heading h1{font-weight:400;font-size:clamp(54px,8vw,128px);line-height:.82;letter-spacing:-.06em;margin:.25em 0}.hero .lede{align-self:end;max-width:380px;font-size:20px;border-left:4px solid var(--red);padding-left:22px}.kicker{text-transform:uppercase;letter-spacing:.18em;color:var(--red);font:700 11px ui-monospace,monospace}.metrics{display:grid;grid-template-columns:repeat(6,1fr);border:1px solid var(--ink);margin:3rem 0}.metric{min-height:170px;padding:20px;border-right:1px solid var(--ink);display:flex;flex-direction:column}.metric:last-child{border:0}.metric span,.metric small{font:11px ui-monospace,monospace;text-transform:uppercase}.metric strong{font-weight:400;font-size:56px;margin:auto 0}.rule,.ledger-row{display:grid;grid-template-columns:150px 1fr 180px;padding:18px 0;border-bottom:1px solid var(--line)}.ledger-row{color:var(--ink);text-decoration:none;transition:padding .18s,background .18s}.ledger-row:hover{padding-left:12px;background:rgba(217,67,47,.07)}.ledger-row span,.ledger-row small{font:11px ui-monospace,monospace;text-transform:uppercase}.ledger-row strong{font-size:19px;font-weight:400}.page-heading{border-bottom:5px solid var(--ink);margin-bottom:3rem;padding-bottom:2rem}.page-heading h1{font-size:clamp(48px,7vw,100px)}.filters,.search{display:flex;gap:8px;margin:2rem 0}.filters input,.filters select,.search input,.editor input,.editor textarea{background:transparent;border:1px solid var(--ink);padding:13px;color:var(--ink);font:14px ui-monospace,monospace}.search input{flex:1;font-size:20px}button{background:var(--red);color:white;border:0;padding:13px 22px;text-transform:uppercase;font:700 11px ui-monospace,monospace;cursor:pointer}.detail-grid{display:grid;grid-template-columns:2fr 1fr;gap:8vw}.rendered{font-size:19px;line-height:1.7}.rendered h2{font-size:34px;margin-top:2em}.stamp{color:var(--red);font:700 11px ui-monospace,monospace;text-transform:uppercase}.metadata{border-top:1px solid var(--ink)}.metadata dt{color:var(--muted);font:10px ui-monospace,monospace;text-transform:uppercase;margin-top:18px}.metadata dd{margin:5px 0;overflow-wrap:anywhere}pre{background:var(--ink);color:var(--paper);padding:24px;overflow:auto;font:12px/1.6 ui-monospace,monospace}.editor{display:grid;gap:16px;margin-top:3rem}.editor label{display:grid;gap:8px;font:11px ui-monospace,monospace;text-transform:uppercase}.search-hit{display:block;color:var(--ink);text-decoration:none;border-top:1px solid var(--ink);padding:24px 0}.search-hit strong{font-size:25px}.search-hit span,.search-hit small{float:right;font:10px ui-monospace,monospace}.callout{border-left:5px solid var(--red);padding:1px 24px;margin:3rem 0}footer{background:var(--ink);color:var(--paper);padding:28px 5vw;display:flex;justify-content:space-between;font:10px ui-monospace,monospace;text-transform:uppercase}.ready main{animation:arrive .45s ease-out both}@keyframes arrive{from{opacity:0;transform:translateY(12px)}}@media(max-width:800px){header{display:block}.brand{width:100%;height:58px}nav{height:50px}.hero{grid-template-columns:1fr}.metrics{grid-template-columns:repeat(2,1fr)}.metric:nth-child(2n){border-right:0}.rule,.ledger-row{grid-template-columns:90px 1fr}.ledger-row small{display:none}.detail-grid{grid-template-columns:1fr}.filters{display:grid}.search-hit span,.search-hit small{float:none;display:block;margin-top:8px}main{padding:9vw 5vw}.hero h1{font-size:18vw}}"#;
