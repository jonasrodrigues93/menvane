use std::fs::{self, File, OpenOptions};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use fs2::FileExt;
use menvane_domain::{HandoffStatus, NormalizedEvent};
use menvane_engine::{
    CaptureOutcome, MAX_HANDOFF_ITEM_BYTES, MAX_HANDOFF_LIST_LIMIT, MAX_RECALL_CWD_BYTES,
    MAX_RECALL_IDENTIFIER_BYTES, Menvane,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

mod ui;

pub const DEFAULT_ADDRESS: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 47_831;

pub async fn serve(menvane: Menvane, address: &str, port: u16) -> Result<()> {
    let home = menvane.home().to_path_buf();
    let lock = acquire_lock(&home)?;
    fs::write(home.join("daemon.pid"), std::process::id().to_string())?;
    let state = Arc::new(menvane);
    let maintenance = Arc::clone(&state);
    let worker = Arc::clone(&maintenance);
    let maintenance_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let _ = worker.finalize_idle_sessions();
            let _ = worker.process_next_job().await;
        }
    });
    let socket: SocketAddr = format!("{address}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(socket).await?;
    let result = axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    maintenance_task.abort();
    let _ = maintenance.flush_dirty_checkpoints().await;
    let _ = fs::remove_file(home.join("daemon.pid"));
    drop(lock);
    result.map_err(Into::into)
}

pub fn app(state: Arc<Menvane>) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/events", post(ingest_event))
        .route("/api/v1/recall", post(recall))
        .route("/api/v1/jobs", get(jobs))
        .route("/api/v1/projects", get(api_projects))
        .route("/api/v1/memories", get(api_memories))
        .route("/api/v1/sessions", get(api_sessions))
        .route("/api/v1/handoffs", get(api_handoffs))
        .route("/api/v1/handoffs/{id}", get(api_handoff_detail))
        .route("/api/v1/handoffs/{id}/consume", post(api_handoff_consume))
        .route("/api/v1/handoffs/{id}/complete", post(api_handoff_complete))
        .route(
            "/api/v1/handoffs/{id}/supersede",
            post(api_handoff_supersede),
        )
        .route("/api/v1/imports", get(api_imports))
        .route("/api/v1/integrations", get(api_integrations))
        .route("/api/v1/settings", get(api_settings))
        .route("/api/v1/providers", get(api_providers))
        .route("/api/v1/search", get(api_search))
        .merge(ui::router())
        .with_state(state)
}

pub fn daemon_running(home: &std::path::Path) -> bool {
    let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) else {
        return false;
    };
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .is_ok_and(|status| status.success())
}

