mod compiler;
mod decay;
mod global_promoter;
mod intent_engine;
mod oauth_provider;
mod project_resolver;
mod providers;
mod retriever;
mod sanitizer;
mod session_engine;
mod technology_detector;

use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;
use menvane_domain::{
    Applicability, JsonSchema, LlmProvider, LlmRequest, Memory, MemoryMetadata, MemoryType,
    NormalizedEvent, NormalizedEventKind, NormalizedSession, Project, PromptIntent, ProviderHealth,
    ReinforcementSignal, Scope, TaskEpisode,
};
pub use menvane_store::mark_forgotten;
pub use menvane_store::{
    IndexStore, InjectionIdentity, IntegrationRecord, JobRecord, MarkdownStore, OrphanRecord,
    SearchResult, SessionRepository,
};
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

pub use compiler::{CompilationInput, CompilationResult, CompiledMemory, MemoryCompiler};
pub use decay::DecayEngine;
pub use global_promoter::GlobalPromoter;
pub use intent_engine::{
    CLASSIFIER_VERSION, ClassifierDiagnostics, ClassifierWeights, IntentEngine,
};
pub use oauth_provider::OpenAiOAuthProvider;
pub use project_resolver::{ProjectResolution, ProjectResolver, normalize_git_remote};
pub use providers::{CodexProvider, OpenAIApiProvider, OpenRouterProvider, ProviderChain};
pub use retriever::{
    ACTIVE_CONSTRAINT_WEIGHT, ACTIVE_CORRECTION_WEIGHT, ACTIVE_EPISODE_GOAL_WEIGHT,
    CONVERSATION_ROOT_GOAL_WEIGHT, CURRENT_PROMPT_WEIGHT, RETRIEVAL_RRF_K, RecallDiagnostics,
    RecallQueryDiagnostic, RecallResultDiagnostic, RecallSourceDiagnostic,
};
pub use retriever::{RetrievalMode, RetrievalScope, Retriever};
pub use sanitizer::{
    CaptureSanitizer, CaptureSanitizerConfig, MAX_RECALL_CWD_BYTES, MAX_RECALL_IDENTIFIER_BYTES,
    MAX_RECALL_PROMPT_BYTES,
};
pub use session_engine::{CaptureOutcome, SessionEngine, is_session_worth_compiling};
pub use technology_detector::TechnologyDetector;

pub struct WriteMemory {
    pub title: String,
    pub body: String,
    pub memory_type: MemoryType,
    pub scope: Scope,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub applies_to: Applicability,
}

pub struct PromptRecall {
    pub results: Vec<SearchResult>,
    pub diagnostics: RecallDiagnostics,
    pub required_context: Vec<String>,
    pub identity: InjectionIdentity,
}

pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

pub struct DoctorCheck {
    pub name: &'static str,
    pub healthy: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported,
    AlreadyImported,
    Orphan,
}

impl DoctorReport {
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|check| check.healthy)
    }
}

pub struct Menvane {
    home: PathBuf,
    markdown: MarkdownStore,
    index: IndexStore,
    sessions: SessionRepository,
    config: MenvaneConfig,
    worker_owner: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MenvaneConfig {
    #[serde(default)]
    capture: CaptureSanitizerConfig,
    #[serde(default)]
    sessions: SessionConfiguration,
    #[serde(default)]
    jobs: JobConfiguration,
    #[serde(default)]
    llm: LlmConfiguration,
}

#[derive(Debug, Clone, Deserialize)]
struct LlmConfiguration {
    #[serde(default = "default_llm_provider")]
    provider: String,
    #[serde(default = "default_llm_model")]
    model: String,
    #[serde(default = "default_openrouter_url")]
    base_url: String,
    #[serde(default = "default_openrouter_key_env")]
    api_key_env: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default = "default_oauth_issuer")]
    oauth_issuer: String,
    #[serde(default = "default_oauth_endpoint")]
    oauth_endpoint: String,
    #[serde(default)]
    fallback: Option<Box<LlmConfiguration>>,
}

impl Default for LlmConfiguration {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            model: default_llm_model(),
            base_url: default_openrouter_url(),
            api_key_env: default_openrouter_key_env(),
            reasoning_effort: Some("medium".to_owned()),
            oauth_issuer: default_oauth_issuer(),
            oauth_endpoint: default_oauth_endpoint(),
            fallback: None,
        }
    }
}

fn default_llm_provider() -> String {
    "openai".to_owned()
}

fn default_llm_model() -> String {
    "gpt-5.6-luna".to_owned()
}

fn default_openrouter_url() -> String {
    "https://api.openai.com/v1".to_owned()
}

fn default_openrouter_key_env() -> String {
    "OPENAI_API_KEY".to_owned()
}

fn default_oauth_issuer() -> String {
    "https://auth.openai.com".to_owned()
}

fn default_oauth_endpoint() -> String {
    "https://chatgpt.com/backend-api/codex/responses".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
struct SessionConfiguration {
    #[serde(default = "default_idle_finalize_seconds")]
    idle_finalize_seconds: u64,
}

impl Default for SessionConfiguration {
    fn default() -> Self {
        Self {
            idle_finalize_seconds: default_idle_finalize_seconds(),
        }
    }
}

fn default_idle_finalize_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, Deserialize)]
struct JobConfiguration {
    #[serde(default = "default_job_lease_timeout_seconds")]
    lease_timeout_seconds: u64,
}

impl Default for JobConfiguration {
    fn default() -> Self {
        Self {
            lease_timeout_seconds: default_job_lease_timeout_seconds(),
        }
    }
}

fn default_job_lease_timeout_seconds() -> u64 {
    300
}

impl Menvane {
    pub fn from_environment() -> Result<Self> {
        let home = match env::var_os("MENVANE_HOME") {
            Some(path) => PathBuf::from(path),
            None => env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set; set MENVANE_HOME explicitly")?
                .join(".menvane"),
        };
        Self::new(home)
    }

    pub fn new(home: impl Into<PathBuf>) -> Result<Self> {
        let home = home.into();
        let markdown = MarkdownStore::new(&home);
        markdown.initialize()?;
        let config: MenvaneConfig = toml::from_str(&fs::read_to_string(home.join("config.toml"))?)?;
        let index = IndexStore::new(home.join("index.sqlite"));
        index.initialize()?;
        let sessions = SessionRepository::new(home.join("state.sqlite"));
        sessions.initialize_with_legacy(Some(&home.join("index.sqlite")))?;
        Ok(Self {
            home,
            markdown,
            index,
            sessions,
            config,
            worker_owner: Uuid::now_v7().to_string(),
        })
    }

