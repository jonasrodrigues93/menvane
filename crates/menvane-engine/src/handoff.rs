use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use chrono::Utc;
use menvane_domain::{
    HandoffStatus, HandoffValidation, NormalizedEventKind, PromptIntentKind, TaskHandoff,
};
use menvane_store::{
    EpisodeEvent, MAX_HANDOFF_CHANGED_FILES, MAX_HANDOFF_ITEM_BYTES, MAX_HANDOFF_SOURCE_EVENTS,
    SessionRepository,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Menvane, RetrievalMode, RetrievalScope, Retriever};

pub struct HandoffGenerator<'a> {
    menvane: &'a Menvane,
}

impl<'a> HandoffGenerator<'a> {
    pub fn new(menvane: &'a Menvane) -> Self {
        Self { menvane }
    }

    pub fn generate(&self, episode_id: Uuid) -> Result<TaskHandoff> {
        let episode = self.menvane.sessions.episode(episode_id)?;
        let events = self.menvane.sessions.episode_events(episode_id)?;
        let latest = events
            .last()
            .ok_or_else(|| anyhow::anyhow!("episode has no linked evidence"))?;
        let cwd = Path::new(&latest.event.cwd);
        let repository = repository_state(cwd, &events);
        let project_name = self
            .menvane
            .all_projects()
            .unwrap_or_default()
            .into_iter()
            .find(|project| episode.project_id.as_deref() == Some(project.id.as_str()))
            .map(|project| project.identity)
            .unwrap_or_else(|| {
                episode
                    .project_id
                    .clone()
                    .unwrap_or_else(|| "global".to_owned())
            });
        let source_event_ids = source_event_ids(&episode.root_event_id, &events);
        let validation = validation(&events);
        let successful_tools = unique_tool_summaries(&events, true);
        let failed_tools = unique_tool_summaries(&events, false);
        let decisions = decisions(&self.menvane.sessions, &events);
        let pending_work = pending_work(&successful_tools, &failed_tools, &validation);
        let blockers = failed_tools.clone();
        let current_state =
            current_state(&project_name, &episode, latest, events.len(), &repository);
        let next_action = pending_work
            .first()
            .cloned()
            .or_else(|| Some("Review the current task state.".to_owned()));
        let relevant_memory_ids = self.relevant_memory_ids(cwd, &episode);
        let existing = self.menvane.sessions.handoff_for_episode(episode_id)?;
        let now = Utc::now();
        let mut handoff = TaskHandoff {
            id: existing
                .as_ref()
                .map_or_else(Uuid::now_v7, |value| value.id),
            project_id: episode.project_id.clone(),
            conversation_key: episode.conversation_key.clone(),
            episode_id,
            source_session_id: latest.session_id,
            source_client: latest.client.clone(),
            status: if matches!(
                latest.event.kind,
                NormalizedEventKind::TurnStopped
                    | NormalizedEventKind::SessionEnded
                    | NormalizedEventKind::ContextCompacted
            ) {
                HandoffStatus::Ready
            } else {
                HandoffStatus::Active
            },
            goal: bounded(&episode.goal, 2_048),
            current_state: bounded(&current_state, 4_096),
            completed_work: successful_tools,
            pending_work,
            next_action: next_action.map(|value| bounded(&value, 4_096)),
            blockers,
            changed_files: repository.changed_files,
            decisions,
            validation,
            relevant_memory_ids,
            source_event_ids,
            git_head: repository.git_head,
            worktree_state_hash: repository.worktree_state_hash,
            created_at: existing.as_ref().map_or(now, |value| value.created_at),
            updated_at: existing.as_ref().map_or(now, |value| value.updated_at),
        };
        if let Some(existing) = &existing {
            let mut comparable = handoff.clone();
            comparable.created_at = existing.created_at;
            comparable.updated_at = existing.updated_at;
            if comparable != *existing {
                handoff.updated_at = now;
            }
        }
        self.menvane.sessions.create_or_update_handoff(&handoff)
    }

