use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use menvane_domain::{
    JsonSchema, LlmError, LlmErrorKind, LlmProvider, LlmRequest, ProviderCapabilities,
    ProviderHealth, StructuredResponse,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::Command;

pub struct CodexProvider {
    binary: PathBuf,
    model: String,
}

impl CodexProvider {
    pub fn new(binary: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            model: model.into(),
        }
    }

    async fn command_output(&self, arguments: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new(&self.binary).args(arguments).output().await
    }
}

#[async_trait]
impl LlmProvider for CodexProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        let temporary = TempDir::new().map_err(internal)?;
        let schema_path = temporary.path().join("schema.json");
        let output_path = temporary.path().join("response.json");
        std::fs::write(
            &schema_path,
            serde_json::to_vec_pretty(&schema.0).map_err(invalid_schema)?,
        )
        .map_err(internal)?;
        let mut command = Command::new(&self.binary);
        command
            .args([
                "exec",
                "-C",
                temporary.path().to_string_lossy().as_ref(),
                "--skip-git-repo-check",
                "--ignore-user-config",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "--disable",
                "shell_tool",
                "--disable",
                "apps",
                "--disable",
                "plugins",
                "--disable",
                "multi_agent",
                "--disable",
                "hooks",
                "-c",
                "web_search=\"disabled\"",
                "--output-schema",
                schema_path.to_string_lossy().as_ref(),
                "--output-last-message",
                output_path.to_string_lossy().as_ref(),
            ])
            .env("MENVANE_INTERNAL", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        if self.model != "default" {
            command.args(["--model", &self.model]);
        }
        command.arg(format!("{}\n\n{}", request.system, request.prompt));
        let output = tokio::time::timeout(request.timeout, command.output())
            .await
            .map_err(|_| unavailable("Codex inference timed out"))?
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    unavailable("Codex binary is missing")
                } else {
                    internal(error)
                }
            })?;
        if !output.status.success() {
            return Err(classify_codex_error(&String::from_utf8_lossy(
                &output.stderr,
            )));
        }
        let bytes = std::fs::read(&output_path)
            .map_err(|error| invalid_schema(format!("Codex produced no response: {error}")))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| invalid_schema(format!("Codex returned invalid JSON: {error}")))?;
        validate_json_schema(&schema.0, &value)?;
        Ok(StructuredResponse {
            value,
            provider: "codex".to_owned(),
            model: self.model.clone(),
        })
    }

    async fn health(&self) -> ProviderHealth {
        let Ok(version) = self.command_output(&["--version"]).await else {
            return ProviderHealth::BinaryMissing;
        };
        if !version.status.success() {
            return ProviderHealth::BinaryMissing;
        }
        let Ok(login) = self.command_output(&["login", "status"]).await else {
            return ProviderHealth::NotAuthenticated;
        };
        if !login.status.success() {
            return ProviderHealth::NotAuthenticated;
        }
        if self.model != "default" {
            let Ok(models) = self.command_output(&["debug", "models"]).await else {
                return ProviderHealth::ModelUnavailable;
            };
            if !models.status.success()
                || !String::from_utf8_lossy(&models.stdout).contains(&self.model)
            {
                return ProviderHealth::ModelUnavailable;
            }
        }
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
        "codex"
    }

    fn model(&self) -> &str {
        &self.model
    }
}

pub struct OpenRouterProvider {
    client: reqwest::Client,
    model: String,
    base_url: String,
    api_key_env: String,
    reasoning_effort: Option<String>,
}

pub struct OpenAIApiProvider {
    compatible: OpenRouterProvider,
}

impl OpenAIApiProvider {
    pub fn new(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
    ) -> Self {
        Self {
            compatible: OpenRouterProvider::new(model, base_url, api_key_env),
        }
    }

    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.compatible = self.compatible.with_reasoning_effort(reasoning_effort);
        self
    }
}

#[async_trait]
impl LlmProvider for OpenAIApiProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        let mut response = self.compatible.generate_structured(request, schema).await?;
        response.provider = "openai".to_owned();
        Ok(response)
    }

    async fn health(&self) -> ProviderHealth {
        self.compatible.health().await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.compatible.capabilities()
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    fn model(&self) -> &str {
        self.compatible.model()
    }
}

