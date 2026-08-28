# Menvane

Version: 2.12.0

Menvane is a local persistent memory system for agents. Its central product is operational continuity between agents, sessions, and different days. It preserves four distinct layers:

```text
chronological session evidence
        ↓
episodic session summary
        ↓
handoff of still-live work fronts
        +
non-obvious knowledge recovered on demand
```

Menvane does not generate project summaries, architecture descriptions, project purpose, or general project state. Code, documentation, configuration, and tests remain the canonical sources of the project.

## Storage

Menvane stores data under `~/.menvane` by default. `MENVANE_HOME` overrides that location.

The home contains `config.toml`, `index.sqlite`, `state.sqlite`, operational directories, global memory directories, and project memory directories. Durable knowledge, sessions, and project metadata are Markdown with YAML frontmatter, organized by responsibility: `sessions/` holds chronological sessions with their derived episodic summaries, `memories/` holds decaying durable memories, `playbooks/` holds durable playbooks, and project metadata lives in `project.md` without any narrative handoff.

`index.sqlite` is strictly derived and rebuildable: projects, knowledge records with FTS5, session summary metadata with FTS5 for internal selection, and optional reconstructible embeddings. `state.sqlite` holds operational state: sessions, session events, jobs and leases, imports and orphans, integration state, access and application events, consolidation result markers, current handoff items and their sources, and delivery claims.

Markdown writes use staging, filesystem synchronization, and atomic rename, and only become visible after complete structural and referential validation. If Git is available, the memory directory is initialized as a local repository and durable memory changes are committed automatically. Menvane remains functional when Git is unavailable.

Deleting `index.sqlite` does not delete durable knowledge or operational evidence. `menvane reindex` validates all Markdown into a temporary SQLite database and atomically installs the rebuilt derived index without changing `state.sqlite`. Reindex acquires the daemon process lock and refuses to replace an index in active use.

## Memory Model

A session is not a memory type. Each durable session is a chronological sanitized capture of the observed events, created deterministically at finalization before any provider call. The session Markdown is the canonical human artifact of the session and contains an immutable chronological section plus a derived episodic-summary section appended atomically after consolidation. Provider failure leaves the session valid, with the summary pending and a retryable job.

Durable knowledge uses exactly two functional types: `memory` and `playbook`. A `memory` stores non-obvious information that is not canonical in the project and is reusable beyond the current task, including user decisions, constraints, preferences, corrections, and confirmed outcomes. A `playbook` stores a non-trivial procedure with trigger, applicability, ordered steps, validation, and failure handling. Open errors, behavior evident in canonical project sources, and pending work never become durable knowledge. Zero promotion remains valid when a session has no reusable knowledge.

Physical scope is either global or project. Project scope exists only when the working directory belongs to a Git repository; outside Git, writes, sessions, search, and automatic recall use global scope and no project metadata is created. Project search returns the current project plus global memories by default and never includes unrelated projects. Forgotten memories remain in Markdown with `status: forgotten`, are excluded from automatic recall and normal search, and are never silently recreated. Explicit MCP search may include them, and an MCP read can reinforce a memory forgotten by decay.

Every knowledge record has an identifier, type, scope, status, timestamps, source sessions, tags, confidence, observed utility, and optional applicability. Applicability dimensions are languages, frameworks, tools, databases, and platforms; empty dimensions indicate the record is not tied to specific technologies. An inferred playbook starts as a candidate with conservative confidence and becomes active only after a second independent successful application; a memory or a playbook explicitly taught by the user may become active immediately when safe and not duplicating a canonical source. Repeated unsupported, irrelevant, incomplete, outdated, contradicted, or harmful playbook deliveries reduce confidence. A playbook with at least three negative assessments and confidence below 0.45 becomes quarantined and is excluded from normal search and automatic delivery while remaining inspectable with an explicit search that includes inactive knowledge.

## Project Resolution

Menvane identifies a project only when the working directory belongs to a Git repository, in this order:

1. An explicit `project` value in the nearest ancestor `.menvane.toml`.
2. A Git repository identity, preferring a normalized canonical remote and otherwise using the canonical Git common directory.

