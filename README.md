# Menvane

**Durable memory for coding agents, with your data kept local.**

[Portuguese (Brazil)](README.pt-BR.md)

Menvane gives coding agents operational continuity across sessions, agents, and days. It captures chronological session evidence, distills an episodic summary per session, maintains a handoff of still-live work fronts, and recalls non-obvious knowledge on demand when work resumes.

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

- **Resume work faster:** a per-project handoff tracks only still-live work fronts, with provenance and next steps.
- **Reuse proven knowledge:** non-obvious memories and playbooks are consolidated from captured evidence, and most sessions promote nothing.
- **Recall the right context:** each prompt receives only the handoff items related to its intent plus up to three knowledge cards.
- **Keep projects isolated:** project memory is separated from unrelated repositories, with applicable global knowledge available when appropriate.
- **Inspect and own the data:** Markdown is the durable source of truth and remains readable without Menvane.
- **Avoid instruction pollution:** `AGENTS.md`, `SKILL.md`, and files under `skills` directories are not processed as memory or handoff file evidence.

## Key Capabilities

### Cross-session continuity

Each durable session is a chronological, sanitized capture of the observed events. Consolidation appends an episodic summary to the session and maintains a per-project handoff of still-live work fronts — in-progress work, open questions, parked ideas, and blockers — so later sessions resume without reconstructing the task. Concluded, discarded, and superseded fronts leave the handoff automatically.

### Evidence-based memory

One language-model consolidation per finalized session interprets the chronological capture and produces the episodic summary, explicit operations over every handoff item, and zero or more durable memories or playbooks — only non-obvious knowledge reusable beyond the current task passes the promotion barrier.

Memories decay to forgotten after 90 days by default, while playbooks keep their validation lifecycle without temporal decay. MCP reads and actual agent injection reinforce memories; CLI, REST, and dashboard views are observational. The lifetime is configurable:

```toml
[decay]
memory_lifetime_days = 90
```

### Local and rebuildable storage

Markdown stores durable knowledge. `index.sqlite` contains derived search data and can be rebuilt with `menvane reindex`; operational session and handoff state is kept separately.

### Agent-native integrations

Claude Code, Codex, and OpenCode use the same capture, sanitization, recall, and trust boundary. Integrations preserve unrelated client configuration and install only Menvane-owned entries.

## How It Works

```text
Agent session
     |
     v
Capture -> sanitize -> chronological session -> LLM consolidation
                                                   |
                    episodic summary <-+-----------+-----------+-> handoff items
                                       |                        |
                                       v                        v
                            on-demand knowledge        live work fronts
                                       |
                                       v
Prompt recall <- related handoff items + up to 3 knowledge cards
```

Menvane captures bounded normalized events, removes sensitive data and ignored paths, and keeps real user prompts, tool activity, and lifecycle events distinct without guessing intent. Lifecycle events produce a deterministic session record and queue one consolidation job without blocking the agent.

## Quickstart

### Install And Connect

Requirements: macOS or Linux with a POSIX shell, `install`, and either Rust
(`cargo`) or a prebuilt Menvane binary. Linux automatic startup additionally
requires a user systemd session and `systemctl --user`; WSL requires systemd
to be enabled. Native Windows is not currently a release target.

```bash
git clone https://github.com/jonasrodrigues93/menvane.git
cd menvane
./install.sh

menvane doctor
menvane connect claude
```

The script builds Menvane with Cargo and installs it at `~/.local/bin/menvane`.
If Cargo is not installed, install Rust with [rustup](https://rustup.rs/) first.
You can pass `--binary <path>` to install an existing release binary instead.
Make sure `~/.local/bin` is in your `PATH`.

On Linux, installation enables and starts a user-scoped `menvane.service`.
The daemon and local UI then start automatically with the user session. On
macOS, installation creates and loads a LaunchAgent at
`~/Library/LaunchAgents/com.jonasrodrigues93.menvane.plist`, so the daemon
starts at login and restarts after failure. Use `menvane connect codex` or
`menvane connect opencode` for the other supported clients. Capture and recall
happen automatically; no Skill, repository instruction file, or explicit
memory prompt is required.

Menvane supports Linux, macOS, and WSL with systemd enabled.

### Enable Memory Compilation

Capture, search, and manual memory operations work without a language-model provider. Episodic summaries, handoff maintenance, and knowledge consolidation require a configured provider. To enable consolidation with OpenAI:

```bash
menvane provider configure openai --model gpt-5.6-luna --reasoning-effort medium
menvane provider login openai
menvane daemon restart
menvane provider status
```

Authorization opens OpenAI in the system browser. Menvane stores its own refreshable credentials under `~/.menvane/oauth/` and never reads credentials from OpenCode or Codex.

GitHub Copilot can be enabled with GitHub's OAuth device flow:

Prerequisites: a GitHub OAuth app with device flow enabled and a GitHub account with Copilot access. Use the app's client ID; no client secret is required.

```bash
menvane provider configure github-copilot --model gpt-4.1 --client-id <github-oauth-client-id>
menvane provider login github-copilot
menvane daemon restart
menvane provider status
```

The login command prints a GitHub verification URL and user code. Menvane stores its own refreshable credentials under `~/.menvane/oauth/github-copilot.json` and never reads GitHub CLI or Copilot CLI credentials.

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
menvane write --type memory --title "Migration rule" --content "..."
menvane forget <memory-id>
menvane handoff inspect
```

Session start delivers only minimal project identity and the current handoff. Each prompt then receives only the handoff items related to its intent plus up to three memory or playbook cards; the hot path never calls a language-model provider. Full memory bodies stay available through explicit reads.

Explicit search uses the query provided by the caller. Full Markdown and bounded provenance remain available through `menvane read` and the local UI.

Automatic recall uses conservative English and Portuguese lexical matching. It combines FTS5 with embeddings whenever an independent embedding provider is configured and healthy, and falls back to FTS5 when embeddings are unavailable. Configure an OpenAI-compatible embedding endpoint in `~/.menvane/config.toml`:

```toml
[embeddings]
provider = "openai-api"
model = "text-embedding-3-small"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
min_similarity = 0.78
```

Restart the daemon and run `menvane reindex` after enabling or changing the embedding model.
External embeddings are disabled by default. Enabling them sends sanitized recall prompts and durable memory titles and bodies to the configured endpoint.

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

Issues, documentation improvements, tests, and implementation contributions are welcome. Please keep changes focused, preserve the documented product behavior in [`product.md`](product.md), and run the relevant test suite before submitting a change. See [`LICENCE.md`](LICENCE.md) and [`SECURITY.md`](SECURITY.md) for project terms and private vulnerability reporting.
