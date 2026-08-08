# Menvane

Version: 0.3.0

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