Without a Git repository, the working directory has no project identity and memory activity is global. A `.menvane.toml` project override does not create a project outside Git.

Equivalent HTTPS, SSH, and SCP-style Git remotes resolve to the same identity. Worktrees sharing a Git common directory resolve to the same project. Known checkout paths are informational and do not define identity when a remote is available.

Each project has a `project.md` containing its stable identity, checkout name, known paths, and detected technology profile. When no usable Git remote exists, the stable identity uses the canonical Git common directory and the displayed project name uses the checkout directory name. Menvane updates this file when paths or technologies change.

## Technology Detection

Technology detection is deterministic and inspects known project files and dependency manifests. Profiles contain languages, frameworks, tools, databases, and platforms.

## Commands

`menvane write` creates a durable memory or playbook. `menvane search` searches current-project and global memory by default. `menvane read` displays a memory. `menvane forget` marks one forgotten. `menvane jobs retry` requeues failed consolidation jobs. `menvane handoff inspect` displays the current project handoff text, its structured items, and their provenance for diagnostics; normal continuation remains automatic. `menvane reindex` reconstructs the derived index from Markdown. `menvane doctor` checks the home, index database, state database, FTS5, Git availability, and Markdown/index consistency independently.

## Sessions And Episodic Summaries

Clients send a normalized vocabulary of session-started, user-prompt, tool-completed, context-compacted, turn-stopped, and session-ended events. Events carry stable event identifiers and explicit provenance and are ingested idempotently. Concurrent delivery uses SQLite WAL and a busy timeout.

Capture removes authentication headers, likely API keys and tokens, bounds prompts and tool inputs and outputs, and drops reliably attributed ignored paths before persistence. Default limits are 16,384 bytes for prompts and 4,096 bytes for tool input and output. Default ignored paths include environment files, secret directories, SSH directories, `AGENTS.md`, `SKILL.md`, and files under `skills` directories, because agent instructions and skill instructions are configuration rather than durable knowledge. Menvane never captures private model reasoning.

The durable session preserves event order, types, timestamps, and stable references, but never private reasoning, secrets, unbounded payloads, or harness instructions. System prompts, skills, `AGENTS.md`, `SKILL.md`, tool metadata, Menvane context, and every injected instruction never enter the durable session, any language-model packet, or any episodic summary. Real user prompts, tool activity, and lifecycle events are distinct categories; system and agent messages are never represented as `UserPrompt` without explicit provenance.

Sessions are open, idle, or finalized. Session end queues deterministic finalization without waiting for background work. Turn stop marks idle, and idle sessions queue finalization after 120 seconds by default. Open sessions whose client omits both terminal events become finalized after 1,800 seconds of inactivity by default, preventing abandoned integrations from withholding consolidation indefinitely. Each finalized session records whether it ended through an explicit session event, idle timeout, or inactivity timeout. Events arriving after finalization reuse the external session identifier in a new generation and process only new evidence. Finalization is asynchronous, idempotent, provider-independent, and recoverable through the daemon worker. Empty or purely operational sessions do not produce consolidation.

Each eligible event is rendered in chronological order with timestamp, type and actor, a stable reference, and bounded sanitized content. Useful tool details such as name, attributed path, success, and sanitized input and output are preserved without inferring decisions or outcomes. Complete payloads remain only in `state.sqlite`; the Markdown is bounded, human-oriented, and reconstructible from the operational evidence.

The episodic summary contains intentions, constraints and corrections, relevant actions and discoveries, outcome, continuity, and candidate learnings. It is written by atomic rewrite that never alters the chronological section. The summary is never injected automatically: it is used to recompose the handoff and can be read explicitly to deepen a work front. Only the metadata and text needed to select summaries later are indexed, as derived data, in `index.sqlite`; the Markdown remains canonical.

## Handoff

There is a single current handoff per project. The handoff is not free text stored as the sole truth: it is rendered deterministically from structured items. Each item represents a still-live work front and has a kind, current state, optional next step, optional blocker, last-confirmed date, and provenance by session and event. The external kinds are `in-progress`, `open-question`, `parked`, and `blocked`.

