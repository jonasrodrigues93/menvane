use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use menvane_domain::{
    EpisodeEvidencePacket, EvidenceItem, EvidenceKind, NormalizedEventKind, PromptIntent,
    PromptIntentKind, TaskEpisode,
};
use menvane_store::EpisodeEvent;
use uuid::Uuid;

use crate::CaptureSanitizer;

pub const DEFAULT_EVIDENCE_BUDGET_BYTES: usize = 32_768;
pub const MAX_SESSION_MARKDOWN_BYTES: usize = 32_768;

pub struct EvidenceBuilder {
    budget_bytes: usize,
}

impl EvidenceBuilder {
    pub fn new(budget_bytes: usize) -> Self {
        Self { budget_bytes }
    }

    pub fn build(
        &self,
        episode: &TaskEpisode,
        events: &[EpisodeEvent],
        intents: &[PromptIntent],
    ) -> Result<EpisodeEvidencePacket> {
        if events.is_empty() {
            bail!("episode has no linked evidence");
        }
        let sanitizer = CaptureSanitizer::new(Default::default())?;
        let event_by_id = events
            .iter()
            .map(|episode_event| (episode_event.event.event_id.as_str(), episode_event))
            .collect::<HashMap<_, _>>();
        let root = event_by_id
            .get(episode.root_event_id.as_str())
            .copied()
            .filter(|episode_event| episode_event.event.is_user_prompt())
            .contextualize("episode root event is not an allowed user prompt")?;
        let intent_by_event = intents
            .iter()
            .map(|intent| (intent.event_id.as_str(), intent.kind))
            .collect::<HashMap<_, _>>();
        let mut candidates = CandidateSet::new(self.budget_bytes, episode.id);

        let goal = root
            .event
            .bounded_input
            .as_deref()
            .and_then(|value| sanitizer.filter_content(value))
            .or_else(|| sanitizer.filter_content(&episode.goal))
            .contextualize("episode goal has no allowed content")?;
        candidates
            .required
            .push(item(&root.event, EvidenceKind::Goal, &goal, 1.0));

        let mut prompts = Vec::new();
        for episode_event in events.iter().filter(|episode_event| {
            episode_event.event.is_user_prompt()
                && episode_event.event.event_id != episode.root_event_id
        }) {
            let kind = intent_by_event
                .get(episode_event.event.event_id.as_str())
                .copied()
                .unwrap_or(PromptIntentKind::FollowUp);
            let priority = match kind {
                PromptIntentKind::Correction => 1.00,
                PromptIntentKind::Constraint => 0.95,
                PromptIntentKind::NewGoal => 0.90,
                PromptIntentKind::Refinement => 0.80,
                PromptIntentKind::FollowUp => 0.55,
                PromptIntentKind::Operational => 0.20,
                PromptIntentKind::RootGoal => 0.90,
            };
            if let Some(input) = episode_event
                .event
                .bounded_input
                .as_deref()
                .and_then(|value| sanitizer.filter_content(value))
            {
                prompts.push((
                    priority,
                    episode_event.event.timestamp,
                    item(&episode_event.event, EvidenceKind::Prompt, &input, priority),
                ));
            }
        }
        prompts.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.event_id.cmp(&right.2.event_id))
        });
        for (priority, _, value) in prompts {
            if priority >= 0.90 {
                candidates.priority_prompts.push(value);
            } else {
                candidates.prompts.push(value);
            }
        }

        let mut actions = BTreeMap::<String, Vec<&EpisodeEvent>>::new();
        for episode_event in events.iter().filter(|episode_event| {
            episode_event.event.kind == NormalizedEventKind::ToolCompleted
                && episode_event.event.is_allowed_evidence()
                && episode_event
                    .event
                    .attributed_path
                    .as_deref()
                    .is_none_or(|path| !sanitizer.path_is_ignored(path))
        }) {
            let event = &episode_event.event;
            let key = format!(
                "{}\0{}\0{}",
                event.tool_family.as_deref().unwrap_or("tool"),
                event.attributed_path.as_deref().unwrap_or(""),
                event
                    .success
                    .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
            );
            actions.entry(key).or_default().push(episode_event);
        }
        for group in actions.into_values() {
            let representative = group
                .iter()
                .rev()
                .find(|episode_event| episode_event.event.success == Some(true))
                .copied()
                .unwrap_or(group[group.len() - 1]);
            let event = &representative.event;
            let family = event.tool_family.as_deref().unwrap_or("tool");
            let status = match event.success {
                Some(true) => "succeeded",
                Some(false) => "failed",
                None => "completed",
            };
            let path = event
                .attributed_path
                .as_deref()
                .map(|value| format!(" on {value}"))
                .unwrap_or_default();
            let content = if group.len() == 1 {
                format!("{family} {status}{path}")
            } else {
                format!("{family} {status}{path} ({} repetitions)", group.len())
            };
            let priority = if event.success == Some(true) && event.attributed_path.is_some() {
                0.72
            } else if event.success == Some(true) {
                0.45
            } else {
                0.30
            };
            candidates.actions.push((
                priority,
                item(event, EvidenceKind::Action, &content, priority),
            ));
            if event.success == Some(true)
                && !is_validation_tool(family)
                && event
                    .bounded_output
                    .as_deref()
                    .and_then(|value| sanitizer.filter_content(value))
                    .is_some_and(|value| is_outcome_output(&value))
            {
                candidates.discoveries.push(item(
                    event,
                    EvidenceKind::Discovery,
                    &format!(
                        "{family} outcome: {}",
                        excerpt(
                            &sanitizer
                                .filter_content(event.bounded_output.as_deref().unwrap_or_default())
                                .unwrap_or_default(),
                            768,
                        )
                    ),
                    0.68,
                ));
            }
        }
        candidates.actions.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.1.timestamp.cmp(&left.1.timestamp))
        });

        let tool_events = events
            .iter()
            .filter(|episode_event| {
                episode_event.event.kind == NormalizedEventKind::ToolCompleted
                    && episode_event.event.is_allowed_evidence()
                    && episode_event
                        .event
                        .attributed_path
                        .as_deref()
                        .is_none_or(|path| !sanitizer.path_is_ignored(path))
            })
            .collect::<Vec<_>>();
        for episode_event in &tool_events {
            let event = &episode_event.event;
            if event.success != Some(false) {
                continue;
            }
            let family = event.tool_family.as_deref().unwrap_or("tool");
            let attempted = event
                .bounded_input
                .as_deref()
                .and_then(|value| sanitizer.filter_content(value))
                .map(|value| format!(" attempted {value}"))
                .unwrap_or_default();
            let output = event
                .bounded_output
                .as_deref()
                .and_then(|value| sanitizer.filter_content(value))
                .map(|value| format!(": {}", excerpt(&value, 1_024)))
                .unwrap_or_default();
            let resolution = tool_events
                .iter()
                .filter(|later| later.event.timestamp > event.timestamp)
                .find(|later| {
                    later.event.success == Some(true)
                        && later.event.tool_family == event.tool_family
                        && (event.attributed_path.is_none()
                            || later.event.attributed_path == event.attributed_path)
                })
                .map(|later| format!("; later resolved by [event:{}]", later.event.event_id))
                .unwrap_or_default();
            candidates.errors.push(item(
                event,
                EvidenceKind::Error,
                &format!("{family}{attempted} failed{output}{resolution}"),
                0.93,
            ));
        }

        for episode_event in &tool_events {
            let event = &episode_event.event;
            let Some(family) = event.tool_family.as_deref() else {
                continue;
            };
            if event.success == Some(true) && is_validation_tool(family) {
                let input = event
                    .bounded_input
                    .as_deref()
                    .and_then(|value| sanitizer.filter_content(value))
                    .map(|value| format!(" command: {value}"))
                    .unwrap_or_default();
                let output = event
                    .bounded_output
                    .as_deref()
                    .and_then(|value| sanitizer.filter_content(value))
                    .filter(|value| is_outcome_output(value))
                    .map(|value| format!(": {}", excerpt(&value, 768)))
                    .unwrap_or_default();
                candidates.validations.push(item(
                    event,
                    EvidenceKind::Validation,
                    &format!("{family} succeeded{input}{output}"),
                    0.88,
                ));
            }
        }

        for event in events.iter().filter_map(|episode_event| {
            let event = &episode_event.event;
            (event.is_user_prompt() && event.is_allowed_evidence()).then_some(event)
        }) {
            if event
                .bounded_input
                .as_deref()
                .is_some_and(is_decision_prompt)
                && let Some(input) = event.bounded_input.as_deref()
            {
                if let Some(input) = sanitizer.filter_content(input) {
                    candidates
                        .decisions
                        .push(item(event, EvidenceKind::Decision, &input, 0.65));
                }
            }
            if event
                .bounded_input
                .as_deref()
                .is_some_and(is_unresolved_question)
                && let Some(input) = event.bounded_input.as_deref()
            {
                if let Some(input) = sanitizer.filter_content(input) {
                    candidates.unresolved_questions.push(item(
                        event,
                        EvidenceKind::UnresolvedQuestion,
                        &input,
                        0.75,
                    ));
                }
            }
        }

        let mut files = events
            .iter()
            .filter(|episode_event| episode_event.event.is_allowed_evidence())
            .filter_map(|episode_event| episode_event.event.attributed_path.as_deref())
            .filter(|path| !path.trim().is_empty())
            .filter(|path| !sanitizer.path_is_ignored(path))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        candidates.files = files;
        candidates.finish()
    }
}

