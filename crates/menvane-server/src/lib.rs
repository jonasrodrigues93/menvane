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
use menvane_domain::{NormalizedEvent, ProjectHandoff};
use menvane_engine::{
    CaptureOutcome, MAX_HANDOFF_ITEM_BYTES, MAX_RECALL_CWD_BYTES, MAX_RECALL_IDENTIFIER_BYTES,
    Menvane,
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
        .route("/api/v1/handoffs/{project_id}", get(api_handoff_detail))
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
}

async fn api_handoffs(
    State(menvane): State<Arc<Menvane>>,
    Query(query): Query<ApiHandoffsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
    if !matches!(scope, "all" | "project" | "session") {
        return Err(bad_request(
            "handoff scope must be all, project, or session with one matching selector".to_owned(),
        ));
    }
    let session_uuid = match scope {
        "session" => Some(parse_uuid(session_id.as_deref().unwrap(), "session_id")?),
        _ => None,
    };
    let handoffs: Vec<ProjectHandoff> = (|| -> Result<Vec<ProjectHandoff>> {
        match scope {
            "all" => menvane.all_project_handoffs(),
            "project" => {
                let project_id = project_id.as_deref().unwrap();
                Ok(menvane
                    .current_project_handoff(Some(project_id))?
                    .into_iter()
                    .collect())
            }
            "session" => Ok(menvane
                .session_project_handoff(session_uuid.unwrap())?
                .into_iter()
                .collect()),
            _ => unreachable!(),
        }
    })()
    .map_err(internal_server_error)?;
    Ok(Json(Value::Array(
        handoffs.into_iter().map(handoff_payload).collect(),
    )))
}

fn handoff_payload(handoff: ProjectHandoff) -> Value {
    json!({
        "project_id": handoff.project_id,
        "summary": handoff.summary,
        "source_session_ids": handoff.source_session_ids,
        "fingerprint": handoff.fingerprint,
        "created_at": handoff.created_at,
        "updated_at": handoff.updated_at,
    })
}

async fn api_handoff_detail(
    State(menvane): State<Arc<Menvane>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    valid_handoff_selector(&project_id, "project_id")?;
    let handoff = menvane
        .current_project_handoff(Some(&project_id))
        .map_err(internal_server_error)?
        .ok_or_else(|| not_found(format!("handoff for project {project_id} not found")))?;
    Ok(Json(handoff_payload(handoff)))
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
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
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
    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use chrono::Utc;
    use menvane_domain::NormalizedEventKind;
    use menvane_domain::{
        Applicability, JsonSchema, LlmError, LlmProvider, LlmRequest, MemoryType,
        ProviderCapabilities, ProviderHealth, Scope, StructuredResponse,
    };
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
            origin: Default::default(),
            role: Default::default(),
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
            harness_injected: false,
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
            origin: Default::default(),
            role: Default::default(),
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
            harness_injected: false,
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
        assert!(project_body.contains("Handoff"));
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
    async fn handoff_rest_returns_one_current_summary_per_project() {
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
        let menvane = Arc::new(
            Menvane::new_with_provider(
                temporary.path().join("home"),
                Arc::new(ServerHandoffProvider),
            )
            .unwrap(),
        );
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
        post_event(
            app(Arc::clone(&menvane)),
            &test_event(
                &project,
                "handoff-end",
                NormalizedEventKind::SessionEnded,
                None,
            ),
        )
        .await;
        assert!(menvane.process_next_job().await.unwrap());
        assert!(menvane.process_next_job().await.unwrap());
        let project_id = menvane.ensure_project(&project).unwrap().unwrap().id;
        let session_id = menvane
            .all_memories()
            .unwrap()
            .into_iter()
            .find(|memory| memory.metadata.memory_type == menvane_domain::MemoryType::Session)
            .unwrap()
            .metadata
            .id;
        let router = app(Arc::clone(&menvane));

        for uri in [
            "/api/v1/handoffs?scope=project&project_id=invalid-project",
            format!("/api/v1/handoffs?scope=project&project_id={project_id}").as_str(),
            format!("/api/v1/handoffs?scope=session&session_id={session_id}").as_str(),
            "/api/v1/handoffs?scope=all",
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }

        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs?scope=project&project_id=invalid-project")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(list.into_body(), usize::MAX).await.unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(list.as_array().unwrap().is_empty());

        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs?scope=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(list.into_body(), usize::MAX).await.unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(
            list.as_array().unwrap()[0]["summary"],
            "server test handoff summary"
        );

        let detail = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/handoffs/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        let bytes = to_bytes(detail.into_body(), usize::MAX).await.unwrap();
        let detail: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail["project_id"], project_id);
        assert_eq!(detail["summary"], "server test handoff summary");

        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/handoffs/missing-project")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        for uri in [
            "/api/v1/handoffs?scope=all&limit=101",
            "/api/v1/handoffs?scope=all&status=unknown",
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }
    }

    struct ServerHandoffProvider;

    #[async_trait]
    impl LlmProvider for ServerHandoffProvider {
        async fn generate_structured(
            &self,
            request: LlmRequest,
            _schema: JsonSchema,
        ) -> Result<StructuredResponse, LlmError> {
            let input: Value = serde_json::from_str(&request.prompt).unwrap();
            let session_id = input["session"]["session_id"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned();
            let events = input["session"]["events"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let ids = events
                .iter()
                .filter_map(|event| event["event_id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            Ok(StructuredResponse {
                value: json!({
                    "goals": [],
                    "memories": [],
                    "handoff": {
                        "summary": "server test handoff summary",
                        "source_session_ids": [session_id],
                        "evidence_event_ids": ids
                    }
                }),
                provider: "server-test".to_owned(),
                model: "test".to_owned(),
            })
        }

        async fn health(&self) -> ProviderHealth {
            ProviderHealth::Ready
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                structured_output: true,
                json_schema: true,
                embeddings: false,
            }
        }

        fn name(&self) -> &'static str {
            "server-test"
        }

        fn model(&self) -> &str {
            "test"
        }
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
            origin: Default::default(),
            role: Default::default(),
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
            harness_injected: false,
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
