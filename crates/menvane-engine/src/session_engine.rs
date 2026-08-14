use std::path::Path;

use anyhow::Result;
use chrono::{Duration, Utc};
use menvane_domain::{NormalizedEvent, SessionMetadata, SummaryStatus};
use menvane_store::{JobRecord, SessionRecord};

use crate::Menvane;
use crate::sanitizer::CaptureSanitizer;
use crate::session_rendering::{MAX_SESSION_MARKDOWN_BYTES, render_session_markdown};

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
        event.project_id = project.as_ref().map(|value| value.id.clone());
        let result = self
            .menvane
            .sessions
            .ingest(&event, event.project_id.as_deref())?;
        Ok(if result.inserted {
            CaptureOutcome::Stored
        } else {
            CaptureOutcome::Duplicate
        })
    }

    pub fn finalize_idle(&self, idle_seconds: u64) -> Result<usize> {
        let seconds = i64::try_from(idle_seconds)?;
        Ok(self
            .menvane
            .sessions
            .finalize_idle_before(Utc::now() - Duration::seconds(seconds))?
            .len())
    }

    pub fn finalize_job(&self, job: &JobRecord) -> Result<()> {
        let session = self.menvane.sessions.session(job.dedupe_key.parse()?)?;
        self.finalize_session(&session, job)
    }

    fn finalize_session(&self, session: &SessionRecord, job: &JobRecord) -> Result<()> {
        if let Some(path) = &session.markdown_path {
            self.menvane.sessions.mark_finalized(
                session.id,
                path,
                job.id,
                job.owner.as_deref().unwrap_or_default(),
                session.summary_status,
            )?;
            return Ok(());
        }
        let sanitizer = CaptureSanitizer::new(self.menvane.config.capture.clone())?;
        let events = self
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
                    .all_projects()?
                    .into_iter()
                    .find(|value| value.id == project_id)
                    .ok_or_else(|| anyhow::anyhow!("session project metadata is missing"))
            })
            .transpose()?;
        let metadata = SessionMetadata {
            id: session.id,
            client: session.client.clone(),
            external_session_id: session.external_session_id.clone(),
            project_id: session.project_id.clone(),
            started_at: Some(session.started_at),
            ended_at: session.ended_at.or(Some(session.last_event_at)),
            imported: session.imported,
            generation: session.generation,
            summary_status: SummaryStatus::Pending,
            summary: None,
        };
        let markdown = render_session_markdown(&events, MAX_SESSION_MARKDOWN_BYTES);
        let summary_status = if is_session_worth_compiling(&events) {
            SummaryStatus::Pending
        } else {
            SummaryStatus::Skipped
        };
        let metadata = SessionMetadata {
            summary_status,
            ..metadata
        };
        let path = self
            .menvane
            .markdown
            .write_session(&metadata, &markdown, project.as_ref())?;
        self.menvane.sessions.mark_finalized(
            session.id,
            &path,
            job.id,
            job.owner.as_deref().unwrap_or_default(),
            summary_status,
        )?;
        self.menvane
            .markdown
            .commit(&format!("feat(session): finalize {}", session.id));
        Ok(())
    }
}

pub fn is_session_worth_compiling(events: &[NormalizedEvent]) -> bool {
    events.iter().any(|event| {
        event.is_consolidation_eligible()
            && (event
                .bounded_input
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
                || event
                    .bounded_output
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty()))
    })
}
