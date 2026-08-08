use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use menvane_domain::{
    JsonSchema, LlmError, LlmErrorKind, LlmProvider, LlmRequest, ProviderCapabilities,
    ProviderHealth, StructuredResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use url::Url;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const DEFAULT_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const CALLBACK_PORT: u16 = 1455;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthCredentials {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
}

pub struct OpenAiOAuthProvider {
    client: reqwest::Client,
    model: String,
    reasoning_effort: Option<String>,
    issuer: String,
    endpoint: String,
    credentials_path: PathBuf,
    refresh_lock: Mutex<()>,
}

impl OpenAiOAuthProvider {
    pub fn new(home: &Path, model: impl Into<String>, reasoning_effort: Option<String>) -> Self {
        Self::with_endpoints(
            home,
            model,
            reasoning_effort,
            DEFAULT_ISSUER,
            DEFAULT_ENDPOINT,
        )
    }

    pub fn with_endpoints(
        home: &Path,
        model: impl Into<String>,
        reasoning_effort: Option<String>,
        issuer: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
            reasoning_effort,
            issuer: issuer.into(),
            endpoint: endpoint.into(),
            credentials_path: home.join("oauth/openai.json"),
            refresh_lock: Mutex::new(()),
        }
    }

    pub async fn login(&self) -> Result<(), LlmError> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
            .await
            .map_err(internal)?;
        let redirect_uri = format!("http://localhost:{CALLBACK_PORT}/auth/callback");
        let verifier = random_base64url(64)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_base64url(32)?;
        let mut authorization_url = Url::parse(&format!(
            "{}/oauth/authorize",
            self.issuer.trim_end_matches('/')
        ))
        .map_err(internal)?;
        authorization_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", "openid profile email offline_access")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("state", &state)
            .append_pair("originator", "menvane");
        println!("Open this URL to authorize Menvane:\n{authorization_url}");
        let _ = webbrowser::open(authorization_url.as_str());
        let (code, returned_state) =
            tokio::time::timeout(Duration::from_secs(300), wait_for_callback(&listener))
                .await
                .map_err(|_| authentication("OAuth browser approval timed out"))??;
        if returned_state != state {
            return Err(authentication("OAuth state mismatch"));
        }
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer.trim_end_matches('/')))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("client_id", CLIENT_ID),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        if !response.status().is_success() {
            return Err(authentication(format!(
                "OAuth token exchange failed with status {}",
                response.status()
            )));
        }
        let tokens: OAuthTokenResponse = response
            .json()
            .await
            .map_err(|error| authentication(error.to_string()))?;
        let refresh_token = tokens
            .refresh_token
            .clone()
            .ok_or_else(|| authentication("OAuth response omitted refresh token"))?;
        self.save_credentials(&OAuthCredentials {
            account_id: extract_account_id(tokens.id_token.as_deref())
                .or_else(|| extract_account_id(Some(&tokens.access_token))),
            access_token: tokens.access_token,
            refresh_token,
            expires_at: Utc::now().timestamp() + tokens.expires_in.unwrap_or(3600),
        })
        .map_err(internal)
    }

    pub fn logout(&self) -> Result<(), LlmError> {
        if self.credentials_path.exists() {
            fs::remove_file(&self.credentials_path).map_err(internal)?;
        }
        Ok(())
    }

    async fn active_credentials(&self) -> Result<OAuthCredentials, LlmError> {
        let _guard = self.refresh_lock.lock().await;
        let mut credentials = self
            .load_credentials()
            .map_err(|error| authentication(format!("OpenAI OAuth login is required: {error}")))?;
        if credentials.expires_at > Utc::now().timestamp() + 60 {
            return Ok(credentials);
        }
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer.trim_end_matches('/')))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", credentials.refresh_token.as_str()),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        if !response.status().is_success() {
            return Err(authentication(format!(
                "OAuth token refresh failed with status {}",
                response.status()
            )));
        }
        let tokens: OAuthTokenResponse = response
            .json()
            .await
            .map_err(|error| authentication(error.to_string()))?;
        credentials.access_token = tokens.access_token;
        if let Some(refresh_token) = tokens.refresh_token {
            credentials.refresh_token = refresh_token;
        }
        credentials.expires_at = Utc::now().timestamp() + tokens.expires_in.unwrap_or(3600);
        credentials.account_id = extract_account_id(tokens.id_token.as_deref())
            .or_else(|| extract_account_id(Some(&credentials.access_token)))
            .or(credentials.account_id);
        self.save_credentials(&credentials).map_err(internal)?;
        Ok(credentials)
    }

    fn load_credentials(&self) -> std::io::Result<OAuthCredentials> {
        let bytes = fs::read(&self.credentials_path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    fn save_credentials(&self, credentials: &OAuthCredentials) -> std::io::Result<()> {
        let parent = self
            .credentials_path
            .parent()
            .ok_or_else(|| std::io::Error::other("OAuth path has no parent"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".openai-{}.tmp", uuid::Uuid::now_v7()));
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
impl LlmProvider for OpenAiOAuthProvider {
    async fn generate_structured(
        &self,
        request: LlmRequest,
        schema: JsonSchema,
    ) -> Result<StructuredResponse, LlmError> {
        let credentials = self.active_credentials().await?;
        let mut payload = json!({
            "model": self.model,
            "instructions": request.system,
            "input": [{
                "role": "user",
                "content": [{ "type": "input_text", "text": request.prompt }]
            }],
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "menvane_compilation",
                    "strict": true,
                    "schema": schema.0
                }
            },
            "store": false,
            "stream": true
        });
        if let Some(reasoning_effort) = &self.reasoning_effort {
            payload["reasoning"] = json!({ "effort": reasoning_effort });
        }
        let mut request_builder = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&credentials.access_token)
            .header("originator", "menvane")
            .header(
                "User-Agent",
                format!("menvane/{}", env!("CARGO_PKG_VERSION")),
            )
            .timeout(request.timeout)
            .json(&payload);
        if let Some(account_id) = &credentials.account_id {
            request_builder = request_builder.header("ChatGPT-Account-Id", account_id);
        }
        let response = request_builder
            .send()
            .await
            .map_err(|error| network(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 | 403 => authentication(message),
                429 => error(LlmErrorKind::RateLimited, message),
                _ if status.is_server_error() => network(message),
                _ => error(LlmErrorKind::InvalidInput, message),
            });
        }
        let body = response
            .text()
            .await
            .map_err(|error| invalid_schema(error.to_string()))?;
        let output = extract_stream_output(&body)?;
        let value = serde_json::from_str(&output)
            .map_err(|error| invalid_schema(format!("ChatGPT returned invalid JSON: {error}")))?;
        Ok(StructuredResponse {
            value,
            provider: "openai".to_owned(),
            model: self.model.clone(),
        })
    }

    async fn health(&self) -> ProviderHealth {
        match self.load_credentials() {
            Ok(credentials)
                if !credentials.access_token.is_empty()
                    && !credentials.refresh_token.is_empty() =>
            {
                ProviderHealth::Ready
            }
            _ => ProviderHealth::NotAuthenticated,
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
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }
}

