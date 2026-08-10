# Menvane

## Your Agents Remember The Work

Menvane is a local, persistent memory layer for coding agents. It captures the important parts of a task, turns proven discoveries into reusable knowledge, and brings the right context back when the work resumes.

No more re-explaining the architecture. No more losing the reason behind a decision. No more asking an agent to rediscover the same fix in every session.

Menvane runs on your machine. Durable knowledge is human-readable Markdown, while SQLite provides a fast, rebuildable search index.

## Why Menvane

- **Continuity across sessions:** Resume unfinished work with goals, blockers, decisions, validation, changed files, and next actions.
- **Memory that earns trust:** Facts, decisions, procedures, and gotchas are consolidated from captured evidence, not copied from raw transcripts.
- **Relevant context, not prompt bloat:** Automatic recall combines the current prompt with the active task goal, corrections, constraints, and conversation goal.
- **Project-aware knowledge:** Project memory stays isolated, while applicable global knowledge can be reused safely across repositories.
- **Local-first control:** Markdown is the source of truth. Search indexes are derived and can be rebuilt.
- **Works with your agent:** Claude Code, Codex, and OpenCode use the same capture, recall, sanitization, and trust boundary.

## How It Works

```text
Agent session
     |
     v
Capture -> sanitize -> task episodes -> handoff
                                      |
                                      v
                              evidence-based memory
                                      |
                                      v
Prompt recall <- project and global knowledge
```

Menvane records bounded, normalized session events and groups them into task episodes. At lifecycle boundaries it creates a concise session record and an operational handoff. A memory compiler can then consolidate reusable knowledge while preserving provenance back to the source events.

Agent instructions are not treated as knowledge: `AGENTS.md`, `SKILL.md`, and files under `skills` directories are excluded from memory and handoff file processing.

## Quick Start

### Build

```bash
cargo build --release --locked
install -m 755 target/release/menvane ~/.local/bin/menvane
```

Rust's supported toolchain installs Menvane on Linux, macOS, and WSL. Native Windows is not currently a release target.

### Connect An Agent

```bash
menvane doctor
menvane daemon start
menvane connect claude
```

Use `menvane connect codex` or `menvane connect opencode` for the other supported integrations. Menvane preserves unrelated client configuration and installs only its owned integration entries.

Capture and recall then happen automatically. No Skill, repository instruction file, or explicit memory prompt is required.

### Enable Memory Compilation

Menvane can capture, search, and provide handoffs without a language-model provider. To enable evidence-based consolidation with OpenAI:

```bash
menvane provider configure openai --model gpt-5.6-luna --reasoning-effort medium
menvane provider login openai
menvane daemon restart
menvane provider status
```

Authorization opens OpenAI in the system browser. Menvane stores its own refreshable credentials under `~/.menvane/oauth/` and never reads credentials from OpenCode or Codex. Run `menvane provider logout openai` to remove them.

## Explore Your Memory

The local dashboard is available at <http://127.0.0.1:47831/>.

```bash
menvane search "database migration"
menvane read <memory-id>
menvane write --type gotcha --title "Migration rule" --content "..."
menvane forget <memory-id>
```

The search path combines SQLite FTS5 with ranked retrieval, project scope, global applicability, memory lifecycle, confidence, freshness, and technology context. Full Markdown remains available when you need the complete provenance.

## Trust And Recovery

- Capture removes authentication headers, likely secrets, and configured ignored paths before persistence.
- Prompts and tool inputs and outputs are bounded.
- Private model reasoning is never captured.
- Injected memories are marked as historical context; current user instructions and repository state remain authoritative.
- Session memories, handoffs, and durable knowledge remain local and inspectable.

Markdown is the durable source of truth. Rebuild the derived index at any time:

```bash
rm ~/.menvane/index.sqlite
menvane reindex
```

Backup and validated restore:

```bash
menvane backup ~/menvane-backup
menvane daemon stop
menvane restore ~/menvane-backup --confirm
```

Set `MENVANE_HOME` to isolate or relocate all Menvane state.

## License

Menvane is under active development. See the repository for the current license and contribution details.