At each consolidation, every previous item receives an explicit operation: `keep`, `update`, `resolve`, `discard`, `replace`, or `uncertain`. `uncertain` keeps the item with an internal low-confidence indication; it never asserts a conclusion without evidence. Resolved, discarded, or replaced items leave the current handoff; their destination is recorded in the episodic summary of the session that caused the transition, without creating public handoff versions.

The handoff contains no project summary, architecture, executed commands, narrative history, or knowledge unrelated to open work. Repository fingerprint and changed files are auxiliary signals only; they never decide the semantic validity of an item on their own. The same state always renders byte-for-byte identical handoff text.

## Consolidation

There is at most one logical consolidation per session. One structured-output repair attempt is allowed and does not count as a second logical consolidation. The response has four independent results: the episodic summary, operations over handoff items, zero or more knowledge operations, and execution metadata.

The packet contains the current session, the current handoff, a few related episodic summaries, related memories only to prevent duplication or contradiction, and a bounded manifest of historical context actually delivered to that session. It never includes injected instructions, complete diffs, credentials, or private reasoning. Every previous handoff item must have an explicit destination in the response. Every delivered context item receives exactly one evidence-bound utility assessment: useful, unused, irrelevant, incomplete, outdated, contradicted, harmful, or unknown. Useful requires concrete session evidence; unknown is preferred over an unsupported causal claim. Every cited evidence must exist in the packet. Schema, references, scope, targets, and limits are validated before any write, and sources are validated before the result is applied.

Candidate knowledge must pass a promotion barrier: utility beyond the current task, not evident in the repository, not present in a known canonical source, cited session evidence, and a plausible future retrieval scenario. A memory may cite user prompts, decisions, corrections, confirmed outcomes, or tool evidence and never requires a tool event. Tool calls and results are strong signals for identifying a possible playbook, while playbook activation remains governed by independent successful applications.

An absent or blank consolidation prompt uses the built-in prompt. The built-in prompt actively identifies reusable memory, treats tools as playbook signals rather than a universal promotion prerequisite, and requires exactly one operation for each supplied handoff item.

Applying the complete result is idempotent and uses a single transactional marker. Markdown writes use staging and become visible only after complete structural and referential validation. Invalid structured output never alters Markdown, the handoff, or knowledge. Consolidation records provider, model, latency, attempts, input and output bytes and, when available, token usage and credits; private reasoning is never recorded.

## Retrieval And Delivery

Session start inside a resolved project delivers only minimal project identity, the current project handoff, and an indication that additional memory is available. Outside a resolved project it injects nothing; global handoff and memory retrieval wait for the first user prompt so intent can constrain selection. Session-start and prompt-time handoff delivery share the same content claim, so identical rendered handoff content is delivered at most once per session identity. Unchanged content is not redelivered, and new content may be delivered again.

Each new prompt selects the handoff items related to the current intent plus zero to three relevant memories or playbooks, presented as bounded type-specific cards. Three is a ceiling, never a target. Automatic recall removes recognized English and Portuguese stopwords, excludes path fragments from generated recall queries, preserves technical identifiers, requires meaningful lexical coverage, and abstains when intent is insufficient or no candidate passes every configured threshold. The defaults require match confidence of 0.45, knowledge confidence of 0.55, and observed utility of 0.55. Candidate generation may use lexical and embedding retrieval, but neither source bypasses these gates. Semantically redundant candidates are removed before delivery. Handoff selection additionally ignores generic file, configuration, error, path, and system terms and requires two or three meaningful shared terms according to prompt size. Unrelated prompts receive no handoff items. Recall diagnostics report the generated query, candidates considered, abstention reason, handoff scope, coincident terms, required match count, and delivery reason. The hot path never calls a language-model provider.

