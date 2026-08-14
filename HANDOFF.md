# Menvane Handoff

## Current State

The continuity-first refactor is in progress. Phases 0 through 8 are committed and the workspace passes `cargo fmt --all -- --check`, locked workspace tests with all targets/features, and clippy with no Phase 8 warnings. Pre-existing warnings remain, targeted by Phase 9.

Completed commits:

- `0549acd docs: define continuity-first memory model`
- `b15277e refactor!: replace legacy memory domain`
- `a0922f8 refactor!: recreate operational and index schemas`
- `763a798 feat: add episodic session summaries`
- `f716345 feat: derive current handoff from live work items`
- `14ed82c feat: replace typed memories with context and playbooks`
- `cca15bc refactor: make recall handoff and intent oriented`
- `0fb11bf refactor!: replace legacy api and cli surfaces`

Phase 3 was an operational home reset, not a repository commit. The old `~/.menvane` data was deleted after approval. Only the OpenAI provider configuration and `~/.menvane/oauth/openai.json` were preserved. The new home passed `provider status`, `doctor`, SQLite integrity checks, and foreign-key checks. OAuth permissions are `0600`.

Phase 6 review note: the promotion-barrier task-state check originally matched substrings, so legitimate reusable content containing words like "appending" or "spending" would have been rejected. It now matches the single-word terms `pending` and `todo` on word boundaries and keeps phrase matching for `in progress`, `current task`, and `implemented behavior`. Unit tests cover both acceptance and rejection.

Phase 7 notes: most of the delivery flow (lexical handoff selection, three-card bound, delivery claims, separate retrieval/injection signals) already existed. The phase added:

- `Applicability::overlaps` and automatic-recall eligibility: project memories and universal globals always eligible; contextual globals only when every populated applicability dimension overlaps the project's detected technologies. Explicit search is unchanged and can still inspect incompatible contextual memories.
- Superseded memories excluded from normal retrieval (only `forgotten` was excluded before); results rank active lifecycle above candidate, then bm25. Superseded and forgotten remain inspectable through explicit reads.
- Stopword filtering in `lexical_tokens`, and automatic recall now queries FTS with sorted content tokens instead of the raw prompt, so prompts like "water the balcony garden" no longer match through "the".
- Checkpoint tests: unrelated/related prompts, bounded cards without full bodies, project isolation, global eligibility, explicit-search escape hatch, superseded exclusion, no provider on the hot path, and p95 under 300 ms with 1,000 memories and 100 handoff items.

Phase 8 notes:

- `handoff inspect` always returns versioned JSON containing the current rendered text, structured items, and provenance; empty handoffs are explicit JSON absence rather than plain text.
- REST now lists sessions and returns session metadata, chronological events, episodic summaries, and consolidation execution diagnostics. Unknown routes return explicit JSON 404 responses.
- UI sessions now list and render summary/evidence/diagnostics; project and handoff views render blockers, confidence, confirmation dates, and provenance. Smoke tests reject Project Brief text.
- Versioned JSON contracts under `contracts/v1` cover CLI handoff inspection, REST sessions/detail/handoff/recall/errors, and MCP tools. Contract tests validate live REST and MCP responses; CLI shape is validated against the same checked-in schema.
- Import re-ingestion tests verify session, summary, handoff, and knowledge state remains unchanged. Clean-home reopen coverage complements the existing per-integration connect/disconnect tests.

## Resume Next

1. Phase 9: remove legacy infrastructure and harden invariants (`refactor: remove legacy episode and goal infrastructure`). Scope per `plan.md` lines 318-337:
   - remove legacy episode/goal tables, types, methods, tests, documentation, versioned handoffs, cards, typed briefing, and compiler infrastructure;
   - split large modules only where needed by responsibility;
   - remove incompatible decay/multiplier configuration;
   - ensure `product.md` documents only final current behavior;
   - checkpoint with legacy-vocabulary grep, locked clippy/tests/build, and a clean-home end-to-end capture/finalization/consolidation/resume/recall/reindex/restart flow.
2. Keep `memory-model-analysis.md` untracked and untouched; it predates this handoff and is not part of the refactor commits.

## Constraints

- Do not restore legacy `fact`, `decision`, `gotcha`, `procedure`, `session-memory`, Goals, episodes, or typed briefing compatibility.
- `product.md` is the behavioral source of truth and must be read before behavior changes.
- Do not generate a Project Brief or general project summary.
- Keep Markdown canonical and SQLite derived/rebuildable where applicable.
- Commit each completed phase with the Conventional Commit message specified by `plan.md`.
- Do not stage `memory-model-analysis.md` unless explicitly requested.
