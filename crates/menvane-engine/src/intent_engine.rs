use anyhow::Result;
use menvane_domain::{
    EpisodeState, IntentClassificationSource, NormalizedEvent, NormalizedEventKind, PromptIntent,
    PromptIntentKind, TaskEpisode,
};
use menvane_store::{SessionRecord, SessionRepository};
use serde::Serialize;

use crate::retriever::{
    ACTIVE_CONSTRAINT_WEIGHT, ACTIVE_CORRECTION_WEIGHT, ACTIVE_EPISODE_GOAL_WEIGHT,
    CONVERSATION_ROOT_GOAL_WEIGHT, CURRENT_PROMPT_WEIGHT,
};

pub const CLASSIFIER_VERSION: &str = "deterministic-v1";

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ClassifierWeights {
    pub current_prompt: f64,
    pub current_correction: f64,
    pub active_episode_goal: f64,
    pub active_constraints: f64,
    pub refinements: f64,
    pub conversation_root_goal: f64,
    pub previous_episodes: f64,
    pub operational_prompts: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClassifierDiagnostics {
    pub version: &'static str,
    pub weights: ClassifierWeights,
}

impl Default for ClassifierWeights {
    fn default() -> Self {
        Self {
            current_prompt: CURRENT_PROMPT_WEIGHT,
            current_correction: ACTIVE_CORRECTION_WEIGHT,
            active_episode_goal: ACTIVE_EPISODE_GOAL_WEIGHT,
            active_constraints: ACTIVE_CONSTRAINT_WEIGHT,
            refinements: 0.70,
            conversation_root_goal: CONVERSATION_ROOT_GOAL_WEIGHT,
            previous_episodes: 0.10,
            operational_prompts: 0.05,
        }
    }
}

impl ClassifierDiagnostics {
    pub fn current() -> Self {
        Self {
            version: CLASSIFIER_VERSION,
            weights: ClassifierWeights::default(),
        }
    }
}

pub struct IntentEngine<'a> {
    repository: &'a SessionRepository,
}

impl<'a> IntentEngine<'a> {
    pub fn new(repository: &'a SessionRepository) -> Self {
        Self { repository }
    }

    pub fn diagnostics(&self) -> ClassifierDiagnostics {
        ClassifierDiagnostics::current()
    }

    pub fn classify(
        &self,
        event: &NormalizedEvent,
        session: &SessionRecord,
    ) -> Result<Option<PromptIntent>> {
        if event.kind != NormalizedEventKind::UserPrompt {
            return Ok(None);
        }
        if let Ok(intent) = self.repository.prompt_intent(&event.event_id) {
            return Ok(Some(intent));
        }
        let prompt = event.bounded_input.as_deref().unwrap_or_default().trim();
        if prompt.is_empty() {
            return Ok(None);
        }

        let active = self
            .repository
            .list_active_episodes(&session.conversation_key, session.project_id.as_deref())?;
        let current = active.iter().max_by_key(|episode| episode.ordinal).cloned();
        let correction = is_correction(prompt);
        let constraint = is_constraint(prompt) && !correction;
        let operational = is_operational(prompt) && !correction && !constraint;
        let overlap = current
            .as_ref()
            .map(|episode| lexical_overlap(prompt, &episode.goal))
            .unwrap_or_default();
        let topic_change = current.is_some()
            && !correction
            && !constraint
            && !operational
            && is_strong_topic_change(prompt, overlap);

        let (episode, kind) = if current.is_none() {
            let episode = self.root_episode(session, event.event_id.as_str(), prompt)?;
            let kind = if correction {
                PromptIntentKind::Correction
            } else if constraint {
                PromptIntentKind::Constraint
            } else if operational {
                PromptIntentKind::Operational
            } else {
                PromptIntentKind::RootGoal
            };
            (episode, kind)
        } else if topic_change {
            for episode in &active {
                let mut dormant = episode.clone();
                dormant.state = EpisodeState::Dormant;
                dormant.updated_at = event.timestamp;
                self.repository.update_episode(&dormant)?;
            }
            (
                self.repository
                    .create_episode(session.id, &event.event_id, prompt)?,
                PromptIntentKind::NewGoal,
            )
        } else {
            let episode = match current {
                Some(episode) => episode,
                None => unreachable!(),
            };
            let kind = if correction {
                PromptIntentKind::Correction
            } else if constraint {
                PromptIntentKind::Constraint
            } else if operational {
                PromptIntentKind::Operational
            } else if is_refinement(prompt, overlap) {
                PromptIntentKind::Refinement
            } else {
                PromptIntentKind::FollowUp
            };
            (episode, kind)
        };

        let mut episode = episode;
        if correction {
            episode.goal = corrected_goal(prompt);
            episode.updated_at = event.timestamp;
            episode = self.repository.update_episode(&episode)?;
        }
        let intent = PromptIntent {
            event_id: event.event_id.clone(),
            episode_id: episode.id,
            kind,
            confidence: confidence(kind),
            weight: weight(kind),
            classifier_version: CLASSIFIER_VERSION.to_owned(),
            source: IntentClassificationSource::Deterministic,
            classified_at: event.timestamp,
        };
        self.repository.record_prompt_intent(&intent)?;
        Ok(Some(intent))
    }