Memory cards contain a complete bounded historical assertion selected around matching terms, applicability when present, and last-confirmed date. Playbook cards contain a larger bounded procedure preserving its trigger, essential steps, validation, and failure handling. Model-visible cards never expose Menvane branding, internal identifiers, numeric scores, lifecycle machinery, or instructions to call a tool. If the useful content cannot be delivered safely within the bound, the candidate is omitted. Full bodies remain available through explicit reads as an optional MCP capability, but normal automatic operation never depends on such a read.

Automatic recall searches only the current project and global memory. Global universal memories are eligible everywhere. Global memories with populated applicability are eligible only when every populated dimension overlaps the current project's detected technologies. Explicit search retains its lexical behavior and may inspect an otherwise incompatible memory when the query names one of its technologies.

Embedding providers are independent from language-model providers. When an embedding provider is configured and healthy, automatic recall combines FTS5 and embedding rankings without exposing a separate tool or agent choice. Embedding storage is derived and reconstructible. When embeddings are unavailable or incomplete, retrieval falls back transparently to FTS5 and remains fully functional.

The initial embedding provider is `openai-api`, using an explicit model, an OpenAI-compatible `/embeddings` endpoint, and an API key read only from the configured environment variable. It is disabled by default because enabling an external provider sends sanitized recall prompts and durable memory titles and bodies to that endpoint. Embedding configuration includes a conservative cosine-similarity threshold. Enabling or changing the embedding provider requires `menvane reindex` to reconstruct vectors for existing memories and a daemon restart to activate the configuration.

Retrieval, actual delivery, and autonomous post-session utility assessment are recorded as separate signals, alongside MCP reads and successful or failed applications. Delivery records preserve the exact bounded card or handoff item sent, its stable subject, session generation, evaluation, reason, and evidence; the session REST detail and web UI expose this audit trail. Only MCP reads and actual agent injection reinforce a `memory` against temporal decay; utility affects a separate relevance lifecycle. Useful assessments raise confidence and observed utility. Unused context reduces utility gradually; irrelevant selection primarily reduces utility rather than asserting that the content is false; incomplete, outdated, contradicted, and harmful assessments apply progressively stronger reductions to both utility and confidence. Unknown has no lifecycle effect. Search results, CLI reads, REST reads, web UI views, and repeated UI rendering do not reinforce memories or playbooks. A successful playbook application is the strongest positive verification, and a second independent success activates a candidate playbook. The normal feedback loop never requires an agent to call an MCP tool, cite a memory identifier, or know that Menvane exists.

## MCP

`menvane mcp` serves MCP over newline-delimited JSON-RPC on standard input and output. It resolves the active project from its process working directory and exposes exactly `memory_search`, `memory_read`, `memory_write`, and `memory_forget`, accepting the memory and playbook types. `memory_search` can include forgotten memories when explicitly requested, and `memory_read` reinforces a memory but does not alter playbook lifecycle. MCP enforces a 4,096-byte UTF-8 query bound, a 50-item search limit and result bound, 512-character search excerpts, and a 32,768-byte serialized response bound. Unsafe search values are capped deterministically.

MCP search returns identifiers, type, scope, title, score, status, applicability, and a bounded short excerpt. Read returns bounded metadata and provenance plus a UTF-8-safe progressive Markdown range. Read ranges use character units by default, also support byte units, default to 4,096 units, and cap each request at 8,192 units. Range metadata reports the effective offset, returned units, total units, and whether more content exists, so large memories can be reconstructed across calls without an unbounded response. Forgetting changes status without deleting Markdown. Automatic manual writes conservatively use project scope.

## Daemon And REST

`menvane serve` runs the Axum daemon on `127.0.0.1:47831` by default. A per-home process lock prevents duplicate daemons. `menvane daemon start`, `stop`, `restart`, and `status` manage the background process.

The REST foundation is under `/api/v1`. Health, normalized event ingestion, and job inspection are available. Capture, consolidation, and finalization share the same engine and stores used by CLI and MCP. SQLite jobs use pending, running, completed, and failed lifecycle states with attempts, retry time, error fields, an owner, and a configurable 300-second lease timeout by default. The daemon worker claims finalization and consolidation jobs, recovers expired leases after restart, and retries all paths idempotently. Eligible jobs are ordered by their next-attempt time and then creation time, so a session in backoff cannot preempt untouched sessions or block the queue. Provider availability, authentication, rate, network, and capability failures remain pending with bounded exponential backoff until the provider recovers; invalid input, invalid schemas, and internal failures become failed after the normal retry limit. `menvane jobs retry` explicitly requeues failed consolidation jobs. Graceful shutdown flushes dirty state when feasible, and capture does not wait for background work.

