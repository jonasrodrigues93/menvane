use std::path::PathBuf;

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Task, Theme, application};
use menvane_setup::{Agent, SetupOptions};

fn main() -> iced::Result {
    application("Menvane Setup", update, view)
        .theme(|_| Theme::Dark)
        .run_with(|| (SetupApp::new(), Task::none()))
}

struct SetupApp {
    home: String,
    provider: String,
    model: String,
    reasoning_effort: String,
    endpoint: String,
    api_key: String,
    api_key_env: String,
    embedding_enabled: bool,
    embedding_model: String,
    max_prompt_bytes: String,
    max_tool_input_bytes: String,
    max_tool_output_bytes: String,
    idle_finalize_seconds: String,
    open_finalize_seconds: String,
    lease_timeout_seconds: String,
    memory_lifetime_days: String,
    min_match_confidence: String,
    min_knowledge_confidence: String,
    min_utility: String,
    max_cards: String,
    agents: [bool; 4],
    status: String,
    busy: bool,
}

#[derive(Debug, Clone)]
enum Message {
    HomeChanged(String),
    ProviderChanged(String),
    ModelChanged(String),
    ReasoningChanged(String),
    EndpointChanged(String),
    ApiKeyChanged(String),
    ApiKeyEnvChanged(String),
    EmbeddingEnabledChanged(bool),
    EmbeddingModelChanged(String),
    MaxPromptChanged(String),
    MaxToolInputChanged(String),
    MaxToolOutputChanged(String),
    IdleFinalizeChanged(String),
    OpenFinalizeChanged(String),
    LeaseTimeoutChanged(String),
    MemoryLifetimeChanged(String),
    MinMatchChanged(String),
    MinKnowledgeChanged(String),
    MinUtilityChanged(String),
    MaxCardsChanged(String),
    AgentChanged(usize, bool),
    Apply,
}

impl SetupApp {
    fn new() -> Self {
        let home = menvane_runtime::home_from_environment()
            .unwrap_or_else(|_| PathBuf::from("~/.menvane"));
        Self {
            home: home.display().to_string(),
            provider: "openai".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            reasoning_effort: "medium".to_owned(),
            endpoint: "https://api.openai.com/v1".to_owned(),
            api_key: String::new(),
            api_key_env: "OPENAI_API_KEY".to_owned(),
            embedding_enabled: false,
            embedding_model: "text-embedding-3-small".to_owned(),
            max_prompt_bytes: "16384".to_owned(),
            max_tool_input_bytes: "4096".to_owned(),
            max_tool_output_bytes: "4096".to_owned(),
            idle_finalize_seconds: "120".to_owned(),
            open_finalize_seconds: "1800".to_owned(),
            lease_timeout_seconds: "300".to_owned(),
            memory_lifetime_days: "90".to_owned(),
            min_match_confidence: "0.45".to_owned(),
            min_knowledge_confidence: "0.55".to_owned(),
            min_utility: "0.55".to_owned(),
            max_cards: "3".to_owned(),
            agents: detected_agents(),
            status: "Review the configuration before applying it.".to_owned(),
            busy: false,
        }
    }
}

fn update(app: &mut SetupApp, message: Message) -> Task<Message> {
    match message {
        Message::HomeChanged(value) => app.home = value,
        Message::ProviderChanged(value) => app.provider = value,
        Message::ModelChanged(value) => app.model = value,
        Message::ReasoningChanged(value) => app.reasoning_effort = value,
        Message::EndpointChanged(value) => app.endpoint = value,
        Message::ApiKeyChanged(value) => app.api_key = value,
        Message::ApiKeyEnvChanged(value) => app.api_key_env = value,
        Message::EmbeddingEnabledChanged(value) => app.embedding_enabled = value,
        Message::EmbeddingModelChanged(value) => app.embedding_model = value,
        Message::MaxPromptChanged(value) => app.max_prompt_bytes = value,
        Message::MaxToolInputChanged(value) => app.max_tool_input_bytes = value,
        Message::MaxToolOutputChanged(value) => app.max_tool_output_bytes = value,
        Message::IdleFinalizeChanged(value) => app.idle_finalize_seconds = value,
        Message::OpenFinalizeChanged(value) => app.open_finalize_seconds = value,
        Message::LeaseTimeoutChanged(value) => app.lease_timeout_seconds = value,
        Message::MemoryLifetimeChanged(value) => app.memory_lifetime_days = value,
        Message::MinMatchChanged(value) => app.min_match_confidence = value,
        Message::MinKnowledgeChanged(value) => app.min_knowledge_confidence = value,
        Message::MinUtilityChanged(value) => app.min_utility = value,
        Message::MaxCardsChanged(value) => app.max_cards = value,
        Message::AgentChanged(index, value) => app.agents[index] = value,
        Message::Apply if !app.busy => {
            app.busy = true;
            let result = build_options(app).and_then(|options| {
                setup_executable()
                    .map_err(|error| error.to_string())
                    .and_then(|executable| {
                        menvane_setup::apply(&options, &executable)
                            .map_err(|error| error.to_string())
                    })
            });
            match result {
                Ok(report) => {
                    app.status = format!(
                        "Setup complete. Service enabled and started for {}.",
                        report.home.display()
                    );
                }
                Err(error) => app.status = format!("Setup failed: {error}"),
            }
            app.busy = false;
        }
        Message::Apply => {}
    }
    Task::none()
}

