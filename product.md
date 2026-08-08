# Menvane

Version: 0.6.0

Menvane is a local persistent memory system for coding agents. In its current version, it provides a durable command-line memory foundation that stores human-readable Markdown as the source of truth and uses SQLite with FTS5 as a rebuildable search index.

## Storage

Menvane stores data under `~/.menvane` by default. `MENVANE_HOME` overrides that location.

The home contains `config.toml`, `index.sqlite`, operational directories, global memory directories, and project memory directories. Durable memories and project metadata are Markdown with YAML frontmatter. SQLite contains derived project and memory metadata and an FTS5 index.

Markdown writes use a temporary file, filesystem synchronization, and atomic rename before the derived index is updated. If Git is available, the memory directory is initialized as a local repository and durable memory changes are committed automatically. Menvane remains functional when Git is unavailable.

Deleting `index.sqlite` does not delete durable knowledge. `menvane reindex` validates all Markdown into a temporary SQLite database and atomically installs the rebuilt index.

## Memory Model

Durable knowledge uses exactly five memory types: fact, decision, procedure, gotcha, and session. The current manual write command creates facts, decisions, procedures, and gotchas. Sessions are reserved for captured episodic evidence.

Physical scope is either global or project. Project search returns the current project plus global memories by default and never includes unrelated projects. Forgotten memories remain in Markdown with `status: forgotten` and are excluded from normal search.

Every memory has an identifier, type, scope, status, confidence, timestamps, source sessions, tags, applicability, and supersession metadata. Applicability dimensions are languages, frameworks, tools, databases, and platforms.

## Project Resolution

Menvane identifies a project in this order:

1. An explicit `project` value in the nearest ancestor `.menvane.toml`.
2. A Git repository identity, preferring a normalized canonical remote and otherwise using the canonical Git common directory.
3. The canonical absolute filesystem path.

Equivalent HTTPS, SSH, and SCP-style Git remotes resolve to the same identity. Worktrees sharing a Git common directory resolve to the same project. Known checkout paths are informational and do not define identity when a remote is available.

Each project has a `project.md` containing its stable identity, known paths, and detected technology profile. Menvane updates this file when paths or technologies change.

## Technology Detection

Technology detection is deterministic and inspects known project files and dependency manifests. Profiles contain languages, frameworks, tools, databases, and platforms.

## Commands

`menvane write` creates a durable memory. `menvane search` searches current-project and global memory by default. `menvane read` displays a memory. `menvane forget` marks one forgotten. `menvane reindex` reconstructs SQLite from Markdown. `menvane doctor` checks the home, SQLite, FTS5, Git availability, and Markdown/index consistency.

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

The default provider is `codex` with model `default`. It invokes the installed Codex CLI and uses existing local Codex authentication without reading or persisting credentials. Internal calls set `MENVANE_INTERNAL=1`, execute in an ephemeral temporary directory with a read-only sandbox, ignore user and project configuration, disable available tools and hooks, supply all evidence directly, and delete schema and response files afterward. Health distinguishes missing binary, missing authentication, unavailable explicit model, and ready state.

The `openrouter` provider uses the OpenAI-compatible chat completions endpoint and JSON Schema response format. Its model must be explicit. The API key is read only from the configured environment variable, which defaults to `OPENROUTER_API_KEY`, and is never written to Markdown, SQLite, logs, Git, or responses.

An explicit fallback provider may be configured under `[llm.fallback]`. Fallback applies only to provider availability, authentication, usage limits, network errors, and unsupported capabilities. It does not hide invalid Menvane input, invalid schemas, or internal defects.

`menvane provider status` performs only local and configuration health checks and does not make paid inference requests. `menvane provider test` performs one minimal structured request and validates the response. Doctor includes provider compatibility and health.

## Memory Compilation

The memory compiler receives bounded session evidence, important prompts and tools, errors, decisions, validation results, existing related memories, and the project technology profile. It requests schema-constrained JSON and never allows a provider to write Markdown directly. Invalid structured output receives one bounded retry and then fails.

Compiler output may contain zero memories. Valid output uses only facts, decisions, procedures, and gotchas. Procedure content contains trigger, preconditions, ordered steps, decision points, validation, failure handling, and expected outcome. Global classification requires high scope confidence; uncertainty resolves to project scope.

Before creating durable memory, Menvane searches the same scope and type. Equivalent content reinforces confidence and source evidence. Incompatible content with the same identity creates a new memory and supersedes the old one instead of silently rewriting history. Provider unavailability does not affect capture, session Markdown, manual memory operations, search, MCP, project detection, or technology detection; compilation jobs remain durable and retryable.

## Procedure Learning And Promotion

A first successful procedure is a candidate with one success and no failures. An independently recorded successful application increments successes, updates verification time, and activates the procedure at two successes. Duplicate delivery of the same session signal is idempotent. A failed application increments failures but does not delete or automatically replace the procedure, which remains available for later compiler evaluation.

Global promotion evaluates equivalent project procedures and gotchas. Execution-derived knowledge remains project-scoped until equivalent evidence exists in at least two independent projects. Promotion creates one active contextual global memory retaining source project identifiers, source sessions, successes, failures, confidence, and applicability. Source variants remain inspectable as historical evidence. Retrieval deduplicates project and global variants.

Project-specific decisions are not candidates for global promotion. Explicit universal preferences may still be classified as global facts directly by the compiler.
