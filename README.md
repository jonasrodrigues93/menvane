# Menvane

**Durable memory for coding agents, with your data kept local.**

[Portuguese (Brazil)](README.pt-BR.md)

Menvane gives coding agents continuity across sessions and projects. It captures task progress, preserves operational handoffs, consolidates reusable knowledge from evidence, and recalls relevant context when work resumes.

Menvane is local-first: durable knowledge is human-readable Markdown, while SQLite provides a fast, rebuildable search index.

## Table Of Contents

- [About](#about)
- [Key Capabilities](#key-capabilities)
- [How It Works](#how-it-works)
- [Quickstart](#quickstart)
- [Integrations](#integrations)
- [Search And Memory](#search-and-memory)
- [Privacy And Trust](#privacy-and-trust)
- [Recovery](#recovery)
- [Build](#build)
- [Contributing](#contributing)

## About

Menvane is designed for teams and developers who want agents to remember the work without handing their project context to a hosted memory service.

- **Resume work faster:** handoffs preserve goals, blockers, decisions, validation, changed files, and next actions.
- **Reuse proven knowledge:** facts, decisions, procedures, and gotchas are consolidated from captured evidence.
- **Recall the right context:** automatic retrieval considers the current prompt, active task goal, corrections, constraints, and conversation goal.
- **Keep projects isolated:** project memory is separated from unrelated repositories, with applicable global knowledge available when appropriate.
- **Inspect and own the data:** Markdown is the durable source of truth and remains readable without Menvane.
- **Avoid instruction pollution:** `AGENTS.md`, `SKILL.md`, and files under `skills` directories are not processed as memory or handoff file evidence.

## Key Capabilities

### Cross-session continuity

Menvane groups activity into task episodes and maintains one current, versioned handoff per project. A later session can receive bounded, repository-aware context instead of reconstructing the task from a transcript.

### Evidence-based memory

Captured sessions are finalized into bounded episodic records. A structured compiler can consolidate reusable facts, decisions, procedures, and gotchas while retaining source-event provenance and respecting contradictions, scope, confidence, and forgotten-memory rules.

### Local and rebuildable storage

Markdown stores durable knowledge. `index.sqlite` contains derived search data and can be rebuilt with `menvane reindex`; operational session and handoff state is kept separately.

### Agent-native integrations

Claude Code, Codex, and OpenCode use the same capture, sanitization, recall, and trust boundary. Integrations preserve unrelated client configuration and install only Menvane-owned entries.

## How It Works

```text
Agent session
     |
     v
Capture -> sanitize -> task episode -> LLM consolidation -> project handoff
                                      |
                                      v
                              evidence-based memory
                                      |
                                      v
Prompt recall <- project and applicable global knowledge
```

Menvane captures bounded normalized events, removes sensitive data and ignored paths, identifies task intent, and links meaningful progress to an episode. Lifecycle events produce a session record and queue durable compilation work without blocking the agent.

## Quickstart

### Install And Connect

```bash
cargo build --release --locked
install -m 755 target/release/menvane ~/.local/bin/menvane

menvane doctor
menvane daemon start
menvane connect claude
```

Use `menvane connect codex` or `menvane connect opencode` for the other supported clients. Capture and recall happen automatically; no Skill, repository instruction file, or explicit memory prompt is required.

Menvane supports Linux, macOS, and WSL. Native Windows is not currently a release target.

### Enable Memory Compilation

Capture, search, and manual memory operations work without a language-model provider. Handoff and evidence-based memory consolidation require a configured provider. To enable consolidation with OpenAI:

```bash
menvane provider configure openai --model gpt-5.6-luna --reasoning-effort medium
menvane provider login openai
menvane daemon restart
menvane provider status
```

Authorization opens OpenAI in the system browser. Menvane stores its own refreshable credentials under `~/.menvane/oauth/` and never reads credentials from OpenCode or Codex.

## Integrations

| Client | Connect command | Captured lifecycle |
| --- | --- | --- |
| Claude Code | `menvane connect claude` | Session, prompts, tools, compaction, stop, end |
| Codex | `menvane connect codex` | Session, prompts, tools, compaction, stop, end |
| OpenCode | `menvane connect opencode` | Session, messages, tools, compaction |

The local dashboard is available at <http://127.0.0.1:47831/>.

## Search And Memory

```bash
menvane search "database migration"
menvane read <memory-id>
menvane write --type gotcha --title "Migration rule" --content "..."
menvane forget <memory-id>
```

Automatic prompt recall combines independently ranked searches for the sanitized current prompt, active episode goal, corrections, constraints, and conversation root goal. It applies project scope, global applicability, memory lifecycle, type, confidence, freshness, and technology context.

Explicit search uses the query provided by the caller. Full Markdown and bounded provenance remain available through `menvane read` and the local UI.

## Privacy And Trust

- Capture removes authentication headers and likely API keys, tokens, and passwords.
- Prompts and tool inputs and outputs are bounded before persistence.
- Configured ignored paths are dropped when reliably attributed.
- `AGENTS.md`, `SKILL.md`, and files under `skills` directories are excluded from memory and handoff file processing.
- Private model reasoning is never captured.
- Injected memories are historical context; current user instructions and repository state remain authoritative.
- Menvane never reads or modifies OpenCode or Codex credentials.

## Recovery

Rebuild the derived index without deleting durable knowledge:

```bash
rm ~/.menvane/index.sqlite
menvane reindex
```

Create and restore a validated backup:

```bash
menvane backup ~/menvane-backup
menvane daemon stop
menvane restore ~/menvane-backup --confirm
```

Set `MENVANE_HOME` to isolate or relocate all Menvane state.

## Build

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo build --release --locked
```

## Contributing

Issues, documentation improvements, tests, and implementation contributions are welcome. Please keep changes focused, preserve the documented product behavior in [`product.md`](product.md), and run the relevant test suite before submitting a change.
