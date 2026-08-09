# Menvane

Version: 1.7.0

Menvane is a local persistent memory system for agents. In its current version, it provides a durable command-line memory foundation that stores human-readable Markdown as the source of truth and uses SQLite with FTS5 as a rebuildable search index.

## Storage

Menvane stores data under `~/.menvane` by default. `MENVANE_HOME` overrides that location.

The home contains `config.toml`, `index.sqlite`, `state.sqlite`, operational directories, global memory directories, and project memory directories. Durable memories and project metadata are Markdown with YAML frontmatter. `index.sqlite` contains only derived project and memory metadata, FTS5 search data, and embeddings. `state.sqlite` contains operational sessions, events, observations, jobs, access events, procedure applications, imports, orphans, integration state, and injection claims.

Markdown writes use a temporary file, filesystem synchronization, and atomic rename before the derived index is updated. If Git is available, the memory directory is initialized as a local repository and durable memory changes are committed automatically. Menvane remains functional when Git is unavailable.

Deleting `index.sqlite` does not delete durable knowledge or operational evidence. `menvane reindex` validates all Markdown into a temporary SQLite database and atomically installs the rebuilt derived index without changing `state.sqlite`. Existing homes migrate operational tables from the legacy `index.sqlite` to `state.sqlite` with durable, resumable table markers; legacy tables remain until the derived index is safely rebuilt.

## Memory Model

Durable knowledge uses exactly five memory types: fact, decision, procedure, gotcha, and session. The current manual write command creates facts, decisions, procedures, and gotchas. Sessions are reserved for captured episodic evidence.

Physical scope is either global or project. Project scope exists only when the working directory belongs to a Git repository; outside Git, writes, sessions, search, and automatic recall use global scope and no project metadata is created. Project search returns the current project plus global memories by default and never includes unrelated projects. Forgotten memories remain in Markdown with `status: forgotten` and are excluded from normal search.

Every memory has an identifier, type, scope, status, confidence, timestamps, source sessions, tags, optional applicability, and supersession metadata. Applicability dimensions are languages, frameworks, tools, databases, and platforms; empty dimensions indicate the memory is not tied to specific technologies.

## Project Resolution

Menvane identifies a project only when the working directory belongs to a Git repository, in this order:

1. An explicit `project` value in the nearest ancestor `.menvane.toml`.
2. A Git repository identity, preferring a normalized canonical remote and otherwise using the canonical Git common directory.

Without a Git repository, the working directory has no project identity and memory activity is global. A `.menvane.toml` project override does not create a project outside Git.

Equivalent HTTPS, SSH, and SCP-style Git remotes resolve to the same identity. Worktrees sharing a Git common directory resolve to the same project. Known checkout paths are informational and do not define identity when a remote is available.

Each project has a `project.md` containing its stable identity, known paths, and detected technology profile. Menvane updates this file when paths or technologies change.

## Technology Detection

Technology detection is deterministic and inspects known project files and dependency manifests. Profiles contain languages, frameworks, tools, databases, and platforms.

## Commands

`menvane write` creates a durable memory. `menvane search` searches current-project and global memory by default. `menvane read` displays a memory. `menvane forget` marks one forgotten. `menvane reindex` reconstructs the derived index from Markdown. `menvane doctor` checks the home, index database, state database, FTS5, Git availability, and Markdown/index consistency independently.

## Retrieval

Retrieval uses SQLite FTS5 and Reciprocal Rank Fusion with `K = 60`. Procedures and decisions receive a 1.15 type multiplier, gotchas 1.10, facts 1.00, and sessions 0.75. Active, candidate, needs-validation, superseded, and historical statuses receive their defined lifecycle multipliers. Sessions are excluded unless explicitly requested.

Automatic recall searches only the current project and global memory. Global universal memories are eligible everywhere. Global contextual memories are eligible only when every populated applicability dimension overlaps the current project's detected technologies. Explicit searches may inspect an otherwise incompatible contextual memory when the query names one of its technologies.

Embedding providers are independent from language-model providers. Embedding storage is derived and reconstructible; retrieval remains fully functional with FTS5 when no embedding provider is configured.

## MCP

`menvane mcp` serves MCP over newline-delimited JSON-RPC on standard input and output. It resolves the active project from its process working directory and exposes exactly `memory_search`, `memory_read`, `memory_write`, and `memory_forget`.

MCP search returns identifiers, type, scope, title, score, status, confidence, applicability, and a short excerpt. Read returns metadata, the full Markdown body, source sessions, and supersession metadata. Forgetting changes status without deleting Markdown. Automatic manual writes conservatively use project scope when no compiler is available.

## Capture And Sessions

Clients send a normalized vocabulary of session-started, user-prompt, tool-completed, context-compacted, turn-stopped, and session-ended events. Events carry stable event identifiers and are ingested idempotently. Concurrent delivery uses SQLite WAL and a busy timeout.

