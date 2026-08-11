use globset::{Glob, GlobSet, GlobSetBuilder};
use menvane_domain::NormalizedEvent;
use regex::Regex;
use serde::Deserialize;

pub const MAX_RECALL_PROMPT_BYTES: usize = 16_384;
pub const MAX_RECALL_IDENTIFIER_BYTES: usize = 512;
pub const MAX_RECALL_CWD_BYTES: usize = 4_096;

#[derive(Debug, Clone, Deserialize)]
pub struct CaptureSanitizerConfig {
    #[serde(default = "default_prompt_bytes")]
    pub max_prompt_bytes: usize,
    #[serde(default = "default_tool_output_bytes")]
    pub max_tool_output_bytes: usize,
    #[serde(default = "default_tool_input_bytes")]
    pub max_tool_input_bytes: usize,
    #[serde(default = "default_ignore_paths")]
    pub ignore_paths: Vec<String>,
}

impl Default for CaptureSanitizerConfig {
    fn default() -> Self {
        Self {
            max_prompt_bytes: default_prompt_bytes(),
            max_tool_output_bytes: default_tool_output_bytes(),
            max_tool_input_bytes: default_tool_input_bytes(),
            ignore_paths: default_ignore_paths(),
        }
    }
}

pub struct CaptureSanitizer {
    config: CaptureSanitizerConfig,
    ignored_paths: GlobSet,
    authentication_header: Regex,
    likely_secret: Regex,
    secret_assignment: Regex,
}

struct InstructionBlock {
    open: &'static str,
    close: &'static str,
}

const INSTRUCTION_BLOCKS: &[InstructionBlock] = &[
    InstructionBlock {
        open: "<available-skills",
        close: "</available-skills>",
    },
    InstructionBlock {
        open: "<recommended_plugins",
        close: "</recommended_plugins>",
    },
    InstructionBlock {
        open: "<recommended-plugins",
        close: "</recommended-plugins>",
    },
    InstructionBlock {
        open: "<system-prompt",
        close: "</system-prompt>",
    },
    InstructionBlock {
        open: "<agent-instructions",
        close: "</agent-instructions>",
    },
    InstructionBlock {
        open: "<system>",
        close: "</system>",
    },
    InstructionBlock {
        open: "[task handoff]",
        close: "end task handoff",
    },
    InstructionBlock {
        open: "menvane memory context",
        close: "end menvane memory context",
    },
];

const FILTERED_LINES: &[&str] = &[
    "agents.md",
    "skill.md",
    "/skills/",
    "<available-skills",
    "<recommended_plugins",
    "<recommended-plugins",
    "[required active constraint or correction]",
    "[required context]",
    "[relevant excerpt]",
    "[retrieval card]",
    "[session-start memory]",
    "[historical handoff card]",
];