REST covers sessions with their episodic summaries, the current handoff with items and provenance, recall, and memory and playbook knowledge. Removed legacy selectors and endpoints return explicit absence, not partial behavior. Session reads by ID return the chronological capture and, when present, the episodic summary.

## Claude Code Integration

`menvane connect claude` installs a user-scoped Menvane MCP server and command hooks for session start, user prompt submission, completed tools, pre-compaction, stop, and session end. It uses the strongest supported Claude lifecycle events, preserves unrelated configuration, creates timestamped backups before changes, and is idempotent. `menvane disconnect claude` removes only entries whose command and MCP definition are owned by the current Menvane executable. Menvane does not create or modify `CLAUDE.md` or skills.

Claude hooks normalize client payloads before domain ingestion and ensure the daemon is running. Hooks originating from `MENVANE_INTERNAL=1` are ignored. Reliably attributed ignored paths are dropped and all capture is sanitized before local daemon transport.

Session start injects only minimal project identity and individually claimed current handoff items when the working directory resolves to a project, at most once per session identity and item content. Empty handoff content is never claimed as a delivery. Global session start injects nothing. User prompts receive only handoff items that meet the meaningful-overlap threshold plus up to three memory or playbook cards. The model-visible envelope describes historical context without naming Menvane or requiring tool awareness. Full bodies are never injected automatically. Recall prompts are sanitized and bounded before search; oversized client, session, and working-directory identifiers are rejected by the daemon. No external language-model request occurs on this path.

Injected memory is delimited as historical context and explicitly states that current user instructions and repository state are authoritative. Hook capture and recall require no memory instruction from the user.

## Language Model Providers

Language-model generation is accessed only through the provider-independent `LlmProvider` boundary. Consolidation requires structured output and JSON Schema capability. Provider failures distinguish unavailable service, authentication, rate or usage limits, network errors, unsupported capabilities, invalid application input, invalid schemas, and internal failures.

The default provider is `openai`. It uses Menvane's native OpenAI OAuth Authorization Code flow with PKCE to access ChatGPT Plus or Pro through the Codex Responses endpoint. Browser authorization uses the OpenAI issuer, a loopback callback on port 1455, state validation, and the `openid profile email offline_access` scopes. The default model is `gpt-5.6-luna` with medium reasoning effort.

`menvane provider login openai` opens the system browser and waits up to five minutes for authorization. Menvane stores the resulting access token, refresh token, expiration, and optional ChatGPT account identifier in `~/.menvane/oauth/openai.json` with owner-only permissions on Unix. It refreshes expired access tokens automatically and atomically replaces the credential file. `menvane provider logout openai` removes Menvane's OpenAI credentials. Menvane never reads or modifies OpenCode or Codex credentials.

`menvane provider configure openai --model <model>` selects the OAuth-backed model. `--reasoning-effort` selects `minimal`, `low`, `medium`, `high`, or `xhigh` and defaults to `medium`. The daemon must be restarted after configuration changes.

The `github-copilot` provider requires a GitHub OAuth app with device flow enabled and a GitHub account with Copilot access. It uses GitHub's OAuth device authorization flow and the GitHub Copilot Chat Completions endpoint. `menvane provider configure github-copilot --model <model> --client-id <client-id>` stores the non-secret OAuth client ID and selects the provider; the daemon must be restarted after configuration changes. `menvane provider login github-copilot` displays GitHub's verification URL and user code, polls until authorization completes, verifies the GitHub identity, and stores Menvane-owned credentials at `~/.menvane/oauth/github-copilot.json` with owner-only permissions on Unix. Access tokens are refreshed when possible, and `menvane provider logout github-copilot` removes the stored credentials. Menvane never reads GitHub CLI or Copilot CLI credentials.

