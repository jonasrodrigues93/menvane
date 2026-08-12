# Menvane

Version: 1.24.0

Menvane is a local persistent memory system for agents. In its current version, it provides a durable command-line memory foundation that stores human-readable Markdown as the source of truth and uses SQLite with FTS5 as a rebuildable search index. Each durable session is a chronological sanitized capture of the observed events, and semantic interpretation happens separately through one language-model consolidation per session that may identify goals and produce zero or more memory operations. The same processing maintains a single short, replaceable handoff per project.

## Storage

Menvane stores data under `~/.menvane` by default. `MENVANE_HOME` overrides that location.

The home contains `config.toml`, `index.sqlite`, `state.sqlite`, operational directories, global memory directories, and project memory directories. Durable memories and project metadata are Markdown with YAML frontmatter. `index.sqlite` contains only derived project and memory metadata, FTS5 search data, and embeddings. `state.sqlite` contains operational sessions, events, observations, jobs, access events, procedure applications, imports, orphans, integration state, and injection claims.

Markdown writes use a temporary file, filesystem synchronization, and atomic rename before the derived index is updated. If Git is available, the memory directory is initialized as a local repository and durable memory changes are committed automatically. Menvane remains functional when Git is unavailable.

Deleting `index.sqlite` does not delete durable knowledge or operational evidence. `menvane reindex` validates all Markdown into a temporary SQLite database and atomically installs the rebuilt derived index without changing `state.sqlite`. Reindex acquires the daemon process lock and refuses to replace an index in active use. Existing homes migrate operational tables from the legacy `index.sqlite` to `state.sqlite` with durable, resumable table markers; legacy tables remain until the derived index is safely rebuilt.

## Memory Model

Durable knowledge uses exactly five memory types: fact, decision, procedure, gotcha, and session. The current manual write command creates facts, decisions, procedures, and gotchas. Sessions are durable chronological sanitized captures of the observed events, created deterministically at finalization; raw captured events remain operational evidence and are not durable memories on their own. Semantic interpretation happens afterward in one language-model consolidation per session, which may identify Goals and produce zero or more memory operations, and which maintains the single current project handoff. A session is created deterministically at finalization regardless of provider availability; only the derived memories, Goals, and handoff depend on a valid language-model result.

Physical scope is either global or project. Project scope exists only when the working directory belongs to a Git repository; outside Git, writes, sessions, search, and automatic recall use global scope and no project metadata is created. Project search returns the current project plus global memories by default and never includes unrelated projects. Forgotten memories remain in Markdown with `status: forgotten` and are excluded from normal search.

Every memory has an identifier, type, scope, status, confidence, timestamps, source sessions, tags, optional applicability, and supersession metadata. Applicability dimensions are languages, frameworks, tools, databases, and platforms; empty dimensions indicate the memory is not tied to specific technologies.

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

`menvane write` creates a durable memory. `menvane search` searches current-project and global memory by default. `menvane read` displays a memory. `menvane forget` marks one forgotten. `menvane handoff inspect` displays the single current project handoff summary and its provenance for diagnostics; normal continuation remains automatic. `menvane reindex` reconstructs the derived index from Markdown. `menvane doctor` checks the home, index database, state database, FTS5, Git availability, and Markdown/index consistency independently.

## Retrieval

Retrieval uses SQLite FTS5 and Reciprocal Rank Fusion with `K = 60`. Explicit search retains its existing lexical behavior. Automatic prompt recall identifies the latest operational session by client and external session identifier, then retrieves the sanitized current prompt and the project's consolidated Goals independently. It fuses those rankings by memory ID with weights `1.00` and `0.85`, then applies bounded lifecycle, type, confidence, freshness, applicability, and scope multipliers. Current-project variants rank above equivalent global variants and title/type deduplication occurs after ranking. Goals come only from validated consolidation results; no deterministic lexical queries are generated from root goals, constraints, corrections, or dormant episodes. The recall hot path never calls a language-model provider. Procedures and decisions receive a 1.15 type multiplier, gotchas 1.10, facts 1.00, and sessions 0.75. Active, candidate, needs-validation, superseded, and historical statuses receive their defined lifecycle multipliers. Sessions are excluded unless explicitly requested.

