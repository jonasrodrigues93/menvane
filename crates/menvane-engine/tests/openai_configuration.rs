use std::fs;

use menvane_engine::Menvane;
use tempfile::TempDir;

#[test]
fn openai_configuration_persists_only_environment_variable_name() {
    let temporary = TempDir::new().unwrap();
    let home = temporary.path().join("home");
    let menvane = Menvane::new(&home).unwrap();
    menvane
        .configure_openai(
            "gpt-test-model",
            "https://api.openai.com/v1",
            "MY_OPENAI_API_KEY",
        )
        .unwrap();
    let configuration = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(configuration.contains("provider = \"openai\""));
    assert!(configuration.contains("model = \"gpt-test-model\""));
    assert!(configuration.contains("api_key_env = \"MY_OPENAI_API_KEY\""));
    assert!(!configuration.contains("sk-"));
}