The optional `codex` compatibility provider invokes the installed Codex CLI and uses existing local Codex authentication without reading or persisting credentials. Internal calls set `MENVANE_INTERNAL=1`, execute in an ephemeral temporary directory with a read-only sandbox, ignore user and project configuration, disable available tools and hooks, supply all evidence directly, and delete schema and response files afterward. Health distinguishes missing binary, missing authentication, unavailable explicit model, and ready state.

The `openai` provider uses the ChatGPT Codex Responses endpoint and JSON Schema structured output. The `openai-api` compatibility provider and `openrouter` use OpenAI-compatible chat completions. Their models must be explicit. Configured reasoning effort is included in structured inference requests. API keys for API-based providers are read only from configured environment variables and are never written to Markdown, SQLite, configuration values, logs, Git, UI, or responses. OpenRouter defaults to `OPENROUTER_API_KEY` and its standard API endpoint when selected.

An explicit fallback provider may be configured under `[llm.fallback]`. Fallback applies only to provider availability, authentication, usage limits, network errors, and unsupported capabilities. It does not hide invalid Menvane input, invalid schemas, or internal defects.

`menvane provider status` performs only local and configuration health checks and does not make paid inference requests. `menvane provider test` performs one minimal structured request and validates the response. Doctor includes provider compatibility and health.

## Knowledge Operations

Valid durable knowledge operations are create, reinforce, merge, supersede, and no-op, for memories and playbooks. Equivalent content reinforces source evidence; complementary targets merge; contradictions supersede eligible targets; and no-op output is valid. Operation application is transactional at the operation marker and idempotent across retries, without using equal titles as the primary identity test. Provider unavailability does not affect capture, session Markdown, manual memory operations, search, MCP, project detection, or technology detection; consolidation jobs remain durable and retryable without partial results.

Playbook content contains trigger, applicability, ordered steps, validation, and failure handling. A candidate playbook becomes active only after a second independent successful application; duplicate delivery of the same session signal is idempotent, and a failed application remains negative evidence without deleting or automatically replacing the playbook. Global classification requires high scope confidence; uncertainty resolves to project scope.

## Codex Agent Integration

`menvane connect codex` merges a user-level MCP server and supported lifecycle hooks into `CODEX_HOME/config.toml`, defaulting to `~/.codex/config.toml`. It preserves unrelated models, servers, hooks, and settings, creates a backup before changes, enables supported hooks, and is idempotent. Only hook events that can return model-visible additional context receive an `additionalContextLimit`; reconnect removes that setting from owned handlers for incompatible events. Disconnect removes only the matching Menvane MCP and hook commands. It never modifies `AGENTS.md`.

Codex session start, user prompt, completed tool, pre- and post-compaction, stop, and session end payloads normalize into the shared event vocabulary. Capture is sanitized before daemon transport. Project-scoped session start and user prompt hooks use the same bounded automatic recall, relevance, deduplication, and trust boundary as Claude Code; global session start injects nothing. `MENVANE_INTERNAL=1` prevents provider inference from recursively creating Codex agent sessions.

## OpenCode Integration

`menvane connect opencode` preserves and extends the user OpenCode JSON configuration, registers the local Menvane MCP server, and installs one owned vanilla JavaScript plugin under the OpenCode configuration directory. The installer creates backups and is idempotent. Disconnect removes only the matching Menvane plugin URI, MCP entry, and unchanged owned plugin file.

The plugin only forwards session, message, compaction, and completed-tool activity to `menvane hook opencode`, appends returned session-start and prompt context before model dispatch, and contains no ranking, applicability, consolidation, or memory-domain logic. OpenCode payloads normalize into the same domain vocabulary and use the same daemon capture, retrieval, sanitization, trust boundary, and identity-aware delivery as Claude Code and Codex.

## Google Antigravity Integration

