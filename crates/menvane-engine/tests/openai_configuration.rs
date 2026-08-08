use std::fs;

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
