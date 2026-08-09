use std::path::Path;

use anyhow::Result;
use chrono::{Duration, Utc};
use menvane_domain::{
    Applicability, Memory, MemoryMetadata, MemoryType, NormalizedEvent, NormalizedEventKind, Scope,
};
use menvane_store::{JobRecord, SessionRecord};

use crate::Menvane;

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
        if !result.inserted {
            return Ok(CaptureOutcome::Duplicate);
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
            let memory = self
                .menvane
                .index
                .read_memory(&self.menvane.markdown, session.id)?
                .0;
            self.menvane.sessions.mark_finalized(
                session.id,
                path,
                is_session_worth_compiling(&memory),
                job.id,
                job.owner.as_deref().unwrap_or_default(),
            )?;
            return Ok(());
        }
        let events = self.menvane.sessions.events(session.id)?;
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
        let title = session_title(session, &events);
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
            body: session_body(&events),
        };
        let path = self
            .menvane
            .markdown
            .write_memory(&memory, project.as_ref())?;
        self.menvane.index.upsert_memory(&memory, &path)?;
        self.menvane.sessions.mark_finalized(
            session.id,
            &path,
            is_session_worth_compiling(&memory),
            job.id,
            job.owner.as_deref().unwrap_or_default(),
        )?;
        self.menvane
            .markdown
            .commit(&format!("feat(session): finalize {}", session.id));
        Ok(())
    }
}

fn session_title(session: &SessionRecord, events: &[NormalizedEvent]) -> String {
    events
        .iter()
        .find(|event| event.kind == NormalizedEventKind::UserPrompt)
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

fn session_body(events: &[NormalizedEvent]) -> String {
    let goal = events
        .iter()
        .find(|event| event.kind == NormalizedEventKind::UserPrompt)
        .and_then(|event| event.bounded_input.as_deref())
        .map(|value| excerpt(value, 1_000))
        .unwrap_or_else(|| "No explicit goal was captured.".to_owned());
    let actions = events
        .iter()
        .filter(|event| event.kind == NormalizedEventKind::ToolCompleted)
        .map(|event| {
            let family = event.tool_family.as_deref().unwrap_or("tool");
            let result = match event.success {
                Some(true) => "succeeded",
                Some(false) => "failed",
                None => "completed",
            };
            format!("- {family} {result}")
        })
        .collect::<Vec<_>>();
    let errors = events
        .iter()
        .filter(|event| event.success == Some(false))
        .filter_map(|event| event.bounded_output.as_deref())
        .map(|output| format!("- {}", excerpt(output, 500)))
        .collect::<Vec<_>>();
    let validation = events
        .iter()
        .filter(|event| event.success == Some(true))
        .filter_map(|event| event.tool_family.as_deref())
        .filter(|family| {
            let family = family.to_ascii_lowercase();
            family.contains("test") || family.contains("build") || family.contains("check")
        })
        .map(|family| format!("- {family} succeeded"))
        .collect::<Vec<_>>();
    let files = events
        .iter()
        .filter_map(|event| event.attributed_path.as_deref())
        .map(|path| format!("- {path}"))
        .collect::<Vec<_>>();
    format!(
        "## Goal\n\n{goal}\n\n## Outcome\n\nSession evidence was captured and finalized.\n\n## Important actions\n\n{}\n\n## Decisions\n\nNo explicit decisions were extracted during deterministic capture.\n\n## Errors and discoveries\n\n{}\n\n## Validation\n\n{}\n\n## Files involved\n\n{}",
        section(&actions),
        section(&errors),
        section(&validation),
        section(&files)
    )
}

fn section(values: &[String]) -> String {
    if values.is_empty() {
        "None captured.".to_owned()
    } else {
        values.join("\n")
    }
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut excerpt = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        excerpt.push_str(" [TRUNCATED]");
    }
    excerpt
}

pub fn is_session_worth_compiling(memory: &Memory) -> bool {
    !is_system_noise_title(&memory.title) && has_meaningful_evidence(&memory.body)
}

fn is_system_noise_title(title: &str) -> bool {
    let title = title.trim();
    title == "<available-skills>" || (title.starts_with('<') && title.ends_with('>'))
}

fn has_meaningful_evidence(body: &str) -> bool {
    let body = body.trim();
    if body.len() < 120 {
        return false;
    }
    let no_goal = body.contains("## Goal\n\nNo explicit goal was captured.");
    let no_actions = body.contains("## Important actions\n\nNone captured.");
    let no_errors = body.contains("## Errors and discoveries\n\nNone captured.");
    let no_validation = body.contains("## Validation\n\nNone captured.");
    let no_files = body.contains("## Files involved\n\nNone captured.");
    !(no_goal && no_actions && no_errors && no_validation && no_files)
}