Automatic recall searches only the current project and global memory. Global universal memories are eligible everywhere. Global contextual memories are eligible only when every populated applicability dimension overlaps the current project's detected technologies. Explicit searches may inspect an otherwise incompatible contextual memory when the query names one of its technologies.

Embedding providers are independent from language-model providers. Embedding storage is derived and reconstructible; retrieval remains fully functional with FTS5 when no embedding provider is configured.

## MCP

`menvane mcp` serves MCP over newline-delimited JSON-RPC on standard input and output. It resolves the active project from its process working directory and exposes exactly `memory_search`, `memory_read`, `memory_write`, and `memory_forget`. MCP enforces a 4,096-byte UTF-8 query bound, a 50-item search limit and result bound, 512-character search excerpts, and a 32,768-byte serialized response bound. Unsafe search values are capped deterministically.

MCP search returns identifiers, type, scope, title, score, status, confidence, applicability, and a bounded short excerpt. Read returns bounded metadata and provenance plus a UTF-8-safe progressive Markdown range. Read ranges use character units by default, also support byte units, default to 4,096 units, and cap each request at 8,192 units. Range metadata reports the effective offset, returned units, total units, and whether more content exists, so large memories can be reconstructed across calls without an unbounded response. Forgetting changes status without deleting Markdown. Automatic manual writes conservatively use project scope when no compiler is available.

## Capture And Sessions

Clients send a normalized vocabulary of session-started, user-prompt, tool-completed, context-compacted, turn-stopped, and session-ended events. Events carry stable event identifiers and explicit provenance and are ingested idempotently. Concurrent delivery uses SQLite WAL and a busy timeout.

Capture removes authentication headers, likely API keys and tokens, bounds prompts and tool inputs and outputs, and drops reliably attributed ignored paths before persistence. Default limits are 16,384 bytes for prompts and 4,096 bytes for tool input and output. Default ignored paths include environment files, secret directories, SSH directories, `AGENTS.md`, `SKILL.md`, and files under `skills` directories, because agent instructions and skill instructions are configuration rather than durable knowledge. Menvane never captures private model reasoning.

The durable session preserves event order, types, timestamps, and stable references, but never private reasoning, secrets, unbounded payloads, or harness instructions. System prompts, skills, `AGENTS.md`, `SKILL.md`, tool metadata, Menvane context, and every injected instruction never enter the durable session or any language-model packet. Real user prompts, tool activity, and lifecycle events are distinct categories; system and agent messages are never represented as `UserPrompt` without explicit provenance.

Sessions are open, idle, or finalized. Session end queues deterministic finalization without waiting for background work. Turn stop marks idle, and idle sessions queue finalization after 120 seconds by default. Events arriving after finalization reuse the external session identifier in a new generation and process only new evidence.

The session is created deterministically at finalization and its Markdown is a chronological capture, not an interpretation. The session builds one idempotent consolidation job; no task-episode or intent classification happens during capture, and no provider call is needed to finalize a session. Only a valid language-model consolidation result may identify or alter Goals, write memories, or replace the project handoff; provider failure leaves the session and the retryable consolidation job intact without partial results. Goals are identified only by the consolidator, never by lexical heuristics at ingestion time.

Each eligible event is rendered in chronological order with timestamp, type and actor, a stable reference, and bounded sanitized content. Useful tool details such as name, attributed path, success, and sanitized input and output are preserved without inferring decisions or outcomes. Complete payloads remain only in `state.sqlite`; the Markdown is bounded, human-oriented, and reconstructible from the operational evidence. Finalization is asynchronous, idempotent, and recoverable through the daemon worker.

