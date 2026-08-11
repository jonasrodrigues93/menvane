use std::path::Path;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{Duration, Utc};
use menvane_domain::{
    Applicability, Memory, MemoryMetadata, MemoryType, NormalizedEvent, NormalizedEventKind, Scope,
};
use menvane_store::{JobRecord, SessionRecord};

use crate::IntentEngine;
use crate::Menvane;
use crate::evidence::{MAX_SESSION_MARKDOWN_BYTES, render_session_markdown};
use crate::sanitizer::CaptureSanitizer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    Dropped,
    Duplicate,
    Stored,
}

pub struct SessionEngine<'a> {
    menvane: &'a Menvane,
}

impl<'a> SessionEngine<'a> {
    pub fn new(menvane: &'a Menvane) -> Self {
        Self { menvane }
    }

    pub fn ingest(&self, mut event: NormalizedEvent) -> Result<CaptureOutcome> {
        let project = self.menvane.ensure_project(Path::new(&event.cwd))?;
        event.project_id = project.as_ref().map(|project| project.id.clone());
        let result = self
            .menvane
            .sessions
            .ingest(&event, project.as_ref().map(|project| project.id.as_str()))?;
        IntentEngine::new(&self.menvane.sessions).classify(&event, &result.session)?;
        let episode_id = self
            .menvane
            .sessions
            .associate_event_with_active_episode(&event.event_id)?;
        if !result.inserted {
            return Ok(CaptureOutcome::Duplicate);
        }
        if let Some(episode_id) = episode_id
            && let Some(debounce) = checkpoint_debounce(
                &event,
                self.menvane
                    .config
                    .handoff
                    .nonvalidation_tool_debounce_seconds,
            )
        {
            if debounce.is_zero() {
                self.menvane
                    .sessions
                    .mark_handoff_dirty(episode_id, debounce)?;
            } else {
                self.menvane.sessions.mark_handoff_dirty_at(
                    episode_id,
                    debounce,
                    event.timestamp,
                )?;
            }
        }
        Ok(CaptureOutcome::Stored)
    }

    pub fn finalize_idle(&self, idle_seconds: u64) -> Result<usize> {
        let seconds = i64::try_from(idle_seconds)?;
        let sessions = self
            .menvane
            .sessions
            .finalize_idle_before(Utc::now() - Duration::seconds(seconds))?;
        Ok(sessions.len())
    }

    pub fn finalize_job(&self, job: &JobRecord) -> Result<()> {
        let session_id = job.dedupe_key.parse()?;
        let session = self.menvane.sessions.session(session_id)?;
        self.finalize_session(&session, job)
    }

    fn finalize_session(&self, session: &SessionRecord, job: &JobRecord) -> Result<()> {
        if let Some(path) = &session.markdown_path {
            self.menvane.sessions.mark_finalized(
                session.id,
                path,
                job.id,
                job.owner.as_deref().unwrap_or_default(),
            )?;
            return Ok(());
        }
        let sanitizer = CaptureSanitizer::new(self.menvane.config.capture.clone())?;
        let durable = self
            .menvane
            .sessions
            .events(session.id)?
            .into_iter()
            .filter_map(|event| sanitizer.filter_durable_event(event))
            .collect::<Vec<_>>();
        let project = session
            .project_id
            .as_deref()
            .map(|project_id| {
                self.menvane
                    .markdown
                    .project_files()?
                    .into_iter()
                    .map(|path| self.menvane.markdown.parse_project(&path))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .find(|project| project.id == project_id)
                    .ok_or_else(|| anyhow::anyhow!("session project metadata is missing"))
            })
            .transpose()?;
        let title = session_title(session, &durable);
        let mut metadata = MemoryMetadata::new(
            MemoryType::Session,
            if project.is_some() {
                Scope::Project
            } else {
                Scope::Global
            },
            project.as_ref().map(|project| project.id.clone()),
            1.0,
            Vec::new(),
            Applicability::default(),
        );
        metadata.id = session.id;
        metadata.created_at = session.started_at;
        metadata.updated_at = session.ended_at.unwrap_or(session.last_event_at);
        metadata.last_verified_at = None;
        metadata.client = Some(session.client.clone());
        metadata.external_session_id = Some(session.external_session_id.clone());
        metadata.started_at = Some(session.started_at);
        metadata.ended_at = session.ended_at;
        metadata.imported = Some(session.imported);
        metadata.generation = Some(session.generation);
        let memory = Memory {
            metadata,
            title,
            body: render_session_markdown(&durable, MAX_SESSION_MARKDOWN_BYTES),
        };
        let path = self
            .menvane
            .markdown
            .write_memory(&memory, project.as_ref())?;
        self.menvane.index.upsert_memory(&memory, &path)?;
        self.menvane.sessions.mark_finalized(
            session.id,
            &path,
            job.id,
            job.owner.as_deref().unwrap_or_default(),
        )?;
        self.menvane
            .markdown
            .commit(&format!("feat(session): finalize {}", session.id));
        Ok(())
    }
}

fn checkpoint_debounce(event: &NormalizedEvent, nonvalidation_seconds: u64) -> Option<StdDuration> {
    match event.kind {
        NormalizedEventKind::UserPrompt if event.is_user_prompt() => event
            .bounded_input
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(|_| StdDuration::ZERO),
        NormalizedEventKind::UserPrompt => None,
        NormalizedEventKind::ToolCompleted => Some(
            if event.tool_family.as_deref().is_some_and(is_validation_tool) {
                StdDuration::ZERO
            } else {
                StdDuration::from_secs(nonvalidation_seconds)
            },
        ),
        NormalizedEventKind::ContextCompacted
        | NormalizedEventKind::TurnStopped
        | NormalizedEventKind::SessionEnded => Some(StdDuration::ZERO),
        NormalizedEventKind::SessionStarted => None,
    }
}

fn is_validation_tool(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    tool.contains("test") || tool.contains("build") || tool.contains("check")
}

fn session_title(session: &SessionRecord, events: &[NormalizedEvent]) -> String {
    events
        .iter()
        .find(|event| event.is_user_prompt())
        .and_then(|event| event.bounded_input.as_deref())
        .and_then(|prompt| prompt.lines().find(|line| !line.trim().is_empty()))
        .map(|line| line.trim().chars().take(100).collect())
        .unwrap_or_else(|| {
            format!(
                "{} session {} generation {}",
                session.client, session.external_session_id, session.generation
            )
        })
}

pub fn is_session_worth_compiling(memory: &Memory) -> bool {
    !is_system_noise_title(&memory.title) && has_meaningful_evidence(&memory.body)
}

fn is_system_noise_title(title: &str) -> bool {
    let title = title.trim();
    title == "<available-skills>" || (title.starts_with('<') && title.ends_with('>'))
}

fn has_meaningful_evidence(body: &str) -> bool {
    !body.trim().is_empty()
}