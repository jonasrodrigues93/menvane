use std::env;
use std::time::Duration;

use menvane_domain::{EmbeddingError, EmbeddingProvider, ProviderCapabilities};
use reqwest::blocking::Client;
use serde_json::{Value, json};

pub struct OpenAICompatibleEmbeddingProvider {
    name: String,
    model: String,
    base_url: String,
    api_key_env: String,
    api_key: Option<String>,
}

impl OpenAICompatibleEmbeddingProvider {
    pub fn new(
        name: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            model: model.into(),
            base_url: base_url.into(),
            api_key_env: api_key_env.into(),
            api_key: None,
        }
    }

    pub fn with_optional_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key;
        self
    }

    fn embed_blocking(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let api_key = self
            .api_key
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var(&self.api_key_env).ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                EmbeddingError::Unavailable(format!("{} is not set", self.api_key_env))
            })?;
        let response = Client::new()
            .post(format!(
                "{}/embeddings",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .timeout(Duration::from_secs(30))
            .json(&json!({ "model": self.model, "input": text }))
            .send()
            .map_err(|error| EmbeddingError::Request(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(EmbeddingError::Request(response.text().unwrap_or_else(
                |_| format!("embedding provider returned {status}"),
            )));
        }
        let response: Value = response
            .json()
            .map_err(|error| EmbeddingError::InvalidResponse(error.to_string()))?;
        let values = response
            .pointer("/data/0/embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                EmbeddingError::InvalidResponse("response has no embedding vector".to_owned())
            })?;
        let embedding = values
            .iter()
            .map(|value| {
                value.as_f64().map(|value| value as f32).ok_or_else(|| {
                    EmbeddingError::InvalidResponse(
                        "embedding vector contains a non-number".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        validate_embedding(&embedding)?;
        Ok(embedding)
    }
}

impl EmbeddingProvider for OpenAICompatibleEmbeddingProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            structured_output: false,
            json_schema: false,
            embeddings: true,
        }
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        std::thread::scope(|scope| {
            scope
                .spawn(|| self.embed_blocking(text))
                .join()
                .map_err(|_| EmbeddingError::Request("embedding request panicked".to_owned()))?
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }
}

pub fn validate_embedding(embedding: &[f32]) -> Result<(), EmbeddingError> {
    if embedding.is_empty() {
        return Err(EmbeddingError::InvalidResponse(
            "embedding vector is empty".to_owned(),
        ));
    }
    if embedding.iter().any(|value| !value.is_finite()) {
        return Err(EmbeddingError::InvalidResponse(
            "embedding vector contains a non-finite value".to_owned(),
        ));
    }
    if embedding.iter().all(|value| *value == 0.0) {
        return Err(EmbeddingError::InvalidResponse(
            "embedding vector has zero magnitude".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn embedding_validation_rejects_unusable_vectors() {
        assert!(validate_embedding(&[]).is_err());
        assert!(validate_embedding(&[0.0, 0.0]).is_err());
        assert!(validate_embedding(&[f32::NAN]).is_err());
        assert!(validate_embedding(&[0.5, -0.25]).is_ok());
    }

    #[test]
    fn openai_compatible_provider_uses_the_embeddings_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            assert!(request_line.starts_with("POST /v1/embeddings "));
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "embedding-test");
            assert_eq!(body["input"], "semantic input");
            let response = r#"{"data":[{"embedding":[0.5,-0.25]}]}"#;
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let provider = OpenAICompatibleEmbeddingProvider::new(
            "openai-api",
            "embedding-test",
            format!("http://{address}/v1"),
            "UNUSED_API_KEY",
        )
        .with_optional_api_key(Some("secret".to_owned()));

        assert_eq!(provider.embed("semantic input").unwrap(), vec![0.5, -0.25]);
        server.join().unwrap();
    }
}