fn setup_executable() -> std::io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let executable = current
        .parent()
        .map(|directory| directory.join("menvane"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("menvane"));
    Ok(executable)
}

fn detected_agents() -> [bool; 4] {
    let claude = menvane_integrations::ClaudePaths::discover()
        .map(|paths| paths.settings.exists() || paths.configuration.exists())
        .unwrap_or(false);
    let codex = menvane_integrations::CodexPaths::discover()
        .map(|paths| paths.configuration.exists())
        .unwrap_or(false);
    let opencode = menvane_integrations::OpenCodePaths::discover()
        .map(|paths| paths.configuration.exists() || paths.plugin.exists())
        .unwrap_or(false);
    let antigravity = menvane_integrations::AntigravityPaths::discover()
        .map(|paths| paths.mcp_configuration.exists() || paths.hooks_configuration.exists())
        .unwrap_or(false);
    [claude, codex, opencode, antigravity]
}

fn build_options(app: &SetupApp) -> Result<SetupOptions, String> {
    let mut options = SetupOptions::new(PathBuf::from(&app.home));
    options.provider = Some(app.provider.clone());
    options.model = Some(app.model.clone());
    options.reasoning_effort = Some(app.reasoning_effort.clone());
    options.base_url = Some(app.endpoint.clone());
    options.api_key = (!app.api_key.is_empty()).then(|| app.api_key.clone());
    options.api_key_env = Some(app.api_key_env.clone());
    options.max_prompt_bytes = Some(parse_u64("maximum prompt bytes", &app.max_prompt_bytes)?);
    options.max_tool_input_bytes = Some(parse_u64(
        "maximum tool input bytes",
        &app.max_tool_input_bytes,
    )?);
    options.max_tool_output_bytes = Some(parse_u64(
        "maximum tool output bytes",
        &app.max_tool_output_bytes,
    )?);
    options.idle_finalize_seconds = Some(parse_u64(
        "idle finalization seconds",
        &app.idle_finalize_seconds,
    )?);
    options.open_finalize_seconds = Some(parse_u64(
        "open finalization seconds",
        &app.open_finalize_seconds,
    )?);
    options.lease_timeout_seconds = Some(parse_u64(
        "lease timeout seconds",
        &app.lease_timeout_seconds,
    )?);
    options.memory_lifetime_days = Some(parse_u64(
        "memory lifetime days",
        &app.memory_lifetime_days,
    )?);
    options.min_match_confidence = Some(parse_f64(
        "minimum match confidence",
        &app.min_match_confidence,
    )?);
    options.min_knowledge_confidence = Some(parse_f64(
        "minimum knowledge confidence",
        &app.min_knowledge_confidence,
    )?);
    options.min_utility = Some(parse_f64("minimum utility", &app.min_utility)?);
    options.max_cards = Some(parse_u64("maximum knowledge cards", &app.max_cards)?);
    if app.embedding_enabled {
        options.embedding_provider = Some("openai-api".to_owned());
        options.embedding_model = Some(app.embedding_model.clone());
        options.embedding_base_url = Some(app.endpoint.clone());
        options.embedding_api_key_env = Some(app.api_key_env.clone());
        options.embedding_min_similarity = Some(0.78);
    }
    options.agents = Agent::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(index, agent)| app.agents[index].then_some(agent))
        .collect();
    Ok(options)
}

