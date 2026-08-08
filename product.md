# Menvane

Version: 0.1.0

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
