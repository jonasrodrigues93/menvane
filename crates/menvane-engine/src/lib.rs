mod decay;
mod embeddings;
mod github_copilot_provider;
mod oauth_provider;
mod project_resolver;
mod providers;
mod sanitizer;
mod session_consolidator;
mod session_engine;
mod session_rendering;
mod stopwords;
mod technology_detector;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;
use menvane_domain::{
    Applicability, ConsolidationPacket, ConsolidationResult, EmbeddingProvider, EpisodicSummary,
    HandoffItem, HandoffItemOperation, HandoffItemSource, JsonSchema, KnowledgeContent,
    KnowledgeMetadata, KnowledgeOperation, KnowledgeOperationKind, KnowledgeRecord, KnowledgeType,
    LlmError, LlmProvider, LlmRequest, MemoryStatus, NormalizedEvent, NormalizedEventKind,
    NormalizedSession, Project, ProviderHealth, ReinforcementSignal, Scope, SessionMetadata,
    SessionState, SummaryStatus,
};
use menvane_store::{
    IndexStore, InjectionIdentity, IntegrationRecord, JobRecord, MAX_SUMMARY_SELECTION_BYTES,
    MAX_SUMMARY_SELECTION_SESSIONS, MarkdownStore, OrphanRecord, SearchResult, SearchScope,
    SessionRepository, mark_forgotten,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use decay::{MemoryDecay, memory_decay};
pub use embeddings::OpenAICompatibleEmbeddingProvider;
pub use github_copilot_provider::GithubCopilotProvider;
pub use menvane_store::{
    ConsolidationMarker, IngestResult, JobRecord as StoreJobRecord, MAX_HANDOFF_ITEM_BYTES,
    MAX_HANDOFF_LIST_LIMIT, RecallContext, SessionEvent, SessionRecord, conversation_key,
};
pub use oauth_provider::OpenAiOAuthProvider;
pub use project_resolver::{ProjectResolution, ProjectResolver, normalize_git_remote};
pub use providers::{CodexProvider, OpenAIApiProvider, OpenRouterProvider, ProviderChain};
pub use sanitizer::{
    CaptureSanitizer, CaptureSanitizerConfig, MAX_RECALL_CWD_BYTES, MAX_RECALL_IDENTIFIER_BYTES,
    MAX_RECALL_PROMPT_BYTES,
};
pub use session_consolidator::{
    ConsolidationOutcome, ConsolidationPacket as EngineConsolidationPacket,
    MAX_HANDOFF_SUMMARY_BYTES, SessionConsolidator,
};
pub use session_engine::{CaptureOutcome, SessionEngine};
pub use technology_detector::TechnologyDetector;

pub const GLOBAL_SCOPE_CONFIDENCE_THRESHOLD: f64 = 0.9;
pub const MAX_RELATED_MEMORIES: usize = 5;

const HANDOFF_DELIVERY_KIND: &str = "handoff";
const GENERIC_HANDOFF_TERMS: [&str; 10] = [
    "arquivo",
    "config",
    "configuracao",
    "configuration",
    "error",
    "erro",
    "file",
    "path",
    "sistema",
    "system",
];

#[derive(Debug, Clone)]
pub struct WriteMemory {
    pub title: String,
    pub body: String,
    pub knowledge_type: KnowledgeType,
    pub scope: Scope,
    pub tags: Vec<String>,
    pub applies_to: Applicability,
}

#[derive(Debug, Clone)]
pub struct PromptRecall {
    pub results: Vec<SearchResult>,
    pub diagnostics: RecallDiagnostics,
    pub identity: InjectionIdentity,
}

struct HybridRecallCandidate {
    result: SearchResult,
    lexical_rank: Option<usize>,
    embedding_rank: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecallDiagnostics {
    pub query: String,
    pub result_count: usize,
    pub handoff_scope: String,
    pub handoff_match_terms: Vec<String>,
    pub handoff_required_match_count: usize,
    pub handoff_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentHandoff {
    pub project_id: Option<String>,
    pub items: Vec<HandoffItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, Serialize)]
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
    #[serde(default)]
    embeddings: EmbeddingConfiguration,
    #[serde(default)]
    decay: DecayConfiguration,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct DecayConfiguration {
    #[serde(default = "default_memory_lifetime_days")]
    memory_lifetime_days: u64,
}

impl Default for DecayConfiguration {
    fn default() -> Self {
        Self {
            memory_lifetime_days: default_memory_lifetime_days(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct EmbeddingConfiguration {
    provider: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default = "default_api_url")]
    base_url: String,
    #[serde(default = "default_api_key_env")]
    api_key_env: String,
    #[serde(default = "default_embedding_min_similarity")]
    min_similarity: f64,
}

impl Default for EmbeddingConfiguration {
    fn default() -> Self {
        Self {
            provider: None,
            model: String::new(),
            base_url: default_api_url(),
            api_key_env: default_api_key_env(),
            min_similarity: default_embedding_min_similarity(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SessionConfiguration {
    #[serde(default = "default_idle_finalize_seconds")]
    idle_finalize_seconds: u64,
    #[serde(default = "default_open_finalize_seconds")]
    open_finalize_seconds: u64,
}

impl Default for SessionConfiguration {
    fn default() -> Self {
        Self {
            idle_finalize_seconds: default_idle_finalize_seconds(),
            open_finalize_seconds: default_open_finalize_seconds(),
        }
    }
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

#[derive(Debug, Clone, Deserialize)]
struct LlmConfiguration {
    #[serde(default = "default_llm_provider")]
    provider: String,
    #[serde(default = "default_llm_model")]
    model: String,
    #[serde(default = "default_api_url")]
    base_url: String,
    #[serde(default = "default_api_key_env")]
    api_key_env: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default = "default_oauth_issuer")]
    oauth_issuer: String,
    #[serde(default = "default_oauth_endpoint")]
    oauth_endpoint: String,
    #[serde(default = "default_github_oauth_issuer")]
    github_oauth_issuer: String,
    #[serde(default)]
    github_client_id: String,
    #[serde(default)]
    consolidation_prompt: Option<String>,
    #[serde(default)]
    fallback: Option<Box<LlmConfiguration>>,
}

impl Default for LlmConfiguration {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            model: default_llm_model(),
            base_url: default_api_url(),
            api_key_env: default_api_key_env(),
            reasoning_effort: Some("medium".to_owned()),
            oauth_issuer: default_oauth_issuer(),
            oauth_endpoint: default_oauth_endpoint(),
            github_oauth_issuer: default_github_oauth_issuer(),
            github_client_id: String::new(),
            consolidation_prompt: None,
            fallback: None,
        }
    }
}

fn default_idle_finalize_seconds() -> u64 {
    120
}
fn default_open_finalize_seconds() -> u64 {
    1_800
}
fn default_job_lease_timeout_seconds() -> u64 {
    300
}
fn default_memory_lifetime_days() -> u64 {
    decay::DEFAULT_MEMORY_LIFETIME_DAYS as u64
}
fn default_llm_provider() -> String {
    "openai".to_owned()
}
fn default_llm_model() -> String {
    "gpt-5.6-luna".to_owned()
}
fn default_api_url() -> String {
    "https://api.openai.com/v1".to_owned()
}
fn default_api_key_env() -> String {
    "OPENAI_API_KEY".to_owned()
}
fn default_oauth_issuer() -> String {
    "https://auth.openai.com".to_owned()
}
fn default_oauth_endpoint() -> String {
    "https://chatgpt.com/backend-api/codex/responses".to_owned()
}
fn default_github_oauth_issuer() -> String {
    "https://github.com".to_owned()
}
fn default_github_api_endpoint() -> String {
    "https://api.githubcopilot.com".to_owned()
}

fn default_embedding_min_similarity() -> f64 {
    0.78
}

pub struct Menvane {
    home: PathBuf,
    pub(crate) markdown: MarkdownStore,
    pub(crate) index: IndexStore,
    pub(crate) sessions: SessionRepository,
    pub(crate) config: MenvaneConfig,
    worker_owner: String,
    provider_override: Option<Arc<dyn LlmProvider>>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    decay_last_sweep: Mutex<chrono::DateTime<Utc>>,
}

impl DoctorReport {
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|check| check.healthy)
    }
}

impl Menvane {
    pub fn from_environment() -> Result<Self> {
        let home = env::var_os("MENVANE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".menvane")))
            .context("HOME is not set; set MENVANE_HOME explicitly")?;
        Self::new(home)
    }

    pub fn new(home: impl Into<PathBuf>) -> Result<Self> {
        let home = home.into();
        let markdown = MarkdownStore::new(&home);
        markdown.initialize()?;
        let config: MenvaneConfig = toml::from_str(&fs::read_to_string(home.join("config.toml"))?)?;
        let embedding_provider = configured_embedding_provider(&config.embeddings)?;
        let index = IndexStore::new(home.join("index.sqlite"));
        index.initialize()?;
        let sessions = SessionRepository::new(home.join("state.sqlite"));
        sessions.initialize()?;
        if config.decay.memory_lifetime_days < 1 {
            bail!("decay.memory_lifetime_days must be at least 1");
        }
        let menvane = Self {
            home,
            markdown,
            index,
            sessions,
            config,
            worker_owner: Uuid::now_v7().to_string(),
            provider_override: None,
            embedding_provider,
            decay_last_sweep: Mutex::new(chrono::DateTime::<Utc>::UNIX_EPOCH),
        };
        menvane.expire_memories()?;
        *menvane
            .decay_last_sweep
            .lock()
            .map_err(|_| anyhow::anyhow!("decay sweep lock is poisoned"))? = Utc::now();
        Ok(menvane)
    }

    pub fn new_with_provider(
        home: impl Into<PathBuf>,
        provider: Arc<dyn LlmProvider>,
    ) -> Result<Self> {
        let mut menvane = Self::new(home)?;
        menvane.provider_override = Some(provider);
        Ok(menvane)
    }

    pub fn new_with_embedding_provider(
        home: impl Into<PathBuf>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self> {
        let mut menvane = Self::new(home)?;
        menvane.embedding_provider = Some(provider);
        Ok(menvane)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn write(&self, cwd: &Path, request: WriteMemory) -> Result<KnowledgeRecord> {
        if request.title.trim().is_empty() {
            bail!("memory title cannot be empty");
        }
        if request.body.trim().is_empty() {
            bail!("memory body cannot be empty");
        }
        let project = match request.scope {
            Scope::Project => self.ensure_project(cwd)?,
            Scope::Global => None,
        };
        let scope = project.as_ref().map_or(Scope::Global, |_| request.scope);
        let metadata = KnowledgeMetadata::new(
            request.knowledge_type,
            scope,
            project.as_ref().map(|value| value.id.clone()),
            request.tags,
            request.applies_to,
            MemoryStatus::Active,
        );
        let memory = KnowledgeRecord {
            metadata,
            title: request.title.trim().to_owned(),
            body: request.body.trim().to_owned(),
        };
        if self.duplicate_memory(&memory.metadata, &memory.title, &memory.body)? {
            bail!("equivalent memory already exists");
        }
        let path = self.markdown.write_memory(&memory, project.as_ref())?;
        self.index.upsert_memory(&memory, &path)?;
        self.refresh_memory_embedding(&memory)?;
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

    pub fn search_without_recording(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_inner(cwd, query, scope, limit)
    }

    pub fn search_including_forgotten(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.expire_memories_if_due()?;
        let project = match scope {
            ScopeSelection::Global => None,
            ScopeSelection::Auto | ScopeSelection::Project => self.ensure_project(cwd)?,
        };
        let search_scope = match scope {
            ScopeSelection::Auto => project.as_ref().map_or(SearchScope::Global, |value| {
                SearchScope::Auto(value.id.as_str())
            }),
            ScopeSelection::Project => project.as_ref().map_or(SearchScope::Global, |value| {
                SearchScope::Project(value.id.as_str())
            }),
            ScopeSelection::Global => SearchScope::Global,
        };
        self.index
            .search_including_forgotten(query, search_scope, limit)
    }

    pub fn search_with_sessions(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
        include_sessions: bool,
    ) -> Result<Vec<SearchResult>> {
        let results =
            self.search_inner_with_sessions(cwd, query, scope, limit, include_sessions)?;
        for result in &results {
            self.sessions
                .record_access(result.id, ReinforcementSignal::Retrieved)?;
        }
        Ok(results)
    }

    pub fn recall(&self, cwd: &Path, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_with_sessions(cwd, query, ScopeSelection::Auto, limit, false)
    }

    pub fn prompt_recall(
        &self,
        cwd: &Path,
        client: &str,
        external_session_id: &str,
        prompt: &str,
        limit: usize,
    ) -> Result<PromptRecall> {
        let sanitizer = CaptureSanitizer::new(self.config.capture.clone())?;
        let prompt = sanitizer.sanitize_prompt(prompt);
        let project = self.ensure_project(cwd)?;
        let identity = self.sessions.injection_identity(
            client,
            external_session_id,
            project.as_ref().map(|value| value.id.as_str()),
        )?;
        if prompt.trim().is_empty() {
            return Ok(PromptRecall {
                results: Vec::new(),
                diagnostics: recall_diagnostics(String::new(), 0),
                identity,
            });
        }
        let mut terms = automatic_recall_tokens(&prompt)
            .into_iter()
            .collect::<Vec<_>>();
        terms.sort();
        let query = terms.join(" ");
        if query.is_empty() {
            return Ok(PromptRecall {
                results: Vec::new(),
                diagnostics: recall_diagnostics(String::new(), 0),
                identity,
            });
        }
        let lexical_candidates = self.search_inner(
            cwd,
            &query,
            ScopeSelection::Auto,
            limit.saturating_mul(16).max(64),
        )?;
        let prompt_tokens = terms.into_iter().collect::<HashSet<_>>();
        let mut candidates = HashMap::<Uuid, HybridRecallCandidate>::new();
        for (index, result) in lexical_candidates.into_iter().enumerate() {
            if !automatically_eligible(&result, project.as_ref()) {
                continue;
            }
            let memory = self.read_without_recording(result.id)?;
            let memory_tokens = lexical_tokens(&format!("{} {}", memory.title, memory.body));
            if meaningful_lexical_overlap(&prompt_tokens, &memory_tokens) {
                candidates.insert(
                    result.id,
                    HybridRecallCandidate {
                        result,
                        lexical_rank: Some(index + 1),
                        embedding_rank: None,
                    },
                );
            }
        }
        if let Some(provider) = &self.embedding_provider
            && let Ok(embedding) = provider.embed(&prompt)
            && embeddings::validate_embedding(&embedding).is_ok()
        {
            let project_id = project
                .as_ref()
                .map(|project| project.id.as_str())
                .unwrap_or_default();
            if let Ok(embedding_candidates) = self.index.search_embeddings(
                &embedding,
                provider.name(),
                provider.model(),
                SearchScope::Auto(project_id),
                limit.saturating_mul(16).max(64),
            ) {
                for (index, result) in embedding_candidates.into_iter().enumerate() {
                    if result.score < self.config.embeddings.min_similarity
                        || !automatically_eligible(&result, project.as_ref())
                    {
                        continue;
                    }
                    candidates
                        .entry(result.id)
                        .and_modify(|candidate| candidate.embedding_rank = Some(index + 1))
                        .or_insert(HybridRecallCandidate {
                            result,
                            lexical_rank: None,
                            embedding_rank: Some(index + 1),
                        });
                }
            }
        }
        let mut results = candidates
            .into_values()
            .map(|mut candidate| {
                candidate.result.score =
                    reciprocal_rank_fusion(candidate.lexical_rank, candidate.embedding_rank);
                candidate.result.fts_rank = candidate.lexical_rank.unwrap_or_default();
                candidate.result
            })
            .collect::<Vec<_>>();
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });
        results.truncate(limit);
        for result in &results {
            self.sessions
                .record_access(result.id, ReinforcementSignal::Retrieved)?;
        }
        Ok(PromptRecall {
            diagnostics: recall_diagnostics(query, results.len()),
            results,
            identity,
        })
    }

    pub fn read(&self, id: Uuid) -> Result<KnowledgeRecord> {
        self.read_without_recording(id)
    }

    pub fn read_from_mcp(&self, id: Uuid) -> Result<KnowledgeRecord> {
        let memory = self.read_without_recording(id)?;
        if memory.metadata.knowledge_type == KnowledgeType::Memory {
            self.record_memory_reinforcement(id, ReinforcementSignal::McpRead)?;
            return self.read_without_recording(id);
        }
        Ok(memory)
    }

    pub fn read_without_recording(&self, id: Uuid) -> Result<KnowledgeRecord> {
        Ok(self.index.read_memory(&self.markdown, id)?.0)
    }

    pub fn forget(&self, id: Uuid) -> Result<KnowledgeRecord> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, id)?;
        mark_forgotten(&mut memory);
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        self.markdown.commit(&format!("chore(memory): forget {id}"));
        Ok(memory)
    }

    pub fn reindex(&self) -> Result<(usize, usize)> {
        let _lock = acquire_daemon_lock(&self.home)?;
        let counts = self.index.reindex(&self.markdown)?;
        if self.embedding_provider.is_some() {
            for path in self.markdown.memory_files()? {
                let memory = self.markdown.parse_memory(&path)?;
                self.refresh_memory_embedding(&memory)?;
            }
        }
        Ok(counts)
    }

    fn refresh_memory_embedding(&self, memory: &KnowledgeRecord) -> Result<()> {
        let Some(provider) = &self.embedding_provider else {
            return Ok(());
        };
        let text = format!("{}\n\n{}", memory.title, memory.body);
        let Ok(embedding) = provider.embed(&text) else {
            return Ok(());
        };
        if embeddings::validate_embedding(&embedding).is_err() {
            return Ok(());
        }
        let _ = self.index.upsert_memory_embedding(
            memory.metadata.id,
            provider.name(),
            provider.model(),
            &embedding,
        );
        Ok(())
    }

    pub fn ingest_event(&self, event: NormalizedEvent) -> Result<CaptureOutcome> {
        let Some(event) = self.sanitize_event(event)? else {
            return Ok(CaptureOutcome::Dropped);
        };
        SessionEngine::new(self).ingest(event)
    }

    pub fn sanitize_event(&self, event: NormalizedEvent) -> Result<Option<NormalizedEvent>> {
        Ok(CaptureSanitizer::new(self.config.capture.clone())?.sanitize(event))
    }

    pub fn finalize_idle_sessions(&self) -> Result<usize> {
        SessionEngine::new(self).finalize_inactive(
            self.config.sessions.idle_finalize_seconds,
            self.config.sessions.open_finalize_seconds,
        )
    }

    pub fn jobs(&self) -> Result<Vec<JobRecord>> {
        self.sessions.jobs()
    }

    pub fn retry_failed_consolidations(&self) -> Result<usize> {
        self.sessions.retry_failed_consolidations()
    }

    pub async fn retry_failed_provider_consolidations(&self) -> Result<usize> {
        let provider = self.configured_provider()?;
        if provider.health().await == ProviderHealth::Ready {
            self.sessions.retry_failed_provider_consolidations()
        } else {
            Ok(0)
        }
    }

    pub fn current_handoff_items(&self, project_id: Option<&str>) -> Result<Vec<HandoffItem>> {
        self.sessions.current_handoff(project_id)
    }

    pub fn current_project_handoff(
        &self,
        project_id: Option<&str>,
    ) -> Result<Option<CurrentHandoff>> {
        let items = self.current_handoff_items(project_id)?;
        Ok((!items.is_empty()).then(|| CurrentHandoff {
            project_id: project_id.map(str::to_owned),
            items,
        }))
    }

    pub fn render_current_handoff(&self, project_id: Option<&str>) -> Result<String> {
        Ok(session_rendering::render_handoff_items(
            &self.current_handoff_items(project_id)?,
            32_768,
        ))
    }

    pub fn session_events(&self, id: Uuid) -> Result<Vec<NormalizedEvent>> {
        self.sessions.events(id)
    }

    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        self.sessions.sessions(limit)
    }

    pub fn session(&self, id: Uuid) -> Result<Option<SessionRecord>> {
        self.sessions.find_session(id)
    }

    pub fn session_summary(&self, id: Uuid) -> Result<Option<EpisodicSummary>> {
        self.sessions.session_summary(id)
    }

    pub fn session_consolidation(&self, id: Uuid) -> Result<Option<ConsolidationMarker>> {
        self.sessions.consolidation_result(id)
    }

    pub fn handoff_is_stale(&self, _project: &Project) -> Result<Option<bool>> {
        Ok(None)
    }

    pub fn memory_access_counts(&self, memory_id: Uuid) -> Result<Vec<(String, u64)>> {
        self.sessions.access_counts(memory_id)
    }

    pub fn memory_reinforcement(
        &self,
        memory_id: Uuid,
    ) -> Result<(u64, Option<chrono::DateTime<Utc>>)> {
        self.sessions.memory_reinforcement(memory_id)
    }

    pub fn decay_state(&self, memory: &KnowledgeRecord) -> Result<Option<MemoryDecay>> {
        if memory.metadata.knowledge_type != KnowledgeType::Memory {
            return Ok(None);
        }
        let (count, latest) = self.sessions.memory_reinforcement(memory.metadata.id)?;
        Ok(Some(memory_decay(
            memory.metadata.created_at,
            count,
            latest,
            Utc::now(),
            self.config.decay.memory_lifetime_days as f64,
        )))
    }

    fn record_memory_reinforcement(
        &self,
        memory_id: Uuid,
        signal: ReinforcementSignal,
    ) -> Result<()> {
        self.sessions.record_access(memory_id, signal)?;
        let (mut memory, path) = self.index.read_memory(&self.markdown, memory_id)?;
        if memory.metadata.knowledge_type != KnowledgeType::Memory {
            return Ok(());
        }
        let decay = self
            .decay_state(&memory)?
            .context("memory has no decay state")?;
        if memory.metadata.status == MemoryStatus::Forgotten
            && memory.metadata.decayed_at.is_some()
            && decay.score > 0.0
        {
            memory.metadata.status = MemoryStatus::Active;
            memory.metadata.decayed_at = None;
            memory.metadata.updated_at = Utc::now();
            self.markdown.update_memory(&path, &memory)?;
            self.index.upsert_memory(&memory, &path)?;
            self.markdown
                .commit(&format!("feat(memory): revive {memory_id}"));
        }
        Ok(())
    }

    fn expire_memories(&self) -> Result<usize> {
        let mut expired = 0;
        for path in self.markdown.memory_files()? {
            let mut memory = self.markdown.parse_memory(&path)?;
            if memory.metadata.knowledge_type != KnowledgeType::Memory
                || memory.metadata.status != MemoryStatus::Active
            {
                continue;
            }
            let decay = self
                .decay_state(&memory)?
                .context("memory has no decay state")?;
            if decay.score > 0.0 {
                continue;
            }
            memory.metadata.status = MemoryStatus::Forgotten;
            memory.metadata.decayed_at = Some(Utc::now());
            memory.metadata.updated_at = Utc::now();
            self.markdown.update_memory(&path, &memory)?;
            self.index.upsert_memory(&memory, &path)?;
            expired += 1;
        }
        if expired > 0 {
            self.markdown
                .commit(&format!("chore(memory): expire {expired} records"));
        }
        Ok(expired)
    }

    fn expire_memories_if_due(&self) -> Result<usize> {
        let mut last_sweep = self
            .decay_last_sweep
            .lock()
            .map_err(|_| anyhow::anyhow!("decay sweep lock is poisoned"))?;
        let now = Utc::now();
        if now - *last_sweep < chrono::Duration::hours(1) {
            return Ok(0);
        }
        let expired = self.expire_memories()?;
        *last_sweep = now;
        Ok(expired)
    }

    pub fn apply_playbook(
        &self,
        memory_id: Uuid,
        session_id: Uuid,
        successful: bool,
    ) -> Result<bool> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, memory_id)?;
        if memory.metadata.knowledge_type != KnowledgeType::Playbook {
            bail!("memory is not a playbook");
        }
        if memory.metadata.status == MemoryStatus::Forgotten {
            return Ok(false);
        }
        if !self
            .sessions
            .record_application(memory_id, session_id, successful)?
        {
            return Ok(false);
        }
        self.sessions.record_access(
            memory_id,
            if successful {
                ReinforcementSignal::SuccessfullyApplied
            } else {
                ReinforcementSignal::FailedApplication
            },
        )?;
        if successful {
            let successes = memory.metadata.successes.get_or_insert(0);
            *successes = successes.saturating_add(1);
            if memory.metadata.status == MemoryStatus::Candidate && *successes >= 2 {
                memory.metadata.status = MemoryStatus::Active;
            }
            memory.metadata.last_verified_at = Some(Utc::now());
        } else {
            let failures = memory.metadata.failures.get_or_insert(0);
            *failures = failures.saturating_add(1);
        }
        memory.metadata.updated_at = Utc::now();
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        Ok(true)
    }

    pub fn session_start_context(&self, cwd: &Path, session_key: &str) -> Result<String> {
        self.session_start_context_for_client(cwd, "direct", session_key)
    }

    pub fn session_start_context_for_client(
        &self,
        cwd: &Path,
        client: &str,
        external_session_id: &str,
    ) -> Result<String> {
        let Some(project) = self.ensure_project(cwd)? else {
            return Ok(String::new());
        };
        let identity =
            self.sessions
                .injection_identity(client, external_session_id, Some(&project.id))?;
        let handoff = self.current_handoff_items(Some(&project.id))?;
        let mut context = String::from(
            "MENVANE MEMORY CONTEXT\nHistorical context only.\nCurrent user instructions and current repository state are authoritative.\n\n",
        );
        context.push_str("Scope: project\n");
        context.push_str(&format!("Project: {}\n", project.identity));
        if !handoff.is_empty() {
            context.push_str("\n[CURRENT HANDOFF]\n");
            context.push_str(&session_rendering::render_handoff_items(&handoff, 2_000));
            context.push('\n');
        }
        context.push_str(
            "\nAdditional memory is available through recall.\nEND MENVANE MEMORY CONTEXT",
        );
        let handoff_content_id = content_identifier(&session_rendering::render_handoff_items(
            &handoff,
            usize::MAX,
        ));
        if self
            .sessions
            .claim_delivery(&identity, HANDOFF_DELIVERY_KIND, &handoff_content_id)?
        {
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
        let PromptRecall {
            results,
            mut diagnostics,
            identity,
        } = self.prompt_recall(cwd, client, external_session_id, prompt, 3)?;
        let project = self.ensure_project(cwd)?;
        let items = self.current_handoff_items(project.as_ref().map(|value| value.id.as_str()))?;
        let has_handoff_items = !items.is_empty();
        let prompt_tokens = handoff_lexical_tokens(prompt);
        let required_match_count = required_handoff_match_count(prompt_tokens.len());
        let mut observed_match_terms = HashSet::new();
        let mut related = Vec::new();
        for item in items {
            let text = format!(
                "{} {} {}",
                item.state,
                item.next_step.as_deref().unwrap_or_default(),
                item.blocker.as_deref().unwrap_or_default()
            );
            let item_tokens = handoff_lexical_tokens(&text);
            let match_terms = prompt_tokens
                .intersection(&item_tokens)
                .cloned()
                .collect::<HashSet<_>>();
            observed_match_terms.extend(match_terms.iter().cloned());
            if match_terms.len() >= required_match_count {
                related.push(item);
            }
        }
        diagnostics.handoff_scope = if project.is_some() {
            "project".to_owned()
        } else {
            "global".to_owned()
        };
        diagnostics.handoff_match_terms = observed_match_terms.into_iter().collect();
        diagnostics.handoff_match_terms.sort();
        diagnostics.handoff_required_match_count = required_match_count;
        let mut context = String::new();
        let related_handoff = session_rendering::render_handoff_items(&related, 2_000);
        let related_content_id = content_identifier(&session_rendering::render_handoff_items(
            &related,
            usize::MAX,
        ));
        diagnostics.handoff_reason = if prompt_tokens.is_empty() {
            "no-meaningful-prompt-terms".to_owned()
        } else if !has_handoff_items {
            "no-current-handoff".to_owned()
        } else if related.is_empty() {
            "insufficient-overlap".to_owned()
        } else if self.sessions.claim_delivery(
            &identity,
            HANDOFF_DELIVERY_KIND,
            &related_content_id,
        )? {
            context.push_str("MENVANE MEMORY CONTEXT\nHistorical context only.\nCurrent user instructions and current repository state are authoritative.\n\n[CURRENT HANDOFF]\n");
            context.push_str(&related_handoff);
            "delivered".to_owned()
        } else {
            "already-delivered".to_owned()
        };
        for result in &results {
            let entry = format!(
                "\n\n[MEMORY CARD]\nID: {}\nType: {}\nTitle: {}\nExcerpt: {}",
                result.id, result.knowledge_type, result.title, result.excerpt
            );
            if context.chars().count() + entry.chars().count() > 6_000 {
                break;
            }
            if self
                .sessions
                .claim_delivery(&identity, "memory", &result.id.to_string())?
            {
                context.push_str(&entry);
                if result.knowledge_type == KnowledgeType::Memory {
                    self.record_memory_reinforcement(result.id, ReinforcementSignal::Injected)?;
                }
            }
        }
        if !context.is_empty() {
            context.push_str("\n\nEND MENVANE MEMORY CONTEXT");
        }
        Ok((context, diagnostics))
    }

    pub fn set_integration_connected(&self, client: &str, connected: bool) -> Result<()> {
        self.sessions.set_integration_connected(client, connected)
    }

    pub fn import_session(&self, mut session: NormalizedSession) -> Result<ImportOutcome> {
        if let Some(cwd) = reliable_import_cwd(&session)? {
            let cwd = cwd.to_string_lossy().into_owned();
            session.cwd = Some(cwd.clone());
            for event in &mut session.events {
                event.cwd.clone_from(&cwd);
            }
        }
        let resolved_project_id = session
            .cwd
            .as_deref()
            .map(Path::new)
            .filter(|cwd| cwd.exists())
            .map(ProjectResolver::resolve)
            .transpose()?
            .flatten()
            .map(|resolution| resolution.id);
        let mut retry_import = false;
        let mut reattribute_import = false;
        let mut reconsolidate_import = false;
        if self
            .sessions
            .import_exists(&session.client, &session.external_session_id)?
        {
            let existing = self
                .sessions
                .latest_session(&session.client, &session.external_session_id)?;
            retry_import = if let Some(existing) = existing {
                let incoming_has_content = has_consolidation_content(&session.events);
                reattribute_import = existing.imported
                    && existing.project_id.is_none()
                    && resolved_project_id.is_some();
                reconsolidate_import = existing.imported
                    && self
                        .sessions
                        .consolidation_result(existing.id)?
                        .is_some_and(|marker| has_unmaterialized_continuity(&marker.result));
                existing.imported
                    && incoming_has_content
                    && (reattribute_import
                        || reconsolidate_import
                        || existing.state != SessionState::Finalized
                        || (existing.summary_status == SummaryStatus::Skipped
                            && !has_consolidation_content(&self.sessions.events(existing.id)?)))
            } else {
                false
            };
            if !retry_import {
                return Ok(ImportOutcome::AlreadyImported);
            }
        }
        let Some(cwd) = session.cwd.as_deref() else {
            self.record_orphan(&session)?;
            return Ok(ImportOutcome::Orphan);
        };
        if !Path::new(cwd).exists() {
            session.cwd = None;
            self.record_orphan(&session)?;
            return Ok(ImportOutcome::Orphan);
        }
        if retry_import {
            for event in &mut session.events {
                if reattribute_import
                    || reconsolidate_import
                    || matches!(
                        event.kind,
                        NormalizedEventKind::SessionStarted | NormalizedEventKind::SessionEnded
                    )
                {
                    event.event_id.push_str(if reattribute_import {
                        ":project-retry"
                    } else if reconsolidate_import {
                        ":continuity-retry"
                    } else {
                        ":retry"
                    });
                }
            }
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
            .orphans()?
            .into_iter()
            .find(|value| {
                value.client == client && value.external_session_id == external_session_id
            })
            .context("orphan session not found")?;
        let project = self
            .all_projects()?
            .into_iter()
            .find(|value| value.id == project_id)
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
        self.import_session(session)
    }

    pub fn all_projects(&self) -> Result<Vec<Project>> {
        self.markdown
            .project_files()?
            .into_iter()
            .map(|path| self.markdown.parse_project(&path))
            .collect()
    }

    pub fn all_memories(&self) -> Result<Vec<KnowledgeRecord>> {
        self.expire_memories_if_due()?;
        self.markdown
            .memory_files()?
            .into_iter()
            .map(|path| self.markdown.parse_memory(&path))
            .collect()
    }

    pub fn edit_memory(&self, id: Uuid, title: &str, body: &str) -> Result<KnowledgeRecord> {
        let (mut memory, path) = self.index.read_memory(&self.markdown, id)?;
        if title.trim().is_empty() {
            bail!("memory title cannot be empty");
        }
        memory.title = title.trim().to_owned();
        memory.body = body.trim().to_owned();
        memory.metadata.updated_at = Utc::now();
        self.markdown.update_memory(&path, &memory)?;
        self.index.upsert_memory(&memory, &path)?;
        self.refresh_memory_embedding(&memory)?;
        self.markdown.commit(&format!("docs(memory): edit {id}"));
        Ok(memory)
    }

    pub fn configuration_text(&self) -> Result<String> {
        Ok(fs::read_to_string(self.home.join("config.toml"))?)
    }

    pub fn update_configuration_text(&self, configuration: &str) -> Result<()> {
        let parsed: MenvaneConfig = toml::from_str(configuration)?;
        if parsed.decay.memory_lifetime_days < 1 {
            bail!("decay.memory_lifetime_days must be at least 1");
        }
        let lowercase = configuration.to_ascii_lowercase();
        for forbidden in ["api_key =", "token =", "password =", "secret ="] {
            if lowercase.contains(forbidden) {
                bail!("secrets must be supplied through environment variables");
            }
        }
        atomic_replace(&self.home.join("config.toml"), configuration.as_bytes())
    }

    pub fn configure_openai(&self, model: &str, reasoning_effort: Option<&str>) -> Result<()> {
        if model.trim().is_empty() {
            bail!("OpenAI model cannot be empty");
        }
        if reasoning_effort
            .is_some_and(|value| !matches!(value, "minimal" | "low" | "medium" | "high" | "xhigh"))
        {
            bail!("reasoning effort must be minimal, low, medium, high, or xhigh");
        }
        let mut configuration: toml::Table = toml::from_str(&self.configuration_text()?)?;
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
        llm.remove("github_client_id");
        llm.remove("github_oauth_issuer");
        if let Some(value) = reasoning_effort {
            llm.insert(
                "reasoning_effort".to_owned(),
                toml::Value::String(value.to_owned()),
            );
        }
        atomic_replace(
            &self.home.join("config.toml"),
            toml::to_string_pretty(&configuration)?.as_bytes(),
        )
    }

    pub fn configure_github_copilot(
        &self,
        model: &str,
        reasoning_effort: Option<&str>,
        client_id: &str,
    ) -> Result<()> {
        if model.trim().is_empty() {
            bail!("GitHub Copilot model cannot be empty");
        }
        if client_id.trim().is_empty() {
            bail!("GitHub OAuth client ID cannot be empty");
        }
        if reasoning_effort
            .is_some_and(|value| !matches!(value, "minimal" | "low" | "medium" | "high" | "xhigh"))
        {
            bail!("reasoning effort must be minimal, low, medium, high, or xhigh");
        }
        let mut configuration: toml::Table = toml::from_str(&self.configuration_text()?)?;
        let llm = configuration
            .entry("llm")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("llm configuration must be a table")?;
        llm.insert(
            "provider".to_owned(),
            toml::Value::String("github-copilot".to_owned()),
        );
        llm.insert(
            "model".to_owned(),
            toml::Value::String(model.trim().to_owned()),
        );
        llm.insert(
            "github_client_id".to_owned(),
            toml::Value::String(client_id.trim().to_owned()),
        );
        llm.insert(
            "github_oauth_issuer".to_owned(),
            toml::Value::String(default_github_oauth_issuer()),
        );
        llm.insert(
            "base_url".to_owned(),
            toml::Value::String(default_github_api_endpoint()),
        );
        llm.remove("api_key_env");
        llm.remove("oauth_issuer");
        llm.remove("oauth_endpoint");
        if let Some(value) = reasoning_effort {
            llm.insert(
                "reasoning_effort".to_owned(),
                toml::Value::String(value.to_owned()),
            );
        }
        atomic_replace(
            &self.home.join("config.toml"),
            toml::to_string_pretty(&configuration)?.as_bytes(),
        )
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

    pub async fn login_github_copilot(&self) -> Result<()> {
        GithubCopilotProvider::with_endpoints(
            &self.home,
            &self.config.llm.model,
            self.config.llm.reasoning_effort.clone(),
            &self.config.llm.github_client_id,
            &self.config.llm.github_oauth_issuer,
            &self.config.llm.base_url,
        )
        .login()
        .await
        .map_err(anyhow::Error::new)
    }

    pub fn logout_github_copilot(&self) -> Result<()> {
        GithubCopilotProvider::new(
            &self.home,
            &self.config.llm.model,
            self.config.llm.reasoning_effort.clone(),
            &self.config.llm.github_client_id,
            &self.config.llm.base_url,
        )
        .logout()
        .map_err(anyhow::Error::new)
    }

    pub fn integrations(&self) -> Result<Vec<IntegrationRecord>> {
        self.sessions.integrations()
    }

    pub fn configured_provider(&self) -> Result<Arc<dyn LlmProvider>> {
        if let Some(provider) = &self.provider_override {
            return Ok(provider.clone());
        }
        let primary = provider_from_configuration(&self.config.llm, &self.home)?;
        let fallback = self
            .config
            .llm
            .fallback
            .as_deref()
            .map(|value| provider_from_configuration(value, &self.home))
            .transpose()?;
        Ok(Arc::new(ProviderChain::new(primary, fallback)))
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
        let response = self.configured_provider()?.generate_structured(LlmRequest { system: "Return the requested structured health response.".to_owned(), prompt: "Return {\"ok\": true}.".to_owned(), timeout: std::time::Duration::from_secs(30) }, JsonSchema(serde_json::json!({"type":"object","additionalProperties":false,"required":["ok"],"properties":{"ok":{"type":"boolean","const":true}}}))).await.map_err(anyhow::Error::new)?;
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

    pub async fn process_next_job(&self) -> Result<bool> {
        let Some(job) = self
            .sessions
            .claim_job(&self.worker_owner, self.config.jobs.lease_timeout_seconds)?
        else {
            return Ok(false);
        };
        let result = match job.job_type.as_str() {
            "finalize_session" => SessionEngine::new(self).finalize_job(&job).map(|_| None),
            "consolidate_session" => self.process_consolidate_job(&job).await.map(Some),
            _ => bail!("unsupported job type: {}", job.job_type),
        };
        match result {
            Ok(provider) => self.sessions.finish_job(
                job.id,
                job.owner.as_deref().unwrap_or_default(),
                provider.as_deref(),
                None,
                false,
            )?,
            Err(error) => {
                let retryable = error
                    .downcast_ref::<LlmError>()
                    .is_some_and(|error| error.fallback_allowed());
                self.sessions.finish_job(
                    job.id,
                    job.owner.as_deref().unwrap_or_default(),
                    None,
                    Some(&error.to_string()),
                    retryable,
                )?;
            }
        }
        Ok(true)
    }

    pub fn doctor(&self) -> DoctorReport {
        let mut checks = Vec::new();
        checks.push(check(
            "home writable",
            fs::write(self.home.join(format!(".doctor-{}", Uuid::now_v7())), b"ok")
                .map(|_| "writable".to_owned())
                .map_err(Into::into),
        ));
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
        checks.push(check(
            "Markdown/index consistency",
            (|| {
                let markdown = (
                    self.markdown.project_files()?.len(),
                    self.markdown.memory_files()?.len(),
                );
                let index = (
                    self.index.project_count()? as usize,
                    self.index.memory_count()? as usize,
                );
                (markdown == index)
                    .then(|| format!("{} projects, {} memories", markdown.0, markdown.1))
                    .context("Markdown and index counts differ")
            })(),
        ));
        checks.push(check(
            "Git",
            Command::new("git")
                .arg("--version")
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .map_err(Into::into),
        ));
        DoctorReport { checks }
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
        let files = backup_files(destination)?;
        let manifest = BackupManifest {
            version: 2,
            created_at: Utc::now(),
            files: files
                .into_iter()
                .map(|path| {
                    Ok((
                        path.strip_prefix(destination)?
                            .to_string_lossy()
                            .into_owned(),
                        sha256_file(&path)?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?,
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
        fs::create_dir_all(&staging)?;
        copy_tree(&source.join("memory"), &staging.join("memory"))?;
        for name in ["config.toml", "index.sqlite", "state.sqlite"] {
            fs::copy(source.join(name), staging.join(name))?;
        }
        for name in ["memory", "config.toml", "index.sqlite", "state.sqlite"] {
            let current = self.home.join(name);
            if current.exists() {
                if current.is_file() {
                    fs::remove_file(&current)?;
                } else {
                    fs::remove_dir_all(&current)?;
                }
            }
            fs::rename(staging.join(name), current)?;
        }
        fs::remove_dir_all(staging)?;
        Ok(())
    }

    async fn process_consolidate_job(&self, job: &JobRecord) -> Result<String> {
        let session_id: Uuid = job.dedupe_key.parse()?;
        if let Some(marker) = self.sessions.consolidation_result(session_id)? {
            let session = self.sessions.session(session_id)?;
            self.update_session_summary(&session, &marker.result)?;
            self.apply_consolidation(session_id, session.project_id.as_deref(), &marker.result)?;
            return Ok(marker.execution.provider);
        }
        let session = self.sessions.session(session_id)?;
        let sanitizer = CaptureSanitizer::new(self.config.capture.clone())?;
        let events = self
            .sessions
            .events(session_id)?
            .into_iter()
            .filter_map(|event| sanitizer.filter_durable_event(event))
            .collect::<Vec<_>>();
        if !session_engine::is_session_worth_compiling(&events) {
            self.sessions
                .set_session_summary(session_id, SummaryStatus::Skipped, None)?;
            return Ok("none".to_owned());
        }
        let packet_events = events
            .iter()
            .filter(|event| {
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
            .cloned()
            .collect();
        let packet = ConsolidationPacket {
            session_id,
            events: packet_events,
            handoff_items: self
                .sessions
                .current_handoff(session.project_id.as_deref())?,
            related_summaries: self.related_summaries(
                &events,
                &self
                    .sessions
                    .current_handoff(session.project_id.as_deref())?,
                session.project_id.as_deref(),
            )?,
            related_memories: self.related_memories(&events, session.project_id.as_deref())?,
        };
        let prompt = self
            .config
            .llm
            .consolidation_prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| session_consolidator::CONSOLIDATION_SYSTEM_PROMPT.to_owned());
        let outcome = SessionConsolidator::new(self.configured_provider()?)
            .with_prompt(prompt)
            .consolidate(&packet)
            .await
            .map_err(anyhow::Error::new)?;
        self.update_session_summary(&session, &outcome.response)?;
        self.apply_consolidation(session_id, session.project_id.as_deref(), &outcome.response)?;
        self.sessions
            .record_consolidation(session_id, &outcome.response, &outcome.execution)?;
        Ok(outcome.provider)
    }

    fn apply_consolidation(
        &self,
        session_id: Uuid,
        project_id: Option<&str>,
        result: &ConsolidationResult,
    ) -> Result<()> {
        for (index, operation) in result.handoff.iter().enumerate() {
            self.apply_handoff_operation(session_id, project_id, index, operation)?;
        }
        for (index, operation) in result.knowledge.iter().enumerate() {
            self.apply_knowledge_operation(session_id, project_id, operation, index)?;
        }
        Ok(())
    }

    fn apply_handoff_operation(
        &self,
        session_id: Uuid,
        project_id: Option<&str>,
        operation_index: usize,
        operation: &HandoffItemOperation,
    ) -> Result<()> {
        let now = Utc::now();
        let source = |event_ids: &[String]| HandoffItemSource {
            session_id,
            event_ids: event_ids.to_vec(),
        };
        match operation {
            HandoffItemOperation::Keep { .. } => {}
            HandoffItemOperation::Uncertain { item_id } => {
                let mut item = self.handoff_item(*item_id, project_id)?;
                if item.low_confidence
                    && item
                        .sources
                        .iter()
                        .any(|value| value.session_id == session_id && value.event_ids.is_empty())
                {
                    return Ok(());
                }
                item.low_confidence = true;
                item.updated_at = now;
                item.sources.push(source(&[]));
                self.sessions.upsert_handoff_item(&item)?;
            }
            HandoffItemOperation::Update(value) => {
                let mut item = self.handoff_item(value.item_id, project_id)?;
                if item.kind == value.kind
                    && item.state == value.state
                    && item.next_step == value.next_step
                    && item.blocker == value.blocker
                    && item.sources.iter().any(|source| {
                        source.session_id == session_id
                            && source.event_ids == value.evidence_event_ids
                    })
                {
                    return Ok(());
                }
                item.kind = value.kind;
                item.state.clone_from(&value.state);
                item.next_step.clone_from(&value.next_step);
                item.blocker.clone_from(&value.blocker);
                item.last_confirmed_at = now;
                item.updated_at = now;
                item.sources.push(source(&value.evidence_event_ids));
                self.sessions.upsert_handoff_item(&item)?;
            }
            HandoffItemOperation::Resolve(value) | HandoffItemOperation::Discard(value) => {
                self.sessions.remove_handoff_item(value.item_id)?;
            }
            HandoffItemOperation::Replace(value) => {
                self.sessions.remove_handoff_item(value.item_id)?;
                self.create_handoff_item(
                    session_id,
                    project_id,
                    operation_index,
                    &value.replacement,
                    &value.evidence_event_ids,
                    now,
                )?;
            }
            HandoffItemOperation::Create(value) => self.create_handoff_item(
                session_id,
                project_id,
                operation_index,
                &value.item,
                &value.evidence_event_ids,
                now,
            )?,
        }
        Ok(())
    }

    fn create_handoff_item(
        &self,
        session_id: Uuid,
        project_id: Option<&str>,
        operation_index: usize,
        value: &menvane_domain::NewHandoffItem,
        event_ids: &[String],
        now: chrono::DateTime<Utc>,
    ) -> Result<()> {
        let id = deterministic_uuid(
            session_id,
            &[operation_index.to_string()],
            &serde_json::to_vec(value)?,
        );
        if let Some(existing) = self
            .sessions
            .current_handoff(project_id)?
            .into_iter()
            .find(|item| item.id == id)
            && existing.kind == value.kind
            && existing.state == value.state
            && existing.next_step == value.next_step
            && existing.blocker == value.blocker
            && existing
                .sources
                .iter()
                .any(|source| source.session_id == session_id && source.event_ids == event_ids)
        {
            return Ok(());
        }
        self.sessions.upsert_handoff_item(&HandoffItem {
            id,
            project_id: project_id.map(str::to_owned),
            kind: value.kind,
            state: value.state.clone(),
            next_step: value.next_step.clone(),
            blocker: value.blocker.clone(),
            low_confidence: false,
            last_confirmed_at: now,
            sources: vec![HandoffItemSource {
                session_id,
                event_ids: event_ids.to_vec(),
            }],
            created_at: now,
            updated_at: now,
        })
    }

    fn handoff_item(&self, id: Uuid, project_id: Option<&str>) -> Result<HandoffItem> {
        self.sessions
            .current_handoff(project_id)?
            .into_iter()
            .find(|item| item.id == id)
            .context("handoff item not found")
    }

    fn apply_knowledge_operation(
        &self,
        session_id: Uuid,
        project_id: Option<&str>,
        operation: &KnowledgeOperation,
        index: usize,
    ) -> Result<()> {
        match operation.operation {
            KnowledgeOperationKind::NoOp => Ok(()),
            KnowledgeOperationKind::Reinforce => {
                if let Some(id) = operation.target_memory_ids.first() {
                    let (mut memory, path) = self.index.read_memory(&self.markdown, *id)?;
                    if memory.metadata.status == MemoryStatus::Forgotten {
                        return Ok(());
                    }
                    if memory.metadata.source_sessions.contains(&session_id) {
                        return Ok(());
                    }
                    memory.metadata.source_sessions.push(session_id);
                    memory.metadata.updated_at = Utc::now();
                    self.markdown.update_memory(&path, &memory)?;
                    self.index.upsert_memory(&memory, &path)?;
                }
                Ok(())
            }
            KnowledgeOperationKind::Create => {
                let knowledge_type = operation
                    .knowledge_type
                    .context("create operation has no knowledge type")?;
                let title = operation
                    .title
                    .clone()
                    .context("create operation has no title")?;
                let scope = operation.scope.unwrap_or(Scope::Project);
                let project = if scope == Scope::Project {
                    self.project_for_id(project_id)?
                } else {
                    None
                };
                let scope = project.as_ref().map_or(Scope::Global, |_| scope);
                let mut metadata = KnowledgeMetadata::new(
                    knowledge_type,
                    scope,
                    project.as_ref().map(|value| value.id.clone()),
                    Vec::new(),
                    operation.applies_to.clone(),
                    if knowledge_type == KnowledgeType::Playbook {
                        MemoryStatus::Candidate
                    } else {
                        MemoryStatus::Active
                    },
                );
                metadata.id =
                    deterministic_uuid(session_id, &[index.to_string()], title.as_bytes());
                metadata.source_sessions.push(session_id);
                let body = operation
                    .content
                    .as_ref()
                    .map(content_markdown)
                    .context("create operation has no content")?;
                if self.duplicate_memory(&metadata, &title, &body)? {
                    return Ok(());
                }
                let memory = KnowledgeRecord {
                    metadata,
                    title,
                    body,
                };
                let path = self.markdown.write_memory(&memory, project.as_ref())?;
                self.index.upsert_memory(&memory, &path)?;
                self.refresh_memory_embedding(&memory)?;
                self.markdown
                    .commit(&format!("feat(memory): consolidate {session_id}:{index}"));
                Ok(())
            }
            KnowledgeOperationKind::Merge => {
                let mut targets = operation
                    .target_memory_ids
                    .iter()
                    .filter_map(|id| self.index.read_memory(&self.markdown, *id).ok())
                    .filter(|(memory, _)| memory.metadata.status != MemoryStatus::Forgotten)
                    .collect::<Vec<_>>();
                let Some((mut survivor, survivor_path)) = targets.first().cloned() else {
                    return Ok(());
                };
                let body = operation
                    .content
                    .as_ref()
                    .map(content_markdown)
                    .context("merge operation has no content")?;
                survivor.title = operation.title.clone().unwrap_or(survivor.title);
                survivor.body = body;
                if !survivor.metadata.source_sessions.contains(&session_id) {
                    survivor.metadata.source_sessions.push(session_id);
                }
                survivor.metadata.applies_to = operation.applies_to.clone();
                survivor.metadata.updated_at = Utc::now();
                for (memory, _) in targets.iter().skip(1) {
                    for source_session in &memory.metadata.source_sessions {
                        if !survivor.metadata.source_sessions.contains(source_session) {
                            survivor.metadata.source_sessions.push(*source_session);
                        }
                    }
                    if !survivor.metadata.supersedes.contains(&memory.metadata.id) {
                        survivor.metadata.supersedes.push(memory.metadata.id);
                    }
                }
                self.markdown.update_memory(&survivor_path, &survivor)?;
                self.index.upsert_memory(&survivor, &survivor_path)?;
                self.refresh_memory_embedding(&survivor)?;
                for (mut memory, path) in targets.drain(1..) {
                    if memory.metadata.status == MemoryStatus::Superseded {
                        continue;
                    }
                    memory.metadata.status = MemoryStatus::Superseded;
                    memory.metadata.updated_at = Utc::now();
                    self.markdown.update_memory(&path, &memory)?;
                    self.index.upsert_memory(&memory, &path)?;
                }
                Ok(())
            }
            KnowledgeOperationKind::Supersede => {
                let mut superseded = Vec::new();
                for id in &operation.target_memory_ids {
                    let Ok((mut memory, path)) = self.index.read_memory(&self.markdown, *id) else {
                        continue;
                    };
                    if memory.metadata.status == MemoryStatus::Forgotten {
                        continue;
                    }
                    superseded.push(memory.metadata.id);
                    if memory.metadata.status != MemoryStatus::Superseded {
                        memory.metadata.status = MemoryStatus::Superseded;
                        memory.metadata.updated_at = Utc::now();
                        self.markdown.update_memory(&path, &memory)?;
                        self.index.upsert_memory(&memory, &path)?;
                    }
                }
                if superseded.is_empty() {
                    return Ok(());
                }
                let knowledge_type = operation
                    .knowledge_type
                    .context("supersede operation has no knowledge type")?;
                let title = operation
                    .title
                    .clone()
                    .context("supersede operation has no title")?;
                let body = operation
                    .content
                    .as_ref()
                    .map(content_markdown)
                    .context("supersede operation has no content")?;
                let project = if operation.scope == Some(Scope::Project) {
                    self.project_for_id(project_id)?
                } else {
                    None
                };
                let scope = project
                    .as_ref()
                    .map_or(Scope::Global, |_| operation.scope.unwrap_or(Scope::Project));
                let mut metadata = KnowledgeMetadata::new(
                    knowledge_type,
                    scope,
                    project.as_ref().map(|value| value.id.clone()),
                    Vec::new(),
                    operation.applies_to.clone(),
                    if knowledge_type == KnowledgeType::Playbook {
                        MemoryStatus::Candidate
                    } else {
                        MemoryStatus::Active
                    },
                );
                metadata.id =
                    deterministic_uuid(session_id, &[index.to_string()], title.as_bytes());
                metadata.source_sessions.push(session_id);
                metadata.supersedes = superseded;
                if self.index.read_memory(&self.markdown, metadata.id).is_ok() {
                    return Ok(());
                }
                let memory = KnowledgeRecord {
                    metadata,
                    title,
                    body,
                };
                let path = self.markdown.write_memory(&memory, project.as_ref())?;
                self.index.upsert_memory(&memory, &path)?;
                self.refresh_memory_embedding(&memory)?;
                Ok(())
            }
        }
    }

    fn duplicate_memory(
        &self,
        metadata: &KnowledgeMetadata,
        title: &str,
        body: &str,
    ) -> Result<bool> {
        let title = normalize_memory_text(title);
        let body = normalize_memory_text(body);
        Ok(self.all_memories()?.into_iter().any(|memory| {
            memory.metadata.knowledge_type == metadata.knowledge_type
                && normalize_memory_text(&memory.title) == title
                && normalize_memory_text(&memory.body) == body
        }))
    }

    fn related_memories(
        &self,
        events: &[NormalizedEvent],
        project_id: Option<&str>,
    ) -> Result<Vec<menvane_domain::RelatedMemory>> {
        let query = events
            .iter()
            .filter_map(|event| {
                event
                    .bounded_input
                    .as_deref()
                    .or(event.bounded_output.as_deref())
            })
            .collect::<Vec<_>>()
            .join(" ");
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let results = self.index.search(
            &query,
            project_id.map_or(SearchScope::Global, SearchScope::Auto),
            MAX_RELATED_MEMORIES,
            false,
            false,
        )?;
        results
            .into_iter()
            .map(|result| {
                let memory = self.read_without_recording(result.id)?;
                Ok(menvane_domain::RelatedMemory {
                    id: memory.metadata.id,
                    knowledge_type: memory.metadata.knowledge_type,
                    scope: memory.metadata.scope,
                    status: memory.metadata.status,
                    title: memory.title,
                    body: memory.body,
                    source_sessions: memory.metadata.source_sessions,
                })
            })
            .collect()
    }

    fn update_session_summary(
        &self,
        session: &menvane_store::SessionRecord,
        result: &ConsolidationResult,
    ) -> Result<()> {
        let Some(path) = &session.markdown_path else {
            return Ok(());
        };
        let parsed = self.markdown.parse_session(path)?;
        let chronology = parsed
            .body
            .split_once("\n## Episodic summary\n")
            .map_or(parsed.body.as_str(), |value| value.0);
        let metadata = SessionMetadata {
            summary_status: SummaryStatus::Ready,
            summary: Some(result.summary.clone()),
            id: session.id,
            client: session.client.clone(),
            external_session_id: session.external_session_id.clone(),
            project_id: session.project_id.clone(),
            started_at: Some(session.started_at),
            ended_at: session.ended_at,
            imported: session.imported,
            generation: session.generation,
        };
        self.markdown.update_session_summary(
            path,
            &metadata,
            chronology,
            &summary_markdown(&result.summary),
        )?;
        self.markdown
            .commit(&format!("feat(session): summarize {}", session.id));
        self.index.upsert_session_summary(
            session.id,
            session.project_id.as_deref(),
            session.ended_at,
            &result.summary,
        )?;
        Ok(())
    }

    fn related_summaries(
        &self,
        events: &[NormalizedEvent],
        handoff: &[HandoffItem],
        project_id: Option<&str>,
    ) -> Result<Vec<menvane_domain::RelatedSummary>> {
        let query = events
            .iter()
            .flat_map(|event| {
                [
                    event.bounded_input.as_deref(),
                    event.bounded_output.as_deref(),
                    event.attributed_path.as_deref(),
                ]
            })
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let source_sessions = handoff
            .iter()
            .flat_map(|item| item.sources.iter().map(|source| source.session_id))
            .collect::<Vec<_>>();
        self.index.related_summaries(
            project_id,
            &source_sessions,
            &query,
            MAX_SUMMARY_SELECTION_SESSIONS.min(5),
            MAX_SUMMARY_SELECTION_BYTES,
        )
    }

    fn search_inner(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_inner_with_sessions(cwd, query, scope, limit, false)
    }

    fn search_inner_with_sessions(
        &self,
        cwd: &Path,
        query: &str,
        scope: ScopeSelection,
        limit: usize,
        include_sessions: bool,
    ) -> Result<Vec<SearchResult>> {
        self.expire_memories_if_due()?;
        let project = match scope {
            ScopeSelection::Global => None,
            ScopeSelection::Auto | ScopeSelection::Project => self.ensure_project(cwd)?,
        };
        let search_scope = match scope {
            ScopeSelection::Auto => project.as_ref().map_or(SearchScope::Global, |value| {
                SearchScope::Auto(value.id.as_str())
            }),
            ScopeSelection::Project => project.as_ref().map_or(SearchScope::Global, |value| {
                SearchScope::Project(value.id.as_str())
            }),
            ScopeSelection::Global => SearchScope::Global,
        };
        self.index
            .search(query, search_scope, limit, include_sessions, false)
    }

    fn record_orphan(&self, session: &NormalizedSession) -> Result<()> {
        self.sessions.record_import(
            &session.client,
            &session.external_session_id,
            "orphan",
            Some(&serde_json::to_string(session)?),
        )
    }

    fn project_for_id(&self, project_id: Option<&str>) -> Result<Option<Project>> {
        project_id.map_or(Ok(None), |id| {
            self.all_projects()
                .map(|projects| projects.into_iter().find(|project| project.id == id))
        })
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
        let changed = project.identity != resolution.identity
            || project.name != resolution.name
            || !project.known_paths.contains(&known_path)
            || project.technologies != technologies;
        project.identity = resolution.identity;
        project.name = resolution.name;
        project.technologies = technologies;
        if !project.known_paths.contains(&known_path) {
            project.known_paths.push(known_path);
            project.known_paths.sort();
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

fn has_consolidation_content(events: &[NormalizedEvent]) -> bool {
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

fn has_unmaterialized_continuity(result: &ConsolidationResult) -> bool {
    let new_continuations = result
        .summary
        .continuity
        .iter()
        .filter(|item| {
            item.disposition == menvane_domain::ContinuityDisposition::Continues
                && item.item_id.is_none()
        })
        .count();
    let creations = result
        .handoff
        .iter()
        .filter(|operation| matches!(operation, HandoffItemOperation::Create(_)))
        .count();
    creations < new_continuations
}

fn reliable_import_cwd(session: &NormalizedSession) -> Result<Option<PathBuf>> {
    if let Some(cwd) = session.cwd.as_deref()
        && Path::new(cwd).exists()
        && let Some(resolution) = ProjectResolver::resolve(Path::new(cwd))?
    {
        return Ok(Some(resolution.root));
    }
    let mut projects = BTreeMap::new();
    for event in session.events.iter().filter(|event| {
        event.success == Some(true)
            && matches!(
                event.tool_family.as_deref(),
                Some("apply_patch" | "edit" | "write" | "read" | "search" | "grep" | "glob")
            )
    }) {
        let Some(path) = event.attributed_path.as_deref() else {
            continue;
        };
        let path = Path::new(path);
        if !path.exists() {
            continue;
        }
        let probe = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        if let Some(resolution) = ProjectResolver::resolve(probe)? {
            projects.insert(resolution.id, resolution.root);
        }
    }
    Ok((projects.len() == 1)
        .then(|| projects.into_values().next())
        .flatten())
}

#[derive(Debug, Clone, Copy)]
pub enum ScopeSelection {
    Auto,
    Project,
    Global,
}

fn check(name: &'static str, result: Result<String>) -> DoctorCheck {
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

fn content_markdown(content: &KnowledgeContent) -> String {
    match content {
        KnowledgeContent::Memory(value) => value.body.trim().to_owned(),
        KnowledgeContent::Playbook(value) => format!(
            "## Trigger\n\n{}\n\n## Applicability\n\n{}\n\n## Steps\n\n{}\n\n## Validation\n\n{}\n\n## Failure handling\n\n{}",
            value.trigger,
            serde_json::to_string(&value.applicability).unwrap_or_default(),
            value
                .steps
                .iter()
                .enumerate()
                .map(|(index, step)| format!("{}. {step}", index + 1))
                .collect::<Vec<_>>()
                .join("\n"),
            value.validation.join("\n"),
            value.failure_handling
        ),
    }
}

fn summary_markdown(summary: &menvane_domain::EpisodicSummary) -> String {
    format!(
        "### Intentions\n{}\n\n### Actions\n{}\n\n### Outcome\n{:?}\n\n### Result\n{}\n\n### Continuity\n{}\n\n### Candidate learnings\n{}",
        bullets(&summary.intentions),
        bullets(&summary.actions),
        summary.outcome,
        summary.result,
        summary
            .continuity
            .iter()
            .map(|item| format!("- {:?}: {}", item.disposition, item.front))
            .collect::<Vec<_>>()
            .join("\n"),
        bullets(&summary.candidate_learnings)
    )
}

fn bullets(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn lexical_tokens(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|raw| {
            let technical = raw.chars().any(|character| character.is_ascii_digit())
                || (raw.chars().count() >= 2
                    && raw
                        .chars()
                        .filter(|character| character.is_alphabetic())
                        .all(|character| character.is_uppercase()));
            let token = stopwords::normalize(raw);
            (token.chars().count() >= 3 && (technical || !stopwords::contains(&token)))
                .then_some(token)
        })
        .collect()
}

fn automatic_recall_tokens(value: &str) -> HashSet<String> {
    value
        .split_whitespace()
        .filter(|fragment| !looks_like_path(fragment))
        .flat_map(lexical_tokens)
        .collect()
}

fn handoff_lexical_tokens(value: &str) -> HashSet<String> {
    automatic_recall_tokens(value)
        .into_iter()
        .filter(|token| !GENERIC_HANDOFF_TERMS.contains(&token.as_str()))
        .collect()
}

fn looks_like_path(fragment: &str) -> bool {
    let fragment = fragment.trim_matches(|character: char| {
        matches!(
            character,
            '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
        )
    });
    fragment.contains('/') || fragment.contains('\\')
}

fn required_handoff_match_count(prompt_token_count: usize) -> usize {
    if prompt_token_count == 0 {
        0
    } else {
        prompt_token_count.div_ceil(3).clamp(2, 3)
    }
}

fn recall_diagnostics(query: String, result_count: usize) -> RecallDiagnostics {
    RecallDiagnostics {
        query,
        result_count,
        handoff_scope: "not-evaluated".to_owned(),
        handoff_match_terms: Vec::new(),
        handoff_required_match_count: 0,
        handoff_reason: "not-evaluated".to_owned(),
    }
}

fn meaningful_lexical_overlap(
    prompt_tokens: &HashSet<String>,
    memory_tokens: &HashSet<String>,
) -> bool {
    let overlap = prompt_tokens.intersection(memory_tokens).count();
    let required = if prompt_tokens.len() == 1 {
        1
    } else {
        prompt_tokens.len().div_ceil(3).clamp(2, 3)
    };
    overlap >= required
}

fn reciprocal_rank_fusion(lexical_rank: Option<usize>, embedding_rank: Option<usize>) -> f64 {
    let lexical = lexical_rank.map_or(0.0, |rank| 0.65 / (60.0 + rank as f64));
    let embedding = embedding_rank.map_or(0.0, |rank| 0.35 / (60.0 + rank as f64));
    lexical + embedding
}

fn normalize_memory_text(value: &str) -> String {
    value
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn automatically_eligible(result: &SearchResult, project: Option<&Project>) -> bool {
    if result.scope == "project" || result.applicability.is_empty() {
        return true;
    }
    project.is_some_and(|project| result.applicability.overlaps(&project.technologies))
}

fn content_identifier(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn deterministic_uuid(session_id: Uuid, parts: &[String], discriminator: &[u8]) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(session_id.as_bytes());
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    digest.update(discriminator);
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.finalize()[..16]);
    Uuid::from_bytes(bytes)
}
fn provider_from_configuration(
    configuration: &LlmConfiguration,
    home: &Path,
) -> Result<Arc<dyn LlmProvider>> {
    match configuration.provider.as_str() {
        "codex" => Ok(Arc::new(CodexProvider::new("codex", &configuration.model))),
        "openai" => Ok(Arc::new(OpenAiOAuthProvider::with_endpoints(
            home,
            &configuration.model,
            configuration.reasoning_effort.clone(),
            &configuration.oauth_issuer,
            &configuration.oauth_endpoint,
        ))),
        "openai-api" => Ok(Arc::new(
            OpenAIApiProvider::new(
                &configuration.model,
                &configuration.base_url,
                &configuration.api_key_env,
            )
            .with_reasoning_effort(configuration.reasoning_effort.clone()),
        )),
        "github-copilot" | "copilot" => Ok(Arc::new(GithubCopilotProvider::with_endpoints(
            home,
            &configuration.model,
            configuration.reasoning_effort.clone(),
            &configuration.github_client_id,
            &configuration.github_oauth_issuer,
            if configuration.base_url == default_api_url() {
                default_github_api_endpoint()
            } else {
                configuration.base_url.clone()
            },
        ))),
        "openrouter" => {
            if configuration.model.trim().is_empty() || configuration.model == "default" {
                bail!("OpenRouter requires an explicit model");
            }
            Ok(Arc::new(OpenRouterProvider::new(
                &configuration.model,
                if configuration.base_url == default_api_url() {
                    "https://openrouter.ai/api/v1"
                } else {
                    &configuration.base_url
                },
                if configuration.api_key_env == default_api_key_env() {
                    "OPENROUTER_API_KEY"
                } else {
                    &configuration.api_key_env
                },
            )))
        }
        provider => bail!("unsupported LLM provider: {provider}"),
    }
}

fn configured_embedding_provider(
    configuration: &EmbeddingConfiguration,
) -> Result<Option<Arc<dyn EmbeddingProvider>>> {
    let Some(provider) = configuration.provider.as_deref() else {
        return Ok(None);
    };
    if configuration.model.trim().is_empty() {
        bail!("embedding provider requires an explicit model")
    }
    if !(0.0..=1.0).contains(&configuration.min_similarity) {
        bail!("embedding minimum similarity must be between zero and one")
    }
    match provider {
        "openai-api" => Ok(Some(Arc::new(OpenAICompatibleEmbeddingProvider::new(
            provider,
            &configuration.model,
            &configuration.base_url,
            &configuration.api_key_env,
        )))),
        provider => bail!("unsupported embedding provider: {provider}"),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    version: u32,
    created_at: chrono::DateTime<Utc>,
    files: BTreeMap<String, String>,
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    let temporary = parent.join(format!(".config-{}.tmp", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    if !source.exists() {
        return Ok(());
    }
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
fn sha256_file(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
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
    Ok(())
}

fn acquire_daemon_lock(home: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(home.join("daemon.lock"))?;
    file.try_lock_exclusive()
        .context("cannot reindex while the Menvane daemon is running")?;
    Ok(file)
}