struct CandidateSet {
    budget_bytes: usize,
    episode_id: Uuid,
    required: Vec<EvidenceItem>,
    priority_prompts: Vec<EvidenceItem>,
    prompts: Vec<EvidenceItem>,
    actions: Vec<(f64, EvidenceItem)>,
    decisions: Vec<EvidenceItem>,
    discoveries: Vec<EvidenceItem>,
    errors: Vec<EvidenceItem>,
    validations: Vec<EvidenceItem>,
    compaction_context: Vec<EvidenceItem>,
    unresolved_questions: Vec<EvidenceItem>,
    files: Vec<String>,
}

impl CandidateSet {
    fn new(budget_bytes: usize, episode_id: Uuid) -> Self {
        Self {
            budget_bytes,
            episode_id,
            required: Vec::new(),
            priority_prompts: Vec::new(),
            prompts: Vec::new(),
            actions: Vec::new(),
            decisions: Vec::new(),
            discoveries: Vec::new(),
            errors: Vec::new(),
            validations: Vec::new(),
            compaction_context: Vec::new(),
            unresolved_questions: Vec::new(),
            files: Vec::new(),
        }
    }

    fn finish(self) -> Result<EpisodeEvidencePacket> {
        let mut packet = EpisodeEvidencePacket {
            episode_id: self.episode_id,
            goal: self
                .required
                .into_iter()
                .next()
                .contextualize("episode goal is missing")?,
            prompts: Vec::new(),
            actions: Vec::new(),
            decisions: Vec::new(),
            discoveries: Vec::new(),
            errors: Vec::new(),
            validations: Vec::new(),
            files: Vec::new(),
            compaction_context: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        fit_goal(&mut packet, self.budget_bytes)?;
        let mut used = evidence_size(&packet)?;
        let mut append = |items: Vec<EvidenceItem>, target: &mut Vec<EvidenceItem>| -> Result<()> {
            for item in items {
                let separator = usize::from(!target.is_empty());
                if let Some(item) =
                    fit_item(item, self.budget_bytes.saturating_sub(used + separator))?
                {
                    used += serialized_size(&item)? + separator;
                    target.push(item);
                }
            }
            Ok(())
        };
        append(self.priority_prompts, &mut packet.prompts)?;
        append(self.errors, &mut packet.errors)?;
        append(
            self.actions.into_iter().map(|(_, item)| item).collect(),
            &mut packet.actions,
        )?;
        append(self.validations, &mut packet.validations)?;
        append(self.decisions, &mut packet.decisions)?;
        append(self.discoveries, &mut packet.discoveries)?;
        append(self.prompts, &mut packet.prompts)?;
        append(self.unresolved_questions, &mut packet.unresolved_questions)?;
        append(self.compaction_context, &mut packet.compaction_context)?;
        for file in self.files {
            let separator = usize::from(!packet.files.is_empty());
            let size = serialized_size(&file)? + separator;
            if used + size <= self.budget_bytes {
                used += size;
                packet.files.push(file);
            }
        }
        Ok(packet)
    }
}

fn fit_goal(packet: &mut EpisodeEvidencePacket, budget: usize) -> Result<()> {
    if evidence_size(packet)? <= budget {
        return Ok(());
    }
    let original = std::mem::take(&mut packet.goal.content);
    if evidence_size(packet)? > budget {
        bail!("evidence budget is too small for required episode metadata");
    }
    packet.goal.content = fit_content(&original, |content| {
        packet.goal.content = content.to_owned();
        evidence_size(packet).map(|size| size <= budget)
    })?;
    Ok(())
}

fn fit_item(mut item: EvidenceItem, available: usize) -> Result<Option<EvidenceItem>> {
    if serialized_size(&item)? <= available {
        return Ok(Some(item));
    }
    let original = std::mem::take(&mut item.content);
    if serialized_size(&item)? > available {
        return Ok(None);
    }
    item.content = fit_content(&original, |content| {
        item.content = content.to_owned();
        serialized_size(&item).map(|size| size <= available)
    })?;
    Ok(Some(item))
}

fn fit_content(original: &str, mut fits: impl FnMut(&str) -> Result<bool>) -> Result<String> {
    let boundaries = original
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(original.len()))
        .collect::<Vec<_>>();
    let suffix = if fits(" [TRUNCATED]")? {
        " [TRUNCATED]"
    } else {
        ""
    };
    let mut low = 0;
    let mut high = boundaries.len();
    while low + 1 < high {
        let middle = (low + high) / 2;
        let candidate = format!("{}{suffix}", &original[..boundaries[middle]]);
        if fits(&candidate)? {
            low = middle;
        } else {
            high = middle;
        }
    }
    let candidate = format!("{}{suffix}", &original[..boundaries[low]]);
    if fits(&candidate)? {
        Ok(candidate)
    } else {
        Ok(String::new())
    }
}

