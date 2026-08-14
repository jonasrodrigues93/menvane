# Menvane Handoff

## Current State

The continuity-first refactor is in progress. Phases 0 through 7 are committed and the workspace passes `cargo fmt --all -- --check`, `cargo test --workspace`, and clippy with no new warnings (12 pre-existing warnings remain, targeted by Phase 9).

Completed commits:

- `0549acd docs: define continuity-first memory model`
- `b15277e refactor!: replace legacy memory domain`
- `a0922f8 refactor!: recreate operational and index schemas`
- `763a798 feat: add episodic session summaries`
- `f716345 feat: derive current handoff from live work items`
- `14ed82c feat: replace typed memories with context and playbooks`
- `cca15bc refactor: make recall handoff and intent oriented`

Phase 3 was an operational home reset, not a repository commit. The old `~/.menvane` data was deleted after approval. Only the OpenAI provider configuration and `~/.menvane/oauth/openai.json` were preserved. The new home passed `provider status`, `doctor`, SQLite integrity checks, and foreign-key checks. OAuth permissions are `0600`.

Phase 6 review note: the promotion-barrier task-state check originally matched substrings, so legitimate reusable content containing words like "appending" or "spending" would have been rejected. It now matches the single-word terms `pending` and `todo` on word boundaries and keeps phrase matching for `in progress`, `current task`, and `implemented behavior`. Unit tests cover both acceptance and rejection.

Phase 7 notes: most of the delivery flow (lexical handoff selection, three-card bound, delivery claims, separate retrieval/injection signals) already existed. The phase added:

- `Applicability::overlaps` and automatic-recall eligibility: project memories and universal globals always eligible; contextual globals only when every populated applicability dimension overlaps the project's detected technologies. Explicit search is unchanged and can still inspect incompatible contextual memories.
- Superseded memories excluded from normal retrieval (only `forgotten` was excluded before); results rank active lifecycle above candidate, then bm25. Superseded and forgotten remain inspectable through explicit reads.
- Stopword filtering in `lexical_tokens`, and automatic recall now queries FTS with sorted content tokens instead of the raw prompt, so prompts like "water the balcony garden" no longer match through "the".
- Checkpoint tests: unrelated/related prompts, bounded cards without full bodies, project isolation, global eligibility, explicit-search escape hatch, superseded exclusion, no provider on the hot path, and p95 under 300 ms with 1,000 memories and 100 handoff items.

## Resume Next

1. Phase 8: external interfaces and operation (`refactor!: replace legacy api and cli surfaces`). Scope per `plan.md` lines 294-316:
   - CLI: `handoff inspect` (current text, items, provenance); `search`/`read`/`write`/`forget` accept only context/playbook.
   - REST: sessions, summaries, current handoff, recall, and new knowledge; legacy selectors and handoff endpoints return explicit absence.
   - UI: project shows current handoff and sources; session shows summary and chronological evidence; memory shows context/playbook; diagnostics show consolidation cost and quality. No backlog transformation.
   - MCP: keep the four operations, accepting context and playbook types.
   - Imports only through normalized sessions and the normal pipeline.
   - Backup/restore stay independent capabilities adapted to the new schema; no backup of old data for this break.
   - Checkpoint: contract tests for CLI/REST/MCP JSON against versioned schemas; double import is idempotent; UI smoke without Project Brief text; clean install plus connect/disconnect per integration and daemon restart in a temporary home.
2. Keep `memory-model-analysis.md` untracked and untouched; it predates this handoff and is not part of the refactor commits.

## Constraints

- Do not restore legacy `fact`, `decision`, `gotcha`, `procedure`, `session-memory`, Goals, episodes, or typed briefing compatibility.
- `product.md` is the behavioral source of truth and must be read before behavior changes.
- Do not generate a Project Brief or general project summary.
- Keep Markdown canonical and SQLite derived/rebuildable where applicable.
- Commit each completed phase with the Conventional Commit message specified by `plan.md`.
- Do not stage `memory-model-analysis.md` unless explicitly requested.
