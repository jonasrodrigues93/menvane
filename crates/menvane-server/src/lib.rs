use std::fs::{self, File, OpenOptions};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use fs2::FileExt;
use menvane_domain::NormalizedEvent;
use menvane_engine::{CaptureOutcome, Menvane};
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_ADDRESS: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 47_831;

pub async fn serve(menvane: Menvane, address: &str, port: u16) -> Result<()> {
    let home = menvane.home().to_path_buf();
    let lock = acquire_lock(&home)?;
    fs::write(home.join("daemon.pid"), std::process::id().to_string())?;
    let state = Arc::new(menvane);
    let maintenance = Arc::clone(&state);
    let maintenance_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let _ = maintenance.finalize_idle_sessions();
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
        CaptureOutcome::Finalized => "finalized",
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
                    "last_error": job.last_error
                })
            })
            .collect(),
    )))
}

#[derive(Deserialize)]
struct RecallRequest {
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
    let cwd = std::path::Path::new(&request.cwd);
    let context = match request.kind.as_str() {
        "session-start" => menvane.session_briefing(cwd, &request.session_id),
        "user-prompt" => menvane.prompt_context(cwd, &request.prompt, &request.session_id),
        _ => Err(anyhow::anyhow!("unsupported recall kind: {}", request.kind)),
    }
    .map_err(internal_server_error)?;
    Ok(Json(json!({ "context": context })))
}

fn internal_server_error(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
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
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn duplicate_event_ingestion_is_idempotent() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        fs::create_dir_all(&project).unwrap();
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
}