    pub fn episodes(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<TaskEpisode>> {
        self.repository.list_episodes(conversation_key, project_id)
    }

    pub fn intents(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<PromptIntent>> {
        self.repository
            .list_prompt_intents(conversation_key, project_id)
    }

    fn root_episode(
        &self,
        session: &SessionRecord,
        event_id: &str,
        prompt: &str,
    ) -> Result<TaskEpisode> {
        if let Some(episode) = self.repository.episode_for_root_event(event_id)? {
            return Ok(episode);
        }
        self.repository.create_episode(session.id, event_id, prompt)
    }
}

fn confidence(kind: PromptIntentKind) -> f64 {
    match kind {
        PromptIntentKind::RootGoal => 0.99,
        PromptIntentKind::NewGoal => 0.92,
        PromptIntentKind::Refinement => 0.86,
        PromptIntentKind::Constraint => 0.95,
        PromptIntentKind::Correction => 0.98,
        PromptIntentKind::FollowUp => 0.75,
        PromptIntentKind::Operational => 0.80,
    }
}

fn weight(kind: PromptIntentKind) -> f64 {
    let weights = ClassifierWeights::default();
    match kind {
        PromptIntentKind::RootGoal | PromptIntentKind::NewGoal => weights.current_prompt,
        PromptIntentKind::Refinement => weights.refinements,
        PromptIntentKind::Constraint => weights.active_constraints,
        PromptIntentKind::Correction => weights.current_correction,
        PromptIntentKind::FollowUp => weights.active_episode_goal,
        PromptIntentKind::Operational => weights.operational_prompts,
    }
}

fn is_correction(prompt: &str) -> bool {
    let normalized = prompt.to_ascii_lowercase();
    normalized.starts_with("correction")
        || normalized.starts_with("actually")
        || normalized.starts_with("to clarify")
        || normalized.contains("i meant")
        || normalized.contains("that's incorrect")
        || normalized.contains("that is incorrect")
        || normalized.contains("was wrong")
        || (normalized.starts_with("do not")
            && (normalized.contains("recreate")
                || normalized.contains("retired")
                || normalized.contains("old ")))
}

fn is_constraint(prompt: &str) -> bool {
    let normalized = prompt.to_ascii_lowercase();
    [
        "must ",
        "should ",
        "required",
        "constraint",
        "keep ",
        "without ",
        "never ",
        "only ",
        "disabled by default",
        "do not ",
        "don't ",
        "avoid ",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn is_operational(prompt: &str) -> bool {
    let normalized = prompt.to_ascii_lowercase();
    [
        "status",
        "show ",
        "list ",
        "help",
        "what is the current",
        "which files",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn is_refinement(prompt: &str, overlap: f64) -> bool {
    let normalized = prompt.to_ascii_lowercase();
    overlap > 0.0
        || [
            "also ",
            "add ",
            "include ",
            "continue ",
            "refine ",
            "update ",
            "change ",
            "make ",
            "ensure ",
        ]
        .iter()
        .any(|marker| normalized.starts_with(marker))
}

fn is_strong_topic_change(prompt: &str, overlap: f64) -> bool {
    if overlap > 0.0 || is_uncertain_follow_up(prompt) {
        return false;
    }
    let tokens = meaningful_tokens(prompt);
    tokens.len() >= 2
        && [
            "now ",
            "switch ",
            "separately",
            "new task",
            "review ",
            "please ",
            "work on ",
            "focus on ",
            "look at ",
            "investigate ",
            "design ",
            "document ",
            "fix ",
            "implement ",
            "build ",
            "create ",
            "run ",
        ]
        .iter()
        .any(|marker| prompt.to_ascii_lowercase().starts_with(marker))
}

fn is_uncertain_follow_up(prompt: &str) -> bool {
    let normalized = prompt.to_ascii_lowercase();
    let tokens = meaningful_tokens(prompt);
    tokens.len() <= 3
        || normalized.starts_with("what about")
        || normalized.starts_with("how about")
        || normalized.starts_with("and ")
        || normalized.starts_with("then ")
}

fn lexical_overlap(left: &str, right: &str) -> f64 {
    let left = meaningful_tokens(left);
    let right = meaningful_tokens(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let shared = left.iter().filter(|token| right.contains(token)).count();
    shared as f64 / left.len().min(right.len()) as f64
}

fn meaningful_tokens(value: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "about", "by", "for", "from", "in", "is", "it", "of", "on", "or",
        "the", "to", "with",
    ];
    value
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() > 1 && !STOP_WORDS.contains(&token.as_str()))
        .collect()
}

fn corrected_goal(prompt: &str) -> String {
    let trimmed = prompt.trim();
    for prefix in [
        "correction:",
        "correction,",
        "actually:",
        "actually,",
        "to clarify:",
        "to clarify,",
    ] {
        if trimmed.to_ascii_lowercase().starts_with(prefix) {
            return capitalize_first(trimmed[prefix.len()..].trim());
        }
    }
    capitalize_first(trimmed)
}

fn capitalize_first(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().chain(characters).collect()
}