    fn relevant_memory_ids(&self, cwd: &Path, episode: &menvane_domain::TaskEpisode) -> Vec<Uuid> {
        let project = self.menvane.ensure_project(cwd).ok().flatten();
        let scope = if project.is_some() {
            RetrievalScope::Project
        } else {
            RetrievalScope::Global
        };
        Retriever::new(&self.menvane.index)
            .retrieve(
                &episode.goal,
                project.as_ref(),
                scope,
                RetrievalMode::Explicit,
                false,
                8,
            )
            .map(|values| values.into_iter().map(|value| value.id).collect())
            .unwrap_or_default()
    }
}

struct RepositoryState {
    changed_files: Vec<String>,
    git_head: Option<String>,
    worktree_state_hash: Option<String>,
}

fn repository_state(cwd: &Path, events: &[EpisodeEvent]) -> RepositoryState {
    let cwd_text = cwd.to_string_lossy().into_owned();
    let status = Command::new("git")
        .args([
            "-C",
            &cwd_text,
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .output();
    let Some(status) = status.ok().filter(|value| value.status.success()) else {
        return RepositoryState {
            changed_files: attributed_files(events),
            git_head: None,
            worktree_state_hash: None,
        };
    };
    let changed_files = changed_files_from_git_status(&status.stdout);
    let git_head = Command::new("git")
        .args(["-C", &cwd_text, "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|value| value.status.success())
        .map(|value| String::from_utf8_lossy(&value.stdout).trim().to_owned())
        .filter(|value| !value.is_empty());
    let mut hasher = Sha256::new();
    hasher.update(&status.stdout);
    let worktree_state_hash = Some(hex::encode(hasher.finalize()));
    RepositoryState {
        changed_files,
        git_head,
        worktree_state_hash,
    }
}

fn attributed_files(events: &[EpisodeEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|value| value.event.attributed_path.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|value| bounded(&value, MAX_HANDOFF_ITEM_BYTES))
        .take(MAX_HANDOFF_CHANGED_FILES)
        .collect()
}

fn changed_files_from_git_status(status: &[u8]) -> Vec<String> {
    let records = status
        .split(|value| *value == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut paths = BTreeSet::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 || record[2] != b' ' {
            index += 1;
            continue;
        }
        paths.insert(String::from_utf8_lossy(&record[3..]).into_owned());
        let renamed = matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
        if renamed {
            if let Some(previous) = records.get(index + 1) {
                paths.insert(String::from_utf8_lossy(previous).into_owned());
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    paths
        .into_iter()
        .map(|value| bounded(&value, MAX_HANDOFF_ITEM_BYTES))
        .take(MAX_HANDOFF_CHANGED_FILES)
        .collect()
}

fn source_event_ids(root_event_id: &str, events: &[EpisodeEvent]) -> Vec<String> {
    if events.len() <= MAX_HANDOFF_SOURCE_EVENTS {
        return events
            .iter()
            .map(|value| value.event.event_id.clone())
            .collect();
    }
    let mut ids = events
        .iter()
        .find(|value| value.event.event_id == root_event_id)
        .map(|value| vec![value.event.event_id.clone()])
        .unwrap_or_default();
    for event in events.iter().rev() {
        if ids.len() == MAX_HANDOFF_SOURCE_EVENTS {
            break;
        }
        if !ids.iter().any(|id| id == &event.event.event_id) {
            ids.push(event.event.event_id.clone());
        }
    }
    ids
}

fn unique_tool_summaries(events: &[EpisodeEvent], success: bool) -> Vec<String> {
    events
        .iter()
        .filter(|value| {
            value.event.kind == NormalizedEventKind::ToolCompleted
                && value.event.success == Some(success)
        })
        .filter_map(|value| value.event.tool_family.as_deref())
        .map(|value| format!("{} {}", value, if success { "succeeded" } else { "failed" }))
        .map(|value| bounded(&value, 1_024))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(32)
        .collect()
}

fn pending_work(
    successful_tools: &[String],
    failed_tools: &[String],
    validation: &[HandoffValidation],
) -> Vec<String> {
    if !failed_tools.is_empty() {
        return failed_tools
            .iter()
            .map(|value| format!("Resolve {value}."))
            .collect();
    }
    if validation.iter().any(|value| !value.success) {
        return vec!["Resolve the failed validation.".to_owned()];
    }
    if successful_tools.is_empty() {
        vec!["Begin work on the current goal.".to_owned()]
    } else {
        vec!["Continue the current goal and validate the next change.".to_owned()]
    }
}

fn decisions(repository: &SessionRepository, events: &[EpisodeEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|value| value.event.kind == NormalizedEventKind::UserPrompt)
        .filter_map(|value| {
            repository
                .prompt_intent(&value.event.event_id)
                .ok()
                .map(|intent| (value, intent))
        })
        .filter(|(_, intent)| {
            matches!(
                intent.kind,
                PromptIntentKind::Constraint | PromptIntentKind::Correction
            )
        })
        .filter_map(|(value, _)| value.event.bounded_input.as_deref())
        .map(|value| bounded(value, 1_024))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(32)
        .collect()
}

fn validation(events: &[EpisodeEvent]) -> Vec<HandoffValidation> {
    events
        .iter()
        .filter(|value| {
            value.event.kind == NormalizedEventKind::ToolCompleted
                && value
                    .event
                    .tool_family
                    .as_deref()
                    .is_some_and(is_validation_tool)
                && value.event.success.is_some()
        })
        .map(|value| HandoffValidation {
            event_id: value.event.event_id.clone(),
            command: value
                .event
                .tool_family
                .as_deref()
                .map(|input| bounded(input, 1_024)),
            success: value.event.success == Some(true),
            summary: bounded(
                &format!(
                    "{} {}",
                    value.event.tool_family.as_deref().unwrap_or("validation"),
                    if value.event.success == Some(true) {
                        "succeeded"
                    } else {
                        "failed"
                    }
                ),
                4_096,
            ),
            timestamp: value.event.timestamp,
        })
        .take(32)
        .collect()
}

fn current_state(
    project_name: &str,
    episode: &menvane_domain::TaskEpisode,
    latest: &EpisodeEvent,
    event_count: usize,
    repository: &RepositoryState,
) -> String {
    format!(
        "Project: {project_name}; conversation: {}; session: {} generation {}; client: {}; episode: {}; linked events: {event_count}; latest event: {:?}; repository changed files: {}.",
        episode.conversation_key,
        latest.external_session_id,
        latest.generation,
        latest.client,
        episode.id,
        latest.event.kind,
        if repository.changed_files.is_empty() {
            "none"
        } else {
            "present"
        }
    )
}

fn bounded(value: &str, limit: usize) -> String {
    let value = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(|line| {
            let lowercase = line.to_ascii_lowercase();
            [
                "authorization:",
                "api_key=",
                "api-key=",
                "access_token=",
                "access-token=",
                "password=",
                "secret=",
            ]
            .iter()
            .find_map(|marker| {
                lowercase
                    .find(marker)
                    .map(|index| format!("{}[REDACTED]", &line[..index + marker.len()]))
            })
            .unwrap_or_else(|| line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate_utf8(&value, limit)
}

fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn is_validation_tool(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    tool.contains("test") || tool.contains("build") || tool.contains("check")
}

#[cfg(test)]
mod tests {
    use super::{changed_files_from_git_status, truncate_utf8};

    #[test]
    fn parses_nul_delimited_changes_and_renames() {
        let status = b" M src/current.rs\0R  src/new.rs\0src/old.rs\0?? odd name.rs\0";
        assert_eq!(
            changed_files_from_git_status(status),
            vec![
                "odd name.rs".to_owned(),
                "src/current.rs".to_owned(),
                "src/new.rs".to_owned(),
                "src/old.rs".to_owned(),
            ]
        );
    }

    #[test]
    fn truncates_on_utf8_byte_boundaries() {
        assert_eq!(truncate_utf8("ééé", 5), "éé");
        assert_eq!(truncate_utf8("ééé", 4), "éé");
        assert_eq!(truncate_utf8("ééé", 3), "é");
    }
}