pub fn start_daemon(home: &std::path::Path, executable: &std::path::Path) -> Result<u32> {
    if daemon_running(home) {
        anyhow::bail!("Menvane daemon is already running");
    }
    fs::create_dir_all(home.join("logs"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("logs/daemon.log"))?;
    let child = std::process::Command::new(executable)
        .arg("serve")
        .env("MENVANE_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    Ok(child.id())
}

pub fn stop_daemon(home: &std::path::Path) -> Result<()> {
    let pid = fs::read_to_string(home.join("daemon.pid")).context("daemon is not running")?;
    let status = std::process::Command::new("kill")
        .arg(pid.trim())
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to stop daemon process {}", pid.trim());
    }
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn ingest_event(
    State(menvane): State<Arc<Menvane>>,
    Json(event): Json<NormalizedEvent>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let outcome = menvane.ingest_event(event).map_err(internal_server_error)?;
    let outcome = match outcome {
        CaptureOutcome::Dropped => "dropped",
        CaptureOutcome::Duplicate => "duplicate",
        CaptureOutcome::Stored => "stored",
    };
    Ok(Json(json!({ "outcome": outcome })))
}

async fn jobs(
    State(menvane): State<Arc<Menvane>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let jobs = menvane.jobs().map_err(internal_server_error)?;
    Ok(Json(Value::Array(
        jobs.into_iter()
            .map(|job| {
                json!({
                    "id": job.id,
                    "type": job.job_type,
                    "status": job.status,
                    "attempt_count": job.attempt_count,
                    "next_retry_at": job.next_retry_at,
                    "last_error": job.last_error,
                    "owner": job.owner,
                    "lease_started_at": job.lease_started_at,
                    "lease_until": job.lease_until
                })
            })
            .collect(),
    )))
}

async fn api_projects(
    State(menvane): State<Arc<Menvane>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Ok(Json(
        serde_json::to_value(menvane.all_projects().map_err(internal_server_error)?)
            .map_err(|error| internal_server_error(error.into()))?,
    ))
}

async fn api_memories(
    State(menvane): State<Arc<Menvane>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let memories = menvane.all_memories().map_err(internal_server_error)?;
    Ok(Json(Value::Array(memories.into_iter().map(|memory| json!({ "metadata": memory.metadata, "title": memory.title, "body": memory.body })).collect())))
}

async fn api_sessions(
    State(menvane): State<Arc<Menvane>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let memories = menvane.all_memories().map_err(internal_server_error)?;
    Ok(Json(Value::Array(memories.into_iter().filter(|memory| memory.metadata.memory_type == menvane_domain::MemoryType::Session).map(|memory| json!({ "metadata": memory.metadata, "title": memory.title, "body": memory.body })).collect())))
}

#[derive(Default, Deserialize)]
struct ApiHandoffsQuery {
    scope: Option<String>,
    project_id: Option<String>,
    session_id: Option<String>,
    project: Option<String>,
    session: Option<String>,
    status: Option<String>,
    #[serde(default = "default_handoff_limit")]
    limit: usize,
}

fn default_handoff_limit() -> usize {
    25
}

async fn api_handoffs(
    State(menvane): State<Arc<Menvane>>,
    Query(query): Query<ApiHandoffsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = validate_handoff_limit(query.limit)?;
    let status = parse_handoff_status(query.status.as_deref())?;
    let project_id = query.project_id.or(query.project);
    let session_id = query.session_id.or(query.session);
    let scope = query
        .scope
        .as_deref()
        .unwrap_or(match (&project_id, &session_id) {
            (Some(_), None) => "project",
            (None, Some(_)) => "session",
            (None, None) => "all",
            (Some(_), Some(_)) => "invalid",
        });
    if let Some(project_id) = project_id.as_deref() {
        valid_handoff_selector(project_id, "project_id")?;
    }
    let handoffs = match scope {
        "all" if project_id.is_none() && session_id.is_none() => {
            menvane.all_handoffs(status, limit)
        }
        "project" if project_id.is_some() && session_id.is_none() => {
            menvane.project_handoffs(project_id.as_deref().unwrap(), status, limit)
        }
        "session" if session_id.is_some() && project_id.is_none() => {
            let id = parse_uuid(session_id.as_deref().unwrap(), "session_id")?;
            menvane.session_handoffs(id, status, limit)
        }
        _ => {
            return Err(bad_request(
                "handoff scope must be all, project, or session with one matching selector"
                    .to_owned(),
            ));
        }
    }
    .map_err(internal_server_error)?;
    Ok(Json(Value::Array(
        handoffs.into_iter().map(handoff_summary).collect(),
    )))
}

fn handoff_summary(handoff: menvane_domain::TaskHandoff) -> Value {
    json!({
        "id": handoff.id,
        "project_id": handoff.project_id,
        "conversation_key": handoff.conversation_key,
        "episode_id": handoff.episode_id,
        "source_session_id": handoff.source_session_id,
        "source_client": handoff.source_client,
        "status": handoff.status,
        "goal": bounded_api_text(&handoff.goal, 512),
        "next_action": handoff.next_action.as_deref().map(|value| bounded_api_text(value, 512)),
        "blocker_count": handoff.blockers.len(),
        "changed_files": handoff.changed_files.into_iter().take(8).collect::<Vec<_>>(),
        "fingerprint": {
            "git_head": handoff.git_head,
            "worktree_state_hash": handoff.worktree_state_hash,
        },
        "updated_at": handoff.updated_at,
    })
}

fn bounded_api_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

async fn api_handoff_detail(
    State(menvane): State<Arc<Menvane>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let id = parse_uuid(&id, "handoff id")?;
    let detail = menvane
        .handoff_detail(id)
        .map_err(internal_server_error)?
        .ok_or_else(|| not_found(format!("handoff {id} not found")))?;
    Ok(Json(
        serde_json::to_value(detail).map_err(|error| internal_server_error(error.into()))?,
    ))
}

async fn api_handoff_consume(
    State(menvane): State<Arc<Menvane>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle_handoff(&menvane, &id, |menvane, id| menvane.consume_handoff(id)).await
}

async fn api_handoff_complete(
    State(menvane): State<Arc<Menvane>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle_handoff(&menvane, &id, |menvane, id| menvane.complete_handoff(id)).await
}

async fn api_handoff_supersede(
    State(menvane): State<Arc<Menvane>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle_handoff(&menvane, &id, |menvane, id| menvane.supersede_handoff(id)).await
}

async fn lifecycle_handoff(
    menvane: &Menvane,
    raw_id: &str,
    action: impl FnOnce(&Menvane, Uuid) -> anyhow::Result<menvane_domain::TaskHandoff>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let id = parse_uuid(raw_id, "handoff id")?;
    if menvane
        .handoff_detail(id)
        .map_err(internal_server_error)?
        .is_none()
    {
        return Err(not_found(format!("handoff {id} not found")));
    }
    let handoff = action(menvane, id).map_err(internal_server_error)?;
    Ok(Json(json!({ "handoff": handoff })))
}

fn validate_handoff_limit(limit: usize) -> Result<usize, (StatusCode, Json<Value>)> {
    if limit == 0 || limit > MAX_HANDOFF_LIST_LIMIT {
        return Err(bad_request(format!(
            "handoff limit must be between 1 and {MAX_HANDOFF_LIST_LIMIT}"
        )));
    }
    Ok(limit)
}

fn parse_handoff_status(
    value: Option<&str>,
) -> Result<Option<HandoffStatus>, (StatusCode, Json<Value>)> {
    let Some(value) = value else {
        return Ok(None);
    };
    let status = match value {
        "active" => HandoffStatus::Active,
        "ready" => HandoffStatus::Ready,
        "consumed" => HandoffStatus::Consumed,
        "completed" => HandoffStatus::Completed,
        "stale" => HandoffStatus::Stale,
        "superseded" => HandoffStatus::Superseded,
        _ => return Err(bad_request(format!("unsupported handoff status: {value}"))),
    };
    Ok(Some(status))
}

fn parse_uuid(value: &str, name: &str) -> Result<Uuid, (StatusCode, Json<Value>)> {
    Uuid::parse_str(value).map_err(|_| bad_request(format!("{name} must be a valid UUID")))
}

fn valid_handoff_selector(value: &str, name: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if value.trim().is_empty() || value.len() > MAX_HANDOFF_ITEM_BYTES || value.contains('\0') {
        return Err(bad_request(format!("{name} is invalid or too large")));
    }
    Ok(())
}

async fn api_imports() -> Json<Value> {
    Json(
        json!({ "clients": ["claude", "codex", "opencode"], "dry_run": true, "orphans": "retained" }),
    )
}

async fn api_integrations() -> Json<Value> {
    Json(json!({ "clients": ["claude", "codex", "opencode"], "mcp": "stdio" }))
}

async fn api_settings(
    State(menvane): State<Arc<Menvane>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Ok(Json(
        json!({ "toml": menvane.configuration_text().map_err(internal_server_error)? }),
    ))
}

async fn api_providers(
    State(menvane): State<Arc<Menvane>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (provider, model, health) = menvane
        .provider_health()
        .await
        .map_err(internal_server_error)?;
    Ok(Json(
        json!({ "provider": provider, "model": model, "health": health }),
    ))
}

#[derive(Deserialize)]
struct ApiSearchQuery {
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    10
}

async fn api_search(
    State(menvane): State<Arc<Menvane>>,
    Query(query): Query<ApiSearchQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let cwd = std::env::current_dir().map_err(|error| internal_server_error(error.into()))?;
    let results = menvane
        .search(
            &cwd,
            &query.query,
            menvane_engine::ScopeSelection::Auto,
            query.limit,
        )
        .map_err(internal_server_error)?;
    Ok(Json(Value::Array(
        results
            .into_iter()
            .map(|memory| {
                json!({
                    "id": memory.id,
                    "type": memory.memory_type,
                    "scope": memory.scope,
                    "title": memory.title,
                    "status": memory.status,
                    "confidence": memory.confidence,
                    "applicability": memory.applicability,
                    "score": memory.score,
                    "fts_rank": memory.fts_rank,
                    "age_days": memory.age_days
                })
            })
            .collect(),
    )))
}

#[derive(Deserialize)]
struct RecallRequest {
    client: String,
    cwd: String,
    session_id: String,
    kind: String,
    #[serde(default)]
    prompt: String,
}

async fn recall(
    State(menvane): State<Arc<Menvane>>,
    Json(request): Json<RecallRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_recall_request(&request)?;
    let cwd = std::path::Path::new(&request.cwd);
    let (context, diagnostics) = match request.kind.as_str() {
        "session-start" => (
            menvane
                .session_briefing_for_client(cwd, &request.client, &request.session_id)
                .map_err(internal_server_error)?,
            None,
        ),
        "user-prompt" => {
            let (context, diagnostics) = menvane
                .prompt_context_for_client(
                    cwd,
                    &request.client,
                    &request.session_id,
                    &request.prompt,
                )
                .map_err(internal_server_error)?;
            (context, Some(diagnostics))
        }
        _ => {
            return Err(bad_request(format!(
                "unsupported recall kind: {}",
                request.kind
            )));
        }
    };
    Ok(Json(
        json!({ "context": context, "diagnostics": diagnostics }),
    ))
}

fn validate_recall_request(request: &RecallRequest) -> Result<(), (StatusCode, Json<Value>)> {
    for (name, value, max_bytes) in [
        (
            "client",
            request.client.as_str(),
            MAX_RECALL_IDENTIFIER_BYTES,
        ),
        (
            "session_id",
            request.session_id.as_str(),
            MAX_RECALL_IDENTIFIER_BYTES,
        ),
        ("cwd", request.cwd.as_str(), MAX_RECALL_CWD_BYTES),
    ] {
        if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
            return Err(bad_request(format!(
                "recall {name} is invalid or too large"
            )));
        }
    }
    Ok(())
}

fn internal_server_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}

