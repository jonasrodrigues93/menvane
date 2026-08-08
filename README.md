# Menvane

Menvane is a local persistent memory system for Claude Code, OpenAI Codex, and OpenCode. Durable knowledge is Markdown; SQLite is a rebuildable FTS and operational index.

## Build

```bash
cargo build --release --locked
install -m 755 target/release/menvane ~/.local/bin/menvane
```

Rust's supported toolchain installs Menvane on Linux, macOS, and WSL. Native Windows is not currently a release target.

## Start

```bash
menvane doctor
menvane daemon start
menvane connect claude
menvane connect codex
menvane connect opencode
```

The local UI is available at `http://127.0.0.1:47831/`. Integrations capture and recall automatically; no Skill, repository instruction file, or explicit memory prompt is required.

## Durable Recovery

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