    pub fn write(&self, cwd: &Path, request: WriteMemory) -> Result<Memory> {
        if request.title.trim().is_empty() {
            bail!("memory title cannot be empty");
        }
        if !(0.0..=1.0).contains(&request.confidence) {
            bail!("confidence must be between 0 and 1");
        }
        if request.memory_type == MemoryType::Session {
            bail!("sessions are created by session capture, not manual writes");
        }
        let project = match request.scope {
            Scope::Project => self.ensure_project(cwd)?,
            Scope::Global => None,
        };
        let scope = if project.is_some() {
            request.scope
        } else {
            Scope::Global
        };
        let project_id = project.as_ref().map(|project| project.id.clone());
        let memory = Memory {
            metadata: MemoryMetadata::new(
                request.memory_type,
                scope,
                project_id,
                request.confidence,
                request.tags,
                request.applies_to,
            ),
            title: request.title.trim().to_owned(),
            body: format_memory_body(request.memory_type, &request.body),
        };
        let path = self.markdown.write_memory(&memory, project.as_ref())?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown
            .commit(&format!("feat(memory): write {}", memory.metadata.id));
        Ok(memory)
    }

    pub fn search(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_with_sessions(cwd, query, scope, limit, false)
    }

    pub fn search_with_sessions(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
        include_sessions: bool,
    ) -> Result<Vec<SearchResult>> {
        let project = match scope {
            ScopeSelection::Global => None,
            ScopeSelection::Auto | ScopeSelection::Project => self.ensure_project(cwd)?,
        };
        let retrieval_scope = match scope {
            ScopeSelection::Auto => RetrievalScope::Auto,
            ScopeSelection::Project => RetrievalScope::Project,
            ScopeSelection::Global => RetrievalScope::Global,
        };
        let results = Retriever::new(&self.index).retrieve(
            query,
            project.as_ref(),
            retrieval_scope,
            RetrievalMode::Explicit,
            include_sessions,
            limit,
        )?;
        for memory in &results {
            self.sessions
                .record_access(memory.id, ReinforcementSignal::Retrieved)?;
        }
        Ok(results)
    }

    pub fn recall(&self, cwd: &Path, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let project = self.ensure_project(cwd)?;
        let results = Retriever::new(&self.index).retrieve(
            query,
            project.as_ref(),
            RetrievalScope::Auto,
            RetrievalMode::Automatic,
            false,
            limit,
        )?;
        for memory in &results {
            self.sessions
                .record_access(memory.id, ReinforcementSignal::Retrieved)?;
        }
        Ok(results)
    }

    pub fn prompt_recall(
        &self,
        cwd: &Path,
        client: &str,
        external_session_id: &str,
        prompt: &str,
        limit: usize,
    ) -> Result<PromptRecall> {
        let prompt = CaptureSanitizer::new(self.config.capture.clone())?.sanitize_prompt(prompt);
        let project = self.ensure_project(cwd)?;
        let identity = self.sessions.injection_identity(
            client,
            external_session_id,
            project.as_ref().map(|value| value.id.as_str()),
        )?;
        if prompt.trim().is_empty() {
            return Ok(PromptRecall {
                results: Vec::new(),
                diagnostics: RecallDiagnostics {
                    rrf_k: RETRIEVAL_RRF_K,
                    queries: Vec::new(),
                    results: Vec::new(),
                },
                required_context: Vec::new(),
                identity,
            });
        }
        let context = self.sessions.recall_context(
            client,
            external_session_id,
            project.as_ref().map(|value| value.id.as_str()),
        )?;
        let required_context = context.as_ref().map_or_else(Vec::new, |context| {
            context
                .active_corrections
                .iter()
                .chain(context.active_constraints.iter())
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .fold(Vec::new(), |mut values, value| {
                    if !values.iter().any(|existing| existing == &value) {
                        values.push(value);
                    }
                    values
                })
        });
        let (results, diagnostics) = Retriever::new(&self.index).retrieve_intent(
            &prompt,
            context.as_ref(),
            project.as_ref(),
            limit,
        )?;
        for memory in &results {
            self.sessions
                .record_access(memory.id, ReinforcementSignal::Retrieved)?;
        }
        Ok(PromptRecall {
            results,
            diagnostics,
            required_context,
            identity,
        })
    }

    pub fn read(&self, id: Uuid) -> Result<Memory> {
        let memory = self.index.read_memory(&self.markdown, id)?.0;
        self.sessions
            .record_access(id, ReinforcementSignal::ExplicitlyRead)?;
        Ok(memory)
    }