impl OpenRouterProvider {
    pub fn new(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
            base_url: base_url.into(),
            api_key_env: api_key_env.into(),
            reasoning_effort: None,
        }
    }

    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        let api_key = std::env::var(&self.api_key_env)
            .map_err(|_| authentication("OpenRouter API key is not configured"))?;
        let mut payload = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.prompt }
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "menvane_compilation", "strict": true, "schema": schema.0 }
            }
        });
        if let Some(reasoning_effort) = &self.reasoning_effort {
            payload["reasoning_effort"] = Value::String(reasoning_effort.clone());
        }
        let response = self
            .client
            .post(format!(
                "{}/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .timeout(request.timeout)
            .json(&payload)
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 | 403 => authentication(message),
                429 => rate_limited(message),
                400 => unsupported(message),
                _ if status.is_server_error() => network(message),
                _ => invalid_input(message),
            });
        }
        let response: Value = response
            .json()
            .await
            .map_err(|error| invalid_schema(error.to_string()))?;
        let content = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_schema("OpenRouter response has no message content"))?;
        let value: Value = serde_json::from_str(content).map_err(|error| {
            invalid_schema(format!("OpenRouter returned invalid JSON: {error}"))
        })?;
        validate_json_schema(&schema.0, &value)?;
        Ok(StructuredResponse {
            value,
            provider: "openrouter".to_owned(),
            model: self.model.clone(),
        })
    }

    async fn health(&self) -> ProviderHealth {
        if std::env::var_os(&self.api_key_env).is_none() {
            ProviderHealth::MissingApiKey
        } else if self.model.trim().is_empty() {
            ProviderHealth::ModelUnavailable
        } else {
            ProviderHealth::Ready
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            structured_output: true,
            json_schema: true,
            embeddings: false,
        }
    }

    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn model(&self) -> &str {
        &self.model
    }
}

pub struct ProviderChain {
    primary: Arc<dyn LlmProvider>,
    fallback: Option<Arc<dyn LlmProvider>>,
}

impl ProviderChain {
    pub fn new(primary: Arc<dyn LlmProvider>, fallback: Option<Arc<dyn LlmProvider>>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl LlmProvider for ProviderChain {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        match self
            .primary
            .generate_structured(request.clone(), schema.clone())
            .await
        {
            Ok(response) => Ok(response),
            Err(error) if error.fallback_allowed() => {
                let fallback = self.fallback.as_ref().ok_or(error)?;
                fallback.generate_structured(request, schema).await
            }
            Err(error) => Err(error),
        }
    }

    async fn health(&self) -> ProviderHealth {
        self.primary.health().await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.primary.capabilities()
    }

    fn name(&self) -> &'static str {
        self.primary.name()
    }

    fn model(&self) -> &str {
        self.primary.model()
    }
}

fn classify_codex_error(stderr: &str) -> LlmError {
    let lowercase = stderr.to_ascii_lowercase();
    if lowercase.contains("not authenticated")
        || lowercase.contains("unauthorized")
        || lowercase.contains("401")
        || lowercase.contains("403")
    {
        authentication(stderr)
    } else if lowercase.contains("model")
        && (lowercase.contains("unavailable") || lowercase.contains("unsupported"))
    {
        unsupported(stderr)
    } else if lowercase.contains("rate") || lowercase.contains("usage limit") {
        rate_limited(stderr)
    } else {
        unavailable(stderr)
    }
}

fn error(kind: LlmErrorKind, message: impl ToString) -> LlmError {
    LlmError {
        kind,
        message: message.to_string(),
    }
}

fn unavailable(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Unavailable, message)
}

fn authentication(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Authentication, message)
}

fn rate_limited(message: impl ToString) -> LlmError {
    error(LlmErrorKind::RateLimited, message)
}

fn network(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Network, message)
}

fn unsupported(message: impl ToString) -> LlmError {
    error(LlmErrorKind::UnsupportedCapability, message)
}

fn invalid_input(message: impl ToString) -> LlmError {
    error(LlmErrorKind::InvalidInput, message)
}

fn invalid_schema(message: impl ToString) -> LlmError {
    error(LlmErrorKind::InvalidSchema, message)
}

fn internal(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Internal, message)
}

