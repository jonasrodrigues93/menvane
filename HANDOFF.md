# Menvane Handoff

## Current State

The continuity-first refactor is in progress. Phases 0 through 5 are committed and the workspace was passing `cargo test --workspace` before Phase 6 began.

Completed commits:

- `0549acd docs: define continuity-first memory model`
- `b15277e refactor!: replace legacy memory domain`
- `a0922f8 refactor!: recreate operational and index schemas`
- `763a798 feat: add episodic session summaries`
- `f716345 feat: derive current handoff from live work items`

Phase 3 was an operational home reset, not a repository commit. The old `~/.menvane` data was deleted after approval. Only the OpenAI provider configuration and `~/.menvane/oauth/openai.json` were preserved. The new home passed `provider status`, `doctor`, SQLite integrity checks, and foreign-key checks. OAuth permissions are `0600`.

## In Progress

Phase 6, knowledge `context` and `playbook`, has been implemented in the working tree but is not committed.

Current modified files:

- `crates/menvane-domain/src/consolidation.rs`
- `crates/menvane-domain/src/lib.rs`
- `crates/menvane-engine/src/lib.rs`
- `crates/menvane-engine/tests/current_continuity.rs`
- `crates/menvane-store/src/sqlite.rs`

The Phase 6 changes add:

- promotion-barrier validation for inferred knowledge;
- duplicate and forgotten-knowledge protection;
- idempotent `context` and `playbook` operations;
- `Menvane::apply_playbook` with success/failure recording;
- candidate-to-active playbook promotion after a second independent success;
- reindex preservation tests for lifecycle, provenance, and application counters;
- deterministic tests for zero promotion, promotion, application retries, failures, forgotten knowledge, merge, and supersede.

The Phase 6 subagent reported `cargo test --workspace` passing, but the result has not been independently rerun after the last working-tree inspection.

## Resume Next

1. Run `cargo fmt --all -- --check` and `cargo test --workspace`.
2. Review the Phase 6 promotion barrier in `validate_knowledge_promotion`; ensure it rejects temporary task state without rejecting legitimate reusable context or playbooks.
3. Review the Phase 6 diff and commit it as `feat: replace typed memories with context and playbooks`.
4. Continue Phase 7: intent-oriented local retrieval and progressive disclosure.
5. Keep `memory-model-analysis.md` untracked and untouched; it predates this handoff and is not part of the refactor commits.

## Constraints

- Do not restore legacy `fact`, `decision`, `gotcha`, `procedure`, `session-memory`, Goals, episodes, or typed briefing compatibility.
- `product.md` is the behavioral source of truth and must be read before behavior changes.
- Do not generate a Project Brief or general project summary.
- Keep Markdown canonical and SQLite derived/rebuildable where applicable.
- Commit each completed phase with the Conventional Commit message specified by `plan.md`.
- Do not stage `memory-model-analysis.md` unless explicitly requested.