fn parse_u64(label: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{label} must be a number"))
}

fn parse_f64(label: &str, value: &str) -> Result<f64, String> {
    value
        .parse()
        .map_err(|_| format!("{label} must be a number"))
}

fn view(app: &SetupApp) -> Element<'_, Message> {
    let agent_names = ["Claude Code", "Codex", "OpenCode", "Antigravity"];
    let agents = Agent::ALL
        .into_iter()
        .enumerate()
        .map(|(index, _)| {
            checkbox(agent_names[index], app.agents[index])
                .on_toggle(move |value| Message::AgentChanged(index, value))
                .spacing(12)
                .size(20)
        })
        .fold(column![], |content, item| content.push(item));

    let content = column![
        text("MENVANE / INITIAL SETUP").size(14),
        text("Make continuity local.").size(38),
        text("Configure the runtime, choose agent connections, then start the service once.")
            .size(16),
        text("Storage home").size(14),
        text_input("~/.menvane", &app.home)
            .on_input(Message::HomeChanged)
            .padding(12),
        row![
            column![
                text("Provider").size(14),
                text_input("openai", &app.provider)
                    .on_input(Message::ProviderChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Model").size(14),
                text_input("gpt-5.6-luna", &app.model)
                    .on_input(Message::ModelChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        row![
            column![
                text("Reasoning effort").size(14),
                text_input("medium", &app.reasoning_effort)
                    .on_input(Message::ReasoningChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Provider endpoint").size(14),
                text_input("https://api.openai.com/v1", &app.endpoint)
                    .on_input(Message::EndpointChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        text("API key (optional)").size(14),
        text_input(
            "Stored in config.toml with restricted permissions",
            &app.api_key
        )
        .on_input(Message::ApiKeyChanged)
        .secure(true)
        .padding(12),
        text_input("API key environment variable", &app.api_key_env)
            .on_input(Message::ApiKeyEnvChanged)
            .padding(12),
        checkbox("Enable embeddings", app.embedding_enabled)
            .on_toggle(Message::EmbeddingEnabledChanged)
            .spacing(12)
            .size(20),
        text_input("text-embedding-3-small", &app.embedding_model)
            .on_input(Message::EmbeddingModelChanged)
            .padding(12),
        row![
            column![
                text("Max prompt bytes").size(14),
                text_input("16384", &app.max_prompt_bytes)
                    .on_input(Message::MaxPromptChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Idle finalization seconds").size(14),
                text_input("120", &app.idle_finalize_seconds)
                    .on_input(Message::IdleFinalizeChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        row![
            column![
                text("Max tool input bytes").size(14),
                text_input("4096", &app.max_tool_input_bytes)
                    .on_input(Message::MaxToolInputChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Max tool output bytes").size(14),
                text_input("4096", &app.max_tool_output_bytes)
                    .on_input(Message::MaxToolOutputChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        row![
            column![
                text("Open session timeout seconds").size(14),
                text_input("1800", &app.open_finalize_seconds)
                    .on_input(Message::OpenFinalizeChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Job lease timeout seconds").size(14),
                text_input("300", &app.lease_timeout_seconds)
                    .on_input(Message::LeaseTimeoutChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        row![
            column![
                text("Memory lifetime days").size(14),
                text_input("90", &app.memory_lifetime_days)
                    .on_input(Message::MemoryLifetimeChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Maximum knowledge cards").size(14),
                text_input("3", &app.max_cards)
                    .on_input(Message::MaxCardsChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        row![
            column![
                text("Minimum match confidence").size(14),
                text_input("0.45", &app.min_match_confidence)
                    .on_input(Message::MinMatchChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Minimum knowledge confidence").size(14),
                text_input("0.55", &app.min_knowledge_confidence)
                    .on_input(Message::MinKnowledgeChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
            column![
                text("Minimum utility").size(14),
                text_input("0.55", &app.min_utility)
                    .on_input(Message::MinUtilityChanged)
                    .padding(12),
            ]
            .width(Length::Fill),
        ]
        .spacing(18),
        text("Connect agents").size(14),
        agents,
        text(&app.status).size(14),
        button(text(if app.busy {
            "Applying..."
        } else {
            "Apply setup and start service"
        }))
        .on_press_maybe((!app.busy).then_some(Message::Apply))
        .padding(14),
    ]
    .spacing(16)
    .padding(32);

    container(scrollable(content))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
