use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use menvane_domain::{
    JsonSchema, LlmError, LlmErrorKind, LlmProvider, LlmRequest, ProviderCapabilities,
    ProviderHealth, ResponseUsage, StructuredResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const DEFAULT_OAUTH_ISSUER: &str = "https://github.com";
const DEFAULT_IDENTITY_ENDPOINT: &str = "https://api.github.com";
const DEFAULT_SCOPE: &str = "read:user offline_access";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopilotCredentials {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
    error_description: Option<String>,
}

pub struct GithubCopilotProvider {
    client: reqwest::Client,
    model: String,
    reasoning_effort: Option<String>,
    client_id: String,
    oauth_issuer: String,
    identity_endpoint: String,
    api_endpoint: String,
    credentials_path: PathBuf,
    refresh_lock: Mutex<()>,
    poll_interval_override: Option<Duration>,
}

impl GithubCopilotProvider {
    pub fn new(
        home: &Path,
        model: impl Into<String>,
        reasoning_effort: Option<String>,
        client_id: impl Into<String>,
        api_endpoint: impl Into<String>,
    ) -> Self {
        Self::with_endpoints(
            home,
            model,
            reasoning_effort,
            client_id,
            DEFAULT_OAUTH_ISSUER,
            api_endpoint,
        )
    }

    pub fn with_endpoints(
        home: &Path,
        model: impl Into<String>,
        reasoning_effort: Option<String>,
        client_id: impl Into<String>,
        oauth_issuer: impl Into<String>,
        api_endpoint: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
            reasoning_effort,
            client_id: client_id.into(),
            oauth_issuer: oauth_issuer.into(),
            identity_endpoint: DEFAULT_IDENTITY_ENDPOINT.to_owned(),
            api_endpoint: api_endpoint.into(),
            credentials_path: home.join("oauth/github-copilot.json"),
            refresh_lock: Mutex::new(()),
            poll_interval_override: None,
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval_override = Some(interval);
        self
    }

    #[cfg(test)]
    fn with_identity_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.identity_endpoint = endpoint.into();
        self
    }

    pub async fn login(&self) -> Result<(), LlmError> {
        if self.client_id.trim().is_empty() {
            return Err(authentication(
                "GitHub OAuth client ID is not configured; run provider configure first",
            ));
        }
        let response = self
            .client
            .post(format!(
                "{}/login/device/code",
                self.oauth_issuer.trim_end_matches('/')
            ))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", DEFAULT_SCOPE),
            ])
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        if !response.status().is_success() {
            return Err(authentication(format!(
                "GitHub device authorization failed with status {}",
                response.status()
            )));
        }
        let device: DeviceCodeResponse = response.json().await.map_err(|error| {
            authentication(format!("GitHub device authorization was invalid: {error}"))
        })?;
        println!(
            "Open {} and enter code {} to authorize GitHub Copilot.",
            device.verification_uri, device.user_code
        );
        let deadline = Instant::now() + Duration::from_secs(device.expires_in);
        let mut interval = self
            .poll_interval_override
            .unwrap_or_else(|| Duration::from_secs(device.interval));
        loop {
            if Instant::now() >= deadline {
                return Err(authentication("GitHub device authorization expired"));
            }
            if !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
            let response = self
                .client
                .post(format!(
                    "{}/login/oauth/access_token",
                    self.oauth_issuer.trim_end_matches('/')
                ))
                .header(reqwest::header::ACCEPT, "application/json")
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("device_code", device.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await
                .map_err(|error| network(error.to_string()))?;
            let status = response.status();
            let token: GithubTokenResponse = response.json().await.map_err(|error| {
                authentication(format!(
                    "GitHub authorization response was invalid: {error}"
                ))
            })?;
            if let Some(access_token) = token.access_token.filter(|value| !value.is_empty()) {
                self.verify_identity(&access_token).await?;
                self.save_credentials(&CopilotCredentials {
                    access_token,
                    refresh_token: token.refresh_token,
                    expires_at: token.expires_in.map(|value| Utc::now().timestamp() + value),
                })
                .map_err(internal)?;
                return Ok(());
            }
            match token.error.as_deref() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval = interval.saturating_add(Duration::from_secs(5));
                }
                Some("expired_token") | Some("token_expired") => {
                    return Err(authentication("GitHub device authorization expired"));
                }
                Some("access_denied") => {
                    return Err(authentication("GitHub authorization was denied"));
                }
                Some(error) => {
                    let detail = token.error_description.as_deref().unwrap_or(error);
                    return Err(authentication(format!(
                        "GitHub authorization failed: {detail}"
                    )));
                }
                None if !status.is_success() => {
                    return Err(authentication(format!(
                        "GitHub authorization failed with status {status}"
                    )));
                }
                None => return Err(authentication("GitHub authorization returned no token")),
            }
        }
    }

    pub fn logout(&self) -> Result<(), LlmError> {
        if self.credentials_path.exists() {
            fs::remove_file(&self.credentials_path).map_err(internal)?;
        }
        Ok(())
    }

    async fn verify_identity(&self, access_token: &str) -> Result<(), LlmError> {
        let response = self
            .client
            .get(format!(
                "{}/user",
                self.identity_endpoint.trim_end_matches('/')
            ))
            .bearer_auth(access_token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(
                reqwest::header::USER_AGENT,
                format!("menvane/{}", env!("CARGO_PKG_VERSION")),
            )
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(authentication(format!(
                "GitHub identity verification failed with status {}",
                response.status()
            )))
        }
    }

    async fn active_credentials(&self) -> Result<CopilotCredentials, LlmError> {
        let _guard = self.refresh_lock.lock().await;
        let mut credentials = self.load_credentials().map_err(|error| {
            authentication(format!("GitHub Copilot login is required: {error}"))
        })?;
        if credentials
            .expires_at
            .is_none_or(|expires_at| expires_at > Utc::now().timestamp() + 60)
        {
            return Ok(credentials);
        }
        let refresh_token = credentials
            .refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| authentication("GitHub Copilot login expired; authenticate again"))?;
        let response = self
            .client
            .post(format!(
                "{}/login/oauth/access_token",
                self.oauth_issuer.trim_end_matches('/')
            ))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        let status = response.status();
        let token: GithubTokenResponse = response.json().await.map_err(|error| {
            authentication(format!(
                "GitHub token refresh response was invalid: {error}"
            ))
        })?;
        let Some(access_token) = token.access_token.filter(|value| !value.is_empty()) else {
            return Err(authentication(if status.is_success() {
                "GitHub Copilot refresh returned no token"
            } else {
                "GitHub Copilot refresh was rejected; authenticate again"
            }));
        };
        credentials.access_token = access_token;
        if token.refresh_token.is_some() {
            credentials.refresh_token = token.refresh_token;
        }
        credentials.expires_at = token.expires_in.map(|value| Utc::now().timestamp() + value);
        self.save_credentials(&credentials).map_err(internal)?;
        Ok(credentials)
    }

    fn load_credentials(&self) -> std::io::Result<CopilotCredentials> {
        let bytes = fs::read(&self.credentials_path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    fn save_credentials(&self, credentials: &CopilotCredentials) -> std::io::Result<()> {
        let parent = self
            .credentials_path
            .parent()
            .ok_or_else(|| std::io::Error::other("GitHub Copilot credential path has no parent"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".github-copilot-{}.tmp", uuid::Uuid::now_v7()));
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?
        };
        #[cfg(not(unix))]
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(credentials).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(temporary, &self.credentials_path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

#[async_trait]
impl LlmProvider for GithubCopilotProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        let credentials = self.active_credentials().await?;
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
                self.api_endpoint.trim_end_matches('/')
            ))
            .bearer_auth(credentials.access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                "Editor-Version",
                format!("menvane/{}", env!("CARGO_PKG_VERSION")),
            )
            .timeout(request.timeout)
            .json(&payload)
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 => {
                    let _ = self.logout();
                    authentication("GitHub Copilot authentication was rejected")
                }
                403 => authentication("GitHub Copilot access is unavailable for this account"),
                429 => rate_limited("GitHub Copilot usage limit reached"),
                400 => invalid_input("GitHub Copilot rejected the inference request"),
                _ if status.is_server_error() => network("GitHub Copilot service error"),
                _ => unavailable(format!("GitHub Copilot returned status {status}")),
            });
        }
        let response: Value = response
            .json()
            .await
            .map_err(|error| invalid_schema(error.to_string()))?;
        let content = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_schema("GitHub Copilot response has no message content"))?;
        let value: Value = serde_json::from_str(content).map_err(|error| {
            invalid_schema(format!("GitHub Copilot returned invalid JSON: {error}"))
        })?;
        validate_json_schema(&schema.0, &value)?;
        Ok(StructuredResponse {
            value,
            provider: "github-copilot".to_owned(),
            model: self.model.clone(),
            usage: response.get("usage").and_then(parse_usage),
        })
    }

    async fn health(&self) -> ProviderHealth {
        if self.model.trim().is_empty() {
            ProviderHealth::ModelUnavailable
        } else {
            match self.load_credentials() {
                Ok(credentials) if !credentials.access_token.is_empty() => ProviderHealth::Ready,
                _ => ProviderHealth::NotAuthenticated,
            }
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
        "github-copilot"
    }

    fn model(&self) -> &str {
        &self.model
    }
}