fn bad_request(message: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn not_found(message: String) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message })))
}

fn acquire_lock(home: &std::path::Path) -> Result<File> {
    fs::create_dir_all(home)?;
    let path = home.join("daemon.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    file.try_lock_exclusive()
        .with_context(|| format!("another daemon owns {}", path.display()))?;
    Ok(file)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

pub fn home_from_environment() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("MENVANE_HOME") {
        return Ok(PathBuf::from(home));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".menvane"))
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use chrono::Utc;
    use menvane_domain::NormalizedEventKind;
    use menvane_domain::{Applicability, MemoryType, Scope};
    use menvane_engine::WriteMemory;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn duplicate_event_ingestion_is_idempotent() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["init", "--quiet"])
            .status()
            .unwrap();
        assert!(status.success());
        let state = Arc::new(Menvane::new(temporary.path().join("home")).unwrap());
        let event = NormalizedEvent {
            event_id: "stable-event-id".to_owned(),
            kind: NormalizedEventKind::SessionStarted,
            client: "test-client".to_owned(),
            external_session_id: "external-session".to_owned(),
            timestamp: Utc::now(),
            cwd: project.to_string_lossy().into_owned(),
            project_id: None,
            tool_family: None,
            bounded_input: None,
            bounded_output: None,
            attributed_path: None,
            success: None,
            model: None,
        };
        let router = app(state);
        let first = post_event(router.clone(), &event).await;
        let second = post_event(router, &event).await;
        assert_eq!(first["outcome"], "stored");
        assert_eq!(second["outcome"], "duplicate");
    }

    #[tokio::test]
    async fn recall_delivery_dedupes_session_start_and_prompt_independently() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["init", "--quiet"])
            .status()
            .unwrap();
        assert!(status.success());
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        menvane
            .write(
                &project,
                WriteMemory {
                    title: "Session briefing decision".to_owned(),
                    body: "Session briefing marker".to_owned(),
                    memory_type: MemoryType::Decision,
                    scope: Scope::Project,
                    confidence: 1.0,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .unwrap();
        menvane
            .write(
                &project,
                WriteMemory {
                    title: "Prompt recall fact".to_owned(),
                    body: "Prompt recall marker".to_owned(),
                    memory_type: MemoryType::Fact,
                    scope: Scope::Project,
                    confidence: 1.0,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .unwrap();
        let state = Arc::new(menvane);
        let router = app(state);
        let event = NormalizedEvent {
            event_id: "server-session-start".to_owned(),
            kind: NormalizedEventKind::SessionStarted,
            client: "claude-code".to_owned(),
            external_session_id: "shared-session".to_owned(),
            timestamp: Utc::now(),
            cwd: project.to_string_lossy().into_owned(),
            project_id: None,
            tool_family: None,
            bounded_input: None,
            bounded_output: None,
            attributed_path: None,
            success: None,
            model: None,
        };
        post_event(router.clone(), &event).await;
        let request = serde_json::json!({
            "client": "claude-code",
            "cwd": project.to_string_lossy().into_owned(),
            "session_id": "shared-session",
            "kind": "session-start",
            "prompt": ""
        });
        let first_briefing = post_recall(router.clone(), request.clone()).await;
        let repeated_briefing = post_recall(router.clone(), request).await;
        let request = serde_json::json!({
            "client": "claude-code",
            "cwd": project.to_string_lossy().into_owned(),
            "session_id": "shared-session",
            "kind": "user-prompt",
            "prompt": "Prompt recall marker"
        });
        let first_prompt = post_recall(router.clone(), request.clone()).await;
        let repeated_prompt = post_recall(router, request).await;
        assert!(
            first_briefing["context"]
                .as_str()
                .unwrap()
                .contains("Session briefing decision")
        );
        assert_eq!(repeated_briefing["context"], "");
        assert!(
            first_prompt["context"]
                .as_str()
                .unwrap()
                .contains("Prompt recall fact")
        );
        assert_eq!(repeated_prompt["context"], "");
    }

    #[tokio::test]
    async fn mandatory_ui_views_and_admin_edit_are_functional() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["init", "--quiet"])
            .status()
            .unwrap();
        assert!(status.success());
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let memory = menvane
            .write(
                &project,
                WriteMemory {
                    title: "UI inspection memory".to_owned(),
                    body: "Visible durable evidence".to_owned(),
                    memory_type: MemoryType::Fact,
                    scope: Scope::Project,
                    confidence: 1.0,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .unwrap();
        let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
        let router = app(Arc::new(menvane));
        let project_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let project_body = to_bytes(project_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let project_body = String::from_utf8(project_body.to_vec()).unwrap();
        assert!(project_body.contains("Handoffs"));
        assert!(project_body.contains("handoff-surface"));
        for path in [
            "/",
            "/projects",
            &format!("/projects/{project_id}"),
            "/memories",
            &format!("/memories/{}", memory.metadata.id),
            "/procedures",
            "/sessions",
            "/search",
            "/imports",
            "/integrations",
            "/providers",
            "/settings",
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
        let body = "title=Edited+memory&body=Immediately+indexed+content";
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/memories/{}/edit", memory.metadata.id))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn recall_rejects_oversized_identifiers_and_cwd() {
        let temporary = TempDir::new().unwrap();
        let state = Arc::new(Menvane::new(temporary.path().join("home")).unwrap());
        let router = app(state);
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/recall")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "client": "c".repeat(MAX_RECALL_IDENTIFIER_BYTES + 1),
                    "cwd": "/tmp",
                    "session_id": "session",
                    "kind": "user-prompt",
                    "prompt": "bounded"
                })
                .to_string(),
            ))
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/recall")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "client": "client",
                    "cwd": "/".to_owned() + &"c".repeat(MAX_RECALL_CWD_BYTES),
                    "session_id": "session",
                    "kind": "user-prompt",
                    "prompt": "bounded"
                })
                .to_string(),
            ))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn handoff_rest_lists_details_validates_and_runs_lifecycle() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["init", "--quiet"])
            .status()
            .unwrap();
        assert!(status.success());
        let menvane = Arc::new(Menvane::new(temporary.path().join("home")).unwrap());
        post_event(
            app(Arc::clone(&menvane)),
            &test_event(
                &project,
                "handoff-start",
                NormalizedEventKind::SessionStarted,
                None,
            ),
        )
        .await;
        post_event(
            app(Arc::clone(&menvane)),
            &test_event(
                &project,
                "handoff-prompt",
                NormalizedEventKind::UserPrompt,
                Some("REST handoff surface"),
            ),
        )
        .await;
        let mut tool = test_event(
            &project,
            "handoff-tool",
            NormalizedEventKind::ToolCompleted,
            Some("cargo test"),
        );
        tool.tool_family = Some("cargo test".to_owned());
        tool.attributed_path = Some("src/lib.rs".to_owned());
        tool.success = Some(true);
        post_event(app(Arc::clone(&menvane)), &tool).await;
        assert!(menvane.process_next_job().await.unwrap());
        let handoff = menvane.all_handoffs(None, 10).unwrap().remove(0);
        let project_id = handoff.project_id.clone().unwrap();
        let session_id = handoff.source_session_id;
        let router = app(Arc::clone(&menvane));

        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs?scope=project&project_id=invalid-project&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let bytes = to_bytes(list.into_body(), usize::MAX).await.unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(list.as_array().unwrap().is_empty());
        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs?scope=all&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let bytes = to_bytes(list.into_body(), usize::MAX).await.unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/handoffs?scope=session&session_id={session_id}&status=active"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let bytes = to_bytes(list.into_body(), usize::MAX).await.unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        let invalid_status = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs?status=unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_status.status(), StatusCode::BAD_REQUEST);

        let detail = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/handoffs/{}", handoff.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        let bytes = to_bytes(detail.into_body(), usize::MAX).await.unwrap();
        let detail: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail["handoff"]["id"], handoff.id.to_string());
        assert!(detail["versions"].is_array());
        assert!(detail["evidence"].is_array());

        let invalid = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let invalid_limit = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs?limit=101")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_limit.status(), StatusCode::BAD_REQUEST);
        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/handoffs/{}", Uuid::now_v7()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        for action in ["consume", "complete"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/handoffs/{}/{}", handoff.id, action))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{action}");
        }
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/handoffs/{}/supersede", handoff.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(project_id, handoff.project_id.unwrap());
    }

    fn test_event(
        project: &std::path::Path,
        event_id: &str,
        kind: NormalizedEventKind,
        input: Option<&str>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            event_id: event_id.to_owned(),
            kind,
            client: "server-test".to_owned(),
            external_session_id: "server-session".to_owned(),
            timestamp: Utc::now(),
            cwd: project.to_string_lossy().into_owned(),
            project_id: None,
            tool_family: None,
            bounded_input: input.map(str::to_owned),
            bounded_output: None,
            attributed_path: None,
            success: None,
            model: None,
        }
    }

    async fn post_event(router: Router, event: &NormalizedEvent) -> Value {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/events")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(event).unwrap()))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn post_recall(router: Router, payload: Value) -> Value {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/recall")
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}