fn validate_json_schema(schema: &Value, value: &Value) -> Result<(), LlmError> {
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        return Err(invalid_schema("response does not match schema const"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(value)
    {
        return Err(invalid_schema("response does not match schema enum"));
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let object = value
                .as_object()
                .ok_or_else(|| invalid_schema("response value must be an object"))?;
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        return Err(invalid_schema(format!("response is missing {key}")));
                    }
                }
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            if schema.get("additionalProperties") == Some(&Value::Bool(false))
                && object
                    .keys()
                    .any(|key| properties.is_none_or(|properties| !properties.contains_key(key)))
            {
                return Err(invalid_schema(
                    "response contains an unexpected object property",
                ));
            }
            if let Some(properties) = properties {
                for (key, property_schema) in properties {
                    if let Some(property) = object.get(key) {
                        validate_json_schema(property_schema, property)?;
                    }
                }
            }
        }
        Some("array") => {
            let array = value
                .as_array()
                .ok_or_else(|| invalid_schema("response value must be an array"))?;
            if let Some(items) = schema.get("items") {
                for item in array {
                    validate_json_schema(items, item)?;
                }
            }
        }
        Some("string") if !value.is_string() => {
            return Err(invalid_schema("response value must be a string"));
        }
        Some("number") if !value.is_number() => {
            return Err(invalid_schema("response value must be a number"));
        }
        Some("integer") if value.as_i64().is_none() && value.as_u64().is_none() => {
            return Err(invalid_schema("response value must be an integer"));
        }
        Some("boolean") if !value.is_boolean() => {
            return Err(invalid_schema("response value must be a boolean"));
        }
        Some("null") if !value.is_null() => {
            return Err(invalid_schema("response value must be null"));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::post;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn codex_sets_internal_marker_and_parses_structured_output() {
        let temporary = TempDir::new().unwrap();
        let binary = script(
            &temporary,
            "success",
            r#"#!/bin/sh
if [ "$MENVANE_INTERNAL" != "1" ]; then exit 19; fi
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then shift; output="$1"; fi
  shift
done
printf '%s' '{"ok":true}' > "$output"
"#,
        );
        let provider = CodexProvider::new(binary, "default");
        let response = provider
            .generate_structured(request(), schema())
            .await
            .unwrap();
        assert_eq!(response.value, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn codex_health_distinguishes_missing_and_unauthenticated() {
        let missing = CodexProvider::new("/missing/menvane-codex", "default");
        assert_eq!(missing.health().await, ProviderHealth::BinaryMissing);
        let temporary = TempDir::new().unwrap();
        let binary = script(
            &temporary,
            "unauthenticated",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then exit 0; fi
if [ "$1" = "login" ]; then exit 1; fi
exit 1
"#,
        );
        let provider = CodexProvider::new(binary, "default");
        assert_eq!(provider.health().await, ProviderHealth::NotAuthenticated);
    }

    #[tokio::test]
    async fn codex_rejects_invalid_json_and_schema_mismatch() {
        for (name, response) in [("invalid", "not-json"), ("mismatch", "{\"ok\":false}")] {
            let temporary = TempDir::new().unwrap();
            let source = format!(
                "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--output-last-message\" ]; then shift; output=\"$1\"; fi\n  shift\ndone\nprintf '%s' '{}' > \"$output\"\n",
                response.replace('\'', "'\\''")
            );
            let provider = CodexProvider::new(script(&temporary, name, &source), "default");
            let error = provider
                .generate_structured(request(), schema())
                .await
                .unwrap_err();
            assert_eq!(error.kind, LlmErrorKind::InvalidSchema);
        }
    }

    #[tokio::test]
    async fn codex_classifies_timeout_and_nonzero_authentication() {
        let temporary = TempDir::new().unwrap();
        let slow = script(&temporary, "slow", "#!/bin/sh\nsleep 1\n");
        let provider = CodexProvider::new(slow, "default");
        let mut timed = request();
        timed.timeout = std::time::Duration::from_millis(10);
        assert_eq!(
            provider
                .generate_structured(timed, schema())
                .await
                .unwrap_err()
                .kind,
            LlmErrorKind::Unavailable
        );
        let auth = script(
            &temporary,
            "auth",
            "#!/bin/sh\nprintf '%s' '401 not authenticated' >&2\nexit 1\n",
        );
        let provider = CodexProvider::new(auth, "default");
        assert_eq!(
            provider
                .generate_structured(request(), schema())
                .await
                .unwrap_err()
                .kind,
            LlmErrorKind::Authentication
        );
    }

    #[tokio::test]
    async fn openrouter_handles_success_and_http_failures() {
        let key_name = "MENVANE_TEST_OPENROUTER_KEY";
        unsafe { std::env::set_var(key_name, "test-key") };
        let success_url = mock_server(
            StatusCode::OK,
            json!({ "choices": [{ "message": { "content": "{\"ok\":true}" } }] }),
            Duration::ZERO,
        )
        .await;
        let provider = OpenRouterProvider::new("test/model", success_url, key_name);
        assert_eq!(
            provider
                .generate_structured(request(), schema())
                .await
                .unwrap()
                .value,
            json!({ "ok": true })
        );
        for (status, expected) in [
            (StatusCode::UNAUTHORIZED, LlmErrorKind::Authentication),
            (StatusCode::TOO_MANY_REQUESTS, LlmErrorKind::RateLimited),
            (StatusCode::INTERNAL_SERVER_ERROR, LlmErrorKind::Network),
        ] {
            let url = mock_server(status, json!({ "error": "failure" }), Duration::ZERO).await;
            let provider = OpenRouterProvider::new("test/model", url, key_name);
            assert_eq!(
                provider
                    .generate_structured(request(), schema())
                    .await
                    .unwrap_err()
                    .kind,
                expected
            );
        }
    }

    #[tokio::test]
    async fn openrouter_rejects_malformed_mismatched_and_timed_out_responses() {
        let key_name = "MENVANE_TEST_OPENROUTER_EDGE_KEY";
        unsafe { std::env::set_var(key_name, "test-key") };
        for body in [
            json!({ "unexpected": true }),
            json!({ "choices": [{ "message": { "content": "{\"ok\":false}" } }] }),
        ] {
            let url = mock_server(StatusCode::OK, body, Duration::ZERO).await;
            let provider = OpenRouterProvider::new("test/model", url, key_name);
            assert_eq!(
                provider
                    .generate_structured(request(), schema())
                    .await
                    .unwrap_err()
                    .kind,
                LlmErrorKind::InvalidSchema
            );
        }
        let url = mock_server(
            StatusCode::OK,
            json!({ "choices": [{ "message": { "content": "{\"ok\":true}" } }] }),
            Duration::from_millis(100),
        )
        .await;
        let provider = OpenRouterProvider::new("test/model", url, key_name);
        let mut request = request();
        request.timeout = Duration::from_millis(10);
        assert_eq!(
            provider
                .generate_structured(request, schema())
                .await
                .unwrap_err()
                .kind,
            LlmErrorKind::Network
        );
    }

    #[tokio::test]
    async fn openai_uses_its_own_api_key_and_reports_provider_identity() {
        let key_name = "MENVANE_TEST_OPENAI_KEY";
        unsafe { std::env::set_var(key_name, "openai-test-key") };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/chat/completions",
            post(
                |headers: axum::http::HeaderMap, axum::Json(payload): axum::Json<Value>| async move {
                if headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    != Some("Bearer openai-test-key")
                {
                    return (
                        StatusCode::UNAUTHORIZED,
                        axum::Json(json!({ "error": "missing key" })),
                    );
                }
                if payload.get("reasoning_effort").and_then(Value::as_str) != Some("medium") {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({ "error": "missing reasoning effort" })),
                    );
                }
                (
                    StatusCode::OK,
                    axum::Json(
                        json!({ "choices": [{ "message": { "content": "{\"ok\":true}" } }] }),
                    ),
                )
                },
            ),
        );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAIApiProvider::new("test-model", format!("http://{address}"), key_name)
            .with_reasoning_effort(Some("medium".to_owned()));
        let response = provider
            .generate_structured(request(), schema())
            .await
            .unwrap();
        assert_eq!(response.provider, "openai");
        assert_eq!(response.model, "test-model");
        assert_eq!(response.value, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn openai_requires_configured_api_key_environment_variable() {
        let provider = OpenAIApiProvider::new(
            "test-model",
            "https://api.openai.com/v1",
            "MENVANE_UNSET_OPENAI_KEY",
        );
        assert_eq!(provider.health().await, ProviderHealth::MissingApiKey);
        assert_eq!(
            provider
                .generate_structured(request(), schema())
                .await
                .unwrap_err()
                .kind,
            LlmErrorKind::Authentication
        );
    }

    fn request() -> LlmRequest {
        LlmRequest {
            system: "test".to_owned(),
            prompt: "test".to_owned(),
            timeout: std::time::Duration::from_secs(2),
        }
    }

    fn schema() -> JsonSchema {
        JsonSchema(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["ok"],
            "properties": { "ok": { "type": "boolean", "const": true } }
        }))
    }

    fn script(temporary: &TempDir, name: &str, source: &str) -> PathBuf {
        let path = temporary.path().join(name);
        std::fs::write(&path, source).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    async fn mock_server(status: StatusCode, body: Value, delay: Duration) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move {
                    tokio::time::sleep(delay).await;
                    (status, axum::Json(body))
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }
}