The consolidation packet is built from the filtered chronological session, the project's current Goals, related memories, and the current handoff. It never includes injected instructions, complete diffs, credentials, or private reasoning. The consolidator returns one structured response with three independent results: goal operations, zero or more memory operations, and an optional replacement of the current handoff. Goal and memory operations must reference `event_id` values present in the packet, and schema, references, scope, targets, and limits are validated before any write. All operations are applied idempotently; a retry after a failure never duplicates Goals, memories, or the handoff.

An automatic handoff is one short, replaceable summary per project. It contains only recently relevant facts and decisions or work still pending from the recent sessions. Each consolidation replaces the previous summary; there are no versions, no cards, no consumed or superseded states, and no narrative or version history. The consolidator receives the previous handoff and a limited set of the most recent relevant sessions and is asked to return only recent relevant facts, pending decisions, pending work, blockers, and a next action when one exists; event lists, executed commands, and complete history are prohibited. Provider output for the handoff field is limited to at most 2,000 tokens when the API supports it, and is validated locally with a model-compatible tokenizer or a conservative byte fallback; a response above the limit is rejected and retried once and is never persisted or injected. Provider failure preserves the last valid handoff. Delivery injects at most the single current handoff of the project and is deduplicated by project, client, conversation, generation, and the identifier of the current content. The fingerprint is maintained only as metadata to detect a possibly stale summary without creating versions or enlarging the injected text. No injected content ever returns to the handoff.

## Daemon And REST

`menvane serve` runs the Axum daemon on `127.0.0.1:47831` by default. A per-home process lock prevents duplicate daemons. `menvane daemon start`, `stop`, `restart`, and `status` manage the background process.

The REST foundation is under `/api/v1`. Health, normalized event ingestion, and job inspection are available. Capture, consolidation, and finalization share the same engine and stores used by CLI and MCP. SQLite jobs use pending, running, completed, and failed lifecycle states with attempts, retry time, error fields, an owner, and a configurable 300-second lease timeout by default. The daemon worker claims finalization and consolidation jobs, recovers expired leases after restart, and retries all paths idempotently. Graceful shutdown flushes dirty checkpoint state when feasible, and capture does not wait for background work.

Handoff REST is under `/api/v1/handoffs`. It returns the single current handoff per project with its summary, latest update, and source sessions, without versions, cards, status buckets, or lifecycle actions; consume, complete, and supersede endpoints are removed. Invalid selectors and limits return 400, while a missing handoff returns 404. Session reads by ID return the chronological capture.

## Claude Code Integration

`menvane connect claude` installs a user-scoped Menvane MCP server and command hooks for session start, user prompt submission, completed tools, pre-compaction, stop, and session end. It uses the strongest supported Claude lifecycle events, preserves unrelated configuration, creates timestamped backups before changes, and is idempotent. `menvane disconnect claude` removes only entries whose command and MCP definition are owned by the current Menvane executable. Menvane does not create or modify `CLAUDE.md` or skills.

Claude hooks normalize client payloads before domain ingestion and ensure the daemon is running. Hooks originating from `MENVANE_INTERNAL=1` are ignored. Reliably attributed ignored paths are dropped and all capture is sanitized before local daemon transport.

Session start injects a separate briefing of project identity, detected technologies, active decisions, critical gotchas, and high-confidence applicable global facts within 2,500 characters, at most once per delivery identity even when it has no memory entries. User prompts use the intent-aware lexical recall path and a 6,000-character three-tier payload: required active corrections, constraints, and critical gotchas start with 2,000 characters; relevant bounded excerpts start with 3,000; and retrieval cards start with 1,000. Unused capacity flows from earlier tiers to later tiers, and secondary entries are omitted when exhausted. Every automatic memory entry and card includes its ID, type, scope, status, confidence, age, bounded provenance indicator with source-session and supersession counts, and relevance reason; full bodies are never injected automatically. Recall diagnostics expose every intent query's rank and contribution plus every reranking multiplier. Retrieval is recorded when selected, while injection is recorded only after an entry is included. Recall prompts are sanitized and bounded before search; oversized client, session, and working-directory identifiers are rejected by the daemon. No external language-model request occurs on this path, and identity-aware claims deliver each memory at most once per client, conversation, generation, episode, and memory identity.

