# Menvane Handoff

## Current State

The continuity-first refactor is in progress. Phases 0 through 9 are committed and the workspace passes formatting, strict locked Clippy, locked workspace tests with all targets/features, and a locked release build.

Completed commits:

- `0549acd docs: define continuity-first memory model`
- `b15277e refactor!: replace legacy memory domain`
- `a0922f8 refactor!: recreate operational and index schemas`
- `763a798 feat: add episodic session summaries`
- `f716345 feat: derive current handoff from live work items`
- `14ed82c feat: replace typed memories with context and playbooks`
- `cca15bc refactor: make recall handoff and intent oriented`
- `0fb11bf refactor!: replace legacy api and cli surfaces`
- `e23e5ff refactor: remove legacy episode and goal infrastructure`

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

Phase 9 notes:

- Removed the orphaned Phase 0 baseline/corpus fixtures and the unused session packet/compiler-era renderer helper.
- Renamed the remaining session-start delivery API from `session_briefing` to `session_start_context`; no typed briefing API remains.
- Removed the remaining legacy vocabulary from executable tests and tightened the daemon lock/open-option and session-status code to satisfy strict Clippy.
- Legacy infrastructure grep is clear except for deliberate absence coverage for `/api/v1/goals` and a normal handoff test phrase containing “in progress”.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, `cargo test --locked --workspace --all-targets --all-features`, and `cargo build --release --locked` pass.

## Resume Next

1. Phase 10: validate development with real sessions and the preserved provider. Scope per `plan.md` lines 339-346:
   - confirm the preserved provider is configured and `provider status` returns `Ready`;
   - run consolidation smoke sessions covering summary-only, new handoff, handoff closure, and zero promotion;
   - correct schema, reference, or contamination failures before proceeding;
   - manually exercise Claude Code, Codex, and OpenCode sessions in temporary homes;
   - inspect summaries, handoff transitions, knowledge promotion, and continuation context before starting the new Menvane instance.
2. Keep `memory-model-analysis.md` untracked and untouched; it predates this handoff and is not part of the refactor commits.

## Constraints

- Do not restore legacy `fact`, `decision`, `gotcha`, `procedure`, `session-memory`, Goals, episodes, or typed briefing compatibility.
- `product.md` is the behavioral source of truth and must be read before behavior changes.
- Do not generate a Project Brief or general project summary.
- Keep Markdown canonical and SQLite derived/rebuildable where applicable.
- Commit each completed phase with the Conventional Commit message specified by `plan.md`.
- Do not stage `memory-model-analysis.md` unless explicitly requested.
