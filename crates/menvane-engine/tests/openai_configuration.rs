use std::fs;

use menvane_domain::ProviderHealth;
use menvane_engine::Menvane;
use tempfile::TempDir;

#[test]
fn openai_configuration_uses_native_oauth_without_api_key_configuration() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    menvane
        .configure_openai("gpt-test-model", Some("medium"))
        .unwrap();
    let configuration = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(configuration.contains("provider = \"openai\""));
    assert!(configuration.contains("model = \"gpt-test-model\""));
    assert!(configuration.contains("reasoning_effort = \"medium\""));
    assert!(configuration.contains("oauth_issuer = \"https://auth.openai.com\""));
    assert!(
        configuration
            .contains("oauth_endpoint = \"https://chatgpt.com/backend-api/codex/responses\"")
    );
    assert!(!configuration.contains("api_key_env"));
    assert!(!configuration.contains("sk-"));
}

#[test]
fn github_copilot_configuration_uses_device_oauth_without_token_configuration() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    menvane
        .configure_github_copilot("gpt-4.1", Some("high"), "client-id")
        .unwrap();
    let configuration = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(configuration.contains("provider = \"github-copilot\""));
    assert!(configuration.contains("model = \"gpt-4.1\""));
    assert!(configuration.contains("github_client_id = \"client-id\""));
    assert!(configuration.contains("github_oauth_issuer = \"https://github.com\""));
    assert!(configuration.contains("base_url = \"https://api.githubcopilot.com\""));
    assert!(!configuration.contains("access_token"));
    assert!(!configuration.contains("refresh_token"));
}

#[tokio::test]
async fn openai_api_configuration_uses_api_key_from_toml() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    menvane
        .update_configuration_text(
            r#"
[llm]
provider = "openai-api"
model = "gpt-test-model"
base_url = "https://api.openai.com/v1"
api_key = "toml-test-key"
api_key_env = "MENVANE_UNSET_OPENAI_KEY"
"#,
        )
        .unwrap();

    let configured = Menvane::new(&home).unwrap();
    assert_eq!(
        configured.provider_health().await.unwrap().2,
        ProviderHealth::Ready
    );
}