Injected memory is delimited as historical context and explicitly states that current user instructions and repository state are authoritative. Hook capture and recall require no memory instruction from the user.

## Language Model Providers

Language-model generation is accessed only through the provider-independent `LlmProvider` boundary. Compilation requires structured output and JSON Schema capability. Provider failures distinguish unavailable service, authentication, rate or usage limits, network errors, unsupported capabilities, invalid application input, invalid schemas, and internal failures.

The default provider is `openai`. It uses Menvane's native OpenAI OAuth Authorization Code flow with PKCE to access ChatGPT Plus or Pro through the Codex Responses endpoint. Browser authorization uses the OpenAI issuer, a loopback callback on port 1455, state validation, and the `openid profile email offline_access` scopes. The default model is `gpt-5.6-luna` with medium reasoning effort.

`menvane provider login openai` opens the system browser and waits up to five minutes for authorization. Menvane stores the resulting access token, refresh token, expiration, and optional ChatGPT account identifier in `~/.menvane/oauth/openai.json` with owner-only permissions on Unix. It refreshes expired access tokens automatically and atomically replaces the credential file. `menvane provider logout openai` removes Menvane's OpenAI credentials. Menvane never reads or modifies OpenCode or Codex credentials.

`menvane provider configure openai --model <model>` selects the OAuth-backed model. `--reasoning-effort` selects `minimal`, `low`, `medium`, `high`, or `xhigh` and defaults to `medium`. The daemon must be restarted after configuration changes.

The optional `codex` compatibility provider invokes the installed Codex CLI and uses existing local Codex authentication without reading or persisting credentials. Internal calls set `MENVANE_INTERNAL=1`, execute in an ephemeral temporary directory with a read-only sandbox, ignore user and project configuration, disable available tools and hooks, supply all evidence directly, and delete schema and response files afterward. Health distinguishes missing binary, missing authentication, unavailable explicit model, and ready state.

The `openai` provider uses the ChatGPT Codex Responses endpoint and JSON Schema structured output. The `openai-api` compatibility provider and `openrouter` use OpenAI-compatible chat completions. Their models must be explicit. Configured reasoning effort is included in structured inference requests. API keys for API-based providers are read only from configured environment variables and are never written to Markdown, SQLite, configuration values, logs, Git, UI, or responses. OpenRouter defaults to `OPENROUTER_API_KEY` and its standard API endpoint when selected.

An explicit fallback provider may be configured under `[llm.fallback]`. Fallback applies only to provider availability, authentication, usage limits, network errors, and unsupported capabilities. It does not hide invalid Menvane input, invalid schemas, or internal defects.

`menvane provider status` performs only local and configuration health checks and does not make paid inference requests. `menvane provider test` performs one minimal structured request and validates the response. Doctor includes provider compatibility and health.

## Memory Compilation

The language-model consolidation treats each finalized session as one unit. It receives the filtered chronological session, the project's current Goals, bounded related memories from the current project and global scope, the project technology profile, and the single current handoff. Related memories include active, candidate, needs-validation, superseded, historical, and forgotten records when relevant, with bounded bodies and provenance summaries; session memories are excluded. The packet preserves resolvable event identifiers and limits only to session content that survived capture filtering. Related-memory input is independently bounded and the packet is limited to the session's bounded captured evidence. The consolidator requests one structured response containing independent Goal operations, memory operations for create, reinforce, merge, supersede, and no-op, and an optional replacement of the current handoff, and validates source events, targets, contradictions, forgotten-memory policy, and conservative global scope. Invalid structured output receives one bounded repair retry and then fails, and the consolidator never allows a provider to write Markdown or the handoff directly.

Consolidation output may contain zero goals, zero memory operations, and no handoff change. Valid durable memory operations use only facts, decisions, procedures, and gotchas. Applicability is optional; empty dimensions indicate the memory is not tied to specific technologies. Procedure content contains trigger, preconditions, ordered steps, decision points, validation, failure handling, and expected outcome. Global classification requires high scope confidence; uncertainty resolves to project scope.