    pub fn forget(&self, id: Uuid) -> Result<Memory> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, id)?;
        mark_forgotten(&mut memory);
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown.commit(&format!("chore(memory): forget {id}"));
        Ok(memory)
    }

    pub fn reindex(&self) -> Result<(usize, usize)> {
        let _lock = acquire_daemon_lock(&self.home)?;
        self.index.reindex(&self.markdown)
    }

    pub fn ingest_event(&self, event: NormalizedEvent) -> Result<CaptureOutcome> {
        let Some(event) = self.sanitize_event(event)? else {
            return Ok(CaptureOutcome::Dropped);
        };
        SessionEngine::new(self).ingest(event)
    }

    pub fn episodes(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<TaskEpisode>> {
        IntentEngine::new(&self.sessions).episodes(conversation_key, project_id)
    }

    pub fn prompt_intents(
        &self,
        conversation_key: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<PromptIntent>> {
        IntentEngine::new(&self.sessions).intents(conversation_key, project_id)
    }

    pub fn classifier_diagnostics(&self) -> ClassifierDiagnostics {
        IntentEngine::new(&self.sessions).diagnostics()
    }

    pub fn sanitize_event(&self, event: NormalizedEvent) -> Result<Option<NormalizedEvent>> {
        Ok(CaptureSanitizer::new(self.config.capture.clone())?.sanitize(event))
    }

    pub fn finalize_idle_sessions(&self) -> Result<usize> {
        SessionEngine::new(self).finalize_idle(self.config.sessions.idle_finalize_seconds)
    }

    pub fn jobs(&self) -> Result<Vec<JobRecord>> {
        self.sessions.jobs()
    }

    pub fn session_briefing(&self, cwd: &Path, session_key: &str) -> Result<String> {
        self.session_briefing_for_client(cwd, "legacy", session_key)
    }

    pub fn session_briefing_for_client(
        &self,
        cwd: &Path,
        client: &str,
        external_session_id: &str,
    ) -> Result<String> {
        let project = self.ensure_project(cwd)?;
        let mut identity = self.sessions.injection_identity(
            client,
            external_session_id,
            project.as_ref().map(|value| value.id.as_str()),
        )?;
        identity.episode_id = None;
        let prefix = project.as_ref().map_or_else(
            || "Scope: global\n".to_owned(),
            |project| {
                let technologies = [
                    project.technologies.languages.join(", "),
                    project.technologies.frameworks.join(", "),
                    project.technologies.tools.join(", "),
                    project.technologies.databases.join(", "),
                    project.technologies.platforms.join(", "),
                ]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
                format!(
                    "Project: {}\nTechnologies: {}\n",
                    project.identity,
                    if technologies.is_empty() {
                        "none detected"
                    } else {
                        &technologies
                    }
                )
            },
        );
        let memories = Retriever::new(&self.index).briefing(project.as_ref(), 20)?;
        for memory in &memories {
            self.sessions
                .record_access(memory.id, ReinforcementSignal::Retrieved)?;
        }
        let context = self.render_briefing(&prefix, &memories, &identity, 2_500)?;
        if self.sessions.claim_briefing(&identity)? {
            Ok(context)
        } else {
            Ok(String::new())
        }
    }

    pub fn prompt_context(&self, cwd: &Path, prompt: &str, session_key: &str) -> Result<String> {
        self.prompt_context_for_client(cwd, "direct", session_key, prompt)
            .map(|value| value.0)
    }

    pub fn prompt_context_for_session(
        &self,
        cwd: &Path,
        client: &str,
        external_session_id: &str,
        prompt: &str,
        _session_key: &str,
    ) -> Result<(String, RecallDiagnostics)> {
        self.prompt_context_for_client(cwd, client, external_session_id, prompt)
    }

    pub fn prompt_context_for_client(
        &self,
        cwd: &Path,
        client: &str,
        external_session_id: &str,
        prompt: &str,
    ) -> Result<(String, RecallDiagnostics)> {
        let recall = self.prompt_recall(cwd, client, external_session_id, prompt, 20)?;
        let context = self.render_prompt_context(
            &recall.required_context,
            &recall.results,
            &recall.diagnostics,
            &recall.identity,
            6_000,
        )?;
        Ok((context, recall.diagnostics))
    }

    fn render_briefing(
        &self,
        prefix: &str,
        memories: &[SearchResult],
        identity: &InjectionIdentity,
        max_chars: usize,
    ) -> Result<String> {
        let mut context = String::from(
            "MENVANE MEMORY CONTEXT\nHistorical context only.\nCurrent user instructions and current repository state are authoritative.\n\n",
        );
        context.push_str(prefix);
        for memory in memories {
            let entry = format_memory_entry(
                memory,
                "SESSION-START MEMORY",
                "selected for session-start briefing",
                true,
            );
            if !fits_context(&context, &entry, max_chars) {
                break;
            }
            self.append_claimed_memory(&mut context, &entry, identity, memory.id)?;
        }
        context.push_str("\nEND MENVANE MEMORY CONTEXT");
        Ok(context)
    }

    fn render_prompt_context(
        &self,
        required_context: &[String],
        results: &[SearchResult],
        diagnostics: &RecallDiagnostics,
        identity: &InjectionIdentity,
        max_chars: usize,
    ) -> Result<String> {
        if required_context.is_empty() && results.is_empty() {
            return Ok(String::new());
        }
        let mut context = String::from(
            "MENVANE MEMORY CONTEXT\nHistorical context only.\nCurrent user instructions and current repository state are authoritative.\n\n",
        );
        let mut required_used = 0;
        let mut included = false;
        for value in required_context {
            let entry = format!("\n[REQUIRED ACTIVE CONSTRAINT OR CORRECTION]\n{}\n", value);
            if required_used + entry.chars().count() > REQUIRED_CONTEXT_BUDGET
                || !fits_context(&context, &entry, max_chars)
            {
                break;
            }
            context.push_str(&entry);
            required_used += entry.chars().count();
            included = true;
        }
        let mut required_remaining = REQUIRED_CONTEXT_BUDGET.saturating_sub(required_used);
        let mut required_gotchas = HashSet::new();
        for memory in results
            .iter()
            .filter(|memory| memory.memory_type == "gotcha")
        {
            let entry = format_memory_entry(
                memory,
                "REQUIRED CONTEXT",
                "critical gotcha selected for required context",
                true,
            );
            if entry.chars().count() > required_remaining
                || !fits_context(&context, &entry, max_chars)
            {
                break;
            }
            if self.append_claimed_memory(&mut context, &entry, identity, memory.id)? {
                required_remaining -= entry.chars().count();
                included = true;
                required_gotchas.insert(memory.id);
            }
        }
        let relevant_budget = RELEVANT_CONTEXT_BUDGET + required_remaining;
        let mut relevant_used = 0;
        let other_memories = results
            .iter()
            .filter(|memory| {
                memory.memory_type != "gotcha" || !required_gotchas.contains(&memory.id)
            })
            .collect::<Vec<_>>();
        for memory in other_memories.iter().take(6) {
            let entry = format_memory_entry(
                memory,
                "RELEVANT EXCERPT",
                relevance_reason(memory, diagnostics),
                true,
            );
            if relevant_used + entry.chars().count() > relevant_budget
                || !fits_context(&context, &entry, max_chars)
            {
                break;
            }
            if self.append_claimed_memory(&mut context, &entry, identity, memory.id)? {
                relevant_used += entry.chars().count();
                included = true;
            }
        }
        let cards_budget = RETRIEVAL_CARD_BUDGET + relevant_budget.saturating_sub(relevant_used);
        let mut cards_used = 0;
        for memory in other_memories.iter().skip(6) {
            let entry = format_memory_entry(
                memory,
                "RETRIEVAL CARD",
                relevance_reason(memory, diagnostics),
                false,
            );
            if cards_used + entry.chars().count() > cards_budget
                || !fits_context(&context, &entry, max_chars)
            {
                break;
            }
            if self.append_claimed_memory(&mut context, &entry, identity, memory.id)? {
                cards_used += entry.chars().count();
                included = true;
            }
        }
        if !included {
            return Ok(String::new());
        }
        context.push_str("\nEND MENVANE MEMORY CONTEXT");
        if context.chars().count() > max_chars {
            bail!("rendered recall context exceeded its character budget");
        }
        Ok(context)
    }

    fn append_claimed_memory(
        &self,
        context: &mut String,
        entry: &str,
        identity: &InjectionIdentity,
        memory_id: Uuid,
    ) -> Result<bool> {
        let previous_length = context.len();
        context.push_str(entry);
        if !self.sessions.claim_injection(identity, memory_id)? {
            context.truncate(previous_length);
            return Ok(false);
        }
        self.sessions
            .record_access(memory_id, ReinforcementSignal::Injected)?;
        Ok(true)
    }

    pub fn set_integration_connected(&self, client: &str, connected: bool) -> Result<()> {
        self.sessions.set_integration_connected(client, connected)
    }

    pub fn record_procedure_application(
        &self,
        id: Uuid,
        source_session: Uuid,
        success: bool,
    ) -> Result<Memory> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, id)?;
        if memory.metadata.memory_type != MemoryType::Procedure {
            bail!("memory {id} is not a procedure");
        }
        if !self
            .sessions
            .record_procedure_application(id, source_session, success)?
        {
            return Ok(memory);
        }
        if success {
            self.sessions
                .record_access(id, ReinforcementSignal::SuccessfullyApplied)?;
            memory.metadata.successes = Some(memory.metadata.successes.unwrap_or(0) + 1);
            memory.metadata.last_verified_at = Some(Utc::now());
            if memory.metadata.successes.unwrap_or(0) >= 2 {
                memory.metadata.status = menvane_domain::MemoryStatus::Active;
            }
        } else {
            self.sessions
                .record_access(id, ReinforcementSignal::FailedApplication)?;
            memory.metadata.failures = Some(memory.metadata.failures.unwrap_or(0) + 1);
        }
        if !memory.metadata.source_sessions.contains(&source_session) {
            memory.metadata.source_sessions.push(source_session);
        }
        memory.metadata.updated_at = Utc::now();
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown
            .commit(&format!("feat(procedure): reinforce {id}"));
        Ok(memory)
    }

    pub fn promote_global_memories(&self) -> Result<Vec<Uuid>> {
        GlobalPromoter::new(self).promote()
    }

    pub fn gc(&self) -> Result<usize> {
        let now = Utc::now();
        let mut archived = 0;
        for path in self.markdown.memory_files()? {
            if path
                .components()
                .any(|component| component.as_os_str() == "archive")
            {
                continue;
            }
            let memory = self.markdown.parse_memory(&path)?;
            if memory.metadata.memory_type != MemoryType::Session {
                continue;
            }
            let ended = memory
                .metadata
                .ended_at
                .unwrap_or(memory.metadata.updated_at);
            let age_days = (now - ended).num_seconds().max(0) as f64 / 86_400.0;
            let (access_count, last_access) =
                self.sessions.meaningful_access(memory.metadata.id)?;
            let days_since_access = last_access
                .map(|access| (now - access).num_seconds().max(0) as f64 / 86_400.0)
                .unwrap_or(age_days);
            if age_days > 90.0
                && DecayEngine::session_retention(age_days, access_count, days_since_access) < 0.15
            {
                let destination = self.markdown.archive_session(&path)?;
                self.index.upsert_memory(&memory, &destination)?;
                archived += 1;
            }
        }
        if archived > 0 {
            self.markdown
                .commit(&format!("chore(sessions): archive {archived}"));
        }
        Ok(archived)
    }

    pub fn import_session(&self, mut session: NormalizedSession) -> Result<ImportOutcome> {
        if self
            .sessions
            .import_exists(&session.client, &session.external_session_id)?
        {
            self.sessions
                .requeue_import_compilation(&session.client, &session.external_session_id)?;
            return Ok(ImportOutcome::AlreadyImported);
        }
        let Some(cwd) = session.cwd.as_deref() else {
            self.sessions.record_import(
                &session.client,
                &session.external_session_id,
                "orphan",
                Some(&serde_json::to_string(&session)?),
            )?;
            return Ok(ImportOutcome::Orphan);
        };
        if !Path::new(cwd).exists() {
            session.cwd = None;
            self.sessions.record_import(
                &session.client,
                &session.external_session_id,
                "orphan",
                Some(&serde_json::to_string(&session)?),
            )?;
            return Ok(ImportOutcome::Orphan);
        }
        let mut ended = None;
        for event in session.events {
            if event.kind == NormalizedEventKind::SessionEnded {
                ended = Some(event);
            } else {
                self.ingest_event(event)?;
            }
        }
        self.sessions
            .mark_latest_session_imported(&session.client, &session.external_session_id)?;
        if let Some(event) = ended {
            self.ingest_event(event)?;
        }
        self.sessions.record_import(
            &session.client,
            &session.external_session_id,
            "imported",
            None,
        )?;
        Ok(ImportOutcome::Imported)
    }

    pub fn all_projects(&self) -> Result<Vec<Project>> {
        self.markdown
            .project_files()?
            .into_iter()
            .map(|path| self.markdown.parse_project(&path))
            .collect()
    }

    pub fn all_memories(&self) -> Result<Vec<Memory>> {
        self.markdown
            .memory_files()?
            .into_iter()
            .map(|path| self.markdown.parse_memory(&path))
            .collect()
    }

    pub fn edit_memory(&self, id: Uuid, title: &str, body: &str) -> Result<Memory> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, id)?;
        memory.title = title.trim().to_owned();
        memory.body = body.trim().to_owned();
        memory.metadata.updated_at = Utc::now();
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown.commit(&format!("docs(memory): edit {id}"));
        Ok(memory)
    }

    pub fn configuration_text(&self) -> Result<String> {
        Ok(fs::read_to_string(self.home.join("config.toml"))?)
    }

    pub fn update_configuration_text(&self, configuration: &str) -> Result<()> {
        let _: MenvaneConfig = toml::from_str(configuration)?;
        let lowercase = configuration.to_ascii_lowercase();
        for forbidden in ["api_key =", "token =", "password =", "secret ="] {
            if lowercase.contains(forbidden) {
                bail!("secrets must be supplied through environment variables");
            }
        }
        let path = self.home.join("config.toml");
        let temporary = self.home.join(format!(".config-{}.tmp", Uuid::now_v7()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        use std::io::Write;
        file.write_all(configuration.as_bytes())?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        std::fs::File::open(&self.home)?.sync_all()?;
        Ok(())
    }

    pub fn configure_openai(&self, model: &str, reasoning_effort: Option<&str>) -> Result<()> {
        if model.trim().is_empty() {
            bail!("OpenAI model cannot be empty");
        }
        if reasoning_effort.is_some_and(|effort| {
            !matches!(effort, "minimal" | "low" | "medium" | "high" | "xhigh")
        }) {
            bail!("reasoning effort must be minimal, low, medium, high, or xhigh");
        }
        let mut configuration: toml::Table =
            toml::from_str(&fs::read_to_string(self.home.join("config.toml"))?)?;
        let llm = configuration
            .entry("llm")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("llm configuration must be a table")?;
        llm.insert(
            "provider".to_owned(),
            toml::Value::String("openai".to_owned()),
        );
        llm.insert(
            "model".to_owned(),
            toml::Value::String(model.trim().to_owned()),
        );
        llm.remove("base_url");
        llm.remove("api_key_env");
        llm.insert(
            "oauth_issuer".to_owned(),
            toml::Value::String(default_oauth_issuer()),
        );
        llm.insert(
            "oauth_endpoint".to_owned(),
            toml::Value::String(default_oauth_endpoint()),
        );
        if let Some(reasoning_effort) = reasoning_effort {
            llm.insert(
                "reasoning_effort".to_owned(),
                toml::Value::String(reasoning_effort.to_owned()),
            );
        } else {
            llm.remove("reasoning_effort");
        }
        self.update_configuration_text(&toml::to_string_pretty(&configuration)?)
    }

    pub async fn login_openai(&self) -> Result<()> {
        OpenAiOAuthProvider::with_endpoints(
            &self.home,
            &self.config.llm.model,
            self.config.llm.reasoning_effort.clone(),
            &self.config.llm.oauth_issuer,
            &self.config.llm.oauth_endpoint,
        )
        .login()
        .await
        .map_err(anyhow::Error::new)
    }

    pub fn logout_openai(&self) -> Result<()> {
        OpenAiOAuthProvider::new(
            &self.home,
            &self.config.llm.model,
            self.config.llm.reasoning_effort.clone(),
        )
        .logout()
        .map_err(anyhow::Error::new)
    }

    pub fn integrations(&self) -> Result<Vec<IntegrationRecord>> {
        self.sessions.integrations()
    }

    pub fn orphans(&self) -> Result<Vec<OrphanRecord>> {
        self.sessions.orphans()
    }

    pub fn associate_orphan(
        &self,
        client: &str,
        external_session_id: &str,
        project_id: &str,
    ) -> Result<ImportOutcome> {
        let orphan = self
            .sessions
            .orphans()?
            .into_iter()
            .find(|orphan| {
                orphan.client == client && orphan.external_session_id == external_session_id
            })
            .context("orphan session not found")?;
        let project = self
            .all_projects()?
            .into_iter()
            .find(|project| project.id == project_id)
            .context("project not found")?;
        let cwd = project
            .known_paths
            .into_iter()
            .find(|path| Path::new(path).exists())
            .context("project has no available checkout")?;
        let mut session: NormalizedSession = serde_json::from_str(&orphan.payload_json)?;
        session.cwd = Some(cwd.clone());
        for event in &mut session.events {
            event.cwd.clone_from(&cwd);
        }
        self.sessions.clear_orphan(client, external_session_id)?;
        match self.import_session(session.clone()) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.sessions.record_import(
                    client,
                    external_session_id,
                    "orphan",
                    Some(&serde_json::to_string(&session)?),
                )?;
                Err(error)
            }
        }
    }

    pub fn backup(&self, destination: &Path) -> Result<()> {
        if destination.exists() {
            bail!(
                "backup destination already exists: {}",
                destination.display()
            );
        }
        fs::create_dir_all(destination)?;
        copy_tree(&self.home.join("memory"), &destination.join("memory"))?;
        fs::copy(
            self.home.join("config.toml"),
            destination.join("config.toml"),
        )?;
        self.index.backup(&destination.join("index.sqlite"))?;
        self.sessions.backup(&destination.join("state.sqlite"))?;
        let mut files = backup_files(destination)?;
        files.sort();
        let manifest = BackupManifest {
            version: 2,
            created_at: Utc::now(),
            files: files
                .into_iter()
                .map(|path| {
                    let relative = path
                        .strip_prefix(destination)?
                        .to_string_lossy()
                        .into_owned();
                    Ok((relative, sha256_file(&path)?))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>>>()?,
        };
        fs::write(
            destination.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(())
    }

    pub fn restore(&self, source: &Path) -> Result<()> {
        validate_backup(source)?;
        if self.home.join("daemon.pid").exists() {
            bail!("stop the Menvane daemon before restoring");
        }
        let staging = self.home.join(format!(".restore-stage-{}", Uuid::now_v7()));
        let previous = self
            .home
            .join(format!(".restore-previous-{}", Uuid::now_v7()));
        fs::create_dir_all(&staging)?;
        copy_tree(&source.join("memory"), &staging.join("memory"))?;
        fs::copy(source.join("config.toml"), staging.join("config.toml"))?;
        fs::copy(source.join("index.sqlite"), staging.join("index.sqlite"))?;
        fs::copy(source.join("state.sqlite"), staging.join("state.sqlite"))?;
        fs::create_dir_all(&previous)?;
        for name in ["memory", "config.toml", "index.sqlite", "state.sqlite"] {
            let current = self.home.join(name);
            if current.exists() {
                if name.ends_with(".sqlite") {
                    remove_sqlite_sidecars(&current)?;
                }
                fs::rename(&current, previous.join(name))?;
            }
            fs::rename(staging.join(name), current)?;
        }
        fs::remove_dir_all(staging)?;
        self.index.initialize()?;
        self.sessions.initialize()?;
        fs::remove_dir_all(previous)?;
        Ok(())
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub async fn provider_health(&self) -> Result<(String, String, ProviderHealth)> {
        let provider = self.configured_provider()?;
        Ok((
            provider.name().to_owned(),
            provider.model().to_owned(),
            provider.health().await,
        ))
    }

    pub async fn provider_test(&self) -> Result<serde_json::Value> {
        let provider = self.configured_provider()?;
        let response = provider
            .generate_structured(
                LlmRequest {
                    system: "Return the requested structured health response.".to_owned(),
                    prompt: "Return {\"ok\": true}.".to_owned(),
                    timeout: std::time::Duration::from_secs(30),
                },
                JsonSchema(serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["ok"],
                    "properties": { "ok": { "type": "boolean", "const": true } }
                })),
            )
            .await
            .map_err(anyhow::Error::new)?;
        if response
            .value
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            bail!("provider test returned an invalid structured response");
        }
        Ok(response.value)
    }

    pub async fn compile_and_store(
        &self,
        cwd: &Path,
        input: CompilationInput,
    ) -> Result<(Vec<Uuid>, String)> {
        let source_session = input.source_session;
        let result = MemoryCompiler::new(self.configured_provider()?)
            .compile(input)
            .await
            .map_err(anyhow::Error::new)?;
        let mut ids = Vec::new();
        for candidate in result.memories {
            ids.push(self.store_compiled_memory(cwd, candidate, source_session)?);
        }
        Ok((ids, result.provider))
    }

    pub async fn process_next_compilation(&self) -> Result<bool> {
        self.process_next_job().await
    }

    pub async fn process_next_job(&self) -> Result<bool> {
        let Some(job) = self
            .sessions
            .claim_job(&self.worker_owner, self.config.jobs.lease_timeout_seconds)?
        else {
            return Ok(false);
        };
        let result = async {
            if job.job_type == "finalize_session" {
                SessionEngine::new(self).finalize_job(&job)?;
                return Ok(None);
            }
            if job.job_type != "compile_session" {
                bail!("unsupported job type: {}", job.job_type);
            }
            let session_id = Uuid::parse_str(&job.dedupe_key)?;
            let session = self.index.read_memory(&self.markdown, session_id)?.0;
            if !is_session_worth_compiling(&session) {
                return Ok(Some("skipped".to_owned()));
            }
            let events = self.sessions.events(session_id)?;
            let (cwd, technology_profile) =
                if let Some(project_id) = session.metadata.project_id.as_deref() {
                    let project = self
                        .all_projects()?
                        .into_iter()
                        .find(|project| project.id == project_id)
                        .context("session project is missing")?;
                    let cwd = project
                        .known_paths
                        .iter()
                        .map(PathBuf::from)
                        .find(|path| path.exists())
                        .context("session project has no available checkout")?;
                    (cwd, serde_json::to_value(project.technologies)?)
                } else {
                    let cwd = events
                        .iter()
                        .map(|event| PathBuf::from(&event.cwd))
                        .find(|path| path.exists())
                        .context("global session has no available working directory")?;
                    (cwd, serde_json::json!({}))
                };
            let important_prompts = events
                .iter()
                .filter(|event| event.kind == NormalizedEventKind::UserPrompt)
                .filter_map(|event| event.bounded_input.clone())
                .collect();
            let important_tool_events = events
                .iter()
                .filter(|event| event.kind == NormalizedEventKind::ToolCompleted)
                .map(|event| {
                    format!(
                        "{}\ninput: {}\noutput: {}\nsuccess: {}",
                        event.tool_family.as_deref().unwrap_or("tool"),
                        event.bounded_input.as_deref().unwrap_or("none"),
                        event.bounded_output.as_deref().unwrap_or("none"),
                        event
                            .success
                            .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
                    )
                })
                .collect();
            let errors = events
                .iter()
                .filter(|event| event.success == Some(false))
                .filter_map(|event| event.bounded_output.clone())
                .collect();
            let validation_results = events
                .iter()
                .filter(|event| event.kind == NormalizedEventKind::ToolCompleted)
                .filter(|event| event.success == Some(true))
                .filter(|event| {
                    event
                        .tool_family
                        .as_deref()
                        .is_some_and(Self::is_validation_tool)
                })
                .filter_map(|event| event.tool_family.clone())
                .collect();
            let (_, provider) = self
                .compile_and_store(
                    &cwd,
                    CompilationInput {
                        session_summary: session.body,
                        important_prompts,
                        important_tool_events,
                        errors,
                        decisions: Vec::new(),
                        validation_results,
                        existing_related_memories: Vec::new(),
                        technology_profile,
                        source_session: Some(session_id),
                    },
                )
                .await?;
            Ok(Some(provider))
        }
        .await;
        match result {
            Ok(provider) => {
                self.sessions.finish_job(
                    job.id,
                    job.owner.as_deref().unwrap_or_default(),
                    provider.as_deref(),
                    None,
                )?;
            }
            Err(error) => {
                self.sessions.finish_job(
                    job.id,
                    job.owner.as_deref().unwrap_or_default(),
                    None,
                    Some(&error.to_string()),
                )?;
            }
        }
        Ok(true)
    }

    fn is_validation_tool(tool: &str) -> bool {
        let tool = tool.to_ascii_lowercase();
        tool.contains("test") || tool.contains("build") || tool.contains("check")
    }

    pub fn configured_provider(&self) -> Result<std::sync::Arc<dyn LlmProvider>> {
        let primary = provider_from_configuration(&self.config.llm, &self.home)?;
        let fallback = self
            .config
            .llm
            .fallback
            .as_deref()
            .map(|configuration| provider_from_configuration(configuration, &self.home))
            .transpose()?;
        Ok(std::sync::Arc::new(ProviderChain::new(primary, fallback)))
    }

    fn store_compiled_memory(
        &self,
        cwd: &Path,
        candidate: CompiledMemory,
        source_session: Option<Uuid>,
    ) -> Result<Uuid> {
        if let Some(memory) = self.all_memories()?.into_iter().find(|memory| {
            memory.metadata.memory_type == candidate.memory_type
                && memory.metadata.scope == candidate.scope
                && normalize_markdown(&memory.body) == normalize_markdown(&candidate.body)
                && (memory.title.eq_ignore_ascii_case(&candidate.title)
                    || source_session.is_some_and(|source_session| {
                        memory.metadata.source_sessions.contains(&source_session)
                    }))
        }) {
            let (mut memory, path) = self.index.read_memory(&self.markdown, memory.metadata.id)?;
            if let Some(source_session) = source_session
                && !memory.metadata.source_sessions.contains(&source_session)
            {
                memory.metadata.source_sessions.push(source_session);
                self.markdown.update_memory(&path, &memory)?;
                self.index.upsert_memory(&memory, &path)?;
            }
            return Ok(memory.metadata.id);
        }
        let scope = match candidate.scope {
            Scope::Global => ScopeSelection::Global,
            Scope::Project => ScopeSelection::Project,
        };
        let related = self.search(cwd, &candidate.title, scope, 20)?;
        let equivalent = related.into_iter().find(|memory| {
            memory.memory_type == candidate.memory_type.to_string()
                && memory.scope == candidate.scope.to_string()
                && memory.title.eq_ignore_ascii_case(&candidate.title)
        });
        let mut supersedes = Vec::new();
        if let Some(existing) = equivalent {
            let (mut memory, path) = self.index.read_memory(&self.markdown, existing.id)?;
            if normalize_markdown(&memory.body) == normalize_markdown(&candidate.body) {
                memory.metadata.confidence = memory.metadata.confidence.max(candidate.confidence);
                memory.metadata.updated_at = Utc::now();
                memory.metadata.last_verified_at = Some(Utc::now());
                if let Some(session) = source_session
                    && !memory.metadata.source_sessions.contains(&session)
                {
                    memory.metadata.source_sessions.push(session);
                }
                self.markdown.update_memory(&path, &memory)?;
                self.index.upsert_memory(&memory, &path)?;
                self.markdown
                    .commit(&format!("feat(memory): reinforce {}", memory.metadata.id));
                return Ok(memory.metadata.id);
            }
            memory.metadata.status = menvane_domain::MemoryStatus::Superseded;
            memory.metadata.updated_at = Utc::now();
            self.markdown.update_memory(&path, &memory)?;
            self.index.upsert_memory(&memory, &path)?;
            supersedes.push(memory.metadata.id);
        }
        let memory = self.write(
            cwd,
            WriteMemory {
                title: candidate.title,
                body: candidate.body,
                memory_type: candidate.memory_type,
                scope: candidate.scope,
                confidence: candidate.confidence,
                tags: Vec::new(),
                applies_to: candidate.applies_to,
            },
        )?;
        let (mut memory, path) = self.index.read_memory(&self.markdown, memory.metadata.id)?;
        if let Some(session) = source_session {
            memory.metadata.source_sessions.push(session);
        }
        memory.metadata.supersedes = supersedes;
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        Ok(memory.metadata.id)
    }

    pub fn doctor(&self) -> DoctorReport {
        let mut checks = Vec::new();
        let writable_probe = self.home.join(format!(".doctor-{}", Uuid::now_v7()));
        let writable = fs::write(&writable_probe, b"ok")
            .and_then(|_| fs::remove_file(&writable_probe))
            .map(|_| "writable".to_owned())
            .map_err(|error| error.to_string());
        checks.push(check("home writable", writable));
        checks.push(check(
            "index database",
            self.index
                .memory_count()
                .map(|count| format!("{count} memories")),
        ));
        checks.push(check(
            "FTS5",
            self.index.fts5_available().and_then(|available| {
                available
                    .then_some("available".to_owned())
                    .context("unavailable")
            }),
        ));
        checks.push(check("state database", self.sessions.health()));
        let markdown_counts = self.markdown.project_files().and_then(|projects| {
            self.markdown
                .memory_files()
                .map(|memories| (projects.len() as u64, memories.len() as u64))
        });
        let index_counts = self.index.project_count().and_then(|projects| {
            self.index
                .memory_count()
                .map(|memories| (projects, memories))
        });
        let consistency = markdown_counts.and_then(|markdown| {
            index_counts.and_then(|index| {
                if markdown == index {
                    Ok(format!("{} projects, {} memories", markdown.0, markdown.1))
                } else {
                    bail!(
                        "Markdown has {} projects/{} memories; index has {} projects/{} memories",
                        markdown.0,
                        markdown.1,
                        index.0,
                        index.1
                    )
                }
            })
        });
        checks.push(check("Markdown/index consistency", consistency));
        let git = Command::new("git")
            .arg("--version")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .map_err(|error| error.to_string());
        checks.push(check("Git", git));
        let daemon = fs::read_to_string(self.home.join("daemon.pid"))
            .map_err(|error| error.to_string())
            .and_then(|pid| {
                Command::new("kill")
                    .args(["-0", pid.trim()])
                    .status()
                    .map_err(|error| error.to_string())
                    .and_then(|status| {
                        status
                            .success()
                            .then(|| format!("process {}", pid.trim()))
                            .ok_or_else(|| "not running".to_owned())
                    })
            });
        checks.push(check("daemon", daemon));
        match self.sessions.integrations() {
            Ok(integrations) => {
                for (client, label) in [
                    ("claude-code", "Claude integration"),
                    ("codex", "Codex integration"),
                    ("opencode", "OpenCode integration"),
                ] {
                    let result = integrations
                        .iter()
                        .find(|state| state.client == client && state.connected)
                        .map(|state| format!("{}; MCP={}", state.hook_status, state.mcp_registered))
                        .ok_or_else(|| "not connected".to_owned());
                    checks.push(check(label, result));
                }
            }
            Err(error) => checks.push(DoctorCheck {
                name: "integrations",
                healthy: false,
                detail: error.to_string(),
            }),
        }
        checks.push(check(
            "jobs",
            self.sessions.jobs().and_then(|jobs| {
                let failed = jobs.iter().filter(|job| job.status == "failed").count();
                if failed == 0 {
                    Ok(format!(
                        "{} pending",
                        jobs.iter().filter(|job| job.status == "pending").count()
                    ))
                } else {
                    bail!("{failed} failed jobs")
                }
            }),
        ));
        checks.push(DoctorCheck {
            name: "embedding provider",
            healthy: true,
            detail: "disabled; FTS5 active".to_owned(),
        });
        DoctorReport { checks }
    }

    pub fn ensure_project(&self, cwd: &Path) -> Result<Option<Project>> {
        let Some(resolution) = ProjectResolver::resolve(cwd)? else {
            return Ok(None);
        };
        let technologies = TechnologyDetector::detect(&resolution.root)?;
        let probe = Project {
            id: resolution.id.clone(),
            identity: resolution.identity.clone(),
            name: resolution.name.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            known_paths: vec![resolution.root.to_string_lossy().into_owned()],
            technologies: technologies.clone(),
        };
        let path = self.markdown.project_directory(&probe).join("project.md");
        let mut project = if path.exists() {
            self.markdown.parse_project(&path)?
        } else {
            probe
        };
        let known_path = resolution.root.to_string_lossy().into_owned();
        let mut changed = false;
        if !project.known_paths.contains(&known_path) {
            project.known_paths.push(known_path);
            project.known_paths.sort();
            changed = true;
        }
        if project.technologies != technologies {
            project.technologies = technologies;
            changed = true;
        }
        if !path.exists() || changed {
            project.updated_at = Utc::now();
            let path = self.markdown.write_project(&project)?;
            self.index.upsert_project(&project, &path)?;
            self.markdown
                .commit(&format!("chore(project): update {}", project.id));
        } else {
            self.index.upsert_project(&project, &path)?;
        }
        Ok(Some(project))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ScopeSelection {
    Auto,
    Project,
    Global,
}

fn check(name: &'static str, result: Result<String, impl ToString>) -> DoctorCheck {
    match result {
        Ok(detail) => DoctorCheck {
            name,
            healthy: true,
            detail,
        },
        Err(error) => DoctorCheck {
            name,
            healthy: false,
            detail: error.to_string(),
        },
    }
}

fn format_memory_body(memory_type: MemoryType, body: &str) -> String {
    let body = body.trim();
    match memory_type {
        MemoryType::Fact => body.to_owned(),
        MemoryType::Decision if body.contains("## Decision") => body.to_owned(),
        MemoryType::Decision => {
            format!("## Decision\n\n{body}\n\n## Reason\n\n## Alternatives\n\n## Consequences")
        }
        MemoryType::Gotcha if body.contains("## Problem") => body.to_owned(),
        MemoryType::Gotcha => {
            format!("## Problem\n\n{body}\n\n## Cause\n\n## Resolution\n\n## Avoidance")
        }
        MemoryType::Procedure if body.contains("## Procedure") => body.to_owned(),
        MemoryType::Procedure => format!(
            "## Trigger\n\n## Preconditions\n\n## Procedure\n\n{body}\n\n## Decision points\n\n## Validation\n\n## Failure handling\n\n## Expected outcome"
        ),
        MemoryType::Session => body.to_owned(),
    }
}

const REQUIRED_CONTEXT_BUDGET: usize = 2_000;
const RELEVANT_CONTEXT_BUDGET: usize = 3_000;
const RETRIEVAL_CARD_BUDGET: usize = 1_000;

fn format_memory_entry(
    memory: &SearchResult,
    tier: &str,
    reason: &str,
    include_excerpt: bool,
) -> String {
    let excerpt = if include_excerpt {
        format!("\nExcerpt: {}", memory.excerpt.trim())
    } else {
        String::new()
    };
    format!(
        "\n[{}]\nID: {}\nType: {}\nScope: {}\nStatus: {}\nConfidence: {:.2}\nAge: {:.0} days\nProvenance: source sessions {}; supersession count {}\nRelevance: {}\nTitle: {}{}\n",
        tier,
        memory.id,
        memory.memory_type,
        memory.scope,
        memory.status,
        memory.confidence,
        memory.age_days,
        memory.source_session_count,
        memory.supersession_count,
        reason,
        memory.title,
        excerpt
    )
}

fn fits_context(context: &str, entry: &str, max_chars: usize) -> bool {
    context.chars().count() + entry.chars().count() + "\nEND MENVANE MEMORY CONTEXT".chars().count()
        <= max_chars
}

fn relevance_reason(memory: &SearchResult, diagnostics: &RecallDiagnostics) -> &'static str {
    diagnostics
        .results
        .iter()
        .find(|diagnostic| diagnostic.memory_id == memory.id.to_string())
        .and_then(|diagnostic| {
            diagnostic
                .sources
                .iter()
                .filter(|source| source.contribution > 0.0)
                .max_by(|left, right| left.contribution.total_cmp(&right.contribution))
        })
        .map_or("selected for current intent", |source| {
            match source.source.as_str() {
                "current-prompt" => "matches current prompt",
                "active-episode-goal" => "matches active episode goal",
                source if source.starts_with("active-correction") => "matches active correction",
                source if source.starts_with("active-constraint") => "matches active constraint",
                "conversation-root-goal" => "matches conversation root goal",
                _ => "selected for current intent",
            }
        })
}

fn provider_from_configuration(
    configuration: &LlmConfiguration,
    home: &Path,
) -> Result<std::sync::Arc<dyn LlmProvider>> {
    match configuration.provider.as_str() {
        "codex" => Ok(std::sync::Arc::new(CodexProvider::new(
            "codex",
            &configuration.model,
        ))),
        "openai" => Ok(std::sync::Arc::new(OpenAiOAuthProvider::with_endpoints(
            home,
            &configuration.model,
            configuration.reasoning_effort.clone(),
            &configuration.oauth_issuer,
            &configuration.oauth_endpoint,
        ))),
        "openai-api" => Ok(std::sync::Arc::new(
            OpenAIApiProvider::new(
                &configuration.model,
                &configuration.base_url,
                &configuration.api_key_env,
            )
            .with_reasoning_effort(configuration.reasoning_effort.clone()),
        )),
        "openrouter" => {
            if configuration.model == "default" || configuration.model.trim().is_empty() {
                bail!("OpenRouter requires an explicit model");
            }
            let base_url = if configuration.base_url == "https://api.openai.com/v1" {
                "https://openrouter.ai/api/v1"
            } else {
                &configuration.base_url
            };
            let api_key_env = if configuration.api_key_env == "OPENAI_API_KEY" {
                "OPENROUTER_API_KEY"
            } else {
                &configuration.api_key_env
            };
            Ok(std::sync::Arc::new(OpenRouterProvider::new(
                &configuration.model,
                base_url,
                api_key_env,
            )))
        }
        provider => bail!("unsupported LLM provider: {provider}"),
    }
}

fn normalize_markdown(markdown: &str) -> String {
    markdown
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[derive(Serialize, Deserialize)]
struct BackupManifest {
    version: u32,
    created_at: chrono::DateTime<Utc>,
    files: std::collections::BTreeMap<String, String>,
}

fn validate_backup(source: &Path) -> Result<()> {
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(source.join("manifest.json"))?)?;
    if manifest.version != 2 {
        bail!("unsupported backup version: {}", manifest.version);
    }
    for required in ["config.toml", "index.sqlite", "state.sqlite"] {
        if !manifest.files.contains_key(required) {
            bail!("backup manifest is missing {required}");
        }
    }
    for (relative, expected) in manifest.files {
        let path = source.join(&relative);
        if !path.starts_with(source) || !path.is_file() {
            bail!("backup file is missing: {relative}");
        }
        if sha256_file(&path)? != expected {
            bail!("backup checksum mismatch: {relative}");
        }
    }
    let _: MenvaneConfig = toml::from_str(&fs::read_to_string(source.join("config.toml"))?)?;
    let markdown = MarkdownStore::new(source);
    for path in markdown.project_files()? {
        markdown.parse_project(&path)?;
    }
    for path in markdown.memory_files()? {
        markdown.parse_memory(&path)?;
    }
    let index = IndexStore::new(source.join("index.sqlite"));
    index.initialize()?;
    index.memory_count()?;
    let state = SessionRepository::new(source.join("state.sqlite"));
    state.initialize()?;
    state.health()?;
    Ok(())
}

fn backup_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_none_or(|name| name != "manifest.json") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        let target = destination.join(path.file_name().context("path has no filename")?);
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            fs::copy(path, target)?;
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(fs::read(path)?)))
}

fn remove_sqlite_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
    }
    Ok(())
}

fn acquire_daemon_lock(home: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("daemon.lock"))?;
    file.try_lock_exclusive()
        .with_context(|| "cannot reindex while the Menvane daemon is running")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use menvane_domain::{Applicability, MemoryType, Scope};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn repeated_compilation_storage_reuses_source_session_memory() {
        let temporary = TempDir::new().unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let source_session = Uuid::from_u128(41);
        let candidate = CompiledMemory {
            memory_type: MemoryType::Fact,
            title: "Retry-safe compilation".to_owned(),
            scope: Scope::Global,
            scope_confidence: 1.0,
            confidence: 0.9,
            applies_to: Applicability::default(),
            body: "The same compilation result is applied once.".to_owned(),
            evidence: vec!["event-1".to_owned()],
        };
        let first = menvane
            .store_compiled_memory(temporary.path(), candidate.clone(), Some(source_session))
            .unwrap();
        let second = menvane
            .store_compiled_memory(temporary.path(), candidate, Some(source_session))
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            menvane
                .all_memories()
                .unwrap()
                .into_iter()
                .filter(|memory| memory.metadata.id == first)
                .count(),
            1
        );
    }
}