impl CaptureSanitizer {
    pub fn new(config: CaptureSanitizerConfig) -> anyhow::Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &config.ignore_paths {
            builder.add(Glob::new(pattern)?);
        }
        for pattern in instruction_ignore_paths() {
            builder.add(Glob::new(pattern)?);
        }
        Ok(Self {
            config,
            ignored_paths: builder.build()?,
            authentication_header: Regex::new(
                r"(?im)^(authorization|proxy-authorization)\s*:\s*.*$",
            )?,
            likely_secret: Regex::new(
                r"(?i)\b(sk-[a-z0-9_-]{16,}|gh[pousr]_[a-z0-9]{20,}|xox[baprs]-[a-z0-9-]{16,}|eyJ[a-z0-9_-]{10,}\.[a-z0-9_-]{10,}\.[a-z0-9_-]{10,})\b",
            )?,
            secret_assignment: Regex::new(
                r#"(?i)\b(api[_-]?key|access[_-]?token|secret|password)\s*[:=]\s*["']?[^\s,"']+"#,
            )?,
        })
    }

    pub fn sanitize(&self, mut event: NormalizedEvent) -> Option<NormalizedEvent> {
        if event
            .attributed_path
            .as_deref()
            .is_some_and(|path| self.path_is_ignored(path))
        {
            return None;
        }
        let prompt_limit = self.config.max_prompt_bytes.min(MAX_RECALL_PROMPT_BYTES);
        let input_limit = self.config.max_tool_input_bytes;
        let output_limit = self.config.max_tool_output_bytes;
        event.bounded_input = event
            .bounded_input
            .map(|value| {
                let limit = if event.tool_family.is_some() {
                    input_limit
                } else {
                    prompt_limit
                };
                self.clean(&value, limit)
            })
            .and_then(|value| self.filter_content(&value));
        event.bounded_output = event
            .bounded_output
            .map(|value| self.clean(&value, output_limit))
            .and_then(|value| self.filter_content(&value));
        Some(event)
    }

    pub fn filter_durable_event(&self, event: NormalizedEvent) -> Option<NormalizedEvent> {
        if event.is_injected_content() {
            return None;
        }
        let mut event = event;
        event.bounded_input = event
            .bounded_input
            .as_deref()
            .and_then(|value| self.filter_content(value))
            .filter(|value| !value.trim().is_empty());
        event.bounded_output = event
            .bounded_output
            .as_deref()
            .and_then(|value| self.filter_content(value))
            .filter(|value| !value.trim().is_empty());
        if event.is_user_prompt() && event.bounded_input.is_none() {
            return None;
        }
        Some(event)
    }

    pub fn sanitize_prompt(&self, value: &str) -> String {
        self.clean(
            value,
            self.config.max_prompt_bytes.min(MAX_RECALL_PROMPT_BYTES),
        )
    }

    pub fn path_is_ignored(&self, path: &str) -> bool {
        let normalized = path.replace('\\', "/");
        self.ignored_paths.is_match(&normalized)
            || std::path::Path::new(&normalized)
                .file_name()
                .is_some_and(|name| self.ignored_paths.is_match(name))
            || normalized
                .split_once('/')
                .is_some_and(|(_, relative)| self.ignored_paths.is_match(relative))
    }

    pub fn filter_content(&self, value: &str) -> Option<String> {
        let mut filtered = Vec::new();
        let mut open_block: Option<&'static str> = None;
        for line in value.lines() {
            let trimmed = line.trim_start().to_ascii_lowercase();
            if let Some(block) = INSTRUCTION_BLOCKS
                .iter()
                .find(|block| trimmed.contains(block.open))
                .map(|block| block.close)
            {
                if trimmed.contains(block) {
                    continue;
                }
                open_block = Some(block);
                continue;
            }
            if let Some(close) = open_block {
                if trimmed.contains(close) {
                    open_block = None;
                }
                continue;
            }
            if FILTERED_LINES
                .iter()
                .any(|marker| trimmed.contains(marker))
            {
                continue;
            }
            filtered.push(line);
        }
        let filtered = filtered.join("\n");
        (!filtered.trim().is_empty()).then_some(filtered)
    }

    fn clean(&self, value: &str, limit: usize) -> String {
        let value = self
            .authentication_header
            .replace_all(value, "$1: [REDACTED]");
        let value = self.likely_secret.replace_all(&value, "[REDACTED]");
        let value = self.secret_assignment.replace_all(&value, "$1=[REDACTED]");
        truncate_utf8(&value, limit)
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let marker = "\n[TRUNCATED]";
    if max_bytes <= marker.len() {
        return marker[..max_bytes].to_owned();
    }
    let content_limit = max_bytes.saturating_sub(marker.len());
    let mut boundary = content_limit;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}{}", &value[..boundary], marker)
}

fn default_prompt_bytes() -> usize {
    16_384
}

fn default_tool_output_bytes() -> usize {
    4_096
}

fn default_tool_input_bytes() -> usize {
    4_096
}

fn default_ignore_paths() -> Vec<String> {
    [".env", ".env.*", "**/secrets/**", "**/.ssh/**"]
        .into_iter()
        .map(str::to_owned)
        .chain(instruction_ignore_paths().into_iter().map(str::to_owned))
        .collect()
}