fn evidence_size(packet: &EpisodeEvidencePacket) -> Result<usize> {
    serialized_size(packet)
}

fn serialized_size(value: &impl serde::Serialize) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len())
}

fn item(
    event: &menvane_domain::NormalizedEvent,
    kind: EvidenceKind,
    content: &str,
    importance: f64,
) -> EvidenceItem {
    EvidenceItem {
        event_id: event.event_id.clone(),
        kind,
        timestamp: event.timestamp,
        content: excerpt(content, 2_048),
        importance,
    }
}

fn is_validation_tool(family: &str) -> bool {
    let family = family.to_ascii_lowercase();
    family.contains("test") || family.contains("build") || family.contains("check")
}

fn is_outcome_output(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    [
        "passed", "pass", "success", "created", "updated", "changed", "wrote", "complete",
        "finished", "verified",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

fn is_decision_prompt(prompt: &str) -> bool {
    let prompt = prompt.to_ascii_lowercase();
    [
        "decide",
        "choose",
        "use ",
        "prefer ",
        "rather than",
        "instead of",
    ]
    .iter()
    .any(|marker| prompt.contains(marker))
}

fn is_unresolved_question(prompt: &str) -> bool {
    let normalized = prompt.to_ascii_lowercase();
    prompt.trim_end().ends_with('?')
        || [
            "unresolved",
            "open question",
            "still need",
            "blocker",
            "pending",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    if max_chars == 0 {
        return String::new();
    }
    let mut excerpt = characters.by_ref().take(max_chars).collect::<String>();
    if characters.next().is_some() {
        let suffix = " [TRUNCATED]";
        let content_limit = max_chars.saturating_sub(suffix.chars().count());
        excerpt = value.chars().take(content_limit).collect();
        excerpt.push_str(suffix);
    }
    excerpt
}

pub fn render_episode_markdown(packet: &EpisodeEvidencePacket, max_bytes: usize) -> String {
    let mut body = format!("## Task episode {}\n\n", packet.episode_id);
    append_markdown_section(&mut body, "Goal", std::slice::from_ref(&packet.goal));
    let outcome = packet
        .validations
        .iter()
        .rev()
        .find(|item| item.content.contains("succeeded"))
        .or_else(|| {
            packet
                .actions
                .iter()
                .rev()
                .find(|item| item.content.contains("succeeded"))
        })
        .cloned()
        .unwrap_or_else(|| EvidenceItem {
            event_id: packet.goal.event_id.clone(),
            kind: EvidenceKind::Discovery,
            timestamp: packet.goal.timestamp,
            content: "No successful outcome was captured.".to_owned(),
            importance: 0.0,
        });
    append_markdown_section(&mut body, "Outcome", &[outcome]);
    append_markdown_section(&mut body, "Actions", &packet.actions);
    append_markdown_section(&mut body, "Decisions", &packet.decisions);
    append_markdown_section(&mut body, "Discoveries", &packet.discoveries);
    append_markdown_section(&mut body, "Errors", &packet.errors);
    append_markdown_section(&mut body, "Validation", &packet.validations);
    append_files_section(&mut body, packet);
    append_markdown_section(
        &mut body,
        "Unresolved questions",
        &packet.unresolved_questions,
    );
    bounded_string(&body, max_bytes)
}

pub fn render_session_markdown(packets: &[EpisodeEvidencePacket], max_bytes: usize) -> String {
    if packets.is_empty() {
        return "## Task episodes\n\nNo task episode was linked to this session.".to_owned();
    }
    let section_budget = (max_bytes / packets.len()).max(1);
    let body = packets
        .iter()
        .map(|packet| render_episode_markdown(packet, section_budget))
        .collect::<Vec<_>>()
        .join("\n\n");
    bounded_string(&body, max_bytes)
}

fn append_markdown_section(body: &mut String, title: &str, items: &[EvidenceItem]) {
    body.push_str("### ");
    body.push_str(title);
    body.push_str("\n\n");
    if items.is_empty() {
        body.push_str("None captured.\n\n");
        return;
    }
    for item in items {
        body.push_str("- [event:");
        body.push_str(&item.event_id);
        body.push_str("] ");
        body.push_str(item.content.trim());
        body.push('\n');
    }
    body.push('\n');
}

fn append_files_section(body: &mut String, packet: &EpisodeEvidencePacket) {
    body.push_str("### Files\n\n");
    if packet.files.is_empty() {
        body.push_str("None captured.\n\n");
        return;
    }
    for file in &packet.files {
        let event_id = packet
            .actions
            .iter()
            .find(|item| item.content.contains(file))
            .map(|item| item.event_id.as_str())
            .unwrap_or(packet.goal.event_id.as_str());
        body.push_str("- [event:");
        body.push_str(event_id);
        body.push_str("] ");
        body.push_str(file);
        body.push('\n');
    }
    body.push('\n');
}

fn bounded_string(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

trait Contextualize<T> {
    fn contextualize(self, message: &str) -> Result<T>;
}

impl<T> Contextualize<T> for Option<T> {
    fn contextualize(self, message: &str) -> Result<T> {
        self.ok_or_else(|| anyhow::anyhow!(message.to_owned()))
    }
}