Capture removes authentication headers, likely API keys and tokens, bounds prompts and tool inputs and outputs, and drops reliably attributed ignored paths before persistence. Default limits are 16,384 bytes for prompts and 4,096 bytes for tool input and output. Default ignored paths include environment files, secret directories, and SSH directories. Menvane never captures private model reasoning.

Sessions are open, idle, or finalized. Session end finalizes immediately. Turn stop marks idle, and idle sessions finalize after 120 seconds by default. Events arriving after finalization reuse the external session identifier in a new generation and process only new evidence.

Finalization writes concise episodic Markdown containing the goal, outcome, important actions, explicit deterministic evidence, errors, validation, and involved files. It does not copy complete transcripts or tool outputs. Finalization is idempotent and queues compilation without requiring an available language-model provider.

## Daemon And REST

`menvane serve` runs the Axum daemon on `127.0.0.1:47831` by default. A per-home process lock prevents duplicate daemons. `menvane daemon start`, `stop`, `restart`, and `status` manage the background process.

The REST foundation is under `/api/v1`. Health, normalized event ingestion, and job inspection are available. Capture and finalization share the same engine and stores used by CLI and MCP. SQLite jobs use pending, running, completed, and failed lifecycle states with attempts, retry time, and error fields; capture does not wait for compiler work.

## Claude Code Integration

`menvane connect claude` installs a user-scoped Menvane MCP server and command hooks for session start, user prompt submission, completed tools, pre-compaction, stop, and session end. It uses the strongest supported Claude lifecycle events, preserves unrelated configuration, creates timestamped backups before changes, and is idempotent. `menvane disconnect claude` removes only entries whose command and MCP definition are owned by the current Menvane executable. Menvane does not create or modify `CLAUDE.md` or skills.

Claude hooks normalize client payloads before domain ingestion and ensure the daemon is running. Hooks originating from `MENVANE_INTERNAL=1` are ignored. Reliably attributed ignored paths are dropped and all capture is sanitized before local daemon transport.

Session start injects a briefing of project identity, detected technologies, active decisions, critical gotchas, and high-confidence applicable global facts within 2,500 characters. User prompts retrieve at most six relevant project and applicable global memories within 6,000 characters. No external language-model request occurs on this path, and identical memories are not repeatedly injected into one external session.

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

The memory compiler receives bounded session evidence, important prompts and tools, errors, decisions, validation results, existing related memories, and the project technology profile. It requests schema-constrained JSON and never allows a provider to write Markdown directly. Invalid structured output receives one bounded retry and then fails.

Compiler output may contain zero memories. Valid output uses only facts, decisions, procedures, and gotchas. Applicability is optional; empty dimensions indicate the memory is not tied to specific technologies. Procedure content contains trigger, preconditions, ordered steps, decision points, validation, failure handling, and expected outcome. Global classification requires high scope confidence; uncertainty resolves to project scope.

Before creating durable memory, Menvane searches the same scope and type. Equivalent content reinforces confidence and source evidence. Incompatible content with the same identity creates a new memory and supersedes the old one instead of silently rewriting history. Provider unavailability does not affect capture, session Markdown, manual memory operations, search, MCP, project detection, or technology detection; compilation jobs remain durable and retryable.

## Procedure Learning And Promotion

A first successful procedure is a candidate with one success and no failures. An independently recorded successful application increments successes, updates verification time, and activates the procedure at two successes. Duplicate delivery of the same session signal is idempotent. A failed application increments failures but does not delete or automatically replace the procedure, which remains available for later compiler evaluation.

Global promotion evaluates equivalent project procedures and gotchas. Execution-derived knowledge remains project-scoped until equivalent evidence exists in at least two independent projects. Promotion creates one active contextual global memory retaining source project identifiers, source sessions, successes, failures, confidence, and applicability. Source variants remain inspectable as historical evidence. Retrieval deduplicates project and global variants.

Project-specific decisions are not candidates for global promotion. Explicit universal preferences may still be classified as global facts directly by the compiler.

## Codex Agent Integration

`menvane connect codex` merges a user-level MCP server and supported lifecycle hooks into `CODEX_HOME/config.toml`, defaulting to `~/.codex/config.toml`. It preserves unrelated models, servers, hooks, and settings, creates a backup before changes, enables supported hooks, and is idempotent. Disconnect removes only the matching Menvane MCP and hook commands. It never modifies `AGENTS.md`.

Codex session start, user prompt, completed tool, pre- and post-compaction, stop, and session end payloads normalize into the shared event vocabulary. Capture is sanitized before daemon transport. Session start and user prompt hooks use the same bounded automatic recall and trust boundary as Claude Code. `MENVANE_INTERNAL=1` prevents provider inference from recursively creating Codex agent sessions.

## OpenCode Integration