Every durable consolidation change is applied through its validated operation. Equivalent content reinforces confidence and source evidence; complementary targets merge while retaining historical records; contradictions supersede eligible targets; and no-op output is valid. Forgotten memories are never silently recreated. Operation application is transactional at the operation marker and idempotent across retries, without using equal titles as the primary identity test. Goals are created, continued, completed, or abandoned only by validated consolidation output, and each Goal carries references to the events and sessions cited by the result. Provider unavailability does not affect capture, session Markdown, manual memory operations, search, MCP, project detection, or technology detection; consolidation jobs remain durable and retryable without partial results.

## Procedure Learning And Promotion

A first successful procedure is a candidate with one success and no failures. An independently recorded successful application increments successes, updates verification time, and activates the procedure at two successes. Duplicate delivery of the same session signal is idempotent. A failed application increments failures but does not delete or automatically replace the procedure, which remains available for later compiler evaluation.

Global promotion evaluates equivalent project procedures and gotchas. Execution-derived knowledge remains project-scoped until equivalent evidence exists in at least two independent projects. Promotion creates one active contextual global memory retaining source project identifiers, source sessions, successes, failures, confidence, and applicability. Source variants remain inspectable as historical evidence. Retrieval deduplicates project and global variants.

Project-specific decisions are not candidates for global promotion. Explicit universal preferences may still be classified as global facts directly by the compiler.

## Codex Agent Integration

`menvane connect codex` merges a user-level MCP server and supported lifecycle hooks into `CODEX_HOME/config.toml`, defaulting to `~/.codex/config.toml`. It preserves unrelated models, servers, hooks, and settings, creates a backup before changes, enables supported hooks, and is idempotent. Disconnect removes only the matching Menvane MCP and hook commands. It never modifies `AGENTS.md`.

Codex session start, user prompt, completed tool, pre- and post-compaction, stop, and session end payloads normalize into the shared event vocabulary. Capture is sanitized before daemon transport. Session start and user prompt hooks use the same bounded automatic recall and trust boundary as Claude Code. `MENVANE_INTERNAL=1` prevents provider inference from recursively creating Codex agent sessions.

## OpenCode Integration

`menvane connect opencode` preserves and extends the user OpenCode JSON configuration, registers the local Menvane MCP server, and installs one owned vanilla JavaScript plugin under the OpenCode configuration directory. The installer creates backups and is idempotent. Disconnect removes only the matching Menvane plugin URI, MCP entry, and unchanged owned plugin file.

The plugin only forwards session, message, compaction, and completed-tool activity to `menvane hook opencode`, appends returned session-start and prompt context before model dispatch, and contains no ranking, applicability, consolidation, compiler, or memory-domain logic. OpenCode payloads normalize into the same domain vocabulary and use the same daemon capture, retrieval, sanitization, trust boundary, and identity-aware delivery as Claude Code and Codex.

## Decay And Maintenance

Sessions use a 45-day time half-life plus meaningful-access reinforcement with a 60-day half-life. `menvane gc` archives a session only when it is older than 90 days and retention is below 0.15. Archived Markdown moves to `memory/archive/sessions`, remains durable, and is excluded from normal retrieval. Hard deletion is disabled.

Facts and gotchas never disappear due to age and have a freshness floor of 0.50 with a 180-day half-life. Procedures never disappear due to age and have a 0.65 floor with a 365-day half-life. Decisions have no temporal decay and rank only by lifecycle status. Superseded and historical memories remain inspectable at lower rank.

Menvane records retrieved, injected, explicitly read, successfully applied, and failed application as separate signals. Retrieval and injection do not validate memory. Explicit reads and successful applications are meaningful access; successful procedure application is the strongest positive verification and failed application remains negative evidence. This prevents popularity loops from simple repeated surfacing.

## Historical Import