fn parse_usage(value: &Value) -> Option<ResponseUsage> {
    Some(ResponseUsage {
        input_tokens: value.get("prompt_tokens").and_then(Value::as_u64),
        output_tokens: value.get("completion_tokens").and_then(Value::as_u64),
        credits: None,
    })
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

fn error(kind: LlmErrorKind, message: impl ToString) -> LlmError {
    LlmError {
        kind,
        message: message.to_string(),
    }
}

fn authentication(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Authentication, message)
}

fn network(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Network, message)
}

fn invalid_input(message: impl ToString) -> LlmError {
    error(LlmErrorKind::InvalidInput, message)
}

fn invalid_schema(message: impl ToString) -> LlmError {
    error(LlmErrorKind::InvalidSchema, message)
}

fn rate_limited(message: impl ToString) -> LlmError {
    error(LlmErrorKind::RateLimited, message)
}

fn unavailable(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Unavailable, message)
}

fn internal(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use menvane_domain::LlmProvider;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn device_login_stores_private_credentials_and_refreshes() {
        let temporary = TempDir::new().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/login/device/code",
                post(|| async {
                    axum::Json(json!({
                        "device_code": "device",
                        "user_code": "USER-CODE",
                        "verification_uri": "https://github.com/login/device",
                        "expires_in": 30,
                        "interval": 0
                    }))
                }),
            )
            .route(
                "/login/oauth/access_token",
                post(|| async {
                    axum::Json(json!({
                        "access_token": "access",
                        "refresh_token": "refresh",
                        "expires_in": 1
                    }))
                }),
            )
            .route(
                "/user",
                get(|| async { (StatusCode::OK, axum::Json(json!({"login":"user"}))) }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = GithubCopilotProvider::with_endpoints(
            temporary.path(),
            "gpt-test",
            None,
            "client-id",
            format!("http://{address}"),
            format!("http://{address}"),
        )
        .with_identity_endpoint(format!("http://{address}"))
        .with_poll_interval(Duration::ZERO);
        provider.login().await.unwrap();
        assert_eq!(provider.health().await, ProviderHealth::Ready);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&provider.credentials_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let refreshed = provider
            .active_credentials()
            .await
            .expect("refresh should succeed");
        assert_eq!(refreshed.access_token, "access");
        provider.logout().unwrap();
        assert_eq!(provider.health().await, ProviderHealth::NotAuthenticated);
    }

    #[tokio::test]
    async fn generation_sends_schema_and_parses_usage_without_leaking_tokens() {
        let temporary = TempDir::new().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/chat/completions",
            post(|headers: axum::http::HeaderMap, axum::Json(payload): axum::Json<Value>| async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer access");
                assert_eq!(payload["response_format"]["type"], "json_schema");
                assert_eq!(payload["response_format"]["json_schema"]["strict"], true);
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "choices": [{"message": {"content": "{\"ok\":true}"}}],
                        "usage": {"prompt_tokens": 2, "completion_tokens": 3}
                    })),
                )
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = GithubCopilotProvider::new(
            temporary.path(),
            "gpt-test",
            Some("medium".to_owned()),
            "client-id",
            format!("http://{address}"),
        )
        .with_identity_endpoint(format!("http://{address}"));
        provider
            .save_credentials(&CopilotCredentials {
                access_token: "access".to_owned(),
                refresh_token: None,
                expires_at: None,
            })
            .unwrap();
        let response = provider
            .generate_structured(request(), schema())
            .await
            .unwrap();
        assert_eq!(response.provider, "github-copilot");
        assert_eq!(response.value, json!({"ok": true}));
        assert_eq!(response.usage.unwrap().output_tokens, Some(3));
    }

    fn request() -> LlmRequest {
        LlmRequest {
            system: "test".to_owned(),
            prompt: "test".to_owned(),
            timeout: Duration::from_secs(2),
        }
    }

    fn schema() -> JsonSchema {
        JsonSchema(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["ok"],
            "properties": {"ok": {"type": "boolean", "const": true}}
        }))
    }
}