`menvane connect antigravity` installs a user-scoped Menvane MCP server in the Antigravity MCP configuration and command hooks for PreInvocation, PostToolUse, and Stop lifecycle events in the Antigravity hooks configuration. The default configuration directory is `~/.gemini/config`, overridable by `ANTIGRAVITY_CONFIG_DIR`. It preserves unrelated configuration, creates timestamped backups before changes, and is idempotent. `menvane disconnect antigravity` removes only entries whose command and MCP definition are owned by the current Menvane executable. Menvane does not create or modify Antigravity skills, rules, or `AGENTS.md`.

Antigravity hooks normalize client payloads before domain ingestion and ensure the daemon is running. Hooks originating from `MENVANE_INTERNAL=1` are ignored. Reliably attributed ignored paths are dropped and all capture is sanitized before local daemon transport. The hook reads the conversation ID from `conversationId` or `conversation_id`, workspace paths from `workspacePaths`, user messages from `userMessage`, tool calls from `toolCall`, and model name from `modelName`. When the user prompt is not directly available in the payload, the hook reads the last `USER_INPUT` entry from the transcript file referenced in `transcriptPath`.

PreInvocation hooks inject relevant recalled context as ephemeral messages using the `injectSteps` response format. PostToolUse hooks capture tool activity without injection. Stop hooks return an allow decision. Project-scoped hooks use the same bounded automatic recall, relevance, deduplication, and trust boundary as Claude Code, Codex, and OpenCode; global session start injects nothing.

`menvane import antigravity` recursively discovers JSONL transcript files under `~/.gemini/antigravity-cli/brain`, `~/.gemini/antigravity/brain`, and `~/.gemini/antigravity-ide/brain`. Readers stream line by line with a ten-megabyte record bound. Antigravity transcript records of type `USER_INPUT` become user prompts and records of type `PLANNER_RESPONSE` with tool calls become tool-completed events; other record types are skipped. Import uses the same idempotent session pipeline as other clients.

## Maintenance

Memories have configurable temporal decay with a default lifetime of 90 days; playbooks have no temporal decay. The score uses exponential time decay normalized to zero at the configured lifetime plus a logarithmic reinforcement bonus whose effect also decays over time. Only MCP reads and actual agent injection count as reinforcement. At score zero, an active memory becomes `forgotten`, leaves automatic recall and injection, and remains inspectable through explicit MCP access. An MCP read can revive a memory forgotten by decay when the resulting score becomes positive. A manually forgotten memory is not revived implicitly.

Menvane records retrieved, injected, MCP-read, successfully applied, and failed application as separate signals. Retrieval alone does not reinforce knowledge. Successful playbook application is the strongest positive verification and failed application remains negative evidence. UI views are observational and never create access or application signals.

## Historical Import

`menvane import claude`, `menvane import codex`, and `menvane import antigravity` recursively discover supported JSONL session files under configured client homes. Readers stream line by line, enforce a configurable record bound, skip and count malformed records, ignore unknown event types, and retain only useful user and tool evidence. Codex checks both active and archived session directories and pairs supported tool-call records with their output records into one normalized tool-completed event while excluding assistant reasoning and messages. Antigravity readers parse transcript JSONL, mapping `USER_INPUT` records to user prompts and `PLANNER_RESPONSE` records with tool calls to tool-completed events. `menvane import opencode` uses the configured local OpenCode HTTP API rather than scraping private storage. An optional positional window such as `7d` imports only sessions with activity in the last seven days; only day-based windows are supported.

All importers produce client-independent normalized sessions and pass them through the normal session pipeline; they never create consolidated knowledge directly. External formats are treated as versioned best-effort input. Reimport uses client plus external session identifier and is idempotent: importing the same session twice never duplicates the session, its summary, handoff items, or knowledge.

`--dry-run` reports discovered sessions, invalid records, and estimated bytes without persistence. A session without reliable existing project path is stored as an orphan in operational SQLite and is never guessed into a project. Orphan payload remains available for later administrative association and consolidation.

## Web Interface