fn instruction_ignore_paths() -> [&'static str; 6] {
    [
        "AGENTS.md",
        "**/AGENTS.md",
        "SKILL.md",
        "**/SKILL.md",
        "skills/**",
        "**/skills/**",
    ]
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use menvane_domain::{NormalizedEvent, NormalizedEventKind, NormalizedEventRole};

    use super::*;

    #[test]
    fn drops_ignored_paths_and_redacts_secrets() {
        let sanitizer = CaptureSanitizer::new(CaptureSanitizerConfig::default()).unwrap();
        let mut ignored = event();
        ignored.attributed_path = Some("project/secrets/token.txt".to_owned());
        assert!(sanitizer.sanitize(ignored).is_none());
        let mut retained = event();
        retained.bounded_output = Some(
            "Authorization: Bearer secret\napi_key=very-secret\nsk-12345678901234567890".to_owned(),
        );
        let cleaned = sanitizer.sanitize(retained).unwrap();
        let output = cleaned.bounded_output.unwrap();
        assert!(!output.contains("very-secret"));
        assert!(!output.contains("12345678901234567890"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn drops_agent_and_skill_instruction_paths() {
        let sanitizer = CaptureSanitizer::new(CaptureSanitizerConfig::default()).unwrap();
        for path in [
            "AGENTS.md",
            "/home/user/project/AGENTS.md",
            "/home/user/.agents/skills/browser-control/SKILL.md",
            "/home/user/project/skills/custom/instructions.md",
        ] {
            let mut ignored = event();
            ignored.attributed_path = Some(path.to_owned());
            assert!(sanitizer.sanitize(ignored).is_none(), "{path}");
        }
    }

    #[test]
    fn removes_instruction_blocks_and_ignored_paths_from_content() {
        let sanitizer = CaptureSanitizer::new(CaptureSanitizerConfig::default()).unwrap();
        let mut captured = event();
        captured.bounded_input = Some(
            "Implement the export\n<available-skills>\n- browser-control\n</available-skills>\nContinue the export"
                .to_owned(),
        );
        captured.bounded_output = Some("read AGENTS.md\nsafe result".to_owned());
        let captured = sanitizer.sanitize(captured).unwrap();
        assert_eq!(
            captured.bounded_input.as_deref(),
            Some("Implement the export\nContinue the export")
        );
        assert_eq!(captured.bounded_output.as_deref(), Some("safe result"));
    }

    #[test]
    fn removes_menvane_and_handoff_delimited_context_from_composed_fields() {
        let sanitizer = CaptureSanitizer::new(CaptureSanitizerConfig::default()).unwrap();
        let mut captured = event();
        captured.kind = NormalizedEventKind::UserPrompt;
        captured.role = NormalizedEventRole::UserPrompt;
        captured.origin = Default::default();
        captured.bounded_input = Some(
            "Implement the export\nMENVANE MEMORY CONTEXT\nHistorical context only.\n[TASK HANDOFF]\nGoal: x\nEND TASK HANDOFF\nEND MENVANE MEMORY CONTEXT\nContinue the export"
                .to_owned(),
        );
        let captured = sanitizer.filter_durable_event(captured).unwrap();
        assert_eq!(
            captured.bounded_input.as_deref(),
            Some("Implement the export\nContinue the export")
        );
    }

    #[test]
    fn fully_injected_prompt_is_dropped_from_the_durable_session() {
        let sanitizer = CaptureSanitizer::new(CaptureSanitizerConfig::default()).unwrap();
        let mut captured = event();
        captured.kind = NormalizedEventKind::UserPrompt;
        captured.role = NormalizedEventRole::UserPrompt;
        captured.origin = Default::default();
        captured.bounded_input =
            Some("MENVANE MEMORY CONTEXT\nEND MENVANE MEMORY CONTEXT".to_owned());
        assert!(sanitizer.filter_durable_event(captured).is_none());

        let mut injected = event();
        injected.kind = NormalizedEventKind::UserPrompt;
        injected.harness_injected = true;
        injected.bounded_input = Some("system instructions".to_owned());
        assert!(sanitizer.filter_durable_event(injected).is_none());
    }

    fn event() -> NormalizedEvent {
        NormalizedEvent {
            event_id: "event-1".to_owned(),
            kind: NormalizedEventKind::ToolCompleted,
            origin: Default::default(),
            role: Default::default(),
            client: "test".to_owned(),
            external_session_id: "session-1".to_owned(),
            timestamp: Utc::now(),
            cwd: "/tmp/project".to_owned(),
            project_id: None,
            tool_family: Some("shell".to_owned()),
            bounded_input: None,
            bounded_output: None,
            attributed_path: None,
            success: Some(true),
            model: None,
            harness_injected: false,
        }
    }
}