`menvane connect opencode` preserves and extends the user OpenCode JSON configuration, registers the local Menvane MCP server, and installs one owned vanilla JavaScript plugin under the OpenCode configuration directory. The installer creates backups and is idempotent. Disconnect removes only the matching Menvane plugin URI, MCP entry, and unchanged owned plugin file.

The plugin only forwards session, message, compaction, and completed-tool activity to `menvane hook opencode` and appends returned context before model dispatch. It contains no ranking, applicability, consolidation, compiler, or memory-domain logic. OpenCode payloads normalize into the same domain vocabulary and use the same daemon capture, retrieval, sanitization, trust boundary, and session lifecycle as Claude Code and Codex.

## Decay And Maintenance

Sessions use a 45-day time half-life plus meaningful-access reinforcement with a 60-day half-life. `menvane gc` archives a session only when it is older than 90 days and retention is below 0.15. Archived Markdown moves to `memory/archive/sessions`, remains durable, and is excluded from normal retrieval. Hard deletion is disabled.

Facts and gotchas never disappear due to age and have a freshness floor of 0.50 with a 180-day half-life. Procedures never disappear due to age and have a 0.65 floor with a 365-day half-life. Decisions have no temporal decay and rank only by lifecycle status. Superseded and historical memories remain inspectable at lower rank.

Menvane records retrieved, injected, explicitly read, successfully applied, and failed application as separate signals. Retrieval and injection do not validate memory. Explicit reads and successful applications are meaningful access; successful procedure application is the strongest positive verification and failed application remains negative evidence. This prevents popularity loops from simple repeated surfacing.

## Historical Import

`menvane import claude` and `menvane import codex` recursively discover supported JSONL session files under configured client homes. Readers stream line by line, enforce a one-megabyte record bound, skip and count malformed records, ignore unknown event types, and retain only useful user and tool evidence. Codex checks both active and archived session directories. `menvane import opencode` uses the configured local OpenCode HTTP API rather than scraping private storage. An optional positional window such as `7d` imports only sessions with activity in the last seven days; only day-based windows are supported.

All importers produce client-independent normalized sessions and pass them through the session engine; they never create consolidated knowledge directly. External formats are treated as versioned best-effort input. Reimport uses client plus external session identifier and is idempotent.

`--dry-run` reports discovered sessions, invalid records, and estimated bytes without persistence. A session without reliable existing project path is stored as an orphan in operational SQLite and is never guessed into a project. Orphan payload remains available for later administrative association and compilation.

## Web Interface

The daemon serves a responsive, server-rendered HTML interface with no CDN, React, or client framework. The dashboard summarizes projects, global memory, procedures, sessions, queue state, integrations, and provider health. Dedicated views cover projects, memories, procedures, sessions, search, imports, integrations, providers, and non-secret settings.

Memory lists filter by physical scope, type, status, and technology. Detail views show rendered content, raw Markdown, metadata, confidence, applicability, source sessions, procedure successes and failures, and supersession evidence. Administrative edits use the same Markdown and index application layer, commit durable history, and update search immediately.

The search view uses the runtime retrieval engine and exposes FTS rank, RRF constant, freshness, and final score. The visual interface is fully local and uses embedded assets with minimal JavaScript only for progressive page arrival.

REST endpoints under `/api/v1` cover health, projects, memories, sessions, imports, integrations, settings, jobs, providers, normalized events, and recall. HTTP handlers delegate to the same engine used by CLI, MCP, hooks, and UI.

## Backup, Restore, And Distribution

`menvane backup <path>` creates a new backup directory containing the complete Markdown memory repository, non-secret configuration, consistent SQLite online backups of both `index.sqlite` and `state.sqlite`, and a checksummed manifest. Existing destinations are never overwritten. `menvane restore <path> --confirm` verifies every checksum, configuration, Markdown frontmatter, and both SQLite databases independently before staging and replacing current state. Restore refuses to run while a daemon PID is present and never replaces state without explicit confirmation.

Daemon startup uses one process lock per Menvane home, graceful shutdown, idle-session recovery, WAL, bounded waits, and idempotent event and job keys. Atomic Markdown writes and derived-index reindex permit reconciliation after interrupted index updates without removing operational state. Git durable-history writes are serialized independently from concurrent capture.

Release builds target Linux, macOS, and WSL as Linux. The repository CI runs formatting, Clippy, all tests, and release builds without real Codex authentication, OpenRouter credentials, or paid APIs. Runtime provider status may use local non-paid health interfaces; deterministic fake providers and mock servers cover CI behavior.

Menvane is operationally complete when normal work in any connected agent is captured and consolidated, and later connected agents in the same project receive project plus applicable global context automatically. Unrelated project memory remains isolated, contextual global memory respects technology, procedures strengthen through reuse, sessions archive without hard deletion, and rebuilding `index.sqlite` preserves all durable knowledge and operational evidence in `state.sqlite`.