async fn wait_for_callback(
    listener: &tokio::net::TcpListener,
) -> Result<(String, String), LlmError> {
    let (mut stream, _) = listener.accept().await.map_err(internal)?;
    let mut buffer = vec![0_u8; 16_384];
    let bytes = stream.read(&mut buffer).await.map_err(internal)?;
    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| authentication("OAuth callback request is invalid"))?;
    let callback =
        Url::parse(&format!("http://localhost:{CALLBACK_PORT}{target}")).map_err(internal)?;
    if callback.path() != "/auth/callback" {
        return Err(authentication("OAuth callback path is invalid"));
    }
    let parameters = callback
        .query_pairs()
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    let result = match parameters.get("error") {
        Some(error_message) => Err(authentication(
            parameters.get("error_description").unwrap_or(error_message),
        )),
        None => Ok((
            parameters
                .get("code")
                .cloned()
                .ok_or_else(|| authentication("OAuth callback omitted code"))?,
            parameters
                .get("state")
                .cloned()
                .ok_or_else(|| authentication("OAuth callback omitted state"))?,
        )),
    };
    let (status, message) = if result.is_ok() {
        (
            "200 OK",
            "Menvane authorization completed. You can close this window.",
        )
    } else {
        (
            "400 Bad Request",
            "Menvane authorization failed. Return to the terminal.",
        )
    };
    let body = format!("<!doctype html><title>Menvane OAuth</title><h1>{message}</h1>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(internal)?;
    result
}