`menvane import claude` and `menvane import codex` recursively discover supported JSONL session files under configured client homes. Readers stream line by line, enforce a one-megabyte record bound, skip and count malformed records, ignore unknown event types, and retain only useful user and tool evidence. Codex checks both active and archived session directories. `menvane import opencode` uses the configured local OpenCode HTTP API rather than scraping private storage. An optional positional window such as `7d` imports only sessions with activity in the last seven days; only day-based windows are supported.

All importers produce client-independent normalized sessions and pass them through the session engine; they never create consolidated knowledge directly. External formats are treated as versioned best-effort input. Reimport uses client plus external session identifier and is idempotent.

`--dry-run` reports discovered sessions, invalid records, and estimated bytes without persistence. A session without reliable existing project path is stored as an orphan in operational SQLite and is never guessed into a project. Orphan payload remains available for later administrative association and compilation.

## Web Interface

The daemon serves a responsive, server-rendered HTML interface with no CDN, React, or client framework. The dashboard summarizes projects, global memory, procedures, sessions, queue state, integrations, and provider health. Dedicated views cover projects, memories, procedures, sessions, search, imports, integrations, providers, and non-secret settings. Sessions appear as a chronological timeline without episodic sections. Project detail shows a single handoff panel with the current summary, its latest update, and its source sessions, plus a fingerprint-stale warning when applicable; revisions, status buckets, historical cards, and consume/complete/supersede actions are removed.

Memory lists filter by physical scope, type, status, and technology. Detail views show rendered content, raw Markdown, metadata, confidence, applicability, source sessions, procedure successes and failures, and supersession evidence. Administrative edits use the same Markdown and index application layer, commit durable history, and update search immediately.

The search view uses the runtime retrieval engine and exposes FTS rank, RRF constant, freshness, and final score. The visual interface is fully local and uses embedded assets with minimal JavaScript only for progressive page arrival.

REST endpoints under `/api/v1` cover health, projects, memories, sessions, imports, integrations, settings, jobs, providers, normalized events, recall, and handoffs. Recall accepts the integration client and external session identifier and returns bounded context with intent-ranking diagnostics. HTTP handlers delegate to the same engine used by CLI, MCP, hooks, and UI.

## Backup, Restore, And Distribution

The repository `install.sh` builds Menvane with Cargo by default, or accepts a prebuilt executable through `--binary`, and installs it at `~/.local/bin/menvane`. On Linux it also installs a user-scoped `menvane.service`, enables it for the user's default target, and requests an immediate non-blocking start. The service runs independently of the system boot critical path, restarts after failures, and serves both the daemon API and local UI. Reinstalling updates the executable and service idempotently. On other supported platforms, the script installs the executable without configuring automatic startup.

`menvane backup <path>` creates a new backup directory containing the complete Markdown memory repository, non-secret configuration, consistent SQLite online backups of both `index.sqlite` and `state.sqlite`, and a checksummed manifest. Existing destinations are never overwritten. `menvane restore <path> --confirm` verifies every checksum, configuration, Markdown frontmatter, and both SQLite databases independently before staging and replacing current state. Restore refuses to run while a daemon PID is present and never replaces state without explicit confirmation.

Daemon startup uses one process lock per Menvane home, graceful shutdown, idle-session recovery, WAL, bounded waits, leased job ownership, and idempotent event and job keys. Atomic Markdown writes and derived-index reindex permit reconciliation after interrupted index updates without removing operational state. Git durable-history writes are serialized independently from concurrent capture.

Release builds target Linux, macOS, and WSL as Linux. The repository CI runs formatting, Clippy, all tests, and release builds without real Codex authentication, OpenRouter credentials, or paid APIs. Runtime provider status may use local non-paid health interfaces; deterministic fake providers and mock servers cover CI behavior.

Menvane is operationally complete when normal work in any connected agent is captured and consolidated, and later connected agents in the same project receive project plus applicable global context automatically. Unrelated project memory remains isolated, contextual global memory respects technology, procedures strengthen through reuse, sessions archive without hard deletion, and rebuilding `index.sqlite` preserves all durable knowledge and operational evidence in `state.sqlite`.