The daemon serves a responsive, server-rendered HTML interface with no CDN, React, or client framework. The dashboard prioritizes projects and operational health, followed by durable knowledge. Dedicated views cover projects, memories, sessions, imports, integrations, providers, and friendly non-secret settings. Project detail shows the current handoff rendered from its items, with provenance and a fingerprint-stale warning when applicable. Session detail shows the episodic summary and the chronological evidence. Memory detail shows memory or playbook content, lifecycle, applicability, source sessions, recall signals, and a descriptive visual decay state for memories. The UI presents Fresh, Aging, Fading, or Forgotten with a progress bar and estimated time remaining rather than only a numeric score. Viewing any UI page is observational and does not reinforce memories or playbooks. Diagnostics show consolidation cost and quality. The web interface does not turn the handoff into a backlog, does not edit durable memories, and does not expose raw Markdown; durable changes use the CLI or the consolidation engine.

Recall is performed by connected agents and injected into their context; it has no web menu or interactive web search view. The visual interface is fully local and uses embedded assets with minimal JavaScript only for progressive page arrival.

REST endpoints under `/api/v1` cover health, projects, memories, sessions, imports, integrations, settings, jobs, providers, normalized events, recall, and the current handoff. HTTP handlers delegate to the same engine used by CLI, MCP, hooks, and UI.

## Backup, Restore, And Distribution

The repository `install.sh` installs the latest compatible published binary after verifying its SHA-256 checksum, accepts a specific release through `--version`, accepts a prebuilt executable through `--binary`, and fails if a published binary cannot be downloaded. It can be run from a checkout or piped directly from the repository's raw GitHub URL. A requested release that cannot be downloaded fails without selecting a different version. It installs the executable at `~/.local/bin/menvane`. On Linux it also installs a user-scoped `menvane.service`, enables it for the user's default target, and requests an immediate non-blocking start. The service runs independently of the system boot critical path, restarts after failures, and serves both the daemon API and local UI. On macOS it installs and loads a user-scoped LaunchAgent that starts at login and restarts after failures. Reinstalling updates the executable and startup definition idempotently. On other supported platforms, the script installs the executable without configuring automatic startup. Release versions are selected manually by updating the Cargo workspace version and pushing the matching `vX.Y.Z` tag after CI passes. A release tag runs repository formatting, Clippy, tests, the installer test, and the Linux and macOS binary builds. The workflow rejects a tag that differs from the Cargo version and creates the GitHub release with all binary assets plus `SHA256SUMS` only after every validation and build succeeds; it does not create tags or modify Cargo versions. GitHub-generated release notes consolidate pull requests into Features, Fixes, and Other Changes using the repository release-note categories.

`menvane backup <path>` creates a new backup directory containing the complete Markdown memory repository, non-secret configuration, consistent SQLite online backups of both `index.sqlite` and `state.sqlite`, and a checksummed manifest. Existing destinations are never overwritten. `menvane restore <path> --confirm` verifies every checksum, configuration, Markdown frontmatter, and both SQLite databases independently before staging and replacing current state. Restore refuses to run while a daemon PID is present and never replaces state without explicit confirmation. Backup and restore are capabilities independent of the product model and follow the current schema.

Daemon startup uses one process lock per Menvane home, graceful shutdown, idle-session recovery, WAL, bounded waits, leased job ownership, and idempotent event and job keys. Atomic Markdown writes and derived-index reindex permit reconciliation after interrupted index updates without removing operational state. Git durable-history writes are serialized independently from concurrent capture.

Release builds target Linux, macOS, and WSL as Linux. The repository CI runs formatting, Clippy, all tests, and release builds without real Codex authentication, OpenRouter credentials, or paid APIs. Runtime provider status may use local non-paid health interfaces; deterministic fake providers and mock servers cover CI behavior.

Menvane is operationally complete when a clean installation captures sessions without a provider, each meaningful session can produce an episodic summary with one logical consolidation, the current handoff preserves only live work fronts across sessions, concluded and discarded items leave correctly, unadopted suggestions never become pending work, no general project summary is ever generated or injected, durable knowledge holds only non-obvious reusable memories and playbooks, retrieval stays local, small, and intent-oriented, reindex rebuilds all derived state without altering operational evidence, and unrelated project memory remains isolated.