fn random_base64url(bytes: usize) -> Result<String, LlmError> {
    let mut value = vec![0_u8; bytes];
    getrandom::fill(&mut value).map_err(internal)?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

fn extract_account_id(token: Option<&str>) -> Option<String> {
    let payload = token?.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(Value::as_array)
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn extract_stream_output(body: &str) -> Result<String, LlmError> {
    let mut deltas = String::new();
    let mut completed = None;
    for data in body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty() && *data != "[DONE]")
    {
        let event: Value = serde_json::from_str(data).map_err(|error| {
            invalid_schema(format!("ChatGPT returned invalid SSE data: {error}"))
        })?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    deltas.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                completed = event.get("text").and_then(Value::as_str).map(str::to_owned);
            }
            Some("response.completed") => {
                completed = event.get("response").and_then(extract_response_output);
            }
            Some("error") | Some("response.failed") => {
                let message = event
                    .pointer("/error/message")
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("ChatGPT streaming response failed");
                return Err(error(LlmErrorKind::Unavailable, message));
            }
            _ => {}
        }
    }
    completed
        .or_else(|| (!deltas.is_empty()).then_some(deltas))
        .ok_or_else(|| invalid_schema("ChatGPT response omitted output text"))
}

fn extract_response_output(response: &Value) -> Option<String> {
    response
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            response
                .get("output")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("content").and_then(Value::as_array))
                .flatten()
                .find_map(|content| {
                    content
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|_| {
                            content.get("type").and_then(Value::as_str) == Some("output_text")
                        })
                        .map(str::to_owned)
                })
        })
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

fn invalid_schema(message: impl ToString) -> LlmError {
    error(LlmErrorKind::InvalidSchema, message)
}

fn internal(message: impl ToString) -> LlmError {
    error(LlmErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use menvane_domain::LlmProvider;
    use tempfile::TempDir;

    #[test]
    fn extracts_account_id_from_supported_claim_shapes() {
        let direct = token(json!({ "chatgpt_account_id": "direct" }));
        let namespaced = token(json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "namespaced" }
        }));
        let organization = token(json!({ "organizations": [{ "id": "organization" }] }));

        assert_eq!(extract_account_id(Some(&direct)).as_deref(), Some("direct"));
        assert_eq!(
            extract_account_id(Some(&namespaced)).as_deref(),
            Some("namespaced")
        );
        assert_eq!(
            extract_account_id(Some(&organization)).as_deref(),
            Some("organization")
        );
    }

    #[test]
    fn extracts_structured_output_from_response_stream() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"{\\\"ok\\\":\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"true}\"}\n\n",
            "data: [DONE]\n\n"
        );

        assert_eq!(extract_stream_output(body).unwrap(), "{\"ok\":true}");
    }

    #[tokio::test]
    async fn credentials_are_private_and_logout_removes_them() {
        let temporary = TempDir::new().unwrap();
        let provider = OpenAiOAuthProvider::new(temporary.path(), "gpt-test", None);
        provider
            .save_credentials(&OAuthCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at: Utc::now().timestamp() + 3600,
                account_id: Some("account".to_owned()),
            })
            .unwrap();

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

        provider.logout().unwrap();
        assert_eq!(provider.health().await, ProviderHealth::NotAuthenticated);
    }

    fn token(claims: Value) -> String {
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }
}
